//! `/ignore` and `/unignore` player commands (D-016).
//!
//! A player types `ignore <clientNumber|playerName>` (the same target shape the
//! admin `mute` command takes, D-011) and the proxy stops delivering that
//! player's chat to them. The ignored player keeps talking normally: their
//! messages still reach everyone else.
//!
//! The command arrives as an ordinary client command (`CL_ForwardCommandToServer`
//! sends every console line the client does not handle itself,
//! `original/jedi-academy/codemp/client/cl_main.cpp:507-538`) and is claimed in
//! `GAME_CLIENT_COMMAND` before the game's `ClientCommand` sees it, exactly like
//! the `tffa_*` commands. The filter lives in the `G_SEND_SERVER_COMMAND` trap:
//! `G_Say`/`G_SayTo` deliver a chat line to each recipient individually
//! (`original/jedi-academy-sdk/codemp/game/g_cmds.c:1478-1597`), so the line is
//! dropped for a recipient whose ignore mask holds its speaker. The speaker is
//! the client whose `GAME_CLIENT_COMMAND` forward is in flight (`ACTING_SENDER`,
//! the same synchronous-scope trick the silent mute uses for its echo).
//!
//! The whole feature is gated by `proxy_sv_enableIgnore` (default `0`).

use core::ffi::c_int;
use core::sync::atomic::{AtomicI32, Ordering};
use std::ffi::CString;

use crate::player_target::{self, Census, Target};
use crate::sdk::MAX_CLIENTS;
use crate::state;
use crate::syscall;

/// Shown when `ignore`/`unignore` is issued without a target.
const USAGE: &str = "usage: ignore <clientNumber|playerName> | unignore <clientNumber|playerName>";
/// Shown when `proxy_sv_enableIgnore` is `0`.
const DISABLED: &str = "proxy: ignore is disabled (proxy_sv_enableIgnore 0).";

/// How many ignored players the usage reply lists. A client `print` is a single
/// reliable command and the engine's string buffer is finite; eight long names
/// stay well inside it, and the count is reported for the rest.
const MAX_LISTED_IGNORED: usize = 8;

/// The per-player mask is a `u32`, one bit per slot (`ClientEntry::ignored`).
const _: () = assert!(MAX_CLIENTS <= 32);

// ---------------------------------------------------------------------------
// GAME_CLIENT_COMMAND — a player ignoring someone
// ---------------------------------------------------------------------------

/// What one `ignore`/`unignore` line asks the proxy to do. Split from the engine
/// glue so the decisions (and every reply line) are unit-tested without a live
/// engine.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Print to the issuing player; nothing changes.
    Reply(Vec<String>),
    /// Set (`ignore`) or clear (`unignore`) one slot in the player's mask, then
    /// print `lines`.
    SetIgnore {
        slot: usize,
        ignore: bool,
        lines: Vec<String>,
    },
}

/// Handle `ignore`/`unignore` from `GAME_CLIENT_COMMAND`. Returns `true` when
/// the command is ours, in which case the caller must not forward it.
pub fn handle_client_command(client_num: c_int) -> bool {
    let Some(ignore) = direction(&player_target::arg(0)) else {
        return false;
    };
    // A disabled feature still claims its two commands so the game does not
    // treat them as unknown client commands; the player gets a clear answer.
    if !enabled() {
        reply(client_num, DISABLED);
        return true;
    }
    let current = ignored_mask(client_num);
    match decide(
        ignore,
        &player_target::target_argument(),
        client_num as usize,
        &player_target::census(),
        current,
    ) {
        Action::Reply(lines) => reply_lines(client_num, &lines),
        Action::SetIgnore {
            slot,
            ignore,
            lines,
        } => {
            state::with_state(|s| apply(&mut s.clients, client_num as usize, slot, ignore));
            reply_lines(client_num, &lines);
        }
    }
    true
}

/// `Some(true)` for `ignore`, `Some(false)` for `unignore`, `None` for any other
/// client command (which is not ours).
fn direction(command: &[u8]) -> Option<bool> {
    if command.eq_ignore_ascii_case(b"ignore") {
        Some(true)
    } else if command.eq_ignore_ascii_case(b"unignore") {
        Some(false)
    } else {
        None
    }
}

/// Whether `proxy_sv_enableIgnore` is on.
pub fn enabled() -> bool {
    state::with_state(|s| s.cvars[state::CVAR_ENABLE_IGNORE].integer != 0)
}

/// The slots `client_num` currently ignores (`0` outside the table).
fn ignored_mask(client_num: c_int) -> u32 {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return 0;
    }
    state::with_state(|s| s.clients[client_num as usize].ignored)
}

/// Decide what an `ignore` (`ignore = true`) / `unignore` line with `target`
/// does. `current` is the issuing player's mask before the change.
fn decide(ignore: bool, target: &[u8], self_slot: usize, census: &Census, current: u32) -> Action {
    if target.is_empty() {
        return Action::Reply(usage_and_ignored_list(census, current));
    }
    match player_target::resolve(target, census) {
        Target::Slot(slot) if slot == self_slot => {
            Action::Reply(vec!["proxy: you cannot ignore yourself.".to_string()])
        }
        Target::Slot(slot) => Action::SetIgnore {
            slot,
            ignore,
            lines: vec![format!(
                "proxy: {} #{} \"{}\"",
                if ignore {
                    "you are now ignoring"
                } else {
                    "you are no longer ignoring"
                },
                slot,
                player_target::display_name(census, slot)
            )],
        },
        Target::EmptySlot(slot) => {
            Action::Reply(vec![format!("proxy: client slot {slot} has no player.")])
        }
        Target::EmptyName => Action::Reply(vec![
            "proxy: give a client number or a player name.".to_string(),
        ]),
        Target::UnknownName => Action::Reply(vec![format!(
            "proxy: no player named \"{}\".",
            String::from_utf8_lossy(target)
        )]),
        Target::Ambiguous(slots) => Action::Reply(vec![format!(
            "proxy: \"{}\" matches {}; use the client number.",
            String::from_utf8_lossy(target),
            player_target::candidate_list(&slots, census)
        )]),
    }
}

/// `usage:` line plus the players this client ignores.
fn usage_and_ignored_list(census: &Census, mask: u32) -> Vec<String> {
    let ignored = (0..MAX_CLIENTS)
        .filter(|&slot| census.connected[slot] && mask & slot_bit(slot) != 0)
        .collect::<Vec<_>>();
    let shown = ignored
        .iter()
        .take(MAX_LISTED_IGNORED)
        .map(|&slot| format!("#{slot} \"{}\"", player_target::display_name(census, slot)))
        .collect::<Vec<_>>()
        .join(", ");
    let list = if ignored.is_empty() {
        "proxy: you are not ignoring anyone.".to_string()
    } else {
        match ignored.len().checked_sub(MAX_LISTED_IGNORED) {
            Some(more) if more > 0 => format!("proxy: ignoring: {shown}, +{more} more"),
            _ => format!("proxy: ignoring: {shown}"),
        }
    };
    vec![USAGE.to_string(), list]
}

/// Set or clear `slot` in `self_slot`'s mask. Bounds-guarded; roster validity is
/// the census's call (`resolve` never returns an empty slot).
fn apply(clients: &mut [state::ClientEntry], self_slot: usize, slot: usize, ignore: bool) -> bool {
    let Some(entry) = clients.get_mut(self_slot) else {
        return false;
    };
    let bit = slot_bit(slot);
    if ignore {
        entry.ignored |= bit;
    } else {
        entry.ignored &= !bit;
    }
    true
}

/// The bit standing for `slot` in an ignore mask (`0` outside the table, so a
/// stray value can never panic a 32-bit shift).
fn slot_bit(slot: usize) -> u32 {
    1u32.checked_shl(slot as u32).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// G_SEND_SERVER_COMMAND — the ignored speaker's chat
// ---------------------------------------------------------------------------

/// Client whose `GAME_CLIENT_COMMAND` forward is in flight, or `-1` when no
/// client command is being processed. Set only around the synchronous forward,
/// so the `G_SEND_SERVER_COMMAND` trap can tell whose chat line it is.
static ACTING_SENDER: AtomicI32 = AtomicI32::new(-1);

/// Open the acting-sender scope for one forwarded client command. Returns the
/// previous value to hand back to [`end_sender`] (nesting-safe).
pub fn begin_sender(client_num: c_int) -> c_int {
    ACTING_SENDER.swap(client_num, Ordering::Relaxed)
}

/// Close a scope opened by [`begin_sender`].
pub fn end_sender(previous: c_int) {
    ACTING_SENDER.store(previous, Ordering::Relaxed);
}

/// Whether a server command emitted during a client-command scope must be
/// dropped for `recipient`: the line is chat and the recipient ignores `sender`.
///
/// Screened on bytes because the payload carries the client's raw chat text
/// (single-byte encoding, not necessarily UTF-8). The speaker always keeps their
/// own line, and a broadcast (`-1`) cannot be filtered per recipient, so it is
/// left to its recipients (the stock game never broadcasts chat).
pub fn chat_dropped(command: &[u8], recipient: c_int) -> bool {
    let sender = ACTING_SENDER.load(Ordering::Relaxed);
    // The common case: no client command in flight at all, so this is one
    // atomic read on every `G_SEND_SERVER_COMMAND`.
    if sender < 0 || recipient < 0 || recipient == sender || !is_chat_message(command) {
        return false;
    }
    if sender as usize >= MAX_CLIENTS || recipient as usize >= MAX_CLIENTS {
        return false;
    }
    state::with_state(|s| {
        s.cvars[state::CVAR_ENABLE_IGNORE].integer != 0
            && s.clients[recipient as usize].ignored & slot_bit(sender as usize) != 0
    })
}

/// Whether `command` is one of the server commands `G_Say`/`G_SayTo` deliver
/// chat with (`chat`, `tchat`, `lchat`, `ltchat`). A `tell` uses the same names.
fn is_chat_message(command: &[u8]) -> bool {
    const PREFIXES: [&[u8]; 4] = [
        b"chat ".as_slice(),
        b"tchat ".as_slice(),
        b"lchat ".as_slice(),
        b"ltchat ".as_slice(),
    ];
    PREFIXES.iter().any(|prefix| command.starts_with(prefix))
}

/// Drop every ignore a disconnecting client issued or received: their own mask
/// goes away with the entry, and the slot is removed from everyone else's mask
/// so the next player in that slot is not silently ignored.
pub fn on_client_disconnect(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let slot = client_num as usize;
    let bit = slot_bit(slot);
    state::with_state(|s| {
        for entry in s.clients.iter_mut() {
            entry.ignored &= !bit;
        }
        s.clients[slot].ignored = 0;
    });
}

// ---------------------------------------------------------------------------
// Reply helpers
// ---------------------------------------------------------------------------

/// Build one `print` server command for a client's own console (not chat).
///
/// The client re-tokenizes this line (`Cmd_TokenizeString`,
/// `original/jedi-academy/codemp/qcommon/cmd_common.cpp:541-634`), which ends a
/// quoted argument at the first inner `"`: a payload containing one loses its
/// tail, including the trailing newline. Every `"` therefore becomes `'` (the
/// same reason `mute` can quote names freely: it answers through `trap_Print`,
/// not a `print` command). Backslashes become `/` and control characters become
/// spaces so the payload stays one plain, closed token.
fn print_command(text: &str) -> Option<CString> {
    let sanitized: String = text
        .chars()
        .map(|c| match c {
            '"' => '\'',
            '\\' => '/',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    CString::new(format!("print \"{sanitized}\n\"")).ok()
}

/// `print` line to one client.
fn reply(client_num: c_int, text: &str) {
    let Some(cmsg) = print_command(text) else {
        return;
    };
    // SAFETY: engine syscall registered; `cmsg` is NUL-terminated.
    unsafe { syscall::send_server_command(client_num, &cmsg) };
}

fn reply_lines(client_num: c_int, lines: &[String]) {
    for line in lines {
        reply(client_num, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils;

    /// Build a census with `(connected, name)` per slot, the rest empty.
    fn census_of(entries: &[(bool, &str)]) -> Census {
        let mut clients: Vec<state::ClientEntry> = (0..MAX_CLIENTS)
            .map(|_| state::ClientEntry::default())
            .collect();
        for (slot, (connected, name)) in entries.iter().enumerate() {
            clients[slot].is_connected = *connected;
            utils::strncpyz(&mut clients[slot].clean_name, name.as_bytes());
        }
        player_target::census_from(&clients)
    }

    fn clients_of(count: usize) -> Vec<state::ClientEntry> {
        (0..count).map(|_| state::ClientEntry::default()).collect()
    }

    #[test]
    fn only_ignore_and_unignore_are_ours() {
        assert_eq!(direction(b"ignore"), Some(true));
        assert_eq!(direction(b"IGNORE"), Some(true));
        assert_eq!(direction(b"unignore"), Some(false));
        assert_eq!(direction(b"UnIgnore"), Some(false));
        for command in [&b""[..], b"ignored", b"unignored", b"ignore_me", b"say"] {
            assert_eq!(
                direction(command),
                None,
                "{:?}",
                String::from_utf8_lossy(command)
            );
        }
    }

    #[test]
    fn chat_commands_are_recognised_on_bytes() {
        for command in [
            &b"chat \"Darth: hi\""[..],
            b"tchat \"Darth: hi\"",
            b"lchat \"Darth\" \"Yavin\" \"^4\" \"hi\"",
            b"ltchat \"Darth\" \"Yavin\" \"^4\" \"hi\"",
            b"chat \"caf\xe9\"",
        ] {
            assert!(
                is_chat_message(command),
                "{:?}",
                String::from_utf8_lossy(command)
            );
        }
        for command in [
            &b""[..],
            b"print \"chat hi\"",
            b"tchatty",
            b"cp \"chat hi\"",
            b"scores \"1 0\"",
        ] {
            assert!(
                !is_chat_message(command),
                "{:?}",
                String::from_utf8_lossy(command)
            );
        }
    }

    #[test]
    fn ignore_sets_and_clears_only_the_target_bit() {
        let mut clients = clients_of(MAX_CLIENTS);
        assert!(apply(&mut clients, 1, 3, true));
        assert_eq!(clients[1].ignored, 1 << 3);
        // Idempotent.
        assert!(apply(&mut clients, 1, 3, true));
        assert_eq!(clients[1].ignored, 1 << 3);
        // Another slot is a different bit, and another player a different mask.
        assert!(apply(&mut clients, 1, 5, true));
        assert_eq!(clients[1].ignored, (1 << 3) | (1 << 5));
        assert_eq!(clients[2].ignored, 0);
        // Unignore only clears its own bit.
        assert!(apply(&mut clients, 1, 3, false));
        assert_eq!(clients[1].ignored, 1 << 5);
        // Out-of-table players are refused.
        assert!(!apply(&mut clients, MAX_CLIENTS, 0, true));
    }

    #[test]
    fn print_payload_is_one_closed_token() {
        // The client's tokenizer (`Cmd_TokenizeString`) ends a quoted argument
        // at the first inner `"`; only the two delimiters may remain, and the
        // trailing newline must sit inside the closing quote.
        let command = print_command(
            "proxy: \"dar\" matches #0 \"Darth\", #1 \"Darker\"; use the client number.",
        )
        .expect("no interior NUL");
        let text = command.to_str().unwrap();
        assert_eq!(text.matches('"').count(), 2);
        assert!(text.starts_with("print \""));
        assert!(text.ends_with(".\n\""));
        // Nothing after the first quote was truncated -- the bug this guards.
        assert!(text.contains("'dar' matches #0 'Darth', #1 'Darker'"));
    }

    #[test]
    fn print_payload_replaces_backslashes_and_controls() {
        let command = print_command("a\\b\"c\nd").expect("no interior NUL");
        assert_eq!(command.to_str().unwrap(), "print \"a/b'c d\n\"");
    }

    #[test]
    fn decide_sets_and_reports_the_resolved_player() {
        let census = census_of(&[(true, "^1Darth"), (true, "Luke")]);
        assert_eq!(
            decide(true, b"darth", 1, &census, 0),
            Action::SetIgnore {
                slot: 0,
                ignore: true,
                lines: vec!["proxy: you are now ignoring #0 \"Darth\"".to_string()],
            }
        );
        assert_eq!(
            decide(false, b"luke", 0, &census, 1 << 1),
            Action::SetIgnore {
                slot: 1,
                ignore: false,
                lines: vec!["proxy: you are no longer ignoring #1 \"Luke\"".to_string()],
            }
        );
    }

    #[test]
    fn decide_refuses_self_and_bad_targets() {
        let census = census_of(&[(true, "Darth"), (true, "Darker")]);
        assert_eq!(
            decide(true, b"darth", 0, &census, 0),
            Action::Reply(vec!["proxy: you cannot ignore yourself.".to_string()])
        );
        assert_eq!(
            decide(true, b"9", 0, &census, 0),
            Action::Reply(vec!["proxy: client slot 9 has no player.".to_string()])
        );
        assert_eq!(
            decide(false, b"yoda", 0, &census, 0),
            Action::Reply(vec!["proxy: no player named \"yoda\".".to_string()])
        );
        assert_eq!(
            decide(true, b"dar", 0, &census, 0),
            Action::Reply(vec![
                "proxy: \"dar\" matches #0 \"Darth\", #1 \"Darker\"; use the client number."
                    .to_string()
            ])
        );
        assert_eq!(
            decide(true, b"^1", 0, &census, 0),
            Action::Reply(vec![
                "proxy: give a client number or a player name.".to_string()
            ])
        );
    }

    #[test]
    fn bare_ignore_lists_the_ignored_players() {
        let census = census_of(&[(true, "Darth"), (true, "Luke")]);
        assert_eq!(
            decide(true, b"", 1, &census, 0),
            Action::Reply(vec![
                USAGE.to_string(),
                "proxy: you are not ignoring anyone.".to_string()
            ])
        );
        assert_eq!(
            decide(false, b"", 1, &census, (1 << 0) | (1 << 1)),
            Action::Reply(vec![
                USAGE.to_string(),
                "proxy: ignoring: #0 \"Darth\", #1 \"Luke\"".to_string()
            ])
        );
    }

    #[test]
    fn bare_ignore_list_is_capped() {
        let entries: Vec<(bool, String)> = (0..MAX_CLIENTS)
            .map(|slot| (true, format!("Raider{slot}")))
            .collect();
        let mut clients: Vec<state::ClientEntry> = (0..MAX_CLIENTS)
            .map(|_| state::ClientEntry::default())
            .collect();
        for (slot, (connected, name)) in entries.iter().enumerate() {
            clients[slot].is_connected = *connected;
            utils::strncpyz(&mut clients[slot].clean_name, name.as_bytes());
        }
        let census = player_target::census_from(&clients);
        let mask = u32::MAX;
        let Action::Reply(lines) = decide(true, b"", 0, &census, mask) else {
            panic!("expected a reply");
        };
        assert!(lines[1].starts_with("proxy: ignoring: #0 \"Raider0\""));
        assert!(lines[1].ends_with(", +24 more"));
    }

    #[test]
    fn disconnect_clears_both_directions_of_the_mask() {
        let _guard = state::TEST_STATE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::with_state(|s| {
            s.clients[1].ignored = slot_bit(3) | slot_bit(5);
            s.clients[3].ignored = slot_bit(1);
            s.clients[5].ignored = slot_bit(3);
        });

        // Slot 3 leaves: its own mask goes, and no one else keeps bit 3.
        on_client_disconnect(3);

        state::with_state(|s| {
            assert_eq!(s.clients[1].ignored, slot_bit(5));
            assert_eq!(s.clients[3].ignored, 0);
            assert_eq!(s.clients[5].ignored, 0);
            s.clients[1].ignored = 0;
            s.clients[5].ignored = 0;
        });
    }

    #[test]
    fn chat_is_dropped_only_for_recipients_that_ignore_the_speaker() {
        let _guard = state::TEST_STATE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::with_state(|s| {
            s.cvars[state::CVAR_ENABLE_IGNORE].integer = 1;
            s.clients[1].ignored = slot_bit(0);
        });

        let chat = b"chat \"Darth: hi\"";
        let previous = begin_sender(0);
        // Client 1 ignores client 0: dropped. Client 2 does not: delivered.
        assert!(chat_dropped(chat, 1));
        assert!(!chat_dropped(chat, 2));
        // The speaker keeps their own line, and a broadcast cannot be split.
        assert!(!chat_dropped(chat, 0));
        assert!(!chat_dropped(chat, -1));
        // Only chat commands are screened.
        assert!(!chat_dropped(b"print \"Darth: hi\"", 1));
        // Feature off: the mask is dormant.
        state::with_state(|s| s.cvars[state::CVAR_ENABLE_IGNORE].integer = 0);
        assert!(!chat_dropped(chat, 1));
        end_sender(previous);
        // No client command in flight: nothing is attributed.
        assert!(!chat_dropped(chat, 1));

        state::with_state(|s| s.clients[1].ignored = 0);
    }
}
