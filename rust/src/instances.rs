//! The shared per-instance core (D-006, D-025).
//!
//! Both instancing features — parallel self-organized TFFA matches
//! (`crate::tffa`) and the parallel duel ladder (`crate::duel`) — are overlays
//! on the one pristine game module: the engine and the game own a single
//! `level`, `g_entities` and set of worlds. This module owns everything that
//! divides that one world into isolated instances:
//!
//! - the client→instance assignment (`client_match`), including the
//!   follow-aware effective instance of a spectator;
//! - the isolation gates (snapshot visibility, damage, trace/touch/query
//!   flips, sound attribution, spawn separation);
//! - the per-instance world (entity tags, clone pass, runtime-spawn
//!   attribution, release);
//! - the acting scope game code runs in;
//! - the per-viewer `cs` plumbing (send, dedup, reliable-window deferral,
//!   `loaddefered`).
//!
//! Feature policy stays out: `crate::tffa` owns team matches, `crate::duel`
//! owns the duel ladder. Both use the same assignment slate and gates, so a
//! server runs exactly one of them (the gametypes are mutually exclusive).

use core::ffi::c_int;

use crate::hooks::engine_sv::{read_i32, read_usize, write_i32, write_usize};
use crate::sdk::{
    CS_PLAYERS, CS_SCORES1, CS_SCORES2, MAX_CLIENTS, MAX_GENTITIES, MAX_NETNAME, OFFSET_GCLIENT_PS,
    OFFSET_GCLIENT_SESS, OFFSET_GENTITY_ACTIVATOR, OFFSET_GENTITY_CLIENT, OFFSET_GENTITY_ENEMY,
    OFFSET_GENTITY_LAST_ENEMY, OFFSET_GENTITY_NEXT_TRAIN, OFFSET_GENTITY_PARENT,
    OFFSET_GENTITY_PREV_TRAIN, OFFSET_GENTITY_TARGET_ENT, OFFSET_GENTITY_TEAMCHAIN,
    OFFSET_GENTITY_TEAMMASTER, OFFSET_PS_LEGS_FLIP, OFFSET_PS_PERSISTANT, OFFSET_PS_TORSO_FLIP,
    OFFSET_SESS_SESSION_TEAM, OFFSET_SESS_SPECTATOR_CLIENT, OFFSET_SESS_SPECTATOR_STATE,
    PERS_KILLED, PERS_SCORE, SIZEOF_GENTITY, SPECTATOR_FOLLOW, TEAM_BLUE, TEAM_RED, TEAM_SPECTATOR,
};
use crate::state;

/// Whether the shared instance core is active.
pub use crate::state::instances_enabled;

/// Match id of the implicit main TFFA.
pub const MAIN_MATCH: u8 = 0;

/// Maximum player-created matches (ids `1..=MAX_MATCHES`).
pub const MAX_MATCHES: usize = 8;

/// Backing array size: main slot + created slots.
pub const TOTAL_MATCH_SLOTS: usize = 1 + MAX_MATCHES;

/// Capacity of the per-frame player-owned entity cache (saber entities,
/// missiles, corpses — one or a few per player).
pub const OWNED_CAP: usize = 96;

/// No remembered membership: a `TffaMatch::remembered` slot (or a
/// `TffaState::pending_join` entry) holding this belongs to no finished
/// instance. The team constants (`TEAM_RED` 1 / `TEAM_BLUE` 2) are the only
/// other values either table stores (D-006.10).
pub const REMEMBERED_NONE: u8 = 0;

/// Instance tag of an entity belonging to no instance: visible and solid in
/// every instance (players, missiles, temp ents, inert props).
pub const ENTITY_SHARED: u8 = u8::MAX;

/// Attribution of a temp entity that belongs to no match: map/world sounds
/// (doors, item respawns, `G_SoundAtLoc`, NPC chatter) stay audible for every
/// match.
pub const SOURCE_WORLD: u8 = u8::MAX;

/// No acting scope is open: `G_Find` is the pristine global search and a
/// `G_Spawn` made now belongs to no instance. Shares `ENTITY_SHARED` with
/// "untagged entity" because both mean "no instance filter", and a scope is only
/// ever a real match id (`0..TOTAL_MATCH_SLOTS`).
pub const SCOPE_NONE: u8 = ENTITY_SHARED;

/// Frames the engine runs between `SV_RestartGameProgs` and the client
/// reconnect burst of a `map_restart` (`SV_MapRestart_f`, `sv_ccmds.cpp:306-349`:
/// three unconditional `GAME_RUN_FRAME` calls, then the connect loop). Used as
/// the fallback length of the settle window when no client is left to reconnect
/// and so no `GAME_CLIENT_CONNECT` can end it.
pub(crate) const RESTART_SETTLE_FRAMES: u8 = 3;

/// Frames the spectator playerstate animation flip is held for (D-006.14).
/// `SpectatorClientEndFrame` rewrites the spectator's ps from the followed
/// player every game frame, so the toggle must be re-applied each frame; the
/// client sees the flip once and then the restored value once.
pub(crate) const ANIM_PS_FLIP_FRAMES: u8 = 5;

/// Backlog at which the proxy stops adding reliable commands to a client,
/// leaving the rest of the 128-slot window to the pristine game's own
/// `map_restart` broadcasts (about four `cs`/`cp`/`print` commands per
/// reconnecting client, so a 24-slot server's burst alone is already ~96).
/// Above it a correction is deferred to a later frame instead of being queued
/// into the burst (D-006.12).
pub(crate) const RELIABLE_SEND_LIMIT: i32 = 16;

/// `GAME_RUN_FRAME` calls the engine runs after `GAME_INIT` before the first
/// real frame of a `map_restart`: three settle frames, then the client
/// reconnect loop, then one trailing settle frame (`SV_MapRestart_f`,
/// `sv_ccmds.cpp:306-349`). Used as the bounded fallback that ends the burst
/// even if the trailing frame never arrives (a modified engine).
pub(crate) const RESTART_BURST_MAX_FRAMES: u8 = 8;

/// Frames' worth of deferred `GAME_CLIENT_BEGIN` work: how many clients may be
/// auto-joined and refreshed per frame once the burst has been acknowledged.
/// Each auto-join makes the pristine game broadcast to every client, so a
/// whole-server burst would recreate the same overflow one frame later; a
/// small batch keeps each frame's addition bounded while every client is
/// served within a few frames (D-006.12).
pub(crate) const DEFERRED_BEGINS_PER_FRAME: usize = 4;

/// `gentity_t` pointer fields that can name another entity. A duplicated
/// team/train chain must stay inside its own instance, so a clone's links are
/// re-pointed at the corresponding clone. A pointer that resolves outside the
/// set (a player, an inert shared prop) is left exactly as copied — players
/// and inert props are shared in every instance by design.
pub(crate) const CLONE_LINK_FIELDS: [usize; 9] = [
    OFFSET_GENTITY_PARENT,
    OFFSET_GENTITY_NEXT_TRAIN,
    OFFSET_GENTITY_PREV_TRAIN,
    OFFSET_GENTITY_TARGET_ENT,
    OFFSET_GENTITY_ENEMY,
    OFFSET_GENTITY_LAST_ENEMY,
    OFFSET_GENTITY_ACTIVATOR,
    OFFSET_GENTITY_TEAMCHAIN,
    OFFSET_GENTITY_TEAMMASTER,
];

/// Match of a client (`0` = main TFFA). Out-of-range or connecting → main.
pub fn client_match(client_num: i32) -> u8 {
    if !(0..MAX_CLIENTS as i32).contains(&client_num) {
        return MAIN_MATCH;
    }
    state::with_state(|s| s.tffa.client_match[client_num as usize])
}

/// What a viewer sees: their own match, else the followed player's match
/// (spectator follow adopts the target's instance).
pub fn effective_match(viewer: i32) -> u8 {
    if !(0..MAX_CLIENTS as c_int).contains(&viewer) {
        return MAIN_MATCH;
    }
    // Following someone sets your instance to theirs (spectator travel).
    if let Some(target) = follow_target(viewer) {
        return client_match(target);
    }
    state::with_state(|s| s.tffa.client_match[viewer as usize])
}

/// Followed player of a spectating viewer (`None` when not following).
pub(crate) fn follow_target(viewer: c_int) -> Option<c_int> {
    use crate::sdk::{
        OFFSET_GCLIENT_SESS, OFFSET_SESS_SPECTATOR_CLIENT, OFFSET_SESS_SPECTATOR_STATE,
        SIZEOF_GENTITY, SPECTATOR_FOLLOW,
    };
    if session_team_of(viewer) != Some(TEAM_SPECTATOR) {
        return None;
    }
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
        return None;
    }
    let ent = g_entities + viewer as usize * SIZEOF_GENTITY;
    // SAFETY: ent is inside the g_entities array.
    let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
    if client == 0 {
        return None;
    }
    // SAFETY: client is a valid gclient_t; sess fields are plain ints.
    let st = unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SPECTATOR_STATE) };
    if st != SPECTATOR_FOLLOW {
        return None;
    }
    let raw = unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SPECTATOR_CLIENT) };
    // `team follow1`/`follow2` store `-1`/`-2` and follow whatever client is
    // playing (`SpectatorClientEndFrame`, `g_active.c:3660-3668`); resolve
    // them here too, or a spectator watching through those aliases keeps
    // their own instance for `CS_PLAYERS`/scores while the snapshot (built
    // from the followed player's `ps.clientNum`) shows the watched instance:
    // the watched players then render with a `(TFFA N)` prefix, a spectator
    // team and their own model/saber colour.
    let target = if raw == -1 || raw == -2 {
        let level = crate::original::level_address();
        if level == 0 {
            return None;
        }
        let offset = if raw == -1 {
            crate::sdk::OFFSET_LEVEL_FOLLOW1
        } else {
            crate::sdk::OFFSET_LEVEL_FOLLOW2
        };
        // SAFETY: level is the game's level_locals_t; follow1/follow2 are ints.
        resolve_follow_target(raw, unsafe { read_i32(level + offset) }, -1)?
    } else {
        resolve_follow_target(raw, -1, -1)?
    };
    let tent = g_entities + target as usize * SIZEOF_GENTITY;
    // SAFETY: target slot is inside the g_entities array.
    let tclient = unsafe { read_usize(tent + OFFSET_GENTITY_CLIENT) };
    if tclient == 0 {
        return None;
    }
    Some(target)
}

/// Pure half of [`follow_target`]'s alias resolution: `spectatorClient == -1`
/// / `-2` means `level.follow1` / `level.follow2`, every other value is the
/// client slot itself (pristine `SpectatorClientEndFrame`, `g_active.c:3660-3668`).
/// `None` when the resolved target is not a client slot (nothing to follow).
pub(crate) fn resolve_follow_target(raw: c_int, follow1: c_int, follow2: c_int) -> Option<c_int> {
    let target = match raw {
        -1 => follow1,
        -2 => follow2,
        other => other,
    };
    (0..MAX_CLIENTS as c_int)
        .contains(&target)
        .then_some(target)
}

/// Pure damage gate: blocked unless both ends share one match.
pub fn damage_blocked(attacker_match: u8, victim_match: u8) -> bool {
    attacker_match != victim_match
}

/// Pure snapshot gate: `true` = keep entity for viewer.
pub fn entity_visible(viewer_match: u8, ent_match: u8) -> bool {
    viewer_match == ent_match
}

/// Visibility gate for a tagged world entity (per-instance world entities,
/// D-006 extension): `ENTITY_SHARED` is visible to every viewer, a tagged one
/// only to its own instance.
pub fn world_entity_visible(viewer_match: u8, tag: u8) -> bool {
    tag == ENTITY_SHARED || viewer_match == tag
}

/// Session team of a client from game memory (`None` when unknown).
pub fn session_team_of(client_num: c_int) -> Option<i32> {
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    session_team_at(g_entities, client_num)
}

/// Session team of a client, read through an already-located `g_entities`
/// base. Split from `session_team_of` so a caller that already holds the state
/// lock (the end-of-match roster capture) can read it without re-entering it.
pub(crate) fn session_team_at(g_entities: usize, client_num: c_int) -> Option<i32> {
    if g_entities == 0 || !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return None;
    }
    let ent = g_entities + client_num as usize * crate::sdk::SIZEOF_GENTITY;
    // SAFETY: ent is inside the g_entities array; client is a pointer field.
    let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
    if client == 0 {
        return None;
    }
    // SAFETY: client is a valid gclient_t.
    Some(unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) })
}

/// Parse a Quake info string (`n\Bob\t\1\...`, with or without leading `\`)
/// into ordered pairs.
pub(crate) fn info_pairs(info: &str) -> Vec<(String, String)> {
    let mut parts: Vec<&str> = info.split('\\').collect();
    // Drop a leading empty part from a leading backslash.
    if parts.first() == Some(&"") {
        parts.remove(0);
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < parts.len() {
        out.push((parts[i].to_owned(), parts[i + 1].to_owned()));
        i += 2;
    }
    out
}

/// Read one key from an info string.
pub fn info_value(info: &str, key: &str) -> Option<String> {
    for (k, v) in info_pairs(info) {
        if k == key {
            return Some(v);
        }
    }
    None
}

/// Set one key in an info string, preserving the other pairs and the
/// leading-backslash style of the input. Appends when missing.
pub fn info_set(info: &str, key: &str, value: &str) -> String {
    let leading = info.starts_with('\\');
    let mut pairs = info_pairs(info);
    let mut found = false;
    for (k, v) in pairs.iter_mut() {
        if k == key {
            *v = value.to_owned();
            found = true;
            break;
        }
    }
    if !found {
        pairs.push((key.to_owned(), value.to_owned()));
    }
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i == 0 && !leading {
            out.push_str(&format!("{k}\\{v}"));
        } else {
            out.push_str(&format!("\\{k}\\{v}"));
        }
    }
    out
}

/// `(TFFA N) ` prefix length for single-digit matches (0..=8): 9 bytes.
pub(crate) fn prefix_for(player_match: u8) -> String {
    format!("(TFFA {player_match}) ")
}

/// Prefixed display name, truncated so the whole thing fits `MAX_NETNAME`
/// (36 inc NUL → 35 bytes max). Truncation respects UTF-8 boundaries; color
/// escapes may be cut mid-code in pathological cases, same as the game's own
/// `Q_strncpyz` behaviour.
pub fn prefixed_name(orig: &str, player_match: u8) -> String {
    let prefix = prefix_for(player_match);
    let max_total = MAX_NETNAME.saturating_sub(1);
    let budget = max_total.saturating_sub(prefix.len());
    let mut cut = orig.len().min(budget);
    while cut > 0 && !orig.is_char_boundary(cut) {
        cut -= 1;
    }
    // Avoid leaving a bare `^` (color escape) at the cut.
    if cut > 0 && orig.as_bytes()[cut - 1] == b'^' {
        cut -= 1;
    }
    format!("{prefix}{}", &orig[..cut])
}

/// Rewrite a `CS_PLAYERS + n` info string for `viewer`:
/// playing outsiders (`player_match != viewer_match`, base team RED/BLUE)
/// get a `(TFFA N)` name prefix and are forced to spectator. The `scores`
/// rewrite already leaves them out of the scoreboard, so this keeps them out
/// of the viewer's team overlay (and names them as another match's player if
/// any other client-side surface shows them). Everyone else (own match, real
/// spectators) is left untouched. Returns `None` when no change.
pub fn rewrite_cs_players_for_viewer(
    base: &str,
    player_match: u8,
    viewer_match: u8,
) -> Option<String> {
    if base.is_empty() {
        return None;
    }
    if player_match == viewer_match {
        return None;
    }
    let team = info_value(base, "t")?.parse::<i32>().ok()?;
    if team != TEAM_RED && team != TEAM_BLUE {
        return None;
    }
    let orig = info_value(base, "n").unwrap_or_default();
    let out = info_set(base, "n", &prefixed_name(&orig, player_match));
    Some(info_set(&out, "t", &TEAM_SPECTATOR.to_string()))
}
