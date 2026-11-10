//! `callvote coinflip`: a real vote whose result is a server-wide coin flip.
//!
//! The game's own `Cmd_CallVote_f` (`original/jedi-academy-sdk/codemp/game/
//! g_cmds.c:1883-2065`) only accepts a fixed set of vote types. The proxy
//! claims `callvote coinflip` in `GAME_CLIENT_COMMAND`, starts the vote through
//! the same `level` fields and configstrings the game uses, and lets the game's
//! own `Cmd_Vote_f`/`CheckVote` handle the 30 s vote, the tally and the
//! pass/fail messages.
//!
//! The vote's command is the proxy-private `proxy_coinflip`. When the vote
//! passes, the game sends it through `G_SEND_CONSOLE_COMMAND`; the proxy drops
//! it and emits `svsay ^3Coin flip: ^7Heads!` (or `Tails`) instead, so every
//! player sees the result in chat as a server message.
//!
//! Like every proxy feature it needs `proxy_sv_enable 1`.

use core::ffi::{CStr, c_char, c_int};
use core::sync::atomic::{AtomicU32, Ordering};
use std::ffi::CString;

use crate::hooks::engine_sv::{read_i32, read_usize, write_i32};
use crate::hooks::game::client_clean_netname;
use crate::original;
use crate::sdk::{
    CS_VOTE_NO, CS_VOTE_STRING, CS_VOTE_TIME, CS_VOTE_YES, EXEC_APPEND, EXEC_NOW, GT_DUEL,
    GT_POWERDUEL, MAX_CLIENTS, MAX_NETNAME, MAX_STRING_CHARS, MAX_VOTE_COUNT,
    OFFSET_GCLIENT_MGAMEFLAGS, OFFSET_GCLIENT_PERS, OFFSET_GCLIENT_SESS, OFFSET_GENTITY_CLIENT,
    OFFSET_LEVEL_CLIENTS, OFFSET_LEVEL_MAXCLIENTS, OFFSET_LEVEL_TIME,
    OFFSET_LEVEL_VOTE_DISPLAY_STRING, OFFSET_LEVEL_VOTE_EXECUTE_TIME, OFFSET_LEVEL_VOTE_NO,
    OFFSET_LEVEL_VOTE_STRING, OFFSET_LEVEL_VOTE_TIME, OFFSET_LEVEL_VOTE_YES,
    OFFSET_PERS_VOTE_COUNT, OFFSET_SESS_SESSION_TEAM, PSG_VOTED, SIZEOF_GCLIENT, SIZEOF_GENTITY,
    TEAM_SPECTATOR,
};
use crate::state;
use crate::syscall;
use crate::utils;

/// What the game runs when the vote passes. Kept unique so no normal vote can
/// collide with it.
const VOTE_COMMAND: &str = "proxy_coinflip";
/// What the vote UI shows.
const VOTE_DISPLAY: &str = "coinflip";

/// Claim `callvote coinflip` and start the vote. Returns `true` when the
/// command is ours, in which case the caller must not forward it to the game.
pub fn handle_client_command(client_num: c_int, command: &[u8], argument: &[u8]) -> bool {
    if !is_coinflip_vote(command, argument) {
        return false;
    }
    start_vote(client_num);
    true
}

/// `callvote coinflip`, case-insensitively, like the game's `Q_stricmp` tests.
fn is_coinflip_vote(command: &[u8], argument: &[u8]) -> bool {
    command.eq_ignore_ascii_case(b"callvote") && argument.eq_ignore_ascii_case(b"coinflip")
}

/// `G_SEND_CONSOLE_COMMAND` helper: read the text pointer safely (the game may
/// pass null). Returns `false` when the command must be dropped.
pub fn console_command_from_ptr(exec: i32, text: *const c_char) -> bool {
    if text.is_null() {
        return true;
    }
    // SAFETY: the game passes a NUL-terminated command string.
    console_command(exec, unsafe { CStr::from_ptr(text) }.to_bytes())
}

/// `G_SEND_CONSOLE_COMMAND` hook: a passed coinflip vote executes
/// [`VOTE_COMMAND`]. Drop that command and announce the random result through
/// `svsay` instead.
fn console_command(exec: i32, text: &[u8]) -> bool {
    if !is_coinflip_execution(exec, text) {
        return true;
    }
    announce_result(flip_is_heads());
    false
}

/// Whether `exec`/`text` is the coinflip vote's execution command.
fn is_coinflip_execution(exec: i32, text: &[u8]) -> bool {
    exec == EXEC_APPEND
        && text
            .trim_ascii()
            .eq_ignore_ascii_case(VOTE_COMMAND.as_bytes())
}

/// Why a coinflip vote may not start, in the same order the game's
/// `Cmd_CallVote_f` checks them.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    Disabled,
    InProgress,
    TooManyVotes,
    Spectator,
}

impl Refusal {
    fn message(self) -> &'static str {
        match self {
            Refusal::Disabled => "Voting is disabled on this server.",
            Refusal::InProgress => "A vote is already in progress.",
            Refusal::TooManyVotes => "You have called too many votes.",
            Refusal::Spectator => "Spectators cannot call votes.",
        }
    }
}

/// Gate a call exactly like `Cmd_CallVote_f` before touching the vote state.
fn refusal(
    allow_vote: bool,
    in_progress: bool,
    vote_count: i32,
    gametype: i32,
    session_team: i32,
) -> Option<Refusal> {
    if !allow_vote {
        return Some(Refusal::Disabled);
    }
    if in_progress {
        return Some(Refusal::InProgress);
    }
    if vote_count >= MAX_VOTE_COUNT {
        return Some(Refusal::TooManyVotes);
    }
    if gametype != GT_DUEL && gametype != GT_POWERDUEL && session_team == TEAM_SPECTATOR {
        return Some(Refusal::Spectator);
    }
    None
}

/// Start the vote in the game's own `level` state: the same fields and
/// configstrings `Cmd_CallVote_f` sets after its validation.
fn start_vote(client_num: c_int) {
    let level = original::level_address();
    if level == 0 {
        return;
    }

    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
        return;
    }
    // SAFETY: the client slot is in range (`shared_api::client_command`) and
    // g_entities is the located game array; a client command only runs for a
    // live player.
    let client = unsafe {
        read_usize(g_entities + client_num as usize * SIZEOF_GENTITY + OFFSET_GENTITY_CLIENT)
    };
    if client == 0 {
        return;
    }

    // SAFETY: `level` is the original module's live `level_locals_t`; the
    // client pointer above is a live `gclient_t`.
    let (now, vote_time, vote_execute_time, session_team, vote_count) = unsafe {
        (
            read_i32(level + OFFSET_LEVEL_TIME),
            read_i32(level + OFFSET_LEVEL_VOTE_TIME),
            read_i32(level + OFFSET_LEVEL_VOTE_EXECUTE_TIME),
            read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM),
            read_i32(client + OFFSET_GCLIENT_PERS + OFFSET_PERS_VOTE_COUNT),
        )
    };

    // SAFETY: engine syscall registered; the cvars are the game's own.
    let allow_vote = unsafe { syscall::cvar_variable_integer_value(c"g_allowVote") } != 0;
    // SAFETY: engine syscall registered.
    let gametype = unsafe { syscall::cvar_variable_integer_value(c"g_gametype") };

    if let Some(reason) = refusal(
        allow_vote,
        vote_time != 0 || vote_execute_time != 0,
        vote_count,
        gametype,
        session_team,
    ) {
        reply(client_num, reason.message());
        return;
    }

    // SAFETY: every address below is a field of the live `level_locals_t` or
    // of a live `gclient_t` in `level.clients`.
    unsafe {
        write_i32(level + OFFSET_LEVEL_VOTE_TIME, now);
        write_i32(level + OFFSET_LEVEL_VOTE_EXECUTE_TIME, 0);
        // The caller automatically votes yes, as `Cmd_CallVote_f` does.
        write_i32(level + OFFSET_LEVEL_VOTE_YES, 1);
        write_i32(level + OFFSET_LEVEL_VOTE_NO, 0);
        write_level_string(level + OFFSET_LEVEL_VOTE_STRING, VOTE_COMMAND.as_bytes());
        write_level_string(
            level + OFFSET_LEVEL_VOTE_DISPLAY_STRING,
            VOTE_DISPLAY.as_bytes(),
        );

        let clients = read_usize(level + OFFSET_LEVEL_CLIENTS);
        let maxclients =
            read_i32(level + OFFSET_LEVEL_MAXCLIENTS).clamp(0, MAX_CLIENTS as i32) as usize;
        if clients != 0 {
            for slot in 0..maxclients {
                let flags = clients + slot * SIZEOF_GCLIENT + OFFSET_GCLIENT_MGAMEFLAGS;
                write_i32(flags, read_i32(flags) & !(PSG_VOTED as i32));
            }
        }
        let flags = client + OFFSET_GCLIENT_MGAMEFLAGS;
        write_i32(flags, read_i32(flags) | PSG_VOTED as i32);
    }

    set_configstring(CS_VOTE_TIME, &now.to_string());
    set_configstring(CS_VOTE_STRING, VOTE_DISPLAY);
    set_configstring(CS_VOTE_YES, "1");
    set_configstring(CS_VOTE_NO, "0");

    announce_vote_called(client_num);
}

/// Copy `value` into a `MAX_STRING_CHARS` string field of the game's `level`.
///
/// # Safety
///
/// `offset` must point to a writable `char[MAX_STRING_CHARS]` field of the live
/// `level_locals_t`.
unsafe fn write_level_string(offset: usize, value: &[u8]) {
    // SAFETY: caller guarantees the field is a writable MAX_STRING_CHARS array.
    let field = unsafe { core::slice::from_raw_parts_mut(offset as *mut u8, MAX_STRING_CHARS) };
    utils::strncpyz(field, value);
}

fn set_configstring(index: i32, value: &str) {
    let Ok(value) = CString::new(value) else {
        return;
    };
    // SAFETY: engine syscall registered; `value` is NUL-terminated.
    unsafe { syscall::set_configstring(index, &value) };
}

/// Tell everyone a vote was called, as the game does (`Cmd_CallVote_f`).
fn announce_vote_called(client_num: c_int) {
    let mut raw = [0u8; MAX_NETNAME];
    client_clean_netname(client_num, &mut raw);
    let name = String::from_utf8_lossy(&raw[..utils::c_strlen(&raw)]);
    let name = sanitize_name(&name);
    send_print(-1, &format!("{name}^7 called a vote."));
}

/// A name goes inside `print "..."`: drop quotes and controls.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            '"' => '\'',
            ch if ch.is_control() => ' ',
            ch => ch,
        })
        .collect()
}

fn reply(client_num: c_int, text: &str) {
    send_print(client_num, &format!("^3{text}^7"));
}

fn send_print(client_num: c_int, text: &str) {
    let Ok(message) = CString::new(format!("print \"{text}\n\"")) else {
        return;
    };
    // SAFETY: engine syscall registered; `message` is NUL-terminated.
    unsafe { syscall::send_server_command(client_num, &message) };
}

/// What `svsay` shows after a passed vote.
fn result_name(heads: bool) -> &'static str {
    if heads { "Heads" } else { "Tails" }
}

/// `svsay ^3Coin flip: ^7Heads!` — the engine prepends `Server: `.
fn svsay_command(heads: bool) -> String {
    format!("svsay ^3Coin flip: ^7{}!\n", result_name(heads))
}

/// Announce the result to every player in chat.
fn announce_result(heads: bool) {
    let Ok(command) = CString::new(svsay_command(heads)) else {
        return;
    };
    // SAFETY: engine syscall registered; `command` is NUL-terminated.
    unsafe { syscall::send_console_command(EXEC_NOW, &command) };
}

/// Monotonic counter for the no-`/dev/urandom` fallback.
static FALLBACK_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Map one entropy byte to a side: bit 0 leaves both sides exactly 50 %.
fn flip_from_entropy(byte: u8) -> bool {
    byte & 1 == 0
}

/// One fair coin: bit 0 of an entropy byte, so either side is exactly 50 %.
fn flip_is_heads() -> bool {
    let mut entropy = [0u8; 1];
    if crate::challenge::fill_random(&mut entropy) {
        return flip_from_entropy(entropy[0]);
    }

    // No entropy source: mix the millisecond clock with a counter. This is not
    // cryptographic, but a coin flip does not need to be.
    let counter = FALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mixed = (syscall::milliseconds() ^ counter.wrapping_mul(0x9E37_79B9)).rotate_left(17);
    flip_from_entropy(mixed as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_only_callvote_coinflip() {
        assert!(is_coinflip_vote(b"callvote", b"coinflip"));
        assert!(is_coinflip_vote(b"CallVote", b"CoinFlip"));
        assert!(is_coinflip_vote(b"CALLVOTE", b"COINFLIP"));
    }

    #[test]
    fn leaves_other_commands_to_the_game() {
        assert!(!is_coinflip_vote(b"vote", b"coinflip"));
        assert!(!is_coinflip_vote(b"callvote", b"map_restart"));
        assert!(!is_coinflip_vote(b"callvote", b"coinflip2"));
        assert!(!is_coinflip_vote(b"callvote", b""));
        assert!(!is_coinflip_vote(b"say", b"callvote coinflip"));
    }

    #[test]
    fn only_the_append_execution_is_the_coinflip_vote() {
        assert!(is_coinflip_execution(EXEC_APPEND, b"proxy_coinflip\n"));
        assert!(is_coinflip_execution(EXEC_APPEND, b"  proxy_coinflip \r\n"));
        assert!(!is_coinflip_execution(EXEC_NOW, b"proxy_coinflip\n"));
        assert!(!is_coinflip_execution(EXEC_APPEND, b"proxy_coinflip2\n"));
        assert!(!is_coinflip_execution(EXEC_APPEND, b"map_restart\n"));
    }

    #[test]
    fn refusal_matches_the_games_check_order() {
        assert_eq!(
            refusal(false, true, 99, GT_DUEL, TEAM_SPECTATOR),
            Some(Refusal::Disabled)
        );
        assert_eq!(
            refusal(true, true, 99, GT_DUEL, TEAM_SPECTATOR),
            Some(Refusal::InProgress)
        );
        assert_eq!(
            refusal(true, false, MAX_VOTE_COUNT, GT_DUEL, TEAM_SPECTATOR),
            Some(Refusal::TooManyVotes)
        );
        assert_eq!(
            refusal(true, false, 0, GT_DUEL, TEAM_SPECTATOR),
            None,
            "duel lets spectators call votes"
        );
        assert_eq!(refusal(true, false, 0, GT_POWERDUEL, TEAM_SPECTATOR), None);
        assert_eq!(
            refusal(true, false, 0, crate::sdk::GT_TEAM, TEAM_SPECTATOR),
            Some(Refusal::Spectator)
        );
        assert_eq!(
            refusal(true, false, 0, crate::sdk::GT_TEAM, crate::sdk::TEAM_RED),
            None
        );
    }

    #[test]
    fn svsay_names_the_result() {
        assert_eq!(svsay_command(true), "svsay ^3Coin flip: ^7Heads!\n");
        assert_eq!(svsay_command(false), "svsay ^3Coin flip: ^7Tails!\n");
    }

    #[test]
    fn names_are_safe_inside_a_print_command() {
        assert_eq!(sanitize_name("Darth \"Vader\"\n"), "Darth 'Vader' ");
    }

    #[test]
    fn entropy_bit_selects_both_sides() {
        assert!(flip_from_entropy(0));
        assert!(!flip_from_entropy(1));
        assert!(flip_from_entropy(0xFE));
        assert!(!flip_from_entropy(0xFF));
    }
}
