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

/// Reliable commands the engine still owes `client_num`: `reliableSequence -
/// reliableAcknowledge`, the number queued but not yet acknowledged.
///
/// The shipped engine buffers `MAX_RELIABLE_COMMANDS` (128) per client and
/// drops a client when it would overwrite an unacknowledged command
/// (`SV_AddServerCommand`, `sv_main.cpp:130-149`); a client whose cgame falls
/// more than that behind prints `CL_GetServerCommand: a reliable command was
/// cycled out` and drops. `None` when the engine client table is not
/// reachable (unit tests, feature off, before `GAME_INIT`).
pub(crate) fn reliable_backlog(client_num: c_int) -> Option<i32> {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) || crate::original::base() == 0 {
        return None;
    }
    // SAFETY: engine memory layer initialised (this path only runs from the
    // game module after GAME_INIT); `clients_base` is `svs.clients` and the
    // client index is in range.
    let base = unsafe { crate::hooks::engine_sv::clients_base() };
    if base == 0 {
        return None;
    }
    let cl = base + client_num as usize * crate::sdk::SIZEOF_CLIENT_T;
    // SAFETY: cl is a valid client_t; both fields are plain ints.
    unsafe {
        Some(
            read_i32(cl + crate::sdk::OFFSET_CLIENT_RELIABLE_SEQUENCE)
                - read_i32(cl + crate::sdk::OFFSET_CLIENT_RELIABLE_ACKNOWLEDGE),
        )
    }
}

/// Whether `client_num` has too many unacknowledged reliable commands for the
/// proxy to add another one now. Pure half of [`reliable_pressure`] so the
/// threshold is unit-tested without an engine.
pub(crate) fn backlog_is_pressured(backlog: Option<i32>) -> bool {
    backlog.is_some_and(|n| n >= RELIABLE_SEND_LIMIT)
}

/// Whether a proxy `cs` correction must wait for a later frame. Reads the
/// engine's per-client window (see [`reliable_backlog`]).
pub(crate) fn reliable_pressure(client_num: c_int) -> bool {
    backlog_is_pressured(reliable_backlog(client_num))
}

/// Send `cs <index> "<value>"` to one client (the same wire format the
/// engine's own `SV_SetConfigstring` broadcast uses, so the client's
/// `gameState` updates identically). `"` becomes `'` and CR/LF become spaces
/// so a hostile player name cannot break the quoting/framing (names allow
/// `0x22` through `ClientCleanName`).
///
/// A client whose reliable window is already full of the `map_restart` burst
/// gets the correction deferred (`pending_view_refresh`) instead: pushing it
/// now is what overflowed the buffer, dropped clients and made real clients
/// print `CL_GetServerCommand: a reliable command was cycled out` (D-006.12).
pub(crate) fn send_cs_to_client(client_num: c_int, index: i32, value: &str) -> bool {
    // Never add to the `map_restart` reconnect burst: the engine queues a
    // reliable command per reconnecting client for every client, and a
    // populated server is already close to the 128-slot limit before the proxy
    // adds anything (D-006.12). The client is re-pushed after the burst.
    if reliable_pressure(client_num) || state::with_state(|s| s.tffa.restart_burst) {
        if (0..MAX_CLIENTS as c_int).contains(&client_num) {
            // A deferred **base** `CS_PLAYERS` push is the one that can leave a
            // same-match player demoted on this viewer only (D-006.16); log it
            // at the verbose level so a recurrence points at the client. The
            // flag below re-queues it on the next full re-push either way.
            if crate::diagnostics::verbose_enabled() {
                let (slot, base, player_match) = state::with_state(|s| {
                    s.tffa.pending_view_refresh[client_num as usize] = true;
                    let slot = cs_players_slot(index);
                    let base = slot.is_some_and(|slot| s.tffa.cs_players_base[slot] == value);
                    let player_match = slot.map(|slot| s.tffa.client_match[slot]);
                    (slot, base, player_match)
                });
                if let Some(slot) = slot {
                    crate::diagnostics::verbose(
                        "tffa.cs_players.deferred",
                        format!(
                            "{} slot={slot} base={base} player_match={}",
                            crate::hooks::engine_sv::client_label_for_num(client_num),
                            player_match.unwrap_or(MAIN_MATCH),
                        ),
                    );
                }
            } else {
                state::with_state(|s| s.tffa.pending_view_refresh[client_num as usize] = true);
            }
        }
        return false;
    }
    let mut safe = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => safe.push('\''),
            '\n' | '\r' => safe.push(' '),
            c => safe.push(c),
        }
    }
    let msg = format!("cs {index} \"{safe}\"\n");
    let Ok(cmsg) = std::ffi::CString::new(msg) else {
        return false;
    };
    // SAFETY: engine syscall registered; cmsg NUL-terminated.
    unsafe { crate::syscall::send_server_command(client_num, &cmsg) };
    // A changed `CS_PLAYERS` entry makes cgame rebuild that player's
    // clientinfo (`CG_NewClientInfo`, `cg_players.c:1499`). With
    // `cg_deferPlayers` on (the default) a team change can make it copy another
    // client's model and mark the entry deferred, which renders the player with
    // the wrong skin and in bind pose until the scoreboard runs
    // `CG_LoadClientInfo`. Queue cgame's own `loaddefered` server command for
    // the viewer after the `cs` correction (D-006.14). Only a player of the
    // viewer's own instance is marked: a demoted outsider is not in the
    // viewer's snapshots, so its model can stay deferred (that is what
    // `cg_deferPlayers` is for) until it actually becomes visible.
    if let Some(slot) = cs_players_slot(index) {
        if (0..MAX_CLIENTS as c_int).contains(&client_num) {
            let player_match = state::with_state(|s| s.tffa.client_match[slot]);
            if player_match == effective_match(client_num) {
                state::with_state(|s| {
                    s.tffa.anim_reload_pending[client_num as usize] = true;
                    s.tffa.anim_ps_flip_pending[client_num as usize] = ANIM_PS_FLIP_FRAMES;
                });
            }
        }
    }
    true
}

/// `CS_PLAYERS + slot` slot of a configstring index (`None` for every other
/// configstring). Pure so the index arithmetic is unit-tested.
pub(crate) fn cs_players_slot(index: i32) -> Option<usize> {
    let slot = index - CS_PLAYERS;
    (0..MAX_CLIENTS as i32)
        .contains(&slot)
        .then_some(slot as usize)
}

/// Re-apply the followed player's animation for a spectator whose per-viewer
/// `CS_PLAYERS` correction replaced that player's clientinfo/model (D-006.14).
///
/// The engine never sends the followed player's entity to a follower ("never
/// send client's own entity", `sv_snapshot.cpp`), so cgame renders it from
/// `cg.snap->ps` and an entity-state reset/flip cannot reach it.
/// `SpectatorClientEndFrame` copies the followed player's ps into the
/// spectator's ps every frame (`g_active.c:3655-3680`), and
/// `CG_TransitionSnapshot` builds the followed cent from it; toggling
/// `ps.legsFlip`/`ps.torsoFlip` there makes `CG_RunLerpFrame` see a flip
/// change and re-apply the current animation to the recreated ghoul2 instance.
/// Only a following spectator is touched; the game rewrites the ps next frame,
/// so the flip never leaks into the followed player's real state.
pub(crate) fn apply_pending_spectator_anim_flips() {
    let pending = state::with_state(|s| s.tffa.anim_ps_flip_pending);
    if pending.iter().all(|&left| left == 0) {
        return;
    }
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    for (viewer, &left) in pending.iter().enumerate() {
        if left != 0 && g_entities != 0 && follow_target(viewer as c_int).is_some() {
            let ent = g_entities + viewer * SIZEOF_GENTITY;
            // SAFETY: viewer < MAX_CLIENTS, so `ent` is inside the g_entities
            // array; a following spectator is a live client.
            let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
            if client != 0 {
                let ps = client + OFFSET_GCLIENT_PS;
                // SAFETY: `ps` is the live playerState_t of the spectator, whose
                // flips were just copied from the followed player.
                unsafe {
                    let legs = read_i32(ps + OFFSET_PS_LEGS_FLIP);
                    let torso = read_i32(ps + OFFSET_PS_TORSO_FLIP);
                    write_i32(ps + OFFSET_PS_LEGS_FLIP, legs ^ 1);
                    write_i32(ps + OFFSET_PS_TORSO_FLIP, torso ^ 1);
                }
            }
        }
    }
    state::with_state(|s| {
        for (viewer, left) in s.tffa.anim_ps_flip_pending.iter_mut().enumerate() {
            *left = pending[viewer].saturating_sub(1);
        }
    });
}

/// Send cgame's `loaddefered` server command to every viewer whose
/// `CS_PLAYERS` correction may have deferred a player model (D-006.14).
///
/// The cgame server-command table carries `{ "loaddefered", CG_LoadDeferredPlayers }`
/// (`cg_servercmds.c`), and `CG_ActualLoadDeferredPlayers` runs at the start of
/// the next `CG_Player`; `CG_LoadClientInfo` there re-duplicates the real model
/// to the centity and calls `CG_ResetPlayerEntity`, which is also what fixes
/// the skin and the bind pose. Sending it after the `cs` commands keeps the
/// command order (reliable commands are delivered in order): the client first
/// updates clientinfo, then loads the now-deferred model.
pub(crate) fn flush_deferred_model_loads() {
    let pending = state::with_state(|s| s.tffa.anim_reload_pending);
    if pending.iter().all(|&owed| !owed) {
        return;
    }
    let restart_burst = state::with_state(|s| s.tffa.restart_burst);
    for (viewer, &owed) in pending.iter().enumerate() {
        if !owed {
            continue;
        }
        // Never add to the `map_restart` reconnect burst (D-006.12): keep the
        // flag and send it after the burst, when the clientinfo corrections are
        // re-pushed as well.
        let client_num = viewer as c_int;
        if restart_burst || reliable_pressure(client_num) {
            continue;
        }
        let Ok(cmsg) = std::ffi::CString::new("loaddefered") else {
            continue;
        };
        // SAFETY: engine syscall registered; cmsg NUL-terminated.
        unsafe { crate::syscall::send_server_command(client_num, &cmsg) };
        state::with_state(|s| s.tffa.anim_reload_pending[viewer] = false);
    }
}

/// Queue a `cs CS_PLAYERS + slot` value for one viewer unless that viewer is
/// already known to hold exactly this value. Returns whether anything was
/// queued.
///
/// This is the per-viewer dedup the scoreboard refresh needs: the demotions are
/// re-pushed before every `scores` message (a lost override would put another
/// match's player on the viewer's side), but the payloads are the full player
/// info strings and a populated multi-match server has dozens of them per
/// viewer — re-queueing identical ones every time the tab is pressed is what
/// made that client's reliable stream burst. A value that was **not** queued
/// (reliable-window pressure / restart burst) leaves the record untouched, so
/// the deferred refresh (`pending_view_refresh`) still sends it later.
/// HUD team tallies of an instance. Parallel TFFA matches carry their own
/// red/blue score copies; the duel ladder has no team score (its per-court
/// duelists are tracked proxy-side), so it reports 0-0 and the duel HUD uses
/// its own values.
pub fn view_scores(instance: u8) -> (i32, i32) {
    if !crate::state::team_mode() {
        return (0, 0);
    }
    state::with_state(|s| {
        s.tffa
            .matches
            .get(instance as usize)
            .map(|m| (m.red, m.blue))
            .unwrap_or((0, 0))
    })
}

pub(crate) fn push_cs_players(client_num: c_int, slot: usize, value: &str) -> bool {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) || slot >= MAX_CLIENTS {
        return false;
    }
    let viewer = client_num as usize;
    let index = CS_PLAYERS + slot as i32;
    if state::with_state(|s| s.tffa.cs_players_sent[viewer][slot] == value) {
        return false;
    }
    if !send_cs_to_client(client_num, index, value) {
        return false;
    }
    state::with_state(|s| s.tffa.cs_players_sent[viewer][slot] = value.to_owned());
    true
}

/// Record a `CS_PLAYERS + slot` value this viewer is known to hold without
/// queueing anything (the engine broadcast it, or it came in a gamestate).
pub(crate) fn record_cs_players_sent(client_num: c_int, slot: usize, value: &str) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) || slot >= MAX_CLIENTS {
        return;
    }
    state::with_state(|s| s.tffa.cs_players_sent[client_num as usize][slot] = value.to_owned());
}

/// Record that the engine itself just broadcast `value` for `slot` to every
/// connected client (`G_SET_CONFIGSTRING` forwarding), so the next per-viewer
/// refresh does not re-send the base string to same-match viewers.
pub(crate) fn record_cs_players_broadcast(slot: usize, value: &str) {
    if slot >= MAX_CLIENTS {
        return;
    }
    state::with_state(|s| {
        for viewer in 0..MAX_CLIENTS {
            if s.clients[viewer].is_connected {
                s.tffa.cs_players_sent[viewer][slot] = value.to_owned();
            }
        }
    });
}

/// Drop one viewer's per-slot record because the engine sent it a fresh
/// gamestate, whose configstrings are the server's base strings again: every
/// per-viewer override the proxy believed it held is gone. A connected client
/// is marked for the next frame's full refresh (`pending_view_refresh`); a
/// connecting one gets it from `GAME_CLIENT_BEGIN` anyway.
///
/// Also records that the viewer's next full re-push may treat the stored bases
/// as held (the gamestate just delivered them), which is what keeps the
/// `map_restart` reconnect burst from re-sending every base string (D-006.12).
pub fn forget_view_overrides(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    state::with_state(|s| {
        let viewer = client_num as usize;
        for slot in s.tffa.cs_players_sent[viewer].iter_mut() {
            slot.clear();
        }
        s.tffa.view_bases_from_gamestate[viewer] = true;
        if s.clients[viewer].is_connected {
            s.tffa.pending_view_refresh[viewer] = true;
        }
    });
}

/// Drop **every** viewer's per-slot record and re-arm the full refresh for the
/// connected ones. Used when the master switch is turned back on: while the
/// proxy was disabled the game's own `CS_PLAYERS` broadcasts kept overwriting
/// the per-viewer overrides, so what each client holds no longer matches the
/// record.
pub fn forget_all_view_overrides() {
    state::with_state(|s| {
        for viewer in 0..MAX_CLIENTS {
            for slot in s.tffa.cs_players_sent[viewer].iter_mut() {
                slot.clear();
            }
            // The master switch may have been off without a view restore: an
            // override queued before the disable can still be what the client
            // holds, so the re-push compares against the (now empty) record and
            // sends every desired value instead of assuming base strings.
            s.tffa.view_bases_from_gamestate[viewer] = false;
            if s.clients[viewer].is_connected {
                s.tffa.pending_view_refresh[viewer] = true;
            }
        }
    });
}

/// Send one `CS_SCORES1/2` correction to a viewer and pin that half of the
/// dedup cache (the other half is untouched: it already matches the viewer's
/// per-match tally).
pub(crate) fn send_single_cs_score(viewer: c_int, index: i32, value: i32) {
    if !(0..MAX_CLIENTS as c_int).contains(&viewer) {
        return;
    }
    state::with_state(|s| {
        let entry = &mut s.tffa.cs_scores_sent[viewer as usize];
        if index == CS_SCORES1 {
            entry.0 = value;
        } else {
            entry.1 = value;
        }
    });
    send_cs_to_client(viewer, index, &value.to_string());
}

/// Live server configstring `index` (`None` when the server holds no string).
pub(crate) fn get_live_cs(index: i32) -> Option<String> {
    let mut buf = [0u8; 1024];
    // SAFETY: engine syscall registered; buffer writable.
    unsafe { crate::syscall::get_configstring(index, &mut buf) };
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    if n == 0 {
        return None;
    }
    String::from_utf8(buf[..n].to_vec()).ok()
}

/// Live server configstring for a player slot (fallback when the proxy base
/// is empty, e.g. the slot was assigned while the feature was off).
pub(crate) fn get_live_cs_players(slot: i32) -> Option<String> {
    get_live_cs(CS_PLAYERS + slot)
}

/// Push every current per-viewer override to one client (fresh connects get
/// the server gamestate with base strings, then these corrections), or repair
/// the ones the client is missing after a deferred send.
///
/// There are two very different callers, told apart by
/// `TffaState::view_bases_from_gamestate`:
///
/// - **the client just received a gamestate** (connect, `map_restart`
///   reconnect, resend): every `CS_PLAYERS` base string came in it, so the
///   stored bases may be recorded as held and only the playing outsiders'
///   demotions are owed (D-006.12: re-sending all bases is what overflowed the
///   reliable window during the reconnect burst);
/// - **any other full re-push** (a respawn/team-change `GAME_CLIENT_BEGIN`, or
///   the deferred work after reliable pressure): the client still holds
///   whatever the proxy last queued, which the per-viewer record mirrors. The
///   record must be trusted and every slot's **desired** value compared against
///   it. Recording a base the client never received would skip its push forever
///   and leave a `(TFFA N)` prefix + spectator team on a same-match player
///   (D-006.16).
pub fn send_all_overrides_to_client(client_num: c_int) {
    if !state::instances_enabled() {
        return;
    }
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let viewer_match = effective_match(client_num);
    let (red, blue) = view_scores(viewer_match);
    // Fresh connects bypass the dedup cache (their gameState came from the
    // server gamestate, not from our pushes).
    state::with_state(|s| {
        if (0..MAX_CLIENTS as c_int).contains(&client_num) {
            s.tffa.cs_scores_sent[client_num as usize] = (red, blue);
            s.tffa.viewer_match_cache[client_num as usize] = viewer_match;
        }
    });
    send_cs_to_client(client_num, CS_SCORES1, &red.to_string());
    send_cs_to_client(client_num, CS_SCORES2, &blue.to_string());
    let from_gamestate = state::with_state(|s| {
        core::mem::take(&mut s.tffa.view_bases_from_gamestate[client_num as usize])
    });
    let (table, mut bases): ([u8; MAX_CLIENTS], [String; MAX_CLIENTS]) =
        state::with_state(|s| (s.tffa.client_match, s.tffa.cs_players_base.clone()));
    for (slot, base) in bases.iter_mut().enumerate() {
        if base.is_empty() {
            if let Some(live) = get_live_cs_players(slot as i32) {
                *base = live.clone();
                state::with_state(|s| {
                    s.tffa.cs_players_base[slot] = live;
                });
            }
        }
    }
    // Only a gamestate delivered the server's base strings: drop this viewer's
    // record and record what it just received, so the demotions below are the
    // only sends. Any other re-push leaves the record alone; it is what the
    // viewer actually holds (see the doc comment).
    if from_gamestate {
        state::with_state(|s| {
            for slot in s.tffa.cs_players_sent[client_num as usize].iter_mut() {
                slot.clear();
            }
        });
        for (slot, base) in bases.iter().enumerate() {
            if !base.is_empty() {
                record_cs_players_sent(client_num, slot, base);
            }
        }
    }
    for (slot, value) in override_push_plan(from_gamestate, viewer_match, &table, &bases) {
        push_cs_players(client_num, slot as usize, &value);
    }
}

/// Full re-push candidate list for one viewer. A gamestate delivered the base
/// strings, so only the playing outsiders' demotions are owed; otherwise every
/// slot's desired value (base or demotion) is a candidate and
/// `push_cs_players`'s per-viewer record decides what is actually sent, which
/// re-queues a base whose earlier push was deferred. Pure for the tests.
pub(crate) fn override_push_plan(
    from_gamestate: bool,
    viewer_match: u8,
    table: &[u8; MAX_CLIENTS],
    bases: &[String; MAX_CLIENTS],
) -> Vec<(i32, String)> {
    if from_gamestate {
        demotions_for_viewer(viewer_match, table, bases)
    } else {
        desired_cs_players(viewer_match, table, bases)
    }
}

/// Refresh one viewer's `CS_PLAYERS` overrides for every slot (called when
/// that viewer changes matches, so stale prefixes/teams are corrected).
pub fn refresh_cs_players_for_viewer(client_num: c_int) {
    if !state::instances_enabled() {
        return;
    }
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let viewer_match = effective_match(client_num);
    let (table, bases): ([u8; MAX_CLIENTS], [String; MAX_CLIENTS]) =
        state::with_state(|s| (s.tffa.client_match, s.tffa.cs_players_base.clone()));
    for slot in 0..MAX_CLIENTS as i32 {
        let mut base = bases[slot as usize].clone();
        if base.is_empty() {
            if let Some(live) = get_live_cs_players(slot) {
                base = live.clone();
                state::with_state(|s| {
                    s.tffa.cs_players_base[slot as usize] = live;
                });
            } else {
                continue;
            }
        }
        let value = rewrite_cs_players_for_viewer(&base, table[slot as usize], viewer_match)
            .unwrap_or(base);
        push_cs_players(client_num, slot as usize, &value);
    }
    let (red, blue) = view_scores(viewer_match);
    // Viewer changed matches: force-push (bypass dedup, then re-arm it).
    state::with_state(|s| {
        if (0..MAX_CLIENTS as c_int).contains(&client_num) {
            s.tffa.cs_scores_sent[client_num as usize] = (red, blue);
            s.tffa.viewer_match_cache[client_num as usize] = viewer_match;
        }
    });
    send_cs_to_client(client_num, CS_SCORES1, &red.to_string());
    send_cs_to_client(client_num, CS_SCORES2, &blue.to_string());
}

/// The `CS_PLAYERS + slot` value `viewer` must hold for every slot with a known
/// base string: the per-viewer demotion for another match's playing player, the
/// base string for everyone else (own match, real spectators). Pure (built from
/// the match table and the stored bases), so the delivery loops stay plain send
/// loops and the rule is unit-tested without an engine.
///
/// The scoreboard refresh pushes every entry (not only the demotions): a slot
/// that moved into the viewer's match must get its base string back, and the
/// per-viewer dedup (`push_cs_players`) turns every unchanged entry into a
/// no-op, so a steady roster queues nothing at all.
pub(crate) fn desired_cs_players(
    viewer_match: u8,
    table: &[u8; MAX_CLIENTS],
    bases: &[String; MAX_CLIENTS],
) -> Vec<(i32, String)> {
    let mut out = Vec::new();
    for (slot, base) in bases.iter().enumerate() {
        if base.is_empty() {
            continue;
        }
        let value = rewrite_cs_players_for_viewer(base, table[slot], viewer_match)
            .unwrap_or_else(|| base.clone());
        out.push((slot as i32, value));
    }
    out
}

/// The `CS_PLAYERS` demotions one viewer's scoreboard needs, as
/// `(slot, rewritten info string)`: every playing outsider with a known base
/// string. Pure (match table + stored bases), so the delivery loop stays a
/// plain send loop and the rule is unit-tested without an engine.
pub(crate) fn demotions_for_viewer(
    viewer_match: u8,
    table: &[u8; MAX_CLIENTS],
    bases: &[String; MAX_CLIENTS],
) -> Vec<(i32, String)> {
    let mut out = Vec::new();
    for (slot, base) in bases.iter().enumerate() {
        if table[slot] == viewer_match || base.is_empty() {
            continue;
        }
        if let Some(modified) = rewrite_cs_players_for_viewer(base, table[slot], viewer_match) {
            out.push((slot as i32, modified));
        }
    }
    out
}

/// Clear accumulated stats for one slot (match moves/finish start the next
/// instance clean; cross-match damage is never recorded anyway, so only
/// same-instance history is dropped — for the mover as viewer and for others
/// as opponent — plus the team counters).
pub(crate) fn reset_slot_stats(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    state::with_state(|s| reset_client_stats(&mut s.clients, client_num));
    reset_game_scores(client_num);
}

/// The proxy-side part of a match move shared by `cmd_join`/`cmd_create`:
/// assign the client to `id` (both its runtime slot and its identity), clear
/// its per-match score copy and clear its `proxy_sv_enableEndGameStats`
/// row/column and team counters (see `reset_client_stats`). The game-side
/// `PERS_SCORE`/`PERS_KILLED` reset is separate (`reset_game_scores`, needs the
/// located entities). Pure so the join move is unit-tested.
pub(crate) fn assign_client_instance(
    client_match: &mut [u8; MAX_CLIENTS],
    home: &mut [u8; MAX_CLIENTS],
    player_score: &mut [i32; MAX_CLIENTS],
    clients: &mut [crate::state::ClientEntry],
    client_num: c_int,
    id: u8,
) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let n = client_num as usize;
    client_match[n] = id;
    home[n] = id;
    player_score[n] = 0;
    reset_client_stats(clients, client_num);
}

/// The end-game-stats half of `reset_slot_stats` (`proxy_sv_enableEndGameStats`):
/// clear the mover's whole row (what they did to others) and whole column
/// (what others did to them), and their team counters. This is what
/// `cmd_join`/`cmd_create`/`cmd_leave` run on an instance switch, so a player
/// carries no kills, deaths or damage from the match they just left into the
/// next match's final sheet. Pure over the client table for the join test.
pub(crate) fn reset_client_stats(clients: &mut [crate::state::ClientEntry], client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let n = client_num as usize;
    if n >= clients.len() {
        return;
    }
    clients[n].game_stats = [crate::state::GameStats::default(); MAX_CLIENTS];
    for client in clients.iter_mut().take(MAX_CLIENTS) {
        client.game_stats[n] = crate::state::GameStats::default();
    }
    clients[n].team_kills = 0;
    clients[n].team_killed = 0;
    clients[n].team_damages_given = 0;
    clients[n].team_damages_taken = 0;
}

/// Zero the game's own per-player score counters for one slot
/// (`ps.persistant[PERS_SCORE/PERS_KILLED]` accumulate all map, which would
/// otherwise trip pristine `CheckExitRules`' per-player fraglimit as soon as
/// any limit is set — and would show stale totals on the per-match HUD).
/// Mirrors the proxy per-match copies (reset alongside them on every move).
pub(crate) fn reset_game_scores(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
        return;
    }
    let ent = g_entities + client_num as usize * crate::sdk::SIZEOF_GENTITY;
    // SAFETY: ent is inside the g_entities array.
    let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
    if client == 0 {
        return;
    }
    // SAFETY: client came from a live gentity's `client` pointer.
    unsafe { reset_game_scores_at(client) };
}

/// Write zero to `ps.persistant[PERS_SCORE]` and `PERS_KILLED` in one
/// `gclient_t`. Split from `reset_game_scores` (which locates the client) so
/// the write can be tested on a scratch buffer without game memory.
///
/// # Safety
/// `client` must be 0 or point to a valid `gclient_t`.
pub(crate) unsafe fn reset_game_scores_at(client: usize) {
    use crate::sdk::{OFFSET_GCLIENT_PS, OFFSET_PS_PERSISTANT, PERS_KILLED, PERS_SCORE};
    if client == 0 {
        return;
    }
    // SAFETY: the caller guarantees a valid gclient_t; persistant slots are
    // plain ints.
    unsafe {
        core::ptr::write_unaligned(
            (client + OFFSET_GCLIENT_PS + OFFSET_PS_PERSISTANT + PERS_SCORE * 4) as *mut c_int,
            0,
        );
        core::ptr::write_unaligned(
            (client + OFFSET_GCLIENT_PS + OFFSET_PS_PERSISTANT + PERS_KILLED * 4) as *mut c_int,
            0,
        );
    }
}
