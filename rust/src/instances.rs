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
use core::sync::atomic::{AtomicU32, Ordering};

use crate::sdk::{
    CONTENTS_TRIGGER, CS_CLIENT_DUELHEALTHS, CS_CLIENT_DUELISTS, CS_PLAYERS, CS_SCORES1,
    CS_SCORES2, ENTITYNUM_MAX_NORMAL, ET_EVENTS, ET_MOVER, ET_NPC, G_LINKENTITY, MAX_CLIENTS,
    MAX_GENTITIES, MAX_NETNAME, OFFSET_GCLIENT_PS, OFFSET_GCLIENT_SESS, OFFSET_GENTITY_ACTIVATOR,
    OFFSET_GENTITY_BLOCKED, OFFSET_GENTITY_CLIENT, OFFSET_GENTITY_ENEMY, OFFSET_GENTITY_GHOUL2,
    OFFSET_GENTITY_INUSE, OFFSET_GENTITY_LAST_ENEMY, OFFSET_GENTITY_NEXT_TRAIN,
    OFFSET_GENTITY_PARENT, OFFSET_GENTITY_PREV_TRAIN, OFFSET_GENTITY_R_CONTENTS,
    OFFSET_GENTITY_R_LINKED, OFFSET_GENTITY_R_OWNER, OFFSET_GENTITY_REACHED,
    OFFSET_GENTITY_S_ETYPE, OFFSET_GENTITY_S_NUMBER, OFFSET_GENTITY_TARGET,
    OFFSET_GENTITY_TARGET_ENT, OFFSET_GENTITY_TARGETNAME, OFFSET_GENTITY_TEAM,
    OFFSET_GENTITY_TEAMCHAIN, OFFSET_GENTITY_TEAMMASTER, OFFSET_GENTITY_THINK,
    OFFSET_GENTITY_TOUCH, OFFSET_GENTITY_USE, OFFSET_PS_LEGS_FLIP, OFFSET_PS_TORSO_FLIP,
    OFFSET_SESS_SESSION_TEAM, SIZEOF_GENTITY, TEAM_BLUE, TEAM_RED, TEAM_SPECTATOR,
};
use crate::state;

/// Match id of the implicit main TFFA.
pub const MAIN_MATCH: u8 = 0;

/// Bumped whenever per-viewer `CS_PLAYERS`/HUD values are re-pushed (match
/// moves, duel labels, W/L). Demo recorders compare it to know when their
/// mirrored `cs` view must be recomputed; no state is duplicated across the
/// boundary.
static CS_GENERATION: AtomicU32 = AtomicU32::new(0);

/// Monotonic per-viewer configstring generation (see [`CS_GENERATION`]).
pub fn cs_generation() -> u32 {
    CS_GENERATION.load(Ordering::Relaxed)
}

pub(crate) fn bump_cs_generation() {
    CS_GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Maximum instance slots (ids `1..=MAX_MATCHES`). Raised from 8 to 16 by
/// D-025: the duel ladder may host up to 16 courts, while
/// `proxy_parallel_tffas_maxMatches` keeps its documented 8-match ceiling
/// (`tffa::TFFA_MATCH_CAP`).
pub const MAX_MATCHES: usize = 16;

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
    prefixed_name_with(&prefix_for(player_match), orig)
}

/// Prefix a display name with an already-built instance label, truncated so
/// the whole thing fits `MAX_NETNAME` (36 inc NUL → 35 bytes max).
pub(crate) fn prefixed_name_with(prefix: &str, orig: &str) -> String {
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

/// The label an outsider's name carries in a viewer's clientinfo: the TFFA
/// match (`(TFFA N) `) or, on the duel ladder, the court (`(Duel N) `).
fn instance_prefix(instance: u8) -> String {
    if crate::state::instance_mode() == crate::state::InstanceMode::Duel {
        return crate::duel::court_prefix(instance as usize + 1);
    }
    prefix_for(instance)
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
    let named = if state::instance_mode() == state::InstanceMode::Duel {
        prefixed_name_with(&instance_prefix(player_match), &orig)
    } else {
        prefixed_name(&orig, player_match)
    };
    let out = info_set(base, "n", &named);
    Some(info_set(&out, "t", &TEAM_SPECTATOR.to_string()))
}

/// Per-viewer `CS_PLAYERS` rewrite for one slot. TFFA demotes a playing
/// outsider to spectator and prefixes the match; the duel ladder keeps every
/// court player in the arena (the board lists the whole ladder in court
/// order), prefixes the court or bench label and injects the ladder W/L.
pub(crate) fn rewrite_cs_players_for_slot(
    base: &str,
    slot: usize,
    player_match: u8,
    viewer_match: u8,
) -> Option<String> {
    if state::instance_mode() == state::InstanceMode::Duel {
        return crate::duel::rewrite_cs_players(base, slot);
    }
    rewrite_cs_players_for_viewer(base, player_match, viewer_match)
}

/// Mark a client's `score`/`scoreboard` request (the TAB board) so the
/// `scores` trap knows the per-viewer `CS_PLAYERS` repair is owed. Called from
/// the `GAME_CLIENT_COMMAND` path before the game's own `Cmd_Score_f`.
pub fn note_client_command(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) || crate::original::base() == 0 {
        return;
    }
    let mut cmd = [0u8; 32];
    // SAFETY: engine syscall registered; buffer writable.
    unsafe { crate::syscall::argv(0, &mut cmd) };
    let end = cmd.iter().position(|&b| b == 0).unwrap_or(cmd.len());
    let name = &cmd[..end];
    if name.eq_ignore_ascii_case(b"score") || name.eq_ignore_ascii_case(b"scoreboard") {
        state::with_state(|s| s.tffa.score_requested[client_num as usize] = true);
    }
}

/// Consume a client's pending scoreboard request. `false` for a
/// server-initiated board, which must not pay for a CS repair burst.
pub(crate) fn take_score_request(client_num: c_int) -> bool {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return false;
    }
    state::with_state(|s| core::mem::take(&mut s.tffa.score_requested[client_num as usize]))
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

/// Send a `cs` correction now, bypassing the reliable-window deferral. Used
/// only by the teardown restore: the instance core is already off, so a
/// deferred `pending_view_refresh` would never run, and the caller has already
/// deduped the values it sends.
fn send_cs_now(client_num: c_int, index: i32, value: &str) -> bool {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
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
    let Ok(msg) = std::ffi::CString::new(format!("cs {index} \"{safe}\"\n")) else {
        return false;
    };
    // SAFETY: engine syscall registered; msg NUL-terminated.
    unsafe { crate::syscall::send_server_command(client_num, &msg) };
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
    if from_gamestate && state::instance_mode() != state::InstanceMode::Duel {
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
        let value =
            rewrite_cs_players_for_slot(&base, slot as usize, table[slot as usize], viewer_match)
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
    bump_cs_generation();
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
        let value = rewrite_cs_players_for_slot(base, slot, table[slot], viewer_match)
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
        if let Some(modified) = rewrite_cs_players_for_slot(base, slot, table[slot], viewer_match) {
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

/// Whether entity `ent_num` is *interactive* and so must be duplicated per
/// instance — the decided structural predicate
/// (`docs/decisions/0006-parallel-tffa.md`):
///
/// - `r.contents & CONTENTS_TRIGGER`, or
/// - `s.eType == ET_MOVER`, or
/// - any of `touch`/`use`/`think`/`reached`/`blocked` non-null, or
/// - `targetname`/`target`/`team` non-null, or
/// - taking part in a team chain (`teamchain`/`teammaster` non-null).
///
/// Everything else — spawn points without keys, `misc_model_static`, lights —
/// stays shared and inert.
pub fn is_interactive_entity(g_entities: usize, ent_num: c_int) -> bool {
    if g_entities == 0 || !(0..MAX_GENTITIES as c_int).contains(&ent_num) {
        return false;
    }
    let ent = g_entities + ent_num as usize * SIZEOF_GENTITY;
    // SAFETY: `ent` is inside the `g_entities` array; every field read is
    // either the int `r.contents`/`s.eType` or a pointer-sized field
    // (function pointers and `char*`), all valid for a spawned gentity.
    unsafe {
        if read_i32(ent + OFFSET_GENTITY_R_CONTENTS) & CONTENTS_TRIGGER != 0 {
            return true;
        }
        if read_i32(ent + OFFSET_GENTITY_S_ETYPE) == ET_MOVER {
            return true;
        }
        if [
            OFFSET_GENTITY_TOUCH,
            OFFSET_GENTITY_USE,
            OFFSET_GENTITY_THINK,
            OFFSET_GENTITY_REACHED,
            OFFSET_GENTITY_BLOCKED,
        ]
        .iter()
        .any(|&off| read_usize(ent + off) != 0)
        {
            return true;
        }
        [
            OFFSET_GENTITY_TARGETNAME,
            OFFSET_GENTITY_TARGET,
            OFFSET_GENTITY_TEAM,
            OFFSET_GENTITY_TEAMCHAIN,
            OFFSET_GENTITY_TEAMMASTER,
        ]
        .iter()
        .any(|&off| read_usize(ent + off) != 0)
    }
}

/// The live interactive entities in `[MAX_CLIENTS, ENTITYNUM_MAX_NORMAL)`, in
/// slot order — the set one further instance duplicates, and therefore the
/// gentity cost of that instance. Measured at `GAME_INIT`
/// (`scan_world_entities`), when no temp entity exists yet.
///
/// # Safety
///
/// Not `unsafe` in Rust's sense, but `g_entities` must be the live array the
/// game located (`G_LOCATE_GAME_DATA`) or `0`: every element read is bounded by
/// `MAX_GENTITIES` and every field read is valid for a spawned gentity.
pub fn interactive_sources(g_entities: usize) -> Vec<c_int> {
    if g_entities == 0 {
        return Vec::new();
    }
    let mut sources = Vec::new();
    for num in MAX_CLIENTS as c_int..ENTITYNUM_MAX_NORMAL {
        let ent = g_entities + num as usize * SIZEOF_GENTITY;
        // SAFETY: `num` is a valid gentity slot.
        let live = unsafe { read_i32(ent + OFFSET_GENTITY_INUSE) } != 0;
        if live && is_interactive_entity(g_entities, num) {
            sources.push(num);
        }
    }
    sources
}

/// Entity slots `G_Spawn` could still hand out right now (free, inside the
/// ordinary range): the capacity check for one further clone set. Slots below
/// `MAX_CLIENTS` are the engine's player slots.
pub fn free_world_slots(g_entities: usize) -> usize {
    if g_entities == 0 {
        return 0;
    }
    (MAX_CLIENTS as i32..ENTITYNUM_MAX_NORMAL)
        .filter(|&num| {
            let ent = g_entities + num as usize * SIZEOF_GENTITY;
            // SAFETY: `num` is a valid gentity slot.
            unsafe { read_i32(ent + OFFSET_GENTITY_INUSE) == 0 }
        })
        .count()
}

/// Whether a naive `memcpy` duplicate of this interactive entity is sound.
///
/// An NPC/actor owns a server-side Ghoul2 model instance and a dynamically
/// allocated `client`; a copy would animate through the *same* body and
/// `G_FreeEntity` would clean the model out from under the other
/// (`g_utils.c:945-1013`). A map holding one takes no instance at all —
/// all-or-nothing, D-006.6.
pub(crate) fn is_duplicable_entity(g_entities: usize, ent_num: c_int) -> bool {
    let ent = g_entities + ent_num as usize * SIZEOF_GENTITY;
    // SAFETY: `ent_num` is a valid gentity slot.
    unsafe {
        read_i32(ent + OFFSET_GENTITY_S_ETYPE) != ET_NPC
            && read_usize(ent + OFFSET_GENTITY_CLIENT) == 0
            && read_usize(ent + OFFSET_GENTITY_GHOUL2) == 0
    }
}

/// `GAME_INIT` post-pass: measure the map's interactive-entity set — one
/// further instance's clone cost — and how many further instances the entity
/// budget can take, once the pristine `GAME_INIT` returned (map spawned,
/// `g_entities` located). The interactive entities are tagged as belonging to
/// main (`MAIN_MATCH`), which is inert while every viewer is in main and makes
/// the set the clone pass reads. Called lazily from `cmd_create` too, so a
/// server that enables `proxy_parallel_tffas_enable` mid-map is still measured
/// before its first instance.
pub fn scan_world_entities() {
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    let sources = interactive_sources(g_entities);
    let cost = sources.len();
    let blocked = sources
        .iter()
        .any(|&num| !is_duplicable_entity(g_entities, num));
    let slots = free_world_slots(g_entities);
    state::with_state(|s| {
        // A map change can leave the previous map's tags and clone bookkeeping
        // behind (only a full reload rebuilds the state); start clean.
        s.tffa.entity_instance = [ENTITY_SHARED; MAX_GENTITIES];
        // A map change must not inherit a chain scope from the map before it,
        // nor spawns of the map before it.
        s.tffa.acting_scope = SCOPE_NONE;
        s.tffa.spawn_pending.clear();
        s.tffa.world_clone_cost = cost;
        s.tffa.world_clone_blocked = blocked;
        s.tffa.world_release = 0;
        s.tffa.world_cloned = 0;
        s.tffa.world_scanned = true;
        if !blocked {
            for &num in &sources {
                s.tffa.entity_instance[num as usize] = MAIN_MATCH;
            }
        }
    });
    // A map with nothing interactive never limits the number of matches.
    let fits = if cost == 0 { MAX_MATCHES } else { slots / cost };
    eprintln!(
        "----- proxy-rs: {cost} interactive map entities, {slots} free entity slots — room for {fits} further TFFA instance(s)"
    );
    if blocked {
        eprintln!(
            "----- proxy-rs: an interactive entity on this map cannot be duplicated (NPC/actor) — parallel TFFA instances are unavailable on it"
        );
    }
}

/// Measure the map's interactive set if that has not happened yet. Returns
/// whether the world is measured (and so whether an instance may be created).
pub(crate) fn ensure_world_scanned() -> bool {
    if state::with_state(|s| s.tffa.world_scanned) {
        return true;
    }
    if !state::instances_enabled() {
        return false;
    }
    scan_world_entities();
    state::with_state(|s| s.tffa.world_scanned)
}

/// Claim a slot's per-instance world before (re)using it. A closed match
/// keeps its clone set until the next frame start; if the slot is re-created
/// before that release runs, the pending release must be cancelled — otherwise
/// it would free the clones the new match is using. Returns whether the set is
/// already live (reuse) and clears any queued release for the slot either way
/// (a slot with no set has nothing to release).
pub(crate) fn claim_world(pending: &mut u32, cloned: u32, instance: u8) -> bool {
    let bit = 1u32 << instance;
    *pending &= !bit;
    cloned & bit != 0
}

/// Duplicate the map's whole interactive set for `instance`, or report why this
/// instance cannot exist. All-or-nothing: a partial set would leave some
/// doors/triggers shared, activatable from every instance.
///
/// The clones are created with the game's own `G_Spawn` (so slot accounting
/// matches the pristine game), copy the *live* source entity at creation time,
/// have their in-set entity links re-pointed at their own copies, and are
/// linked into the engine world through `G_LINKENTITY`. `s.number` is restored
/// to the clone's own slot: the engine addresses snapshots by it.
pub fn realize_instance(instance: u8) -> Result<(), &'static str> {
    if instance == MAIN_MATCH {
        // Main is the pristine map entity set.
        return Ok(());
    }
    if instance as usize >= TOTAL_MATCH_SLOTS {
        return Err("not a match slot");
    }
    let (g_entities, blocked, done, cost) = state::with_state(|s| {
        let done = claim_world(&mut s.tffa.world_release, s.tffa.world_cloned, instance);
        (
            s.located_game_data.g_entities,
            s.tffa.world_clone_blocked,
            done,
            s.tffa.world_clone_cost,
        )
    });
    let bit = 1u32 << instance;
    if done {
        // The set survived the instance that used it (a closed match keeps its
        // world until the next map): reuse it.
        return Ok(());
    }
    if !ensure_world_scanned() {
        return Err("this map has not been measured for per-instance worlds");
    }
    if blocked {
        return Err("an interactive entity here cannot be duplicated (NPC/actor)");
    }
    if g_entities == 0 {
        return Err("the game module has not located its entities yet");
    }
    // The set is exactly the entities tagged as main's. Reading it from the
    // tag table (not the `GAME_INIT` count) keeps the pass correct when a
    // source was destroyed during the map.
    let tags: [u8; MAX_GENTITIES] = state::with_state(|s| s.tffa.entity_instance);
    let needed = tags[..ENTITYNUM_MAX_NORMAL as usize]
        .iter()
        .filter(|t| **t == MAIN_MATCH)
        .count();
    // `G_Spawn` hands out `free_world_slots` further ordinary slots before it
    // `G_Error`s, and the pass runs synchronously (no temp entity appears
    // mid-pass), so the whole set fits whenever the count does.
    if free_world_slots(g_entities) < needed {
        return Err("this map has no entity capacity left for another full instance");
    }

    // Pass 1: spawn a clone per source and take the source's live state.
    let mut clone_of = [u16::MAX; MAX_GENTITIES];
    let mut made: Vec<u16> = Vec::with_capacity(needed);
    for src in MAX_CLIENTS as c_int..ENTITYNUM_MAX_NORMAL {
        if tags[src as usize] != MAIN_MATCH {
            continue;
        }
        // SAFETY: the game module is loaded and running; `G_Spawn` allocates
        // from the ordinary range the capacity check just showed is free.
        let clone = unsafe { crate::jampgame::g_spawn() };
        if clone == 0 {
            break;
        }
        let Some(clone_num) = ent_num_of(g_entities, clone) else {
            break;
        };
        let clone_num = clone_num as u16;
        // SAFETY: `src` and `clone` are live gentity slots inside the same
        // `g_entities` array; the copy is the plain struct duplicate this
        // mechanism is defined as.
        unsafe {
            core::ptr::copy_nonoverlapping(
                (g_entities + src as usize * SIZEOF_GENTITY) as *const u8,
                clone as *mut u8,
                SIZEOF_GENTITY,
            );
            // `G_InitGentity` set the clone's own number; the copy replaced it
            // with the source's. The engine routes snapshots by `s.number`.
            write_i32(clone + OFFSET_GENTITY_S_NUMBER, clone_num as i32);
        }
        clone_of[src as usize] = clone_num;
        made.push(clone_num);
    }
    if made.len() != needed {
        // Not reachable through the capacity check above; unwind rather than
        // leave a partial set (it would be activatable from every instance).
        for &num in &made {
            // SAFETY: the slot is a clone this pass just spawned.
            unsafe {
                crate::jampgame::g_free_entity(g_entities + num as usize * SIZEOF_GENTITY);
            }
        }
        return Err("the map ran out of entity slots while duplicating its world");
    }

    // Pass 2: re-point in-set links at the copies, then link each clone into
    // the engine world so it collides and renders.
    for &clone_num in &made {
        let clone = g_entities + clone_num as usize * SIZEOF_GENTITY;
        for off in CLONE_LINK_FIELDS {
            // SAFETY: `clone` is a live gentity; `off` is one of its pointer
            // fields.
            let target = unsafe { read_usize(clone + off) };
            if target == 0 {
                continue;
            }
            let Some(src_num) = ent_num_of(g_entities, target) else {
                continue;
            };
            if tags[src_num as usize] != MAIN_MATCH {
                continue; // a player, or an inert shared prop
            }
            let mapped = clone_of[src_num as usize];
            if mapped != u16::MAX {
                let mapped = g_entities + mapped as usize * SIZEOF_GENTITY;
                // SAFETY: the clone field may be written; `mapped` is a live
                // gentity of this same set.
                unsafe { write_usize(clone + off, mapped) };
            }
        }
        // SAFETY: engine syscall registered; `clone` is a gentity slot. The
        // engine links by entity number, which the copy already carries.
        unsafe {
            crate::syscall::call_engine(G_LINKENTITY, &[clone as c_int]);
        }
    }
    state::with_state(|s| {
        for &num in &made {
            s.tffa.entity_instance[num as usize] = instance;
        }
        s.tffa.world_cloned |= bit;
    });
    eprintln!(
        "----- proxy-rs: instance {instance} world: duplicated {} of the {cost} interactive map entities measured at GAME_INIT",
        made.len()
    );
    Ok(())
}

/// Free one instance's clone set and clear its tags, so a later match in the
/// same slot starts from a fresh world and the capacity comes back.
pub(crate) fn release_instance(instance: u8) {
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    let victims: Vec<u16> = state::with_state(|s| {
        if g_entities == 0 {
            return Vec::new();
        }
        (MAX_CLIENTS..ENTITYNUM_MAX_NORMAL as usize)
            .filter(|n| s.tffa.entity_instance[*n] == instance)
            .map(|n| n as u16)
            .collect()
    });
    for num in victims {
        let ent = g_entities + num as usize * SIZEOF_GENTITY;
        // SAFETY: the slot is a clone this proxy spawned; frame start is
        // between game frames, so nothing is iterating `g_entities`.
        unsafe { crate::jampgame::g_free_entity(ent) };
        state::with_state(|s| s.tffa.entity_instance[num as usize] = ENTITY_SHARED);
    }
    state::with_state(|s| s.tffa.world_cloned &= !(1u32 << instance));
}

/// Drain the instance releases queued since the last frame.
pub fn drain_world_releases() {
    let pending = state::with_state(|s| core::mem::take(&mut s.tffa.world_release));
    if pending == 0 {
        return;
    }
    for instance in 1..TOTAL_MATCH_SLOTS {
        if pending & (1u32 << instance) != 0 {
            release_instance(instance as u8);
        }
    }
}

/// `G_FreeEntity` detour body: an entity that actually left the world must lose
/// its instance tag with its slot. `G_Spawn` hands freed slots to later
/// entities (temp entities included), and a recycled slot that kept a stale tag
/// would hide its new occupant from every other instance. A `neverFree` entity
/// is unlinked but stays in use (`g_utils.c:945`): its tag stays with it.
pub fn note_entity_freed(ed: usize) {
    if ed == 0 {
        return;
    }
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    let Some(num) = ent_num_of(g_entities, ed) else {
        return;
    };
    // SAFETY: `ed` is a gentity the game just handed to `G_FreeEntity`.
    if unsafe { read_i32(ed + OFFSET_GENTITY_INUSE) } != 0 {
        return;
    }
    state::with_state(|s| {
        if s.tffa.entity_instance[num as usize] != ENTITY_SHARED {
            s.tffa.entity_instance[num as usize] = ENTITY_SHARED;
        }
    });
}

/// How one temp-event number reaches the clients' speakers, and therefore how
/// the snapshot filter must attribute it to a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundAttribution {
    /// Not a sound event (falls through to the generic owner attribution).
    None,
    /// `G_Sound` sound: the creating entity's match was stamped into
    /// `TffaState::sound_source` at construction.
    Stamped,
    /// `G_EntitySound`/`G_SoundOnEnt` and the player respawn/teleport temp
    /// entities (`EV_PLAYER_TELEPORT_IN`/`OUT`): the emitting player rides
    /// `s.clientNum`.
    Emitter,
    /// Deliberate broadcast (`EV_GLOBAL_SOUND`, `EV_GLOBAL_TEAM_SOUND`,
    /// `EV_MUTE_SOUND`): head-relative map/CTF audio, or a mute that is a
    /// no-op for matches that never received the sound.
    Broadcast,
}

/// How a (non-sound) temp event names the match it belongs to in its
/// `entityState_t`, and therefore which fields the snapshot filter may read.
///
/// The game builds temp entities with `G_TempEntity`, which zeroes only what
/// the caller sets: `G_InitGentity` resets `r.ownerNum` but not `s.*`
/// (`g_utils.c:694-700`, and the freed slot was memset, `g_utils.c:1100`), so
/// every field the event does not use keeps the bare `0` default. That `0`
/// coincides with client 0, so a blanket "read both `s.otherEntityNum*`" rule
/// dropped every such effect from every match that was not client 0's
/// (D-006, "World-entity visibility fix" residual). This classification is
/// source-derived from every `G_TempEntity` call site in `codemp/game`; see
/// the per-event notes below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventOwners {
    /// The event carries no player reference at all: a map/world effect
    /// (impact sparks, glass shards, screen shakes, broadcasts). Kept for
    /// every viewer; the engine's PVS still culls it by position.
    World,
    /// `s.otherEntityNum` always names the affected player; a `0` there is
    /// client 0.
    Victim,
    /// `s.otherEntityNum` names the affected player only on some hits; a `0`
    /// there is the untouched default ("no target"), not client 0.
    OptionalVictim,
    /// `s.otherEntityNum2` names the other player only in some clash paths; a
    /// `0` there is the untouched default, not client 0. When the constructing
    /// scope was open, the stamp recorded by `stamp_temp_source_event` is used
    /// instead (it also resolves the real client 0).
    OptionalAttacker,
    /// `s.otherEntityNum` (victim) and `s.otherEntityNum2` (attacker/killer),
    /// both always set.
    VictimAndAttacker,
    /// `s.owner` always names the affected player; a `0` there is client 0.
    SOwner,
}

/// Classification of a temp event (`event = eType - ET_EVENTS`).
pub fn sound_attribution(event: c_int) -> SoundAttribution {
    use crate::sdk::{
        EV_ENTITY_SOUND, EV_GENERAL_SOUND, EV_GLOBAL_SOUND, EV_GLOBAL_TEAM_SOUND, EV_MUTE_SOUND,
        EV_PLAYER_TELEPORT_IN, EV_PLAYER_TELEPORT_OUT, EV_PREDEFSOUND,
    };
    match event {
        EV_GENERAL_SOUND => SoundAttribution::Stamped,
        // `G_PreDefSound` (`w_force.c:40-49`) builds its event with plain
        // `G_TempEntity`, so it has no owner field: attribute it at
        // construction from the acting scope. The force-jump sound is built by
        // `WP_ForcePowersUpdate`, which `G_RunFrame` runs on the player's
        // behalf (`g_main.c:3949`) and whose wrapper opens the player's
        // instance; the think (`ClientThink_real`) covers the rest. Without
        // this it fell through to the generic owner-fields rule, whose memset
        // zeros resolve to client 0, and the sound was silenced in every other
        // match.
        EV_PREDEFSOUND => SoundAttribution::Stamped,
        // `G_EntitySound`/`G_SoundOnEnt` (`g_utils.c:1392-1408`) and the
        // respawn/teleport temp entities all put the player in `s.clientNum`.
        // The teleport pair carries no `s.owner`/`s.otherEntityNum*`
        // (`G_TempEntity` leaves them at the freed-slot memset zero), so
        // without this they were attributed to client 0 and every instance
        // heard the other instances' respawn `telein` sounds. `s.clientNum`
        // is synced by `ClientSpawn` (`g_client.c:3788`) before `respawn()`
        // builds the event, and set explicitly by every other call site.
        EV_ENTITY_SOUND | EV_PLAYER_TELEPORT_IN | EV_PLAYER_TELEPORT_OUT => {
            SoundAttribution::Emitter
        }
        EV_GLOBAL_SOUND | EV_GLOBAL_TEAM_SOUND | EV_MUTE_SOUND => SoundAttribution::Broadcast,
        _ => SoundAttribution::None,
    }
}

/// Pure snapshot gate for an attributed sound: world sounds are heard by
/// every match, attributed ones only by the source's own match.
pub fn sound_visible(viewer_match: u8, source_match: u8) -> bool {
    source_match == SOURCE_WORLD || entity_visible(viewer_match, source_match)
}

/// Classification of a temp event (`event = eType - ET_EVENTS`) from its
/// creators in `codemp/game`. Only events that must stay match-scoped are
/// listed; everything else is a world effect.
pub fn event_owners(event: c_int) -> EventOwners {
    use crate::sdk::{
        EV_CONC_ALT_IMPACT, EV_DEBRIS, EV_DISRUPTOR_SNIPER_SHOT, EV_FORCE_DRAINED, EV_GHOUL2_MARK,
        EV_GLOBAL_DUEL, EV_LOCALTIMER, EV_OBITUARY, EV_SABER_BLOCK, EV_SABER_HIT, EV_SCOREPLUM,
        EV_SHIELD_HIT, EV_SIEGESPEC, EV_VEH_FIRE,
    };
    match event {
        // `g_combat.c:5624-5628` and `g_missile.c:914-917`: the entity the
        // projectile mark is placed on.
        EV_GHOUL2_MARK => EventOwners::Victim,
        // `g_main.c:3101-3105,3205-3209`: the two duelists.
        EV_GLOBAL_DUEL => EventOwners::VictimAndAttacker,
        // `g_weapon.c:3811-3815`: the firing vehicle (`ent->s.number`).
        EV_VEH_FIRE => EventOwners::SOwner,
        // `w_saber.c:3508-3512,5571-5573,5652-5655` (victim + saber owner,
        // the latter `ENTITYNUM_NONE` for thrown-saber damage) and
        // `g_combat.c:3246-3248` (victim + `ENTITYNUM_NONE`).
        EV_SABER_HIT => EventOwners::VictimAndAttacker,
        // `WP_SaberDoClash` (`w_saber.c:3569-3573`) sets only
        // `s.otherEntityNum2`, but the projectile-deflect paths
        // (`g_missile.c:338,507,582`, `g_weapon.c:559`, `w_saber.c:5516,8317`)
        // set neither field: there the untouched `0` is not client 0.
        EV_SABER_BLOCK => EventOwners::OptionalAttacker,
        // `g_weapon.c:757` (no target) and `:776-786` (target only when the
        // shot hits a client): the beam itself is a world effect.
        EV_DISRUPTOR_SNIPER_SHOT => EventOwners::OptionalVictim,
        // `g_items.c:1405-1408`: `s.owner = ps.clientNum` (thrown-item timer).
        EV_LOCALTIMER => EventOwners::SOwner,
        // `g_mover.c:2370-2374`: `s.owner` is the exploding entity, a client
        // only when the breakable belongs to one.
        EV_DEBRIS => EventOwners::SOwner,
        // `g_weapon.c:3165-3168`: `s.owner = ent->s.number` (the shooter).
        EV_CONC_ALT_IMPACT => EventOwners::SOwner,
        // `g_combat.c:2468-2472`: `s.otherEntityNum` = victim,
        // `s.otherEntityNum2` = killer.
        EV_OBITUARY => EventOwners::VictimAndAttacker,
        // `w_force.c:2179-2181`: `s.owner = traceEnt->s.number` (the victim).
        EV_FORCE_DRAINED => EventOwners::SOwner,
        // `g_combat.c:419-424`: `s.otherEntityNum = ent->s.number` (the player
        // the score plum belongs to); the bot-waypoint preview
        // (`ai_wpnav.c:258,339`) sets none, but is a development broadcast.
        EV_SCOREPLUM => EventOwners::Victim,
        // `g_combat.c:5257-5259`: `s.otherEntityNum = targ->s.number` (the
        // shielded victim); `s.otherEntityNum2` is never set.
        EV_SHIELD_HIT => EventOwners::Victim,
        // `g_client.c:1117-1119`: `s.owner = ent->s.number` (the player).
        EV_SIEGESPEC => EventOwners::SOwner,
        // Everything else: `EV_CLIENTJOIN`, `EV_GIVE_NEW_RANK`,
        // `EV_SET_FREE_SABER`/`EV_SET_FORCE_DISABLE`, `EV_TEAM_POWER`,
        // `EV_SCREENSHAKE`, `EV_GLASS_SHATTER`, `EV_MISC_MODEL_EXP`, the
        // weapon-impact events (`EV_MISSILE_MISS`, `EV_DISRUPTOR_*_MISS/_HIT`,
        // ...), the `EV_SABER_CLASHFLARE` hit point, and the deliberate
        // broadcasts already handled by `sound_attribution` are owner-less
        // world events. (`EV_PLAY_EFFECT*` is also owner-less, but it is
        // attributed by its construction stamp first — see
        // `event_uses_construction_source`; this arm is its fallback for an
        // effect built with no scope open.)
        _ => EventOwners::World,
    }
}

/// gentity slot of `ent` (`ent - g_entities` in `SIZEOF_GENTITY` steps), or
/// `None` when the pointer is not a slot of the `g_entities` array.
pub(crate) fn ent_num_of(g_entities: usize, ent: usize) -> Option<c_int> {
    if g_entities == 0 || ent < g_entities {
        return None;
    }
    let off = ent - g_entities;
    if off % crate::sdk::SIZEOF_GENTITY != 0 {
        return None;
    }
    let num = off / crate::sdk::SIZEOF_GENTITY;
    if num >= MAX_GENTITIES {
        return None;
    }
    Some(num as c_int)
}

/// Match that owns entity `ent`: a player slot directly, or one `r.ownerNum`
/// hop to its player (missiles, saber entities) — the same resolution the
/// trace isolation uses (`request_match`), taken while the entity is still
/// live. `SOURCE_WORLD` when no live player resolves (world/map/NPC).
pub fn source_match(g_entities: usize, ent: usize, table: &[u8; MAX_CLIENTS]) -> u8 {
    match ent_num_of(g_entities, ent) {
        Some(num) => source_match_of_num(g_entities, num, table),
        None => SOURCE_WORLD,
    }
}

/// `source_match` for an already-known gentity slot.
pub fn source_match_of_num(g_entities: usize, ent_num: c_int, table: &[u8; MAX_CLIENTS]) -> u8 {
    request_match(g_entities, ent_num, table).unwrap_or(SOURCE_WORLD)
}

/// `G_Sound` entry wrapper: resolve which match's action owns the temp entity
/// the pristine body is about to construct and stash it for the constructor
/// handshake. Returns the previous pending value for `end_sound_source` to
/// restore (nested sound creation must not steal the outer attribution).
pub fn begin_sound_source(ent: usize) -> u8 {
    state::with_state(|s| {
        let source = source_match(s.located_game_data.g_entities, ent, &s.tffa.client_match);
        let previous = s.tffa.pending_sound_source;
        s.tffa.pending_sound_source = source;
        previous
    })
}

/// Restore the pending slot saved by `begin_sound_source`.
pub fn end_sound_source(previous: u8) {
    state::with_state(|s| s.tffa.pending_sound_source = previous);
}

/// Temp-entity constructor wrapper (`G_TempEntity`/`G_SoundTempEntity`):
/// attribute the fresh slot to the pending source and consume it. Every
/// construction stamps, so a slot reuse never keeps a stale attribution.
pub fn stamp_temp_source(te: usize) {
    stamp_temp_source_event(te, -1);
}

/// `G_TempEntity` wrapper's stamping rule: a pending `G_Sound` handshake wins;
/// otherwise an event that carries no reliable owner of its own takes the open
/// acting scope — the owner-less `EV_PREDEFSOUND` force sounds (force
/// jump/protect/absorb), the clash-family events and the positional
/// `EV_PLAY_EFFECT*` effects (see `event_uses_construction_source`) — and every
/// other direct `G_TempEntity` call stays a world/map sound. Consumes the
/// pending source in every case.
pub fn stamp_temp_source_event(te: usize, event: c_int) {
    state::with_state(|s| {
        let Some(num) = ent_num_of(s.located_game_data.g_entities, te) else {
            return;
        };
        let source = temp_stamp_source(s.tffa.pending_sound_source, event, s.tffa.acting_scope);
        s.tffa.pending_sound_source = SOURCE_WORLD;
        s.tffa.sound_source[num as usize] = source;
    });
}

/// Temp events whose match must be read from the construction-time stamp
/// (the open acting scope) rather than from their own entity fields.
///
/// `EV_PREDEFSOUND` is a sound built by `G_PreDefSound` with a plain
/// `G_TempEntity`: no owner field at all.
///
/// `EV_SABER_BLOCK` and `EV_SABER_CLASHFLARE` are the saber clash effects the
/// client renders and plays (`cg_event.c:2318-2426`: block sound, block effect,
/// screen flash; `saberhitwall` sound on the flare). Most creators leave every
/// owner field at the freed slot's memset zero — the saber-lock tick
/// (`w_saber.c` `8300+`), a thrown saber blocked by a player (`:5516`),
/// projectile/disruptor deflects (`g_missile.c:338,507,582`,
/// `g_weapon.c:559,762`) and the wall-hit flare (`w_saber.c:3548,5674`) — and
/// `WP_SaberDoClash`'s `s.otherEntityNum2 = self->s.number` is ambiguous with
/// that zero when the attacker is client 0. The stamp, recorded while the
/// creating player's acting scope is open, names the instance in every one of
/// those paths.
///
/// `EV_PLAY_EFFECT`/`EV_PLAY_EFFECT_ID`/`EV_PLAY_PORTAL_EFFECT_ID` are the
/// positional effects `G_PlayEffect`/`G_PlayEffectID` build (`g_utils.c:1250-1284`).
/// They carry the effect id in `s.eventParm` and no owner field either. The
/// reported bug — *"when one in another instance jumps on sand, the landing
/// effect of his landing is in all instances"* — is `PM_CrashLandEffect`
/// (`bg_pmove.c:3684-3724`, reached from `Pmove` inside `ClientThink_real`)
/// calling `G_PlayEffect(EFFECT_LANDING_SAND, …)`; the water/lava/acid splashes
/// (`PM_WaterEvents`, `bg_pmove.c:5671-5734`) and the other creators (turret,
/// mine and detpack explosions, sparks, debris) have the same shape. The stamp
/// recorded while the pmove/think/missile scope is open attributes the effect to
/// the acting player's (or clone's) instance; an effect built with no scope open
/// stays a world effect, visible to every instance.
pub fn event_uses_construction_source(event: c_int) -> bool {
    use crate::sdk::{
        EV_PLAY_EFFECT, EV_PLAY_EFFECT_ID, EV_PLAY_PORTAL_EFFECT_ID, EV_PREDEFSOUND,
        EV_SABER_BLOCK, EV_SABER_CLASHFLARE,
    };
    matches!(
        event,
        EV_PREDEFSOUND
            | EV_SABER_BLOCK
            | EV_SABER_CLASHFLARE
            | EV_PLAY_EFFECT
            | EV_PLAY_EFFECT_ID
            | EV_PLAY_PORTAL_EFFECT_ID
    )
}

/// Pure stamping rule (unit-tested): the `G_Sound` handshake beats everything;
/// an event in `event_uses_construction_source` takes the open acting scope;
/// with no scope it stays a world event (audible/visible everywhere).
pub fn temp_stamp_source(pending: u8, event: c_int, scope: u8) -> u8 {
    if pending != SOURCE_WORLD {
        return pending;
    }
    if scope != SCOPE_NONE && event_uses_construction_source(event) {
        return scope;
    }
    SOURCE_WORLD
}

/// Build the flip list for one trace: the `r.contents` slot of every entity the
/// requester must not collide with, paired with the saved value for the
/// restore. `out` ends up holding exactly this trace's entries.
///
/// The engine's `SV_ClipMoveToEntities` (`sv_world.cpp:637`) skips entities
/// whose `r.contents` has no bit in the trace's `contentmask`, so zeroing
/// `contents` for the duration of this single synchronous trap makes
/// cross-match entities intangible game-side (no body blocking, no saber
/// clashes, no foreign door in the way) without touching movement code.
///
/// Call only while the feature is on: the `G_TRACE` arm tests `enabled()`
/// before taking the state lock, so this never runs on a server that has not
/// turned parallel TFFA on.
pub fn trace_flips(
    pass_entity_num: c_int,
    contentmask: c_int,
    out: &mut Vec<(usize, i32)>,
) -> usize {
    out.clear();
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
        return 0;
    }
    state::with_state(|s| {
        // Rebuild the owned-entity cache once per game frame.
        if s.tffa.rebuilt_stamp != s.tffa.frame_stamp {
            rebuild_owned(s, g_entities);
            s.tffa.rebuilt_stamp = s.tffa.frame_stamp;
        }
        flip_list(&s.tffa, g_entities, pass_entity_num, contentmask, out)
    })
}

/// The flip selection proper — no state access, so it can be unit-tested with a
/// fabricated `g_entities` array and table. `out` is cleared first. Three groups
/// are collected, each only when its `r.contents` intersects the trace's
/// `contentmask`:
///
/// - players of other matches, and their owned entities (sabers, missiles,
///   corpses): cross-match body blocking and saber clashes;
/// - per-instance world entities (doors, plats, elevators, items) tagged with
///   an instance other than the requester's: each instance collides with its
///   own copies, and with nobody else's. Main's originals (tag `MAIN_MATCH`)
///   are foreign to every created instance.
///
/// A requester with no match (a world trace, `ENTITYNUM_NONE`) flips nothing:
/// the world stays solid for it, exactly as before this feature.
pub(crate) fn flip_list(
    t: &crate::state::TffaState,
    g_entities: usize,
    pass_entity_num: c_int,
    contentmask: c_int,
    out: &mut Vec<(usize, i32)>,
) -> usize {
    out.clear();
    let table = &t.client_match;
    // The trace's requester: a player slot, or (missiles/sabers/other ents) the
    // `r.ownerNum` player. Unknown/world requesters keep the world solid
    // everywhere (no filtering).
    let Some(requester) = request_match(g_entities, pass_entity_num, table) else {
        return 0;
    };
    collect_flips(t, g_entities, requester, contentmask, out)
}

/// The flip selection for a *known* requester instance: every entity whose
/// contents intersect `contentmask` and which the requester must not collide
/// with or touch. Shared by the trace gate (`flip_list`) and the touch gate
/// (`touch_flips`), so both see exactly the same per-instance world.
pub(crate) fn collect_flips(
    t: &crate::state::TffaState,
    g_entities: usize,
    requester: u8,
    contentmask: c_int,
    out: &mut Vec<(usize, i32)>,
) -> usize {
    let table = &t.client_match;
    let mut flip = |addr: usize| {
        // SAFETY: `addr` is a `r.contents` slot inside the game's gentity array
        // (validated by the caller's construction).
        let contents = unsafe { read_i32(addr) };
        if contents != 0 && (contents & contentmask) != 0 {
            out.push((addr, contents));
        }
    };
    // Player entities.
    for cn in 0..MAX_CLIENTS as i32 {
        if table[cn as usize] == requester {
            continue;
        }
        let ent = g_entities + cn as usize * SIZEOF_GENTITY;
        // SAFETY: ent is inside the g_entities array.
        if unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) } == 0 {
            continue;
        }
        flip(ent + OFFSET_GENTITY_R_CONTENTS);
    }
    // Player-owned entities (saber ents, missiles, corpses).
    for k in 0..t.owned_count {
        let (num, owner) = t.owned[k];
        if table[owner as usize] == requester {
            continue;
        }
        flip(g_entities + num as usize * SIZEOF_GENTITY + OFFSET_GENTITY_R_CONTENTS);
    }
    // Per-instance world entities of every other instance. Only the ordinary
    // range is ever tagged (`scan_world_entities`/`realize_instance`), so the
    // walk can stop there.
    for (num, &tag) in t
        .entity_instance
        .iter()
        .enumerate()
        .take(ENTITYNUM_MAX_NORMAL as usize)
        .skip(MAX_CLIENTS)
    {
        if tag == ENTITY_SHARED || tag == requester {
            continue;
        }
        flip(g_entities + num * SIZEOF_GENTITY + OFFSET_GENTITY_R_CONTENTS);
    }
    out.len()
}

/// Zero `r.contents` on every collected flip slot. The caller restores them
/// once the window (the synchronous trap, or the pristine game function) is
/// done.
pub fn flip_apply(flips: &[(usize, i32)]) {
    for (addr, _) in flips {
        // SAFETY: `addr` is a `r.contents` slot inside the game's gentity array
        // (built by `flip_list`/`touch_flips`).
        unsafe { write_i32(*addr, 0) };
    }
}

/// Restore a window's flips. A slot is restored only while it is still in use
/// and still shows the `0` this gate wrote: the guard keeps a window that runs
/// *game* code (`G_TouchTriggers`) from writing a stale `contents` back over a
/// value the game set meanwhile (`Use_Item` re-arming an item, a target that
/// reached a foreign entity) and from resurrecting contents on a slot the game
/// freed inside the window (`G_FreeEntity` memsets it). For a trace window,
/// which runs no game code at all, the guard always holds.
pub fn flip_restore(flips: &[(usize, i32)]) {
    for (addr, saved) in flips {
        // SAFETY: `addr` is a `r.contents` slot inside the game's gentity array;
        // the entity pointer is recovered by the same offset math.
        let inuse = unsafe { read_i32(*addr - OFFSET_GENTITY_R_CONTENTS + OFFSET_GENTITY_INUSE) };
        if inuse == 0 {
            continue;
        }
        // SAFETY: same slot.
        if unsafe { read_i32(*addr) } != 0 {
            continue;
        }
        // SAFETY: same slot.
        unsafe { write_i32(*addr, *saved) };
    }
}

/// The flip list for one `G_TouchTriggers` call: `CONTENTS_TRIGGER` cleared on
/// every foreign instance's trigger, door trigger and item for the duration of
/// the call. Pristine `G_TouchTriggers` skips an entity whose `r.contents` lacks
/// `CONTENTS_TRIGGER` (`g_active.c:548`), so nothing is reimplemented: a player
/// touches their own instance's triggers and nobody else's.
pub fn touch_flips(ent: usize, out: &mut Vec<(usize, i32)>) -> usize {
    out.clear();
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
        return 0;
    }
    state::with_state(|s| {
        let Some(requester) = touch_requester(&s.tffa, g_entities, ent) else {
            return 0;
        };
        collect_flips(&s.tffa, g_entities, requester, CONTENTS_TRIGGER, out)
    })
}

/// The instance a `G_TouchTriggers(ent)` call belongs to: the calling player's
/// own match. `None` for anything that is not a live client slot — pristine
/// `G_TouchTriggers` returns immediately for those (`g_active.c:524-531`), so
/// there is nothing to isolate.
pub(crate) fn touch_requester(
    s: &crate::state::TffaState,
    g_entities: usize,
    ent: usize,
) -> Option<u8> {
    if ent < g_entities {
        return None;
    }
    let idx = (ent - g_entities) / SIZEOF_GENTITY;
    if idx >= MAX_CLIENTS {
        return None;
    }
    // SAFETY: ent is inside the g_entities array.
    if unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) } == 0 {
        return None;
    }
    Some(s.client_match[idx])
}

/// The instance a call into game code belongs to — the *acting* entity's own
/// instance. `None` when the actor cannot be attributed: a live player slot
/// resolves through `client_match` (a chain or a think fired by a player is that
/// player's instance's), a tagged world entity through its tag (main included),
/// a live entity that carries no tag through its owner (one `r.ownerNum` hop to
/// a live player: missiles, saber entities, physics objects, bodies — the same
/// resolution the trace gate's `request_match` uses), and anything else — a
/// shared inert prop, a freed one, a pointer below the array, the world slots —
/// resolves to nothing.
pub(crate) fn acting_instance(
    s: &crate::state::TffaState,
    g_entities: usize,
    ent: usize,
) -> Option<u8> {
    if g_entities == 0 || ent < g_entities {
        return None;
    }
    let num = (ent - g_entities) / SIZEOF_GENTITY;
    if num >= MAX_GENTITIES {
        return None;
    }
    if num < MAX_CLIENTS {
        // SAFETY: `ent` is inside the g_entities array.
        if unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) } == 0 {
            return None;
        }
        return Some(s.client_match[num]);
    }
    if num >= ENTITYNUM_MAX_NORMAL as usize {
        // Only the ordinary range is ever tagged (and `G_Spawn` never hands out
        // `ENTITYNUM_WORLD`/`ENTITYNUM_NONE`).
        return None;
    }
    // SAFETY: `ent` is inside the g_entities array.
    if unsafe { read_i32(ent + OFFSET_GENTITY_INUSE) } == 0 {
        // A freed slot keeps no instance: its clone set is gone, and its stale
        // tag must not scope anybody's chain.
        return None;
    }
    let tag = s.entity_instance[num];
    if tag != ENTITY_SHARED {
        return Some(tag);
    }
    // Untagged but owned: a missile, saber entity, physics object or body acts
    // in its owner's instance. Its instance is not the tag table's business (the
    // owned-entity machinery already scopes it), but the *code it runs* — an
    // impact's radius damage, a box query, a chain it fires — belongs there.
    request_match(g_entities, num as c_int, &s.client_match)
}

/// Open the acting scope for one call into game code (`G_RunThink`,
/// `G_RunMissile`, `G_UseTargets2`, `player_die`) and return the scope to hand
/// back to `close_acting_scope`. The acting entity's instance is authoritative;
/// when it cannot be attributed the enclosing scope is kept, so a cascade a
/// player action started never widens halfway through.
pub fn open_acting_scope(ent: usize) -> u8 {
    state::with_state(|s| {
        let previous = s.tffa.acting_scope;
        let g_entities = s.located_game_data.g_entities;
        let this = acting_instance(&s.tffa, g_entities, ent);
        s.tffa.acting_scope = this.unwrap_or(previous);
        previous
    })
}

/// The acting scope now open, or `SCOPE_NONE`. A trap gate reads it once per
/// call rather than holding the state lock across the engine call.
pub fn acting_scope() -> u8 {
    state::with_state(|s| s.tffa.acting_scope)
}

/// Open the acting scope for one call the **engine** makes on behalf of a client
/// (`vmMain`: `GAME_CLIENT_COMMAND`, `GAME_CLIENT_DISCONNECT`). The vmMain path
/// carries only the client number — and the game's own `ClientCommand` /
/// `ClientDisconnect` may clear `ent->client` inside the call — so the client's
/// match is read from the proxy's own table instead of the entity. Keeps the
/// enclosing scope for a number that names no client.
pub fn open_client_scope(client_num: c_int) -> u8 {
    state::with_state(|s| {
        let previous = s.tffa.acting_scope;
        if (0..MAX_CLIENTS as c_int).contains(&client_num) {
            s.tffa.acting_scope = s.tffa.client_match[client_num as usize];
        }
        previous
    })
}

/// Close the acting scope of one call into game code. At the outermost close —
/// no scope was open before it — the entities spawned anywhere inside the call
/// are classified: an interactive world entity joins the instance of the entity
/// whose code spawned it, so each instance's own copy of that entity produces
/// its own children (and never one that is shared by all of them).
pub fn close_acting_scope(previous: u8) {
    if previous == SCOPE_NONE {
        flush_pending_spawns();
    }
    state::with_state(|s| s.tffa.acting_scope = previous);
}

/// Whether the open acting scope hides an entity a `G_Find` search named: yes
/// for an entity tagged to another instance, no for everything else. Players and
/// untagged entities (shared props, `info_player_*`, lights) stay reachable from
/// every instance. This is the `G_Find` link gate's question (S7); the
/// box/contact gates ask the wider world-query rule (`query_hides`), which also
/// drops the players of another instance.
pub fn link_hides(found: usize, window: u8) -> bool {
    state::with_state(|s| {
        link_hides_tag(
            &s.tffa.entity_instance,
            s.located_game_data.g_entities,
            found,
            window,
        )
    })
}

/// Compact a box-query result in place, dropping the entries a foreign instance
/// owns or plays; returns the number of entries kept. Pure over the caller's
/// slice, so the rule is unit-tested directly.
pub(crate) fn retain_visible_slots(
    tags: &[u8; MAX_GENTITIES],
    client_match: &[u8; MAX_CLIENTS],
    list: &mut [c_int],
    scope: u8,
) -> usize {
    let mut kept = 0usize;
    for i in 0..list.len() {
        let num = list[i];
        if query_hides_num(tags, client_match, num, scope) {
            continue;
        }
        list[kept] = num;
        kept += 1;
    }
    kept
}

/// Whether the open acting scope hides a *specific* entity in a world query
/// (`G_EntityContact`/`G_EntityContactCapsule`) — the pointer form of the box
/// filter's rule: an entity tagged to another instance, or a player of another
/// instance, is not there for this query.
pub fn query_hides(found: usize, window: u8) -> bool {
    state::with_state(|s| {
        let g_entities = s.located_game_data.g_entities;
        if g_entities == 0 || found < g_entities {
            return false;
        }
        let num = ((found - g_entities) / SIZEOF_GENTITY) as i32;
        query_hides_num(&s.tffa.entity_instance, &s.tffa.client_match, num, window)
    })
}

/// The hide rule proper, in the form every user needs — a *slot number* is
/// hidden when it is tagged to another instance than `scope`. The world slots
/// and untagged (shared) entities are never hidden, the rule the visibility,
/// collision and touch gates also follow. No state access, so it can be
/// unit-tested with a fabricated tag table.
pub(crate) fn scope_hides_num(tags: &[u8; MAX_GENTITIES], num: i32, scope: u8) -> bool {
    if !(MAX_CLIENTS as i32..ENTITYNUM_MAX_NORMAL).contains(&num) {
        return false;
    }
    let tag = tags[num as usize];
    tag != ENTITY_SHARED && tag != scope
}

/// The world-query hide rule: the tag rule plus the players of another
/// instance. A player carries no instance tag (players are never cloned and the
/// tag table covers the ordinary `MAX_CLIENTS..ENTITYNUM_MAX_NORMAL` range), so
/// their instance is read from the proxy's `client_match` table — the one the
/// visibility, collision and damage gates use. Only the box/contact gates ask
/// this question; the `G_Find` link gate keeps the tag-only rule
/// (`scope_hides_num`), because a `targetname` chain resolves *named* entities
/// and never a player by slot.
pub(crate) fn query_hides_num(
    tags: &[u8; MAX_GENTITIES],
    client_match: &[u8; MAX_CLIENTS],
    num: i32,
    scope: u8,
) -> bool {
    if (0..MAX_CLIENTS as i32).contains(&num) {
        // `SCOPE_NONE` means "no instance filter" and never reaches the gates
        // (they forward the pristine call instead); keep that answer intact
        // rather than hiding every player if one ever did.
        return scope != SCOPE_NONE && client_match[num as usize] != scope;
    }
    scope_hides_num(tags, num, scope)
}

/// The hide decision for a `G_Find` result pointer — the pointer form of
/// `scope_hides_num`.
pub(crate) fn link_hides_tag(
    tags: &[u8; MAX_GENTITIES],
    g_entities: usize,
    found: usize,
    window: u8,
) -> bool {
    if g_entities == 0 || found < g_entities {
        return false;
    }
    scope_hides_num(tags, ((found - g_entities) / SIZEOF_GENTITY) as i32, window)
}

/// Walk pristine `G_Find` results from `from`, skipping the entities an open
/// acting scope hides, and return the first visible match (or `0`, `NULL`).
/// `next` is one pristine search step — every comparison and the
/// `level.num_entities` bound stay the game's own, nothing is reimplemented —
/// and it always advances, so the walk terminates. No lock is held across it.
pub fn find_visible(
    from: usize,
    mut next: impl FnMut(usize) -> usize,
    hides: impl Fn(usize) -> bool,
) -> usize {
    let mut cur = from;
    loop {
        let found = next(cur);
        if found == 0 || !hides(found) {
            return found;
        }
        cur = found;
    }
}

/// The `(slot, instance)` one `G_Spawn` inside game code contributes, or `None`
/// when it belongs to no instance: nothing is running game code, the slot is not
/// a `gentity_t` of the ordinary world range (`MAX_CLIENTS..ENTITYNUM_MAX_NORMAL`,
/// the range `G_Spawn` hands out and the only range that is ever tagged), or the
/// `g_entities` array is not located.
pub(crate) fn spawn_record(acting_scope: u8, g_entities: usize, ed: usize) -> Option<(u16, u8)> {
    if acting_scope == SCOPE_NONE || ed == 0 {
        return None;
    }
    let num = ent_num_of(g_entities, ed)?;
    if !(MAX_CLIENTS as c_int..ENTITYNUM_MAX_NORMAL).contains(&num) {
        return None;
    }
    Some((num as u16, acting_scope))
}

/// `G_Spawn` detour body — the pristine allocation already happened. A fresh
/// entity carries no callbacks yet (`G_InitGentity` just reset it), so the
/// interactive predicate cannot be applied here: the slot is only remembered,
/// with the instance of the code that spawned it, and
/// `flush_pending_spawns` classifies it when that code returns.
pub fn note_spawned(ed: usize) {
    state::with_state(|s| {
        let scope = s.tffa.acting_scope;
        if scope == SCOPE_NONE {
            return;
        }
        let g_entities = s.located_game_data.g_entities;
        if let Some(record) = spawn_record(scope, g_entities, ed) {
            s.tffa.spawn_pending.push(record);
        }
    });
}

/// Classify the slots spawned since the outermost acting call opened: an
/// interactive world entity inherits the instance recorded with it, everything
/// else (an inert prop, a temp event entity, a slot the game already freed) stays
/// untagged and shared. Returns how many entities joined an instance. No state is
/// touched, so it can be unit-tested on a fabricated `g_entities`.
pub(crate) fn tag_pending(
    tags: &mut [u8; MAX_GENTITIES],
    table: &[u8; MAX_CLIENTS],
    g_entities: usize,
    pending: &[(u16, u8)],
) -> usize {
    let mut tagged = 0usize;
    for &(num, instance) in pending {
        if g_entities == 0 {
            break;
        }
        let ent = g_entities + num as usize * SIZEOF_GENTITY;
        // SAFETY: `num` is an ordinary-range gentity slot (checked when it was
        // recorded) and every field read is valid for a spawned gentity.
        let live = unsafe { read_i32(ent + OFFSET_GENTITY_INUSE) } != 0;
        if !live {
            // `G_Spawn` recycled the slot again, or the game freed the entity
            // before its spawner returned: there is nothing to tag.
            continue;
        }
        // A temp event entity (`eType = ET_EVENTS + event`) is the event/sound
        // machinery's, never a world entity: its visibility is the sound
        // attribution's and it frees itself after its event, so it keeps no
        // instance even inside an acting scope.
        // SAFETY: same slot as above; `s.eType` is a plain int.
        if unsafe { read_i32(ent + OFFSET_GENTITY_S_ETYPE) } >= ET_EVENTS {
            continue;
        }
        // An entity that carries a live-player owner is already isolated by the
        // owned-entity machinery (visibility, trace and touch all resolve it
        // through `r.ownerNum`), and a tag would additionally enroll a live
        // projectile or physics object in the clone set a later instance copies.
        // Only what nothing else scopes — a dropped item, a trigger or door
        // trigger the map creates mid-map — needs the tag.
        if request_match(g_entities, num as c_int, table).is_some() {
            continue;
        }
        if !is_interactive_entity(g_entities, num as c_int) {
            continue;
        }
        tags[num as usize] = instance;
        tagged += 1;
    }
    tagged
}

/// Drain the pending spawns and tag those that turned out interactive. Runs at
/// the outermost acting call's close (and, defensively, at frame start, so an
/// entry can never outlive the call it was recorded in).
pub fn flush_pending_spawns() {
    let pending: Vec<(u16, u8)> = state::with_state(|s| core::mem::take(&mut s.tffa.spawn_pending));
    if pending.is_empty() {
        return;
    }
    let tagged = state::with_state(|s| {
        let g_entities = s.located_game_data.g_entities;
        tag_pending(
            &mut s.tffa.entity_instance,
            &s.tffa.client_match,
            g_entities,
            &pending,
        )
    });
    if tagged > 0 {
        eprintln!(
            "----- proxy-rs: {tagged} of {} runtime-spawned entities joined their spawner's instance",
            pending.len()
        );
    }
}

/// The match whose trace this is: player slots resolve directly; other
/// entities resolve once through `r.ownerNum` (missiles, saber entities).
/// World/unknown requesters get `None` (no isolation for that trace).
pub(crate) fn request_match(
    g_entities: usize,
    ent_num: c_int,
    table: &[u8; MAX_CLIENTS],
) -> Option<u8> {
    // `ENTITYNUM_WORLD` (1022) and `ENTITYNUM_NONE` (1023) name no entity, and
    // `G_Spawn` never hands those slots out (`g_utils.c:837`): reading one would
    // take a zeroed slot's `r.ownerNum == 0` for a reference to client 0 and
    // attribute a world trace to client 0's match.
    if !(0..ENTITYNUM_MAX_NORMAL).contains(&ent_num) {
        return None;
    }
    let ent = g_entities + ent_num as usize * SIZEOF_GENTITY;
    let live_player = |num: i32| -> bool {
        if !(0..MAX_CLIENTS as i32).contains(&num) {
            return false;
        }
        // SAFETY: num is a valid g_entities slot.
        unsafe {
            read_usize(g_entities + num as usize * SIZEOF_GENTITY + OFFSET_GENTITY_CLIENT) != 0
        }
    };
    if live_player(ent_num) {
        return Some(table[ent_num as usize]);
    }
    // SAFETY: ent is inside the g_entities array.
    if unsafe { read_i32(ent + OFFSET_GENTITY_INUSE) } == 0 {
        // A freed slot's owner fields are memset zeros, not a reference to
        // client 0: an entity that is gone owns nothing.
        return None;
    }
    // SAFETY: ent is inside the g_entities array.
    let owner = unsafe { read_i32(ent + OFFSET_GENTITY_R_OWNER) };
    if live_player(owner) {
        return Some(table[owner as usize]);
    }
    None
}

/// Rebuild the linked player-owned entity cache (`r.linked` set and
/// `r.ownerNum` pointing at a live player).
pub(crate) fn rebuild_owned(s: &mut state::ProxyState, g_entities: usize) {
    s.tffa.owned_count = 0;
    for num in 0..MAX_GENTITIES as i32 {
        if s.tffa.owned_count >= OWNED_CAP {
            break;
        }
        let ent = g_entities + num as usize * crate::sdk::SIZEOF_GENTITY;
        // SAFETY: ent is inside the g_entities array.
        if unsafe { read_i32(ent + OFFSET_GENTITY_R_LINKED) } == 0 {
            continue;
        }
        // SAFETY: ent is inside the g_entities array.
        let owner = unsafe { read_i32(ent + OFFSET_GENTITY_R_OWNER) };
        if !(0..MAX_CLIENTS as i32).contains(&owner) {
            continue;
        }
        // SAFETY: owner is a valid gentity slot.
        let owner_client = unsafe {
            read_usize(
                g_entities + owner as usize * crate::sdk::SIZEOF_GENTITY + OFFSET_GENTITY_CLIENT,
            )
        };
        if owner_client == 0 {
            continue;
        }
        s.tffa.owned[s.tffa.owned_count] = (num as u16, owner as u8);
        s.tffa.owned_count += 1;
    }
}

/// Restore every client's pristine `CS_PLAYERS` bases and clear the duel HUD
/// configstrings after the ladder is torn down. The per-viewer refresh helpers
/// are gated on the instance core being enabled, which is already false by the
/// time a cvar-off teardown runs; this pushes the bases directly so no
/// `(Duel N)`/`(TFFA N)` prefix or proxy W/L survives into the stock game.
pub(crate) fn restore_pristine_views() {
    // The engine's `SV_SetConfigstring` skips a value equal to the server's,
    // so writing the base back would not broadcast: the per-viewer override
    // has to be replaced on each client directly. The instance core is off by
    // now, so a normal `send_cs_to_client` would defer under reliable
    // pressure into a `pending_view_refresh` that never runs; these are sent
    // immediately, and only where the viewer's recorded value differs.
    let viewers: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|v| s.clients[*v as usize].is_connected)
            .collect()
    });
    let (bases, sent): ([String; MAX_CLIENTS], [[String; MAX_CLIENTS]; MAX_CLIENTS]) =
        state::with_state(|s| {
            (
                s.tffa.cs_players_base.clone(),
                s.tffa.cs_players_sent.clone(),
            )
        });
    let mut restored = 0usize;
    let viewer_count = viewers.len();
    for viewer in viewers {
        let v = viewer as usize;
        for (slot, stored) in bases.iter().enumerate() {
            let base = if stored.is_empty() {
                get_live_cs_players(slot as i32).unwrap_or_default()
            } else {
                stored.clone()
            };
            if sent[v][slot] != base {
                send_cs_now(viewer, CS_PLAYERS + slot as i32, &base);
                restored += 1;
            }
        }
        send_cs_now(viewer, CS_CLIENT_DUELISTS, "-1|-1");
        send_cs_now(viewer, CS_CLIENT_DUELHEALTHS, "-1|-1|!");
        state::with_state(|s| {
            for value in s.tffa.cs_players_sent[v].iter_mut() {
                value.clear();
            }
            s.tffa.view_bases_from_gamestate[v] = false;
        });
    }
    eprintln!(
        "----- proxy-rs: restored {restored} per-viewer CS_PLAYERS value(s) across {viewer_count} viewer(s)"
    );
    state::with_state(|s| {
        s.duel.hud_duelists = core::array::from_fn(|_| String::new());
        s.duel.hud_healths = core::array::from_fn(|_| String::new());
        s.duel.hud_scores = [(i32::MIN, i32::MIN); MAX_CLIENTS];
    });
}

/// Record a `CS_PLAYERS + n` base string the game just set, then push the
/// per-viewer variants (prefixed outsiders forced to spectator) to every
/// connected client. Call after the trap has been forwarded so the engine
/// broadcast lands first and our per-viewer corrections win.
pub fn on_set_cs_players(slot: i32, value: &str) {
    if !state::instances_enabled() {
        return;
    }
    if !(0..MAX_CLIENTS as i32).contains(&slot) {
        return;
    }
    state::with_state(|s| {
        s.tffa.cs_players_base[slot as usize] = value.to_owned();
    });
    // The engine broadcast this value to every client before the handler ran
    // (the trap was forwarded first), so every connected viewer holds it now —
    // record that instead of re-sending it to same-match viewers below.
    record_cs_players_broadcast(slot as usize, value);
    // Follow-aware viewer matches (spectating adopts the target's instance).
    // State snapshot first (no nesting: effective_match locks per viewer).
    let (table, base): (u8, String) = state::with_state(|s| {
        (
            s.tffa.client_match[slot as usize],
            s.tffa.cs_players_base[slot as usize].clone(),
        )
    });
    if base.is_empty() {
        return;
    }
    let viewers: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|v| s.clients[*v as usize].is_connected)
            .collect()
    });
    for viewer in viewers {
        let viewer_match = effective_match(viewer);
        // Same-match / spectator viewers already received this base via the
        // engine broadcast we just forwarded — resending it doubles traffic
        // for zero effect (and overflows the reliable buffer on team swaps).
        if let Some(modified) =
            rewrite_cs_players_for_slot(&base, slot as usize, table, viewer_match)
        {
            push_cs_players(viewer, slot as usize, &modified);
        }
    }
}

/// Broadcast one player's slot to every viewer from the stored base (called
/// after proxy-side match moves, which change prefix/team without any game
/// `G_SET_CONFIGSTRING`).
pub fn broadcast_cs_players_slot(slot: i32) {
    if !state::instances_enabled() {
        return;
    }
    if !(0..MAX_CLIENTS as i32).contains(&slot) {
        return;
    }
    let (table, base): (u8, String) = state::with_state(|s| {
        (
            s.tffa.client_match[slot as usize],
            s.tffa.cs_players_base[slot as usize].clone(),
        )
    });
    if base.is_empty() {
        // No base yet (missed the game's SetConfigstring while disabled?):
        // fall back to the live server configstring so moves still prefix.
        if let Some(live) = get_live_cs_players(slot) {
            state::with_state(|s| {
                s.tffa.cs_players_base[slot as usize] = live.clone();
            });
            return broadcast_cs_players_slot(slot);
        }
        return;
    }
    let viewers: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|v| s.clients[*v as usize].is_connected)
            .collect()
    });
    for viewer in viewers {
        let viewer_match = effective_match(viewer);
        let value = rewrite_cs_players_for_slot(&base, slot as usize, table, viewer_match)
            .unwrap_or_else(|| base.clone());
        push_cs_players(viewer, slot as usize, &value);
    }
}
