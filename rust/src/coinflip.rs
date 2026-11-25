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

use crate::sdk::{EXEC_APPEND, EXEC_NOW};
use crate::state::{self, CVAR_COINFLIP};
use crate::syscall;

/// What the game runs when the vote passes. Kept unique so no normal vote can
/// collide with it.
const VOTE_COMMAND: &str = "proxy_coinflip";
/// What the vote UI shows.
const VOTE_DISPLAY: &str = "coinflip";

/// Whether the coin-flip vote is enabled (`proxy_sv_coinflip`, default `1`).
/// The master switch is checked by the callers through `shared_api`, but the
/// cvar is also read here so the join notice and the command agree.
pub(crate) fn enabled() -> bool {
    state::proxy_enabled() && state::cvar_integer(CVAR_COINFLIP) != 0
}

/// Claim `callvote coinflip` and start the vote. Returns `true` when the
/// command is ours, in which case the caller must not forward it to the game.
pub fn handle_client_command(client_num: c_int, command: &[u8], argument: &[u8]) -> bool {
    if !is_coinflip_vote(command, argument) {
        return false;
    }
    // Disabled: let the pristine game reject the unknown vote name.
    if !enabled() {
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
    if !enabled() {
        crate::votes::send_print(-1, "^3Coin flip vote canceled: the feature is disabled.");
        return false;
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

/// Start the vote through the shared proxy-vote machinery (`crate::votes`).
fn start_vote(client_num: c_int) {
    if let Err(reason) = crate::votes::start_proxy_vote(client_num, VOTE_COMMAND, VOTE_DISPLAY) {
        crate::votes::reply(client_num, reason.message());
    }
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
    fn svsay_names_the_result() {
        assert_eq!(svsay_command(true), "svsay ^3Coin flip: ^7Heads!\n");
        assert_eq!(svsay_command(false), "svsay ^3Coin flip: ^7Tails!\n");
    }

    #[test]
    fn entropy_bit_selects_both_sides() {
        assert!(flip_from_entropy(0));
        assert!(!flip_from_entropy(1));
        assert!(flip_from_entropy(0xFE));
        assert!(!flip_from_entropy(0xFF));
    }
}
