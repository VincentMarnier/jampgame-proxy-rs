//! Round-start team locking (`proxy_sv_lockTeamsAfterXSecs`, TDM/CTF).
//!
//! `proxy_sv_lockTeamsAfterXSecs` is the **grace delay in seconds** before a
//! round's teams lock; `0` turns the feature off. When the reconnect burst that
//! follows a map load/map_restart has reconnected the retained clients, the
//! next frame opens the grace window: no team is capped, so players can still
//! join. A player who joins a team in an instance during the window cancels
//! that instance's lock for the whole round. If nobody joins, the window
//! expires and every instance still eligible is capped at its current roster.
//!
//! The caps follow the even-roster rule: a roster locks only when `red == blue`
//! and both teams field at least one player, so an uneven round (e.g. `2 vs 3`)
//! is left alone. A cap is a *reservation*: it survives players leaving, so the
//! freed slots stay open for the same team instead of being taken by the other
//! one.
//!
//! The lock is **per instance**. With parallel TFFA matches
//! (`proxy_tffa_enable`), every match that existed at the round start is
//! snapshotted against its own roster, and a join is refused against the caps of
//! the match the joining player belongs to. A join in one match cancels only
//! that match's lock. With TFFA off the whole server is the one (main) instance.
//! A match created after the round started has no round-start roster and stays
//! unlocked until the next round (a `map_restart` keeps the matches, so their
//! rosters are eligible again then).
//!
//! The round start is the reconnect burst the engine issues when a map loads or
//! is restarted (`SV_InitGameProgs`/`SV_RestartGameProgs`): every retained
//! client gets `GAME_CLIENT_CONNECT` with `firstTime == qfalse` *before* the next
//! `GAME_RUN_FRAME`, at which point the game module has restored the session
//! teams from the `session%i` cvars. Late joins use `firstTime == qtrue` and do
//! not retrigger the window.
//!
//! Enforcement is a single `SetTeam` detour: an explicit `red`/`blue` request
//! for a full, locked team is dropped with a message, and so is an auto pick
//! (`team free`/empty, which the game routes through `PickTeam`) once *both*
//! locked teams are full. Spectator changes and the game's own internal
//! `SetTeam` calls go through unchanged. The same wrapper observes successful
//! joins while the grace window is open and cancels the joining player's
//! instance.

use core::ffi::{CStr, c_char, c_int};
use std::ffi::CString;

use crate::hooks::engine_sv::{read_i32, read_usize};
use crate::hooks::{game_hook_original_set_team, original_call};
use crate::sdk::{
    CON_DISCONNECTED, GT_CTF, GT_TEAM, MAX_CLIENTS, OFFSET_GCLIENT_PERS, OFFSET_GCLIENT_SESS,
    OFFSET_GENTITY_CLIENT, OFFSET_PERS_CONNECTED, OFFSET_SESS_SESSION_TEAM, SIZEOF_GENTITY,
    TEAM_BLUE, TEAM_RED,
};
use crate::state::{self, CVAR_LOCK_TEAMS_AFTER_SECS, TeamCaps, TeamLock};
use crate::syscall;
use crate::tffa;

/// The `proxy_sv_lockTeamsAfterXSecs` grace delay in seconds (`<= 0` = off).
fn delay_secs() -> i32 {
    state::with_state(|s| s.cvars[CVAR_LOCK_TEAMS_AFTER_SECS].integer)
}

/// `proxy_sv_lockTeamsAfterXSecs` gate: any positive delay turns the feature on.
fn feature_enabled() -> bool {
    delay_secs() > 0
}

/// The game's `g_gametype` (engine cvar trap) — only the team modes are
/// eligible.
fn gametype_supported() -> bool {
    // SAFETY: engine syscall registered (GAME_INIT ran before any hook fires).
    let gametype = unsafe { syscall::cvar_variable_integer_value(c"g_gametype") };
    gametype == GT_TEAM || gametype == GT_CTF
}

/// The round-start caps: both teams lock at their (equal, non-empty) size.
/// Uneven or empty rosters are left unlocked (`-1`).
pub fn snapshot_caps(red: i32, blue: i32) -> (i32, i32) {
    if red == blue && red >= 1 {
        (red, blue)
    } else {
        (-1, -1)
    }
}

/// Per-instance caps: one pair per TFFA match slot (index = match id, `0` =
/// main). Only instances that were armed at the round start and had no join
/// during the grace window are snapshotted; each uses its own roster. Pure so
/// the per-instance rule is unit-tested without the engine.
pub fn snapshot_match_caps(
    counts: &[(i32, i32); tffa::TOTAL_MATCH_SLOTS],
    armed: &[bool; tffa::TOTAL_MATCH_SLOTS],
    joined: &[bool; tffa::TOTAL_MATCH_SLOTS],
) -> [TeamCaps; tffa::TOTAL_MATCH_SLOTS] {
    core::array::from_fn(|id| {
        if !armed[id] || joined[id] {
            return TeamCaps::default();
        }
        let (red, blue) = counts[id];
        let (red, blue) = snapshot_caps(red, blue);
        TeamCaps { red, blue }
    })
}

/// Whether the `delay_secs` grace window that opened at `start` has expired at
/// server time `now` (both milliseconds). A non-positive delay never expires
/// (the feature is off).
pub fn grace_due(now: i32, start: i32, delay_secs: i32) -> bool {
    delay_secs > 0 && i64::from(now.wrapping_sub(start)) >= i64::from(delay_secs) * 1000
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

/// `GAME_RUN_FRAME` bookkeeping: open the round-start grace window on the first
/// frame after the reconnect burst (all reconnects happen before that frame),
/// then lock the eligible instances once the window expires.
pub fn on_run_frame() {
    let ready = state::with_state(|s| s.team_lock.pending && s.team_lock.saw_reconnect);
    if ready {
        start_grace();
    }
    lock_after_grace();
}

/// Open this round's grace window. Only the instances that exist now are armed:
/// a TFFA match created later in the round has no round-start roster and stays
/// unlocked until the next round, as before.
fn start_grace() {
    let gt = unsafe { syscall::cvar_variable_integer_value(c"g_gametype") };
    let delay = delay_secs();
    let tffa_on = tffa::enabled();
    let supported = gt == GT_TEAM || gt == GT_CTF;
    let armed: [bool; tffa::TOTAL_MATCH_SLOTS] = state::with_state(|s| {
        let armed = if supported && delay > 0 {
            core::array::from_fn(|id| {
                if tffa_on {
                    s.tffa.matches[id].active
                } else {
                    id == tffa::MAIN_MATCH as usize
                }
            })
        } else {
            [false; tffa::TOTAL_MATCH_SLOTS]
        };
        s.team_lock.caps = [TeamCaps::default(); tffa::TOTAL_MATCH_SLOTS];
        s.team_lock.joined = [false; tffa::TOTAL_MATCH_SLOTS];
        s.team_lock.armed = armed;
        s.team_lock.pending = false;
        s.team_lock.saw_reconnect = false;
        s.team_lock.grace_start = s.svs_time;
        s.team_lock.grace_running = supported && delay > 0;
        armed
    });
    eprintln!(
        "----- proxy-rs: teamlock grace gt={gt} delay={delay}s tffa={tffa_on} armed={}",
        describe_instances(&armed)
    );
}

/// Lock every armed instance whose grace window passed without a join.
fn lock_after_grace() {
    if !state::with_state(|s| s.team_lock.grace_running) {
        return;
    }
    let delay = delay_secs();
    if delay <= 0 {
        // Turned off mid-round: drop any caps and stop waiting.
        state::with_state(|s| {
            s.team_lock.caps = [TeamCaps::default(); tffa::TOTAL_MATCH_SLOTS];
            s.team_lock.grace_running = false;
        });
        return;
    }
    let (now, start) = state::with_state(|s| (s.svs_time, s.team_lock.grace_start));
    if !grace_due(now, start, delay) {
        return;
    }
    let tffa_on = tffa::enabled();
    let counts = if tffa_on {
        tffa::match_team_counts()
    } else {
        let mut counts = [(0, 0); tffa::TOTAL_MATCH_SLOTS];
        counts[tffa::MAIN_MATCH as usize] = (team_count(TEAM_RED), team_count(TEAM_BLUE));
        counts
    };
    let (armed, joined) = state::with_state(|s| (s.team_lock.armed, s.team_lock.joined));
    let caps = snapshot_match_caps(&counts, &armed, &joined);
    let summary = describe_caps(&caps);
    eprintln!("----- proxy-rs: teamlock lock delay={delay}s tffa={tffa_on} -> {summary}");
    state::with_state(|s| {
        s.team_lock.caps = caps;
        s.team_lock.grace_running = false;
    });
}

/// Diagnostic rendering of a cap table (once per round, so it may allocate).
fn describe_caps(caps: &[TeamCaps; tffa::TOTAL_MATCH_SLOTS]) -> String {
    let locked: Vec<String> = caps
        .iter()
        .enumerate()
        .filter(|(_, caps)| caps.red >= 0 || caps.blue >= 0)
        .map(|(id, caps)| format!("{} {} vs {}", instance_name(id), caps.red, caps.blue))
        .collect();
    if locked.is_empty() {
        "no lock".to_owned()
    } else {
        locked.join(", ")
    }
}

/// Diagnostic rendering of the armed-instance flags.
fn describe_instances(flags: &[bool; tffa::TOTAL_MATCH_SLOTS]) -> String {
    let armed: Vec<String> = flags
        .iter()
        .enumerate()
        .filter(|(_, armed)| **armed)
        .map(|(id, _)| instance_name(id))
        .collect();
    if armed.is_empty() {
        "none".to_owned()
    } else {
        armed.join(", ")
    }
}

fn instance_name(id: usize) -> String {
    if id == tffa::MAIN_MATCH as usize {
        "main".to_owned()
    } else {
        format!("#{id}")
    }
}

/// Count a team exactly like the game's `TeamCount` (`g_client.c:1237`): every
/// connected client whose `sess.sessionTeam` matches.
pub(crate) fn team_count(team: i32) -> i32 {
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
/// `trap_SendServerCommand`. Returns -1 when the entity is not a player slot
/// (null/base not set/underflow/overflow), so callers fail closed instead of
/// addressing an out-of-range client.
fn ent_client_num(ent: usize) -> c_int {
    let base = state::with_state(|s| s.located_game_data.g_entities);
    if base == 0 || ent < base {
        return -1;
    }
    let idx = (ent - base) / SIZEOF_GENTITY;
    if idx >= MAX_CLIENTS {
        return -1;
    }
    idx as c_int
}

/// Reset the round state for a new map (`GAME_INIT`): caps and the grace
/// bookkeeping cleared, the window re-armed for the next reconnect burst.
/// Without this the previous round's caps (or cancellation) stick across all
/// later rounds.
pub fn reset() {
    state::with_state(|s| s.team_lock = TeamLock::default());
}

/// Drop the lock state of a closed instance so a match later reusing its slot
/// does not inherit the old match's lock or grace cancellation. Main (`0`)
/// always stays open and is ignored.
pub fn clear_match(id: u8) {
    if id == tffa::MAIN_MATCH {
        return;
    }
    let id = id as usize;
    state::with_state(|s| {
        if let Some(caps) = s.team_lock.caps.get_mut(id) {
            *caps = TeamCaps::default();
        }
        if let Some(armed) = s.team_lock.armed.get_mut(id) {
            *armed = false;
        }
        if let Some(joined) = s.team_lock.joined.get_mut(id) {
            *joined = false;
        }
    });
}

/// Carry a promoted instance's lock state into main (it becomes the surviving
/// game) and clear the slot it was promoted from, so the surviving game keeps
/// the promoted instance's caps and its grace cancellation.
pub fn promote_match(id: u8) {
    if id == tffa::MAIN_MATCH || id as usize >= tffa::TOTAL_MATCH_SLOTS {
        return;
    }
    let id = id as usize;
    state::with_state(|s| {
        let main = tffa::MAIN_MATCH as usize;
        s.team_lock.caps[main] = s.team_lock.caps[id];
        s.team_lock.armed[main] = s.team_lock.armed[id];
        s.team_lock.joined[main] = s.team_lock.joined[id];
        s.team_lock.caps[id] = TeamCaps::default();
        s.team_lock.armed[id] = false;
        s.team_lock.joined[id] = false;
    });
}

/// The client's session team, or `None` when `ent` carries no live client.
fn client_team(ent: usize) -> Option<i32> {
    if ent == 0 {
        return None;
    }
    // SAFETY: ent is a valid gentity_t* passed by the game.
    let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
    if client == 0 {
        return None;
    }
    // SAFETY: client is a valid gclient_t.
    Some(unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) })
}

/// A player joined a team in `match_id` during the grace window: that instance
/// stays unlocked for the rest of the round. Only the first join is logged, and
/// only for an instance the round start armed.
fn cancel_grace(match_id: u8) {
    let id = match_id as usize;
    let cancelled = state::with_state(|s| {
        if !s.team_lock.grace_running || id >= tffa::TOTAL_MATCH_SLOTS {
            return false;
        }
        if !s.team_lock.armed[id] || s.team_lock.joined[id] {
            return false;
        }
        s.team_lock.joined[id] = true;
        true
    });
    if cancelled {
        eprintln!("----- proxy-rs: teamlock join match={match_id} -> not locked this round");
    }
}

/// Observe the outcome of a successful `SetTeam`: a move into `red`/`blue`
/// during the grace window cancels the joining player's instance. The decision
/// is read after the pristine body so auto picks (which the game resolves
/// through `PickTeam`) are seen too.
fn note_team_join(ent: usize, old_team: i32) {
    let Some(new_team) = client_team(ent) else {
        return;
    };
    if new_team == old_team || (new_team != TEAM_RED && new_team != TEAM_BLUE) {
        return;
    }
    let client_num = ent_client_num(ent);
    let match_id = if client_num >= 0 {
        tffa::client_match(client_num)
    } else {
        tffa::MAIN_MATCH
    };
    cancel_grace(match_id);
}

/// Observe a successful `SetTeam` for the parallel-TFFA roster memory
/// (D-006.10, D-006.11): a move into RED/BLUE is the client playing again, so a
/// team a finished game remembered for them is stale and the auto-join must not
/// override what they chose. The id they belong to does not change here — a
/// player's `home` only moves with an explicit `tffa_*` command — so a
/// `map_restart` still returns them to their own TFFA. Spectator/follow moves
/// are not joins and keep the memory.
fn note_tffa_team_join(ent: usize, old_team: i32) {
    let Some(new_team) = client_team(ent) else {
        return;
    };
    if new_team == old_team || (new_team != TEAM_RED && new_team != TEAM_BLUE) {
        return;
    }
    let client_num = ent_client_num(ent);
    if client_num >= 0 {
        tffa::forget_return_rosters(client_num);
    }
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
    // Caps and counts are those of the instance the joining player belongs to:
    // each TFFA match locks against its own roster (main when TFFA is off).
    let client_num = ent_client_num(ent);
    let match_id = if client_num >= 0 {
        tffa::client_match(client_num)
    } else {
        tffa::MAIN_MATCH
    };
    let (red_max, blue_max) = state::with_state(|s| {
        let caps = s
            .team_lock
            .caps
            .get(match_id as usize)
            .copied()
            .unwrap_or_default();
        (caps.red, caps.blue)
    });
    if red_max < 0 && blue_max < 0 {
        return None;
    }
    if !gametype_supported() {
        return None;
    }
    let (red_count, blue_count) = tffa::match_team_counts()
        .get(match_id as usize)
        .copied()
        .unwrap_or((0, 0));
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
        "----- proxy-rs: teamlock join match={match_id} team={team} old={old_team} count={count} cap={cap} (red={red_max} blue={blue_max})"
    );
    Some(Block {
        client_num,
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

/// `SetTeam(gentity_t*, char*)` entry wrapper
/// (`proxy_sv_lockTeamsAfterXSecs`): drop a join that would exceed a locked
/// team's cap, otherwise run the pristine body. While the round-start grace
/// window is open, observe the resulting team and cancel the instance's lock
/// when a player joined.
///
/// # Safety
///
/// Entered from the game module with the pristine `SetTeam` argument list.
pub unsafe extern "C" fn set_team(ent: usize, s: *const c_char) {
    if let Some(block) = blocked_join(ent, s) {
        send_locked_message(&block);
        return;
    }
    // Detour missing (attach failed or raced with detach): fail closed
    // instead of calling a null function pointer.
    if crate::patch::GAME_HOOKS[5].original() == 0 {
        return;
    }
    // Read the pre-call team only while the grace window can still be
    // cancelled, or when parallel TFFA needs to know a client actually joined
    // a team (D-006.10); after the lock is final for the round and the feature
    // is off, the read is not needed.
    let observe_tffa = tffa::enabled();
    let old_team = if observe_tffa || state::with_state(|s| s.team_lock.grace_running) {
        client_team(ent)
    } else {
        None
    };
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(game_hook_original_set_team);
    unsafe { original(ent, s) };
    if let Some(old_team) = old_team {
        note_team_join(ent, old_team);
        if observe_tffa {
            note_tffa_team_join(ent, old_team);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_locks_even_populated_rosters() {
        assert_eq!(snapshot_caps(1, 1), (1, 1));
        assert_eq!(snapshot_caps(2, 2), (2, 2));
        assert_eq!(snapshot_caps(3, 3), (3, 3));
        assert_eq!(snapshot_caps(8, 8), (8, 8));
        // Uneven and empty rosters are never locked.
        assert_eq!(snapshot_caps(2, 3), (-1, -1));
        assert_eq!(snapshot_caps(3, 2), (-1, -1));
        assert_eq!(snapshot_caps(1, 0), (-1, -1));
        assert_eq!(snapshot_caps(0, 0), (-1, -1));
    }

    #[test]
    fn per_match_snapshot_caps_each_instance_against_its_roster() {
        let mut counts = [(0, 0); tffa::TOTAL_MATCH_SLOTS];
        counts[tffa::MAIN_MATCH as usize] = (2, 2);
        counts[1] = (1, 1);
        counts[2] = (3, 3);
        counts[3] = (2, 3);
        counts[4] = (0, 0);
        let armed = [true; tffa::TOTAL_MATCH_SLOTS];
        let not_joined = [false; tffa::TOTAL_MATCH_SLOTS];

        // Every armed instance with an even roster locks at its own size; the
        // uneven and empty matches stay unlocked.
        let caps = snapshot_match_caps(&counts, &armed, &not_joined);
        assert_eq!(
            caps[tffa::MAIN_MATCH as usize],
            TeamCaps { red: 2, blue: 2 }
        );
        assert_eq!(caps[1], TeamCaps { red: 1, blue: 1 });
        assert_eq!(caps[2], TeamCaps { red: 3, blue: 3 });
        assert_eq!(caps[3], TeamCaps { red: -1, blue: -1 });
        assert_eq!(caps[4], TeamCaps { red: -1, blue: -1 });
        // Untouched slots stay unlocked.
        assert_eq!(caps[5], TeamCaps { red: -1, blue: -1 });

        // An instance that was not armed at the round start (a match created
        // later) never locks this round.
        let mut armed = [true; tffa::TOTAL_MATCH_SLOTS];
        armed[2] = false;
        let caps = snapshot_match_caps(&counts, &armed, &not_joined);
        assert_eq!(caps[2], TeamCaps::default());

        // A join during the grace window cancels only that instance.
        let mut joined = [false; tffa::TOTAL_MATCH_SLOTS];
        joined[tffa::MAIN_MATCH as usize] = true;
        let caps = snapshot_match_caps(&counts, &armed, &joined);
        assert_eq!(caps[0], TeamCaps::default());
        assert_eq!(caps[1], TeamCaps { red: 1, blue: 1 });
        assert_eq!(caps[2], TeamCaps::default());
    }

    #[test]
    fn grace_expires_after_the_configured_seconds() {
        // `0` (and any non-positive value) disables the feature: never due.
        assert!(!grace_due(i32::MAX, 0, 0));
        assert!(!grace_due(i32::MAX, 0, -5));
        // 30 s window.
        assert!(!grace_due(29_999, 1_000, 30));
        assert!(grace_due(31_000, 1_000, 30));
        assert!(grace_due(31_001, 1_000, 30));
        // A 1 s window expires one second after the round start.
        assert!(!grace_due(999, 0, 1));
        assert!(grace_due(1_000, 0, 1));
    }

    #[test]
    fn caps_follow_a_promotion_and_are_cleared_with_their_instance() {
        state::with_state(|s| {
            s.team_lock = TeamLock::default();
            s.team_lock.caps[1] = TeamCaps { red: 2, blue: 2 };
            s.team_lock.caps[2] = TeamCaps { red: 3, blue: 3 };
            // #1 is still eligible; #2 had a join and must stay unlocked.
            s.team_lock.armed[1] = true;
            s.team_lock.armed[2] = true;
            s.team_lock.joined[2] = true;
        });
        promote_match(1);
        let (main, one, two) = state::with_state(|s| {
            (
                s.team_lock.caps[0],
                s.team_lock.caps[1],
                s.team_lock.caps[2],
            )
        });
        // The promoted instance's caps and grace state become main's; its own
        // slot is cleared.
        assert_eq!(main, TeamCaps { red: 2, blue: 2 });
        assert_eq!(one, TeamCaps::default());
        assert_eq!(two, TeamCaps { red: 3, blue: 3 });
        let (armed, joined) = state::with_state(|s| {
            (
                (s.team_lock.armed[0], s.team_lock.armed[1]),
                (s.team_lock.joined[0], s.team_lock.joined[1]),
            )
        });
        assert_eq!(armed, (true, false));
        assert_eq!(joined, (false, false));
        // Closing an instance drops its caps; main keeps its own.
        clear_match(2);
        clear_match(tffa::MAIN_MATCH);
        let (main, two) = state::with_state(|s| (s.team_lock.caps[0], s.team_lock.caps[2]));
        assert_eq!(main, TeamCaps { red: 2, blue: 2 });
        assert_eq!(two, TeamCaps::default());
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
