//! Round-start team locking (`proxy_sv_lockTeams`, TDM/CTF).
//!
//! When a round starts with an even, populated roster (both teams at least two
//! players), each team is capped at its round-start size. The cap is a
//! *reservation*: it survives players leaving, so the freed slots stay open for
//! the same team instead of being taken by the other one. A round that starts
//! uneven (e.g. 2 vs 3) is not locked.
//!
//! The trigger is the reconnect burst the engine issues when a map loads or is
//! restarted (`SV_InitGameProgs`/`SV_RestartGameProgs`): every retained client
//! gets `GAME_CLIENT_CONNECT` with `firstTime == qfalse` *before* the next
//! `GAME_RUN_FRAME`, at which point the game module has restored the session
//! teams from the `session%i` cvars. Late joins use `firstTime == qtrue` and do
//! not retrigger the snapshot.
//!
//! Enforcement is a single `SetTeam` detour: an explicit `red`/`blue` request
//! for a full, locked team is dropped with a message, and so is an auto pick
//! (`team free`/empty, which the game routes through `PickTeam`) once *both*
//! locked teams are full. Spectator changes and the game's own internal
//! `SetTeam` calls go through unchanged.

use core::ffi::{CStr, c_char, c_int};
use std::ffi::CString;

use crate::hooks::engine_sv::{read_i32, read_usize};
use crate::hooks::{game_hook_original_set_team, original_call};
use crate::sdk::{
    CON_DISCONNECTED, GT_CTF, GT_TEAM, MAX_CLIENTS, OFFSET_GCLIENT_PERS, OFFSET_GCLIENT_SESS,
    OFFSET_GENTITY_CLIENT, OFFSET_PERS_CONNECTED, OFFSET_SESS_SESSION_TEAM, SIZEOF_GENTITY,
    TEAM_BLUE, TEAM_RED,
};
use crate::state::{self, CVAR_LOCK_TEAMS};
use crate::syscall;

/// `proxy_sv_lockTeams` gate.
fn feature_enabled() -> bool {
    state::with_state(|s| s.cvars[CVAR_LOCK_TEAMS].integer != 0)
}

/// The game's `g_gametype` (engine cvar trap) — only the team modes are
/// eligible.
fn gametype_supported() -> bool {
    // SAFETY: engine syscall registered (GAME_INIT ran before any hook fires).
    let gametype = unsafe { syscall::cvar_variable_integer_value(c"g_gametype") };
    gametype == GT_TEAM || gametype == GT_CTF
}

/// The round-start caps: both teams lock at their (equal) size only when each
/// fielded at least two players; otherwise no team is locked (`-1`).
pub fn snapshot_caps(red: i32, blue: i32) -> (i32, i32) {
    if red >= 2 && blue >= 2 && red == blue {
        (red, blue)
    } else {
        (-1, -1)
    }
}

/// Whether an auto pick (`team free`, an empty string, or any other request the
/// game routes through `PickTeam`) must be refused: it is refused only when
/// *every* locked team is already at its cap. A cap is a reservation, so while
/// one team is still below its cap the game's least-populated pick lands there
/// and the join is allowed.
pub fn auto_join_blocked(red_max: i32, blue_max: i32, red: i32, blue: i32) -> bool {
    let red_full = red_max >= 0 && red >= red_max;
    let blue_full = blue_max >= 0 && blue >= blue_max;
    red_full && blue_full
}

/// `GAME_CLIENT_CONNECT` bookkeeping: a `firstTime == qfalse` connect is part
/// of the map-load/map-restart reconnect burst, i.e. the round start.
pub fn on_client_connect(first_time: bool) {
    eprintln!("----- proxy-rs: teamlock connect first_time={first_time}");
    if first_time {
        return;
    }
    state::with_state(|s| s.team_lock.saw_reconnect = true);
}

/// `GAME_RUN_FRAME` bookkeeping: take the roster snapshot on the first frame
/// after the reconnect burst (all reconnects happen before that frame).
pub fn on_run_frame() {
    let ready = state::with_state(|s| s.team_lock.pending && s.team_lock.saw_reconnect);
    if !ready {
        return;
    }
    let gt = unsafe { syscall::cvar_variable_integer_value(c"g_gametype") };
    let (red, blue) = (team_count(TEAM_RED), team_count(TEAM_BLUE));
    let (red_max, blue_max) = if feature_enabled() && gametype_supported() {
        snapshot_caps(red, blue)
    } else {
        (-1, -1)
    };
    eprintln!(
        "----- proxy-rs: teamlock snapshot gt={gt} enabled={} red={red} blue={blue} -> {red_max} vs {blue_max}",
        feature_enabled()
    );
    state::with_state(|s| {
        s.team_lock.red_max = red_max;
        s.team_lock.blue_max = blue_max;
        s.team_lock.pending = false;
    });
}

/// Count a team exactly like the game's `TeamCount` (`g_client.c:1237`): every
/// connected client whose `sess.sessionTeam` matches.
fn team_count(team: i32) -> i32 {
    let base = state::with_state(|s| s.located_game_data.g_entities);
    if base == 0 {
        return 0;
    }
    let mut count = 0;
    for i in 0..MAX_CLIENTS {
        let ent = base + i * SIZEOF_GENTITY;
        // SAFETY: g_entities is a valid gentity_t array of the loaded game.
        let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
        if client == 0 {
            continue;
        }
        // SAFETY: client is a valid gclient_t; pers.connected is its first int.
        let connected = unsafe { read_i32(client + OFFSET_GCLIENT_PERS + OFFSET_PERS_CONNECTED) };
        if connected == CON_DISCONNECTED {
            continue;
        }
        // SAFETY: client is a valid gclient_t.
        if unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) } == team {
            count += 1;
        }
    }
    count
}

/// `(ent - g_entities) / sizeof(gentity_t)` — the trap index used for
/// `trap_SendServerCommand`.
fn ent_client_num(ent: usize) -> c_int {
    let base = state::with_state(|s| s.located_game_data.g_entities);
    if base == 0 || ent < base {
        return -1;
    }
    ((ent - base) / SIZEOF_GENTITY) as c_int
}

/// The team an explicit `red`/`blue` request names, or `None` when the request
/// is not a direct team join (spectator/follow/empty → the game picks a team).
fn requested_team(s: *const c_char) -> Option<i32> {
    if s.is_null() {
        return None;
    }
    // SAFETY: SetTeam's `s` is a NUL-terminated string (game prototype).
    let bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
    if ascii_eq_ignore_case(bytes, b"red") || ascii_eq_ignore_case(bytes, b"r") {
        Some(TEAM_RED)
    } else if ascii_eq_ignore_case(bytes, b"blue") || ascii_eq_ignore_case(bytes, b"b") {
        Some(TEAM_BLUE)
    } else {
        None
    }
}

/// Whether the request explicitly selects the spectator team — the exact arms
/// `SetTeam` treats as spectator (`g_cmds.c`). Everything else that is not an
/// explicit `red`/`blue` falls through to `PickTeam` (auto pick).
fn requested_spectator(s: *const c_char) -> bool {
    if s.is_null() {
        return false;
    }
    // SAFETY: SetTeam's `s` is a NUL-terminated string (game prototype).
    let bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
    const SPECTATOR_REQUESTS: [&[u8]; 6] = [
        b"spectator",
        b"s",
        b"score",
        b"scoreboard",
        b"follow1",
        b"follow2",
    ];
    SPECTATOR_REQUESTS
        .iter()
        .any(|request| ascii_eq_ignore_case(bytes, request))
}

fn ascii_eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// A dropped team join and the data the message needs.
struct Block {
    client_num: c_int,
    team: i32,
    red_max: i32,
    blue_max: i32,
}

/// Decide whether a `SetTeam(ent, s)` call must be dropped under the current
/// lock. An explicit join to a locked, full team is blocked, as is an auto pick
/// (`team free`/empty → `PickTeam`) once every locked team is full; spectator
/// changes, no-op same-team requests and the game's internal picks pass.
fn blocked_join(ent: usize, s: *const c_char) -> Option<Block> {
    if ent == 0 || !state::proxy_enabled() || !feature_enabled() {
        return None;
    }
    let explicit = requested_team(s);
    // Spectator/follow/scoreboard requests never field a playing team; leave
    // the transition to the original.
    if explicit.is_none() && requested_spectator(s) {
        return None;
    }
    // SAFETY: ent is a valid gentity_t* passed by the game.
    let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
    if client == 0 {
        return None;
    }
    // SAFETY: client is a valid gclient_t.
    let old_team = unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) };
    // The game no-ops the same-team request; leave it to the original.
    if explicit == Some(old_team) {
        return None;
    }
    let (red_max, blue_max) = state::with_state(|s| (s.team_lock.red_max, s.team_lock.blue_max));
    if red_max < 0 && blue_max < 0 {
        return None;
    }
    if !gametype_supported() {
        return None;
    }
    let (red_count, blue_count) = (team_count(TEAM_RED), team_count(TEAM_BLUE));
    let team = match explicit {
        Some(team) => {
            let (cap, count) = if team == TEAM_RED {
                (red_max, red_count)
            } else {
                (blue_max, blue_count)
            };
            if cap < 0 || count < cap {
                return None;
            }
            team
        }
        None => {
            if !auto_join_blocked(red_max, blue_max, red_count, blue_count) {
                return None;
            }
            // Both locked teams are full: name the team `PickTeam` would land
            // on (the least-populated one) in the refusal message.
            if red_count <= blue_count {
                TEAM_RED
            } else {
                TEAM_BLUE
            }
        }
    };
    let cap = if team == TEAM_RED { red_max } else { blue_max };
    let count = if team == TEAM_RED {
        red_count
    } else {
        blue_count
    };
    eprintln!(
        "----- proxy-rs: teamlock join team={team} old={old_team} count={count} cap={cap} (red={red_max} blue={blue_max})"
    );
    Some(Block {
        client_num: ent_client_num(ent),
        team,
        red_max,
        blue_max,
    })
}

/// Tell the player why the join was refused, naming both locked sizes.
fn send_locked_message(block: &Block) {
    if block.client_num < 0 {
        return;
    }
    let team_name = if block.team == TEAM_RED {
        "^1Red^7"
    } else {
        "^4Blue^7"
    };
    let msg = format!(
        "print \"You cannot join the {team_name} team: teams are locked for ^3{} vs {}^7.\n\"",
        block.red_max, block.blue_max
    );
    let Ok(cmsg) = CString::new(msg) else {
        return;
    };
    // SAFETY: engine syscall registered; cmsg is NUL-terminated.
    unsafe { syscall::send_server_command(block.client_num, &cmsg) };
}

/// `SetTeam(gentity_t*, char*)` entry wrapper (`proxy_sv_lockTeams`): drop a
/// join that would exceed a locked team's cap, otherwise run the pristine body.
///
/// # Safety
///
/// Entered from the game module with the pristine `SetTeam` argument list.
pub unsafe extern "C" fn set_team(ent: usize, s: *const c_char) {
    if let Some(block) = blocked_join(ent, s) {
        send_locked_message(&block);
        return;
    }
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(game_hook_original_set_team);
    unsafe { original(ent, s) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_locks_only_even_rosters_of_at_least_two() {
        assert_eq!(snapshot_caps(3, 3), (3, 3));
        assert_eq!(snapshot_caps(2, 2), (2, 2));
        assert_eq!(snapshot_caps(8, 8), (8, 8));
        // Uneven rosters are never locked, even when both are populated.
        assert_eq!(snapshot_caps(2, 3), (-1, -1));
        assert_eq!(snapshot_caps(3, 2), (-1, -1));
        // A team below two players cannot be locked.
        assert_eq!(snapshot_caps(1, 1), (-1, -1));
        assert_eq!(snapshot_caps(1, 0), (-1, -1));
        assert_eq!(snapshot_caps(0, 0), (-1, -1));
    }

    #[test]
    fn requested_team_parses_red_blue_aliases() {
        assert_eq!(requested_team(c"red".as_ptr()), Some(TEAM_RED));
        assert_eq!(requested_team(c"r".as_ptr()), Some(TEAM_RED));
        assert_eq!(requested_team(c"RED".as_ptr()), Some(TEAM_RED));
        assert_eq!(requested_team(c"R".as_ptr()), Some(TEAM_RED));
        assert_eq!(requested_team(c"blue".as_ptr()), Some(TEAM_BLUE));
        assert_eq!(requested_team(c"b".as_ptr()), Some(TEAM_BLUE));
        assert_eq!(requested_team(c"Blue".as_ptr()), Some(TEAM_BLUE));
        // Everything else is not a direct team join.
        assert_eq!(requested_team(c"spectator".as_ptr()), None);
        assert_eq!(requested_team(c"s".as_ptr()), None);
        assert_eq!(requested_team(c"".as_ptr()), None);
        assert_eq!(requested_team(core::ptr::null()), None);
    }

    #[test]
    fn auto_join_is_blocked_only_when_every_team_is_full() {
        // 2 vs 2 locked: an auto pick (`team free`) lands on a full team.
        assert!(auto_join_blocked(2, 2, 2, 2));
        assert!(auto_join_blocked(2, 2, 3, 2));
        // A freed reservation stays open: one team below cap lets the pick in.
        assert!(!auto_join_blocked(2, 2, 2, 1));
        assert!(!auto_join_blocked(2, 2, 1, 2));
        assert!(!auto_join_blocked(2, 2, 0, 0));
        // No lock at all: nothing is refused.
        assert!(!auto_join_blocked(-1, -1, 2, 2));
    }

    #[test]
    fn requested_spectator_matches_set_team_arms() {
        for request in [
            c"spectator".as_ptr(),
            c"s".as_ptr(),
            c"score".as_ptr(),
            c"scoreboard".as_ptr(),
            c"follow1".as_ptr(),
            c"follow2".as_ptr(),
        ] {
            assert!(requested_spectator(request));
        }
        // Auto picks and explicit joins are not spectator requests.
        for request in [
            c"free".as_ptr(),
            c"".as_ptr(),
            c"red".as_ptr(),
            c"blue".as_ptr(),
            core::ptr::null(),
        ] {
            assert!(!requested_spectator(request));
        }
    }

    #[test]
    fn ascii_eq_ignore_case_is_exact_length() {
        assert!(ascii_eq_ignore_case(b"Red", b"red"));
        assert!(ascii_eq_ignore_case(b"", b""));
        assert!(!ascii_eq_ignore_case(b"red", b"reds"));
        assert!(!ascii_eq_ignore_case(b"red", b"blu"));
    }
}
