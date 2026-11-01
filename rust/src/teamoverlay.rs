//! Spectator team overlay (`tinfo`) feed.
//!
//! Both the stock cgame (`cg_newDraw.c:CG_DrawNewTeamInfo`) and TaystJK's
//! `cg_drawTeamOverlay` family draw their rows from `cgs.clientinfo[]` /
//! `sortedTeamPlayers`, which the game module fills with the `tinfo` reliable
//! command (`g_team.c:TeamplayInfoMessage`). The pristine game only builds it
//! for clients on a RED/BLUE team (`CheckTeamStatus` skips everyone else,
//! `g_team.c:1180-1191`) and only lists teammates.
//!
//! A spectator following a player is therefore left out: no `tinfo` ever
//! reaches it, so `numSortedTeamPlayers` is `0` (never played this map) or
//! stale from before going spectator. The cgame would already draw the
//! overlay, because a following spectator is sent the followed player's
//! playerState (`g_active.c:SpectatorClientEndFrame` copies `ps` and sets
//! `PMF_FOLLOW`), so `cg.snap->ps.persistant[PERS_TEAM]` is that player's team
//! — which is what TaystJK's `CG_DrawTeamOverlay`/`CG_DrawTeamOverlay3` test
//! (`cg_draw.c:6108`, `cg_draw.c:6573`).
//!
//! The proxy supplies the missing roster while the game module keeps sending
//! its own `tinfo` to teammates untouched:
//!
//! - once per second (`TEAM_LOCATION_UPDATE_TIME`, the pristine cadence) and
//!   immediately when the followed player's team (and parallel-TFFA instance)
//!   changes, one `tinfo` goes to each connected spectator in
//!   `SPECTATOR_FOLLOW` state whose `pers.teamInfo` is set (forced on in team
//!   gametypes, `g_client.c:2160`), i.e. exactly the pristine recipients the
//!   game itself would use if it handled spectators;
//! - the roster is the followed player's team, ascending by client number,
//!   capped at `TEAM_MAXOVERLAY` and at the engine's reliable-command budget
//!   (whole entries only). Both teams are never listed: the cgame only ever
//!   considers `min(8, numSortedTeamPlayers)` entries, so a mixed list would
//!   hide players of one team. When the spectator cycles to the other team the
//!   next roster is sent at once;
//! - the payload is the stock six-int entry `clientNum location health armor
//!   weapon powerups` with raw `STAT_ARMOR`, never jaPRO's `armor*100+fp`
//!   packing: the proxy runs the pristine module, whose `gamename` is
//!   `basejk*`, so the cgame treats it as `SVMOD_BASEJKA` and decodes nothing
//!   (`cg_servercmds.c:252-302`);
//! - in parallel TFFA only players of the followed player's match are listed
//!   (the game module has no notion of the instances).
//!
//! Compatible with stock cgames by construction: the format is theirs (their
//! `CG_ParseTeamInfo` is the same six-int stride), the counts stay inside
//! `TEAM_MAXOVERLAY` (base 1.01's parser has no bounds checks), health/armor
//! are clamped like the game clamps them, and only the premise the feature adds
//! changes — a spectator's roster is now populated. A client with the overlay
//! off (`teamoverlay 0` outside team gametypes, which the pristine game already
//! respects through `pers.teamInfo`) is not sent anything.
//!
//! Limitation: while a spectator is in free-fly (`SPECTATOR_FREE`) its
//! playerState is its own and `PERS_TEAM` is `TEAM_SPECTATOR`, so the cgame
//! returns before drawing. The overlay appears while following a player; that
//! gate is client-side and cannot be changed from the server.

use core::ffi::c_int;

use crate::hooks::engine_sv::{read_i32, read_usize};
use crate::sdk::{
    CON_CONNECTED, GT_TEAM, MAX_CLIENTS, MAX_STRING_CHARS, OFFSET_ENTITY_POWERUPS,
    OFFSET_GCLIENT_PERS, OFFSET_GCLIENT_PS, OFFSET_GCLIENT_SESS, OFFSET_GENTITY_CLIENT,
    OFFSET_GENTITY_INUSE, OFFSET_GENTITY_S, OFFSET_LEVEL_FOLLOW1, OFFSET_LEVEL_FOLLOW2,
    OFFSET_LEVEL_TIME, OFFSET_PERS_CONNECTED, OFFSET_PERS_TEAM_INFO,
    OFFSET_PERS_TEAM_STATE_LOCATION, OFFSET_PS_STATS, OFFSET_PS_WEAPON, OFFSET_SESS_SESSION_TEAM,
    OFFSET_SESS_SPECTATOR_CLIENT, OFFSET_SESS_SPECTATOR_STATE, SIZEOF_GENTITY, SPECTATOR_FOLLOW,
    STAT_ARMOR, STAT_HEALTH, TEAM_BLUE, TEAM_MAXOVERLAY, TEAM_RED, TEAM_SPECTATOR,
};
use crate::state;
use crate::syscall;

/// Pristine `tinfo` cadence: `TEAM_LOCATION_UPDATE_TIME` (`bg_public.h:1028`).
const REFRESH_MS: i32 = 1000;

/// Bytes the engine copies per reliable command: `Q_strncpyz` into
/// `char[MAX_STRING_CHARS]` keeps at most 1023 bytes
/// (`sv_main.cpp:SV_AddServerCommand`), and `syscall::send_server_command`
/// drops anything above 1022.
const MAX_COMMAND_LEN: usize = MAX_STRING_CHARS - 2;

/// Upper bound of the `"tinfo <count>"` header used to budget entries
/// (`count` never exceeds `TEAM_MAXOVERLAY`, i.e. two digits).
const HEADER_RESERVE: usize = "tinfo 32".len();

/// One `tinfo` row: the six ints the pristine `TeamplayInfoMessage` writes per
/// player (`g_team.c:1134-1140`), read by the cgame as
/// `clientNum location health armor weapon powerups`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub client: c_int,
    pub location: c_int,
    pub health: c_int,
    pub armor: c_int,
    pub weapon: c_int,
    pub powerups: c_int,
}

impl Row {
    /// Build a row with the same clamps as the pristine body — it clamps a
    /// negative health/armor to `0` before writing the entry
    /// (`g_team.c:1134-1136`).
    fn clamped(
        client: c_int,
        location: c_int,
        health: c_int,
        armor: c_int,
        weapon: c_int,
        powerups: c_int,
    ) -> Self {
        Self {
            client,
            location,
            health: health.max(0),
            armor: armor.max(0),
            weapon,
            powerups,
        }
    }
}

/// The followed player's team, and — while parallel TFFA matches run — the
/// instance whose roster the viewer may see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scope {
    team: c_int,
    match_filter: Option<u8>,
}

/// The scope a viewer is watching: the followed player's team plus, while
/// parallel TFFA matches run, that player's instance.
fn follow_scope(viewer: c_int, g_entities: usize, level: usize, tffa_on: bool) -> Option<Scope> {
    // SAFETY: both addresses are the game module's live globals.
    let (target, team) = unsafe { followed_player(viewer, g_entities, level) }?;
    // Parallel TFFA: a spectator following a player watches that player's
    // instance, so the roster may only list the same instance's players.
    let match_filter = if tffa_on {
        Some(crate::tffa::client_match(target))
    } else {
        None
    };
    Some(Scope { team, match_filter })
}

/// Whether the feature is on: master switch, its own cvar, and a team
/// gametype. The cgame only draws a team overlay in team modes
/// (`cg_draw.c:CG_DrawUpperRight`: `cgs.gametype >= GT_TEAM`), so nothing is
/// sent in FFA/duel modes.
fn enabled() -> bool {
    if !state::proxy_enabled() {
        return false;
    }
    if state::with_state(|s| s.cvars[state::CVAR_SPECTATOR_TEAM_OVERLAY].integer == 0) {
        return false;
    }
    // SAFETY: engine syscall registered (`GAME_INIT` ran before any frame).
    (unsafe { syscall::cvar_variable_integer_value(c"g_gametype") }) >= GT_TEAM
}

/// Per-frame hook, run after the pristine `G_RunFrame`: send the periodic
/// roster and the immediate resent a follow-scope change needs.
pub fn on_run_frame_post() {
    if !enabled() {
        return;
    }
    let level = crate::original::level_address();
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if level == 0 || g_entities == 0 {
        return;
    }
    // SAFETY: `level`/`g_entities` are the loaded game module's globals
    // (`GAME_INIT` ran) and `level.time` is a plain int field.
    let now = unsafe { read_i32(level + OFFSET_LEVEL_TIME) };
    let periodic = state::with_state(|s| {
        let elapsed = now.wrapping_sub(s.spec_overlay_last_time);
        // A clock that moved backwards (a fresh round after `map_restart`
        // resets neither proxy state nor engine time, but be safe) sends at
        // once instead of waiting for the next tick.
        if !(0..REFRESH_MS).contains(&elapsed) {
            s.spec_overlay_last_time = now;
            true
        } else {
            false
        }
    });

    // One feature read per frame, not per spectator slot: `enabled()` reads an
    // engine cvar through a trap.
    let tffa_on = crate::tffa::enabled();
    for viewer in 0..MAX_CLIENTS as c_int {
        let Some(scope) = follow_scope(viewer, g_entities, level, tffa_on) else {
            // No longer following: the cgame hides the overlay, and the next
            // follow must not be mistaken for "same scope" by the cache.
            state::with_state(|s| s.spec_overlay_scope[viewer as usize] = 0);
            continue;
        };
        // `+ 1` keeps `match_filter` distinguishable from "no match filter"
        // (key `team` alone) while staying non-zero for the sentinel `0`.
        let key = scope.team | ((scope.match_filter.map_or(0, |m| m as c_int + 1)) << 8);
        let changed = state::with_state(|s| s.spec_overlay_scope[viewer as usize] != key);
        if !periodic && !changed {
            continue;
        }
        state::with_state(|s| s.spec_overlay_scope[viewer as usize] = key);

        // SAFETY: game globals as above; every read is a live field.
        let matches = state::with_state(|s| s.tffa.client_match);
        let rows = unsafe { collect_rows(g_entities, scope.team, scope.match_filter, &matches) };
        send(viewer, &format_tinfo(&rows));
    }
}

/// Forget a disconnecting client's cached scope: the slot may be handed to a
/// new spectator before the next periodic tick.
pub fn on_client_disconnect(client_num: c_int) {
    if (0..MAX_CLIENTS as c_int).contains(&client_num) {
        state::with_state(|s| s.spec_overlay_scope[client_num as usize] = 0);
    }
}

/// `g_entities + client_num` → `ent->client` (`gclient_t*`), or `None` for an
/// empty slot.
fn client_of(g_entities: usize, client_num: c_int) -> Option<usize> {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return None;
    }
    let ent = g_entities + client_num as usize * SIZEOF_GENTITY;
    // SAFETY: `ent` is inside the `g_entities[MAX_GENTITIES]` array, and
    // `client` is a plain pointer field.
    let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
    (client != 0).then_some(client)
}

/// The followed player and team of a spectator viewer, when it is following a
/// player on a team. Mirrors pristine `SpectatorClientEndFrame`'s target
/// resolution: `sess.spectatorClient == -1`/`-2` means `level.follow1`/
/// `follow2` (`g_active.c:3660-3668`).
///
/// # Safety
///
/// `g_entities` and `level` must be the loaded game module's live globals.
unsafe fn followed_player(
    viewer: c_int,
    g_entities: usize,
    level: usize,
) -> Option<(c_int, c_int)> {
    let client = client_of(g_entities, viewer)?;
    // SAFETY: `client` came from a live gentity's `client` pointer.
    let session = unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) };
    if session != TEAM_SPECTATOR {
        return None;
    }
    // A connected spectator has a live playerState to mirror; connecting
    // (never-begun) slots do not. The proxy's own client mirror cannot be used
    // here: bots never arrive through its `GAME_CLIENT_CONNECT` event.
    //
    // SAFETY: live `gclient_t` field.
    if unsafe { read_i32(client + OFFSET_GCLIENT_PERS + OFFSET_PERS_CONNECTED) } != CON_CONNECTED {
        return None;
    }
    // SAFETY: live `gclient_t` fields.
    if unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SPECTATOR_STATE) }
        != SPECTATOR_FOLLOW
    {
        return None;
    }
    // The pristine `TeamplayInfoMessage` only serves a client that asked for
    // team info (`pers.teamInfo`, `g_team.c:1104`).
    if unsafe { read_i32(client + OFFSET_GCLIENT_PERS + OFFSET_PERS_TEAM_INFO) } == 0 {
        return None;
    }
    // SAFETY: live `gclient_t`/`level_locals_t` int fields.
    let target = unsafe {
        match read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SPECTATOR_CLIENT) {
            -1 => read_i32(level + OFFSET_LEVEL_FOLLOW1),
            -2 => read_i32(level + OFFSET_LEVEL_FOLLOW2),
            other => other,
        }
    };
    if !(0..MAX_CLIENTS as c_int).contains(&target) {
        return None;
    }
    let target_client = client_of(g_entities, target)?;
    // SAFETY: live `gclient_t` field; the followed player's ps is what the
    // spectator's cgame receives (`PERS_TEAM` is this team).
    let team = unsafe { read_i32(target_client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) };
    if team != TEAM_RED && team != TEAM_BLUE {
        return None;
    }
    Some((target, team))
}

/// Whether a player belongs on `team`'s roster (and in the followed instance,
/// when one is requested). Pure so the team/instance gating is unit-tested.
fn roster_includes(
    team: c_int,
    player_team: c_int,
    player_match: u8,
    match_filter: Option<u8>,
) -> bool {
    player_team == team && match_filter.is_none_or(|want| player_match == want)
}

/// Collect the roster for `team`: every in-use player in client-number order,
/// capped at `TEAM_MAXOVERLAY`. This is the pristine `TeamplayInfoMessage`
/// shape once the followed-instance filter is applied.
///
/// `matches` is the proxy's per-slot parallel-TFFA instance table
/// (`TffaState::client_match`), taken out of the state by the caller so this
/// read pass never re-enters it.
///
/// # Safety
///
/// `g_entities` must be the loaded game module's live global.
unsafe fn collect_rows(
    g_entities: usize,
    team: c_int,
    match_filter: Option<u8>,
    matches: &[u8; MAX_CLIENTS],
) -> Vec<Row> {
    let mut rows = Vec::with_capacity(TEAM_MAXOVERLAY);
    for client_num in 0..MAX_CLIENTS as c_int {
        if rows.len() >= TEAM_MAXOVERLAY {
            break;
        }
        let ent = g_entities + client_num as usize * SIZEOF_GENTITY;
        // SAFETY: `ent` is inside the `g_entities` array; `inuse` is a plain
        // int field (pristine body skips players that are not in use).
        if unsafe { read_i32(ent + OFFSET_GENTITY_INUSE) } == 0 {
            continue;
        }
        let Some(client) = client_of(g_entities, client_num) else {
            continue;
        };
        // SAFETY: live `gclient_t`/`gentity_t` fields.
        let (player_team, health, armor, weapon, location, powerups) = unsafe {
            (
                read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM),
                read_i32(client + OFFSET_GCLIENT_PS + OFFSET_PS_STATS + STAT_HEALTH * 4),
                read_i32(client + OFFSET_GCLIENT_PS + OFFSET_PS_STATS + STAT_ARMOR * 4),
                read_i32(client + OFFSET_GCLIENT_PS + OFFSET_PS_WEAPON),
                read_i32(client + OFFSET_GCLIENT_PERS + OFFSET_PERS_TEAM_STATE_LOCATION),
                read_i32(ent + OFFSET_GENTITY_S + OFFSET_ENTITY_POWERUPS),
            )
        };
        let player_match = matches[client_num as usize];
        if !roster_includes(team, player_team, player_match, match_filter) {
            continue;
        }
        rows.push(Row::clamped(
            client_num, location, health, armor, weapon, powerups,
        ));
    }
    rows
}

/// Format the `tinfo` reliable command: `tinfo <count>` then one six-int entry
/// per row, each introduced by a space (the pristine `va("tinfo %i %s", …)`
/// shape with `" %i %i %i %i %i %i"` entries).
///
/// Entries are appended whole until the next one would exceed the engine's
/// reliable-command budget, and the header count is the number actually
/// written, so a truncated tail can never shift a later entry's fields.
fn format_tinfo(rows: &[Row]) -> String {
    let mut body = String::new();
    let mut count = 0usize;
    for row in rows.iter().take(TEAM_MAXOVERLAY) {
        let piece = format!(
            " {} {} {} {} {} {}",
            row.client, row.location, row.health, row.armor, row.weapon, row.powerups
        );
        if HEADER_RESERVE + body.len() + piece.len() > MAX_COMMAND_LEN {
            break;
        }
        body.push_str(&piece);
        count += 1;
    }
    format!("tinfo {count}{body}")
}

/// `trap_SendServerCommand(viewer, text)`. Fail closed on an interior NUL
/// (impossible with the integer fields above) instead of truncating.
fn send(viewer: c_int, text: &str) {
    let Ok(text) = std::ffi::CString::new(text) else {
        return;
    };
    // SAFETY: engine syscall registered; the engine copies the text into the
    // client's reliable ring synchronously.
    unsafe { syscall::send_server_command(viewer, &text) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tinfo` entry width (`cg_servercmds.c`: `CG_Argv(i * 6 + n)`).
    const STRIDE: usize = 6;

    fn row(client: i32, health: i32, armor: i32) -> Row {
        Row::clamped(client, 4, health, armor, 3, 0)
    }

    fn nums(text: &str) -> Vec<i32> {
        text.strip_prefix("tinfo ")
            .expect("tinfo prefix")
            .split_whitespace()
            .map(|token| token.parse().expect("integer"))
            .collect()
    }

    #[test]
    fn formats_the_stock_six_int_entry() {
        let out = format_tinfo(&[row(0, 100, 25), row(7, 5, 0)]);
        assert!(out.starts_with("tinfo 2 "), "{out}");
        let nums = nums(&out);
        assert_eq!(nums.len(), 1 + 2 * STRIDE);
        // Header count, then clientNum first in every entry.
        assert_eq!(nums[0], 2);
        assert_eq!(nums[1], 0);
        assert_eq!(nums[1 + STRIDE], 7);
        // Field order: client, location, health, armor, weapon, powerups.
        assert_eq!(&nums[1..1 + STRIDE], &[0, 4, 100, 25, 3, 0]);
        assert_eq!(&nums[1 + STRIDE..1 + 2 * STRIDE], &[7, 4, 5, 0, 3, 0]);
    }

    #[test]
    fn negative_health_and_armor_are_clamped_to_zero() {
        let out = format_tinfo(&[Row::clamped(2, 0, -40, -3, 0, 0)]);
        let nums = nums(&out);
        assert_eq!(&nums[1..1 + STRIDE], &[2, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn an_empty_roster_is_a_valid_zero_count_message() {
        assert_eq!(format_tinfo(&[]), "tinfo 0");
    }

    #[test]
    fn the_roster_is_capped_at_team_maxoverlay() {
        let rows: Vec<Row> = (0..40).map(|n| row(n, 100, 0)).collect();
        let nums = nums(&format_tinfo(&rows));
        assert_eq!(nums[0] as usize, TEAM_MAXOVERLAY);
    }

    #[test]
    fn the_command_stays_inside_the_engine_reliable_buffer() {
        // Extreme field values (corrupt/very wide game state) would overflow
        // the 1022-byte reliable budget: the tail is dropped whole and the
        // header count follows what was written.
        let rows: Vec<Row> = (0..TEAM_MAXOVERLAY)
            .map(|n| Row {
                client: n as i32,
                location: i32::MIN,
                health: i32::MAX,
                armor: i32::MAX,
                weapon: i32::MAX,
                powerups: i32::MAX,
            })
            .collect();
        let out = format_tinfo(&rows);
        assert!(out.len() <= MAX_COMMAND_LEN, "{} bytes", out.len());
        let nums = nums(&out);
        let emitted = nums[0] as usize;
        assert!(emitted > 0 && emitted < TEAM_MAXOVERLAY);
        assert_eq!(nums.len(), 1 + emitted * STRIDE);
        for i in 0..emitted {
            assert_eq!(nums[1 + i * STRIDE], i as i32);
        }
    }

    #[test]
    fn the_roster_only_lists_the_followed_team() {
        assert!(roster_includes(TEAM_RED, TEAM_RED, 0, None));
        assert!(!roster_includes(TEAM_RED, TEAM_BLUE, 0, None));
        assert!(!roster_includes(TEAM_RED, TEAM_SPECTATOR, 0, None));
        assert!(!roster_includes(TEAM_RED, crate::sdk::TEAM_FREE, 0, None));
    }

    #[test]
    fn parallel_tffa_roster_is_limited_to_the_followed_instance() {
        assert!(roster_includes(TEAM_RED, TEAM_RED, 2, Some(2)));
        assert!(!roster_includes(TEAM_RED, TEAM_RED, 1, Some(2)));
        // Without the feature (no filter) every same-team player is listed.
        assert!(roster_includes(TEAM_RED, TEAM_RED, 1, None));
    }

    // -----------------------------------------------------------------
    // Synthetic game memory: the live readers are exercised against an
    // image laid out at the same SDK offsets, so the offset arithmetic
    // (spectator follow target, `inuse`, team, health/armor/weapon/location/
    // powerups) is covered without an engine.
    // -----------------------------------------------------------------

    struct GameImage {
        entities: Vec<u8>,
        level: Vec<u8>,
        clients: Vec<u8>,
    }

    impl GameImage {
        fn new() -> Self {
            let mut image = Self {
                entities: vec![0u8; SIZEOF_GENTITY * MAX_CLIENTS],
                level: vec![0u8; OFFSET_LEVEL_FOLLOW2 + 4],
                clients: vec![0u8; crate::sdk::SIZEOF_GCLIENT * MAX_CLIENTS],
            };
            let clients = image.clients.as_ptr() as usize;
            for slot in 0..MAX_CLIENTS {
                // `read_usize` width on the test host (the shipped module is
                // i386; both sides of this image use the host width).
                let base = clients + slot * crate::sdk::SIZEOF_GCLIENT;
                let at = slot * SIZEOF_GENTITY + OFFSET_GENTITY_CLIENT;
                image.entities[at..at + core::mem::size_of::<usize>()]
                    .copy_from_slice(&base.to_ne_bytes());
            }
            image
        }

        fn entities_ptr(&self) -> usize {
            self.entities.as_ptr() as usize
        }

        fn level_ptr(&self) -> usize {
            self.level.as_ptr() as usize
        }

        /// Client-relative field write (`gclient_t` + field offset).
        fn client_field(&mut self, slot: usize, field: usize) -> &mut [u8] {
            let at = slot * crate::sdk::SIZEOF_GCLIENT + field;
            &mut self.clients[at..]
        }

        fn set(&mut self, slot: usize, field: usize, value: i32) {
            self.client_field(slot, field)[..4].copy_from_slice(&value.to_ne_bytes());
        }

        fn detach(&mut self, slot: usize) {
            let at = slot * SIZEOF_GENTITY + OFFSET_GENTITY_CLIENT;
            self.entities[at..at + core::mem::size_of::<usize>()]
                .copy_from_slice(&0usize.to_ne_bytes());
        }

        fn set_inuse(&mut self, slot: usize, value: i32) {
            let at = slot * SIZEOF_GENTITY + OFFSET_GENTITY_INUSE;
            self.entities[at..at + 4].copy_from_slice(&value.to_ne_bytes());
        }

        fn set_session_team(&mut self, slot: usize, value: i32) {
            self.set(slot, OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM, value);
        }

        fn set_spectator_state(&mut self, slot: usize, value: i32) {
            self.set(
                slot,
                OFFSET_GCLIENT_SESS + OFFSET_SESS_SPECTATOR_STATE,
                value,
            );
        }

        fn set_spectator_client(&mut self, slot: usize, value: i32) {
            self.set(
                slot,
                OFFSET_GCLIENT_SESS + OFFSET_SESS_SPECTATOR_CLIENT,
                value,
            );
        }

        fn set_team_info(&mut self, slot: usize, value: i32) {
            self.set(slot, OFFSET_GCLIENT_PERS + OFFSET_PERS_TEAM_INFO, value);
        }

        fn set_connected(&mut self, slot: usize, value: i32) {
            self.set(slot, OFFSET_GCLIENT_PERS + OFFSET_PERS_CONNECTED, value);
        }

        fn set_location(&mut self, slot: usize, value: i32) {
            self.set(
                slot,
                OFFSET_GCLIENT_PERS + OFFSET_PERS_TEAM_STATE_LOCATION,
                value,
            );
        }

        fn set_health(&mut self, slot: usize, value: i32) {
            self.set(
                slot,
                OFFSET_GCLIENT_PS + OFFSET_PS_STATS + STAT_HEALTH * 4,
                value,
            );
        }

        fn set_armor(&mut self, slot: usize, value: i32) {
            self.set(
                slot,
                OFFSET_GCLIENT_PS + OFFSET_PS_STATS + STAT_ARMOR * 4,
                value,
            );
        }

        fn set_weapon(&mut self, slot: usize, value: i32) {
            self.set(slot, OFFSET_GCLIENT_PS + OFFSET_PS_WEAPON, value);
        }

        fn set_powerups(&mut self, slot: usize, value: i32) {
            let at = slot * SIZEOF_GENTITY + OFFSET_GENTITY_S + OFFSET_ENTITY_POWERUPS;
            self.entities[at..at + 4].copy_from_slice(&value.to_ne_bytes());
        }

        fn set_follow(&mut self, which: usize, value: i32) {
            let at = if which == 1 {
                OFFSET_LEVEL_FOLLOW1
            } else {
                OFFSET_LEVEL_FOLLOW2
            };
            self.level[at..at + 4].copy_from_slice(&value.to_ne_bytes());
        }
    }

    /// A connected client following `target` (team `team`) on `slot`.
    fn following(image: &mut GameImage, slot: usize, target: usize, team: i32) {
        image.set_connected(slot, CON_CONNECTED);
        image.set_session_team(slot, TEAM_SPECTATOR);
        image.set_spectator_state(slot, SPECTATOR_FOLLOW);
        image.set_spectator_client(slot, target as i32);
        image.set_team_info(slot, 1);
        image.set_session_team(target, team);
    }

    #[test]
    fn followed_player_resolves_the_watched_team() {
        let mut image = GameImage::new();
        following(&mut image, 3, 1, TEAM_BLUE);
        // SAFETY: synthetic image laid out at the SDK offsets.
        let got = unsafe { followed_player(3, image.entities_ptr(), image.level_ptr()) };
        assert_eq!(got, Some((1, TEAM_BLUE)));
    }

    #[test]
    fn only_a_following_spectator_has_a_scope() {
        let mut image = GameImage::new();
        // A player is not a spectator.
        image.set_session_team(0, TEAM_RED);
        image.set_spectator_state(0, SPECTATOR_FOLLOW);
        image.set_team_info(0, 1);
        image.set_session_team(1, TEAM_RED);
        image.set_spectator_client(0, 1);
        assert_eq!(
            unsafe { followed_player(0, image.entities_ptr(), image.level_ptr()) },
            None
        );

        // A free-flying spectator follows nobody.
        image.set_session_team(0, TEAM_SPECTATOR);
        image.set_spectator_state(0, crate::sdk::SPECTATOR_FREE);
        image.set_connected(0, CON_CONNECTED);
        assert_eq!(
            unsafe { followed_player(0, image.entities_ptr(), image.level_ptr()) },
            None
        );

        // A spectator slot that is not connected yet is skipped.
        following(&mut image, 0, 1, TEAM_RED);
        image.set_connected(0, crate::sdk::CON_DISCONNECTED);
        assert_eq!(
            unsafe { followed_player(0, image.entities_ptr(), image.level_ptr()) },
            None
        );

        // A spectator that opted out of team info is not served (this is also
        // the pristine `TeamplayInfoMessage` gate).
        following(&mut image, 0, 1, TEAM_RED);
        image.set_team_info(0, 0);
        assert_eq!(
            unsafe { followed_player(0, image.entities_ptr(), image.level_ptr()) },
            None
        );
    }

    #[test]
    fn auto_follow_slots_resolve_through_the_level() {
        let mut image = GameImage::new();
        following(&mut image, 4, 1, TEAM_RED);
        image.set_spectator_client(4, -1);
        image.set_follow(1, 2);
        image.set_session_team(2, TEAM_BLUE);
        assert_eq!(
            unsafe { followed_player(4, image.entities_ptr(), image.level_ptr()) },
            Some((2, TEAM_BLUE))
        );

        image.set_spectator_client(4, -2);
        image.set_follow(2, 5);
        image.set_session_team(5, TEAM_RED);
        assert_eq!(
            unsafe { followed_player(4, image.entities_ptr(), image.level_ptr()) },
            Some((5, TEAM_RED))
        );
    }

    #[test]
    fn following_another_spectator_is_not_a_team_scope() {
        let mut image = GameImage::new();
        following(&mut image, 3, 1, TEAM_SPECTATOR);
        assert_eq!(
            unsafe { followed_player(3, image.entities_ptr(), image.level_ptr()) },
            None
        );
    }

    #[test]
    fn the_roster_reads_the_pristine_fields() {
        let mut image = GameImage::new();
        // RED slots 0 and 2 (2 has a live state), BLUE slot 1, an unused slot
        // 3, an in-use slot without a client pointer 4.
        image.set_inuse(0, 1);
        image.set_session_team(0, TEAM_RED);
        image.set_location(0, 7);
        image.set_health(0, 100);
        image.set_armor(0, 25);
        image.set_weapon(0, 3);
        image.set_powerups(0, 1 << 2);

        image.set_inuse(1, 1);
        image.set_session_team(1, TEAM_BLUE);
        image.set_health(1, 100);

        image.set_inuse(2, 1);
        image.set_session_team(2, TEAM_RED);
        image.set_health(2, -12); // dead: the pristine body clamps to 0
        image.set_armor(2, -1);
        image.set_weapon(2, 1);

        image.set_inuse(3, 0);
        image.set_session_team(3, TEAM_RED);

        image.set_inuse(4, 1);
        image.detach(4);

        let matches = [0u8; MAX_CLIENTS];
        // SAFETY: synthetic image laid out at the SDK offsets.
        let rows = unsafe { collect_rows(image.entities_ptr(), TEAM_RED, None, &matches) };
        assert_eq!(
            rows,
            vec![
                Row::clamped(0, 7, 100, 25, 3, 1 << 2),
                Row::clamped(2, 0, 0, 0, 1, 0),
            ]
        );
    }

    #[test]
    fn the_roster_honours_the_parallel_match_filter() {
        let mut image = GameImage::new();
        for slot in [0usize, 1, 2] {
            image.set_inuse(slot, 1);
            image.set_session_team(slot, TEAM_RED);
            image.set_health(slot, 100);
        }
        let mut matches = [0u8; MAX_CLIENTS];
        matches[1] = 2;
        matches[2] = 3;
        // SAFETY: synthetic image laid out at the SDK offsets.
        let in_match_2 = unsafe { collect_rows(image.entities_ptr(), TEAM_RED, Some(2), &matches) };
        assert_eq!(in_match_2.len(), 1);
        assert_eq!(in_match_2[0].client, 1);
        let unfiltered = unsafe { collect_rows(image.entities_ptr(), TEAM_RED, None, &matches) };
        assert_eq!(unfiltered.len(), 3);
    }
}
