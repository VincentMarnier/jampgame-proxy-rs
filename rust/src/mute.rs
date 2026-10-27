//! `mute` / `unmute` console commands (D-011).
//!
//! A command the engine does not recognise is handed to the game module
//! (`qcommon/cmd_console.cpp:122` → `SV_GameCommand` → `vmMain(GAME_CONSOLE_COMMAND)`),
//! which is how `rcon mute 0` — and the same line typed on the dedicated
//! server's own console — reaches us: `SVC_RemoteCommand` executes the rcon
//! payload with `Cmd_ExecuteString` (`server/sv_main.cpp:510`) after it opened
//! its `Com_BeginRedirect`, so a `trap_Print` answer goes back to the admin.
//! The pristine module does not know the command (`g_svcmds.c:ConsoleCommand`)
//! and, on a dedicated server, would echo the line to every client as
//! `server: …`; the proxy claims it instead and returns qtrue.
//!
//! Muting is a per-slot admin decision, not a gameplay state: a muted client
//! keeps playing normally, but the commands that carry their text or voice to
//! other players are dropped in `GAME_CLIENT_COMMAND` before the game's
//! `ClientCommand` (`g_cmds.c:3093-3120`) sees them. Everything else the
//! client sends is forwarded untouched.

use core::ffi::c_int;
use std::ffi::CString;

use crate::sdk::{
    CON_DISCONNECTED, MAX_CLIENTS, MAX_NETNAME, MAX_TOKEN_CHARS, OFFSET_GCLIENT_PERS,
    OFFSET_GENTITY_CLIENT, OFFSET_PERS_CONNECTED, SIZEOF_GENTITY,
};
use crate::state::{self, ClientEntry};
use crate::syscall;
use crate::utils;

use crate::hooks::engine_sv::{read_i32, read_usize};

/// The client commands a muted player may no longer issue: the two chat
/// commands, private tells, and the voice menu's `voice_cmd`
/// (`g_cmds.c:3093-3120`).
const CHAT_COMMANDS: [&[u8]; 4] = [
    b"say".as_slice(),
    b"say_team".as_slice(),
    b"tell".as_slice(),
    b"voice_cmd".as_slice(),
];

/// Print sent to a muted player when a chat command is dropped.
const MUTED_NOTICE: &std::ffi::CStr = c"print \"^3You are muted by a server admin.^7\n\"";
/// Print sent to a player an admin mutes.
const MUTE_NOTICE: &std::ffi::CStr = c"print \"^3You have been muted by a server admin.^7\n\"";
/// Print sent to a player an admin unmutes.
const UNMUTE_NOTICE: &std::ffi::CStr = c"print \"^3You have been unmuted.^7\n\"";
/// Shown when `mute`/`unmute` is issued without a target.
const USAGE: &str = "usage: mute <clientNumber|playerName> | unmute <clientNumber|playerName>";

// ---------------------------------------------------------------------------
// GAME_CONSOLE_COMMAND — the rcon / server-console side
// ---------------------------------------------------------------------------

/// What one console line asks the proxy to do. Split from the engine glue so the
/// decisions (and every reply line) are unit-tested without a live engine.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Console lines to print; nothing changes.
    Reply(Vec<String>),
    /// Set (`mute`) or clear (`unmute`) the mute of `slot`, then print `lines`.
    SetMute {
        slot: usize,
        mute: bool,
        lines: Vec<String>,
    },
}

/// Handle `mute`/`unmute` from `GAME_CONSOLE_COMMAND`. Returns `true` when the
/// command is ours, in which case the caller must answer qtrue so the engine
/// does not treat the line as a server chat.
pub fn handle_console_command() -> bool {
    // Every other console line is forwarded to the game without reading proxy
    // state: `rcon status`, `map`, `kick`, … stay on the pristine path.
    let Some(mute) = mute_direction(&arg(0)) else {
        return false;
    };
    match decide(mute, &target_argument(), &census()) {
        Action::Reply(lines) => print_lines(&lines),
        Action::SetMute { slot, mute, lines } => {
            if !state::with_state(|s| apply_target(&mut s.clients, slot, mute)) {
                // The slot emptied between the census and the write
                // (single-threaded, so this is defensive only).
                print_line(&format!("proxy: client slot {slot} has no player."));
                return true;
            }
            // SAFETY: engine syscall registered; both notices are
            // NUL-terminated literals.
            unsafe {
                syscall::send_server_command(
                    slot as c_int,
                    if mute { MUTE_NOTICE } else { UNMUTE_NOTICE },
                );
            }
            print_lines(&lines);
        }
    }
    true
}

/// `Some(true)` for `mute`, `Some(false)` for `unmute`, `None` for any other
/// console command (which the game module keeps).
fn mute_direction(command: &[u8]) -> Option<bool> {
    if command.eq_ignore_ascii_case(b"mute") {
        Some(true)
    } else if command.eq_ignore_ascii_case(b"unmute") {
        Some(false)
    } else {
        None
    }
}

/// Decide what a `mute` (`mute = true`) / `unmute` line with `target` does.
fn decide(mute: bool, target: &[u8], census: &Census) -> Action {
    if target.is_empty() {
        return Action::Reply(usage_and_muted_list(census));
    }
    plan(mute, target, census)
}

/// Route one resolved argument to its action. `mute` selects the direction;
/// `unmute` with no such player reports exactly like `mute` would.
fn plan(mute: bool, target: &[u8], census: &Census) -> Action {
    match resolve(target, census) {
        Target::Slot(slot) => Action::SetMute {
            slot,
            mute,
            lines: vec![format!(
                "proxy: {} #{slot} \"{}\"",
                if mute { "muted" } else { "unmuted" },
                display_name(census, slot)
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
            candidate_list(&slots, census)
        )]),
    }
}

/// How many candidates an ambiguity reply lists. Each printed line is its own
/// rcon packet and the engine's redirect buffer is 1008 bytes
/// (`SVC_RemoteCommand`'s `sv_outputbuf`), so a full 32-slot list would be
/// truncated mid-name.
const MAX_LISTED_CANDIDATES: usize = 8;

/// `#0 "Darth", #1 "Darker", … (+N more)` for an ambiguity reply.
fn candidate_list(slots: &[usize], census: &Census) -> String {
    let shown = slots
        .iter()
        .take(MAX_LISTED_CANDIDATES)
        .map(|&slot| format!("#{slot} \"{}\"", display_name(census, slot)))
        .collect::<Vec<_>>()
        .join(", ");
    match slots.len().checked_sub(MAX_LISTED_CANDIDATES) {
        Some(more) if more > 0 => format!("{shown}, +{more} more (see status)"),
        _ => shown,
    }
}

/// `usage:` line plus the players that are currently muted.
fn usage_and_muted_list(census: &Census) -> Vec<String> {
    let muted = (0..MAX_CLIENTS)
        .filter(|&slot| census.muted[slot] && census.connected[slot])
        .map(|slot| format!("#{slot} \"{}\"", display_name(census, slot)))
        .collect::<Vec<_>>();
    let list = if muted.is_empty() {
        "proxy: no player is muted.".to_string()
    } else {
        format!("proxy: muted players: {}", muted.join(", "))
    };
    vec![USAGE.to_string(), list]
}

// ---------------------------------------------------------------------------
// GAME_CLIENT_COMMAND — the muted player's side
// ---------------------------------------------------------------------------

/// Drop a muted client's chat commands. Returns `true` when the command must
/// not reach the game module.
pub fn handle_client_command(client_num: c_int) -> bool {
    if !state::proxy_enabled() || !is_muted(client_num) {
        return false;
    }
    let command = arg(0);
    if !is_chat_command(&command) {
        return false;
    }
    // SAFETY: engine syscall registered; the notice is a NUL-terminated
    // literal.
    unsafe { syscall::send_server_command(client_num, MUTED_NOTICE) };
    true
}

/// Whether `command` is one of the commands a mute suppresses.
fn is_chat_command(command: &[u8]) -> bool {
    CHAT_COMMANDS
        .iter()
        .any(|name| command.eq_ignore_ascii_case(name))
}

/// Whether the client slot currently carries a mute.
fn is_muted(client_num: c_int) -> bool {
    (0..MAX_CLIENTS as c_int).contains(&client_num)
        && state::with_state(|s| s.clients[client_num as usize].muted)
}

// ---------------------------------------------------------------------------
// Target resolution (client number or player name)
// ---------------------------------------------------------------------------

/// What a `mute`/`unmute` argument selected.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// A connected client slot.
    Slot(usize),
    /// A decimal argument naming a slot that holds no player (or no slot at
    /// all).
    EmptySlot(usize),
    /// No connected player has a name matching the argument.
    UnknownName,
    /// The argument is empty once colour codes are removed (nothing to match).
    EmptyName,
    /// Several connected players match; the slots, in slot order.
    Ambiguous(Vec<usize>),
}

/// Per-slot connection state, mute flag and normalised (colour-stripped) name,
/// taken once so resolution, reporting and the following write see the same
/// roster.
struct Census {
    connected: [bool; MAX_CLIENTS],
    muted: [bool; MAX_CLIENTS],
    names: [[u8; MAX_NETNAME]; MAX_CLIENTS],
}

fn census() -> Census {
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);

    // Before the world is located (bootstrap window) the connect mirror is all
    // there is.
    if g_entities == 0 {
        return state::with_state(|s| census_from(&s.clients));
    }

    let muted: [bool; MAX_CLIENTS] =
        state::with_state(|s| core::array::from_fn(|i| s.clients[i].muted));
    let mut census = Census {
        connected: [false; MAX_CLIENTS],
        muted,
        names: [[0u8; MAX_NETNAME]; MAX_CLIENTS],
    };

    // Live game state, like `tffa::connected_clients`: bots are added by the
    // game module itself and never raise `GAME_CLIENT_BEGIN`, so the proxy's
    // own mirror does not see them.
    for slot in 0..MAX_CLIENTS {
        let entity = g_entities + slot * SIZEOF_GENTITY;
        // SAFETY: `entity` is inside the located g_entities array.
        let client = unsafe { read_usize(entity + OFFSET_GENTITY_CLIENT) };
        // SAFETY: a client slot's `pers.connected` is a plain int.
        census.connected[slot] = client != 0
            && unsafe { read_i32(client + OFFSET_GCLIENT_PERS + OFFSET_PERS_CONNECTED) }
                != CON_DISCONNECTED;
        if census.connected[slot] {
            crate::hooks::game::client_clean_netname(slot as c_int, &mut census.names[slot]);
        }
    }
    census
}

/// The connect-mirror census (`Proxy_SharedAPI_ClientBegin`'s `is_connected` and
/// the mirrored, sanitised userinfo name). Used before the world is located and
/// by the unit tests; [`census`] prefers the live game memory.
fn census_from(clients: &[ClientEntry]) -> Census {
    let mut census = Census {
        connected: [false; MAX_CLIENTS],
        muted: [false; MAX_CLIENTS],
        names: [[0u8; MAX_NETNAME]; MAX_CLIENTS],
    };
    for (slot, entry) in clients.iter().take(MAX_CLIENTS).enumerate() {
        census.connected[slot] = entry.is_connected;
        census.muted[slot] = entry.muted;
        if entry.is_connected {
            utils::strncpyz(
                &mut census.names[slot],
                syscall::read_engine_string(&entry.clean_name),
            );
            utils::strip_color(&mut census.names[slot]);
        }
    }
    census
}

/// Resolve a `mute`/`unmute` argument against the census.
///
/// A decimal word is a client slot (`mute 0` is the engine's own clientNum, the
/// number `status` and the scoreboard use). Anything else is a player name,
/// matched against the colour-stripped `pers.netname` in three tiers:
/// exact, then prefix, then substring — case-insensitive, with colour codes
/// ignored on both sides. The first tier that matches decides, so `darth` picks
/// "Darth" over "Darth Vader", and `ade` picks "Darth Vader". Several players in
/// the winning tier are reported rather than guessed.
fn resolve(target: &[u8], census: &Census) -> Target {
    if let Some(slot) = parse_slot(target) {
        return if slot < MAX_CLIENTS && census.connected[slot] {
            Target::Slot(slot)
        } else {
            Target::EmptySlot(slot)
        };
    }
    match_name(target, census)
}

/// A whole decimal word as a client slot number (`"0"`), or `None` when the
/// argument is not a plain non-negative number and must be a name.
fn parse_slot(target: &[u8]) -> Option<usize> {
    let text = core::str::from_utf8(target).ok()?;
    // `u32` covers every real slot; a longer number is out of range anyway and
    // is reported as an empty slot.
    text.parse::<u32>().ok().map(|slot| slot as usize)
}

fn match_name(target: &[u8], census: &Census) -> Target {
    let mut wanted = [0u8; MAX_NETNAME];
    utils::strncpyz(&mut wanted, target);
    utils::strip_color(&mut wanted);
    let wanted = syscall::read_engine_string(&wanted);
    if wanted.is_empty() {
        return Target::EmptyName;
    }

    let mut exact = Vec::new();
    let mut prefix = Vec::new();
    let mut substring = Vec::new();
    for slot in 0..MAX_CLIENTS {
        if !census.connected[slot] {
            continue;
        }
        let name = syscall::read_engine_string(&census.names[slot]);
        if name.eq_ignore_ascii_case(wanted) {
            exact.push(slot);
        } else if name.len() > wanted.len() && name[..wanted.len()].eq_ignore_ascii_case(wanted) {
            prefix.push(slot);
        } else if name.len() > wanted.len() && contains_ignore_ascii_case(name, wanted) {
            substring.push(slot);
        }
    }

    match_tier(exact)
        .or_else(|| match_tier(prefix))
        .or_else(|| match_tier(substring))
        .unwrap_or(Target::UnknownName)
}

/// Turn one tier's candidate list into a target: the only candidate, an
/// ambiguity, or nothing so the next tier can be tried.
fn match_tier(mut candidates: Vec<usize>) -> Option<Target> {
    match candidates.len() {
        0 => None,
        1 => Some(Target::Slot(candidates[0])),
        _ => {
            candidates.sort_unstable();
            Some(Target::Ambiguous(candidates))
        }
    }
}

/// ASCII-case-insensitive `strstr` on a name and a wanted fragment. Both come
/// from colour-stripped buffers, so a `^` in a name is literal there.
fn contains_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Set or clear the mute on `slot`.
///
/// Roster validity is the census's call: [`resolve`] never returns a slot
/// nobody occupies. This only guards the table bounds, so a player the connect
/// mirror never saw begin (a bot) can still be muted.
fn apply_target(clients: &mut [ClientEntry], slot: usize, mute: bool) -> bool {
    let Some(entry) = clients.get_mut(slot) else {
        return false;
    };
    entry.muted = mute;
    true
}

/// The census's name for a slot, for console output.
fn display_name(census: &Census, slot: usize) -> String {
    String::from_utf8_lossy(syscall::read_engine_string(&census.names[slot])).into_owned()
}

// ---------------------------------------------------------------------------
// Engine helpers
// ---------------------------------------------------------------------------

/// Command argument `n` of the command being executed (`trap_Argv`), copied out
/// of the engine's token buffer.
fn arg(n: c_int) -> Vec<u8> {
    let mut buffer = [0u8; MAX_TOKEN_CHARS];
    // SAFETY: engine syscall registered; buffer writable and NUL-terminated by
    // the engine.
    unsafe { syscall::argv(n, &mut buffer) };
    syscall::read_engine_string(&buffer).to_vec()
}

/// The whole target: every argument from 1 on, joined with single spaces by the
/// game's own `ConcatArgs` (the same helper the proxy uses for chat text).
///
/// Quoting cannot be relied on here: `SVC_RemoteCommand` re-tokenises the rcon
/// payload and rebuilds the line from `Cmd_Argv(2..)` with single spaces
/// (`original/jedi-academy/codemp/server/sv_main.cpp:504-510`), so
/// `rcon mute "Darth Vader"` reaches the module as `mute Darth Vader`.
/// Concatenating the remainder is what lets a name with spaces still be named,
/// and it is a no-op for `mute 0`.
fn target_argument() -> Vec<u8> {
    // SAFETY: `GAME_CONSOLE_COMMAND` is called through our vmMain, so the
    // original module is loaded and `GAME_INIT` has run; the returned buffer is
    // valid until the next command is processed.
    let ptr = unsafe { crate::jampgame::concat_args(1) };
    if ptr.is_null() {
        return Vec::new();
    }
    // SAFETY: `ConcatArgs` returns a NUL-terminated game static buffer.
    unsafe { core::ffi::CStr::from_ptr(ptr) }
        .to_bytes()
        .to_vec()
}

/// `trap_Print`: one console line. During an rcon the engine redirects it back
/// to the remote admin.
fn print_line(text: &str) {
    let Ok(line) = CString::new(format!("{text}\n")) else {
        return;
    };
    // SAFETY: engine syscall registered; `line` is NUL-terminated.
    unsafe { syscall::print(&line) };
}

fn print_lines(lines: &[String]) {
    for line in lines {
        print_line(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a census with `(connected, name)` per slot, the rest empty.
    fn census_of(entries: &[(bool, &str)]) -> Census {
        let mut clients: Vec<ClientEntry> =
            (0..MAX_CLIENTS).map(|_| ClientEntry::default()).collect();
        for (slot, (connected, name)) in entries.iter().enumerate() {
            clients[slot].is_connected = *connected;
            utils::strncpyz(&mut clients[slot].clean_name, name.as_bytes());
        }
        census_from(&clients)
    }

    #[test]
    fn chat_commands_are_recognised_exactly() {
        for command in [&b"say"[..], b"say_team", b"tell", b"voice_cmd"] {
            assert!(
                is_chat_command(command),
                "{:?}",
                String::from_utf8_lossy(command)
            );
            assert!(is_chat_command(&command.to_ascii_uppercase()));
        }
        // Prefixes, suffixes and other player commands are untouched.
        for command in [&b""[..], b"say_team2", b"admin_say", b"kill", b"tffa_list"] {
            assert!(!is_chat_command(command));
        }
    }

    #[test]
    fn numeric_target_is_a_client_slot() {
        let census = census_of(&[(true, "Darth"), (true, "Luke")]);
        assert_eq!(resolve(b"0", &census), Target::Slot(0));
        assert_eq!(resolve(b"1", &census), Target::Slot(1));
        // An empty slot, and a number beyond the table, are not players.
        assert_eq!(resolve(b"2", &census), Target::EmptySlot(2));
        assert_eq!(resolve(b"99", &census), Target::EmptySlot(99));
        // A negative number is not a slot; it is looked up as a name.
        assert_eq!(resolve(b"-1", &census), Target::UnknownName);
    }

    #[test]
    fn name_target_ignores_case_and_colour_codes() {
        let census = census_of(&[(true, "^1Darth"), (true, "Luke")]);
        for target in [&b"darth"[..], b"DARTH", b"^1Darth", b"^1darth"] {
            assert_eq!(
                resolve(target, &census),
                Target::Slot(0),
                "{:?}",
                String::from_utf8_lossy(target)
            );
        }
        assert_eq!(resolve(b"luke", &census), Target::Slot(1));
        assert_eq!(resolve(b"yoda", &census), Target::UnknownName);
        // Only colour codes: nothing to match.
        assert_eq!(resolve(b"^1", &census), Target::EmptyName);
    }

    #[test]
    fn disconnected_slots_are_not_players() {
        let census = census_of(&[(false, "Darth"), (true, "Luke")]);
        assert_eq!(resolve(b"darth", &census), Target::UnknownName);
        assert_eq!(resolve(b"0", &census), Target::EmptySlot(0));
        // Colour codes on a disconnected name give an empty query.
        assert_eq!(resolve(b"^1", &census), Target::EmptyName);
    }

    #[test]
    fn any_part_of_a_name_matches() {
        let census = census_of(&[(true, "Darth Vader"), (true, "Luke")]);
        // Substring anywhere in the name, in any case.
        for target in [&b"ade"[..], b"ADE", b"th va", b"vad"] {
            assert_eq!(
                resolve(target, &census),
                Target::Slot(0),
                "{:?}",
                String::from_utf8_lossy(target)
            );
        }
        assert_eq!(resolve(b"uke", &census), Target::Slot(1));
        // Not a part of any name.
        assert_eq!(resolve(b"yoda", &census), Target::UnknownName);
    }

    #[test]
    fn exact_beats_prefix_beats_substring() {
        let census = census_of(&[(true, "Darth"), (true, "Darth Vader"), (true, "Lewdarth")]);
        // Exact name: "Darth".
        assert_eq!(resolve(b"darth", &census), Target::Slot(0));
        // Prefix of "Darth Vader" only.
        assert_eq!(resolve(b"darth v", &census), Target::Slot(1));
        // Only a substring of "Lewdarth".
        assert_eq!(resolve(b"lewd", &census), Target::Slot(2));
        // "arth" is a substring of all three: refused, never guessed.
        assert_eq!(resolve(b"arth", &census), Target::Ambiguous(vec![0, 1, 2]));
    }

    #[test]
    fn ambiguous_tier_lists_every_candidate() {
        let census = census_of(&[(true, "Darth"), (true, "Darker"), (true, "Luke")]);
        assert_eq!(resolve(b"dar", &census), Target::Ambiguous(vec![0, 1]));
        // Duplicate names are ambiguous too, never guessed.
        let duplicates = census_of(&[(true, "Darth"), (true, "darth")]);
        assert_eq!(
            resolve(b"Darth", &duplicates),
            Target::Ambiguous(vec![0, 1])
        );
    }

    #[test]
    fn ambiguity_reply_caps_the_list_to_fit_one_rcon_line() {
        let entries: Vec<(bool, String)> = (0..MAX_CLIENTS)
            .map(|slot| (true, format!("Raider{slot}")))
            .collect();
        let mut clients: Vec<ClientEntry> =
            (0..MAX_CLIENTS).map(|_| ClientEntry::default()).collect();
        for (slot, (connected, name)) in entries.iter().enumerate() {
            clients[slot].is_connected = *connected;
            utils::strncpyz(&mut clients[slot].clean_name, name.as_bytes());
        }
        let census = census_from(&clients);
        let slots: Vec<usize> = (0..MAX_CLIENTS).collect();
        let list = candidate_list(&slots, &census);
        assert!(list.starts_with("#0 \"Raider0\", #1 \"Raider1\""));
        assert!(list.ends_with(", +24 more (see status)"));
        // Eight names and their slots stay well inside the 1008-byte redirect
        // buffer, which is what makes the reply one readable line.
        assert!(list.len() < 400, "candidate list too long: {}", list.len());
    }

    #[test]
    fn mute_flips_the_flag_of_a_slot_only() {
        let mut clients: Vec<ClientEntry> =
            (0..MAX_CLIENTS).map(|_| ClientEntry::default()).collect();
        clients[3].is_connected = true;

        assert!(apply_target(&mut clients, 3, true));
        assert!(clients[3].muted);
        assert!(apply_target(&mut clients, 3, false));
        assert!(!clients[3].muted);

        // A bot never raises `GAME_CLIENT_BEGIN`, so the connect mirror stays
        // false for its slot; the mute must still apply (the census is what
        // validated the slot).
        assert!(apply_target(&mut clients, 4, true));
        assert!(clients[4].muted);

        // Out-of-table slots are refused.
        assert!(!apply_target(&mut clients, MAX_CLIENTS, true));

        // Muting one slot leaves the others alone.
        assert!(apply_target(&mut clients, 3, true));
        assert!(!clients[2].muted && clients[3].muted);
    }

    #[test]
    fn plan_mutes_and_reports_the_resolved_player() {
        let census = census_of(&[(true, "^1Darth"), (true, "Luke")]);
        // Colour codes are kept out of the reply; the slot is the clientNum the
        // admin will see in `status`.
        assert_eq!(
            plan(true, b"darth", &census),
            Action::SetMute {
                slot: 0,
                mute: true,
                lines: vec!["proxy: muted #0 \"Darth\"".to_string()],
            }
        );
        assert_eq!(
            plan(false, b"luke", &census),
            Action::SetMute {
                slot: 1,
                mute: false,
                lines: vec!["proxy: unmuted #1 \"Luke\"".to_string()],
            }
        );
    }

    #[test]
    fn plan_reports_an_ambiguous_name_with_every_candidate() {
        let census = census_of(&[(true, "Darth"), (true, "Darker"), (true, "Luke")]);
        assert_eq!(
            plan(true, b"dar", &census),
            Action::Reply(vec![
                "proxy: \"dar\" matches #0 \"Darth\", #1 \"Darker\"; use the client number."
                    .to_string()
            ])
        );
    }

    #[test]
    fn plan_reports_bad_targets_without_touching_them() {
        let census = census_of(&[(true, "Darth")]);
        assert_eq!(
            plan(true, b"9", &census),
            Action::Reply(vec!["proxy: client slot 9 has no player.".to_string()])
        );
        assert_eq!(
            plan(false, b"yoda", &census),
            Action::Reply(vec!["proxy: no player named \"yoda\".".to_string()])
        );
        assert_eq!(
            plan(true, b"^1", &census),
            Action::Reply(vec![
                "proxy: give a client number or a player name.".to_string()
            ])
        );
    }

    #[test]
    fn only_mute_and_unmute_are_ours() {
        assert_eq!(mute_direction(b"mute"), Some(true));
        assert_eq!(mute_direction(b"MUTE"), Some(true));
        assert_eq!(mute_direction(b"unmute"), Some(false));
        assert_eq!(mute_direction(b"UnMute"), Some(false));
        for command in [&b""[..], b"addbot", b"say", b"mutes", b"unmuted"] {
            assert_eq!(
                mute_direction(command),
                None,
                "{:?}",
                String::from_utf8_lossy(command)
            );
        }
    }

    #[test]
    fn usage_lists_the_muted_players() {
        let empty = census_of(&[(true, "Darth")]);
        assert_eq!(
            decide(true, b"", &empty),
            Action::Reply(vec![
                USAGE.to_string(),
                "proxy: no player is muted.".to_string()
            ])
        );

        let mut clients: Vec<ClientEntry> =
            (0..MAX_CLIENTS).map(|_| ClientEntry::default()).collect();
        clients[0].is_connected = true;
        utils::strncpyz(&mut clients[0].clean_name, b"^1Darth");
        clients[0].muted = true;
        clients[1].is_connected = true;
        utils::strncpyz(&mut clients[1].clean_name, b"Luke");
        clients[1].muted = true;
        assert_eq!(
            decide(false, b"", &census_from(&clients)),
            Action::Reply(vec![
                USAGE.to_string(),
                "proxy: muted players: #0 \"Darth\", #1 \"Luke\"".to_string()
            ])
        );
    }
}
