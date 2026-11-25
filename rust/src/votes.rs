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

use core::ffi::{CStr, c_char, c_int};
use std::ffi::CString;
use std::sync::Mutex;

use crate::hooks::engine_sv::{self, read_i32, read_usize, write_i32};
use crate::hooks::game::client_clean_netname;
use crate::original;
use crate::sdk::{
    CS_VOTE_NO, CS_VOTE_STRING, CS_VOTE_TIME, CS_VOTE_YES, EXEC_APPEND, GT_DUEL, GT_POWERDUEL,
    MAX_CLIENTS, MAX_NETNAME, MAX_STRING_CHARS, MAX_VOTE_COUNT, OFFSET_GCLIENT_MGAMEFLAGS,
    OFFSET_GCLIENT_PERS, OFFSET_GCLIENT_SESS, OFFSET_GENTITY_CLIENT, OFFSET_LEVEL_CLIENTS,
    OFFSET_LEVEL_MAXCLIENTS, OFFSET_LEVEL_TIME, OFFSET_LEVEL_VOTE_DISPLAY_STRING,
    OFFSET_LEVEL_VOTE_EXECUTE_TIME, OFFSET_LEVEL_VOTE_NO, OFFSET_LEVEL_VOTE_STRING,
    OFFSET_LEVEL_VOTE_TIME, OFFSET_LEVEL_VOTE_YES, OFFSET_PERS_VOTE_COUNT,
    OFFSET_SESS_SESSION_TEAM, PSG_VOTED, SIZEOF_GCLIENT, SIZEOF_GENTITY, TEAM_SPECTATOR,
};
use crate::state;
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

// ---------------------------------------------------------------------------
// Proxy-owned votes (`callvote coinflip`, `callvote shuffle`)
// ---------------------------------------------------------------------------

/// Why a proxy-owned vote may not start, in the same order the game's
/// `Cmd_CallVote_f` checks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VoteRefusal {
    Disabled,
    InProgress,
    TooManyVotes,
    Spectator,
}

impl VoteRefusal {
    pub(crate) fn message(self) -> &'static str {
        match self {
            VoteRefusal::Disabled => "Voting is disabled on this server.",
            VoteRefusal::InProgress => "A vote is already in progress.",
            VoteRefusal::TooManyVotes => "You have called too many votes.",
            VoteRefusal::Spectator => "Spectators cannot call votes.",
        }
    }
}

/// Gate a call exactly like `Cmd_CallVote_f` before touching the vote state.
pub(crate) fn refusal(
    allow_vote: bool,
    in_progress: bool,
    vote_count: i32,
    gametype: i32,
    session_team: i32,
) -> Option<VoteRefusal> {
    if !allow_vote {
        return Some(VoteRefusal::Disabled);
    }
    if in_progress {
        return Some(VoteRefusal::InProgress);
    }
    if vote_count >= MAX_VOTE_COUNT {
        return Some(VoteRefusal::TooManyVotes);
    }
    if gametype != GT_DUEL && gametype != GT_POWERDUEL && session_team == TEAM_SPECTATOR {
        return Some(VoteRefusal::Spectator);
    }
    None
}

/// Start a proxy-owned vote in the game's own `level` state: the same fields
/// and configstrings `Cmd_CallVote_f` sets after its validation. `command` is
/// what the game will run when the vote passes (a proxy-private name that
/// [`console_command`] turns into its effect); `display` is what the vote UI
/// shows.
pub(crate) fn start_proxy_vote(
    client_num: c_int,
    command: &str,
    display: &str,
) -> Result<(), VoteRefusal> {
    let level = original::level_address();
    if level == 0 {
        return Ok(());
    }

    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
        return Ok(());
    }
    // SAFETY: the client slot is in range (the caller checked `MAX_CLIENTS`)
    // and g_entities is the located game array; a client command only runs for
    // a live player.
    let client = unsafe {
        read_usize(g_entities + client_num as usize * SIZEOF_GENTITY + OFFSET_GENTITY_CLIENT)
    };
    if client == 0 {
        return Ok(());
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
        return Err(reason);
    }

    // SAFETY: every address below is a field of the live `level_locals_t` or
    // of a live `gclient_t` in `level.clients`.
    unsafe {
        write_i32(level + OFFSET_LEVEL_VOTE_TIME, now);
        write_i32(level + OFFSET_LEVEL_VOTE_EXECUTE_TIME, 0);
        // The caller automatically votes yes, as `Cmd_CallVote_f` does.
        write_i32(level + OFFSET_LEVEL_VOTE_YES, 1);
        write_i32(level + OFFSET_LEVEL_VOTE_NO, 0);
        write_level_string(level + OFFSET_LEVEL_VOTE_STRING, command.as_bytes());
        write_level_string(level + OFFSET_LEVEL_VOTE_DISPLAY_STRING, display.as_bytes());

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
    set_configstring(CS_VOTE_STRING, display);
    set_configstring(CS_VOTE_YES, "1");
    set_configstring(CS_VOTE_NO, "0");

    announce_vote_called(client_num);
    Ok(())
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

/// Send one coloured proxy line to one client (`-1` = everyone).
pub(crate) fn reply(client_num: c_int, text: &str) {
    send_print(client_num, &format!("^3{text}^7"));
}

/// Send one `print "..."` server command.
pub(crate) fn send_print(client_num: c_int, text: &str) {
    let Ok(message) = CString::new(format!("print \"{text}\n\"")) else {
        return;
    };
    // SAFETY: engine syscall registered; `message` is NUL-terminated.
    unsafe { syscall::send_server_command(client_num, &message) };
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

    #[test]
    fn refusal_matches_the_games_check_order() {
        assert_eq!(
            refusal(false, true, 99, GT_DUEL, TEAM_SPECTATOR),
            Some(VoteRefusal::Disabled)
        );
        assert_eq!(
            refusal(true, true, 99, GT_DUEL, TEAM_SPECTATOR),
            Some(VoteRefusal::InProgress)
        );
        assert_eq!(
            refusal(true, false, MAX_VOTE_COUNT, GT_DUEL, TEAM_SPECTATOR),
            Some(VoteRefusal::TooManyVotes)
        );
        assert_eq!(
            refusal(true, false, 0, GT_DUEL, TEAM_SPECTATOR),
            None,
            "duel lets spectators call votes"
        );
        assert_eq!(refusal(true, false, 0, GT_POWERDUEL, TEAM_SPECTATOR), None);
        assert_eq!(
            refusal(true, false, 0, crate::sdk::GT_TEAM, TEAM_SPECTATOR),
            Some(VoteRefusal::Spectator)
        );
        assert_eq!(
            refusal(true, false, 0, crate::sdk::GT_TEAM, crate::sdk::TEAM_RED),
            None
        );
    }

    #[test]
    fn names_are_safe_inside_a_print_command() {
        assert_eq!(sanitize_name("Darth \"Vader\"\n"), "Darth 'Vader' ");
    }
}
