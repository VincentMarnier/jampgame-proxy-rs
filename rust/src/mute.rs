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
use core::sync::atomic::{AtomicI32, Ordering};
use std::ffi::CString;

use crate::player_target::{self, Census, Target};
use crate::sdk::MAX_CLIENTS;
use crate::state::{self, ClientEntry};
use crate::syscall;

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
/// Shown when `proxy_sv_muteMode` is `0`.
const DISABLED: &str = "proxy: mute is disabled (proxy_sv_muteMode 0).";

// ---------------------------------------------------------------------------
// proxy_sv_muteMode
// ---------------------------------------------------------------------------

/// `proxy_sv_muteMode` values (D-011).
pub const MUTE_MODE_OFF: i32 = 0;
pub const MUTE_MODE_NORMAL: i32 = 1;
pub const MUTE_MODE_SILENT: i32 = 2;

/// How `proxy_sv_muteMode` gates the feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuteMode {
    /// `0`: no mute capability. `mute`/`unmute` answer that the feature is off
    /// and no chat is filtered, even for a slot still carrying a flag.
    Off,
    /// `1` (default): the stock mute. Chat/voice commands are dropped and the
    /// player is told they are muted.
    Normal,
    /// `2`: silent mute. Text chat reaches the game so the player sees their
    /// own line, but only the speaker receives it; no notice is sent. Voice is
    /// dropped without a word (it has no chat text to echo).
    Silent,
}

/// The mode held by the `proxy_sv_muteMode` mirror. Values outside `0..=2`
/// keep the stock mute, like the proxy did before the cvar existed.
pub fn mode() -> MuteMode {
    mode_from_integer(state::with_state(|s| {
        s.cvars[state::CVAR_MUTE_MODE].integer
    }))
}

/// Pure mapping from the cvar's integer to [`MuteMode`].
fn mode_from_integer(value: i32) -> MuteMode {
    match value {
        MUTE_MODE_OFF => MuteMode::Off,
        MUTE_MODE_SILENT => MuteMode::Silent,
        MUTE_MODE_NORMAL => MuteMode::Normal,
        // Unknown values keep the stock behaviour too.
        _ => MuteMode::Normal,
    }
}

/// The notice an admin action sends to the target, or `None` when the mode is
/// silent/off (a silent mute must not reveal itself).
fn change_notice(mode: MuteMode, mute: bool) -> Option<&'static std::ffi::CStr> {
    match mode {
        MuteMode::Normal => Some(if mute { MUTE_NOTICE } else { UNMUTE_NOTICE }),
        MuteMode::Off | MuteMode::Silent => None,
    }
}

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
    let Some(mute) = mute_direction(&player_target::arg(0)) else {
        return false;
    };
    // A disabled feature still claims its own two commands: forwarding them
    // would make the pristine `ConsoleCommand` broadcast `server: mute …` to
    // every client.
    let mode = mode();
    if mode == MuteMode::Off {
        print_line(DISABLED);
        return true;
    }
    match decide(
        mute,
        &player_target::target_argument(),
        &player_target::census(),
    ) {
        Action::Reply(lines) => print_lines(&lines),
        Action::SetMute { slot, mute, lines } => {
            if !state::with_state(|s| apply_target(&mut s.clients, slot, mute)) {
                // The slot emptied between the census and the write
                // (single-threaded, so this is defensive only).
                print_line(&format!("proxy: client slot {slot} has no player."));
                return true;
            }
            if let Some(notice) = change_notice(mode, mute) {
                // SAFETY: engine syscall registered; the notices are
                // NUL-terminated literals.
                unsafe { syscall::send_server_command(slot as c_int, notice) };
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
    match player_target::resolve(target, census) {
        Target::Slot(slot) => Action::SetMute {
            slot,
            mute,
            lines: vec![format!(
                "proxy: {} #{slot} \"{}\"",
                if mute { "muted" } else { "unmuted" },
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

/// `usage:` line plus the players that are currently muted.
fn usage_and_muted_list(census: &Census) -> Vec<String> {
    let muted = (0..MAX_CLIENTS)
        .filter(|&slot| census.muted[slot] && census.connected[slot])
        .map(|slot| format!("#{slot} \"{}\"", player_target::display_name(census, slot)))
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

/// What `GAME_CLIENT_COMMAND` does with one command from a muted client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientCommandOutcome {
    /// Not a muted client's chat command (or the feature is off): forward it
    /// untouched.
    Forward,
    /// Forward it, but keep its chat deliveries to the speaker only (silent
    /// mute): the game still echoes the line back to its author.
    Shadow,
    /// Do not forward it — the game never sees the command.
    Drop,
}

/// The decision for one command from a client that carries a mute, independent
/// of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutedResponse {
    /// Not a command the mute covers.
    Forward,
    /// Drop it after telling the player (stock mute).
    DropWithNotice,
    /// Drop it without a word (silent mute's voice commands).
    DropSilently,
    /// Forward it and keep its chat to the speaker (silent mute's text chat).
    Shadow,
}

/// Handle a client command from `GAME_CLIENT_COMMAND`. [`ClientCommandOutcome`]
/// tells the caller whether to forward it, forward it in a shadow scope, or
/// drop it.
pub fn handle_client_command(client_num: c_int) -> ClientCommandOutcome {
    if !state::proxy_enabled() || !is_muted(client_num) {
        return ClientCommandOutcome::Forward;
    }
    match muted_response(mode(), &player_target::arg(0)) {
        MutedResponse::Forward => ClientCommandOutcome::Forward,
        MutedResponse::DropWithNotice => {
            // SAFETY: engine syscall registered; the notice is a NUL-terminated
            // literal.
            unsafe { syscall::send_server_command(client_num, MUTED_NOTICE) };
            ClientCommandOutcome::Drop
        }
        MutedResponse::DropSilently => ClientCommandOutcome::Drop,
        MutedResponse::Shadow => ClientCommandOutcome::Shadow,
    }
}

/// The mute decision for `command` in `mode`: only the four communication
/// commands are covered, and only the text three are shadowed in silent mode.
fn muted_response(mode: MuteMode, command: &[u8]) -> MutedResponse {
    if mode == MuteMode::Off || !is_chat_command(command) {
        return MutedResponse::Forward;
    }
    if mode == MuteMode::Silent {
        // Voice has no text line to echo back, so it stays dropped.
        if is_voice_command(command) {
            MutedResponse::DropSilently
        } else {
            MutedResponse::Shadow
        }
    } else {
        MutedResponse::DropWithNotice
    }
}

/// Whether `command` is one of the commands a mute suppresses.
fn is_chat_command(command: &[u8]) -> bool {
    CHAT_COMMANDS
        .iter()
        .any(|name| command.eq_ignore_ascii_case(name))
}

/// Whether `command` is the voice menu's command (the one chat command with no
/// text to show its author in silent mode).
fn is_voice_command(command: &[u8]) -> bool {
    command.eq_ignore_ascii_case(b"voice_cmd")
}

// ---------------------------------------------------------------------------
// Silent mute — the speaker's own chat echo
// ---------------------------------------------------------------------------

/// Client whose text chat is currently being forwarded in silent mode, or `-1`
/// when no such command is in flight. Set only around the synchronous
/// `GAME_CLIENT_COMMAND` forward, so the `G_SEND_SERVER_COMMAND` trap can tell
/// whose line it is.
static SHADOW_CLIENT: AtomicI32 = AtomicI32::new(-1);

/// Open the silent-chat scope for one forwarded client command. Returns the
/// previous value to hand back to [`end_shadow_scope`] (nesting-safe).
pub fn begin_shadow_scope(client_num: c_int) -> c_int {
    SHADOW_CLIENT.swap(client_num, Ordering::Relaxed)
}

/// Close a silent-chat scope opened by [`begin_shadow_scope`].
pub fn end_shadow_scope(previous: c_int) {
    SHADOW_CLIENT.store(previous, Ordering::Relaxed);
}

/// Whether a server command emitted during a silent-mute scope must be dropped
/// for `recipient`: only the speaking client may receive their own chat line.
///
/// Screened on bytes because the payload carries the client's raw chat text
/// (single-byte encoding, not necessarily UTF-8). A broadcast (`-1`) is not the
/// speaker, so it is dropped too; `G_Say` addresses each client individually.
pub fn shadow_chat_dropped(command: &[u8], recipient: c_int) -> bool {
    let sender = SHADOW_CLIENT.load(Ordering::Relaxed);
    // The common case: no silent-mute forwarding in flight at all, so this is
    // one atomic read on every `G_SEND_SERVER_COMMAND`.
    sender >= 0 && shadow_chat_dropped_in(command, recipient, sender, mode())
}

/// Pure form of [`shadow_chat_dropped`].
fn shadow_chat_dropped_in(command: &[u8], recipient: c_int, sender: c_int, mode: MuteMode) -> bool {
    mode == MuteMode::Silent && sender >= 0 && recipient != sender && is_chat_message(command)
}

/// Whether `command` is one of the server commands `G_Say`/`G_SayTo` deliver
/// chat with (`chat`, `tchat`, `lchat`, `ltchat`).
fn is_chat_message(command: &[u8]) -> bool {
    const PREFIXES: [&[u8]; 4] = [
        b"chat ".as_slice(),
        b"tchat ".as_slice(),
        b"lchat ".as_slice(),
        b"ltchat ".as_slice(),
    ];
    PREFIXES.iter().any(|prefix| command.starts_with(prefix))
}

/// Whether the client slot currently carries a mute.
fn is_muted(client_num: c_int) -> bool {
    (0..MAX_CLIENTS as c_int).contains(&client_num)
        && state::with_state(|s| s.clients[client_num as usize].muted)
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
    use crate::utils;

    /// Build a census with `(connected, name)` per slot, the rest empty.
    fn census_of(entries: &[(bool, &str)]) -> Census {
        let mut clients: Vec<ClientEntry> =
            (0..MAX_CLIENTS).map(|_| ClientEntry::default()).collect();
        for (slot, (connected, name)) in entries.iter().enumerate() {
            clients[slot].is_connected = *connected;
            utils::strncpyz(&mut clients[slot].clean_name, name.as_bytes());
        }
        player_target::census_from(&clients)
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
            decide(false, b"", &player_target::census_from(&clients)),
            Action::Reply(vec![
                USAGE.to_string(),
                "proxy: muted players: #0 \"Darth\", #1 \"Luke\"".to_string()
            ])
        );
    }

    #[test]
    fn mute_mode_maps_the_cvar_values() {
        assert_eq!(mode_from_integer(MUTE_MODE_OFF), MuteMode::Off);
        assert_eq!(mode_from_integer(MUTE_MODE_NORMAL), MuteMode::Normal);
        assert_eq!(mode_from_integer(MUTE_MODE_SILENT), MuteMode::Silent);
        // Anything else keeps the stock behaviour, like the pre-cvar proxy.
        for value in [-1, 3, 42] {
            assert_eq!(mode_from_integer(value), MuteMode::Normal, "{value}");
        }
    }

    #[test]
    fn silent_and_off_modes_never_notify_the_player() {
        assert_eq!(change_notice(MuteMode::Normal, true), Some(MUTE_NOTICE));
        assert_eq!(change_notice(MuteMode::Normal, false), Some(UNMUTE_NOTICE));
        assert_eq!(change_notice(MuteMode::Silent, true), None);
        assert_eq!(change_notice(MuteMode::Silent, false), None);
        assert_eq!(change_notice(MuteMode::Off, true), None);
        assert_eq!(change_notice(MuteMode::Off, false), None);
    }

    #[test]
    fn silent_mode_shadows_text_chat_and_drops_voice_silently() {
        for command in [&b"say"[..], b"say_team", b"TELL"] {
            assert_eq!(
                muted_response(MuteMode::Silent, command),
                MutedResponse::Shadow,
                "{:?}",
                String::from_utf8_lossy(command)
            );
        }
        assert_eq!(
            muted_response(MuteMode::Silent, b"voice_cmd"),
            MutedResponse::DropSilently
        );
        // The stock mute still drops everything with a notice.
        for command in [&b"say"[..], b"say_team", b"tell", b"voice_cmd"] {
            assert_eq!(
                muted_response(MuteMode::Normal, command),
                MutedResponse::DropWithNotice
            );
        }
        // Off never filters; non-chat commands are always forwarded.
        assert_eq!(
            muted_response(MuteMode::Off, b"say"),
            MutedResponse::Forward
        );
        for mode in [MuteMode::Off, MuteMode::Normal, MuteMode::Silent] {
            for command in [&b""[..], b"kill", b"say_team2", b"admin_say"] {
                assert_eq!(
                    muted_response(mode, command),
                    MutedResponse::Forward,
                    "{mode:?} {:?}",
                    String::from_utf8_lossy(command)
                );
            }
        }
    }

    #[test]
    fn shadow_scope_keeps_only_the_speakers_chat() {
        let say = b"chat \"^1Darth^7: hi\"";
        // Every other recipient is dropped, broadcasts included.
        for recipient in [1, 2, -1, MAX_CLIENTS as c_int] {
            assert!(
                shadow_chat_dropped_in(say, recipient, 0, MuteMode::Silent),
                "recipient {recipient}"
            );
        }
        // The speaker keeps their own echo.
        assert!(!shadow_chat_dropped_in(say, 0, 0, MuteMode::Silent));
        // Team and local variants are chat too.
        for command in [
            &b"tchat \"^1Darth^7: hi\""[..],
            b"lchat \"Darth\" \"Yavin\" \"^4\" \"hi\"",
            b"ltchat \"Darth\" \"Yavin\" \"^4\" \"hi\"",
        ] {
            assert!(
                shadow_chat_dropped_in(command, 1, 0, MuteMode::Silent),
                "{:?}",
                String::from_utf8_lossy(command)
            );
        }
        // Non-UTF-8 chat is screened on its bytes.
        assert!(shadow_chat_dropped_in(
            b"chat \"caf\xe9\"",
            1,
            0,
            MuteMode::Silent
        ));
    }

    #[test]
    fn shadow_scope_leaves_other_messages_and_modes_alone() {
        let say = b"chat \"Darth: hi\"";
        // Only chat commands are filtered.
        for command in [
            &b""[..],
            b"print \"chat hi\"",
            b"tchatty",
            b"scores \"1 0\"",
            b"cp \"chat hi\"",
        ] {
            assert!(
                !shadow_chat_dropped_in(command, 1, 0, MuteMode::Silent),
                "{:?}",
                String::from_utf8_lossy(command)
            );
        }
        // No scope, or the feature in another mode: untouched.
        assert!(!shadow_chat_dropped_in(say, 1, -1, MuteMode::Silent));
        assert!(!shadow_chat_dropped_in(say, 1, 0, MuteMode::Normal));
        assert!(!shadow_chat_dropped_in(say, 1, 0, MuteMode::Off));
    }
}
