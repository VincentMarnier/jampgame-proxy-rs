//! Parallel self-organized TFFA matches on one `GT_TEAM` map.
//!
//! Match `0` is the **main TFFA**: every connected client belongs to it
//! unless they create/join another match; it is always active and its
//! fraglimit is enforced like any other match. Slots `1..=MAX_MATCHES` are
//! player-created. Setting `proxy_tffa_enable` back to `0` closes every
//! created match and returns its members to the main game as spectators, with
//! a message explaining why (D-006.9). Scores are kept proxy-side per match
//! (team tally + per-player copy); the game's global `level.teamScores` is
//! neutralised (see `on_add_score`) so pristine `CheckExitRules` never ends
//! the server early, and the `scores` trap rewrite substitutes the per-player
//! copies so alternative matches never show main-TFFA-era numbers. Every
//! match shares the server's own `fraglimit`; `timelimit` stays global and
//! ends everything.
//!
//! A match that finishes remembers its roster (who played and for which team):
//! its players are sent to spectator as before, and a `map_restart`
//! re-creates the match and puts them back on those teams. Playing for another
//! match forgets the old one (D-006.10).
//!
//! Isolation:
//! - damage: `G_Damage`/`player_die` wrappers drop cross-match effects.
//! - visibility: `SV_AddEntitiesVisibleFromPoint` post-filter drops
//!   cross-match player/owned entities (kills ride as `EV_OBITUARY` temp
//!   entities, so the kill feed is hidden the same way).
//! - physics: the `G_TRACE`/`G_G2TRACE`/`G_TRACECAPSULE` traps flip
//!   `r.contents` of cross-match entities for the duration of the trace, so
//!   other matches neither body-block nor saber-clash (`trace_flips`).
//! - sounds: the `G_Sound`/`G_SoundTempEntity`/`G_TempEntity` detours stamp
//!   every temp entity with the match that created it (`sound_source`), and
//!   the snapshot filter drops cross-match `EV_GENERAL_SOUND`/
//!   `EV_ENTITY_SOUND`/`EV_PLAYER_TELEPORT_IN`/`EV_PLAYER_TELEPORT_OUT` events
//!   plus the predicted `EF_PLAYER_EVENT` combat events (`EV_FIRE_WEAPON`,
//!   `EV_SABER_ATTACK`, `EV_JUMP`, ...). Map/world sounds (`G_SoundAtLoc`,
//!   doors, NPC chatter) and `EV_GLOBAL_SOUND` broadcasts stay audible
//!   everywhere; item entities are per-instance clones (D-006.6), so their
//!   `EV_ITEM_RESPAWN` drops with the clone.
//! - scoreboard: `G_SEND_SERVER_COMMAND` trap rewrites `scores ...` per
//!   viewer with the per-player per-match score copies. The roster is unified
//!   (every connected client is listed, so players can see who is in which
//!   match); players of other matches are grouped as spectators through
//!   per-viewer `CS_PLAYERS` demotions re-pushed just before the scoreboard.
//! - team chat: the same trap drops per-viewer `tchat`/`ltchat` commands whose
//!   recipient is in another match (`team_chat_visible`), using the acting
//!   scope the `GAME_CLIENT_COMMAND` forward opens for the sender. Spectators
//!   are one team server-wide, so a spectator's team chat crosses matches (how
//!   people watching different games talk to each other) while a playing
//!   sender's stays inside its match. Global `chat` stays server-wide, and
//!   `tell` stays private as in base JKA.

use core::ffi::{CStr, c_char, c_int};

use crate::hooks::engine_sv::{read_i32, read_usize, write_i32, write_usize};
use crate::sdk::{
    CON_CONNECTED, CON_DISCONNECTED, CONTENTS_TRIGGER, CS_PLAYERS, CS_SCORES1, CS_SCORES2,
    ENTITYNUM_MAX_NORMAL, ET_EVENTS, ET_MOVER, ET_NPC, G_LINKENTITY, MAX_CLIENTS, MAX_GENTITIES,
    MAX_NETNAME, MAX_TOKEN_CHARS, OFFSET_GCLIENT_PERS, OFFSET_GCLIENT_PS, OFFSET_GCLIENT_SESS,
    OFFSET_GENTITY_ACTIVATOR, OFFSET_GENTITY_BLOCKED, OFFSET_GENTITY_CLIENT, OFFSET_GENTITY_ENEMY,
    OFFSET_GENTITY_GHOUL2, OFFSET_GENTITY_INUSE, OFFSET_GENTITY_LAST_ENEMY,
    OFFSET_GENTITY_NEXT_TRAIN, OFFSET_GENTITY_PARENT, OFFSET_GENTITY_PREV_TRAIN,
    OFFSET_GENTITY_R_CONTENTS, OFFSET_GENTITY_R_LINKED, OFFSET_GENTITY_R_OWNER,
    OFFSET_GENTITY_REACHED, OFFSET_GENTITY_S_ETYPE, OFFSET_GENTITY_S_NUMBER, OFFSET_GENTITY_TARGET,
    OFFSET_GENTITY_TARGET_ENT, OFFSET_GENTITY_TARGETNAME, OFFSET_GENTITY_TEAM,
    OFFSET_GENTITY_TEAMCHAIN, OFFSET_GENTITY_TEAMMASTER, OFFSET_GENTITY_THINK,
    OFFSET_GENTITY_TOUCH, OFFSET_GENTITY_USE, OFFSET_PERS_CONNECTED, OFFSET_PS_LEGS_FLIP,
    OFFSET_PS_TORSO_FLIP, OFFSET_SESS_SESSION_TEAM, SIZEOF_GENTITY, TEAM_BLUE, TEAM_RED,
    TEAM_SPECTATOR,
};
use crate::state::{self, CVAR_TFFA_ENABLE, CVAR_TFFA_MAX_GROUP_SIZE, CVAR_TFFA_MAX_MATCHES};

/// Match id of the implicit main TFFA.
pub const MAIN_MATCH: u8 = 0;
/// Maximum player-created matches (ids `1..=MAX_MATCHES`).
pub const MAX_MATCHES: usize = 8;
/// Backing array size: main slot + created slots.
pub const TOTAL_MATCH_SLOTS: usize = 1 + MAX_MATCHES;
/// Maximum length of a match password, in bytes. A password is one client
/// command argument (`trap_Argv`); quoting it (`tffa_create "my pass"`) allows
/// spaces, so the limit is on bytes, not words. Longer passwords are refused at
/// `tffa_create`/`tffa_join` rather than truncated.
pub const MAX_TFFA_PASSWORD: usize = 32;
/// Capacity of the per-frame player-owned entity cache (saber entities,
/// missiles, corpses — one or a few per player).
pub const OWNED_CAP: usize = 96;
/// No remembered membership: a `TffaMatch::remembered` slot (or a
/// `TffaState::pending_join` entry) holding this belongs to no finished
/// instance. The team constants (`TEAM_RED` 1 / `TEAM_BLUE` 2) are the only
/// other values either table stores (D-006.10).
pub const REMEMBERED_NONE: u8 = 0;
/// Frames the engine runs between `SV_RestartGameProgs` and the client
/// reconnect burst of a `map_restart` (`SV_MapRestart_f`, `sv_ccmds.cpp:306-349`:
/// three unconditional `GAME_RUN_FRAME` calls, then the connect loop). Used as
/// the fallback length of the settle window when no client is left to reconnect
/// and so no `GAME_CLIENT_CONNECT` can end it.
const RESTART_SETTLE_FRAMES: u8 = 3;
/// Frames the spectator playerstate animation flip is held for (D-006.14).
/// `SpectatorClientEndFrame` rewrites the spectator's ps from the followed
/// player every game frame, so the toggle must be re-applied each frame; the
/// client sees the flip once and then the restored value once.
const ANIM_PS_FLIP_FRAMES: u8 = 5;
/// `scores ...` entries carry 14 ints each (`g_cmds.c:64-74`); the second
/// int is `PERS_SCORE` (`g_cmds.c:65-66`).
const SCORE_ENTRY_INTS: usize = 14;
const SCORE_ENTRY_SCORE: usize = 1;

/// Whether parallel TFFA (the team-match policy) is active.
///
/// Instance-wide machinery (isolation gates, per-viewer `cs`, acting scopes,
/// per-instance worlds) is gated by [`state::instances_enabled`] instead, so
/// the shared core serves the parallel duel ladder too (D-025). The `tffa_*`
/// commands, team rules, passwords, promotion and team chat stay here.
pub fn enabled() -> bool {
    state::team_mode()
}

/// Match of a client (`0` = main TFFA). Out-of-range or connecting → main.
pub fn client_match(client_num: i32) -> u8 {
    if !(0..MAX_CLIENTS as i32).contains(&client_num) {
        return MAIN_MATCH;
    }
    state::with_state(|s| s.tffa.client_match[client_num as usize])
}

/// Whether the match is inside its end-of-match delay: its outcome is decided
/// (fraglimit hit or timelimit expired untied) but the pristine
/// `LogExit` → `BeginIntermission` window has not elapsed. Damage involving its
/// players is void until it ends (`END_DELAY_MS`).
pub fn match_ending(match_id: u8) -> bool {
    state::with_state(|s| {
        s.tffa
            .matches
            .get(match_id as usize)
            .is_some_and(state::TffaMatch::ending)
    })
}

/// Whether a server command is a team chat message. `G_SayTo` sends `SAY_TEAM`
/// as `tchat` (plain) or `ltchat` (with a location prefix)
/// (`g_cmds.c:1511-1522`); global `chat`/`lchat` are not team chat.
///
/// Byte-level on purpose: the command carries the client's raw chat text, which
/// is whatever single-byte encoding the sender typed (a latin-1 `é` is `0xE9`,
/// not valid UTF-8). Gating on `str` made the whole `G_SEND_SERVER_COMMAND`
/// interception — team chat included — skip every non-UTF-8 message, which sent
/// accented team chat to every instance.
pub fn is_team_chat(command: &[u8]) -> bool {
    command.starts_with(b"tchat ") || command.starts_with(b"ltchat ")
}

/// Pure team-chat gate: a `tchat`/`ltchat` sent by a client in `sender_match`
/// may only reach a client in that same match. `SCOPE_NONE` (no attributable
/// sender) and out-of-range recipients (a broadcast) are left alone, so chat
/// generated outside a scoped client command keeps its old reach.
///
/// `sender_spectator` relaxes the split: every spectator is on the one
/// `TEAM_SPECTATOR` team of the whole server (`g_cmds.c:1489-1491`'s
/// `OnSameTeam`), and spectator chat is how people watching one match talk to
/// people watching another. A playing sender stays confined to its match.
pub fn team_chat_visible_in(
    sender_match: u8,
    sender_spectator: bool,
    recipient: c_int,
    client_match: &[u8; MAX_CLIENTS],
) -> bool {
    if sender_match == SCOPE_NONE
        || sender_spectator
        || !(0..MAX_CLIENTS as c_int).contains(&recipient)
    {
        return true;
    }
    client_match[recipient as usize] == sender_match
}

/// `team_chat_visible_in` against the live client table, with the speaker's
/// team read from game memory.
pub fn team_chat_visible(sender_match: u8, recipient: c_int) -> bool {
    let sender_spectator = sender_match != SCOPE_NONE && sender_is_spectator();
    state::with_state(|s| {
        team_chat_visible_in(
            sender_match,
            sender_spectator,
            recipient,
            &s.tffa.client_match,
        )
    })
}

/// Whether the client whose command is in flight (`GAME_CLIENT_COMMAND`, the
/// forward that runs `ClientCommand` → `G_Say`) is on the spectator team.
/// Unknown or absent (`-1`) is "not a spectator", so the fail-safe answer keeps
/// the per-match split in force.
fn sender_is_spectator() -> bool {
    let sender = crate::ignore::acting_sender();
    (0..MAX_CLIENTS as c_int).contains(&sender) && session_team_of(sender) == Some(TEAM_SPECTATOR)
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
fn follow_target(viewer: c_int) -> Option<c_int> {
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
fn resolve_follow_target(raw: c_int, follow1: c_int, follow2: c_int) -> Option<c_int> {
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

/// Pure end-of-match damage gate: a hit is void while either end belongs to a
/// match serving its end-of-match delay (`TffaMatch::ending`). An absent end
/// (a non-player entity such as a door or the world) contributes nothing, so a
/// player in an ending match can neither deal nor take damage, while every
/// other instance keeps fighting. Mirrors pristine `G_Damage`'s
/// `level.intermissionQueued` early return (`g_combat.c:4520-4524`).
pub fn damage_void_in(
    matches: &[state::TffaMatch; TOTAL_MATCH_SLOTS],
    victim_match: Option<u8>,
    attacker_match: Option<u8>,
) -> bool {
    [victim_match, attacker_match]
        .into_iter()
        .flatten()
        .any(|m| {
            matches
                .get(m as usize)
                .is_some_and(state::TffaMatch::ending)
        })
}

/// Pure snapshot gate: `true` = keep entity for viewer.
pub fn entity_visible(viewer_match: u8, ent_match: u8) -> bool {
    viewer_match == ent_match
}

/// Instance tag of an entity belonging to no instance: visible and solid in
/// every instance (players, missiles, temp ents, inert props).
pub const ENTITY_SHARED: u8 = u8::MAX;

/// Visibility gate for a tagged world entity (per-instance world entities,
/// D-006 extension): `ENTITY_SHARED` is visible to every viewer, a tagged one
/// only to its own instance.
pub fn world_entity_visible(viewer_match: u8, tag: u8) -> bool {
    tag == ENTITY_SHARED || viewer_match == tag
}

/// Whether a per-match score has hit the shared `fraglimit`.
pub fn fraglimit_hit(red: i32, blue: i32, fraglimit: i32) -> bool {
    fraglimit > 0 && (red >= fraglimit || blue >= fraglimit)
}

// ---------------------------------------------------------------------------
// scores trap rewrite
// ---------------------------------------------------------------------------

/// Rewrite a `scores ...` server command for `viewer`: unified roster — every
/// connected client is listed, the team header is the viewer's board match
/// red/blue, and each entry's `PERS_SCORE` is the per-match copy. Players of
/// other matches are shown as spectators by the per-viewer `CS_PLAYERS`
/// overrides (name prefixed `(TFFA N)`), which are re-pushed right before the
/// scoreboard is sent (`refresh_cs_players_for_scores`), so an outsider can
/// never be grouped into the viewer's RED/BLUE side.
/// Returns `None` when the input is not a scores command or does not parse
/// (caller then passes the original through).
pub fn rewrite_scores_for_viewer(text: &str, viewer: c_int) -> Option<String> {
    if !enabled() {
        return None;
    }
    let body = text.strip_prefix("scores ")?;
    let mut nums: Vec<i32> = Vec::with_capacity(3 + 32 * SCORE_ENTRY_INTS);
    for tok in body.split_whitespace() {
        nums.push(tok.parse::<i32>().ok()?);
    }
    if nums.len() < 3 {
        return None;
    }
    let entries = &nums[3..];
    if entries.len() % SCORE_ENTRY_INTS != 0 {
        return None;
    }
    let viewer_match = viewer_board_match(viewer);
    let (mred, mblue, player_scores) = state::with_state(|s| {
        let m = &s.tffa.matches[viewer_match as usize];
        (m.red, m.blue, s.tffa.player_score)
    });
    Some(filter_scores(mred, mblue, &player_scores, &nums[3..]))
}

/// Pure `scores ...` rewrite (no global state): keep every entry (unified
/// roster), substitute the per-match red/blue header and each player's
/// per-match score copy. Outsider-vs-own separation happens in the viewer's
/// client-side team data (the per-viewer `CS_PLAYERS` team override), so the
/// tab lists everyone while only the viewer's match players count as playing.
/// Entries naming an out-of-range client are kept untouched (the game never
/// emits one, and dropping data would be worse than passing it through).
pub fn filter_scores(
    red: i32,
    blue: i32,
    player_scores: &[i32; MAX_CLIENTS],
    entries: &[i32],
) -> String {
    let mut kept: Vec<i32> = Vec::new();
    let mut count = 0i32;
    for chunk in entries.chunks(SCORE_ENTRY_INTS) {
        if chunk.len() < SCORE_ENTRY_INTS {
            break;
        }
        kept.extend_from_slice(chunk);
        // Substitute the global PERS_SCORE with the per-match copy.
        let cn = chunk[0];
        if (0..MAX_CLIENTS as i32).contains(&cn) {
            kept[count as usize * SCORE_ENTRY_INTS + SCORE_ENTRY_SCORE] =
                player_scores[cn as usize];
        }
        count += 1;
    }
    let mut out = format!("scores {count} {red} {blue}");
    for v in kept {
        out.push_str(&format!(" {v}"));
    }
    out
}

/// Match whose scoreboard `viewer` sees. Normally the follow-aware
/// `effective_match`; while an intermission runs, the match captured just
/// before the pristine `BeginIntermission` stopped following
/// (`snapshot_intermission_boards`), so a spectator watching another match
/// keeps that match's final board instead of falling back to their own
/// (often empty) one and reading e.g. `TIED AT 0`. The captured match must
/// still be active (a finished match already had its tally reset); otherwise
/// the normal rule applies.
fn viewer_board_match(viewer: c_int) -> u8 {
    let effective = effective_match(viewer);
    if !(0..MAX_CLIENTS as c_int).contains(&viewer) {
        return effective;
    }
    state::with_state(|s| {
        let board = s.tffa.intermission_boards[viewer as usize];
        let board_active = s.tffa.matches.get(board as usize).is_some_and(|m| m.active);
        pick_board_match(effective, board, board_active, s.tffa.intermission)
    })
}

/// Pure board choice: during an intermission keep the match captured for the
/// viewer when it is still active, otherwise use the follow-aware match.
fn pick_board_match(effective: u8, board: u8, board_active: bool, intermission: bool) -> u8 {
    if intermission && board_active {
        board
    } else {
        effective
    }
}

/// Capture each connected viewer's follow-aware match before the pristine
/// `BeginIntermission` runs: it stops following (dropping the watched match)
/// before it sends the final `scores` message, so the scoreboard rewrite needs
/// the pre-move value (`viewer_board_match`).
pub fn snapshot_intermission_boards() {
    if !enabled() {
        return;
    }
    let viewers: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|v| s.clients[*v as usize].is_connected)
            .collect()
    });
    let mut boards = [MAIN_MATCH; MAX_CLIENTS];
    for viewer in viewers {
        boards[viewer as usize] = effective_match(viewer);
    }
    state::with_state(|s| s.tffa.intermission_boards = boards);
}

// ---------------------------------------------------------------------------
// Per-viewer configstring overrides (`cs` via SendServerCommand)
// ---------------------------------------------------------------------------

/// Parse a Quake info string (`n\Bob\t\1\...`, with or without leading `\`)
/// into ordered pairs.
fn info_pairs(info: &str) -> Vec<(String, String)> {
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
fn prefix_for(player_match: u8) -> String {
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

/// Whether a `G_SEND_SERVER_COMMAND` text is a pristine fraglimit notice to
/// swallow while TFFA owns all fraglimit endings (companion to the `log_exit`
/// hook, which stops the intermission itself; this stops the spurious chat
/// line sent just before it): `print "Red|Blue <ed-ref>\n"` for team takes
/// and `print "<name>^7 <ed-ref>.\n"` for the per-player take. On the wire the
/// string-ed reference carries the `HIT_THE_KILL_LIMIT` key; player chat
/// travels as `say`/`chat`, never as game-originated `print "` with that key.
pub fn is_kill_limit_print(text: &str) -> bool {
    text.starts_with("print \"") && text.contains("HIT_THE_KILL_LIMIT")
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
fn reliable_backlog(client_num: c_int) -> Option<i32> {
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

/// Backlog at which the proxy stops adding reliable commands to a client,
/// leaving the rest of the 128-slot window to the pristine game's own
/// `map_restart` broadcasts (about four `cs`/`cp`/`print` commands per
/// reconnecting client, so a 24-slot server's burst alone is already ~96).
/// Above it a correction is deferred to a later frame instead of being queued
/// into the burst (D-006.12).
const RELIABLE_SEND_LIMIT: i32 = 16;

/// Whether `client_num` has too many unacknowledged reliable commands for the
/// proxy to add another one now. Pure half of [`reliable_pressure`] so the
/// threshold is unit-tested without an engine.
fn backlog_is_pressured(backlog: Option<i32>) -> bool {
    backlog.is_some_and(|n| n >= RELIABLE_SEND_LIMIT)
}

/// Whether a proxy `cs` correction must wait for a later frame. Reads the
/// engine's per-client window (see [`reliable_backlog`]).
fn reliable_pressure(client_num: c_int) -> bool {
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
fn send_cs_to_client(client_num: c_int, index: i32, value: &str) -> bool {
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
fn cs_players_slot(index: i32) -> Option<usize> {
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
fn apply_pending_spectator_anim_flips() {
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
fn flush_deferred_model_loads() {
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
fn push_cs_players(client_num: c_int, slot: usize, value: &str) -> bool {
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
fn record_cs_players_sent(client_num: c_int, slot: usize, value: &str) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) || slot >= MAX_CLIENTS {
        return;
    }
    state::with_state(|s| s.tffa.cs_players_sent[client_num as usize][slot] = value.to_owned());
}

/// Record that the engine itself just broadcast `value` for `slot` to every
/// connected client (`G_SET_CONFIGSTRING` forwarding), so the next per-viewer
/// refresh does not re-send the base string to same-match viewers.
fn record_cs_players_broadcast(slot: usize, value: &str) {
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
        if let Some(modified) = rewrite_cs_players_for_viewer(&base, table, viewer_match) {
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
        let value = rewrite_cs_players_for_viewer(&base, table, viewer_match)
            .unwrap_or_else(|| base.clone());
        push_cs_players(viewer, slot as usize, &value);
    }
}

/// Push per-viewer `CS_SCORES1/2` (the viewer's own match tallies) to every
/// connected client. Call after the game's `CalculateRanks` broadcast so the
/// global (neutralised/transient) values are corrected per viewer and the
/// mini HUD shows each match's own score instead of flapping `1-0`.
pub fn on_set_cs_scores() {
    if !enabled() {
        return;
    }
    // Follow-aware: spectating adopts the target's instance, so the HUD
    // follows who you watch.
    let viewers: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|v| s.clients[*v as usize].is_connected)
            .collect()
    });
    let matches = state::with_state(|s| s.tffa.matches);
    for viewer in viewers {
        let vm = effective_match(viewer);
        let Some(m) = matches.get(vm as usize) else {
            continue;
        };
        // Per-component dedup (reliable-buffer guard): a frag changes exactly
        // one tally, so send a single `cs` instead of both — resending
        // unchanged scores to everyone overflows (every frag triggers
        // `CalculateRanks` twice plus this fresh push).
        let (cached_red, cached_blue) =
            state::with_state(|s| s.tffa.cs_scores_sent[viewer as usize]);
        if m.red != cached_red {
            send_single_cs_score(viewer, CS_SCORES1, m.red);
        }
        if m.blue != cached_blue {
            send_single_cs_score(viewer, CS_SCORES2, m.blue);
        }
    }
}

/// Per-viewer correction after the game itself broadcast `CS_SCORES1` or
/// `CS_SCORES2` (pristine `CalculateRanks` inside `AddScore`, connect,
/// disconnect, team change).
///
/// The engine broadcast just overwrote every client's HUD with the *global*
/// `level.teamScores` value — which is wrong for every match whose tally
/// differs. The `(red, blue)` dedup cache cannot see that overwrite (it only
/// tracks what *we* last sent), so a plain `on_set_cs_scores()` here would skip every viewer whose per-match tally
/// did not change — leaving innocent bystander matches stuck on the killer's
/// global bump. Instead compare each viewer's own match tally against the
/// broadcast value and re-push (single score) only where they differ, keeping
/// the cache pinned to the per-match tallies so the later fresh push (from
/// `on_add_score`, after the proxy bump) only re-sends the scorer's match.
pub fn on_set_cs_scores_broadcast(index: i32, value: &str) {
    if !enabled() {
        return;
    }
    let is_red = index == CS_SCORES1;
    let is_blue = index == CS_SCORES2;
    if !is_red && !is_blue {
        return;
    }
    let Ok(broadcast) = value.trim().parse::<i32>() else {
        // Unparseable broadcast: cannot compare, so invalidate and fall back
        // to a full push rather than leave HUDs stuck.
        state::with_state(|s| {
            for v in 0..MAX_CLIENTS {
                s.tffa.cs_scores_sent[v] = (i32::MIN, i32::MIN);
            }
        });
        on_set_cs_scores();
        return;
    };
    let viewers: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|v| s.clients[*v as usize].is_connected)
            .collect()
    });
    let matches = state::with_state(|s| s.tffa.matches);
    for viewer in viewers {
        let vm = effective_match(viewer);
        let Some(m) = matches.get(vm as usize) else {
            continue;
        };
        let expected = if is_red { m.red } else { m.blue };
        if expected != broadcast {
            send_single_cs_score(viewer, index, expected);
        } else {
            // Broadcast already correct for this viewer: pin the cache to the
            // per-match tally (e.g. first broadcast after connect, when the
            // cache still holds the `(MIN, MIN)` sentinel) so later dedup
            // compares against reality.
            state::with_state(|s| {
                if (0..MAX_CLIENTS as c_int).contains(&viewer) {
                    let entry = &mut s.tffa.cs_scores_sent[viewer as usize];
                    if is_red {
                        entry.0 = m.red;
                    } else {
                        entry.1 = m.blue;
                    }
                }
            });
        }
    }
}

/// Send one `CS_SCORES1/2` correction to a viewer and pin that half of the
/// dedup cache (the other half is untouched: it already matches the viewer's
/// per-match tally).
fn send_single_cs_score(viewer: c_int, index: i32, value: i32) {
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
fn get_live_cs(index: i32) -> Option<String> {
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
fn get_live_cs_players(slot: i32) -> Option<String> {
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
    let (red, blue) = state::with_state(|s| {
        let m = &s.tffa.matches[viewer_match as usize];
        (m.red, m.blue)
    });
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
fn override_push_plan(
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
    let (red, blue) = state::with_state(|s| {
        let m = &s.tffa.matches[viewer_match as usize];
        (m.red, m.blue)
    });
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

/// Re-push the `CS_PLAYERS` overrides (prefix + spectator team) for `viewer`
/// right before a `scores ...` message is forwarded.
///
/// The tab scoreboard groups entries by each client's `cgs.clientinfo[].team`,
/// which comes from `CS_PLAYERS`; an override sent when the base changed can be
/// lost (reliable-buffer pressure during connect/team churn, a gamestate
/// resync, or a base the viewer processed after it). When that happens an
/// outsider is grouped on the viewer's RED/BLUE side even though the proxy had
/// classified them as another match's player. Re-sending the overrides first
/// (reliable commands are ordered) makes the team data correct at the moment
/// the scoreboard is parsed, and it stays correct afterwards.
///
/// Only values the viewer does not already hold are queued
/// (`push_cs_players`): the payloads are full player info strings and a
/// populated multi-match server has dozens of playing outsiders per viewer, so
/// re-sending all of them on every scoreboard request queued kilobytes of
/// reliable commands to the one client that opened the board.
pub fn refresh_cs_players_for_scores(viewer: c_int) {
    if !state::instances_enabled() {
        return;
    }
    if !(0..MAX_CLIENTS as c_int).contains(&viewer) {
        return;
    }
    // Same board match the `scores` rewrite uses (follow-aware, and the
    // pre-intermission capture while the final board is built).
    let viewer_match = viewer_board_match(viewer);
    let (table, mut bases): ([u8; MAX_CLIENTS], [String; MAX_CLIENTS]) =
        state::with_state(|s| (s.tffa.client_match, s.tffa.cs_players_base.clone()));
    // Fall back to the live configstring for a slot whose base was assigned
    // while the feature was off (they are clientinfo sources too).
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
    for (slot, value) in desired_cs_players(viewer_match, &table, &bases) {
        push_cs_players(viewer, slot as usize, &value);
    }
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
fn desired_cs_players(
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
fn demotions_for_viewer(
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
fn reset_slot_stats(client_num: c_int) {
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
fn assign_client_instance(
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
fn reset_client_stats(clients: &mut [crate::state::ClientEntry], client_num: c_int) {
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
fn reset_game_scores(client_num: c_int) {
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
unsafe fn reset_game_scores_at(client: usize) {
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

/// Session team of a client from game memory (`None` when unknown).
pub fn session_team_of(client_num: c_int) -> Option<i32> {
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    session_team_at(g_entities, client_num)
}

/// Session team of a client, read through an already-located `g_entities`
/// base. Split from `session_team_of` so a caller that already holds the state
/// lock (the end-of-match roster capture) can read it without re-entering it.
fn session_team_at(g_entities: usize, client_num: c_int) -> Option<i32> {
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

/// `TeamCount` scoped to one instance, for the game's auto team pick
/// (`PickTeam` → `TeamCount`, `g_client.c:1294-1295`) and `SetTeam`'s
/// `g_teamForceBalance` check (`g_cmds.c:723-724`): both pass the joining
/// client, so counting only that client's instance balances the roster of the
/// match being joined instead of the whole level (base `TeamCount` counts every
/// connected client, so an instance's teams could be lopsided while the global
/// counts looked even). `None` when the count must stay pristine (feature off,
/// world not located).
pub fn instance_team_count(ignore_client_num: c_int, team: c_int) -> Option<c_int> {
    if !enabled() {
        return None;
    }
    let (g_entities, matches) =
        state::with_state(|s| (s.located_game_data.g_entities, s.tffa.client_match));
    if g_entities == 0 {
        return None;
    }
    // A caller with a client slot (`team free` → `PickTeam(clientNum)`, and the
    // force-balance check itself) counts that client's instance; a caller
    // without one (`-1`: the connect auto-join and bot creation) means main, the
    // instance every unassigned client belongs to.
    let instance = if (0..MAX_CLIENTS as c_int).contains(&ignore_client_num) {
        matches[ignore_client_num as usize]
    } else {
        MAIN_MATCH
    };
    // The roster is read from game memory like the pristine body
    // (`g_client.c:1240-1266`: `level.clients[i]`, reachable through
    // `g_entities[i].client`), then censused by the pure helper.
    let mut rows = [RosterRow::default(); MAX_CLIENTS];
    for (i, row) in rows.iter_mut().enumerate() {
        let ent = g_entities + i * SIZEOF_GENTITY;
        // SAFETY: ent is inside the g_entities array.
        let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
        if client == 0 {
            continue;
        }
        // SAFETY: client is a valid gclient_t; both fields are plain ints.
        let connected = unsafe {
            read_i32(client + OFFSET_GCLIENT_PERS + OFFSET_PERS_CONNECTED) != CON_DISCONNECTED
        };
        // SAFETY: as above.
        let team_of = unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) };
        *row = RosterRow {
            connected,
            team: team_of,
            instance: matches[i],
        };
    }
    Some(count_instance_team(
        ignore_client_num,
        instance,
        &rows,
        team,
    ))
}

/// One roster entry: whether the slot is connected, its session team and its
/// proxy instance. `default()` is a disconnected main slot (its team is never
/// read).
#[derive(Clone, Copy)]
struct RosterRow {
    connected: bool,
    team: i32,
    instance: u8,
}

impl Default for RosterRow {
    fn default() -> Self {
        Self {
            connected: false,
            team: TEAM_SPECTATOR, // any value; `connected = false` skips it
            instance: MAIN_MATCH,
        }
    }
}

/// Live `(red, blue)` playing-client counts per match slot, indexed by match
/// id (`0` = main). Spectators and disconnected clients count for
/// nothing, and inactive slots stay `(0, 0)`, so the per-team-size rules
/// (`crate::rules`) can ignore a match nobody is playing in. Every slot is
/// reported, not only the populated ones, so the caller decides what an empty
/// match means. The whole table comes from one pass over the roster.
pub fn match_team_counts() -> [(i32, i32); TOTAL_MATCH_SLOTS] {
    let (g_entities, matches, active) = state::with_state(|s| {
        (
            s.located_game_data.g_entities,
            s.tffa.client_match,
            core::array::from_fn(|i| s.tffa.matches[i].active),
        )
    });
    if g_entities == 0 {
        return [(0, 0); TOTAL_MATCH_SLOTS];
    }
    let mut rows = [RosterRow::default(); MAX_CLIENTS];
    for (i, row) in rows.iter_mut().enumerate() {
        let ent = g_entities + i * SIZEOF_GENTITY;
        // SAFETY: ent is inside the g_entities array.
        let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
        if client == 0 {
            continue;
        }
        // SAFETY: client is a valid gclient_t; both fields are plain ints.
        let connected = unsafe {
            read_i32(client + OFFSET_GCLIENT_PERS + OFFSET_PERS_CONNECTED) != CON_DISCONNECTED
        };
        // SAFETY: as above.
        let team = unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) };
        *row = RosterRow {
            connected,
            team,
            instance: matches[i],
        };
    }
    count_match_teams(&rows, &active)
}

/// The census itself (pure, so a test can fabricate the roster): connected
/// clients only, RED/BLUE only, grouped by match id. Spectators (`team` neither
/// RED nor BLUE) and inactive slots are not counted.
fn count_match_teams(
    rows: &[RosterRow; MAX_CLIENTS],
    active: &[bool; TOTAL_MATCH_SLOTS],
) -> [(i32, i32); TOTAL_MATCH_SLOTS] {
    let mut counts = [(0, 0); TOTAL_MATCH_SLOTS];
    for row in rows {
        if !row.connected {
            continue;
        }
        let id = row.instance as usize;
        if id >= TOTAL_MATCH_SLOTS || !active[id] {
            continue;
        }
        match row.team {
            TEAM_RED => counts[id].0 += 1,
            TEAM_BLUE => counts[id].1 += 1,
            _ => {}
        }
    }
    counts
}

/// The census itself (pure, so a test can fabricate the roster): the pristine
/// `TeamCount`'s filters — skip the ignored client, count clients that are
/// connected, on `team`, and in `instance`. TFFA is `GT_TEAM`, so the siege
/// `siegeDesiredTeam` arm of the pristine loop cannot apply.
fn count_instance_team(
    ignore_client_num: c_int,
    instance: u8,
    rows: &[RosterRow; MAX_CLIENTS],
    team: i32,
) -> c_int {
    let mut count = 0;
    for (i, row) in rows.iter().enumerate() {
        if i as c_int == ignore_client_num {
            continue;
        }
        if !row.connected || row.instance != instance || row.team != team {
            continue;
        }
        count += 1;
    }
    count
}

// ---------------------------------------------------------------------------
// AddScore accounting (called from the game wrapper after the pristine body)
// ---------------------------------------------------------------------------

/// Attribute `score` to the scorer's match (team tally + per-player copy)
/// and neutralise the global `level.teamScores` slot the pristine `AddScore`
/// just bumped, so the global fraglimit never fires early. No-op when the
/// feature is off.
pub fn on_add_score(ent_client_num: c_int, team: c_int, score: c_int) {
    if !enabled() || score == 0 {
        return;
    }
    if team != TEAM_RED && team != TEAM_BLUE {
        return;
    }
    // Pristine `SetTeam` suicides run under `g_dontPenalizeTeam`: the pristine
    // body skips the global bump by design, so any proxy accounting would
    // invent phantom points (subtract a point nobody added).
    if crate::original::dont_penalize_team() {
        return;
    }
    let mid = client_match(ent_client_num);
    state::with_state(|s| {
        if let Some(m) = s.tffa.matches.get_mut(mid as usize) {
            if team == TEAM_RED {
                m.red += score;
            } else {
                m.blue += score;
            }
        }
        if (0..MAX_CLIENTS as c_int).contains(&ent_client_num) {
            s.tffa.player_score[ent_client_num as usize] += score;
        }
    });
    neutralise_global(team, score);
    // The pristine body already ran `CalculateRanks` (which broadcast
    // `CS_SCORES1/2` and triggered per-viewer corrections with the *stale*
    // pre-frag tallies). Push fresh corrections now that the proxy tallies
    // include this frag, otherwise the mini HUD lags one frag behind.
    on_set_cs_scores();
}

/// Subtract `score` back from the global `level.teamScores` slot.
fn neutralise_global(team: c_int, score: c_int) {
    let level = crate::original::level_address();
    if level == 0 {
        return;
    }
    let slot = if team == TEAM_RED {
        level + crate::sdk::OFFSET_LEVEL_TEAM_SCORES_RED
    } else {
        level + crate::sdk::OFFSET_LEVEL_TEAM_SCORES_BLUE
    };
    // SAFETY: level is the game's level_locals_t; teamScores are plain ints.
    unsafe {
        let p = slot as *mut c_int;
        *p = (*p).wrapping_sub(score);
    }
}

/// Zero stale global points at frame start (before the game frame runs its
/// own `CheckExitRules`): only hooked `AddScore` bumps are neutralised
/// synchronously, so anything left over is a leak from an unhooked writer and
/// must not end the map spuriously.
fn clamp_global_scores() {
    let level = crate::original::level_address();
    if level == 0 {
        return;
    }
    // SAFETY: teamScores are plain ints in the game's level_locals_t.
    unsafe {
        let red = (level + crate::sdk::OFFSET_LEVEL_TEAM_SCORES_RED) as *mut c_int;
        let blue = (level + crate::sdk::OFFSET_LEVEL_TEAM_SCORES_BLUE) as *mut c_int;
        let (r, b) = (
            core::ptr::read_unaligned(red as *const c_int),
            core::ptr::read_unaligned(blue as *const c_int),
        );
        if r != 0 || b != 0 {
            core::ptr::write_unaligned(red, 0);
            core::ptr::write_unaligned(blue, 0);
        }
    }
}

// ---------------------------------------------------------------------------
// vmMain bookkeeping
// ---------------------------------------------------------------------------

/// `GAME_INIT`: reset the round — match tallies and per-player copies go to
/// zero, the main slot reopens.
///
/// `restart` is the `GAME_INIT` restart flag. A `map_restart` (`qtrue`) keeps
/// the overlay `state::reset_session` carried — which created matches were open
/// and which instance each client was in — so the players land back in their
/// matches; a full map change (`qfalse`) closes every created match and starts
/// from main, exactly like a fresh module load.
///
/// Scores, per-player copies, cs caches and the per-instance worlds are never
/// carried: they are per round / per map. The world bookkeeping is cleared
/// unconditionally because the pristine `GAME_INIT` memset the entity array
/// and respawned the map, so no clone and no tag survived it; the clones are
/// rebuilt for the carried instances by `restore_instances` once the hook layer
/// is attached, and `scan_world_entities` re-measures the map in between.
pub fn reset_round(restart: bool) {
    // `enabled()` and `max_matches()` take the state lock themselves: hoist
    // them (the lock is non-reentrant).
    let keep = restart && state::instances_enabled();
    let max = max_matches();
    let mut kept = false;
    state::with_state(|s| {
        kept = reset_round_state(&mut s.tffa, keep, max);
        // Re-latch the enable gate: an admin who set `proxy_tffa_enable 0`
        // before the restart must not see a first-frame disable sweep for
        // matches `reset_round_state` already closed.
        s.tffa.enable_latched = s.cvars[CVAR_TFFA_ENABLE].integer != 0;
        // Per-instance worlds are per map (see above).
        s.tffa.entity_instance = [ENTITY_SHARED; MAX_GENTITIES];
        s.tffa.world_clone_cost = 0;
        s.tffa.world_clone_blocked = false;
        s.tffa.world_release = 0;
        s.tffa.world_cloned = 0;
        s.tffa.world_scanned = false;
        // Only a carried overlay needs the settle window: the engine runs its
        // frames before reconnecting the retained clients, and every preserved
        // created match would look member-less during them.
        s.tffa.restart_grace_frames = if kept { RESTART_SETTLE_FRAMES } else { 0 };
    });
    if kept {
        eprintln!(
            "----- proxy-rs: parallel TFFA kept across map_restart — instances are rebuilt once the hooks are up"
        );
    }
}

/// The match-table half of `reset_round`, split out so the carry rule is
/// unit-tested without an engine.
///
/// `keep` retains the overlay carried by `state::reset_session`; `max` is the
/// live `proxy_tffa_maxMatches` (an admin may have lowered it before the
/// restart). Retained matches keep no tallies.
///
/// Every client returns to its **home id** (`TffaState::home`): a game promoted
/// into the main slot was runtime-assigned to main, but its TFFA id never
/// changed, so TFFA 0 stays TFFA 0 and TFFA 1 stays TFFA 1 across the restart
/// (D-006.11). A created id that was open, finished (remembered roster) or
/// promoted is reactivated; its remembered members are assigned back to it and
/// their old team is scheduled in `pending_join` (D-006.10). Main stays open and
/// consumes its roster the same way. Returns whether any created match
/// survived, i.e. whether the post-restart grace window is needed.
fn reset_round_state(t: &mut state::TffaState, keep: bool, max: usize) -> bool {
    // Auto-joins are scheduled for the round being initialised only, and no
    // deferred begin/refresh may survive the round that queued it (D-006.12).
    t.pending_join = [REMEMBERED_NONE; MAX_CLIENTS];
    t.deferred_begin = [false; MAX_CLIENTS];
    t.pending_view_refresh = [false; MAX_CLIENTS];
    // Main hosts its own id again: the promoted game goes back to its slot.
    t.main_origin = MAIN_MATCH;
    if keep {
        // The runtime assignment is a slot, the home is the identity: undo every
        // promotion (and every finished match's return to main) here.
        for (cn, home) in t.home.iter().copied().enumerate() {
            t.client_match[cn] = home;
        }
    } else {
        // A full map change drops identities with the rest of the overlay.
        t.home = [MAIN_MATCH; MAX_CLIENTS];
        t.pass_ok = [MAIN_MATCH; MAX_CLIENTS];
    }
    for (i, m) in t.matches.iter_mut().enumerate() {
        m.red = 0;
        m.blue = 0;
        // A new round starts from a fresh score: no match carries a pending
        // end-of-match delay across the restart.
        m.end_queued_at = 0;
        // A promoted game's slot is inactive at runtime but its identity still
        // has clients (assigned to main); it must come back as its own match.
        let has_home_members = t.home.iter().any(|home| *home as usize == i);
        if i == MAIN_MATCH as usize {
            m.active = true;
            // Main consumes its identity's roster: the players a finished main
            // game sent to spectator come back on their teams (D-006.11).
            if keep {
                for (cn, team) in m.remembered.iter().copied().enumerate() {
                    if team == REMEMBERED_NONE {
                        continue;
                    }
                    t.client_match[cn] = MAIN_MATCH;
                    t.home[cn] = MAIN_MATCH;
                    t.pending_join[cn] = team;
                }
            }
            m.remembered = [REMEMBERED_NONE; MAX_CLIENTS];
            // Main hosts its own id again and is never password protected (a
            // promoted game keeps its password under its own identity slot).
            m.clear_password();
            continue;
        }
        // A created id survives when its match was open, when it finished
        // (remembered roster) or when its game lives on in the main slot after a
        // promotion (home members) — all inside the live limit.
        let survives = keep && i <= max && (m.active || m.has_remembered() || has_home_members);
        if survives {
            m.active = true;
            for (cn, team) in m.remembered.iter().copied().enumerate() {
                if team == REMEMBERED_NONE {
                    continue;
                }
                t.client_match[cn] = i as u8;
                t.home[cn] = i as u8;
                t.pending_join[cn] = team;
            }
            m.remembered = [REMEMBERED_NONE; MAX_CLIENTS];
            continue;
        }
        // The id is gone: its clients cannot stay in it, they start in main
        // (their game-side team is restored separately by the base game's
        // session data on reconnect). Its remembered roster dies with it.
        m.active = false;
        m.remembered = [REMEMBERED_NONE; MAX_CLIENTS];
        // The id is gone: its protection and everyone's authorisation to it die
        // with it, so a later match in this slot starts from its own password.
        m.clear_password();
        let id = i as u8;
        // A promoted id above the lowered limit has its clients assigned to
        // main, not to its slot, so both tables are checked here.
        for cn in 0..MAX_CLIENTS {
            if t.home[cn] == id {
                t.home[cn] = MAIN_MATCH;
            }
            if t.client_match[cn] == id {
                t.client_match[cn] = MAIN_MATCH;
            }
            if t.pass_ok[cn] == id {
                t.pass_ok[cn] = MAIN_MATCH;
            }
        }
    }
    t.player_score = [0; MAX_CLIENTS];
    t.owned_count = 0;
    t.rebuilt_stamp = 0;
    // The bases mirror the game's last `CS_PLAYERS` broadcasts; the new round
    // re-sends them as each client connects (and `send_all_overrides_to_client`
    // falls back to the live configstring meanwhile), so the old round's copies
    // must not be pushed as if they were current.
    t.cs_players_base = core::array::from_fn(|_| String::new());
    // The record of what each viewer holds is about the old gamestates: every
    // client reconnects into a fresh one after the restart.
    for viewer in t.cs_players_sent.iter_mut() {
        for slot in viewer.iter_mut() {
            slot.clear();
        }
    }
    // The reconnect's gamestate sets this again per client; until then a full
    // re-push must not assume the base strings are held.
    t.view_bases_from_gamestate = [false; MAX_CLIENTS];
    // Re-arm the score dedup so round-start 0-0 is pushed.
    t.cs_scores_sent = [(i32::MIN, i32::MIN); MAX_CLIENTS];
    // New round: fraglimit handling runs again.
    t.intermission = false;
    t.intermission_boards = [MAIN_MATCH; MAX_CLIENTS];
    t.matches
        .iter()
        .enumerate()
        .any(|(i, m)| i != MAIN_MATCH as usize && m.active)
}

/// The created matches `restore_instances` must rebuild after a carry: every
/// active one inside the live `proxy_tffa_maxMatches`. Ids above the limit were
/// already closed (and their members returned to main) by `reset_round_state`.
/// Pure so the selection is unit-tested.
fn restorable_instances(matches: &[state::TffaMatch; TOTAL_MATCH_SLOTS], max: usize) -> Vec<u8> {
    (1..=max.min(TOTAL_MATCH_SLOTS - 1))
        .filter(|&i| matches[i].active)
        .map(|i| i as u8)
        .collect()
}

/// `GAME_INIT` post-pass, after the hook layer is attached: give every carried
/// created match its per-instance world back, so the instance a player returns
/// to is the same isolated place it was before the `map_restart` (door, plats,
/// triggers).
///
/// All-or-nothing per instance, as at creation: a match whose set cannot be
/// rebuilt is closed and its members return to main (they still reconnect into
/// the overlays that did come back).
///
/// Requires the `G_Spawn` detour and `G_LINKENTITY` trampoline, i.e. the hook
/// layer attached (`hooks::set_enabled` at the end of `GAME_INIT`), and the
/// pristine module loaded.
pub fn restore_instances() {
    if !state::instances_enabled()
        || crate::original::base() == 0
        || !crate::hooks::engine_sv::attached()
    {
        return;
    }
    let max = max_matches();
    let ids = state::with_state(|s| restorable_instances(&s.tffa.matches, max));
    if ids.is_empty() {
        return;
    }
    let mut restored = 0usize;
    let mut closed = 0usize;
    for id in ids {
        match realize_instance(id) {
            Ok(()) => restored += 1,
            Err(reason) => {
                closed += 1;
                eprintln!(
                    "----- proxy-rs: instance {id} could not be rebuilt after map_restart ({reason}) — closing it, its members return to main"
                );
                discard_instance(id);
            }
        }
        // The players have not reconnected yet (`GAME_CLIENT_CONNECT` comes
        // after the settle frames), so there is no view or team to refresh.
    }
    eprintln!(
        "----- proxy-rs: parallel TFFA after map_restart: {restored} instance(s) rebuilt, {closed} closed"
    );
}

/// Close one carried instance whose world could not be rebuilt: reset its slot
/// and return its members to main, without the broadcasts `close_instance`
/// makes (no client is connected yet at this point).
fn discard_instance(id: u8) {
    if id == MAIN_MATCH || id as usize >= TOTAL_MATCH_SLOTS {
        return;
    }
    crate::teamlock::clear_match(id);
    state::with_state(|s| {
        // Nothing of this instance is live (a failed rebuild unwinds its
        // clones), but a stale piece of the old round's bookkeeping must not
        // keep a tag alive.
        s.tffa.world_release |= 1u16 << id;
        let moved = close_instance_state(
            &mut s.tffa.matches,
            &mut s.tffa.client_match,
            &mut s.tffa.home,
            &mut s.tffa.pass_ok,
            &mut s.tffa.player_score,
            id,
        );
        // The members return to main: they must reconnect there as spectators,
        // not be auto-joined to the team the lost instance remembered.
        for cn in moved {
            if let Some(pending) = s.tffa.pending_join.get_mut(cn as usize) {
                *pending = REMEMBERED_NONE;
            }
        }
    });
}
/// Clear all TFFA state for a fresh first-time connect (the slot may have
/// been used by a different player before).
pub fn on_client_connect(client_num: c_int, first_time: bool) {
    // The reconnect burst that follows a `map_restart` runs after the engine's
    // settle frames (`SV_MapRestart_f`, `sv_ccmds.cpp:306-349`); its first
    // connect ends the grace window, so the next frame sweeps empty matches
    // normally. Cleared for a first-time connect too: a brand-new player on a
    // carried server means the burst is over (or never had anyone to bring
    // back).
    state::with_state(|s| s.tffa.restart_grace_frames = 0);
    if !first_time || !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    // A fresh client in this slot is not the previous occupant: no finished
    // instance may remember them, and no pending auto-join survives.
    forget_client_everywhere(client_num);
    state::with_state(|s| {
        s.tffa.client_match[client_num as usize] = MAIN_MATCH;
        s.tffa.player_score[client_num as usize] = 0;
        s.tffa.cs_players_base[client_num as usize].clear();
        for slot in s.tffa.cs_players_sent[client_num as usize].iter_mut() {
            slot.clear();
        }
        s.tffa.cs_scores_sent[client_num as usize] = (i32::MIN, i32::MIN);
        s.tffa.viewer_match_cache[client_num as usize] = MAIN_MATCH;
        s.tffa.intermission_boards[client_num as usize] = MAIN_MATCH;
        s.tffa.view_bases_from_gamestate[client_num as usize] = false;
        s.tffa.deferred_begin[client_num as usize] = false;
        s.tffa.pending_view_refresh[client_num as usize] = false;
    });
}

/// Clear all TFFA state for a disconnecting client.
pub fn on_client_disconnect(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    forget_client_everywhere(client_num);
    state::with_state(|s| {
        s.tffa.client_match[client_num as usize] = MAIN_MATCH;
        s.tffa.player_score[client_num as usize] = 0;
        s.tffa.cs_players_base[client_num as usize].clear();
        for slot in s.tffa.cs_players_sent[client_num as usize].iter_mut() {
            slot.clear();
        }
        s.tffa.cs_scores_sent[client_num as usize] = (i32::MIN, i32::MIN);
        s.tffa.viewer_match_cache[client_num as usize] = MAIN_MATCH;
        s.tffa.intermission_boards[client_num as usize] = MAIN_MATCH;
        s.tffa.view_bases_from_gamestate[client_num as usize] = false;
        s.tffa.deferred_begin[client_num as usize] = false;
        s.tffa.pending_view_refresh[client_num as usize] = false;
    });
    // The slot is free: other viewers' stale prefixed views of it will be
    // refreshed when the game clears `CS_PLAYERS + n` (empty base). Push an
    // immediate restore so a stale `(TFFA N)` does not linger.
    if state::instances_enabled() {
        let viewers: Vec<c_int> = state::with_state(|s| {
            (0..MAX_CLIENTS as c_int)
                .filter(|v| s.clients[*v as usize].is_connected)
                .collect()
        });
        for viewer in viewers {
            push_cs_players(viewer, client_num as usize, "");
        }
    }
}

/// Push the current per-viewer overrides to a client that just began
/// (it received the server gamestate with base strings; these corrections
/// apply its match's scores and prefixed outsiders).
///
/// A client a `map_restart` re-created a finished instance for also gets
/// auto-joined to the team it played for before the match ended (D-006.10);
/// that runs first so the overrides below are built from the final team and
/// instance.
pub fn on_client_begin(client_num: c_int) {
    if !state::instances_enabled() {
        return;
    }
    // The `map_restart` reconnect burst queues the game's own per-client
    // broadcasts for every retained client before anyone acknowledges them
    // (`SV_MapRestart_f`, `sv_ccmds.cpp:306-349`). Running the auto-join (a
    // pristine `SetTeam` → `ClientUserinfoChanged`/`ClientBegin`, three more
    // broadcasts to *every* client) and the override push here on top of that
    // burst is what overflowed the 128-slot reliable window with a populated
    // server, dropped clients and made real clients print
    // `CL_GetServerCommand: a reliable command was cycled out` (D-006.12).
    // Defer both to the first frame after the burst; the client is already
    // active (the pristine `ClientBegin` ran), so a frame's delay is invisible.
    let (burst, pressured) = (
        state::with_state(|s| s.tffa.restart_burst),
        reliable_pressure(client_num),
    );
    if burst || pressured {
        if (0..MAX_CLIENTS as c_int).contains(&client_num) {
            state::with_state(|s| {
                s.tffa.deferred_begin[client_num as usize] = true;
                if s.tffa.restart_burst {
                    s.tffa.restart_burst_connects = true;
                }
            });
        }
        return;
    }
    restore_pending_join(client_num);
    send_all_overrides_to_client(client_num);
}

/// `GAME_RUN_FRAME` calls the engine runs after `GAME_INIT` before the first
/// real frame of a `map_restart`: three settle frames, then the client
/// reconnect loop, then one trailing settle frame (`SV_MapRestart_f`,
/// `sv_ccmds.cpp:306-349`). Used as the bounded fallback that ends the burst
/// even if the trailing frame never arrives (a modified engine).
const RESTART_BURST_MAX_FRAMES: u8 = 8;

/// Advance the `map_restart` burst state machine once per `GAME_RUN_FRAME`
/// (called from [`on_frame_start`], before the frame is forwarded).
///
/// `SV_MapRestart_f` reconnects every retained client in one engine frame and
/// then runs one trailing `GAME_RUN_FRAME` in the *same* engine frame, before
/// any client acknowledgement is read. The deferred work may therefore only
/// start on the `GAME_RUN_FRAME` after that trailing one. The sequence the
/// proxy observes is: settle frames (no `GAME_CLIENT_BEGIN`), the reconnect
/// loop (begins, no frame), the trailing frame (the first frame after a begin),
/// then the first real frame.
fn advance_restart_burst() {
    state::with_state(|s| advance_restart_burst_state(&mut s.tffa));
}

/// The state-machine half of [`advance_restart_burst`], split out so the
/// sequence is unit-tested without an engine.
fn advance_restart_burst_state(t: &mut state::TffaState) {
    if !t.restart_burst {
        return;
    }
    t.restart_burst_frames = t.restart_burst_frames.saturating_add(1);
    if t.restart_burst_connects && !t.restart_burst_trailing {
        // The engine's trailing settle frame: clients are reconnected but
        // nobody has acknowledged the burst yet.
        t.restart_burst_trailing = true;
        return;
    }
    let trailing_seen = t.restart_burst_trailing;
    let bounded = t.restart_burst_frames >= RESTART_BURST_MAX_FRAMES;
    if trailing_seen || bounded {
        // The burst is over. The per-client reliable-window check still keeps
        // the deferred work off until each client's acknowledgement of the
        // burst has been read (`Com_EventLoop` runs before the next frame's
        // `GAME_RUN_FRAME`), so the first real frame may safely start draining.
        t.restart_burst = false;
        t.restart_burst_connects = false;
        t.restart_burst_trailing = false;
        t.restart_burst_frames = 0;
    }
}

/// Frames' worth of deferred `GAME_CLIENT_BEGIN` work: how many clients may be
/// auto-joined and refreshed per frame once the burst has been acknowledged.
/// Each auto-join makes the pristine game broadcast to every client, so a
/// whole-server burst would recreate the same overflow one frame later; a
/// small batch keeps each frame's addition bounded while every client is
/// served within a few frames (D-006.12).
const DEFERRED_BEGINS_PER_FRAME: usize = 4;

/// Run the `GAME_CLIENT_BEGIN` work a full reliable window deferred, plus any
/// `cs` correction dropped by [`send_cs_to_client`], for the clients whose
/// window now has room. Called once per game frame before the early returns in
/// [`on_run_frame_post`], so a pending auto-join/refresh is never starved by
/// the intermission or settle-window gates.
fn drain_deferred_client_work() {
    if state::with_state(|s| s.tffa.restart_burst) {
        return;
    }
    let candidates: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|v| {
                let i = *v as usize;
                s.clients[i].is_connected
                    && (s.tffa.deferred_begin[i] || s.tffa.pending_view_refresh[i])
            })
            .collect()
    });
    let mut budget = DEFERRED_BEGINS_PER_FRAME;
    for viewer in candidates {
        if budget == 0 || reliable_pressure(viewer) {
            continue;
        }
        let begin = state::with_state(|s| {
            core::mem::replace(&mut s.tffa.deferred_begin[viewer as usize], false)
        });
        // The refresh flag is cleared before the push so a correction that
        // trips the limit again re-arms it from `send_cs_to_client`.
        state::with_state(|s| s.tffa.pending_view_refresh[viewer as usize] = false);
        if begin {
            restore_pending_join(viewer);
        }
        send_all_overrides_to_client(viewer);
        budget -= 1;
    }
}

/// Bump the frame stamp so the owned-entity cache is rebuilt once per game
/// frame (called from `GAME_RUN_FRAME` before the frame is forwarded).
pub fn on_frame_start() {
    state::with_state(|s| s.tffa.frame_stamp = s.tffa.frame_stamp.wrapping_add(1));
    // Track the `map_restart` reconnect burst so the deferred per-viewer work
    // only starts on the first frame whose acknowledgements have been read
    // (D-006.12).
    advance_restart_burst();
    // `proxy_tffa_enable` turned off since the last frame (the cvar mirror was
    // just refreshed): dissolve every created match before the game runs, so
    // no frame plays with the isolation gates off while players are still
    // split across instances.
    let turned_off = state::with_state(|s| {
        let now_on = s.cvars[CVAR_TFFA_ENABLE].integer != 0;
        enable_turned_off(&mut s.tffa.enable_latched, now_on)
    });
    if turned_off {
        disable_parallel_tffa();
    }
    // Closed instances give their clones back here, between game frames. Not
    // gated on `enabled()`: a disable queues its releases a moment before this
    // and `enabled()` is already false by then.
    drain_world_releases();
    if state::instances_enabled() {
        clamp_global_scores();
        // Defensive: a spawn recorded by a call that never reached its close
        // (nothing should) is classified rather than carried into the next map.
        flush_pending_spawns();
    }
}

/// Remember this frame's `proxy_tffa_enable` value and report a non-zero → zero
/// edge (an admin turned parallel TFFA off). Pure so the edge is unit-tested.
fn enable_turned_off(latched: &mut bool, now_on: bool) -> bool {
    let off = *latched && !now_on;
    *latched = now_on;
    off
}

/// The created matches an admin disable closes: every active one. Main (id 0)
/// is implicit and stays. Pure so the selection is unit-tested.
fn created_matches_to_close(matches: &[state::TffaMatch; TOTAL_MATCH_SLOTS]) -> Vec<u8> {
    matches
        .iter()
        .enumerate()
        .filter(|(i, m)| *i != MAIN_MATCH as usize && m.active)
        .map(|(i, _)| i as u8)
        .collect()
}

/// `proxy_tffa_enable` was turned off: hand the main game's score back to the
/// game, close every created match, and return its members to the main game as
/// spectators, telling each of them what happened. Main (id 0) stays — it is
/// the pristine map's own game.
///
/// Runs at `GAME_RUN_FRAME` start, before the frame is forwarded: from this
/// frame on `tffa::enabled()` is false, so every isolation gate is open and an
/// instance left standing would be visible, solid and damageable from main.
/// The per-viewer configstring overrides of the last TFFA frame (prefixed
/// outsider names, forced-spectator teams, per-match HUD tallies) are undone at
/// the end, so every client is back on the server's own strings.
fn disable_parallel_tffa() {
    let created = state::with_state(|s| created_matches_to_close(&s.tffa.matches));
    if !created.is_empty() {
        close_created_matches(&created);
    }
    // Main keeps playing, so the score the game owns again is main's: while the
    // overlay ran, `level.teamScores` was held at `0` and main's tally lived
    // proxy-side (D-006.3). Restoring it keeps the running score instead of
    // dropping the map to 0-0, and it is what the per-viewer HUD push below
    // carries to every client (they are all in main from here on).
    let (red, blue) = restore_main_global_scores();
    restore_pristine_views(red, blue);
    state::with_state(|s| {
        // The stored bases and caches belong to the overlay that just ended: a
        // later re-enable falls back to the live server strings and starts with
        // a clean dedup.
        s.tffa.cs_players_base = core::array::from_fn(|_| String::new());
        for viewer in s.tffa.cs_players_sent.iter_mut() {
            for slot in viewer.iter_mut() {
                slot.clear();
            }
        }
        s.tffa.view_bases_from_gamestate = [false; MAX_CLIENTS];
        s.tffa.cs_scores_sent = [(i32::MIN, i32::MIN); MAX_CLIENTS];
        s.tffa.viewer_match_cache = [MAIN_MATCH; MAX_CLIENTS];
        s.tffa.intermission_boards = [MAIN_MATCH; MAX_CLIENTS];
        s.tffa.restart_grace_frames = 0;
        // The disable dissolves the overlay: a finished match's remembered
        // roster and every client's identity (D-006.10, D-006.11) must not bring
        // an instance back after a later re-enable. Main hosts its own id again.
        for m in s.tffa.matches.iter_mut() {
            m.remembered = [REMEMBERED_NONE; MAX_CLIENTS];
            m.clear_password();
        }
        s.tffa.home = [MAIN_MATCH; MAX_CLIENTS];
        s.tffa.pass_ok = [MAIN_MATCH; MAX_CLIENTS];
        s.tffa.main_origin = MAIN_MATCH;
        s.tffa.pending_join = [REMEMBERED_NONE; MAX_CLIENTS];
        s.tffa.deferred_begin = [false; MAX_CLIENTS];
        s.tffa.pending_view_refresh = [false; MAX_CLIENTS];
        // Main keeps playing: a pending end-of-match delay from before the
        // disable must not void its damage after a later re-enable.
        s.tffa.matches[MAIN_MATCH as usize].end_queued_at = 0;
        // Gameplay while the feature is off can spawn interactive entities no
        // tag covers, so a later re-enable must re-measure the map (the
        // measured tag set is what an instance clones).
        s.tffa.world_scanned = false;
    });
}

/// Close the `created` matches: announce each on the engine log, deactivate the
/// slots (queuing their per-instance worlds for release) and move their members
/// to the main game. A member who was playing is sent to the spectator team with
/// an explanation (see `disable_notice`); one who was already spectating is
/// returned to main with the shorter notice.
fn close_created_matches(created: &[u8]) {
    let (connected, playing) = client_census();
    // Members per match, taken before the close returns them to main.
    let groups: Vec<(u8, Vec<c_int>)> = state::with_state(|s| {
        created
            .iter()
            .map(|&id| {
                let members = s
                    .tffa
                    .client_match
                    .iter()
                    .enumerate()
                    .filter(|(i, m)| **m == id && connected[*i])
                    .map(|(i, _)| i as c_int)
                    .collect();
                (id, members)
            })
            .collect()
    });
    for (id, group) in &groups {
        let (red, blue) = state::with_state(|s| {
            let m = &s.tffa.matches[*id as usize];
            (m.red, m.blue)
        });
        eprintln!(
            "----- proxy-rs: parallel TFFA disabled — closed instance {id} ({red}-{blue}), {} member(s) sent to the main game",
            group.len()
        );
    }
    // The overlay goes first: from here on every proxy view of the server is the
    // main game's, and the per-viewer pushes below are the last TFFA-era writes
    // of this round.
    state::with_state(|s| {
        for (id, _) in &groups {
            s.tffa.world_release |= 1u16 << id;
            close_instance_state(
                &mut s.tffa.matches,
                &mut s.tffa.client_match,
                &mut s.tffa.home,
                &mut s.tffa.pass_ok,
                &mut s.tffa.player_score,
                *id,
            );
        }
    });
    // A closed instance's team lock goes with it, so a later match reusing the
    // slot does not inherit the old match's caps.
    for (id, _) in &groups {
        crate::teamlock::clear_match(*id);
    }
    // Spectator move and explanation. These call into the game (`SetTeam` →
    // `ClientUserinfoChanged` → `CS_PLAYERS`) and back into the proxy, so no
    // state lock may be held here.
    for (id, group) in &groups {
        for &cn in group {
            let was_playing = playing[cn as usize];
            if was_playing {
                // The same move as `/team spectator`: the player suicides and
                // respawns spectating. Refused only for a slot with no live
                // game entity, which has nothing to move.
                set_client_spectator(cn);
            }
            reset_slot_stats(cn);
            tell(cn, &disable_notice(*id, was_playing));
        }
    }
}

/// Hand the pristine team tallies back to the game with main's score, and
/// return that score. While parallel TFFA ran, `level.teamScores` was kept
/// neutralised (D-006.3) so the pristine `CheckExitRules` never fired early;
/// from the disable on the game owns them again, so `0-0` would read as a score
/// reset for the one remaining (main) game.
///
/// Safe with respect to `CheckExitRules`: main's tally is the value the proxy
/// itself compared against the shared `fraglimit` every frame, so a main game
/// that had reached the limit was already ended/promoted before a disable can
/// be observed at the next frame start.
fn restore_main_global_scores() -> (i32, i32) {
    let (red, blue) = state::with_state(|s| {
        let m = &s.tffa.matches[MAIN_MATCH as usize];
        (m.red, m.blue)
    });
    let level = crate::original::level_address();
    if level != 0 {
        // SAFETY: `level` is the game's `level_locals_t` (0 checked above).
        unsafe { write_global_team_scores(level, red, blue) };
    }
    (red, blue)
}

/// Write main's tally into the game's `level.teamScores` slots (the same fields
/// `neutralise_global` subtracts from). Split out from
/// `restore_main_global_scores` so the write is unit-tested on a scratch
/// buffer, and so it is obvious the disable restores *main's* score instead of
/// leaving the neutralised `0-0`.
///
/// # Safety
/// `level` must point to the game's live `level_locals_t`.
unsafe fn write_global_team_scores(level: usize, red: i32, blue: i32) {
    unsafe {
        core::ptr::write_unaligned(
            (level + crate::sdk::OFFSET_LEVEL_TEAM_SCORES_RED) as *mut c_int,
            red,
        );
        core::ptr::write_unaligned(
            (level + crate::sdk::OFFSET_LEVEL_TEAM_SCORES_BLUE) as *mut c_int,
            blue,
        );
    }
}

/// `print` text explaining a disable to one member of a closed match: the
/// players moved out of it are told they are spectators now and how to play
/// again, a member who was already spectating only that the match is gone.
fn disable_notice(match_id: u8, was_playing: bool) -> String {
    if was_playing {
        format!(
            "print \"Parallel TFFA was disabled by the server: match {match_id} was closed and you have been moved to spectator. Use /team red or /team blue to rejoin the main game.\n\""
        )
    } else {
        format!(
            "print \"Parallel TFFA was disabled by the server: match {match_id} was closed. You are now watching the main game.\n\""
        )
    }
}

/// Undo the per-viewer configstring overrides after a disable: every connected
/// client gets the server's own `CS_PLAYERS + n` strings and the `(red, blue)`
/// tallies the game owns again (main's), so no `(TFFA n)` prefix,
/// forced-spectator outsider or per-match HUD tally outlives the feature.
///
/// The engine's own `CS_SCORES1/2` configstrings are not touched: the next
/// pristine `CalculateRanks` (any frag, connect, team change) rewrites them from
/// the `level.teamScores` value just restored, and a client that connects
/// meanwhile gets them from that same call before its gamestate is sent.
fn restore_pristine_views(red: i32, blue: i32) {
    let connected = connected_clients();
    let viewers: Vec<c_int> = (0..MAX_CLIENTS as c_int)
        .filter(|v| connected[*v as usize])
        .collect();
    if viewers.is_empty() {
        return;
    }
    let bases: [Option<String>; MAX_CLIENTS] =
        core::array::from_fn(|slot| get_live_cs(CS_PLAYERS + slot as i32));
    let red = red.to_string();
    let blue = blue.to_string();
    for viewer in viewers {
        for (slot, base) in bases.iter().enumerate() {
            if let Some(base) = base {
                send_cs_to_client(viewer, CS_PLAYERS + slot as i32, base);
            }
        }
        send_cs_to_client(viewer, CS_SCORES1, &red);
        send_cs_to_client(viewer, CS_SCORES2, &blue);
    }
}

/// Whether the shared timelimit has expired, mirroring pristine
/// `CheckExitRules` (`g_main.c:2664-2677`): `timelimit > 0`, not warmup, and
/// `level.time - level.startTime >= timelimit * 60000`.
fn timelimit_expired() -> bool {
    // SAFETY: engine syscall registered.
    let minutes = unsafe { crate::syscall::cvar_variable_integer_value(c"timelimit") };
    if minutes <= 0 {
        return false;
    }
    let level = crate::original::level_address();
    if level == 0 {
        return false;
    }
    // SAFETY: level is the game's level_locals_t; all three fields are ints.
    let (time, start, warmup) = unsafe {
        (
            read_i32(level + crate::sdk::OFFSET_LEVEL_TIME),
            read_i32(level + crate::sdk::OFFSET_LEVEL_START_TIME),
            read_i32(level + crate::sdk::OFFSET_LEVEL_WARMUP_TIME),
        )
    };
    if warmup != 0 {
        return false;
    }
    // i64: a large admin `timelimit` must not wrap the multiplication.
    i64::from(time) - i64::from(start) >= i64::from(minutes) * 60_000
}

/// `level.time` in milliseconds (0 when the level block is not located yet).
fn level_time() -> i32 {
    let level = crate::original::level_address();
    if level == 0 {
        return 0;
    }
    // SAFETY: level is the game's `level_locals_t`; `time` is an int.
    unsafe { read_i32(level + crate::sdk::OFFSET_LEVEL_TIME) }
}

/// Pristine `INTERMISSION_DELAY_TIME` (`g_local.h:41`): after `LogExit` queues
/// the end of the game, `CheckExitRules` keeps forwarding frames for one
/// second before `BeginIntermission` runs. The shipped binary compares against
/// `0x3e8` (`0x88b8c`). During that window pristine `G_Damage` returns early
/// ("don't allow any extra scoring", `g_combat.c:4520-4524`), which is the
/// "damage is void before the game ends" behaviour a per-match TFFA end keeps.
pub const END_DELAY_MS: i32 = 1000;

/// One frame's outcome decision for an active match: `Some(reason)` when the
/// match's end must be applied **this** frame, `None` while it is still
/// playing or still inside the end-of-match delay.
///
/// The first frame a limit is hit records `end_queued_at` (the pristine
/// `LogExit` → `BeginIntermission` delay). Later frames keep returning `None`
/// until [`END_DELAY_MS`] elapsed; the match then stays in the proxy tables
/// (and its scoreboard) until the caller finalises it. A limit that no longer
/// applies (an admin raised `fraglimit` inside the window) cancels the pending
/// end. Pure so the delay is unit-tested.
fn queued_finish(
    m: &mut state::TffaMatch,
    reason: Option<&'static str>,
    now: i32,
) -> Option<&'static str> {
    let Some(reason) = reason else {
        m.end_queued_at = 0;
        return None;
    };
    if m.end_queued_at == 0 {
        // `level.time` is far past 0 in play; the clamp only keeps 0 meaning
        // "not queued" on a fabricated/rewound clock.
        m.end_queued_at = now.max(1);
        return None;
    }
    (now - m.end_queued_at >= END_DELAY_MS).then_some(reason)
}

/// Any active match whose outcome is decided but whose end has not been
/// applied yet. The map must not run its final intermission while one is
/// pending (the matches still owe their one-second end window).
fn any_match_ending(matches: &[state::TffaMatch]) -> bool {
    matches.iter().any(|m| m.ending())
}

/// Why an active match ends this frame: the shared fraglimit, or the shared
/// timelimit once it expired **and the match is no longer tied**. A tied match
/// keeps playing (per-match sudden death); it ends as soon as a frag makes the
/// teams differ. `None` means it continues.
fn match_finish_reason(
    red: i32,
    blue: i32,
    fraglimit: i32,
    timelimit_expired: bool,
) -> Option<&'static str> {
    if fraglimit_hit(red, blue, fraglimit) {
        Some("fraglimit")
    } else if timelimit_expired && red != blue {
        Some("timelimit")
    } else {
        None
    }
}

/// Whether any active match still needs the map: tied **and** with at least
/// one playing client (a tied match left with spectators only can never
/// untie). Once this is false after the timelimit, every game is over and the
/// map restarts normally.
fn has_continuing_match(
    matches: &[state::TffaMatch; TOTAL_MATCH_SLOTS],
    client_match: &[u8; MAX_CLIENTS],
    playing: &[bool; MAX_CLIENTS],
) -> bool {
    matches.iter().enumerate().any(|(id, m)| {
        m.active
            && m.red == m.blue
            && client_match
                .iter()
                .zip(playing)
                .any(|(cm, playing)| *playing && *cm == id as u8)
    })
}

/// Lowest-id active created match with connected members, for promoting into
/// the main slot when main ends. Pure so the closure path is unit-testable.
fn promotion_candidate(
    matches: &[state::TffaMatch; TOTAL_MATCH_SLOTS],
    client_match: &[u8; MAX_CLIENTS],
    connected: &[bool; MAX_CLIENTS],
    max: usize,
) -> Option<(u8, i32, i32, Vec<c_int>)> {
    for i in 1..=max {
        if i >= matches.len() {
            break;
        }
        let m = &matches[i];
        if !m.active {
            continue;
        }
        // A match that already hit its limit is ending, not surviving: it must
        // not be promoted into the main slot on its way out.
        if m.ending() {
            continue;
        }
        let members: Vec<c_int> = client_match
            .iter()
            .enumerate()
            .filter(|(ci, mm)| **mm == i as u8 && connected[*ci])
            .map(|(ci, _)| ci as c_int)
            .collect();
        if members.is_empty() {
            continue;
        }
        return Some((i as u8, m.red, m.blue, members));
    }
    None
}

/// Migrate a promoted match's tallies and members into the main slot: main
/// carries the surviving game, the promoted id becomes an inactive empty slot,
/// and its members are assigned to main. The old main's members were already
/// sent to spectator before this runs (`move_match_to_spectator`), so both
/// groups end up in the surviving (main) game as players or spectators.
fn promote_into_main(
    matches: &mut [state::TffaMatch; TOTAL_MATCH_SLOTS],
    client_match: &mut [u8; MAX_CLIENTS],
    id: u8,
    red: i32,
    blue: i32,
    members: &[c_int],
) {
    let main = &mut matches[MAIN_MATCH as usize];
    main.red = red;
    main.blue = blue;
    main.active = true;
    // The main slot now carries the promoted *continuing* game: its own
    // pending end (the reason it is being replaced) must not void the new
    // game's damage.
    main.end_queued_at = 0;
    let promoted = &mut matches[id as usize];
    promoted.red = 0;
    promoted.blue = 0;
    promoted.active = false;
    promoted.end_queued_at = 0;
    // The promoted game continues as main; the slot it came from is free and
    // remembers nobody.
    promoted.remembered = [REMEMBERED_NONE; MAX_CLIENTS];
    for cn in members {
        if let Some(slot) = client_match.get_mut(*cn as usize) {
            *slot = MAIN_MATCH;
        }
    }
}

/// A new TFFA may not be created once the shared timelimit expired: the map
/// is winding down (untied matches are closed, a tied one is in sudden death)
/// and must end, not grow a fresh game. Pure for the `cmd_create` test.
fn create_allowed(timelimit_expired: bool) -> bool {
    !timelimit_expired
}

/// One match's end-of-match facts, captured from the live game state before
/// anything is moved or reset: the connected members, the team each of them
/// played for, and the per-team name lists for the announcement. Shared by the
/// per-match finish path and by [`finish_map_keeping_matches`].
struct Finished {
    /// The runtime slot the game was played in.
    id: u8,
    /// The TFFA id the game belongs to: `id` for a created match, and
    /// `TffaState::main_origin` for the main slot (a game promoted into it
    /// keeps its own id, D-006.11).
    identity: u8,
    red: i32,
    blue: i32,
    /// `"fraglimit"` or `"timelimit"` (for the end-of-match line).
    reason: &'static str,
    members: Vec<c_int>,
    /// Team each member played for when the match ended (`TEAM_RED` /
    /// `TEAM_BLUE`), aligned with `members`; every other team is
    /// `REMEMBERED_NONE` and is not remembered (a spectator was not in the
    /// game, D-006.10).
    teams: Vec<u8>,
    red_names: Vec<String>,
    blue_names: Vec<String>,
}

/// Snapshot one match's end-of-match facts. `connected` keeps free slots out:
/// they default to main and would otherwise inflate a finish to 30 "members"
/// (reliable overflow + `SetTeam` on empty entities).
fn collect_result(
    state: &state::ProxyState,
    id: u8,
    red: i32,
    blue: i32,
    reason: &'static str,
    connected: &[bool; MAX_CLIENTS],
) -> Finished {
    let members: Vec<c_int> = state
        .tffa
        .client_match
        .iter()
        .enumerate()
        .filter(|(i, m)| **m == id && connected[*i])
        .map(|(i, _)| i as c_int)
        .collect();
    // Team split + names from the stored CS_PLAYERS bases (game `t` team +
    // `n` netname at last update).
    let g_entities = state.located_game_data.g_entities;
    let mut teams = Vec::with_capacity(members.len());
    let mut red_names = Vec::new();
    let mut blue_names = Vec::new();
    for cn in &members {
        let team = session_team_at(g_entities, *cn);
        teams.push(match team {
            Some(TEAM_RED) => TEAM_RED as u8,
            Some(TEAM_BLUE) => TEAM_BLUE as u8,
            _ => REMEMBERED_NONE,
        });
        let base = &state.tffa.cs_players_base[*cn as usize];
        let team = base
            .is_empty()
            .then_some(None)
            .unwrap_or_else(|| info_value(base, "t").and_then(|t| t.parse::<i32>().ok()));
        let name = if base.is_empty() {
            format!("player{cn}")
        } else {
            announce_name(&info_value(base, "n").unwrap_or_else(|| format!("player{cn}")))
        };
        match team {
            Some(TEAM_RED) => red_names.push(name),
            Some(TEAM_BLUE) => blue_names.push(name),
            _ => {}
        }
    }
    Finished {
        id,
        identity: if id == MAIN_MATCH {
            state.tffa.main_origin
        } else {
            id
        },
        red,
        blue,
        reason,
        members,
        teams,
        red_names,
        blue_names,
    }
}

/// Human label for a game's TFFA id in announcements. Main is "main (TFFA 0)"
/// when it hosts its own game; a created match promoted into the main slot
/// keeps its own id (D-006.11).
fn match_label(identity: u8) -> String {
    if identity == MAIN_MATCH {
        "main (TFFA 0)".to_owned()
    } else {
        format!("TFFA {identity}")
    }
}

/// Whether the shared timelimit is ending the **whole map**: the limit expired
/// and no active match is still tied with a player who could untie it. Every
/// game is decided, so there is nothing left to play for; the map ends as a
/// whole (one intermission, every match intact) instead of ending match by
/// match, which would move each instance's players to main as spectators
/// before the final `scores` message and hand them main's board.
/// Pure so the choice is unit-tested.
fn map_ends_at_timelimit(
    limit_expired: bool,
    matches: &[state::TffaMatch; TOTAL_MATCH_SLOTS],
    client_match: &[u8; MAX_CLIENTS],
    playing: &[bool; MAX_CLIENTS],
) -> bool {
    limit_expired && !has_continuing_match(matches, client_match, playing)
}

/// Whether some active match is still inside its end-of-match window
/// ([`END_DELAY_MS`]): the map's final intermission must wait for it, exactly
/// like `any_match_ending` gates the per-match path.
fn any_match_waiting(matches: &[state::TffaMatch], now: i32) -> bool {
    matches
        .iter()
        .any(|m| m.ending() && now - m.end_queued_at < END_DELAY_MS)
}

/// End the map while keeping every match — and its members — intact: announce
/// each result, print the per-instance end-game tables, then run the pristine
/// intermission.
///
/// Used when the timelimit expired with no tied match left to play. The
/// per-match path (`move_match_to_spectator`) would deactivate each slot and
/// move its players into main as spectators before `BeginIntermission` builds
/// the final board, so every instance player would read main's scoreboard.
/// Here nothing is moved or reset: `snapshot_intermission_boards` captures each
/// viewer's own match and the `scores` rewrite shows its tallies, and the
/// `map_restart` carry (D-006.8) keeps the instances with their members, so the
/// next round starts in the same game instead of in main as a spectator.
fn finish_map_keeping_matches(connected: &[bool; MAX_CLIENTS], fraglimit: i32) {
    let results = state::with_state(|s| {
        (0..TOTAL_MATCH_SLOTS)
            .filter(|i| s.tffa.matches[*i].active)
            .map(|i| {
                let m = &s.tffa.matches[i];
                // A match that hit the fraglimit before the timelimit keeps
                // that reason; a tied slot nobody played in ends with the map.
                let reason =
                    match_finish_reason(m.red, m.blue, fraglimit, true).unwrap_or("timelimit");
                collect_result(s, i as u8, m.red, m.blue, reason, connected)
            })
            .collect::<Vec<Finished>>()
    });
    for f in &results {
        if f.members.is_empty() {
            continue;
        }
        eprintln!(
            "----- proxy-rs: TFFA {} ended ({}) {}-{} — map over, players keep their own final scoreboard",
            f.identity, f.reason, f.red, f.blue
        );
        let red_list = if f.red_names.is_empty() {
            "-".to_owned()
        } else {
            f.red_names.join(", ")
        };
        let blue_list = if f.blue_names.is_empty() {
            "-".to_owned()
        } else {
            f.blue_names.join(", ")
        };
        let label = match_label(f.identity);
        announce_to_all(&format!(
            "print \"{label} ended ({}): RED {} - BLUE {}. RED: {red_list} | BLUE: {blue_list}. Final scoreboard incoming.\n\"",
            f.reason, f.red, f.blue
        ));
        // Per-instance tables only: the pristine intermission below already
        // prints the standard tables for everyone (main included), as it does
        // for a solo-main finish.
        if f.identity != MAIN_MATCH {
            crate::hooks::game::print_tffa_match_stats(&f.members);
        }
    }
    crate::hooks::game::run_begin_intermission();
}

/// Per-frame: finish matches that hit the shared fraglimit, and — after the
/// shared timelimit expires — every match that is not tied. A tied match keeps
/// playing (per-match sudden death) until a frag separates the teams; once no
/// active tied match with players is left, the whole map ends
/// ([`finish_map_keeping_matches`]) and no instance player is moved out, so
/// each of them keeps their own final scoreboard.
/// While a tied match keeps the map alive, sub-match instances are announced to
/// **everyone** with final teams/scores, members moved back to main as
/// spectators, stats printed to members + spectators. When the main itself
/// finishes, the lowest-id still-played TFFA becomes the new main; with no
/// other active TFFA the map ends via pristine intermission (final scoreboard +
/// standard stats for all).
/// Runs after the frame is forwarded so scores from this frame are visible.
pub fn on_run_frame_post() {
    // Make cgame load any player model its `CS_PLAYERS` correction deferred
    // (D-006.14). Before the gate/early returns below: the disable path
    // (`restore_pristine_views`) is a mass `CS_PLAYERS` change too.
    flush_deferred_model_loads();
    apply_pending_spectator_anim_flips();
    if !enabled() {
        return;
    }
    // Finish the `GAME_CLIENT_BEGIN` work the `map_restart` burst deferred, and
    // any `cs` correction it dropped, now that the clients have acknowledged
    // it (D-006.12). Before the intermission/settle early returns below, which
    // must not starve it.
    drain_deferred_client_work();
    // Game-follow changes adopt a new instance with no proxy command: when a
    // viewer's effective match moves (follow start/stop/switch, or a travel
    // that missed its explicit refresh), re-push prefixed names + scores so
    // the scoreboard follows who you watch.
    let viewers: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|v| s.clients[*v as usize].is_connected)
            .collect()
    });
    for v in viewers {
        let eff = effective_match(v);
        let known = state::with_state(|s| s.tffa.viewer_match_cache[v as usize]);
        if eff != known {
            refresh_cs_players_for_viewer(v);
        }
    }
    // The refresh loop above can queue new `cs` corrections after the flush at
    // the top of this function; load those deferred models in the same frame.
    flush_deferred_model_loads();
    // While an intermission runs (pristine timelimit exit or a solo-main
    // finish below) the map is ending: no more fraglimit handling.
    if state::with_state(|s| s.tffa.intermission) {
        return;
    }
    // Post-`map_restart` settle window: the engine runs its frames *before*
    // reconnecting the retained clients (`SV_MapRestart_f`, `sv_ccmds.cpp:306-349`),
    // so every carried created match is temporarily member-less. Skip the
    // sweep (and limit handling) until the first connect clears the window; a
    // genuinely abandoned match closes on the first frame after the burst.
    // The countdown is the fallback for a restart with nobody to reconnect.
    let grace = state::with_state(|s| {
        let left = s.tffa.restart_grace_frames;
        s.tffa.restart_grace_frames = left.saturating_sub(1);
        left
    });
    if grace != 0 {
        return;
    }
    // A finished match's auto-join is deferred past the restart burst
    // (D-006.12): its remembered members are still spectators, so the match
    // looks empty until the deferred `restore_pending_join` runs. Keep the
    // sweep (and limit handling) off until every scheduled auto-join has been
    // applied — the same protection the settle window gives the open matches.
    if state::with_state(|s| {
        s.tffa
            .pending_join
            .iter()
            .any(|team| *team != REMEMBERED_NONE)
    }) {
        return;
    }
    // A created match whose last player went spectator or disconnected has
    // nothing left to host: close it (main is implicit and never closes).
    // Runs before the limit early-return so it also holds with no limits.
    close_empty_created_matches();
    // SAFETY: engine syscall registered.
    let fraglimit = unsafe { crate::syscall::cvar_variable_integer_value(c"fraglimit") };
    let limit_expired = timelimit_expired();
    if fraglimit <= 0 && !limit_expired {
        // No limit is in force. An admin who raised/disabled the limit inside
        // an end-of-match window keeps the match playing: cancel the pending
        // end so its damage is not void forever (the limit early-return would
        // otherwise skip the cancel pass below every frame).
        state::with_state(|s| {
            for m in s.tffa.matches.iter_mut() {
                m.end_queued_at = 0;
            }
        });
        return;
    }
    // Authoritative game-side census (includes bots and connecting clients).
    let (connected, playing) = client_census();
    // First pass: decide which matches that hit a limit this frame are *newly*
    // ending (queue the pristine one-second end window) and which have been
    // ending long enough to be applied now. A match inside the window keeps
    // playing; its damage is void (`tffa::match_ending` in the `G_Damage`
    // gate), so the final kill is not immediately followed by a spectator
    // teleport — exactly like pristine `LogExit` → `BeginIntermission`.
    let now = level_time();
    let mut newly_ending: Vec<(u8, &'static str, i32, i32)> = Vec::new();
    let mut due: Vec<u8> = Vec::new();
    state::with_state(|s| {
        let main_origin = s.tffa.main_origin;
        for (i, m) in s.tffa.matches.iter_mut().enumerate() {
            if !m.active {
                continue;
            }
            let reason = match_finish_reason(m.red, m.blue, fraglimit, limit_expired);
            let was_ending = m.ending();
            let due_now = queued_finish(m, reason, now).is_some();
            if !was_ending && m.ending() {
                let identity = if i == MAIN_MATCH as usize {
                    main_origin
                } else {
                    i as u8
                };
                newly_ending.push((identity, reason.unwrap_or("fraglimit"), m.red, m.blue));
            }
            if due_now {
                due.push(i as u8);
            }
        }
    });
    for (id, reason, red, blue) in &newly_ending {
        eprintln!(
            "----- proxy-rs: TFFA {id} reached {reason} ({red}-{blue}) — damage void for {END_DELAY_MS} ms before the match ends"
        );
    }
    // Timelimit reached with no tied match left to play: every game is decided,
    // so the **map** ends. Finishing the matches one by one would move each
    // instance's players to main as spectators and deactivate their slot before
    // `BeginIntermission` builds the final `scores` message, so they would read
    // main's board instead of their own. Keep every match (and its members)
    // intact through the intermission instead: each viewer keeps their own
    // instance's final scoreboard, and the next `map_restart` carries the
    // instance with them (D-006.8) rather than dropping them into main.
    // An end-of-match window already running (`fraglimit`, or the timelimit
    // queue above) is served first — damage void, exactly like the pristine
    // exit delay.
    {
        let (matches, table) = state::with_state(|s| (s.tffa.matches, s.tffa.client_match));
        if map_ends_at_timelimit(limit_expired, &matches, &table, &playing) {
            if any_match_waiting(&matches, now) {
                return;
            }
            finish_map_keeping_matches(&connected, fraglimit);
            return;
        }
    }
    let mut finished: Vec<Finished> = Vec::new();
    state::with_state(|s| {
        for id in &due {
            let m = &s.tffa.matches[*id as usize];
            let Some(reason) = match_finish_reason(m.red, m.blue, fraglimit, limit_expired) else {
                continue;
            };
            finished.push(collect_result(s, *id, m.red, m.blue, reason, &connected));
        }
        for f in &finished {
            // Main keeps its final tallies for the intermission scoreboard
            // when it ends solo (see below); subs reset as usual.
            if f.id != MAIN_MATCH {
                // Remember who played this match and for which team: a
                // `map_restart` re-creates the instance and puts them back on
                // those teams (D-006.10). The roster is stored under the game's
                // identity (the slot id for a created match) and the client is
                // remembered by one id only.
                let roster = roster_from_members(&f.members, &f.teams, &s.tffa.home, f.identity);
                for (j, other) in s.tffa.matches.iter_mut().enumerate() {
                    if j != f.identity as usize {
                        for (cn, team) in roster.iter().enumerate() {
                            if *team != REMEMBERED_NONE {
                                other.remembered[cn] = REMEMBERED_NONE;
                            }
                        }
                    }
                }
                let m = &mut s.tffa.matches[f.identity as usize];
                m.red = 0;
                m.blue = 0;
                m.active = false;
                m.end_queued_at = 0;
                m.remembered = roster;
                // The instance is over: queue its world for release at the next
                // frame start (members are being moved out below).
                s.tffa.world_release |= 1u16 << f.id;
            }
        }
    });
    // A finished instance's team lock goes with it (members are moved to
    // spectator below and the slot may be reused by a new match).
    for f in &finished {
        if f.id != MAIN_MATCH {
            crate::teamlock::clear_match(f.id);
        }
        eprintln!(
            "----- proxy-rs: TFFA {} ended ({}) {}-{}",
            f.identity, f.reason, f.red, f.blue
        );
    }
    if finished.is_empty() {
        // No match ended this frame: after the timelimit the map lives only
        // while a tied match still has players, and not while a match that
        // reached its limit is still serving its end-of-match delay.
        if limit_expired {
            let (matches, table) = state::with_state(|s| (s.tffa.matches, s.tffa.client_match));
            if !has_continuing_match(&matches, &table, &playing) && !any_match_ending(&matches) {
                crate::hooks::game::run_begin_intermission();
            }
        }
        return;
    }
    // Sub-matches first: announce to all, stats to members + spectators,
    // then back to main as spectators (the move resets scores/stats, so the
    // tables must print first).
    for f in finished.iter().filter(|f| f.id != MAIN_MATCH) {
        if f.members.is_empty() {
            continue;
        }
        let red_list = if f.red_names.is_empty() {
            "-".to_owned()
        } else {
            f.red_names.join(", ")
        };
        let blue_list = if f.blue_names.is_empty() {
            "-".to_owned()
        } else {
            f.blue_names.join(", ")
        };
        announce_to_all(&format!(
            "print \"TFFA {} ended ({}): RED {} - BLUE {}. RED: {} | BLUE: {}. Players returned to main as spectator; they return here on the next map restart.\n\"",
            f.id, f.reason, f.red, f.blue, red_list, blue_list
        ));
        crate::hooks::game::print_tffa_match_stats(&f.members);
        move_match_to_spectator(f.id);
    }
    if let Some(main) = finished.iter().find(|f| f.id == MAIN_MATCH) {
        if main.members.is_empty() {
            // Nobody home (connect churn window): drop the stale lead silently
            // so it cannot re-fire every frame; the next real frag starts
            // clean. At the timelimit the check below then ends a map that has
            // no players left.
            state::with_state(|s| {
                s.tffa.matches[MAIN_MATCH as usize].red = 0;
                s.tffa.matches[MAIN_MATCH as usize].blue = 0;
                s.tffa.matches[MAIN_MATCH as usize].end_queued_at = 0;
            });
        } else {
            // Lowest-id still-played TFFA becomes the new main.
            let max = max_matches();
            let candidate = state::with_state(|s| {
                promotion_candidate(&s.tffa.matches, &s.tffa.client_match, &connected, max)
            });
            if let Some((id, red, blue, members)) = candidate {
                // Main ends like any other instance first: final scores, team
                // lists, stats tables, members back to spectator. Only then does
                // the candidate migrate into the emptied main slot.
                let red_list = if main.red_names.is_empty() {
                    "-".to_owned()
                } else {
                    main.red_names.join(", ")
                };
                let blue_list = if main.blue_names.is_empty() {
                    "-".to_owned()
                } else {
                    main.blue_names.join(", ")
                };
                announce_to_all(&format!(
                    "print \"{} ended ({}): RED {} - BLUE {}. RED: {} | BLUE: {}. Players returned to main as spectator; they return here on the next map restart.\n\"",
                    match_label(main.identity),
                    main.reason,
                    main.red,
                    main.blue,
                    red_list,
                    blue_list
                ));
                crate::hooks::game::print_tffa_match_stats(&main.members);
                // The players this game sent to spectator are remembered under
                // **its own id** (D-006.11), not under the main slot: the group
                // comes back to the same TFFA id at the next `map_restart`, even
                // though the surviving game takes the main slot over below.
                let home = state::with_state(|s| s.tffa.home);
                let roster = roster_from_members(&main.members, &main.teams, &home, main.identity);
                move_match_to_spectator(MAIN_MATCH);
                state::with_state(|s| {
                    store_return_roster(&mut s.tffa.matches, main.identity, &roster);
                    promote_into_main(
                        &mut s.tffa.matches,
                        &mut s.tffa.client_match,
                        id,
                        red,
                        blue,
                        &members,
                    );
                    // The surviving game keeps its id while it runs in the main
                    // slot: a restart puts it back in its own slot, and until
                    // then that slot must not be handed to a new match.
                    s.tffa.main_origin = id;
                    // The promoted instance's own world does not become main's:
                    // its players see the pristine map entity set from here on, and
                    // its clone set is given back at the next frame start.
                    s.tffa.world_release |= 1u16 << id;
                });
                // The promoted instance is the surviving game from here on, so
                // its team-lock caps follow it into main.
                crate::teamlock::promote_match(id);
                for cn in &members {
                    broadcast_cs_players_slot(*cn);
                    refresh_cs_players_for_viewer(*cn);
                }
                on_set_cs_scores();
                announce_to_all(&format!(
                    "print \"TFFA {id} becomes the new main (RED {red} - BLUE {blue}).\n\""
                ));
                // At the timelimit the promoted match may itself be untied or
                // player-less; the check below then ends the map. A tied one
                // keeps it running.
            } else {
                // Solo main finish (no other active TFFA): end the map normally so
                // the final scoreboard shows. Members stay in their teams with final
                // tallies intact (no reset, no spectator move, no per-instance
                // stats — pristine intermission prints the standard tables for all).
                let red_list = if main.red_names.is_empty() {
                    "-".to_owned()
                } else {
                    main.red_names.join(", ")
                };
                let blue_list = if main.blue_names.is_empty() {
                    "-".to_owned()
                } else {
                    main.blue_names.join(", ")
                };
                announce_to_all(&format!(
                    "print \"{} ended ({}): RED {} - BLUE {}. RED: {} | BLUE: {}. Final scoreboard incoming.\n\"",
                    match_label(main.identity),
                    main.reason,
                    main.red,
                    main.blue,
                    red_list,
                    blue_list
                ));
                crate::hooks::game::run_begin_intermission();
                return;
            }
        }
    }

    // After the timelimit the map lives only while at least one active match
    // is tied and still has playing members. `playing` is re-read because the
    // handling above moved finished matches to spectator and may have
    // promoted a tied contender into main. When nothing continues, every game
    // is over: run the normal intermission (final scoreboard + map restart).
    if limit_expired {
        let (matches, table) = state::with_state(|s| (s.tffa.matches, s.tffa.client_match));
        if !has_continuing_match(&matches, &table, &playing_clients())
            && !any_match_ending(&matches)
        {
            crate::hooks::game::run_begin_intermission();
        }
    }
}

/// Send a server command to everyone (`-1`).
fn announce_to_all(text: &str) {
    let Ok(cmsg) = std::ffi::CString::new(text) else {
        return;
    };
    // SAFETY: engine syscall registered; cmsg NUL-terminated.
    unsafe { crate::syscall::send_server_command(-1, &cmsg) };
}

/// One name for the end-of-match announcement: no quotes/newlines (they
/// would break the `print "..."` wire format), truncated so the whole team
/// list cannot overflow the 1022-char reliable-command guard.
fn announce_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars().take(20) {
        match ch {
            '"' => out.push('\''),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    // Strip color escapes for the announcement (keeps it short + readable).
    let mut bytes = out.into_bytes();
    crate::utils::strip_color(&mut bytes);
    let n = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..n]).into_owned()
}

/// Send one client to a team through the pristine `SetTeam` body
/// (trampoline, not the wrapper → no recursion, no teamlock block: the proxy
/// only asks for spectator/red/blue on a match it just prepared). `team` is
/// the same C string `/team` takes. Returns false when the client has no live
/// game entity (a free slot's `ent->client` is null and the game body would
/// deref it) or the trampoline is not attached.
fn set_client_team(client_num: c_int, team: &CStr) -> bool {
    if crate::original::base() == 0 {
        return false;
    }
    // SAFETY: trampoline holds the pristine SetTeam body (attached at GAME_INIT).
    let tramp = crate::patch::GAME_HOOKS[5].original();
    if tramp == 0 {
        return false;
    }
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
        return false;
    }
    let ent = g_entities + client_num as usize * crate::sdk::SIZEOF_GENTITY;
    // SAFETY: ent is inside the g_entities array.
    if unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) } == 0 {
        return false;
    }
    let original: unsafe extern "C" fn(usize, *const c_char) =
        unsafe { core::mem::transmute(tramp) };
    // SAFETY: ent points into g_entities; `team` is a static C string.
    unsafe { original(ent, team.as_ptr()) };
    true
}

/// Send one client to the spectator team through the pristine `SetTeam` body
/// (trampoline, not the wrapper → no recursion, no teamlock block: spectator
/// requests always pass). This is the same move `/team spectator` makes, so
/// the player suicides and respawns spectating.
fn set_client_spectator(client_num: c_int) -> bool {
    set_client_team(client_num, c"spectator")
}

/// Force every member of a match to spectator through the pristine `SetTeam`
/// body. Members return to the main slot so they can see it and join
/// elsewhere.
fn move_match_to_spectator(match_id: u8) {
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
        return;
    }
    let connected = connected_clients();
    let members: Vec<c_int> = state::with_state(|s| {
        s.tffa
            .client_match
            .iter()
            .enumerate()
            .filter(|(i, m)| **m == match_id && connected[*i])
            .map(|(i, _)| i as c_int)
            .collect()
    });
    for cn in members {
        // Free slots have no client — `set_client_spectator` refuses them
        // (null `ent->client` deref would take the server down); just reset
        // the proxy assignment.
        set_client_spectator(cn);
        let moved = state::with_state(|s| {
            if !(0..MAX_CLIENTS as c_int).contains(&cn) {
                return false;
            }
            let old = s.tffa.client_match[cn as usize];
            s.tffa.client_match[cn as usize] = MAIN_MATCH;
            s.tffa.player_score[cn as usize] = 0;
            // Main members stay main: no prefix/team change for anyone, and
            // their own view is unchanged — skip the 2×32 resend storm that
            // overflowed the reliable buffer on main finishes.
            old != MAIN_MATCH
        });
        if moved {
            // The slot changed instances: refresh its own view and everyone
            // else's prefixed view of it (the game will also re-broadcast
            // `CS_PLAYERS + cn` via `ClientUserinfoChanged`, which restores
            // the base through `on_set_cs_players`). Stats start clean for
            // the next instance.
            reset_slot_stats(cn);
            broadcast_cs_players_slot(cn);
            refresh_cs_players_for_viewer(cn);
        }
    }
    // Tallies changed for the viewers that stay: push fresh mini-HUD (deduped
    // per viewer inside).
    on_set_cs_scores();
}

/// Build a finished game's remembered roster from its members and their teams,
/// restricted to the clients whose identity is `identity` (D-006.11).
///
/// Only a member who was actually playing (RED/BLUE) is remembered — a
/// spectator was not "in the game". The identity filter matters when a game
/// runs in the main slot after promoting a created match: the old main's
/// spectators are still `client_match = 0`, i.e. runtime members of that game,
/// but they belong to (and return to) another id. Pure so the capture is
/// unit-tested.
fn roster_from_members(
    members: &[c_int],
    teams: &[u8],
    home: &[u8; MAX_CLIENTS],
    identity: u8,
) -> [u8; MAX_CLIENTS] {
    let mut roster = [REMEMBERED_NONE; MAX_CLIENTS];
    for (cn, team) in members.iter().zip(teams) {
        if *team == REMEMBERED_NONE || !(0..MAX_CLIENTS as c_int).contains(cn) {
            continue;
        }
        let n = *cn as usize;
        if home[n] != identity {
            continue;
        }
        roster[n] = *team;
    }
    roster
}

/// Store a finished game's roster under its **identity slot** (D-006.10,
/// D-006.11), so a `map_restart` brings the group back to the same TFFA id —
/// even when the game was promoted into the main slot, which stores it in the
/// original created slot instead of `0`. Entries are merged: a `REMEMBERED_NONE`
/// (a spectator) never erases a team the same client was remembered with.
fn store_return_roster(
    matches: &mut [state::TffaMatch; TOTAL_MATCH_SLOTS],
    identity: u8,
    roster: &[u8; MAX_CLIENTS],
) {
    let Some(slot) = matches.get_mut(identity as usize) else {
        return;
    };
    for (cn, team) in roster.iter().copied().enumerate() {
        if team != REMEMBERED_NONE {
            slot.remembered[cn] = team;
        }
    }
}

/// Drop a client from every finished game's remembered roster, from the
/// pending auto-join table and from its identity. Used when they disconnect or
/// a fresh client takes their slot: no id may put them back on a team at the
/// next `map_restart` (D-006.10, D-006.11).
fn forget_client_everywhere(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let cn = client_num as usize;
    state::with_state(|s| {
        for m in s.tffa.matches.iter_mut() {
            m.remembered[cn] = REMEMBERED_NONE;
        }
        s.tffa.pending_join[cn] = REMEMBERED_NONE;
        s.tffa.home[cn] = MAIN_MATCH;
        // A new occupant of the slot starts with no match authorisation.
        s.tffa.pass_ok[cn] = MAIN_MATCH;
    });
}

/// Drop every remembered return team for one client (D-006.10, D-006.11),
/// without changing the id they belong to. Called on an explicit instance move
/// and when the client plays again (a team join), so a stale entry cannot
/// override what they chose; a `map_restart` still returns them to their id
/// through `TffaState::home`.
pub fn forget_return_rosters(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    state::with_state(|s| forget_return_rosters_in(&mut s.tffa.matches, client_num as usize));
}

/// Pure part of `forget_return_rosters`: clear `client_num` from every slot's
/// roster.
fn forget_return_rosters_in(
    matches: &mut [state::TffaMatch; TOTAL_MATCH_SLOTS],
    client_num: usize,
) {
    if client_num >= MAX_CLIENTS {
        return;
    }
    for m in matches.iter_mut() {
        m.remembered[client_num] = REMEMBERED_NONE;
    }
}

/// Put a reconnecting client back into the instance a `map_restart` re-created
/// for it, on the team it played for before the match finished (D-006.10).
///
/// Runs after the game's own `ClientBegin` (the client's entity is live) and
/// only consumes the one-shot `pending_join` entry: a client with no entry is
/// left to the base game's session data, i.e. an active carried match restores
/// RED/BLUE exactly as before. The move itself is the pristine `SetTeam` body,
/// the same one `/team red|blue` runs (and the one `tffa_create` uses).
fn restore_pending_join(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let team = state::with_state(|s| {
        core::mem::replace(
            &mut s.tffa.pending_join[client_num as usize],
            REMEMBERED_NONE,
        )
    });
    let team = match team {
        t if t == TEAM_RED as u8 => TEAM_RED,
        t if t == TEAM_BLUE as u8 => TEAM_BLUE,
        _ => return,
    };
    set_client_team(client_num, team_name(team));
}

// ---------------------------------------------------------------------------
// Per-instance world entities (D-006 extension)
//
// Every *interactive* map entity (door, plat, elevator, `trigger_*`, button)
// is meant to exist once per instance, so one instance cannot open another's
// door or fire another's trigger. An interactive entity that is not
// duplicated is a *shared* entity any instance can activate for all of them,
// so coverage is all-or-nothing per instance: either the whole interactive
// set is cloned for an instance or the instance is refused. Capacity is spent
// on instances, never on coverage.
// ---------------------------------------------------------------------------

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
fn is_duplicable_entity(g_entities: usize, ent_num: c_int) -> bool {
    let ent = g_entities + ent_num as usize * SIZEOF_GENTITY;
    // SAFETY: `ent_num` is a valid gentity slot.
    unsafe {
        read_i32(ent + OFFSET_GENTITY_S_ETYPE) != ET_NPC
            && read_usize(ent + OFFSET_GENTITY_CLIENT) == 0
            && read_usize(ent + OFFSET_GENTITY_GHOUL2) == 0
    }
}

/// `gentity_t` pointer fields that can name another entity. A duplicated
/// team/train chain must stay inside its own instance, so a clone's links are
/// re-pointed at the corresponding clone. A pointer that resolves outside the
/// set (a player, an inert shared prop) is left exactly as copied — players
/// and inert props are shared in every instance by design.
const CLONE_LINK_FIELDS: [usize; 9] = [
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

/// `GAME_INIT` post-pass: measure the map's interactive-entity set — one
/// further instance's clone cost — and how many further instances the entity
/// budget can take, once the pristine `GAME_INIT` returned (map spawned,
/// `g_entities` located). The interactive entities are tagged as belonging to
/// main (`MAIN_MATCH`), which is inert while every viewer is in main and makes
/// the set the clone pass reads. Called lazily from `cmd_create` too, so a
/// server that enables `proxy_tffa_enable` mid-map is still measured before
/// its first instance.
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
fn ensure_world_scanned() -> bool {
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
fn claim_world(pending: &mut u16, cloned: u16, instance: u8) -> bool {
    let bit = 1u16 << instance;
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
    let bit = 1u16 << instance;
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
fn release_instance(instance: u8) {
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
    state::with_state(|s| s.tffa.world_cloned &= !(1u16 << instance));
}

/// Drain the instance releases queued since the last frame.
pub fn drain_world_releases() {
    let pending = state::with_state(|s| core::mem::take(&mut s.tffa.world_release));
    if pending == 0 {
        return;
    }
    for instance in 1..TOTAL_MATCH_SLOTS {
        if pending & (1u16 << instance) != 0 {
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

// ---------------------------------------------------------------------------
// Sound isolation (temp-entity sound attribution)
// ---------------------------------------------------------------------------

/// Attribution of a temp entity that belongs to no match: map/world sounds
/// (doors, item respawns, `G_SoundAtLoc`, NPC chatter) stay audible for every
/// match.
pub const SOURCE_WORLD: u8 = u8::MAX;

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
    /// `0` there is the untouched default, not client 0.
    OptionalAttacker,
    /// `s.otherEntityNum` (victim) and `s.otherEntityNum2` (attacker/killer),
    /// both always set.
    VictimAndAttacker,
    /// `s.owner` always names the affected player; a `0` there is client 0.
    SOwner,
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
        // `EV_SCREENSHAKE`, `EV_PLAY_EFFECT*`, `EV_GLASS_SHATTER`,
        // `EV_MISC_MODEL_EXP`, the weapon-impact events
        // (`EV_MISSILE_MISS`, `EV_DISRUPTOR_*_MISS/_HIT`, ...), the
        // `EV_SABER_CLASHFLARE` hit point, and the deliberate broadcasts
        // already handled by `sound_attribution` are owner-less world events.
        _ => EventOwners::World,
    }
}

/// gentity slot of `ent` (`ent - g_entities` in `SIZEOF_GENTITY` steps), or
/// `None` when the pointer is not a slot of the `g_entities` array.
fn ent_num_of(g_entities: usize, ent: usize) -> Option<c_int> {
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
/// otherwise an `EV_PREDEFSOUND` constructed while an acting scope is open
/// belongs to that instance (force jump/protect/absorb sounds), and every
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

/// Pure stamping rule (unit-tested): the `G_Sound` handshake beats everything;
/// an owner-less `EV_PREDEFSOUND` takes the open acting scope (no scope → world
/// sound, audible everywhere); anything else is a world sound.
pub fn temp_stamp_source(pending: u8, event: c_int, scope: u8) -> u8 {
    if pending != SOURCE_WORLD {
        return pending;
    }
    if event == crate::sdk::EV_PREDEFSOUND && scope != SCOPE_NONE {
        return scope;
    }
    SOURCE_WORLD
}

// ---------------------------------------------------------------------------
// Physics isolation (G_TRACE / G_G2TRACE / G_TRACECAPSULE interception)
// ---------------------------------------------------------------------------

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
fn flip_list(
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
fn collect_flips(
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
fn touch_requester(s: &crate::state::TffaState, g_entities: usize, ent: usize) -> Option<u8> {
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

// ---------------------------------------------------------------------------
// Acting scope (S7 link gate, S8 runtime-spawn gate): the instance whose game
// code is running
// ---------------------------------------------------------------------------

/// No acting scope is open: `G_Find` is the pristine global search and a
/// `G_Spawn` made now belongs to no instance. Shares `ENTITY_SHARED` with
/// "untagged entity" because both mean "no instance filter", and a scope is only
/// ever a real match id (`0..TOTAL_MATCH_SLOTS`).
pub const SCOPE_NONE: u8 = ENTITY_SHARED;

/// The instance a call into game code belongs to — the *acting* entity's own
/// instance. `None` when the actor cannot be attributed: a live player slot
/// resolves through `client_match` (a chain or a think fired by a player is that
/// player's instance's), a tagged world entity through its tag (main included),
/// a live entity that carries no tag through its owner (one `r.ownerNum` hop to
/// a live player: missiles, saber entities, physics objects, bodies — the same
/// resolution the trace gate's `request_match` uses), and anything else — a
/// shared inert prop, a freed one, a pointer below the array, the world slots —
/// resolves to nothing.
fn acting_instance(s: &crate::state::TffaState, g_entities: usize, ent: usize) -> Option<u8> {
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

// ---------------------------------------------------------------------------
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
fn retain_visible_slots(
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

/// Keep only the entities the open acting scope may see in a box-query result,
/// compacting `list` in place and returning the new count. The engine already
/// did the box test (`G_EntitiesInBox`); the entries tagged to another instance
/// and the players of another instance are dropped, so a mover, an explosion or
/// a script running in one instance no longer sees another instance's doors,
/// triggers, items — or players — in its box.
///
/// Dropping the foreign players is what keeps a match's spawn points its own
/// (S11, spawn separation): `SpotWouldTelefrag` asks the engine "is a player
/// standing on this spawn point?" (`g_client.c:529-552`) and the answer used to
/// include every other match's players, so on maps with few spawn points every
/// match saw the others' bodies as occupied spots, exhausted its own pool and
/// fell back to the map's first spawn point — piling its players onto the same
/// spot. A player is only in their own instance's world: the snapshot filter
/// does not send them anywhere else and the trace gate makes them non-solid
/// there (`trace_flips`), so they must not answer another instance's box query
/// either.
///
/// # Safety
///
/// `list` must point to at least `count` writable `c_int`s — the game's own
/// array, exactly as the engine just filled it.
pub unsafe fn filter_box_list(list: *mut c_int, count: c_int, scope: u8) -> c_int {
    if list.is_null() || count <= 0 {
        return count.max(0);
    }
    state::with_state(|s| {
        if s.located_game_data.g_entities == 0 {
            // Nothing can be judged without the array: keep what the engine
            // returned rather than hide entities at random.
            return count;
        }
        // SAFETY: the caller guarantees `count` writable `c_int`s at `list`
        // (the game's own array, as the engine just filled it).
        let slice = unsafe { core::slice::from_raw_parts_mut(list, count as usize) };
        retain_visible_slots(&s.tffa.entity_instance, &s.tffa.client_match, slice, scope) as c_int
    })
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
fn scope_hides_num(tags: &[u8; MAX_GENTITIES], num: i32, scope: u8) -> bool {
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
fn query_hides_num(
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
fn link_hides_tag(tags: &[u8; MAX_GENTITIES], g_entities: usize, found: usize, window: u8) -> bool {
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

// ---------------------------------------------------------------------------
// Runtime-spawn gate (S8): an entity the map's code creates mid-map belongs to
// the instance that created it
// ---------------------------------------------------------------------------

/// The `(slot, instance)` one `G_Spawn` inside game code contributes, or `None`
/// when it belongs to no instance: nothing is running game code, the slot is not
/// a `gentity_t` of the ordinary world range (`MAX_CLIENTS..ENTITYNUM_MAX_NORMAL`,
/// the range `G_Spawn` hands out and the only range that is ever tagged), or the
/// `g_entities` array is not located.
fn spawn_record(acting_scope: u8, g_entities: usize, ed: usize) -> Option<(u16, u8)> {
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
fn tag_pending(
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
fn request_match(g_entities: usize, ent_num: c_int, table: &[u8; MAX_CLIENTS]) -> Option<u8> {
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
fn rebuild_owned(s: &mut state::ProxyState, g_entities: usize) {
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

// ---------------------------------------------------------------------------
// Player commands (called from GAME_CLIENT_COMMAND; true = consumed)
// ---------------------------------------------------------------------------

/// Handle `tffa_*` client commands. Returns true when consumed (do not forward).
pub fn handle_client_command(client_num: c_int) -> bool {
    if !state::proxy_enabled() || state::instance_mode() == state::InstanceMode::Duel {
        return false;
    }
    let mut cmd = [0u8; MAX_TOKEN_CHARS];
    // SAFETY: engine syscall registered; buffer writable.
    unsafe { crate::syscall::argv(0, &mut cmd) };
    let end = cmd.iter().position(|&b| b == 0).unwrap_or(cmd.len());
    let name = &cmd[..end];
    if !name.starts_with(b"tffa") {
        return false;
    }
    // tffa commands work even when the feature gate (gametype) is off so
    // players get a helpful message instead of an unknown-command.
    if name == b"tffa_create" {
        cmd_create(client_num);
        return true;
    }
    if name == b"tffa_join" {
        match cmd_arg_i32(1) {
            Some(id) => cmd_join(client_num, id),
            // Bare `tffa_join` must not fall through as `0` (which now
            // selects the main game): report the usage instead.
            None => tell(client_num, JOIN_USAGE),
        }
        return true;
    }
    if name == b"tffa_list" {
        cmd_list(client_num);
        return true;
    }
    if name == b"tffa" {
        cmd_list(client_num);
        return true;
    }
    false
}

/// What a travel command must do about the client's team before it can move
/// them to another instance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TravelMove {
    /// Already spectating: nothing to do.
    None,
    /// Playing (RED/BLUE/duel/…): force the spectator team first, exactly as
    /// `/team spectator` does (the player suicides), then move the instance.
    ToSpectator,
    /// Team unknown (no live game slot): fail closed, travel refused.
    Refuse,
}

/// Decide the team move from the client's session team (`None` = unknown).
fn travel_move(team: Option<i32>) -> TravelMove {
    match team {
        Some(TEAM_SPECTATOR) => TravelMove::None,
        Some(_) => TravelMove::ToSpectator,
        None => TravelMove::Refuse,
    }
}

/// `print` constant: travel refused (no live game slot to move).
const TRAVEL_REFUSED: &str = "print \"You cannot travel between TFFA matches right now.\n\"";
/// `print` constant: the client was playing and is now spectating.
const MOVED_TO_SPECTATOR: &str = "print \"Moved to spectator to travel between TFFA matches.\n\"";

/// Make the client legal for a match move: a playing client is first sent to
/// the spectator team through the pristine `SetTeam` body (the same move as
/// `/team spectator`, so the player suicides and respawns spectating) because
/// every instance switch happens as a spectator. A client already spectating
/// is left untouched. Returns false (with a message) when the client has no
/// live game slot.
fn travel_to_spectator(client_num: c_int) -> bool {
    match travel_move(session_team_of(client_num)) {
        TravelMove::None => true,
        TravelMove::ToSpectator if set_client_spectator(client_num) => {
            tell(client_num, MOVED_TO_SPECTATOR);
            true
        }
        TravelMove::ToSpectator | TravelMove::Refuse => {
            tell(client_num, TRAVEL_REFUSED);
            false
        }
    }
}

/// Copy client-command argument `n` into `buf` and return the number of bytes
/// the engine wrote (the string is NUL-terminated inside `buf`).
fn cmd_arg(n: c_int, buf: &mut [u8]) -> usize {
    // SAFETY: engine syscall registered; buffer writable.
    unsafe { crate::syscall::argv(n, buf) };
    buf.iter().position(|&b| b == 0).unwrap_or(buf.len())
}

fn cmd_arg_i32(n: c_int) -> Option<i32> {
    let mut buf = [0u8; 64];
    let end = cmd_arg(n, &mut buf);
    core::str::from_utf8(&buf[..end])
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
}

const JOIN_USAGE: &str = "print \"Usage: tffa_join <match> [password] (0 = main game, 1..N = created matches, see tffa_list). A password is only needed to play in a protected match; anyone may spectate one.\n\"";

/// The TFFA identity whose game runs in a runtime slot: a created slot's own
/// id, or the promoted game's id while it lives in the main slot (`main_origin`
/// `0` = main's own game). Pure for the tests.
fn slot_identity(main_origin: u8, slot: u8) -> u8 {
    if slot == MAIN_MATCH {
        main_origin
    } else {
        slot
    }
}

/// Whether `client_num` may join RED/BLUE in the match it is currently in:
/// always true for an open match, and for a protected one only once the client
/// has given its password (`pass_ok` names its identity). Spectating is never
/// gated. Pure for the tests.
fn password_cleared(
    matches: &[state::TffaMatch; TOTAL_MATCH_SLOTS],
    client_match: &[u8; MAX_CLIENTS],
    main_origin: u8,
    pass_ok: &[u8; MAX_CLIENTS],
    client_num: usize,
) -> bool {
    let Some(slot) = client_match.get(client_num) else {
        return false;
    };
    let identity = slot_identity(main_origin, *slot);
    let Some(m) = matches.get(identity as usize) else {
        return true;
    };
    !m.has_password() || pass_ok.get(client_num).copied() == Some(identity)
}

/// What a password argument of `tffa_join` did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Auth {
    /// The client's match has no password: nothing to unlock.
    Open,
    /// The password matched and the client may now join a team.
    Accepted,
    /// The password did not match; the client keeps spectating.
    Wrong,
    /// Longer than [`MAX_TFFA_PASSWORD`]; never compared.
    TooLong,
}

/// Give `given` as the password of the match the client is in, recording the
/// authorisation by **identity**. No move, no team change: the client picks a
/// team itself (`/team red|blue`).
fn authenticate_here(client_num: c_int, given: &[u8]) -> Auth {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return Auth::Wrong;
    }
    if given.len() > MAX_TFFA_PASSWORD {
        return Auth::TooLong;
    }
    let cn = client_num as usize;
    state::with_state(|s| {
        let identity = slot_identity(s.tffa.main_origin, s.tffa.client_match[cn]);
        let m = &s.tffa.matches[identity as usize];
        if !m.has_password() {
            return Auth::Open;
        }
        if !m.password_matches(given) {
            return Auth::Wrong;
        }
        s.tffa.pass_ok[cn] = identity;
        Auth::Accepted
    })
}

/// Tell the client the outcome of a password attempt made by
/// `tffa_join <id> <password>`; the command's own paths report their own cases
/// and fall back here for `Open`/`TooLong`.
fn send_auth_outcome(client_num: c_int, auth: Auth) {
    match auth {
        Auth::Open => tell(
            client_num,
            "print \"This match is not password protected: join a team with /team red|blue.\n\"",
        ),
        Auth::Accepted => tell(
            client_num,
            "print \"Password accepted: join a team with /team red|blue.\n\"",
        ),
        Auth::Wrong => tell(
            client_num,
            "print \"Wrong password: you can still spectate this match.\n\"",
        ),
        Auth::TooLong => tell(
            client_num,
            &format!("print \"Password too long (max {MAX_TFFA_PASSWORD} characters).\n\""),
        ),
    }
}

/// Whether the `SetTeam` wrapper must refuse this client's team join for lack
/// of the match password (the client's own instance only: main is normally
/// open, and a promoted protected game is checked under its identity).
pub fn password_gate_blocks(client_num: c_int) -> bool {
    if !enabled() || !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return false;
    }
    let cn = client_num as usize;
    !state::with_state(|s| {
        password_cleared(
            &s.tffa.matches,
            &s.tffa.client_match,
            s.tffa.main_origin,
            &s.tffa.pass_ok,
            cn,
        )
    })
}

/// Tell a client why its team join was refused (the `SetTeam` wrapper). The
/// match id is the client's own identity, so a promoted game is named by its
/// own id even though it plays in the main slot.
pub fn send_password_required(client_num: c_int) {
    let id = state::with_state(|s| {
        s.tffa
            .client_match
            .get(client_num as usize)
            .map(|slot| slot_identity(s.tffa.main_origin, *slot))
            .unwrap_or(MAIN_MATCH)
    });
    tell(
        client_num,
        &format!(
            "print \"This match is password protected: use tffa_join {id} <password> to play in it. Anyone may spectate.\n\""
        ),
    );
}

/// Raw byte spans of the tokens the engine's `Cmd_TokenizeString` would parse
/// from `cmd` (`cmd_common.cpp:541-620`; the subset the redaction needs:
/// whitespace is any byte `<= ' '`, a `"` starts a quoted token that runs to the
/// closing quote or the end, and `//` / `/* */` start a comment).
fn raw_token_spans(cmd: &[u8]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut i = 0;
    loop {
        // Skip whitespace and comments, exactly where the engine does.
        loop {
            while i < cmd.len() && cmd[i] <= b' ' {
                i += 1;
            }
            if i >= cmd.len() {
                return spans;
            }
            if cmd[i] == b'/' && cmd.get(i + 1) == Some(&b'/') {
                return spans;
            }
            if cmd[i] == b'/' && cmd.get(i + 1) == Some(&b'*') {
                i += 2;
                while i + 1 < cmd.len() && !(cmd[i] == b'*' && cmd[i + 1] == b'/') {
                    i += 1;
                }
                if i + 1 >= cmd.len() {
                    return spans;
                }
                i += 2;
                continue;
            }
            break;
        }
        let start = i;
        if cmd[i] == b'"' {
            i += 1;
            while i < cmd.len() && cmd[i] != b'"' {
                i += 1;
            }
            if i < cmd.len() {
                i += 1;
            }
        } else {
            while i < cmd.len() {
                if cmd[i] <= b' ' || cmd[i] == b'"' {
                    break;
                }
                if cmd[i] == b'/' && matches!(cmd.get(i + 1), Some(b'/') | Some(b'*')) {
                    break;
                }
                i += 1;
            }
        }
        spans.push((start, i));
    }
}

/// Redact the password argument of `tffa_create`/`tffa_join` from a raw client
/// command before it is written to the verbose diagnostics log
/// (`proxy_debug.log`). Everything else is returned byte for byte, so a log
/// line still shows the command shape. Pure for the tests.
pub fn redact_client_command(cmd: &[u8]) -> Vec<u8> {
    let spans = raw_token_spans(cmd);
    let Some(&(first, first_end)) = spans.first() else {
        return cmd.to_vec();
    };
    // A quoted command name is not sent in practice, but `argv(0)` would strip
    // the quotes, so match it either way.
    let first_token = &cmd[first..first_end];
    let name = first_token
        .strip_prefix(b"\"")
        .and_then(|inner| inner.strip_suffix(b"\""))
        .unwrap_or(first_token);
    let secret_index = match name {
        b"tffa_create" => 1,
        b"tffa_join" => 2,
        _ => return cmd.to_vec(),
    };
    let Some(&(secret, secret_end)) = spans.get(secret_index) else {
        return cmd.to_vec();
    };
    let mut out = Vec::with_capacity(cmd.len());
    out.extend_from_slice(&cmd[..secret]);
    out.extend_from_slice(b"***");
    out.extend_from_slice(&cmd[secret_end..]);
    out
}

/// Resolve a `tffa_join` argument for a created match slot (`1..=max`).
///
/// Main (`0`) is handled before this call (`cmd_join` routes it through
/// `cmd_leave`), so `0` is not a valid target here.
fn join_target(id: i32, max: usize) -> Option<u8> {
    if (1..=MAX_MATCHES as i32).contains(&id) && id as usize <= max {
        Some(id as u8)
    } else {
        None
    }
}

fn tell(client_num: c_int, text: &str) {
    let Ok(cmsg) = std::ffi::CString::new(text) else {
        return;
    };
    // SAFETY: engine syscall registered; cmsg NUL-terminated.
    unsafe { crate::syscall::send_server_command(client_num, &cmsg) };
}

fn max_matches() -> usize {
    let v = state::with_state(|s| s.cvars[CVAR_TFFA_MAX_MATCHES].integer);
    // Clamp before the `as usize` cast: a negative admin value would
    // otherwise wrap huge and silently select the maximum.
    v.clamp(1, MAX_MATCHES as i32) as usize
}

fn max_group_size() -> usize {
    let v = state::with_state(|s| s.cvars[CVAR_TFFA_MAX_GROUP_SIZE].integer);
    // Same negative-wrap guard as max_matches (bounds 2..=MAX_CLIENTS).
    v.clamp(2, MAX_CLIENTS as i32) as usize
}

fn match_size(match_id: u8) -> usize {
    let table = state::with_state(|s| s.tffa.client_match);
    counted_match_size(match_id, &table, &connected_clients())
}

/// Connected clients (`pers.connected != CON_DISCONNECTED`), read from game
/// memory like the pristine `CalculateRanks`/`TeamCount`. Bots must count too
/// (they can bypass the proxy's `GAME_CLIENT_BEGIN` flag when the game adds
/// them), so the proxy's own `is_connected` mirror is only a fallback for the
/// bootstrap window where the world is not located yet.
fn connected_clients() -> [bool; MAX_CLIENTS] {
    client_census().0
}

/// Playing clients only (connected and not spectators), for the timelimit
/// continuation check.
fn playing_clients() -> [bool; MAX_CLIENTS] {
    client_census().1
}

/// Connected clients and, separately, **playing** clients (connected and not
/// on the spectator team — the pristine `numPlayingClients` filter). The
/// timelimit continuation asks whether a tied match still has someone who can
/// change its score; a match left with only spectators can never untie.
fn client_census() -> ([bool; MAX_CLIENTS], [bool; MAX_CLIENTS]) {
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
        return (
            state::with_state(|s| core::array::from_fn(|i| s.clients[i].is_connected)),
            [false; MAX_CLIENTS],
        );
    }
    let mut connected = [false; MAX_CLIENTS];
    let mut playing = [false; MAX_CLIENTS];
    for i in 0..MAX_CLIENTS {
        let ent = g_entities + i * SIZEOF_GENTITY;
        // SAFETY: ent is inside the g_entities array.
        let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
        if client == 0 {
            continue;
        }
        // SAFETY: client is a valid gclient_t; both fields are plain ints.
        let conn = unsafe { read_i32(client + OFFSET_GCLIENT_PERS + OFFSET_PERS_CONNECTED) };
        // SAFETY: as above.
        let team = unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) };
        connected[i] = conn != CON_DISCONNECTED;
        playing[i] = conn == CON_CONNECTED && team != TEAM_SPECTATOR;
    }
    (connected, playing)
}

/// Pure census of one match: connected clients assigned to it only (free and
/// disconnected slots default to main and must not inflate its player count).
fn counted_match_size(
    match_id: u8,
    client_match: &[u8; MAX_CLIENTS],
    connected: &[bool; MAX_CLIENTS],
) -> usize {
    client_match
        .iter()
        .zip(connected)
        .filter(|(m, connected)| **m == match_id && **connected)
        .count()
}

/// Pure `tffa_list` body: the main line counts only connected players in main
/// (not the free slots that also default to `MAIN_MATCH`), and every active
/// created match counts only its connected members. A password-protected match
/// is marked, including a protected game promoted into the main slot under its
/// own identity (`main_origin`).
fn render_match_list(
    max: usize,
    main_origin: u8,
    matches: &[state::TffaMatch; TOTAL_MATCH_SLOTS],
    client_match: &[u8; MAX_CLIENTS],
    connected: &[bool; MAX_CLIENTS],
) -> String {
    let mut out = String::from("Matches:");
    for (i, m) in matches.iter().enumerate() {
        let id = i as u8;
        if id != MAIN_MATCH && i > max {
            break;
        }
        if id != MAIN_MATCH && !m.active {
            continue;
        }
        let size = counted_match_size(id, client_match, connected);
        // Main is open unless it hosts a promoted protected game.
        let identity = slot_identity(main_origin, id);
        let protected = matches
            .get(identity as usize)
            .map(state::TffaMatch::has_password)
            .unwrap_or(false);
        let lock = if protected { " ^3[password]^7" } else { "" };
        if id == MAIN_MATCH {
            out.push_str(&format!(
                "\n0 (main): {size} players, {}-{}^7{lock}",
                m.red, m.blue
            ));
        } else {
            out.push_str(&format!(
                "\n{id}: {size} players, {}-{}^7{lock}",
                m.red, m.blue
            ));
        }
    }
    out.push('\n');
    out
}

/// Team a creator joins in a just-created match: the smaller of its two tally
/// sides, red on a tie (a fresh slot is 0-0, so creators start red unless a
/// stale tally somehow survived — they should not). Pure for the test.
fn create_team(red: i32, blue: i32) -> i32 {
    if blue < red { TEAM_BLUE } else { TEAM_RED }
}

/// The pristine `SetTeam` argument for a team constant.
fn team_name(team: i32) -> &'static CStr {
    if team == TEAM_BLUE { c"blue" } else { c"red" }
}

/// The slot `tffa_create` opens, or `None` when every slot up to `max` is in
/// use. Slots are `1..=max` (matching `cmd_list`); a slot is free when it is
/// inactive and no client is still assigned to it. A slot whose clients are
/// waiting to return to it — a finished match's roster (D-006.10), or a game
/// promoted into the main slot (D-006.11) — is only taken when no free slot
/// without such a group is left, so a new match does not evict a waiting group
/// while an empty slot is available. `reserved` is the id of the live promoted
/// game (it is still being played in main) and is never handed out at all.
/// Pure so the preference is unit-tested.
fn select_create_slot(
    matches: &[state::TffaMatch; TOTAL_MATCH_SLOTS],
    client_match: &[u8; MAX_CLIENTS],
    home: &[u8; MAX_CLIENTS],
    max: usize,
    reserved: u8,
) -> Option<u8> {
    let free = |i: usize| {
        i as u8 != reserved && !matches[i].active && client_match.iter().all(|c| *c != i as u8)
    };
    let waiting = |i: usize| home.iter().any(|h| *h == i as u8);
    let last = max.min(TOTAL_MATCH_SLOTS - 1);
    (1..=last)
        .find(|&i| free(i) && !waiting(i) && !matches[i].has_remembered())
        .or_else(|| (1..=last).find(|&i| free(i) && !waiting(i)))
        .or_else(|| (1..=last).find(|&i| free(i)))
        .map(|i| i as u8)
}

fn cmd_create(client_num: c_int) {
    if !enabled() {
        tell(
            client_num,
            "print \"TFFA matches are not enabled on this server (GT_TEAM only).\n\"",
        );
        return;
    }
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    // `tffa_create [password]`: a password makes the match's teams private
    // while spectating stays open. Refused before the travel below, so a bad
    // argument never kills the player for nothing.
    let mut pbuf = [0u8; MAX_TFFA_PASSWORD + 64];
    let pn = cmd_arg(1, &mut pbuf);
    if pn > MAX_TFFA_PASSWORD {
        tell(
            client_num,
            &format!(
                "print \"Password too long (max {MAX_TFFA_PASSWORD} characters); no match created.\n\""
            ),
        );
        return;
    }
    let password = &pbuf[..pn];
    // Once the shared timelimit expired the map is winding down: untied
    // matches are closed and a tied one is playing sudden death. Letting
    // someone open a fresh match would keep the map alive past the end; only
    // the surviving match may finish.
    if !create_allowed(timelimit_expired()) {
        tell(
            client_num,
            "print \"The timelimit has expired; no new TFFA match can be created. Wait for the running match to finish.\n\"",
        );
        return;
    }
    // Hoisted: `max_matches()` itself takes the state lock, and `with_state`
    // is non-reentrant — calling it inside the closure below deadlocks the
    // engine thread (the `tffa_list`/`tffa_create` server crash).
    let max = max_matches();
    let id = state::with_state(|s| {
        select_create_slot(
            &s.tffa.matches,
            &s.tffa.client_match,
            &s.tffa.home,
            max,
            s.tffa.main_origin,
        )
    });
    let Some(id) = id else {
        tell(client_num, "print \"No free match slot.\n\"");
        return;
    };
    // Travel last among the checks that can refuse: a playing client is
    // moved to spectator (which suicides them), so no refusal may come after
    // it. `tffa_create` works ingame, exactly like `tffa_join`.
    if !travel_to_spectator(client_num) {
        return;
    }
    // Leave current match first.
    cmd_leave_silent(client_num);
    // Per-instance world entities (D-006 extension): the instance's own doors,
    // plats, elevators and triggers must exist *before* it is handed a player —
    // a door left shared is activatable from every instance. Capacity is spent
    // on whole instances, so the map either pays for the complete interactive
    // set or the instance is refused.
    if let Err(reason) = realize_instance(id) {
        tell(
            client_num,
            &format!("print \"Cannot create match {id}: {reason}.\n\""),
        );
        return;
    }
    // A fresh match starts unlocked: rows from a previous match in this slot
    // must not carry over.
    crate::teamlock::clear_match(id);
    state::with_state(|s| {
        // Reusing a slot a finished group is waiting for (only done when no
        // idle slot is left) evicts that group: its clients stay in main and no
        // longer return here on the next restart.
        for home in s.tffa.home.iter_mut() {
            if *home == id {
                *home = MAIN_MATCH;
            }
        }
        let m = &mut s.tffa.matches[id as usize];
        m.active = true;
        m.red = 0;
        m.blue = 0;
        // A new match in this slot starts with no memory of the match that
        // finished here before.
        m.remembered = [REMEMBERED_NONE; MAX_CLIENTS];
        // The slot's password is the new match's: a reused slot must never
        // keep the previous match's protection or anyone's authorisation to it.
        m.set_password(password);
        for ok in s.tffa.pass_ok.iter_mut() {
            if *ok == id {
                *ok = MAIN_MATCH;
            }
        }
        if m.has_password() {
            // The creator knows the password they just set.
            s.tffa.pass_ok[client_num as usize] = id;
        }
        assign_client_instance(
            &mut s.tffa.client_match,
            &mut s.tffa.home,
            &mut s.tffa.player_score,
            &mut s.clients,
            client_num,
            id,
        );
    });
    reset_game_scores(client_num);
    // New assignment: other viewers need the prefixed view of this slot,
    // and the creator needs their own match's scores/names.
    // The creator plays from the start. A created match with nobody in game
    // is closed again by `close_empty_created_matches`, so leaving them a
    // spectator would undo the creation on the next frame; join them to the
    // smaller side (red on the fresh 0-0 slot), exactly like `/team red|blue`.
    let team = state::with_state(|s| {
        let m = &s.tffa.matches[id as usize];
        create_team(m.red, m.blue)
    });
    if !set_client_team(client_num, team_name(team)) {
        // No live game entity or no `SetTeam` trampoline: the match would be
        // empty and swept shut next frame. Undo it now and say why.
        close_instance(id);
        tell(
            client_num,
            &format!("print \"Cannot create match {id}: could not join a team.\n\""),
        );
        return;
    }
    // The creator plays here now: any finished game that still remembered them
    // forgets their return (D-006.10). The trampoline above bypasses the
    // `SetTeam` wrapper, so this cannot rely on that observer.
    forget_return_rosters(client_num);
    broadcast_cs_players_slot(client_num);
    refresh_cs_players_for_viewer(client_num);
    let side = if team == TEAM_BLUE { "BLUE" } else { "RED" };
    if password.is_empty() {
        tell(
            client_num,
            &format!(
                "print \"Created match {id} and joined {side} team. Invite others with tffa_join {id}.\n\""
            ),
        );
    } else {
        tell(
            client_num,
            &format!(
                "print \"Created password-protected match {id} and joined {side} team. Players join with tffa_join {id} <password>; anyone may spectate with tffa_join {id}.\n\""
            ),
        );
    }
}

fn cmd_join(client_num: c_int, id: i32) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    // `tffa_join 0` returns to the main game and closes the match being left
    // when nobody is left playing there. Like the other travel commands it
    // works even when the feature gate is off (a match can outlive the
    // cvar/gametype that opened it).
    if id == MAIN_MATCH as i32 {
        cmd_leave(client_num);
        return;
    }
    if !enabled() {
        tell(
            client_num,
            "print \"TFFA matches are not enabled on this server (GT_TEAM only).\n\"",
        );
        return;
    }
    let Some(id) = join_target(id, max_matches()) else {
        tell(client_num, JOIN_USAGE);
        return;
    };
    // Optional password. It is only needed to *play*: joining without one
    // still travels to the match as a spectator, which is how anyone can watch
    // a protected match.
    let mut pbuf = [0u8; MAX_TFFA_PASSWORD + 64];
    let pn = cmd_arg(2, &mut pbuf);
    let given = &pbuf[..pn];
    // Already in the game of `id`: its own runtime slot, or a promoted game
    // running in the main slot (identity `main_origin`, D-006.11). There is no
    // move, so do not put the client through the spectator step (a travel would
    // kill a playing client for nothing). `tffa_join 0` says the same through
    // `cmd_leave`.
    let runtime = client_match(client_num);
    let main_origin = state::with_state(|s| s.tffa.main_origin);
    if runtime == id || (runtime == MAIN_MATCH && main_origin == id) {
        // Already allowed to play here — an open match, or the password was
        // given before — there is nothing to do.
        if state::with_state(|s| {
            password_cleared(
                &s.tffa.matches,
                &s.tffa.client_match,
                s.tffa.main_origin,
                &s.tffa.pass_ok,
                client_num as usize,
            )
        }) {
            tell(
                client_num,
                &format!("print \"You are already in match {id}.\n\""),
            );
            return;
        }
        // A spectator already in a protected match can unlock its teams
        // without travelling: the same check a password argument makes.
        if given.is_empty() {
            tell(
                client_num,
                &format!(
                    "print \"Match {id} is password protected: you are spectating. Use tffa_join {id} <password> to play in it.\n\""
                ),
            );
            return;
        }
        let auth = authenticate_here(client_num, given);
        if auth == Auth::Wrong {
            crate::diagnostics::suspicious(
                "tffa",
                format!("password rejected for client {client_num} on match {id}"),
            );
            tell(
                client_num,
                &format!("print \"Wrong password for match {id}: you can still spectate it.\n\""),
            );
            return;
        }
        if auth == Auth::Accepted {
            tell(
                client_num,
                "print \"Password accepted: join a team with /team red|blue.\n\"",
            );
        } else {
            // `TooLong` (reported by `send_auth_outcome`) and `Open` (the
            // match lost its password meanwhile) both end here.
            send_auth_outcome(client_num, auth);
        }
        return;
    }
    let active = state::with_state(|s| s.tffa.matches[id as usize].active);
    if !active {
        if id == main_origin && main_origin != MAIN_MATCH {
            // Its game is running in the main slot (D-006.11): there is no
            // active slot to join, the client has to come to main first.
            tell(
                client_num,
                &format!(
                    "print \"The game of match {id} is running as the main game: use tffa_join 0, then tffa_join {id} <password> to play in it.\n\""
                ),
            );
        } else {
            tell(client_num, "print \"That match is not active.\n\"");
        }
        return;
    }
    // A match whose outcome is already decided is in its one-second end
    // window: joining it would only move the client back to spectator when it
    // closes.
    if match_ending(id) {
        tell(client_num, &format!("print \"Match {id} is ending.\n\""));
        return;
    }
    if match_size(id) >= max_group_size() {
        tell(client_num, "print \"That match is full.\n\"");
        return;
    }
    // Decided before the travel below: the move is not refusable, only the
    // authorisation to play is. An authorisation this client already holds for
    // the id (it gave the password before) survives the move.
    let (protected, authorised) = state::with_state(|s| {
        let already = s.tffa.pass_ok[client_num as usize] == id;
        let m = &s.tffa.matches[id as usize];
        (
            m.has_password(),
            m.has_password() && (already || m.password_matches(given)),
        )
    });
    if protected && !authorised && !given.is_empty() {
        crate::diagnostics::suspicious(
            "tffa",
            format!("password rejected for client {client_num} on match {id}"),
        );
    }
    // Every check that can refuse has passed: a playing client is moved to
    // spectator now (which suicides them), so the instance switch is the only
    // thing left.
    if !travel_to_spectator(client_num) {
        return;
    }
    cmd_leave_silent(client_num);
    state::with_state(|s| {
        assign_client_instance(
            &mut s.tffa.client_match,
            &mut s.tffa.home,
            &mut s.tffa.player_score,
            &mut s.clients,
            client_num,
            id,
        );
        if protected && authorised {
            s.tffa.pass_ok[client_num as usize] = id;
        }
    });
    // The old id's roster must not pull them back at the restart.
    forget_return_rosters(client_num);
    reset_game_scores(client_num);
    broadcast_cs_players_slot(client_num);
    refresh_cs_players_for_viewer(client_num);
    if !protected {
        tell(client_num, &format!("print \"Joined match {id}.\n\""));
    } else if authorised {
        tell(
            client_num,
            &format!("print \"Joined match {id}. You may join a team with /team red|blue.\n\""),
        );
    } else if given.is_empty() {
        tell(
            client_num,
            &format!(
                "print \"Joined match {id} as a spectator. It is password protected: use tffa_join {id} <password> to play in it.\n\""
            ),
        );
    } else if given.len() > MAX_TFFA_PASSWORD {
        tell(
            client_num,
            &format!(
                "print \"Joined match {id} as a spectator: password too long (max {MAX_TFFA_PASSWORD} characters). Use tffa_join {id} <password> to play in it.\n\""
            ),
        );
    } else {
        tell(
            client_num,
            &format!(
                "print \"Joined match {id} as a spectator: wrong password. Use tffa_join {id} <password> to play in it.\n\""
            ),
        );
    }
}

fn cmd_leave(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let (slot, home) = state::with_state(|s| {
        (
            s.tffa.client_match[client_num as usize],
            s.tffa.home[client_num as usize],
        )
    });
    // Already in main, runtime and identity: nothing to move.
    if slot == MAIN_MATCH && home == MAIN_MATCH {
        tell(client_num, "print \"You are already in the main game.\n\"");
        return;
    }
    // `tffa_join 0` works ingame too: the playing client is moved to spectator
    // first. A player whose game was promoted into main is already there at
    // runtime; leaving only drops its id, so no suicide.
    if slot != MAIN_MATCH && !travel_to_spectator(client_num) {
        return;
    }
    cmd_leave_silent(client_num);
    broadcast_cs_players_slot(client_num);
    refresh_cs_players_for_viewer(client_num);
    tell(client_num, "print \"Left your match (back to main).\n\"");
}

fn cmd_leave_silent(client_num: c_int) {
    let old = state::with_state(|s| {
        let old = s.tffa.client_match[client_num as usize];
        s.tffa.client_match[client_num as usize] = MAIN_MATCH;
        s.tffa.home[client_num as usize] = MAIN_MATCH;
        s.tffa.player_score[client_num as usize] = 0;
        old
    });
    // Leaving is an explicit move: no finished game may pull them back to
    // their old id at the next restart.
    forget_return_rosters(client_num);
    // A created match nobody is left in game in has nothing to host: close it
    // instead of leaving an empty "active" slot in `tffa_list`. Main is
    // implicit and never closes.
    close_if_empty(old);
}

/// Close created match `id` once none of its members is in game (connected
/// and off the spectator team). Main is implicit and never closes.
fn close_if_empty(id: u8) {
    if id == MAIN_MATCH || !enabled() || id as usize >= TOTAL_MATCH_SLOTS {
        return;
    }
    let (_, playing) = client_census();
    let empty = state::with_state(|s| {
        s.tffa.matches[id as usize].active
            && !s
                .tffa
                .client_match
                .iter()
                .zip(playing)
                .any(|(m, p)| p && *m == id)
    });
    if empty {
        close_instance(id);
    }
}

/// Pure part of `close_instance`: reset the slot, deactivate it and return
/// everyone still assigned (spectators) to main — runtime slot *and* identity,
/// because the id is gone — reporting the moved clients. Main and out-of-range
/// ids are a no-op.
#[allow(clippy::too_many_arguments)]
fn close_instance_state(
    matches: &mut [state::TffaMatch; TOTAL_MATCH_SLOTS],
    client_match: &mut [u8; MAX_CLIENTS],
    home: &mut [u8; MAX_CLIENTS],
    pass_ok: &mut [u8; MAX_CLIENTS],
    player_score: &mut [i32; MAX_CLIENTS],
    id: u8,
) -> Vec<c_int> {
    let mut moved = Vec::new();
    if id == MAIN_MATCH || id as usize >= TOTAL_MATCH_SLOTS {
        return moved;
    }
    for (cn, m) in client_match.iter_mut().enumerate() {
        if *m == id {
            *m = MAIN_MATCH;
            player_score[cn] = 0;
            moved.push(cn as c_int);
        }
        if home[cn] == id {
            home[cn] = MAIN_MATCH;
        }
        // The id is gone: nobody keeps an authorisation to it, so the slot can
        // be reused by a new match with its own password.
        if pass_ok[cn] == id {
            pass_ok[cn] = MAIN_MATCH;
        }
    }
    let m = &mut matches[id as usize];
    m.red = 0;
    m.blue = 0;
    m.active = false;
    m.end_queued_at = 0;
    // A closed instance remembers nobody: the slot is free for a new match.
    m.remembered = [REMEMBERED_NONE; MAX_CLIENTS];
    // Its protection dies with it (a new match in the slot sets its own).
    m.clear_password();
    moved
}

/// Close one created match: reset its tallies, free the slot and give its
/// per-instance world back at the next frame start. Remaining assigned
/// members are spectators (the caller only closes matches with nobody in
/// game), so they return to main without another `SetTeam` move; their views
/// and per-match stats are refreshed for the main game.
fn close_instance(id: u8) {
    if id == MAIN_MATCH || id as usize >= TOTAL_MATCH_SLOTS {
        return;
    }
    crate::teamlock::clear_match(id);
    let moved = state::with_state(|s| {
        s.tffa.world_release |= 1u16 << id;
        close_instance_state(
            &mut s.tffa.matches,
            &mut s.tffa.client_match,
            &mut s.tffa.home,
            &mut s.tffa.pass_ok,
            &mut s.tffa.player_score,
            id,
        )
    });
    if moved.is_empty() {
        return;
    }
    for cn in moved {
        reset_slot_stats(cn);
        broadcast_cs_players_slot(cn);
        refresh_cs_players_for_viewer(cn);
    }
    on_set_cs_scores();
}

/// Created matches with no client in game: every member left the team (or
/// left/disconnected) and only spectators remain. Pure so the per-frame sweep
/// is unit-tested; main (id 0) is never reported.
fn empty_created_matches(
    matches: &[state::TffaMatch; TOTAL_MATCH_SLOTS],
    client_match: &[u8; MAX_CLIENTS],
    playing: &[bool; MAX_CLIENTS],
) -> Vec<u8> {
    matches
        .iter()
        .enumerate()
        .filter(|(id, m)| *id != MAIN_MATCH as usize && m.active)
        .filter(|(id, _)| {
            !client_match
                .iter()
                .zip(playing)
                .any(|(cm, p)| *p && *cm == *id as u8)
        })
        .map(|(id, _)| id as u8)
        .collect()
}

/// Per-frame sweep: a created match whose last player went spectator or
/// disconnected closes itself (main stays). Catches moves made by the game
/// (`/team spectator`, forced spectator, disconnect) that no proxy command
/// sees.
fn close_empty_created_matches() {
    let (_, playing) = client_census();
    let empty = state::with_state(|s| {
        empty_created_matches(&s.tffa.matches, &s.tffa.client_match, &playing)
    });
    for id in empty {
        close_instance(id);
    }
}

fn cmd_list(client_num: c_int) {
    // Hoisted before `with_state` (same non-reentrancy as `cmd_create`).
    let max = max_matches();
    let (matches, table, main_origin) =
        state::with_state(|s| (s.tffa.matches, s.tffa.client_match, s.tffa.main_origin));
    let out = render_match_list(max, main_origin, &matches, &table, &connected_clients());
    tell(client_num, &format!("print \"{out}\""));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::{CONTENTS_SOLID, ENTITYNUM_NONE, ENTITYNUM_WORLD};

    #[test]
    fn damage_blocked_unless_same_match() {
        assert!(!damage_blocked(1, 1));
        assert!(damage_blocked(1, 2));
        // Main vs created, and vice versa.
        assert!(damage_blocked(MAIN_MATCH, 1));
        assert!(damage_blocked(1, MAIN_MATCH));
        // Main fights itself.
        assert!(!damage_blocked(MAIN_MATCH, MAIN_MATCH));
    }

    /// The pristine end-of-match window voids damage for the match that reached
    /// its limit, on both ends of the hit, and only for that match: a hit
    /// against a door/world entity (no match on that end) is still void when
    /// the player's own match is ending, and other instances keep fighting.
    #[test]
    fn damage_is_void_inside_a_matches_end_delay() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[1].active = true;
        // Instance 1 reached its fraglimit and is serving the end delay.
        matches[1].end_queued_at = 5_000;
        // Inside instance 1: victim, attacker or both.
        assert!(damage_void_in(&matches, Some(1), Some(1)));
        assert!(damage_void_in(&matches, Some(1), None));
        assert!(damage_void_in(&matches, None, Some(1)));
        // Main keeps playing: neither end belongs to the ending instance.
        assert!(!damage_void_in(
            &matches,
            Some(MAIN_MATCH),
            Some(MAIN_MATCH)
        ));
        assert!(!damage_void_in(&matches, None, None));
        // An inactive or non-ending slot never voids anything.
        matches[1].active = false;
        assert!(!damage_void_in(&matches, Some(1), Some(1)));
        matches[1].active = true;
        matches[1].end_queued_at = 0;
        assert!(!damage_void_in(&matches, Some(1), Some(1)));
    }

    #[test]
    fn entity_visible_only_same_match() {
        assert!(entity_visible(1, 1));
        assert!(!entity_visible(1, 2));
        assert!(!entity_visible(MAIN_MATCH, 1));
        assert!(entity_visible(MAIN_MATCH, MAIN_MATCH));
    }

    #[test]
    fn team_chat_stays_inside_the_senders_instance() {
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        table[1] = 1;
        table[2] = 1;
        table[3] = 2;
        // Same match: delivered (the pristine G_SayTo already handles the team).
        assert!(team_chat_visible_in(MAIN_MATCH, false, 0, &table));
        assert!(team_chat_visible_in(MAIN_MATCH, false, 4, &table));
        assert!(team_chat_visible_in(1, false, 1, &table));
        assert!(team_chat_visible_in(1, false, 2, &table));
        // Cross-match: dropped, in both directions.
        assert!(!team_chat_visible_in(MAIN_MATCH, false, 1, &table));
        assert!(!team_chat_visible_in(1, false, 0, &table));
        assert!(!team_chat_visible_in(1, false, 3, &table));
        assert!(!team_chat_visible_in(2, false, 2, &table));
        // No attributable sender (no scope) and broadcast recipients: left
        // alone, so an unscoped game-generated team chat keeps its old reach.
        assert!(team_chat_visible_in(SCOPE_NONE, false, 1, &table));
        assert!(team_chat_visible_in(1, false, -1, &table));
        assert!(team_chat_visible_in(1, false, MAX_CLIENTS as c_int, &table));
    }

    #[test]
    fn spectator_team_chat_crosses_matches() {
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        table[1] = 1;
        table[2] = 2;
        // A spectator's team chat is server-wide among spectators: the pristine
        // OnSameTeam already limits the recipients to `TEAM_SPECTATOR`, so the
        // match split must not cut a watcher off from another match's watchers.
        assert!(team_chat_visible_in(MAIN_MATCH, true, 1, &table));
        assert!(team_chat_visible_in(MAIN_MATCH, true, 2, &table));
        assert!(team_chat_visible_in(1, true, 0, &table));
        assert!(team_chat_visible_in(2, true, 0, &table));
        // The same recipients from a playing sender keep the split.
        assert!(!team_chat_visible_in(MAIN_MATCH, false, 1, &table));
        assert!(!team_chat_visible_in(1, false, 2, &table));
    }

    #[test]
    fn team_chat_commands_are_the_tchat_shapes_only() {
        assert!(is_team_chat(b"tchat \"^1Bob^7: hi\""));
        assert!(is_team_chat(b"ltchat \"Bob\" \"Yavin\" \"^4\" \"hi\""));
        // Global chat and unrelated commands are not team chat.
        assert!(!is_team_chat(b"chat \"^1Bob^7: hi\""));
        assert!(!is_team_chat(b"lchat \"Bob\" \"Yavin\" \"^4\" \"hi\""));
        assert!(!is_team_chat(b"print \"tchat hi\""));
        assert!(!is_team_chat(b"tchatty"));
        assert!(!is_team_chat(b""));
    }

    #[test]
    fn team_chat_recognition_accepts_non_utf8_chat_text() {
        // A latin-1 `é` (0xE9) is not valid UTF-8: the command must still be
        // recognised so the per-instance gate applies to it.
        assert!(is_team_chat(
            b"tchat \"\x19(alpha^7\x19)\x19: ^5CAF\xe9_ACCENT\""
        ));
        assert!(is_team_chat(
            b"ltchat \"alpha\" \"Tatooine\" \"^7\" \"caf\xe9\""
        ));
        // Only the command word matters, whatever the payload bytes are.
        assert!(is_team_chat(b"tchat \xff\xfe"));
        assert!(!is_team_chat(b"chat caf\xe9"));
        assert!(!is_team_chat(b"caf\xe9 tchat "));
    }

    #[test]
    fn sound_attribution_classifies_temp_events() {
        use SoundAttribution::*;
        assert_eq!(sound_attribution(crate::sdk::EV_GENERAL_SOUND), Stamped);
        assert_eq!(sound_attribution(crate::sdk::EV_PREDEFSOUND), Stamped);
        assert_eq!(sound_attribution(crate::sdk::EV_ENTITY_SOUND), Emitter);
        // Player respawn/teleport temp entities carry their player in
        // `s.clientNum`; they must not fall through to client 0.
        assert_eq!(
            sound_attribution(crate::sdk::EV_PLAYER_TELEPORT_IN),
            Emitter
        );
        assert_eq!(
            sound_attribution(crate::sdk::EV_PLAYER_TELEPORT_OUT),
            Emitter
        );
        assert_eq!(sound_attribution(crate::sdk::EV_GLOBAL_SOUND), Broadcast);
        assert_eq!(
            sound_attribution(crate::sdk::EV_GLOBAL_TEAM_SOUND),
            Broadcast
        );
        assert_eq!(sound_attribution(crate::sdk::EV_MUTE_SOUND), Broadcast);
        // Non-sound events keep the generic owner attribution.
        assert_eq!(sound_attribution(crate::sdk::EV_SABER_BLOCK), None);
        assert_eq!(sound_attribution(crate::sdk::EV_SABER_CLASHFLARE), None);
        assert_eq!(sound_attribution(crate::sdk::EV_OBITUARY), None);
        assert_eq!(sound_attribution(0), None);
    }

    #[test]
    fn sound_visible_world_and_same_match_only() {
        // Map/world sounds stay audible for every match.
        assert!(sound_visible(1, SOURCE_WORLD));
        assert!(sound_visible(MAIN_MATCH, SOURCE_WORLD));
        // Attributed sounds follow the source's match.
        assert!(sound_visible(1, 1));
        assert!(!sound_visible(1, 2));
        assert!(!sound_visible(MAIN_MATCH, 1));
        assert!(sound_visible(MAIN_MATCH, MAIN_MATCH));
    }

    #[test]
    fn event_owners_classifies_temp_events() {
        use EventOwners::*;
        // Reported bug: the shield shell names only `s.otherEntityNum`.
        assert_eq!(event_owners(crate::sdk::EV_SHIELD_HIT), Victim);
        // Both fields set by the creator.
        assert_eq!(event_owners(crate::sdk::EV_OBITUARY), VictimAndAttacker);
        assert_eq!(event_owners(crate::sdk::EV_SABER_HIT), VictimAndAttacker);
        assert_eq!(event_owners(crate::sdk::EV_GLOBAL_DUEL), VictimAndAttacker);
        // `s.owner`-tagged events.
        assert_eq!(event_owners(crate::sdk::EV_FORCE_DRAINED), SOwner);
        assert_eq!(event_owners(crate::sdk::EV_LOCALTIMER), SOwner);
        assert_eq!(event_owners(crate::sdk::EV_DEBRIS), SOwner);
        assert_eq!(event_owners(crate::sdk::EV_CONC_ALT_IMPACT), SOwner);
        assert_eq!(event_owners(crate::sdk::EV_VEH_FIRE), SOwner);
        assert_eq!(event_owners(crate::sdk::EV_SIEGESPEC), SOwner);
        // Set only on some creator paths: a zero is "no owner".
        assert_eq!(
            event_owners(crate::sdk::EV_DISRUPTOR_SNIPER_SHOT),
            OptionalVictim
        );
        assert_eq!(event_owners(crate::sdk::EV_SABER_BLOCK), OptionalAttacker);
        // Owner-less effects and broadcasts.
        for event in [
            crate::sdk::EV_SABER_CLASHFLARE,
            crate::sdk::EV_GLASS_SHATTER,
            crate::sdk::EV_MISSILE_MISS,
            crate::sdk::EV_DISRUPTOR_HIT,
            0,
        ] {
            assert_eq!(event_owners(event), World, "event {event}");
        }
    }

    #[test]
    fn ent_num_of_accepts_only_gentity_slots() {
        let base = 0x1000usize;
        let size = crate::sdk::SIZEOF_GENTITY;
        assert_eq!(ent_num_of(0, base), None);
        assert_eq!(ent_num_of(base, base), Some(0));
        assert_eq!(ent_num_of(base, base + size), Some(1));
        // One past the array, and an unaligned pointer, are not slots.
        assert_eq!(ent_num_of(base, base + MAX_GENTITIES * size), None);
        assert_eq!(ent_num_of(base, base + 1), None);
    }

    #[test]
    fn fraglimit_hit_shared_value() {
        assert!(fraglimit_hit(10, 3, 10));
        assert!(fraglimit_hit(3, 10, 10));
        assert!(!fraglimit_hit(9, 9, 10));
        assert!(!fraglimit_hit(10, 10, 0));
    }

    /// Reported bug: the last kill of a TFFA instance moved its players to
    /// spectator in the same frame, so the game "stopped immediately". A match
    /// now serves the pristine `INTERMISSION_DELAY_TIME` window first: for one
    /// second it keeps playing, with damage void (`match_ending`), and only
    /// then is it announced and ended.
    #[test]
    fn a_finished_match_serves_the_pristine_end_delay() {
        // The shipped `CheckExitRules` compares against 0x3e8 (`0x88b8c`).
        assert_eq!(END_DELAY_MS, 1000);
        let mut m = state::TffaMatch {
            active: true,
            red: 5,
            blue: 4,
            end_queued_at: 0,
            ..Default::default()
        };
        // The frame the limit is reached only queues the end: play continues.
        assert_eq!(queued_finish(&mut m, Some("fraglimit"), 10_000), None);
        assert_eq!(m.end_queued_at, 10_000);
        assert!(m.ending());
        // Inside the delay nothing is applied yet.
        assert_eq!(queued_finish(&mut m, Some("fraglimit"), 10_999), None);
        assert!(m.ending());
        // Exactly one second later the match is due for its announcement and
        // spectator move.
        assert_eq!(
            queued_finish(&mut m, Some("fraglimit"), 11_000),
            Some("fraglimit")
        );
        // A raised fraglimit inside the window cancels the pending end (and
        // gives damage back).
        let mut m = state::TffaMatch {
            active: true,
            red: 5,
            blue: 4,
            end_queued_at: 0,
            ..Default::default()
        };
        assert_eq!(queued_finish(&mut m, Some("fraglimit"), 10_000), None);
        assert_eq!(queued_finish(&mut m, None, 10_500), None);
        assert!(!m.ending());
        assert_eq!(m.end_queued_at, 0);
        // A tied match at the timelimit never queues an end.
        let mut m = state::TffaMatch {
            active: true,
            red: 3,
            blue: 3,
            end_queued_at: 0,
            ..Default::default()
        };
        assert_eq!(queued_finish(&mut m, None, 10_000), None);
        assert!(!m.ending());
        assert!(!any_match_ending(core::slice::from_ref(&m)));
    }

    /// A match that already reached its limit is ending, not surviving: it
    /// must not become the new main when the current one ends.
    #[test]
    fn an_ending_match_is_not_promoted_into_main() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[1].active = true;
        matches[1].red = 2;
        matches[1].blue = 1;
        matches[1].end_queued_at = 5_000;
        matches[2].active = true;
        matches[2].red = 4;
        matches[2].blue = 3;
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        let mut connected = [false; MAX_CLIENTS];
        table[0] = 1;
        connected[0] = true;
        table[1] = 2;
        connected[1] = true;
        // The lower id is ending: the surviving game is instance 2.
        let (id, red, blue, members) =
            promotion_candidate(&matches, &table, &connected, 8).expect("instance 2 survives");
        assert_eq!((id, red, blue), (2, 4, 3));
        assert_eq!(members, vec![1]);
        // Promotion clears both slots' pending ends so the new main's damage
        // is not void.
        promote_into_main(&mut matches, &mut table, id, red, blue, &members);
        assert!(!matches[MAIN_MATCH as usize].ending());
        assert!(!matches[2].ending());
        assert_eq!(table[1], MAIN_MATCH);
    }

    /// The requested timelimit rule: at the shared limit an untied match ends,
    /// a tied one continues (per-match sudden death) until a frag separates
    /// the teams. The fraglimit keeps its own semantics (a 15-15 at fraglimit
    /// 15 ends).
    #[test]
    fn timelimit_ends_untied_matches_only() {
        // Before the limit: nothing ends on time.
        assert_eq!(match_finish_reason(15, 13, 0, false), None);
        assert_eq!(match_finish_reason(15, 15, 0, false), None);
        // At the limit: untied ends, tied continues.
        assert_eq!(match_finish_reason(15, 13, 0, true), Some("timelimit"));
        assert_eq!(match_finish_reason(13, 15, 0, true), Some("timelimit"));
        assert_eq!(match_finish_reason(15, 15, 0, true), None);
        assert_eq!(match_finish_reason(0, 0, 0, true), None);
        // Shared fraglimit still ends on its own, including a tied 15-15.
        assert_eq!(match_finish_reason(15, 13, 15, false), Some("fraglimit"));
        assert_eq!(match_finish_reason(15, 15, 15, false), Some("fraglimit"));
        // A fraglimit hit is reported as such even at the limit.
        assert_eq!(match_finish_reason(15, 13, 15, true), Some("fraglimit"));
    }

    /// The requested end-of-timelimit flow: main is not tied and closes, the
    /// tied instance survives (promoted into the main slot), its game keeps
    /// waiting for a winner, and no fresh instance may be opened while the map
    /// is winding down.
    #[test]
    fn timelimit_main_ends_and_tied_instance_continues() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[MAIN_MATCH as usize].red = 5;
        matches[MAIN_MATCH as usize].blue = 3;
        matches[1].active = true;
        matches[1].red = 7;
        matches[1].blue = 7;

        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        let mut connected = [false; MAX_CLIENTS];
        let mut playing = [false; MAX_CLIENTS];
        // Main: clients 0 and 1. Instance 1: clients 2 and 3.
        for c in [0usize, 1] {
            connected[c] = true;
            playing[c] = true;
        }
        for c in [2usize, 3] {
            connected[c] = true;
            playing[c] = true;
            table[c] = 1;
        }

        // At the timelimit main (5-3) closes; instance 1 (7-7) continues.
        assert_eq!(match_finish_reason(5, 3, 0, true), Some("timelimit"));
        assert_eq!(match_finish_reason(7, 7, 0, true), None);

        // Closing main sends its players to spectator
        // (`move_match_to_spectator`), so they no longer count as playing.
        playing[0] = false;
        playing[1] = false;
        assert!(has_continuing_match(&matches, &table, &playing));

        // Main's closure promotes the lowest-id tied instance with members.
        let (id, red, blue, members) =
            promotion_candidate(&matches, &table, &connected, 8).expect("instance 1 survives");
        assert_eq!((id, red, blue), (1, 7, 7));
        assert_eq!(members, vec![2, 3]);
        promote_into_main(&mut matches, &mut table, id, red, blue, &members);

        // The surviving game now lives in the main slot; the old id is empty.
        assert!(matches[MAIN_MATCH as usize].active);
        assert_eq!(
            (
                matches[MAIN_MATCH as usize].red,
                matches[MAIN_MATCH as usize].blue
            ),
            (7, 7)
        );
        assert!(!matches[1].active);
        assert_eq!((matches[1].red, matches[1].blue), (0, 0));
        // Both old-main spectators and the surviving players share the main
        // context; only instance 1's players are still playing.
        assert_eq!(table[0], MAIN_MATCH);
        assert_eq!(table[1], MAIN_MATCH);
        assert_eq!(table[2], MAIN_MATCH);
        assert_eq!(table[3], MAIN_MATCH);
        assert!(!playing[0] && !playing[1]);
        assert!(playing[2] && playing[3]);

        // The map waits for the tied match: no intermission while it can still
        // untie.
        assert!(has_continuing_match(&matches, &table, &playing));

        // No new TFFA instance may be created after the timelimit expired;
        // before it (or with `timelimit 0`) creation stays available.
        assert!(!create_allowed(true));
        assert!(create_allowed(false));
    }

    /// The map keeps running only while a tied match still has someone who can
    /// change the score; a tied match left with spectators only cannot untie.
    #[test]
    fn only_a_tied_match_with_players_continues() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[MAIN_MATCH as usize].red = 5;
        matches[MAIN_MATCH as usize].blue = 5;
        matches[2].active = true;
        matches[2].red = 7;
        matches[2].blue = 7;
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        let mut playing = [false; MAX_CLIENTS];
        // One player in main, one in instance 2.
        table[0] = MAIN_MATCH;
        playing[0] = true;
        table[1] = 2;
        playing[1] = true;
        assert!(has_continuing_match(&matches, &table, &playing));

        // Tied main with no playing members, tied instance with playing ones:
        // the instance keeps the map alive.
        playing[0] = false;
        assert!(has_continuing_match(&matches, &table, &playing));
        // Last tied player becomes a spectator: everything is over.
        playing[1] = false;
        assert!(!has_continuing_match(&matches, &table, &playing));

        // An untied match does not continue even with players.
        matches[MAIN_MATCH as usize].blue = 4;
        playing[0] = true;
        matches[2].active = false;
        assert!(!has_continuing_match(&matches, &table, &playing));

        // A tied but inactive match does not continue.
        matches[MAIN_MATCH as usize].blue = 5;
        matches[MAIN_MATCH as usize].active = false;
        assert!(!has_continuing_match(&matches, &table, &playing));
    }

    /// The requested timelimit end (operator report: instance players were sent
    /// to spectator/main and lost their final scoreboard): when no game is tied,
    /// the **map** ends as a whole and nobody is moved out, so each viewer's
    /// intermission board stays their own instance's.
    #[test]
    fn timelimit_with_nothing_tied_ends_the_map_with_every_match_intact() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[MAIN_MATCH as usize].red = 5;
        matches[MAIN_MATCH as usize].blue = 3;
        matches[1].active = true;
        matches[1].red = 7;
        matches[1].blue = 2;
        // A tied instance nobody plays in cannot untie, so it does not keep the
        // map alive.
        matches[2].active = true;
        matches[2].red = 4;
        matches[2].blue = 4;

        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        let mut playing = [false; MAX_CLIENTS];
        for c in [0usize, 1] {
            playing[c] = true;
        }
        for c in [2usize, 3] {
            table[c] = 1;
            playing[c] = true;
        }
        for c in [4usize, 5] {
            table[c] = 2; // spectators of the tied match
        }

        // Every game is decided: the map is over, not the matches one by one.
        assert!(map_ends_at_timelimit(true, &matches, &table, &playing));
        // Before the limit nothing ends on time.
        assert!(!map_ends_at_timelimit(false, &matches, &table, &playing));
        // No slot was deactivated or reset, so the intermission capture keeps
        // each viewer's own match (instance 1's player reads its 7-2 board,
        // main's its 5-3) instead of main's.
        assert!(matches[1].active && matches[MAIN_MATCH as usize].active);
        assert_eq!(pick_board_match(MAIN_MATCH, 1, matches[1].active, true), 1);
        assert_eq!(pick_board_match(1, 1, matches[1].active, true), 1);
        assert_eq!(
            pick_board_match(
                MAIN_MATCH,
                MAIN_MATCH,
                matches[MAIN_MATCH as usize].active,
                true
            ),
            MAIN_MATCH
        );

        // A tied game with a playing member keeps the map waiting: the
        // per-match path still applies (untied instances return to main as
        // spectators).
        playing[4] = true;
        assert!(!map_ends_at_timelimit(true, &matches, &table, &playing));
    }

    /// The map-end intermission waits for every end-of-match window that is
    /// already running (the pristine damage-void second), then ends once.
    #[test]
    fn the_map_end_waits_for_an_end_of_match_window() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[MAIN_MATCH as usize].end_queued_at = 10_000;
        // Just queued: still serving its window.
        assert!(any_match_waiting(&matches, 10_000));
        assert!(any_match_waiting(&matches, 10_999));
        // Window served: the map may end.
        assert!(!any_match_waiting(&matches, 11_000));
        // A rewound clock must wait, not skip the window.
        assert!(any_match_waiting(&matches, 9_000));
        // An inactive slot's stale stamp is not a window.
        matches[MAIN_MATCH as usize].active = false;
        assert!(!any_match_waiting(&matches, 10_500));
    }

    /// Per-instance world entities (D-006 extension): a fabricated
    /// `g_entities` array in the state `G_InitGame`'s `memset` leaves it.
    struct World {
        mem: Vec<u8>,
    }

    impl World {
        fn new() -> Self {
            Self {
                mem: vec![0u8; MAX_GENTITIES * SIZEOF_GENTITY],
            }
        }

        fn base(&self) -> usize {
            self.mem.as_ptr() as usize
        }

        fn put_i32(&mut self, ent: usize, off: usize, v: i32) {
            let at = ent * SIZEOF_GENTITY + off;
            self.mem[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }

        fn put_ptr(&mut self, ent: usize, off: usize, v: usize) {
            let at = ent * SIZEOF_GENTITY + off;
            self.mem[at..at + 4].copy_from_slice(&(v as u32).to_le_bytes());
        }

        /// A live entity with the given `eType` and no other field set (an
        /// inert prop such as `misc_model_static`).
        fn spawn(&mut self, ent: usize, etype: i32) {
            self.put_i32(ent, OFFSET_GENTITY_INUSE, 1);
            self.put_i32(ent, OFFSET_GENTITY_S_ETYPE, etype);
        }
    }

    #[test]
    fn world_entity_visible_only_when_shared_or_same_instance() {
        assert!(world_entity_visible(0, ENTITY_SHARED));
        assert!(world_entity_visible(3, ENTITY_SHARED));
        assert!(world_entity_visible(0, 0));
        assert!(world_entity_visible(2, 2));
        assert!(!world_entity_visible(0, 2));
        assert!(!world_entity_visible(2, 0));
    }

    /// The decided structural predicate: triggers, movers, anything with a
    /// callback or a name, and team members are interactive; a plain
    /// key-less spawn point / static model / light is not.
    #[test]
    fn interactive_predicate_matches_the_decided_rule() {
        let mut w = World::new();
        // Inert: spawned, no keys, no callbacks.
        w.spawn(48, crate::sdk::ET_GENERAL);
        assert!(!is_interactive_entity(w.base(), 48));

        // `r.contents & CONTENTS_TRIGGER` (a `trigger_multiple` brush).
        w.put_i32(48, OFFSET_GENTITY_R_CONTENTS, CONTENTS_TRIGGER);
        assert!(is_interactive_entity(w.base(), 48));

        // `s.eType == ET_MOVER` alone (a door/plat/elevator).
        w.spawn(49, ET_MOVER);
        assert!(is_interactive_entity(w.base(), 49));

        // A callback (`touch` here; any of the five suffices).
        w.spawn(50, crate::sdk::ET_GENERAL);
        w.put_ptr(50, OFFSET_GENTITY_TOUCH, 0x0800_0000);
        assert!(is_interactive_entity(w.base(), 50));

        // A name a target can resolve (`targetname`).
        w.spawn(51, crate::sdk::ET_GENERAL);
        w.put_ptr(51, OFFSET_GENTITY_TARGETNAME, 0x0800_1000);
        assert!(is_interactive_entity(w.base(), 51));

        // A team-chain member (`teammaster`).
        w.spawn(52, crate::sdk::ET_GENERAL);
        w.put_ptr(52, OFFSET_GENTITY_TEAMMASTER, 0x0800_2000);
        assert!(is_interactive_entity(w.base(), 52));

        // Out-of-range slots and a null array are never interactive.
        assert!(!is_interactive_entity(w.base(), -1));
        assert!(!is_interactive_entity(w.base(), MAX_GENTITIES as c_int));
        assert!(!is_interactive_entity(0, 48));
    }

    /// The clone cost counts live interactive entities only: freed slots and
    /// the engine's player range never contribute.
    #[test]
    fn interactive_sources_ignore_free_and_player_slots() {
        let mut w = World::new();
        // Player range (below MAX_CLIENTS) is never scanned even when it looks
        // interactive.
        w.spawn(0, ET_MOVER);
        w.put_i32(0, OFFSET_GENTITY_R_CONTENTS, CONTENTS_TRIGGER);
        // Two live interactive entities and one inert prop above MAX_CLIENTS.
        w.spawn(48, ET_MOVER);
        w.spawn(49, crate::sdk::ET_GENERAL);
        w.put_i32(49, OFFSET_GENTITY_R_CONTENTS, CONTENTS_TRIGGER);
        w.spawn(50, crate::sdk::ET_GENERAL);
        // A freed slot that still carries interactive-looking fields.
        w.put_ptr(51, OFFSET_GENTITY_TARGETNAME, 0x0800_3000);
        assert_eq!(interactive_sources(w.base()), vec![48, 49]);
        assert!(interactive_sources(0).is_empty());
    }

    /// An NPC/actor cannot be duplicated (its Ghoul2 model and `client` are
    /// shared): a map holding one takes no instance at all, while an ordinary
    /// door/trigger can.
    #[test]
    fn duplicable_entities_exclude_npc_actors() {
        let mut w = World::new();
        let npc = crate::sdk::ET_NPC;
        // A plain mover, an item (CONTENTS_TRIGGER), and a trigger brush.
        w.spawn(48, ET_MOVER);
        assert!(is_duplicable_entity(w.base(), 48));
        w.spawn(49, crate::sdk::ET_GENERAL);
        w.put_i32(49, OFFSET_GENTITY_R_CONTENTS, CONTENTS_TRIGGER);
        assert!(is_duplicable_entity(w.base(), 49));
        // An NPC tells on itself by its eType...
        w.spawn(50, npc);
        assert!(!is_duplicable_entity(w.base(), 50));
        // ... and so does any entity with a Ghoul2 model or a dynamic client.
        w.spawn(51, ET_MOVER);
        w.put_ptr(51, OFFSET_GENTITY_GHOUL2, 0x0900_0000);
        assert!(!is_duplicable_entity(w.base(), 51));
        w.spawn(52, ET_MOVER);
        w.put_ptr(52, OFFSET_GENTITY_CLIENT, 0x0900_1000);
        assert!(!is_duplicable_entity(w.base(), 52));
    }

    /// The collision gate (S5): a trace by one instance's player ignores every
    /// other instance's world entities — main's originals included — while the
    /// requester's own copies and untagged shared props stay solid. Entities
    /// whose contents do not intersect the trace's `contentmask` are left
    /// alone, exactly as the engine skips them.
    #[test]
    fn trace_flips_cover_foreign_instances_only() {
        let mut w = World::new();
        let mut t = crate::state::TffaState::default();
        // Client 0 plays match 1 (the requester), client 1 plays match 2.
        w.spawn(0, 1);
        w.put_ptr(0, OFFSET_GENTITY_CLIENT, 0x0800_0000);
        w.put_i32(0, OFFSET_GENTITY_R_CONTENTS, CONTENTS_SOLID);
        w.spawn(1, 1);
        w.put_ptr(1, OFFSET_GENTITY_CLIENT, 0x0800_0100);
        w.put_i32(1, OFFSET_GENTITY_R_CONTENTS, CONTENTS_SOLID);
        t.client_match[0] = 1;
        t.client_match[1] = 2;

        // Main's original door (tag 0), the requester's own door (tag 1), a
        // foreign door (tag 2), and an untagged solid prop.
        for (slot, tag, contents) in [
            (48usize, MAIN_MATCH, CONTENTS_SOLID),
            (49, 1, CONTENTS_SOLID),
            (50, 2, CONTENTS_SOLID),
            (51, ENTITY_SHARED, CONTENTS_SOLID),
        ] {
            w.spawn(slot, ET_MOVER);
            w.put_i32(slot, OFFSET_GENTITY_R_CONTENTS, contents);
            t.entity_instance[slot] = tag;
        }
        // A foreign instance's trigger brush: invisible to a solid mask.
        w.spawn(52, crate::sdk::ET_GENERAL);
        w.put_i32(52, OFFSET_GENTITY_R_CONTENTS, CONTENTS_TRIGGER);
        t.entity_instance[52] = 2;

        let mut out = Vec::new();
        let n = flip_list(&t, w.base(), 0, CONTENTS_SOLID, &mut out);
        assert_eq!(n, 3, "foreign player, main's door, foreign door");
        let mut flipped: Vec<usize> = out.iter().map(|(addr, _)| *addr).collect();
        flipped.sort_unstable();
        let mut want = vec![
            w.base() + SIZEOF_GENTITY + OFFSET_GENTITY_R_CONTENTS,
            w.base() + 48 * SIZEOF_GENTITY + OFFSET_GENTITY_R_CONTENTS,
            w.base() + 50 * SIZEOF_GENTITY + OFFSET_GENTITY_R_CONTENTS,
        ];
        want.sort_unstable();
        assert_eq!(flipped, want);
        // Saved values are the live contents, so the restore is exact.
        for (_, saved) in &out {
            assert_eq!(*saved, CONTENTS_SOLID);
        }

        // A trigger-mask trace (what `G_TouchTriggers` uses) picks the foreign
        // trigger instead: the doors carry no `CONTENTS_TRIGGER` bit.
        let n = flip_list(&t, w.base(), 0, CONTENTS_TRIGGER, &mut out);
        assert_eq!(n, 1);
        assert_eq!(
            out[0],
            (
                w.base() + 52 * SIZEOF_GENTITY + OFFSET_GENTITY_R_CONTENTS,
                CONTENTS_TRIGGER
            )
        );
    }

    /// A trace nobody owns (`ENTITYNUM_WORLD`/`ENTITYNUM_NONE`, out of range,
    /// or a freed requester slot whose zeroed `r.ownerNum` would read as client
    /// 0) filters nothing — the world stays solid for it, as before this
    /// feature. A live owned entity still resolves through `r.ownerNum`.
    #[test]
    fn trace_flips_ignore_world_and_freed_requesters() {
        let mut w = World::new();
        let mut t = crate::state::TffaState::default();
        w.spawn(0, 1);
        w.put_ptr(0, OFFSET_GENTITY_CLIENT, 0x0800_0000);
        t.client_match[0] = 1;
        // A foreign door that would be flipped if the requester resolved.
        w.spawn(50, ET_MOVER);
        w.put_i32(50, OFFSET_GENTITY_R_CONTENTS, CONTENTS_SOLID);
        t.entity_instance[50] = 2;

        let mut out = Vec::new();
        // A freed ordinary slot: `inuse == 0` and `r.ownerNum == 0`.
        w.put_i32(70, OFFSET_GENTITY_INUSE, 0);
        // A live missile of client 0's match.
        w.spawn(80, 1);
        w.put_i32(80, OFFSET_GENTITY_R_OWNER, 0);
        for pass in [ENTITYNUM_WORLD, ENTITYNUM_NONE, -1, 1024, 70] {
            assert_eq!(
                flip_list(&t, w.base(), pass, CONTENTS_SOLID, &mut out),
                0,
                "passEntityNum {pass}"
            );
        }
        // The live missile resolves to its owner's match, so main's door (tag
        // 0, another instance) is flipped for its trace.
        assert_eq!(flip_list(&t, w.base(), 80, CONTENTS_SOLID, &mut out), 1);
        assert_eq!(
            out[0],
            (
                w.base() + 50 * SIZEOF_GENTITY + OFFSET_GENTITY_R_CONTENTS,
                CONTENTS_SOLID
            )
        );
    }

    /// The touch gate (S6) resolves its requester from the calling entity: only a
    /// live client slot has one (pristine `G_TouchTriggers` returns immediately
    /// for anything else, `g_active.c:524-531`), and it is that player's own
    /// match — the instance their body is in.
    #[test]
    fn touch_requester_resolves_only_live_client_slots() {
        let mut w = World::new();
        let mut t = crate::state::TffaState::default();
        let base = w.base();
        let ent = |slot: usize| base + slot * SIZEOF_GENTITY;
        w.spawn(3, 1);
        w.put_ptr(3, OFFSET_GENTITY_CLIENT, 0x0800_0000);
        t.client_match[3] = 5;
        assert_eq!(touch_requester(&t, base, ent(3)), Some(5));
        // Main is a match like any other.
        w.spawn(4, 1);
        w.put_ptr(4, OFFSET_GENTITY_CLIENT, 0x0800_0100);
        assert_eq!(touch_requester(&t, base, ent(4)), Some(MAIN_MATCH));
        // A freed client slot (no `client`), a non-player slot above the player
        // range, a pointer below the array, and a null array resolve to none.
        w.spawn(5, 1);
        assert_eq!(touch_requester(&t, base, ent(5)), None);
        w.spawn(48, crate::sdk::ET_GENERAL);
        w.put_ptr(48, OFFSET_GENTITY_CLIENT, 0x0800_0200);
        assert_eq!(touch_requester(&t, base, ent(48)), None);
        assert_eq!(touch_requester(&t, base, base - SIZEOF_GENTITY), None);
        assert_eq!(touch_requester(&t, 0, ent(3)), None);
    }

    /// The window helpers (S5/S6): `flip_apply` zeroes exactly the collected
    /// slots, and `flip_restore` puts the saved value back only while the slot is
    /// still ours — in use and still showing the `0` we wrote. A slot the game
    /// freed inside the window, or one whose contents the game changed meanwhile,
    /// is left alone (no stale write, no resurrection).
    #[test]
    fn flip_apply_and_restore_are_exact_but_never_resurrect() {
        let mut w = World::new();
        let contents = |w: &World, slot: usize| {
            let at = slot * SIZEOF_GENTITY + OFFSET_GENTITY_R_CONTENTS;
            i32::from_le_bytes(w.mem[at..at + 4].try_into().unwrap())
        };
        let mut flips = Vec::new();
        for slot in [48usize, 49, 50] {
            w.spawn(slot, ET_MOVER);
            w.put_i32(slot, OFFSET_GENTITY_R_CONTENTS, CONTENTS_SOLID);
            flips.push((
                w.base() + slot * SIZEOF_GENTITY + OFFSET_GENTITY_R_CONTENTS,
                CONTENTS_SOLID,
            ));
        }
        flip_apply(&flips);
        for slot in [48usize, 49, 50] {
            assert_eq!(contents(&w, slot), 0, "slot {slot} zeroed");
        }
        // Slot 49 is freed inside the window, slot 50 is re-armed by the game.
        w.put_i32(49, OFFSET_GENTITY_INUSE, 0);
        w.put_i32(50, OFFSET_GENTITY_R_CONTENTS, CONTENTS_TRIGGER);
        flip_restore(&flips);
        assert_eq!(contents(&w, 48), CONTENTS_SOLID, "ours, restored");
        assert_eq!(contents(&w, 49), 0, "freed: not resurrected");
        assert_eq!(contents(&w, 50), CONTENTS_TRIGGER, "game's value kept");
    }

    /// The acting-instance resolution the acting scope uses for both of its
    /// gates (S7 link, S8 runtime-spawn): a live player slot through `client_match` (as the touch gate does), a
    /// tagged world entity through its instance tag — main included — and
    /// nothing for an actor that carries no instance (shared/untagged, freed, the
    /// world/NONE slots, a pointer outside the array).
    #[test]
    fn acting_instance_resolves_the_firing_entitys_instance() {
        let mut w = World::new();
        let mut t = crate::state::TffaState::default();
        let base = w.base();
        let ent = |slot: usize| base + slot * SIZEOF_GENTITY;

        // A live player in instance 3 fires through that player's match.
        w.spawn(2, 1);
        w.put_ptr(2, OFFSET_GENTITY_CLIENT, 0x0800_0000);
        t.client_match[2] = 3;
        assert_eq!(acting_instance(&t, base, ent(2)), Some(3));

        // A tagged world entity (a cloned trigger) resolves to its own instance.
        w.spawn(48, ET_MOVER);
        t.entity_instance[48] = 1;
        assert_eq!(acting_instance(&t, base, ent(48)), Some(1));
        w.spawn(49, ET_MOVER);
        t.entity_instance[49] = MAIN_MATCH;
        assert_eq!(acting_instance(&t, base, ent(49)), Some(MAIN_MATCH));

        // Untagged (shared) and freed entities, the world slots, a client slot
        // without a client, a pointer below the array and a null array: no
        // instance, so the call keeps the scope it was already in.
        w.spawn(50, ET_MOVER);
        assert_eq!(acting_instance(&t, base, ent(50)), None);
        w.spawn(51, ET_MOVER);
        t.entity_instance[51] = 2;
        w.put_i32(51, OFFSET_GENTITY_INUSE, 0);
        assert_eq!(acting_instance(&t, base, ent(51)), None, "freed slot");
        w.spawn(3, 1);
        assert_eq!(acting_instance(&t, base, ent(3)), None, "no client");
        assert_eq!(
            acting_instance(&t, base, ent(ENTITYNUM_WORLD as usize)),
            None
        );
        assert_eq!(
            acting_instance(&t, base, ent(ENTITYNUM_NONE as usize)),
            None
        );
        assert_eq!(acting_instance(&t, base, base - SIZEOF_GENTITY), None);
        assert_eq!(acting_instance(&t, 0, ent(49)), None);
    }

    /// An untagged entity that carries a live-player owner acts in its owner's
    /// instance — the resolution the trace gate uses for missiles, saber
    /// entities, physics objects and bodies (S9). An owner that is not a live
    /// player (the world, a freed client slot, `ENTITYNUM_NONE`, which is what a
    /// freshly spawned entity carries) resolves to nothing.
    #[test]
    fn acting_instance_follows_the_owner_of_an_untagged_entity() {
        let mut w = World::new();
        let mut t = crate::state::TffaState::default();
        let base = w.base();
        let ent = |slot: usize| base + slot * SIZEOF_GENTITY;
        t.client_match[3] = 2;
        w.spawn(3, 1);
        w.put_ptr(3, OFFSET_GENTITY_CLIENT, 0x0800_0000);

        // A missile fired by instance 2's client 3.
        w.spawn(60, crate::sdk::ET_GENERAL);
        w.put_i32(60, OFFSET_GENTITY_R_OWNER, 3);
        assert_eq!(acting_instance(&t, base, ent(60)), Some(2));

        // A physics object with no owner (as `G_Spawn` leaves one).
        w.spawn(61, crate::sdk::ET_GENERAL);
        assert_eq!(acting_instance(&t, base, ent(61)), None);
        w.put_i32(61, OFFSET_GENTITY_R_OWNER, ENTITYNUM_NONE);
        assert_eq!(acting_instance(&t, base, ent(61)), None);
        // Owner pointing at a client slot that holds no client.
        w.put_i32(61, OFFSET_GENTITY_R_OWNER, 4);
        assert_eq!(acting_instance(&t, base, ent(61)), None);
        // A freed owner slot owns nothing.
        w.put_i32(61, OFFSET_GENTITY_R_OWNER, 3);
        w.put_ptr(3, OFFSET_GENTITY_CLIENT, 0);
        assert_eq!(acting_instance(&t, base, ent(61)), None);
        // A tagged entity never falls back to its owner: the tag wins.
        t.entity_instance[61] = 1;
        w.put_ptr(3, OFFSET_GENTITY_CLIENT, 0x0800_0000);
        assert_eq!(acting_instance(&t, base, ent(61)), Some(1));
    }

    /// A box query made inside an acting scope keeps the scope's own entities and
    /// every shared/untagged slot, and drops the entries tagged to another
    /// instance *plus the players of another instance* (S11 spawn separation):
    /// a foreign match's player is not in this match's world, exactly as the
    /// visibility and trace gates treat them.
    #[test]
    fn box_list_drops_foreign_entities_and_players() {
        let mut tags = [ENTITY_SHARED; MAX_GENTITIES];
        tags[48] = 1; // instance 1's door
        tags[49] = 2; // instance 2's own
        tags[51] = MAIN_MATCH; // main's original
        // 50 stays shared; player 3 is instance 2's, player 4 is main's,
        // player 5 is instance 1's; 1023/`-1` are not slots at all.
        let mut client_match = [MAIN_MATCH; MAX_CLIENTS];
        client_match[3] = 2;
        client_match[4] = MAIN_MATCH;
        client_match[5] = 1;
        let mut list = [48, 49, 51, 50, 3, 4, 5, ENTITYNUM_NONE as c_int, -1];
        assert_eq!(retain_visible_slots(&tags, &client_match, &mut list, 2), 5);
        assert_eq!(
            &list[..5],
            &[49, 50, 3, ENTITYNUM_NONE as c_int, -1],
            "own instance and shared entries kept; foreign entities and players dropped"
        );
        // Main's scope hides both instances' copies and both players but not its
        // own original or its own player.
        let mut list = [48, 49, 51, 50, 4, 5];
        assert_eq!(
            retain_visible_slots(&tags, &client_match, &mut list, MAIN_MATCH),
            3
        );
        assert_eq!(&list[..3], &[51, 50, 4]);
        // A scope that owns nothing keeps only the shared entry.
        let mut list = [48, 49, 51, 50, 3];
        assert_eq!(retain_visible_slots(&tags, &client_match, &mut list, 5), 1);
        assert_eq!(&list[..1], &[50]);
    }

    /// The world-query rule hides foreign tagged entities and foreign players,
    /// keeps own/shared slots, and leaves the non-slot numbers (the world,
    /// `NONE`, negative) alone — the rule the contact gate applies to a single
    /// entity and the box filter to a list.
    #[test]
    fn world_query_hides_foreign_players_but_keeps_own_and_shared() {
        let mut tags = [ENTITY_SHARED; MAX_GENTITIES];
        tags[48] = 1;
        tags[49] = 2;
        let mut client_match = [MAIN_MATCH; MAX_CLIENTS];
        client_match[0] = 2;
        client_match[1] = 1;
        client_match[2] = MAIN_MATCH;

        assert!(!query_hides_num(&tags, &client_match, 0, 2), "own player");
        assert!(
            query_hides_num(&tags, &client_match, 1, 2),
            "instance 1's player"
        );
        assert!(query_hides_num(&tags, &client_match, 2, 2), "main's player");
        assert!(query_hides_num(&tags, &client_match, 48, 2), "foreign tag");
        assert!(!query_hides_num(&tags, &client_match, 49, 2), "own tag");
        tags[50] = ENTITY_SHARED;
        assert!(!query_hides_num(&tags, &client_match, 50, 2), "shared");
        assert!(
            !query_hides_num(&tags, &client_match, ENTITYNUM_NONE as c_int, 2),
            "NONE is not a slot"
        );
        assert!(!query_hides_num(&tags, &client_match, -1, 2));
        // Main's scope keeps its own player and hides both instances' players.
        assert!(!query_hides_num(&tags, &client_match, 2, MAIN_MATCH));
        assert!(query_hides_num(&tags, &client_match, 0, MAIN_MATCH));
        assert!(query_hides_num(&tags, &client_match, 1, MAIN_MATCH));
        // No scope never hides a player (the gates forward pristine instead).
        assert!(!query_hides_num(&tags, &client_match, 0, SCOPE_NONE));
    }

    /// The acting scope hides exactly the entities tagged to another instance:
    /// the firing instance's own copies and everything shared (untagged props,
    /// players) stay reachable, so a chain resolves to its instance's targets and
    /// no game code is reimplemented.
    #[test]
    fn acting_scope_hides_only_foreign_tagged_entities() {
        let w = World::new();
        let mut tags = [ENTITY_SHARED; MAX_GENTITIES];
        let base = w.base();
        let ent = |slot: usize| base + slot * SIZEOF_GENTITY;
        tags[48] = MAIN_MATCH;
        tags[49] = 1;
        tags[50] = 2;
        tags[51] = ENTITY_SHARED;

        // A chain fired by instance 2's entity reaches instance 2's targets, and
        // is out of reach of main's original, instance 1's clone and nobody
        // else's — while a shared prop stays reachable from every instance.
        assert!(!link_hides_tag(&tags, base, ent(50), 2), "own clone");
        assert!(
            link_hides_tag(&tags, base, ent(49), 2),
            "instance 1's clone"
        );
        assert!(link_hides_tag(&tags, base, ent(48), 2), "main's original");
        assert!(!link_hides_tag(&tags, base, ent(51), 2), "shared stays");
        // A chain fired by main reaches main's own targets only.
        assert!(!link_hides_tag(&tags, base, ent(48), MAIN_MATCH));
        assert!(link_hides_tag(&tags, base, ent(49), MAIN_MATCH));
        assert!(link_hides_tag(&tags, base, ent(50), MAIN_MATCH));
        // Players and out-of-range slots are never hidden.
        assert!(!link_hides_tag(&tags, base, ent(3), 2));
        assert!(!link_hides_tag(
            &tags,
            base,
            ent(ENTITYNUM_WORLD as usize),
            2
        ));
        assert!(!link_hides_tag(&tags, base, base - SIZEOF_GENTITY, 2));
        assert!(!link_hides_tag(&tags, 0, ent(49), 2));
    }

    /// The windowed search walks pristine `G_Find` results and returns the first
    /// one the window does not hide, resuming past each hidden match; `NULL`
    /// ends it. The stub stands in for the pristine search, which always
    /// advances and starts at the array head for a null `from`.
    #[test]
    fn find_visible_skips_hidden_matches_and_resumes() {
        let w = World::new();
        let mut tags = [ENTITY_SHARED; MAX_GENTITIES];
        let base = w.base();
        tags[48] = MAIN_MATCH;
        tags[49] = 2;
        tags[50] = 1;
        tags[51] = ENTITY_SHARED;
        let found: Vec<usize> = [48usize, 49, 50, 51]
            .iter()
            .map(|&slot| base + slot * SIZEOF_GENTITY)
            .collect();
        let next = |cur: usize| {
            let after = if cur == 0 { base - 1 } else { cur };
            found.iter().copied().find(|f| *f > after).unwrap_or(0)
        };
        let visible =
            |window: u8| find_visible(0, next, |f| link_hides_tag(&tags, base, f, window));
        // Instance 2: main's original is hidden, its own clone comes first, then
        // the shared prop; instance 1's clone stays hidden throughout.
        assert_eq!(visible(2), base + 49 * SIZEOF_GENTITY);
        assert_eq!(
            find_visible(base + 49 * SIZEOF_GENTITY, next, |f| link_hides_tag(
                &tags, base, f, 2
            )),
            base + 51 * SIZEOF_GENTITY
        );
        assert_eq!(
            find_visible(base + 51 * SIZEOF_GENTITY, next, |f| link_hides_tag(
                &tags, base, f, 2
            )),
            0
        );
        // Instance 1 sees only its own, main only main's.
        assert_eq!(visible(1), base + 50 * SIZEOF_GENTITY);
        assert_eq!(visible(MAIN_MATCH), base + 48 * SIZEOF_GENTITY);
    }

    /// One further instance costs `interactive_sources().len()` slots; capacity
    /// is the free ordinary range divided by that cost.
    #[test]
    fn free_world_slots_covers_the_ordinary_range_only() {
        let mut w = World::new();
        assert_eq!(
            free_world_slots(w.base()),
            (ENTITYNUM_MAX_NORMAL - MAX_CLIENTS as i32) as usize
        );
        // Occupying the player range changes nothing.
        w.spawn(0, ET_MOVER);
        // Occupying an ordinary slot removes exactly one.
        w.spawn(48, ET_MOVER);
        assert_eq!(
            free_world_slots(w.base()),
            (ENTITYNUM_MAX_NORMAL - MAX_CLIENTS as i32) as usize - 1
        );
        assert_eq!(free_world_slots(0), 0);
    }

    /// Re-creating a slot before its queued world release runs must cancel the
    /// release: otherwise `drain_world_releases` would free the clones the new
    /// match just started using. A live set is reported for reuse.
    #[test]
    fn reactivating_a_slot_cancels_its_pending_world_release() {
        let mut pending = (1u16 << 3) | (1u16 << 4);
        assert!(claim_world(&mut pending, 1 << 3, 3));
        assert_eq!(pending, 1 << 4);
        // No live set: no reuse, but a stale pending bit is still cleared.
        assert!(!claim_world(&mut pending, 0, 4));
        assert_eq!(pending, 0);
        // Already released (no bit, no set): claim is a no-op.
        assert!(!claim_world(&mut pending, 0, 5));
        assert_eq!(pending, 0);
    }

    fn sample_entry(client: i32, score: i32) -> Vec<i32> {
        vec![client, score, 50, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    }

    #[test]
    fn filter_scores_unified_roster_with_per_player_scores() {
        let mut entries = sample_entry(0, 5);
        entries.extend(sample_entry(1, 7));
        entries.extend(sample_entry(4, 9));
        let pscores = [3; MAX_CLIENTS];
        // Unified roster: every client is listed (so a viewer can see who is
        // in the other matches), global PERS_SCORE values (5, 7, 9) replaced
        // by the per-match copies (3); the header is the viewer's match.
        let out = filter_scores(11, 22, &pscores, &entries);
        let nums: Vec<i32> = out
            .strip_prefix("scores ")
            .unwrap()
            .split_whitespace()
            .map(|t| t.parse().unwrap())
            .collect();
        assert_eq!(&nums[..3], &[3, 11, 22]);
        assert_eq!(nums.len(), 3 + 3 * 14);
        assert_eq!(nums[3], 0);
        assert_eq!(nums[3 + SCORE_ENTRY_SCORE], 3);
        assert_eq!(nums[3 + 14], 1);
        assert_eq!(nums[3 + 14 + SCORE_ENTRY_SCORE], 3);
        assert_eq!(nums[3 + 28], 4);
        assert_eq!(nums[3 + 28 + SCORE_ENTRY_SCORE], 3);

        // An entry naming a client outside the table is passed through
        // untouched (the game never emits one).
        let odd = sample_entry(MAX_CLIENTS as i32 + 3, 9);
        let out = filter_scores(0, 0, &pscores, &odd);
        assert_eq!(
            out,
            format!(
                "scores 1 0 0 {}",
                odd.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        );
    }

    #[test]
    fn intermission_board_keeps_the_watched_match_only_when_active() {
        // Normal play: the follow-aware match always wins.
        assert_eq!(pick_board_match(0, 2, true, false), 0);
        // Intermission: the captured board wins while the match is active
        // (a spectator watching match 2's 15-13 finish keeps that board).
        assert_eq!(pick_board_match(MAIN_MATCH, 2, true, true), 2);
        // A finished (reset) match falls back to the viewer's own match.
        assert_eq!(pick_board_match(MAIN_MATCH, 2, false, true), MAIN_MATCH);
    }

    #[test]
    fn temp_stamp_prefers_the_sound_handshake_then_the_scope() {
        use crate::sdk::EV_GENERAL_SOUND;
        // `G_Sound`'s pending attribution always wins (even for another event).
        assert_eq!(temp_stamp_source(1, EV_GENERAL_SOUND, SCOPE_NONE), 1);
        assert_eq!(
            temp_stamp_source(1, crate::sdk::EV_PREDEFSOUND, MAIN_MATCH),
            1
        );
        // An owner-less `EV_PREDEFSOUND` takes the open acting scope.
        assert_eq!(
            temp_stamp_source(SOURCE_WORLD, crate::sdk::EV_PREDEFSOUND, 2),
            2
        );
        // No scope (or another event) stays a world sound.
        assert_eq!(
            temp_stamp_source(SOURCE_WORLD, crate::sdk::EV_PREDEFSOUND, SCOPE_NONE),
            SOURCE_WORLD
        );
        assert_eq!(
            temp_stamp_source(SOURCE_WORLD, EV_GENERAL_SOUND, 2),
            SOURCE_WORLD
        );
    }

    #[test]
    fn the_proxy_defers_sends_once_the_reliable_window_is_nearly_full() {
        // No engine client (unit tests / feature off): never pressured.
        assert!(!backlog_is_pressured(None));
        // An acknowledged client (or one command in flight) has room.
        assert!(!backlog_is_pressured(Some(0)));
        assert!(!backlog_is_pressured(Some(1)));
        // Just under the limit still sends.
        assert!(!backlog_is_pressured(Some(RELIABLE_SEND_LIMIT - 1)));
        // At the limit the proxy stops and defers to a later frame.
        assert!(backlog_is_pressured(Some(RELIABLE_SEND_LIMIT)));
        // The shipped engine's own overflow point (128 pending + the one it
        // is about to add) is far past the proxy's threshold.
        assert!(backlog_is_pressured(Some(129)));
        // The threshold leaves the pristine game real headroom in the 128-slot
        // window for its own per-client `map_restart` broadcasts.
        const { assert!(RELIABLE_SEND_LIMIT > 0 && RELIABLE_SEND_LIMIT < 128) };
    }

    #[test]
    fn a_single_match_needs_no_per_viewer_player_override() {
        // Fix A: same-match slots are not re-sent at connect (they already hold
        // the base string from the gamestate / the game's own broadcast), so a
        // main-only server queues no `cs` per slot during the reconnect burst.
        let base = "n\\Bob\\t\\2\\model\\kyle";
        let mut bases: [String; MAX_CLIENTS] = core::array::from_fn(|_| String::new());
        bases[0] = base.to_owned();
        bases[1] = base.to_owned();
        let table = [MAIN_MATCH; MAX_CLIENTS];
        assert!(demotions_for_viewer(MAIN_MATCH, &table, &bases).is_empty());
        // A real spectator watching main is own too.
        let mut bases2: [String; MAX_CLIENTS] = core::array::from_fn(|_| String::new());
        bases2[0] = "n\\Spec\\t\\3\\model\\kyle".to_owned();
        assert!(demotions_for_viewer(MAIN_MATCH, &table, &bases2).is_empty());
    }

    #[test]
    fn scoreboard_demotions_cover_exactly_the_playing_outsiders() {
        // Real CS_PLAYERS shape captured from a running GT_TEAM server.
        let blue_main = "n\\TIE Pilot\\t\\2\\model\\stormpilot\\c1\\0\\c2\\0\\hc\\100\\w\\0\\l\\0\\skill\\ 0.00\\tt\\0\\tl\\0\\siegeclass\\none\\st\\single_2\\st2\\none\\dt\\0\\sdt\\0";
        let red_instance = "n\\Alora\\t\\1\\model\\alora\\c1\\0\\c2\\0\\hc\\100\\w\\0\\l\\0\\skill\\ 0.00\\tt\\0\\tl\\0\\siegeclass\\none\\st\\single_4\\st2\\single_4\\dt\\0\\sdt\\0";
        let real_spectator = "n\\Spec\\t\\3\\model\\kyle\\c1\\0\\c2\\0";
        let mut bases: [String; MAX_CLIENTS] = core::array::from_fn(|_| String::new());
        bases[0] = blue_main.to_owned(); // plays main, blue
        bases[1] = red_instance.to_owned(); // plays instance 1, red
        bases[2] = real_spectator.to_owned(); // spectator in main
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        table[1] = 1;

        // Instance-1 viewer: the main player must be demoted (prefixed +
        // spectator), the instance-1 player is own, the real spectator is
        // already spectator and stays untouched.
        let out = demotions_for_viewer(1, &table, &bases);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].0, 0);
        assert_eq!(
            info_value(&out[0].1, "n").as_deref(),
            Some("(TFFA 0) TIE Pilot")
        );
        assert_eq!(info_value(&out[0].1, "t").as_deref(), Some("3"));
        // The rest of the info string survives (model/colors/team task ...).
        assert_eq!(
            info_value(&out[0].1, "model").as_deref(),
            Some("stormpilot")
        );
        assert_eq!(info_value(&out[0].1, "st2").as_deref(), Some("none"));

        // Main viewer: only instance-1's player is an outsider.
        let out = demotions_for_viewer(MAIN_MATCH, &table, &bases);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].0, 1);
        assert_eq!(
            info_value(&out[0].1, "n").as_deref(),
            Some("(TFFA 1) Alora")
        );
        assert_eq!(info_value(&out[0].1, "t").as_deref(), Some("3"));
    }

    #[test]
    fn the_scoreboard_refresh_covers_both_own_and_outsider_slots() {
        // The refresh must carry every known slot, not only the demotions: a
        // slot that moved into the viewer's match needs its base string back.
        // (The per-viewer dedup turns the unchanged entries into no-ops.)
        let own = "n\\Bob\\t\\1\\model\\kyle";
        let outsider = "n\\Alora\\t\\2\\model\\alora";
        let mut bases: [String; MAX_CLIENTS] = core::array::from_fn(|_| String::new());
        bases[0] = own.to_owned();
        bases[1] = outsider.to_owned();
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        table[1] = 1;

        let out = desired_cs_players(MAIN_MATCH, &table, &bases);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0], (0, own.to_owned()));
        assert_eq!(out[1].0, 1);
        assert_eq!(
            info_value(&out[1].1, "n").as_deref(),
            Some("(TFFA 1) Alora")
        );
        assert_eq!(info_value(&out[1].1, "t").as_deref(), Some("3"));

        // Instance-1's viewer: its own slot keeps the base, main's is demoted.
        let out = desired_cs_players(1, &table, &bases);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(info_value(&out[0].1, "n").as_deref(), Some("(TFFA 0) Bob"));
        assert_eq!(out[1], (1, outsider.to_owned()));

        // Unknown (empty) bases are skipped, never pushed as empty strings.
        let mut empty: [String; MAX_CLIENTS] = core::array::from_fn(|_| String::new());
        empty[3] = "n\\Spec\\t\\3".to_owned();
        let out = desired_cs_players(MAIN_MATCH, &table, &empty);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 3);
    }

    #[test]
    fn a_steady_roster_needs_no_second_player_override_push() {
        // Mirror of the delivery loop: `push_cs_players` records what it queued
        // per viewer and slot, so a second refresh of the same roster queues
        // nothing (this is the traffic the "spike when watching scores" came
        // from), while a slot moving into the viewer's match flips back to its
        // base string.
        let own = "n\\Bob\\t\\1\\model\\kyle";
        let outsider = "n\\Alora\\t\\2\\model\\alora";
        let mut bases: [String; MAX_CLIENTS] = core::array::from_fn(|_| String::new());
        bases[0] = own.to_owned();
        bases[1] = outsider.to_owned();
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        table[1] = 1;

        // The record the production code keeps (`cs_players_sent[viewer]`).
        let mut sent: [String; MAX_CLIENTS] = core::array::from_fn(|_| String::new());
        let queued = |table: &[u8; MAX_CLIENTS],
                      bases: &[String; MAX_CLIENTS],
                      sent: &mut [String; MAX_CLIENTS]| {
            let mut n = 0;
            for (slot, value) in desired_cs_players(MAIN_MATCH, table, bases) {
                let slot = slot as usize;
                if sent[slot] != value {
                    sent[slot] = value;
                    n += 1;
                }
            }
            n
        };

        // First refresh queues both slots (the viewer holds neither override
        // nor base yet in this simulation).
        assert_eq!(queued(&table, &bases, &mut sent), 2);
        // Steady roster: nothing changes, nothing is queued.
        assert_eq!(queued(&table, &bases, &mut sent), 0);
        // The outsider joins the viewer's match: its demotion is replaced by
        // the base string, and only that slot is queued.
        table[1] = MAIN_MATCH;
        assert_eq!(queued(&table, &bases, &mut sent), 1);
        assert_eq!(sent[1], outsider);
        // And it stays quiet again.
        assert_eq!(queued(&table, &bases, &mut sent), 0);
    }

    /// D-006.16: a base push deferred under reliable pressure must be replayed
    /// by the next full re-push. Treating the stored bases as held outside a
    /// gamestate recorded a value the client never received, so the
    /// `(TFFA N)` demotion of a same-match player survived every later refresh
    /// (wrong skin/saber colour, grouped as a spectator and therefore missing
    /// from the viewer's team rows).
    #[test]
    fn a_deferred_base_push_is_replayed_by_the_next_plain_resync() {
        let base = "n\\Alora\\t\\1\\model\\alora";
        let demoted = rewrite_cs_players_for_viewer(base, 1, MAIN_MATCH).expect("playing outsider");
        let mut bases: [String; MAX_CLIENTS] = core::array::from_fn(|_| String::new());
        bases[1] = base.to_owned();
        // The viewer is in the same match as slot 1 now, so the desired value
        // is the base string.
        let table = [MAIN_MATCH; MAX_CLIENTS];

        // What the viewer holds: the demotion, because the base push was
        // deferred (the record is left as the demotion too, `push_cs_players`
        // only records what it sent).
        let mut sent: [String; MAX_CLIENTS] = core::array::from_fn(|_| String::new());
        sent[1] = demoted;

        // A plain resync compares every desired value against the record, so
        // the owed base is queued.
        let plan = override_push_plan(false, MAIN_MATCH, &table, &bases);
        let owed: Vec<&(i32, String)> = plan
            .iter()
            .filter(|(slot, value)| sent[*slot as usize] != *value)
            .collect();
        assert_eq!(owed, vec![&(1, base.to_owned())]);

        // A gamestate delivered the base strings, so its resync owes only the
        // demotions of the viewers' outsiders and records the bases as held.
        let plan = override_push_plan(true, MAIN_MATCH, &table, &bases);
        assert!(
            plan.iter().all(|(slot, _)| *slot != 1),
            "a same-match base is already held: {plan:?}"
        );
    }

    #[test]
    fn info_value_and_set_round_trip() {
        let base = "n\\Bob\\t\\1\\model\\kyle";
        assert_eq!(info_value(base, "n").as_deref(), Some("Bob"));
        assert_eq!(info_value(base, "t").as_deref(), Some("1"));
        assert_eq!(info_value(base, "missing"), None);
        let out = info_set(base, "n", "(TFFA 1) Bob");
        assert_eq!(info_value(&out, "n").as_deref(), Some("(TFFA 1) Bob"));
        assert_eq!(info_value(&out, "t").as_deref(), Some("1"));
        // Appends missing keys, keeps leading-backslash style.
        let out2 = info_set("\\n\\Bob", "t", "3");
        assert_eq!(info_value(&out2, "t").as_deref(), Some("3"));
    }

    #[test]
    fn kill_limit_prints_match_ed_ref_shapes_only() {
        assert!(is_kill_limit_print("print \"Red @@@HIT_THE_KILL_LIMIT\n\""));
        assert!(is_kill_limit_print(
            "print \"Blue @@@HIT_THE_KILL_LIMIT\n\""
        ));
        assert!(is_kill_limit_print(
            "print \"^6^^0TuA^7 @@@HIT_THE_KILL_LIMIT.\n\""
        ));
        assert!(!is_kill_limit_print("print \"Red hit the kill limit\n\""));
        assert!(!is_kill_limit_print("chat \"HIT_THE_KILL_LIMIT\""));
        assert!(!is_kill_limit_print("scores 2 1 0"));
        assert!(!is_kill_limit_print("print \"hello\"\n"));
    }

    #[test]
    fn prefixed_name_fits_max_netname() {
        let long = "A".repeat(100);
        let out = prefixed_name(&long, 1);
        assert!(out.len() < MAX_NETNAME);
        assert!(out.starts_with("(TFFA 1) "));
        assert_eq!(prefixed_name("Bob", 0), "(TFFA 0) Bob");
    }

    #[test]
    fn rewrite_cs_players_prefixes_only_playing_outsiders() {
        // Own match: untouched.
        assert_eq!(rewrite_cs_players_for_viewer("n\\Bob\\t\\1", 1, 1), None);
        // Real spectator: untouched even across matches.
        assert_eq!(rewrite_cs_players_for_viewer("n\\Spec\\t\\3", 0, 1), None);
        // Playing outsider: prefixed + forced to spectator.
        let out = rewrite_cs_players_for_viewer("n\\Bob\\t\\1", 1, 0).expect("outsider rewritten");
        assert_eq!(info_value(&out, "n").as_deref(), Some("(TFFA 1) Bob"));
        assert_eq!(info_value(&out, "t").as_deref(), Some("3"));
        // Empty base: no change.
        assert_eq!(rewrite_cs_players_for_viewer("", 1, 0), None);
    }

    /// Only a `CS_PLAYERS` configstring index names a player slot; scores and
    /// every other configstring must not queue an animation restart.
    #[test]
    fn cs_players_slot_matches_only_player_configstrings() {
        assert_eq!(cs_players_slot(CS_PLAYERS), Some(0));
        assert_eq!(cs_players_slot(CS_PLAYERS + 7), Some(7));
        assert_eq!(
            cs_players_slot(CS_PLAYERS + MAX_CLIENTS as i32 - 1),
            Some(MAX_CLIENTS - 1)
        );
        assert_eq!(cs_players_slot(CS_PLAYERS - 1), None);
        assert_eq!(cs_players_slot(CS_PLAYERS + MAX_CLIENTS as i32), None);
        assert_eq!(cs_players_slot(CS_SCORES1), None);
        assert_eq!(cs_players_slot(CS_SCORES2), None);
    }

    #[test]
    fn entitynum_constants_match_sdk() {
        assert_eq!(ENTITYNUM_NONE, 1023);
        assert_eq!(ENTITYNUM_WORLD, 1022);
    }

    /// `G_Spawn` records a slot only while a world entity is running game code,
    /// and only for the ordinary range it hands out: nothing else can carry an
    /// instance.
    #[test]
    fn spawn_record_takes_ordinary_slots_with_a_context_only() {
        let w = World::new();
        let base = w.base();
        let ent = |slot: usize| base + slot * SIZEOF_GENTITY;
        assert_eq!(spawn_record(SCOPE_NONE, base, ent(48)), None);
        assert_eq!(spawn_record(1, 0, ent(48)), None);
        assert_eq!(spawn_record(1, base, 0), None);
        assert_eq!(spawn_record(1, base, ent(48)), Some((48, 1)));
        assert_eq!(spawn_record(MAIN_MATCH, base, ent(300)), Some((300, 0)));
        // The player range and the world/NONE slots are never handed out by
        // `G_Spawn` and are never tagged.
        for slot in [0usize, MAX_CLIENTS - 1] {
            assert_eq!(spawn_record(1, base, ent(slot)), None, "slot {slot}");
        }
        for slot in [ENTITYNUM_WORLD as usize, ENTITYNUM_NONE as usize] {
            assert_eq!(spawn_record(1, base, ent(slot)), None, "slot {slot}");
        }
        assert_eq!(spawn_record(1, base, base - SIZEOF_GENTITY), None);
        assert_eq!(spawn_record(1, base, base + 1), None);
    }

    /// When the acting call returns, only the spawned entities that are *live
    /// interactive, unowned world entities* join the instance recorded for them:
    /// an inert prop, a temp event entity, a slot the game already freed or
    /// recycled, and an entity the owned-entity machinery already scopes (a
    /// missile or physics object with a live-player owner) keep the tagless,
    /// shared life they had before this feature.
    #[test]
    fn tag_pending_joins_only_live_interactive_world_entities() {
        let mut w = World::new();
        let mut tags = [ENTITY_SHARED; MAX_GENTITIES];
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        table[3] = 2;
        // An interactive trigger and a door spawned by main's own code.
        w.spawn(48, crate::sdk::ET_GENERAL);
        w.put_i32(48, OFFSET_GENTITY_R_CONTENTS, CONTENTS_TRIGGER);
        w.spawn(49, ET_MOVER);
        // An inert prop, a temp event entity (`ET_EVENTS + EV_...`), a freed
        // slot, a slot recycled into an inert prop, and an interactive entity
        // owned by a live player (a missile: `r.ownerNum` -> client 3).
        w.spawn(50, crate::sdk::ET_GENERAL);
        w.spawn(51, ET_EVENTS + 5);
        w.spawn(52, ET_MOVER);
        w.put_i32(52, OFFSET_GENTITY_INUSE, 0);
        w.spawn(53, crate::sdk::ET_GENERAL);
        w.spawn(54, crate::sdk::ET_GENERAL);
        w.put_i32(54, OFFSET_GENTITY_R_CONTENTS, CONTENTS_TRIGGER);
        w.put_i32(54, OFFSET_GENTITY_R_OWNER, 3);
        w.spawn(3, 1);
        w.put_ptr(3, OFFSET_GENTITY_CLIENT, 0x0800_0000);
        let pending = [
            (48u16, MAIN_MATCH),
            (49, 2),
            (50, MAIN_MATCH),
            (51, MAIN_MATCH),
            (52, MAIN_MATCH),
            (53, 2),
            (54, 2),
        ];
        assert_eq!(tag_pending(&mut tags, &table, w.base(), &pending), 2);
        assert_eq!(tags[48], MAIN_MATCH, "main's runtime trigger");
        assert_eq!(tags[49], 2, "instance 2's runtime door");
        assert_eq!(tags[50], ENTITY_SHARED, "inert prop stays shared");
        assert_eq!(tags[51], ENTITY_SHARED, "temp event entity stays shared");
        assert_eq!(tags[52], ENTITY_SHARED, "freed slot");
        assert_eq!(tags[53], ENTITY_SHARED, "recycled inert slot");
        assert_eq!(
            tags[54], ENTITY_SHARED,
            "owned entity keeps its tagless life"
        );
        // A missing `g_entities` tags nothing and does not read memory.
        assert_eq!(tag_pending(&mut tags, &table, 0, &pending), 0);
    }

    #[test]
    fn travel_forces_spectator_only_for_playing_clients() {
        assert_eq!(travel_move(Some(TEAM_SPECTATOR)), TravelMove::None);
        assert_eq!(travel_move(Some(TEAM_RED)), TravelMove::ToSpectator);
        assert_eq!(travel_move(Some(TEAM_BLUE)), TravelMove::ToSpectator);
        // Duel/free is playing, not spectating (travel must still spectate).
        assert_eq!(
            travel_move(Some(crate::sdk::TEAM_FREE)),
            TravelMove::ToSpectator
        );
        assert_eq!(travel_move(None), TravelMove::Refuse);
    }

    #[test]
    fn instance_team_count_scopes_the_census_to_one_instance() {
        // Main: 2 red + 1 blue. Instance 1: 1 red + 2 blue. A disconnected slot
        // (and one on neither team) rounds the roster out.
        let mut rows = [RosterRow::default(); MAX_CLIENTS];
        let live = |team: i32, instance: u8| RosterRow {
            connected: true,
            team,
            instance,
        };
        rows[0] = live(TEAM_RED, MAIN_MATCH);
        rows[1] = live(TEAM_RED, MAIN_MATCH);
        rows[2] = live(TEAM_BLUE, MAIN_MATCH);
        rows[3] = live(TEAM_RED, 1);
        rows[4] = live(TEAM_BLUE, 1);
        rows[5] = live(TEAM_BLUE, 1);
        rows[6] = live(crate::sdk::TEAM_FREE, MAIN_MATCH);
        // A client joining main (slot 7) is censused against main alone.
        assert_eq!(count_instance_team(7, MAIN_MATCH, &rows, TEAM_RED), 2);
        assert_eq!(count_instance_team(7, MAIN_MATCH, &rows, TEAM_BLUE), 1);
        // A client joining instance 1 is censused against instance 1 alone.
        assert_eq!(count_instance_team(7, 1, &rows, TEAM_RED), 1);
        assert_eq!(count_instance_team(7, 1, &rows, TEAM_BLUE), 2);
        // The joining client itself is skipped, like the pristine count.
        assert_eq!(count_instance_team(0, MAIN_MATCH, &rows, TEAM_RED), 1);
        // A disconnected slot is not censused.
        rows[3].connected = false;
        assert_eq!(count_instance_team(7, 1, &rows, TEAM_RED), 0);
    }

    #[test]
    fn match_team_counts_groups_playing_clients_by_match() {
        let mut rows = [RosterRow::default(); MAX_CLIENTS];
        let live = |team: i32, instance: u8| RosterRow {
            connected: true,
            team,
            instance,
        };
        rows[0] = live(TEAM_RED, MAIN_MATCH);
        rows[1] = live(TEAM_BLUE, MAIN_MATCH);
        rows[2] = live(TEAM_RED, 1);
        rows[3] = live(TEAM_RED, 1);
        rows[4] = live(TEAM_BLUE, 1);
        // Spectators and connecting/disconnected slots are not a team size.
        rows[5] = live(crate::sdk::TEAM_SPECTATOR, MAIN_MATCH);
        rows[6] = live(TEAM_RED, MAIN_MATCH);
        rows[6].connected = false;
        // A stale assignment to an inactive slot does not invent a match.
        rows[7] = live(TEAM_RED, 2);
        let mut active = [false; TOTAL_MATCH_SLOTS];
        active[MAIN_MATCH as usize] = true;
        active[1] = true;
        let counts = count_match_teams(&rows, &active);
        assert_eq!(counts[MAIN_MATCH as usize], (1, 1));
        assert_eq!(counts[1], (2, 1));
        assert_eq!(counts[2], (0, 0));
        // Empty slots stay empty rather than inheriting a neighbour.
        assert_eq!(counts[3], (0, 0));
    }

    #[test]
    fn join_target_accepts_created_slots_up_to_the_limit_only() {
        // Main (0) is routed to `cmd_leave` before this resolution, so it is
        // not a created slot.
        assert_eq!(join_target(0, 8), None);
        assert_eq!(join_target(-1, 8), None);
        assert_eq!(join_target(1, 8), Some(1));
        assert_eq!(join_target(8, 8), Some(8));
        // The admin's `proxy_tffa_maxMatches` caps the accepted ids.
        assert_eq!(join_target(5, 4), None);
        assert_eq!(join_target(9, 8), None);
        // A wrapped negative must not select the top slot.
        assert_eq!(join_target(i32::MIN, 8), None);
    }

    /// A protected match is marked in `tffa_list`, including a protected game
    /// promoted into the main slot (its password lives with its identity, so
    /// the main line has to resolve `main_origin`).
    #[test]
    fn tffa_list_marks_password_protected_matches() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[1].active = true;
        assert!(matches[1].set_password(b"hunter2"));
        let table = [MAIN_MATCH; MAX_CLIENTS];
        let connected = [false; MAX_CLIENTS];

        let out = render_match_list(8, MAIN_MATCH, &matches, &table, &connected);
        assert!(
            out.contains("\n1: 0 players, 0-0^7 ^3[password]^7"),
            "{out}"
        );
        assert!(!out.contains("0 (main): 0 players, 0-0^7 ^3[password]^7"));

        // After a promotion identity 1 plays in the main slot: the main line
        // carries its lock.
        let out = render_match_list(8, 1, &matches, &table, &connected);
        assert!(
            out.contains("0 (main): 0 players, 0-0^7 ^3[password]^7"),
            "{out}"
        );
    }

    /// Passwords are exact, case-sensitive and never empty: an open match has
    /// no password to give.
    #[test]
    fn passwords_are_exact_and_case_sensitive() {
        let mut m = state::TffaMatch::default();
        assert!(!m.has_password());
        assert!(!m.password_matches(b""));
        assert!(m.set_password(b"OpenSesame"));
        assert!(m.has_password());
        assert!(m.password_matches(b"OpenSesame"));
        assert!(!m.password_matches(b"opensesame"));
        assert!(!m.password_matches(b"OpenSesame "));
        assert!(!m.password_matches(b"Open"));
        // Too long for the storage: refused, the previous password is kept.
        assert!(!m.set_password(&[b'x'; MAX_TFFA_PASSWORD + 1]));
        assert!(m.password_matches(b"OpenSesame"));
        m.clear_password();
        assert!(!m.has_password());
        assert!(!m.password_matches(b"OpenSesame"));
    }

    /// Protection gates *playing*, not membership: a client in a protected
    /// match may spectate without the password, an authorisation for another id
    /// does not count, and a protected game promoted into the main slot is
    /// checked under its own identity.
    #[test]
    fn a_protected_match_refuses_play_until_its_password_is_given() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[1].active = true;
        assert!(matches[1].set_password(b"swordfish"));
        matches[2].active = true;

        let mut client_match = [MAIN_MATCH; MAX_CLIENTS];
        let mut pass_ok = [MAIN_MATCH; MAX_CLIENTS];
        client_match[2] = 1;

        // Spectator of the protected match: still a member (snapshots, scores,
        // chat) but not allowed to join a team.
        assert!(!password_cleared(
            &matches,
            &client_match,
            MAIN_MATCH,
            &pass_ok,
            2
        ));
        // Main stays open.
        assert!(password_cleared(
            &matches,
            &client_match,
            MAIN_MATCH,
            &pass_ok,
            3
        ));
        // The password unlocks the match; another id's authorisation does not.
        pass_ok[2] = 1;
        assert!(password_cleared(
            &matches,
            &client_match,
            MAIN_MATCH,
            &pass_ok,
            2
        ));
        pass_ok[2] = 2;
        assert!(!password_cleared(
            &matches,
            &client_match,
            MAIN_MATCH,
            &pass_ok,
            2
        ));
        // An open created match needs nothing.
        client_match[2] = 2;
        pass_ok[2] = MAIN_MATCH;
        assert!(password_cleared(
            &matches,
            &client_match,
            MAIN_MATCH,
            &pass_ok,
            2
        ));
        // Identity 1 promoted into the main slot: a fresh main client cannot
        // join its teams for free.
        assert!(!password_cleared(&matches, &client_match, 1, &pass_ok, 4));
        pass_ok[4] = 1;
        assert!(password_cleared(&matches, &client_match, 1, &pass_ok, 4));
        // `slot_identity` maps main to the promoted game and a created slot to
        // itself.
        assert_eq!(slot_identity(MAIN_MATCH, MAIN_MATCH), MAIN_MATCH);
        assert_eq!(slot_identity(1, MAIN_MATCH), 1);
        assert_eq!(slot_identity(1, 3), 3);
    }

    /// The verbose client-command log must never contain a match password.
    #[test]
    fn client_command_logging_redacts_password_arguments() {
        assert_eq!(
            redact_client_command(b"tffa_create hunter2"),
            &b"tffa_create ***"[..]
        );
        assert_eq!(
            redact_client_command(b"tffa_join 3 hunter2"),
            &b"tffa_join 3 ***"[..]
        );
        // Tokenisation matches the engine's `Cmd_TokenizeString`: the original
        // separators survive in the log line, and a quoted password (which may
        // contain spaces) is redacted as a whole.
        assert_eq!(
            redact_client_command(b"tffa_join\t3\thunter2"),
            &b"tffa_join\t3\t***"[..]
        );
        assert_eq!(
            redact_client_command(b"tffa_create \"my pass\""),
            &b"tffa_create ***"[..]
        );
        assert_eq!(
            redact_client_command(b"tffa_join 3 \"my pass\" // note"),
            &b"tffa_join 3 *** // note"[..]
        );
        // A comment can even swallow the argument, and then nothing is secret.
        assert_eq!(
            redact_client_command(b"tffa_create // hunter2"),
            &b"tffa_create // hunter2"[..]
        );
        // No password argument: unchanged, and no marker is invented.
        assert_eq!(redact_client_command(b"tffa_create"), &b"tffa_create"[..]);
        assert_eq!(redact_client_command(b"tffa_join 3"), &b"tffa_join 3"[..]);
        assert_eq!(redact_client_command(b"tffa_list"), &b"tffa_list"[..]);
        // Other commands are left alone, byte for byte (no UTF-8 assumption).
        assert_eq!(
            redact_client_command(b"say_team hunter2"),
            &b"say_team hunter2"[..]
        );
        assert_eq!(redact_client_command(&[0xff, 0xfe]), &[0xff, 0xfe]);
    }

    /// A `map_restart` keeps a protected match's password; a slot dropped by a
    /// lowered limit (or a full map change) loses it, so its next match starts
    /// open unless it sets its own.
    #[test]
    fn a_map_restart_keeps_a_matches_password_and_a_dropped_match_loses_it() {
        let mut t = state::TffaState::default();
        t.matches[1].active = true;
        assert!(t.matches[1].set_password(b"pw-one"));
        t.matches[3].active = true;
        assert!(t.matches[3].set_password(b"pw-three"));
        t.client_match[5] = 1;
        t.home[5] = 1;
        t.pass_ok[5] = 1;

        reset_round_state(&mut t, true, 2);
        assert!(t.matches[1].active);
        assert!(t.matches[1].password_matches(b"pw-one"));
        assert_eq!(t.pass_ok[5], 1);
        // Slot 3 is above the live limit: the id is gone with its password.
        assert!(!t.matches[3].active);
        assert!(!t.matches[3].has_password());

        let mut t = state::TffaState::default();
        t.matches[1].active = true;
        assert!(t.matches[1].set_password(b"pw-one"));
        t.pass_ok[5] = 1;
        reset_round_state(&mut t, false, 8);
        assert!(!t.matches[1].has_password());
        assert_eq!(t.pass_ok[5], MAIN_MATCH);
    }

    /// Reported bug: on a 32-slot server with 3 connected players, `tffa_list`
    /// showed 32 in main because every free slot defaults to `MAIN_MATCH`.
    /// Player counts are censused over connected clients only.
    #[test]
    fn tffa_list_counts_only_connected_players() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[MAIN_MATCH as usize].red = 5;
        matches[MAIN_MATCH as usize].blue = 3;
        matches[1].active = true;
        matches[1].red = 2;
        matches[1].blue = 1;

        // 29 free slots keep the default `MAIN_MATCH`; only 3 are connected.
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        let mut connected = [false; MAX_CLIENTS];
        for c in connected.iter_mut().take(3) {
            *c = true;
        }
        // Instance 1: two connected members, plus a disconnected stale slot.
        table[3] = 1;
        table[4] = 1;
        table[5] = 1;
        connected[3] = true;
        connected[4] = true;

        let out = render_match_list(8, MAIN_MATCH, &matches, &table, &connected);
        assert!(out.contains("0 (main): 3 players, 5-3"), "{out}");
        assert!(
            !out.contains("0 (main): 32 players"),
            "free slots must not count as main players: {out}"
        );
        assert!(out.contains("\n1: 2 players, 2-1"), "{out}");
        assert_eq!(counted_match_size(MAIN_MATCH, &table, &connected), 3);
        assert_eq!(counted_match_size(1, &table, &connected), 2);

        // Exact reported shape: 32 slots, 3 connected, nothing else.
        let table = [MAIN_MATCH; MAX_CLIENTS];
        let mut connected = [false; MAX_CLIENTS];
        for c in connected.iter_mut().take(3) {
            *c = true;
        }
        let out = render_match_list(8, MAIN_MATCH, &matches, &table, &connected);
        assert!(out.contains("0 (main): 3 players"), "{out}");
    }

    #[test]
    fn creator_joins_the_smaller_side_red_on_a_tie() {
        // A freshly created slot is 0-0, so its creator starts red.
        assert_eq!(create_team(0, 0), TEAM_RED);
        assert_eq!(create_team(2, 2), TEAM_RED);
        // Later creations keep the sides balanced (the smaller side wins).
        assert_eq!(create_team(0, 1), TEAM_RED);
        assert_eq!(create_team(1, 0), TEAM_BLUE);
        assert_eq!(team_name(TEAM_RED).to_bytes(), b"red");
        assert_eq!(team_name(TEAM_BLUE).to_bytes(), b"blue");
    }

    /// A created match with nobody in game (only spectators, or no members)
    /// is reported for closure; main is implicit and never closes.
    #[test]
    fn empty_created_matches_close_but_main_stays() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[1].active = true;
        matches[1].red = 3;
        matches[1].blue = 2;
        matches[2].active = true; // spectator only
        matches[3].active = true; // no member at all
        matches[4].active = false; // already closed

        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        let mut playing = [false; MAX_CLIENTS];
        // Main has one player; match 1 has one player; match 2 has only a
        // spectator assigned.
        playing[0] = true;
        table[1] = 1;
        playing[1] = true;
        table[2] = 2;

        assert_eq!(
            empty_created_matches(&matches, &table, &playing),
            vec![2, 3]
        );

        // Main keeps running even when everyone there is a spectator.
        playing[0] = false;
        assert_eq!(
            empty_created_matches(&matches, &table, &playing),
            vec![2, 3]
        );
    }

    /// Requested: joining another TFFA instance starts the mover's
    /// `proxy_sv_enableEndGameStats` sheet from zero — no kills, deaths or
    /// damage carried over, in either direction — and the scoreboard's
    /// per-match score copy starts at zero. `cmd_join` performs this through
    /// `assign_client_instance` (`reset_client_stats` + assignment).
    #[test]
    fn joining_another_instance_resets_end_game_stats() {
        use crate::state::{ClientEntry, GameStats};
        let mut client_match = [MAIN_MATCH; MAX_CLIENTS];
        let mut home = [MAIN_MATCH; MAX_CLIENTS];
        let mut player_score = [0i32; MAX_CLIENTS];
        let mut clients = [ClientEntry::default(); MAX_CLIENTS];
        let me = 3usize;
        let opponent = 0usize;
        let other = 5usize;

        // I am leaving match 1 for match 2 and already have a per-match score.
        client_match[me] = 1;
        home[me] = 1;
        player_score[me] = 12;

        // My row: what I did to opponents. My column: what they did to me.
        clients[me].game_stats[opponent] = GameStats {
            killed: 4,
            killed_by: 2,
            damages_given: 210,
            damages_taken: 90,
        };
        clients[opponent].game_stats[me] = GameStats {
            killed: 2,
            killed_by: 4,
            damages_given: 90,
            damages_taken: 210,
        };
        clients[me].game_stats[other] = GameStats {
            killed: 1,
            killed_by: 0,
            damages_given: 50,
            damages_taken: 0,
        };
        clients[other].game_stats[me] = GameStats {
            killed: 0,
            killed_by: 1,
            damages_given: 0,
            damages_taken: 50,
        };
        // Stats between two other players, and another player's own team
        // counters, must survive my move.
        clients[opponent].game_stats[other] = GameStats {
            killed: 7,
            killed_by: 6,
            damages_given: 42,
            damages_taken: 24,
        };
        clients[opponent].team_kills = 5;
        // My team counters are match-local too.
        clients[me].team_kills = 3;
        clients[me].team_killed = 2;
        clients[me].team_damages_given = 300;
        clients[me].team_damages_taken = 120;

        assign_client_instance(
            &mut client_match,
            &mut home,
            &mut player_score,
            &mut clients,
            me as c_int,
            2,
        );

        // The move itself: new instance and identity, per-match score copy reset.
        assert_eq!(client_match[me], 2);
        assert_eq!(home[me], 2);
        assert_eq!(player_score[me], 0);
        // Neither my row nor my column keeps a kill, death or damage point.
        for slot in 0..MAX_CLIENTS {
            let row = clients[me].game_stats[slot];
            assert_eq!(
                (
                    row.killed,
                    row.killed_by,
                    row.damages_given,
                    row.damages_taken
                ),
                (0, 0, 0, 0),
                "my row vs slot {slot} survived the move"
            );
            let column = clients[slot].game_stats[me];
            assert_eq!(
                (
                    column.killed,
                    column.killed_by,
                    column.damages_given,
                    column.damages_taken
                ),
                (0, 0, 0, 0),
                "slot {slot}'s record of me survived the move"
            );
        }
        assert_eq!(
            (
                clients[me].team_kills,
                clients[me].team_killed,
                clients[me].team_damages_given,
                clients[me].team_damages_taken
            ),
            (0, 0, 0, 0)
        );

        // Other players' mutual stats and their own team counters are intact.
        let kept = clients[opponent].game_stats[other];
        assert_eq!(
            (
                kept.killed,
                kept.killed_by,
                kept.damages_given,
                kept.damages_taken
            ),
            (7, 6, 42, 24)
        );
        assert_eq!(clients[opponent].team_kills, 5);
    }

    /// The game-side half of the same move: `PERS_SCORE`/`PERS_KILLED` (the
    /// final K/D columns) are zeroed, and nothing around them is touched.
    #[test]
    fn joining_another_instance_resets_game_score_and_deaths() {
        use crate::sdk::{OFFSET_GCLIENT_PS, OFFSET_PS_PERSISTANT, PERS_KILLED, PERS_SCORE};
        let score_at = OFFSET_GCLIENT_PS + OFFSET_PS_PERSISTANT + PERS_SCORE * 4;
        let killed_at = OFFSET_GCLIENT_PS + OFFSET_PS_PERSISTANT + PERS_KILLED * 4;
        let end = score_at.max(killed_at) + 4;
        let mut client = vec![0x5au8; end];
        let base = client.as_mut_ptr() as usize;
        // SAFETY: the scratch buffer covers both persistant slots.
        unsafe {
            write_i32(base + score_at, 9);
            write_i32(base + killed_at, 4);
            reset_game_scores_at(base);
        }

        // SAFETY: the same scratch buffer, still alive and not aliased.
        unsafe {
            assert_eq!(read_i32(base + score_at), 0);
            assert_eq!(read_i32(base + killed_at), 0);
        }
        for (i, byte) in client.iter().enumerate() {
            if (score_at..score_at + 4).contains(&i) || (killed_at..killed_at + 4).contains(&i) {
                continue;
            }
            assert_eq!(*byte, 0x5a, "reset touched byte {i}");
        }
    }

    /// Only a non-zero → zero flip of `proxy_tffa_enable` shuts the matches
    /// down: turning it on, leaving it on/off, and the frames after a disable
    /// all stay quiet (a repeated shutdown would re-send every message and
    /// reset the overlay every frame).
    #[test]
    fn disable_edge_only_fires_on_a_nonzero_to_zero_flip() {
        let mut latched = false;
        // Off at start and still off: nothing to close.
        assert!(!enable_turned_off(&mut latched, false));
        // Turning the feature on is not a disable...
        assert!(!enable_turned_off(&mut latched, true));
        assert!(!enable_turned_off(&mut latched, true));
        // ...turning it off is exactly one...
        assert!(enable_turned_off(&mut latched, false));
        // ...and the next frame must not repeat it.
        assert!(!enable_turned_off(&mut latched, false));
        // A later re-enable/disable shuts down again.
        assert!(!enable_turned_off(&mut latched, true));
        assert!(enable_turned_off(&mut latched, false));
    }

    /// A disable closes every created match (active slots only) and leaves the
    /// main game standing with its own tallies, while the closed instances'
    /// members come back to main with clean per-match stats.
    #[test]
    fn a_disable_closes_every_created_match_but_main() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize] = state::TffaMatch {
            active: true,
            red: 5,
            blue: 3,
            ..Default::default()
        };
        matches[1] = state::TffaMatch {
            active: true,
            red: 2,
            blue: 1,
            ..Default::default()
        };
        matches[4] = state::TffaMatch {
            active: true,
            red: 0,
            blue: 0,
            ..Default::default()
        };
        // A slot above a lowered `proxy_tffa_maxMatches` is still an open
        // match: a disable closes it like any other (only `map_restart`'s carry
        // applies the limit).
        matches[TOTAL_MATCH_SLOTS - 1].active = true;
        assert!(matches[1].set_password(b"hunter2"));
        // An inactive slot stays out of the sweep (and main is never one).
        assert_eq!(
            created_matches_to_close(&matches),
            vec![1, 4, TOTAL_MATCH_SLOTS as u8 - 1]
        );

        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        table[0] = 1;
        table[7] = 4;
        table[9] = TOTAL_MATCH_SLOTS as u8 - 1;
        let mut score = [0i32; MAX_CLIENTS];
        score[0] = 9;
        score[7] = 2;
        score[9] = 4;
        let mut home = [MAIN_MATCH; MAX_CLIENTS];
        home[0] = 1;
        home[7] = 4;
        home[9] = TOTAL_MATCH_SLOTS as u8 - 1;
        let mut pass_ok = [MAIN_MATCH; MAX_CLIENTS];
        pass_ok[0] = 1;
        pass_ok[7] = 4;
        for id in created_matches_to_close(&matches) {
            close_instance_state(
                &mut matches,
                &mut table,
                &mut home,
                &mut pass_ok,
                &mut score,
                id,
            );
        }
        // Closing an id drops everyone's authorisation to it, and its
        // protection dies with it.
        assert_eq!(pass_ok[0], MAIN_MATCH);
        assert_eq!(pass_ok[7], MAIN_MATCH);
        assert!(!matches[1].has_password());
        assert!(matches[MAIN_MATCH as usize].active);
        assert_eq!(
            (
                matches[MAIN_MATCH as usize].red,
                matches[MAIN_MATCH as usize].blue
            ),
            (5, 3)
        );
        assert!(matches.iter().skip(1).all(|m| !m.active));
        assert!(matches.iter().skip(1).all(|m| m.red == 0 && m.blue == 0));
        assert_eq!(table[0], MAIN_MATCH);
        assert_eq!(table[7], MAIN_MATCH);
        assert_eq!(table[9], MAIN_MATCH);
        assert_eq!(score[0], 0);
        assert_eq!(score[7], 0);
        assert_eq!(score[9], 0);
        // Nothing left for a second pass to close.
        assert!(created_matches_to_close(&matches).is_empty());
    }

    /// The message a member of a closed match receives explains what happened:
    /// a player is told they are spectating now and how to play again, a
    /// spectator is only told the match is gone.
    #[test]
    fn disable_notice_explains_the_spectator_move() {
        let moved = disable_notice(3, true);
        assert!(moved.starts_with("print \""));
        assert!(moved.contains("match 3"));
        assert!(moved.contains("moved to spectator"));
        assert!(moved.contains("/team red"));
        assert!(moved.ends_with("\n\""));
        let watching = disable_notice(3, false);
        assert!(watching.contains("match 3"));
        assert!(!watching.contains("moved to spectator"));
        assert!(watching.contains("main game"));
        // No quote or newline can break the `print "..."` wire format.
        for text in [moved, watching] {
            assert_eq!(text.matches('"').count(), 2);
            assert_eq!(text.matches('\n').count(), 1);
        }
    }

    /// The disable hands main's tally back to `level.teamScores` instead of
    /// leaving the neutralised `0-0`: the write lands exactly on the RED/BLUE
    /// slots the neutraliser subtracts from and touches nothing around them, so
    /// the pristine `CheckExitRules`/scoreboard/`CalculateRanks` continue from
    /// the running score.
    #[test]
    fn disable_restores_main_tally_into_the_global_scores() {
        // A `level_locals_t` scratch buffer around the three `teamScores`
        // slots (`+44` TEAM_FREE, `+48` RED, `+52` BLUE).
        let mut level = [0u8; 64];
        fn read(level: *const u8, off: usize) -> c_int {
            // SAFETY: the test buffer covers every offset it reads.
            unsafe { core::ptr::read_unaligned(level.add(off) as *const c_int) }
        }
        unsafe {
            core::ptr::write_unaligned(
                level.as_mut_ptr().add(crate::sdk::OFFSET_LEVEL_TEAM_SCORES) as *mut c_int,
                7,
            );
            core::ptr::write_unaligned(level.as_mut_ptr().add(56) as *mut c_int, 9);
            // What the overlay left: neutralised to 0-0 (D-006.3).
            write_global_team_scores(level.as_mut_ptr() as usize, 0, 0);
        }
        assert_eq!(
            read(level.as_ptr(), crate::sdk::OFFSET_LEVEL_TEAM_SCORES_RED),
            0
        );
        assert_eq!(
            read(level.as_ptr(), crate::sdk::OFFSET_LEVEL_TEAM_SCORES_BLUE),
            0
        );

        // Main was 5-3 when the admin turned the feature off.
        unsafe { write_global_team_scores(level.as_mut_ptr() as usize, 5, 3) };
        assert_eq!(
            read(level.as_ptr(), crate::sdk::OFFSET_LEVEL_TEAM_SCORES_RED),
            5
        );
        assert_eq!(
            read(level.as_ptr(), crate::sdk::OFFSET_LEVEL_TEAM_SCORES_BLUE),
            3
        );
        // Neighbouring slots are untouched (a wrong offset would clobber them).
        assert_eq!(
            read(level.as_ptr(), crate::sdk::OFFSET_LEVEL_TEAM_SCORES),
            7
        );
        assert_eq!(read(level.as_ptr(), 56), 9);
    }

    /// Closing a created instance deactivates it, resets its tallies and
    /// returns its remaining (spectating) members to main with clean stats.
    #[test]
    fn closing_an_instance_returns_its_spectators_to_main() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize] = state::TffaMatch {
            active: true,
            red: 4,
            blue: 2,
            ..Default::default()
        };
        matches[1] = state::TffaMatch {
            active: true,
            red: 3,
            blue: 1,
            ..Default::default()
        };
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        table[0] = 1;
        table[1] = 1;
        table[2] = MAIN_MATCH;
        let mut home = [MAIN_MATCH; MAX_CLIENTS];
        home[0] = 1;
        home[1] = 1;
        let mut score = [0i32; MAX_CLIENTS];
        score[0] = 7;
        score[1] = 2;
        score[2] = 5;

        let mut pass_ok = [MAIN_MATCH; MAX_CLIENTS];
        pass_ok[0] = 1;
        pass_ok[1] = 1;
        let moved = close_instance_state(
            &mut matches,
            &mut table,
            &mut home,
            &mut pass_ok,
            &mut score,
            1,
        );
        assert_eq!(moved, vec![0, 1]);
        assert!(!matches[1].active);
        assert_eq!((matches[1].red, matches[1].blue), (0, 0));
        // Main and another main member are untouched.
        assert!(matches[MAIN_MATCH as usize].active);
        assert_eq!(
            (
                matches[MAIN_MATCH as usize].red,
                matches[MAIN_MATCH as usize].blue
            ),
            (4, 2)
        );
        assert_eq!(table[0], MAIN_MATCH);
        assert_eq!(table[1], MAIN_MATCH);
        assert_eq!(table[2], MAIN_MATCH);
        // The id is gone, so its clients' identities return to main too.
        assert_eq!(home[0], MAIN_MATCH);
        assert_eq!(home[1], MAIN_MATCH);
        assert_eq!(score[0], 0);
        assert_eq!(score[1], 0);
        assert_eq!(score[2], 5);

        // Main is never closed through this path.
        assert!(
            close_instance_state(
                &mut matches,
                &mut table,
                &mut home,
                &mut pass_ok,
                &mut score,
                MAIN_MATCH
            )
            .is_empty()
        );
        assert!(matches[MAIN_MATCH as usize].active);
    }

    /// D-006.12: the proxy stays silent through `SV_MapRestart_f`'s settle
    /// frames, the reconnect loop and its trailing frame, and only re-arms on
    /// the frame after that (where the clients' acknowledgements have been
    /// read). The bounded fallback ends the burst if the trailing frame never
    /// arrives.
    #[test]
    fn the_restart_burst_defers_proxy_sends_until_after_the_trailing_frame() {
        let mut t = state::TffaState {
            restart_burst: true,
            ..state::TffaState::default()
        };

        // Three settle frames run before the reconnect loop: still bursting.
        for _ in 0..3 {
            advance_restart_burst_state(&mut t);
            assert!(t.restart_burst);
            assert!(!t.restart_burst_trailing);
        }
        // The reconnect loop begins clients: the next frame is the trailing one.
        t.restart_burst_connects = true;
        advance_restart_burst_state(&mut t);
        assert!(
            t.restart_burst,
            "the trailing frame is still inside the burst"
        );
        assert!(t.restart_burst_trailing);
        // The first frame after it ends the burst.
        advance_restart_burst_state(&mut t);
        assert!(!t.restart_burst);
        assert!(!t.restart_burst_connects);
        assert!(!t.restart_burst_trailing);
        assert_eq!(t.restart_burst_frames, 0);

        // A restart nobody reconnected for (empty server) ends after the
        // bounded wait, so a modified engine can never freeze the proxy.
        t.restart_burst = true;
        for _ in 0..RESTART_BURST_MAX_FRAMES {
            advance_restart_burst_state(&mut t);
        }
        assert!(!t.restart_burst);
    }

    /// A `map_restart` keeps the matches that were open and each client's
    /// instance, but starts the new round with clean tallies.
    #[test]
    fn map_restart_keeps_open_matches_and_assignments() {
        let mut t = state::TffaState::default();
        t.matches[1].active = true;
        t.matches[1].red = 4;
        t.matches[1].blue = 2;
        t.matches[3].active = true;
        t.client_match[2] = 1;
        t.client_match[5] = 3;
        t.home[2] = 1;
        t.home[5] = 3;
        t.player_score[2] = 6;
        t.cs_players_base[2] = r"\n\Bob\t\1".to_owned();

        let kept = reset_round_state(&mut t, true, MAX_MATCHES);
        assert!(kept);
        // The overlay survives...
        assert!(t.matches[1].active);
        assert!(t.matches[3].active);
        assert_eq!(t.client_match[2], 1);
        assert_eq!(t.client_match[5], 3);
        assert!(t.matches[MAIN_MATCH as usize].active);
        // ...with the round's tallies and per-player copies gone.
        assert_eq!((t.matches[1].red, t.matches[1].blue), (0, 0));
        assert_eq!(t.player_score[2], 0);
        // The last round's `CS_PLAYERS` mirrors are not current any more.
        assert!(t.cs_players_base[2].is_empty());
    }

    /// A full map change starts from main, and so does an instance above a
    /// lowered `proxy_tffa_maxMatches` even on a restart: its members cannot
    /// stay in a slot that no longer exists.
    #[test]
    fn reset_round_closes_matches_a_restart_does_not_keep() {
        let mut t = state::TffaState::default();
        t.matches[1].active = true;
        t.matches[1].red = 2;
        t.client_match[2] = 1;
        t.home[2] = 1;
        t.player_score[2] = 3;

        assert!(!reset_round_state(&mut t, false, MAX_MATCHES));
        assert!(!t.matches[1].active);
        assert_eq!((t.matches[1].red, t.matches[1].blue), (0, 0));
        assert_eq!(t.client_match[2], MAIN_MATCH);
        assert_eq!(t.home[2], MAIN_MATCH);
        assert_eq!(t.player_score[2], 0);

        let mut t = state::TffaState::default();
        t.matches[1].active = true;
        t.matches[5].active = true;
        t.client_match[2] = 5;
        t.home[2] = 5;

        assert!(reset_round_state(&mut t, true, 3));
        assert!(t.matches[1].active);
        assert!(!t.matches[5].active);
        assert_eq!(t.client_match[2], MAIN_MATCH);
        assert_eq!(t.home[2], MAIN_MATCH);
    }

    /// Only active created slots inside the live limit get their world rebuilt.
    #[test]
    fn restorable_instances_select_active_slots_inside_the_limit() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[1].active = true;
        matches[4].active = true;
        assert_eq!(restorable_instances(&matches, MAX_MATCHES), vec![1, 4]);
        // Above the live `proxy_tffa_maxMatches` (closed by the reset already).
        assert_eq!(restorable_instances(&matches, 2), vec![1]);
        assert!(restorable_instances(&matches, 0).is_empty());
    }

    /// D-006.10/D-006.11: a game that ended remembers who was playing, for
    /// which team, and only for the clients whose identity is that game's —
    /// a spectator is not remembered, and neither is a client another game owns
    /// (the old main's spectator in a promoted main game).
    #[test]
    fn a_finished_match_remembers_the_teams_its_players_had() {
        let members = vec![2, 5, 7, 9];
        let teams = vec![
            TEAM_RED as u8,
            REMEMBERED_NONE,
            TEAM_BLUE as u8,
            TEAM_RED as u8,
        ];
        let mut home = [MAIN_MATCH; MAX_CLIENTS];
        home[2] = 1;
        home[5] = 1;
        home[7] = 1;
        // Client 9 belongs to another id: it is a runtime member only.
        home[9] = 3;
        let roster = roster_from_members(&members, &teams, &home, 1);
        assert_eq!(roster[2], TEAM_RED as u8);
        assert_eq!(roster[7], TEAM_BLUE as u8);
        assert_eq!(roster[5], REMEMBERED_NONE);
        assert_eq!(roster[9], REMEMBERED_NONE);
        // Out-of-range members (never in `members` in practice) are ignored.
        assert_eq!(roster.iter().filter(|t| **t != REMEMBERED_NONE).count(), 2);
    }

    /// D-006.10: a `map_restart` re-creates a finished instance from its
    /// remembered roster, puts each member back in it and schedules the team
    /// auto-join; the roster is consumed by that restart.
    #[test]
    fn a_remembered_match_is_recreated_on_a_map_restart() {
        let mut t = state::TffaState::default();
        t.matches[1].remembered[3] = TEAM_RED as u8;
        t.matches[1].remembered[4] = TEAM_BLUE as u8;
        t.client_match[3] = MAIN_MATCH;
        t.client_match[4] = MAIN_MATCH;
        t.home[3] = 1;
        t.home[4] = 1;

        let kept = reset_round_state(&mut t, true, MAX_MATCHES);
        assert!(kept);
        assert!(t.matches[1].active);
        assert_eq!(t.client_match[3], 1);
        assert_eq!(t.client_match[4], 1);
        assert_eq!(t.pending_join[3], TEAM_RED as u8);
        assert_eq!(t.pending_join[4], TEAM_BLUE as u8);
        assert_eq!(t.home[3], 1);
        assert_eq!((t.matches[1].red, t.matches[1].blue), (0, 0));
        // Applied once: the instance is now a normal active match.
        assert!(!t.matches[1].has_remembered());
        let _ = reset_round_state(&mut t, true, MAX_MATCHES);
        assert_eq!(t.pending_join[3], REMEMBERED_NONE);
    }

    /// D-006.11: a finished game's roster is stored under the game's TFFA id,
    /// not under the slot it ran in. The identity filter keeps the old main's
    /// spectators (runtime members of a promoted main game) out of the promoted
    /// game's roster, and a `REMEMBERED_NONE` entry never erases a team.
    #[test]
    fn a_finished_game_remembers_its_players_under_its_own_id() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        let mut home = [MAIN_MATCH; MAX_CLIENTS];
        // Clients 2, 3 belong to TFFA 0 (the game being replaced); client 4
        // belongs to TFFA 1 and is only a runtime member of this main game.
        home[2] = MAIN_MATCH;
        home[3] = MAIN_MATCH;
        home[4] = 1;
        home[5] = 1;

        let roster = roster_from_members(
            &[2, 3, 4],
            &[TEAM_RED as u8, TEAM_BLUE as u8, TEAM_RED as u8],
            &home,
            MAIN_MATCH,
        );
        store_return_roster(&mut matches, MAIN_MATCH, &roster);
        assert_eq!(matches[MAIN_MATCH as usize].remembered[2], TEAM_RED as u8);
        assert_eq!(matches[MAIN_MATCH as usize].remembered[3], TEAM_BLUE as u8);
        assert_eq!(
            matches[MAIN_MATCH as usize].remembered[4], REMEMBERED_NONE,
            "another id's client must not join this roster"
        );

        // The promoted game's roster goes to its own (created) slot.
        let promoted = roster_from_members(
            &[2, 4, 5],
            &[TEAM_BLUE as u8, TEAM_RED as u8, TEAM_BLUE as u8],
            &home,
            1,
        );
        store_return_roster(&mut matches, 1, &promoted);
        assert_eq!(matches[1].remembered[4], TEAM_RED as u8);
        assert_eq!(matches[1].remembered[5], TEAM_BLUE as u8);
        assert_eq!(matches[1].remembered[2], REMEMBERED_NONE);
        // Merging keeps an earlier entry for the same client.
        let later = roster_from_members(&[4], &[REMEMBERED_NONE], &home, 1);
        store_return_roster(&mut matches, 1, &later);
        assert_eq!(matches[1].remembered[4], TEAM_RED as u8);
        // Main's roster is untouched by the promoted id's capture.
        assert_eq!(matches[MAIN_MATCH as usize].remembered[2], TEAM_RED as u8);
    }

    /// D-006.11 (the reported bug): TFFA 0 finishes, TFFA 1 is promoted into
    /// main and finishes too. The restart puts each original group back in its
    /// own id — TFFA 0 in slot 0, TFFA 1 in slot 1 — with its own team.
    #[test]
    fn a_promoted_game_returns_to_its_own_id_on_a_map_restart() {
        let mut t = state::TffaState::default();
        // Runtime end state: everyone is assigned to main; TFFA 0's players are
        // spectators remembered in slot 0, TFFA 1's players are remembered in
        // slot 1, and main is running TFFA 1's identity.
        t.matches[MAIN_MATCH as usize].remembered[2] = TEAM_RED as u8;
        t.matches[MAIN_MATCH as usize].remembered[3] = TEAM_BLUE as u8;
        t.matches[1].remembered[4] = TEAM_RED as u8;
        t.matches[1].remembered[5] = TEAM_BLUE as u8;
        t.client_match = [MAIN_MATCH; MAX_CLIENTS];
        t.home[2] = MAIN_MATCH;
        t.home[3] = MAIN_MATCH;
        t.home[4] = 1;
        t.home[5] = 1;
        t.main_origin = 1;

        assert!(reset_round_state(&mut t, true, MAX_MATCHES));
        // Two groups, two ids.
        assert_eq!(t.client_match[2], MAIN_MATCH);
        assert_eq!(t.client_match[3], MAIN_MATCH);
        assert_eq!(t.client_match[4], 1);
        assert_eq!(t.client_match[5], 1);
        assert!(t.matches[1].active);
        assert_eq!(t.pending_join[2], TEAM_RED as u8);
        assert_eq!(t.pending_join[3], TEAM_BLUE as u8);
        assert_eq!(t.pending_join[4], TEAM_RED as u8);
        assert_eq!(t.pending_join[5], TEAM_BLUE as u8);
        // Main hosts its own game again; the promoted game is back in its slot.
        assert_eq!(t.main_origin, MAIN_MATCH);
        assert!(!t.matches[1].has_remembered());
        assert!(!t.matches[MAIN_MATCH as usize].has_remembered());
    }

    /// D-006.11: a promoted game that is still being played (no finished
    /// roster) also returns to its own id; its players keep their session team.
    #[test]
    fn a_live_promoted_game_returns_to_its_own_id() {
        let mut t = state::TffaState::default();
        // TFFA 1 was promoted (its slot inactive), its players are runtime
        // members of main; TFFA 0 finished earlier and waits in main.
        t.home[4] = 1;
        t.home[5] = 1;
        t.client_match[4] = MAIN_MATCH;
        t.client_match[5] = MAIN_MATCH;
        t.matches[MAIN_MATCH as usize].remembered[2] = TEAM_RED as u8;
        t.home[2] = MAIN_MATCH;
        t.main_origin = 1;

        assert!(reset_round_state(&mut t, true, MAX_MATCHES));
        assert!(t.matches[1].active);
        assert_eq!(t.client_match[4], 1);
        assert_eq!(t.client_match[5], 1);
        // A still-playing client keeps the team the base game restores.
        assert_eq!(t.pending_join[4], REMEMBERED_NONE);
        assert_eq!(t.pending_join[5], REMEMBERED_NONE);
        // The finished TFFA 0 group is in main.
        assert_eq!(t.client_match[2], MAIN_MATCH);
        assert_eq!(t.pending_join[2], TEAM_RED as u8);
    }

    /// D-006.11: main's (identity 0) return roster is consumed by a
    /// `map_restart` — each client auto-joins its old team in main, main stays
    /// open — and dropped by a full map change.
    #[test]
    fn main_finishers_come_back_on_their_teams_after_a_map_restart() {
        let mut t = state::TffaState::default();
        t.matches[MAIN_MATCH as usize].remembered[2] = TEAM_RED as u8;
        t.matches[MAIN_MATCH as usize].remembered[3] = TEAM_BLUE as u8;

        let _ = reset_round_state(&mut t, true, MAX_MATCHES);
        assert!(t.matches[MAIN_MATCH as usize].active);
        assert!(!t.matches[MAIN_MATCH as usize].has_remembered());
        assert_eq!(t.client_match[2], MAIN_MATCH);
        assert_eq!(t.client_match[3], MAIN_MATCH);
        assert_eq!(t.pending_join[2], TEAM_RED as u8);
        assert_eq!(t.pending_join[3], TEAM_BLUE as u8);
        // Consumed once: a second restart has nothing left to apply.
        let _ = reset_round_state(&mut t, true, MAX_MATCHES);
        assert_eq!(t.pending_join[2], REMEMBERED_NONE);

        let mut t = state::TffaState::default();
        t.matches[MAIN_MATCH as usize].remembered[2] = TEAM_RED as u8;
        assert!(!reset_round_state(&mut t, false, MAX_MATCHES));
        assert!(!t.matches[MAIN_MATCH as usize].has_remembered());
        assert_eq!(t.pending_join[2], REMEMBERED_NONE);
    }

    /// D-006.11: playing again drops the remembered return team (so a stale
    /// entry cannot override the team the client chose); the id itself is kept,
    /// so the restart still returns them to their own TFFA.
    #[test]
    fn playing_again_forgets_the_remembered_team() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].remembered[4] = TEAM_RED as u8;
        matches[2].remembered[4] = TEAM_BLUE as u8;
        forget_return_rosters_in(&mut matches, 4);
        assert_eq!(matches[MAIN_MATCH as usize].remembered[4], REMEMBERED_NONE);
        assert_eq!(matches[2].remembered[4], REMEMBERED_NONE);
        // Another client's memory is untouched.
        matches[2].remembered[5] = TEAM_BLUE as u8;
        forget_return_rosters_in(&mut matches, 4);
        assert_eq!(matches[2].remembered[5], TEAM_BLUE as u8);
    }

    /// D-006.11: promotion keeps the identity rosters — the finished game's
    /// memory lives in its own slot and the promoted slot is the one cleared.
    #[test]
    fn promoting_a_created_match_keeps_the_other_ids_rosters() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[MAIN_MATCH as usize].remembered[5] = TEAM_RED as u8;
        matches[1].active = true;
        matches[1].remembered[6] = TEAM_BLUE as u8;
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        table[0] = 1;
        promote_into_main(&mut matches, &mut table, 1, 2, 1, &[0]);
        // Main's slot is now TFFA 1's game, but TFFA 0's roster (slot 0) and the
        // promoted slot's own bookkeeping are separate: the promoted slot is
        // cleared, the finishing ids keep their memory.
        assert_eq!(matches[MAIN_MATCH as usize].remembered[5], TEAM_RED as u8);
        assert!(!matches[1].has_remembered());
        assert_eq!(table[0], MAIN_MATCH);
    }

    /// A full map change (or a lowered match limit) drops the memory and the
    /// identities: those players reconnect in main and are not auto-joined.
    #[test]
    fn a_full_map_change_drops_a_remembered_roster() {
        let mut t = state::TffaState::default();
        t.matches[2].remembered[6] = TEAM_RED as u8;
        t.home[6] = 2;
        assert!(!reset_round_state(&mut t, false, MAX_MATCHES));
        assert!(!t.matches[2].active);
        assert!(!t.matches[2].has_remembered());
        assert_eq!(t.pending_join[6], REMEMBERED_NONE);
        assert_eq!(t.home[6], MAIN_MATCH);

        let mut t = state::TffaState::default();
        t.matches[5].remembered[6] = TEAM_BLUE as u8;
        t.home[6] = 5;
        assert!(!reset_round_state(&mut t, true, 3));
        assert!(!t.matches[5].active);
        assert!(!t.matches[5].has_remembered());
        assert_eq!(t.client_match[6], MAIN_MATCH);
        assert_eq!(t.home[6], MAIN_MATCH);
        assert_eq!(t.pending_join[6], REMEMBERED_NONE);
    }

    /// A new match takes an empty slot first and only evicts a waiting group
    /// when no empty slot is left; the id of a live promoted game is never
    /// handed out.
    #[test]
    fn a_new_match_prefers_a_slot_without_a_waiting_group() {
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        matches[1].remembered[0] = TEAM_RED as u8;
        let table = [MAIN_MATCH; MAX_CLIENTS];
        let mut home = [MAIN_MATCH; MAX_CLIENTS];
        home[0] = 1;

        assert_eq!(
            select_create_slot(&matches, &table, &home, MAX_MATCHES, MAIN_MATCH),
            Some(2)
        );
        // Only the waiting slot is left: the new match reuses it (and
        // `cmd_create` clears the old roster and identity).
        assert_eq!(
            select_create_slot(&matches, &table, &home, 1, MAIN_MATCH),
            Some(1)
        );

        // An active or still-assigned slot is never taken.
        matches[1].active = true;
        let mut table = [MAIN_MATCH; MAX_CLIENTS];
        table[3] = 2;
        matches[2].remembered[0] = TEAM_RED as u8;
        home[0] = 2;
        assert_eq!(
            select_create_slot(&matches, &table, &home, 2, MAIN_MATCH),
            None
        );

        // A game promoted into the main slot keeps its own id reserved, even
        // though its slot is inactive and nobody is assigned to it.
        let mut matches = [state::TffaMatch::default(); TOTAL_MATCH_SLOTS];
        matches[MAIN_MATCH as usize].active = true;
        let free_table = [MAIN_MATCH; MAX_CLIENTS];
        let no_home = [MAIN_MATCH; MAX_CLIENTS];
        assert_eq!(
            select_create_slot(&matches, &free_table, &no_home, 2, 1),
            Some(2)
        );
        assert_eq!(
            select_create_slot(&matches, &free_table, &no_home, 1, 1),
            None,
            "the live promoted id is reserved"
        );
    }

    #[test]
    fn follow_sentinels_resolve_through_level_follow1_and_follow2() {
        // `team follow1`/`follow2` store -1/-2 in `sess.spectatorClient`;
        // pristine `SpectatorClientEndFrame` resolves them to `level.follow1`
        // / `level.follow2` (`g_active.c:3660-3668`). The CS_PLAYERS/scores
        // view must use the same target or a spectator watching through an
        // alias keeps their own instance while the snapshot shows the
        // followed one (the reported `(TFFA N)` names on live players).
        assert_eq!(resolve_follow_target(-1, 7, 3), Some(7));
        assert_eq!(resolve_follow_target(-2, 7, 3), Some(3));
        // A direct clientNum is its own target.
        assert_eq!(resolve_follow_target(5, 7, 3), Some(5));
        // No playing client (CalculateRanks leaves them -1) / out of range:
        // nothing to follow.
        assert_eq!(resolve_follow_target(-1, -1, 3), None);
        assert_eq!(resolve_follow_target(-2, 7, -1), None);
        assert_eq!(resolve_follow_target(MAX_CLIENTS as c_int, 7, 3), None);
        assert_eq!(resolve_follow_target(-3, 7, 3), None);
    }
}
