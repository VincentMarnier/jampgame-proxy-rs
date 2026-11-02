//! S14 (11-01 F8): a kick vote must kick the client it was called about, not
//! whoever holds the slot when it runs.
//!
//! The game stores a slot number, not a client: `callvote kick <name>` and
//! `callvote clientkick <n>` both build `clientkick <slot>`, the vote lasts up
//! to 30 s, and the game waits 3 more seconds before sending
//! `EXEC_APPEND clientkick <slot>` to the engine. The target can leave during
//! that window; after `sv_zombietime` (2 s) the slot is free, and the next
//! client (or a `bot_minplayers` bot) that takes it would be kicked instead.
//!
//! The proxy snapshots every slot's `netadr_t` when the game sets a non-empty
//! `CS_VOTE_TIME` (vote start), and drops the vote's `EXEC_APPEND clientkick`
//! when the slot's address is no longer the same. The game's automatic
//! team-killer kick uses `EXEC_INSERT` and still targets the current client of
//! the slot on purpose.
//!
//! Limits, as upstream: a target who comes back into the same slot with the
//! same address is still kicked; a bot has a zero/`NA_BOT` address, so a bot
//! that replaces a bot is still kicked.

use core::ffi::{CStr, c_char};
use std::ffi::CString;
use std::sync::Mutex;

use crate::hooks::engine_sv;
use crate::sdk::{CS_VOTE_TIME, EXEC_APPEND, MAX_CLIENTS};
use crate::syscall;
use crate::utils;

/// One 20-byte `netadr_t` per slot, copied at vote start; all-zero for a slot
/// outside `sv_maxclients`.
static VOTE_ADDRESSES: Mutex<[[u8; 20]; MAX_CLIENTS]> = Mutex::new([[0u8; 20]; MAX_CLIENTS]);

fn maxclients() -> usize {
    // SAFETY: engine syscall registered; the cvar is the engine's
    // `sv_maxclients` (latched).
    unsafe { syscall::cvar_variable_integer_value(c"sv_maxclients") }.clamp(0, MAX_CLIENTS as i32)
        as usize
}

/// `G_SET_CONFIGSTRING` hook: when `CS_VOTE_TIME` becomes non-empty (a vote
/// starts), copy every slot's address. An empty value (vote end) is ignored.
pub fn on_configstring(num: i32, value: &[u8]) {
    if num != CS_VOTE_TIME || value.is_empty() {
        return;
    }

    let maxclients = maxclients();
    let mut snapshot = VOTE_ADDRESSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    for (slot, entry) in snapshot.iter_mut().enumerate() {
        *entry = if slot < maxclients {
            // SAFETY: engine memory layer initialised; slot < sv_maxclients.
            unsafe { engine_sv::client_address_bytes(slot) }
        } else {
            [0u8; 20]
        };
    }
}

/// Whether `exec`/`text` is the vote's kick command (`EXEC_APPEND` and the
/// `clientkick ` prefix, case-insensitive: the game keeps the client's case).
fn is_vote_clientkick(exec: i32, text: &[u8]) -> bool {
    exec == EXEC_APPEND && text.len() >= 11 && text[..11].eq_ignore_ascii_case(b"clientkick ")
}

/// The slot of a `clientkick <slot>` text, or `None` when it is outside the
/// valid client slots.
fn vote_kick_slot(text: &[u8]) -> Option<usize> {
    let slot = utils::atoi(text.get(11..).unwrap_or(&[]));
    if (0..MAX_CLIENTS as i32).contains(&slot) {
        Some(slot as usize)
    } else {
        None
    }
}

/// Whether the slot changed hands: the current 20-byte address differs from
/// the snapshot taken at vote start.
fn slot_changed(snapshot: &[[u8; 20]; MAX_CLIENTS], slot: usize, current: &[u8; 20]) -> bool {
    snapshot[slot] != *current
}

/// `G_SEND_CONSOLE_COMMAND` hook: returns `false` when the vote's kick must be
/// dropped because the slot no longer holds the client it held at vote start.
pub fn console_command(exec: i32, text: &[u8]) -> bool {
    if !is_vote_clientkick(exec, text) {
        return true;
    }
    let Some(slot) = vote_kick_slot(text) else {
        return true;
    };
    let maxclients = maxclients();
    if slot >= maxclients {
        return true; // the engine rejects a slot outside sv_maxclients itself
    }

    // SAFETY: engine memory layer initialised; slot < sv_maxclients.
    let current = unsafe { engine_sv::client_address_bytes(slot) };
    let snapshot = VOTE_ADDRESSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if !slot_changed(&snapshot, slot, &current) {
        return true;
    }

    // Every player sees it, as upstream.
    // SAFETY: static text has no interior NUL; engine syscall registered.
    let message = CString::new("print \"Vote kick canceled: the player left the server.\n\"")
        .expect("static message");
    unsafe { syscall::send_server_command(-1, &message) };

    false
}

/// `G_SEND_CONSOLE_COMMAND(int exec, const char *text)` helper: read the text
/// pointer safely (the game may pass null).
pub fn console_command_from_ptr(exec: i32, text: *const c_char) -> bool {
    if text.is_null() {
        return true;
    }
    // SAFETY: the game passes a NUL-terminated command string.
    console_command(exec, unsafe { CStr::from_ptr(text) }.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exec_append_clientkick_is_the_vote() {
        assert!(is_vote_clientkick(EXEC_APPEND, b"clientkick 3\n"));
        assert!(is_vote_clientkick(EXEC_APPEND, b"ClientKick 12"));
        assert!(!is_vote_clientkick(1, b"clientkick 3\n"), "EXEC_INSERT");
        assert!(!is_vote_clientkick(EXEC_APPEND, b"kick 3\n"));
        assert!(!is_vote_clientkick(EXEC_APPEND, b"clientkick"));
        assert!(!is_vote_clientkick(EXEC_APPEND, b"clientkickx 3"));
    }

    #[test]
    fn the_slot_is_parsed_and_ranged() {
        assert_eq!(vote_kick_slot(b"clientkick 3\n"), Some(3));
        assert_eq!(vote_kick_slot(b"clientkick 31"), Some(31));
        assert_eq!(vote_kick_slot(b"clientkick 32"), None);
        assert_eq!(vote_kick_slot(b"clientkick -1"), None);
        assert_eq!(vote_kick_slot(b"clientkick abc"), Some(0), "atoi semantics");
    }

    #[test]
    fn a_changed_slot_address_drops_the_kick() {
        let mut snapshot = [[0u8; 20]; MAX_CLIENTS];
        snapshot[3] = [4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let same = [4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let other = [4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

        assert!(!slot_changed(&snapshot, 3, &same));
        assert!(slot_changed(&snapshot, 3, &other));
        // An empty snapshot (no vote seen) differs from a real client.
        assert!(slot_changed(&snapshot, 4, &same));
    }
}
