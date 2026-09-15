//! Parallel self-organized TFFA matches on one `GT_TEAM` map.
//!
//! Match `0` is the **main TFFA**: every connected client belongs to it
//! unless they create/join another match; it is always active and its
//! fraglimit is enforced like any other match. Slots `1..=MAX_MATCHES` are
//! player-created. Scores are kept proxy-side per match (team tally +
//! per-player copy); the game's global `level.teamScores` is neutralised
//! (see `on_add_score`) so pristine `CheckExitRules` never ends the server
//! early, and the `scores` trap rewrite substitutes the per-player copies so
//! alternative matches never show main-TFFA-era numbers. Every match shares
//! the server's own `fraglimit`; `timelimit` stays global and ends
//! everything.
//!
//! Isolation:
//! - damage: `G_Damage`/`player_die` wrappers drop cross-match effects.
//! - visibility: `SV_AddEntitiesVisibleFromPoint` post-filter drops
//!   cross-match player/owned entities (kills ride as `EV_OBITUARY` temp
//!   entities, so the kill feed is hidden the same way).
//! - physics: the `G_TRACE`/`G_G2TRACE`/`G_TRACECAPSULE` traps flip
//!   `r.contents` of cross-match entities for the duration of the trace, so
//!   other matches neither body-block nor saber-clash (`trace_flips`).
//! - scoreboard: `G_SEND_SERVER_COMMAND` trap rewrites `scores ...` per
//!   viewer with the per-player per-match score copies.
//! - chat is intentionally left global.

use core::ffi::{c_char, c_int};

use crate::hooks::engine_sv::{read_i32, read_usize};
use crate::sdk::{
    CS_PLAYERS, CS_SCORES1, CS_SCORES2, GT_TEAM, MAX_CLIENTS, MAX_GENTITIES, MAX_NETNAME,
    MAX_TOKEN_CHARS, OFFSET_GCLIENT_SESS, OFFSET_GENTITY_CLIENT, OFFSET_GENTITY_R_CONTENTS,
    OFFSET_GENTITY_R_LINKED, OFFSET_GENTITY_R_OWNER, OFFSET_SESS_SESSION_TEAM, TEAM_BLUE, TEAM_RED,
    TEAM_SPECTATOR,
};
use crate::state::{self, CVAR_TFFA_ENABLE, CVAR_TFFA_MAX_GROUP_SIZE, CVAR_TFFA_MAX_MATCHES};

/// Match id of the implicit main TFFA.
pub const MAIN_MATCH: u8 = 0;
/// Maximum player-created matches (ids `1..=MAX_MATCHES`).
pub const MAX_MATCHES: usize = 8;
/// Backing array size: main slot + created slots.
pub const TOTAL_MATCH_SLOTS: usize = 1 + MAX_MATCHES;
/// Capacity of the per-frame player-owned entity cache (saber entities,
/// missiles, corpses — one or a few per player).
pub const OWNED_CAP: usize = 96;
/// `scores ...` entries carry 14 ints each (`g_cmds.c:64-74`); the second
/// int is `PERS_SCORE` (`g_cmds.c:65-66`).
const SCORE_ENTRY_INTS: usize = 14;
const SCORE_ENTRY_SCORE: usize = 1;

/// Whether the feature is on: master switch + `proxy_tffa_enable` + `GT_TEAM`.
pub fn enabled() -> bool {
    if !state::proxy_enabled() {
        return false;
    }
    let on = state::with_state(|s| s.cvars[CVAR_TFFA_ENABLE].integer != 0);
    if !on {
        return false;
    }
    // SAFETY: engine syscall registered (GAME_INIT ran before any hook/frame).
    let gt = unsafe { crate::syscall::cvar_variable_integer_value(c"g_gametype") };
    gt == GT_TEAM
}

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
    let target = unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SPECTATOR_CLIENT) };
    if !(0..MAX_CLIENTS as c_int).contains(&target) {
        return None;
    }
    let tent = g_entities + target as usize * SIZEOF_GENTITY;
    // SAFETY: target slot is inside the g_entities array.
    let tclient = unsafe { read_usize(tent + OFFSET_GENTITY_CLIENT) };
    if tclient == 0 {
        return None;
    }
    Some(target)
}

/// Pure damage gate: blocked unless both ends share one match.
pub fn damage_blocked(attacker_match: u8, victim_match: u8) -> bool {
    attacker_match != victim_match
}

/// Pure snapshot gate: `true` = keep entity for viewer.
pub fn entity_visible(viewer_match: u8, ent_match: u8) -> bool {
    viewer_match == ent_match
}

/// Whether a per-match score has hit the shared `fraglimit`.
pub fn fraglimit_hit(red: i32, blue: i32, fraglimit: i32) -> bool {
    fraglimit > 0 && (red >= fraglimit || blue >= fraglimit)
}

// ---------------------------------------------------------------------------
// scores trap rewrite
// ---------------------------------------------------------------------------

/// Rewrite a `scores ...` server command for `viewer`:
/// unified roster — every connected client is listed (outsiders appear in the
/// spectator section client-side via the per-viewer `CS_PLAYERS` team
/// override), the team header is the viewer's own match red/blue, and each
/// entry's `PERS_SCORE` is the per-match copy.
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
    let viewer_match = effective_match(viewer);
    let (mred, mblue, player_scores) = state::with_state(|s| {
        let m = &s.tffa.matches[viewer_match as usize];
        (m.red, m.blue, s.tffa.player_score)
    });
    Some(filter_scores(mred, mblue, &player_scores, &nums[3..]))
}

/// Pure `scores ...` rewrite (no global state): keep every entry, substitute
/// per-match red/blue header and each player's per-match score copy.
/// Outsider-vs-own separation happens client-side via the per-viewer
/// `CS_PLAYERS` team override (outsiders forced to spectator with a
/// `(TFFA N)` name prefix), so the tab lists everyone.
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
/// get a `(TFFA N)` name prefix and are forced to spectator so the team
/// scoreboard lists them in the spectator section; everyone else (own match,
/// real spectators) is left untouched. Returns `None` when no change.
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

/// Send `cs <index> "<value>"` to one client (the same wire format the
/// engine's own `SV_SetConfigstring` broadcast uses, so the client's
/// `gameState` updates identically).
fn send_cs_to_client(client_num: c_int, index: i32, value: &str) {
    let msg = format!("cs {index} \"{value}\"\n");
    let Ok(cmsg) = std::ffi::CString::new(msg) else {
        return;
    };
    // SAFETY: engine syscall registered; cmsg NUL-terminated.
    unsafe { crate::syscall::send_server_command(client_num, &cmsg) };
}

/// Record a `CS_PLAYERS + n` base string the game just set, then push the
/// per-viewer variants (prefixed outsiders) to every connected client.
/// Call after the trap has been forwarded so the engine broadcast lands
/// first and our per-viewer corrections win.
pub fn on_set_cs_players(slot: i32, value: &str) {
    if !enabled() {
        return;
    }
    if !(0..MAX_CLIENTS as i32).contains(&slot) {
        return;
    }
    state::with_state(|s| {
        s.tffa.cs_players_base[slot as usize] = value.to_owned();
    });
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
    let index = CS_PLAYERS + slot;
    for viewer in viewers {
        let viewer_match = effective_match(viewer);
        // Same-match / spectator viewers already received this base via the
        // engine broadcast we just forwarded — resending it doubles traffic
        // for zero effect (and overflows the reliable buffer on team swaps).
        if let Some(modified) = rewrite_cs_players_for_viewer(&base, table, viewer_match) {
            send_cs_to_client(viewer, index, &modified);
        }
    }
}

/// Broadcast one player's slot to every viewer from the stored base (called
/// after proxy-side match moves, which change prefix/team without any game
/// `G_SET_CONFIGSTRING`).
pub fn broadcast_cs_players_slot(slot: i32) {
    if !enabled() {
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
    let index = CS_PLAYERS + slot;
    for viewer in viewers {
        let viewer_match = effective_match(viewer);
        if let Some(modified) = rewrite_cs_players_for_viewer(&base, table, viewer_match) {
            send_cs_to_client(viewer, index, &modified);
        } else {
            send_cs_to_client(viewer, index, &base);
        }
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
        let m = &matches[vm as usize];
        send_scores_to_viewer(viewer, m.red, m.blue);
    }
}

/// Send `CS_SCORES1/2` to one viewer unless identical to the last push
/// (reliable-buffer guard: every frag triggers `CalculateRanks` twice plus
/// our fresh push — resending unchanged scores to everyone overflows).
fn send_scores_to_viewer(viewer: c_int, red: i32, blue: i32) {
    let skip = state::with_state(|s| {
        if !(0..MAX_CLIENTS as c_int).contains(&viewer) {
            return true;
        }
        if s.tffa.cs_scores_sent[viewer as usize] == (red, blue) {
            return true;
        }
        s.tffa.cs_scores_sent[viewer as usize] = (red, blue);
        false
    });
    if skip {
        return;
    }
    send_cs_to_client(viewer, CS_SCORES1, &red.to_string());
    send_cs_to_client(viewer, CS_SCORES2, &blue.to_string());
}

/// Live server configstring for a player slot (fallback when the proxy base
/// is empty, e.g. the slot was assigned while the feature was off).
fn get_live_cs_players(slot: i32) -> Option<String> {
    let mut buf = [0u8; 1024];
    // SAFETY: engine syscall registered; buffer writable.
    unsafe { crate::syscall::get_configstring(CS_PLAYERS + slot, &mut buf) };
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    if n == 0 {
        return None;
    }
    String::from_utf8(buf[..n].to_vec()).ok()
}

/// Push every current per-viewer override to one client (fresh connects get
/// the server gamestate with base strings, then these corrections).
pub fn send_all_overrides_to_client(client_num: c_int) {
    if !enabled() {
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
        if let Some(modified) =
            rewrite_cs_players_for_viewer(&base, table[slot as usize], viewer_match)
        {
            send_cs_to_client(client_num, CS_PLAYERS + slot, &modified);
        } else {
            send_cs_to_client(client_num, CS_PLAYERS + slot, &base);
        }
    }
}

/// Refresh one viewer's `CS_PLAYERS` overrides for every slot (called when
/// that viewer changes matches, so stale prefixes/teams are corrected).
pub fn refresh_cs_players_for_viewer(client_num: c_int) {
    if !enabled() {
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
        let index = CS_PLAYERS + slot;
        if let Some(modified) =
            rewrite_cs_players_for_viewer(&base, table[slot as usize], viewer_match)
        {
            send_cs_to_client(client_num, index, &modified);
        } else {
            // Own match / spectator: restore the base string (it may carry a
            // stale prefix from a previous viewer_match).
            send_cs_to_client(client_num, index, &base);
        }
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

/// Clear accumulated stats for one slot (match moves/finish start the next
/// instance clean; cross-match damage is never recorded anyway, so only
/// same-instance history is dropped — for the mover as viewer and for others
/// as opponent — plus the team counters).
fn reset_slot_stats(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    state::with_state(|s| {
        let n = client_num as usize;
        s.clients[n].game_stats = [crate::state::GameStats::default(); MAX_CLIENTS];
        for other in 0..MAX_CLIENTS {
            s.clients[other].game_stats[n] = crate::state::GameStats::default();
        }
        s.clients[n].team_kills = 0;
        s.clients[n].team_killed = 0;
        s.clients[n].team_damages_given = 0;
        s.clients[n].team_damages_taken = 0;
    });
    reset_game_scores(client_num);
}

/// Reset the game's own per-player score counters for one slot
/// (`ps.persistant[PERS_SCORE/PERS_KILLED]` accumulate all map, which would
/// otherwise trip pristine `CheckExitRules`' per-player fraglimit as soon as
/// any limit is set — and would show stale totals on the per-match HUD).
/// Mirrors the proxy per-match copies (reset alongside them on every move).
fn reset_game_scores(client_num: c_int) {
    use crate::sdk::{OFFSET_GCLIENT_PS, OFFSET_PS_PERSISTANT, PERS_KILLED, PERS_SCORE};
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
    // SAFETY: client is a valid gclient_t; persistant slots are plain ints.
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
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return None;
    }
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
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
/// Whether the client is currently spectating (unknown → false, fail closed:
/// travel commands refuse without game memory).
pub fn is_spectator(client_num: c_int) -> bool {
    session_team_of(client_num) == Some(TEAM_SPECTATOR)
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
/// zero, created matches close, the main slot reopens. Client assignments
/// are kept (players keep their groups across a `map_restart`; a full map
/// change re-dlopens the module and starts from a fresh state anyway).
pub fn reset_round() {
    state::with_state(|s| {
        for (i, m) in s.tffa.matches.iter_mut().enumerate() {
            m.red = 0;
            m.blue = 0;
            m.active = i == MAIN_MATCH as usize;
        }
        s.tffa.player_score = [0; MAX_CLIENTS];
        s.tffa.owned_count = 0;
        s.tffa.rebuilt_stamp = 0;
        // Re-arm the score dedup so round-start 0-0 is pushed.
        s.tffa.cs_scores_sent = [(i32::MIN, i32::MIN); MAX_CLIENTS];
        // New round: fraglimit handling runs again.
        s.tffa.intermission = false;
    });
}
/// Clear all TFFA state for a fresh first-time connect (the slot may have
/// been used by a different player before).
pub fn on_client_connect(client_num: c_int, first_time: bool) {
    if !first_time || !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    state::with_state(|s| {
        s.tffa.client_match[client_num as usize] = MAIN_MATCH;
        s.tffa.player_score[client_num as usize] = 0;
        s.tffa.cs_players_base[client_num as usize].clear();
        s.tffa.cs_scores_sent[client_num as usize] = (i32::MIN, i32::MIN);
        s.tffa.viewer_match_cache[client_num as usize] = MAIN_MATCH;
    });
}

/// Clear all TFFA state for a disconnecting client.
pub fn on_client_disconnect(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    state::with_state(|s| {
        s.tffa.client_match[client_num as usize] = MAIN_MATCH;
        s.tffa.player_score[client_num as usize] = 0;
        s.tffa.cs_players_base[client_num as usize].clear();
        s.tffa.cs_scores_sent[client_num as usize] = (i32::MIN, i32::MIN);
        s.tffa.viewer_match_cache[client_num as usize] = MAIN_MATCH;
    });
    // The slot is free: other viewers' stale prefixed views of it will be
    // refreshed when the game clears `CS_PLAYERS + n` (empty base). Push an
    // immediate restore so a stale `(TFFA N)` does not linger.
    if enabled() {
        let viewers: Vec<c_int> = state::with_state(|s| {
            (0..MAX_CLIENTS as c_int)
                .filter(|v| s.clients[*v as usize].is_connected)
                .collect()
        });
        for viewer in viewers {
            send_cs_to_client(viewer, CS_PLAYERS + client_num, "");
        }
    }
}

/// Push the current per-viewer overrides to a client that just began
/// (it received the server gamestate with base strings; these corrections
/// apply its match's scores and prefixed outsiders).
pub fn on_client_begin(client_num: c_int) {
    if !enabled() {
        return;
    }
    send_all_overrides_to_client(client_num);
}

/// Bump the frame stamp so the owned-entity cache is rebuilt once per game
/// frame (called from `GAME_RUN_FRAME` before the frame is forwarded).
pub fn on_frame_start() {
    state::with_state(|s| s.tffa.frame_stamp = s.tffa.frame_stamp.wrapping_add(1));
    if enabled() {
        clamp_global_scores();
    }
}

/// Per-frame: finish matches that hit the shared fraglimit. Sub-match
/// instances are announced to **everyone** with final teams/scores, members
/// moved back to main as spectators, stats printed to members + spectators.
/// When the main itself finishes, the lowest-id still-played TFFA becomes the
/// new main; with no other active TFFA the map ends via pristine intermission
/// (final scoreboard + standard stats for all).
/// Runs after the frame is forwarded so scores from this frame are visible.
pub fn on_run_frame_post() {
    if !enabled() {
        return;
    }
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
    // While an intermission runs (pristine timelimit exit or a solo-main
    // finish below) the map is ending: no more fraglimit handling.
    if state::with_state(|s| s.tffa.intermission) {
        return;
    }
    // SAFETY: engine syscall registered.
    let fraglimit = unsafe { crate::syscall::cvar_variable_integer_value(c"fraglimit") };
    if fraglimit <= 0 {
        return;
    }
    struct Finished {
        id: u8,
        red: i32,
        blue: i32,
        members: Vec<c_int>,
        red_names: Vec<String>,
        blue_names: Vec<String>,
    }
    let mut finished: Vec<Finished> = Vec::new();
    state::with_state(|s| {
        for (i, m) in s.tffa.matches.iter().enumerate() {
            if m.active && fraglimit_hit(m.red, m.blue, fraglimit) {
                let id = i as u8;
                // Only live assignments: free slots default to main and would
                // otherwise inflate main finishes to 30 "members" (reliable
                // overflow + SetTeam on empty entities).
                let members: Vec<c_int> = s
                    .tffa
                    .client_match
                    .iter()
                    .enumerate()
                    .filter(|(i, m)| **m == id && s.clients[*i].is_connected)
                    .map(|(i, _)| i as c_int)
                    .collect();
                // Team split + names from the stored CS_PLAYERS bases (game
                // `t` team + `n` netname at last update).
                let mut red_names = Vec::new();
                let mut blue_names = Vec::new();
                for cn in &members {
                    let base = &s.tffa.cs_players_base[*cn as usize];
                    let team = base.is_empty().then_some(None).unwrap_or_else(|| {
                        info_value(base, "t").and_then(|t| t.parse::<i32>().ok())
                    });
                    let name = if base.is_empty() {
                        format!("player{cn}")
                    } else {
                        announce_name(
                            &info_value(base, "n").unwrap_or_else(|| format!("player{cn}")),
                        )
                    };
                    match team {
                        Some(TEAM_RED) => red_names.push(name),
                        Some(TEAM_BLUE) => blue_names.push(name),
                        _ => {}
                    }
                }
                finished.push(Finished {
                    id,
                    red: m.red,
                    blue: m.blue,
                    members,
                    red_names,
                    blue_names,
                });
            }
        }
        for f in &finished {
            // Main keeps its final tallies for the intermission scoreboard
            // when it ends solo (see below); subs reset as usual.
            if f.id != MAIN_MATCH {
                let m = &mut s.tffa.matches[f.id as usize];
                m.red = 0;
                m.blue = 0;
                m.active = false;
            }
        }
    });
    if finished.is_empty() {
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
            "print \"TFFA {} ended: RED {} - BLUE {}. RED: {} | BLUE: {}. Players returned to main as spectator.\n\"",
            f.id, f.red, f.blue, red_list, blue_list
        ));
        crate::hooks::game::print_tffa_match_stats(&f.members);
        move_match_to_spectator(f.id);
    }
    let Some(main) = finished.iter().find(|f| f.id == MAIN_MATCH) else {
        return;
    };
    if main.members.is_empty() {
        // Nobody home (connect churn window): drop the stale lead silently so
        // it cannot re-fire every frame; the next real frag starts clean.
        state::with_state(|s| {
            s.tffa.matches[MAIN_MATCH as usize].red = 0;
            s.tffa.matches[MAIN_MATCH as usize].blue = 0;
        });
        return;
    }
    {
        // Lowest-id still-played TFFA becomes the new main.
        let max = max_matches();
        let candidate: Option<(u8, i32, i32, Vec<c_int>)> = state::with_state(|s| {
            for i in 1..=max {
                if i >= s.tffa.matches.len() {
                    break;
                }
                let m = &s.tffa.matches[i];
                if !m.active {
                    continue;
                }
                let members: Vec<c_int> = s
                    .tffa
                    .client_match
                    .iter()
                    .enumerate()
                    .filter(|(ci, mm)| **mm == i as u8 && s.clients[*ci].is_connected)
                    .map(|(ci, _)| ci as c_int)
                    .collect();
                if members.is_empty() {
                    continue;
                }
                return Some((i as u8, m.red, m.blue, members));
            }
            None
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
                "print \"main (TFFA 0) ended: RED {} - BLUE {}. RED: {} | BLUE: {}. Players returned to main as spectator.\n\"",
                main.red, main.blue, red_list, blue_list
            ));
            crate::hooks::game::print_tffa_match_stats(&main.members);
            move_match_to_spectator(MAIN_MATCH);
            state::with_state(|s| {
                s.tffa.matches[MAIN_MATCH as usize].red = red;
                s.tffa.matches[MAIN_MATCH as usize].blue = blue;
                s.tffa.matches[MAIN_MATCH as usize].active = true;
                s.tffa.matches[id as usize].red = 0;
                s.tffa.matches[id as usize].blue = 0;
                s.tffa.matches[id as usize].active = false;
                for cn in &members {
                    s.tffa.client_match[*cn as usize] = MAIN_MATCH;
                }
            });
            for cn in &members {
                broadcast_cs_players_slot(*cn);
                refresh_cs_players_for_viewer(*cn);
            }
            on_set_cs_scores();
            announce_to_all(&format!(
                "print \"TFFA {id} becomes the new main (RED {red} - BLUE {blue}).\n\""
            ));
            return;
        }
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
            "print \"main (TFFA 0) ended: RED {} - BLUE {}. RED: {} | BLUE: {}. Final scoreboard incoming.\n\"",
            main.red, main.blue, red_list, blue_list
        ));
        crate::hooks::game::run_begin_intermission();
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

/// Force every member of a match to spectator through the pristine `SetTeam`
/// body (trampoline, not the wrapper → no recursion, no teamlock block:
/// spectator requests always pass). Members return to the main slot so they
/// can see it and join elsewhere.
fn move_match_to_spectator(match_id: u8) {
    let base = crate::original::base();
    if base == 0 {
        return;
    }
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
        return;
    }
    // SAFETY: trampoline holds the pristine SetTeam body (attached at GAME_INIT).
    let tramp = crate::patch::GAME_HOOKS[5].original();
    if tramp == 0 {
        return;
    }
    let original: unsafe extern "C" fn(usize, *const c_char) =
        unsafe { core::mem::transmute(tramp) };
    let members: Vec<usize> = state::with_state(|s| {
        s.tffa
            .client_match
            .iter()
            .enumerate()
            .filter(|(i, m)| **m == match_id && s.clients[*i].is_connected)
            .map(|(i, _)| g_entities + i * crate::sdk::SIZEOF_GENTITY)
            .collect()
    });
    for ent in members {
        let cn = ((ent - g_entities) / crate::sdk::SIZEOF_GENTITY) as c_int;
        // Free slots have no client — never call the game body on them
        // (null `ent->client` deref would take the server down); just reset
        // the proxy assignment.
        // SAFETY: ent is inside the g_entities array.
        let has_client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) } != 0;
        if has_client {
            // SAFETY: ent points into g_entities; spectator string is static.
            unsafe { original(ent, c"spectator".as_ptr()) };
        }
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

// ---------------------------------------------------------------------------
// Physics isolation (G_TRACE / G_G2TRACE / G_TRACECAPSULE interception)
// ---------------------------------------------------------------------------

/// Build the flip list for one trace: the `r.contents` slot of every
/// cross-match entity (players + player-owned entities) the requester must
/// not collide with, paired with the saved value for the restore. Returns
/// the number of entries written into `out` (capped at its length).
///
/// The engine's `SV_ClipMoveToEntities` (`sv_world.cpp:589`) skips entities
/// whose `r.contents` has no bit in the trace's contentmask, so zeroing
/// `contents` for the duration of this single synchronous trap makes
/// cross-match players, sabers, missiles and corpses intangible game-side
/// (no body blocking, no saber clashes) without touching movement code.
pub fn trace_flips(pass_entity_num: c_int, out: &mut [(usize, i32)]) -> usize {
    if out.is_empty() || !enabled() {
        return 0;
    }
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
        let table = s.tffa.client_match;
        // The trace's requester: a player slot, or (missiles/sabers/other
        // ents) the `r.ownerNum` player. Unknown/world requesters keep the
        // world solid everywhere (no filtering).
        let requester = request_match(g_entities, pass_entity_num, &table);
        let Some(requester) = requester else {
            return 0;
        };
        let mut n = 0usize;
        // Player entities.
        for cn in 0..MAX_CLIENTS as i32 {
            if table[cn as usize] == requester {
                continue;
            }
            let ent = g_entities + cn as usize * crate::sdk::SIZEOF_GENTITY;
            // SAFETY: ent is inside the g_entities array.
            let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
            if client == 0 {
                continue;
            }
            let addr = ent + OFFSET_GENTITY_R_CONTENTS;
            // SAFETY: contents is a plain int inside gentity_t.
            let contents = unsafe { read_i32(addr) };
            if contents != 0 && n < out.len() {
                out[n] = (addr, contents);
                n += 1;
            }
        }
        // Player-owned entities (saber ents, missiles, corpses).
        for k in 0..s.tffa.owned_count {
            let (num, owner) = s.tffa.owned[k];
            if table[owner as usize] == requester {
                continue;
            }
            let addr =
                g_entities + num as usize * crate::sdk::SIZEOF_GENTITY + OFFSET_GENTITY_R_CONTENTS;
            // SAFETY: addr is inside gentity_t.
            let contents = unsafe { read_i32(addr) };
            if contents != 0 && n < out.len() {
                out[n] = (addr, contents);
                n += 1;
            }
        }
        n
    })
}

/// The match whose trace this is: player slots resolve directly; other
/// entities resolve once through `r.ownerNum` (missiles, saber entities).
/// World/unknown requesters get `None` (no isolation for that trace).
fn request_match(g_entities: usize, ent_num: c_int, table: &[u8; MAX_CLIENTS]) -> Option<u8> {
    let live_player = |num: i32| -> bool {
        if !(0..MAX_CLIENTS as i32).contains(&num) {
            return false;
        }
        // SAFETY: num is a valid g_entities slot.
        unsafe {
            read_usize(
                g_entities + num as usize * crate::sdk::SIZEOF_GENTITY + OFFSET_GENTITY_CLIENT,
            ) != 0
        }
    };
    if live_player(ent_num) {
        return Some(table[ent_num as usize]);
    }
    if (0..MAX_GENTITIES as i32).contains(&ent_num) {
        // SAFETY: ent_num is a valid gentity index.
        let owner = unsafe {
            read_i32(
                g_entities + ent_num as usize * crate::sdk::SIZEOF_GENTITY + OFFSET_GENTITY_R_OWNER,
            )
        };
        if live_player(owner) {
            return Some(table[owner as usize]);
        }
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
    if !state::proxy_enabled() {
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
        let id = cmd_arg_i32(1);
        cmd_join(client_num, id);
        return true;
    }
    if name == b"tffa_leave" {
        cmd_leave(client_num);
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

fn require_spectator(client_num: c_int) -> bool {
    if is_spectator(client_num) {
        return true;
    }
    tell(
        client_num,
        "print \"You must be spectator to travel between TFFA matches (use /team spectator first).\n\"",
    );
    false
}

fn cmd_arg_i32(n: c_int) -> i32 {
    let mut buf = [0u8; 64];
    // SAFETY: engine syscall registered; buffer writable.
    unsafe { crate::syscall::argv(n, &mut buf) };
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    core::str::from_utf8(&buf[..end])
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(0)
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
    (v as usize).clamp(1, MAX_MATCHES)
}

fn max_group_size() -> usize {
    let v = state::with_state(|s| s.cvars[CVAR_TFFA_MAX_GROUP_SIZE].integer);
    (v as usize).clamp(2, MAX_CLIENTS)
}

fn match_size(match_id: u8) -> usize {
    state::with_state(|s| {
        s.tffa
            .client_match
            .iter()
            .filter(|m| **m == match_id)
            .count()
    })
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
    if !require_spectator(client_num) {
        return;
    }
    // Leave current match first.
    cmd_leave_silent(client_num);
    // Hoisted: `max_matches()` itself takes the state lock, and `with_state`
    // is non-reentrant — calling it inside the closure below deadlocks the
    // engine thread (the `tffa_list`/`tffa_create` server crash).
    let max = max_matches();
    let mut id = 0u8;
    state::with_state(|s| {
        for (i, m) in s.tffa.matches.iter_mut().enumerate() {
            if i == MAIN_MATCH as usize {
                continue; // slot 0 is the implicit main match
            }
            if i >= max {
                break;
            }
            if !m.active && s.tffa.client_match.iter().all(|c| *c != i as u8) {
                m.active = true;
                m.red = 0;
                m.blue = 0;
                id = i as u8;
                break;
            }
        }
        if id != 0 {
            s.tffa.client_match[client_num as usize] = id;
            s.tffa.player_score[client_num as usize] = 0;
        }
    });
    if id == 0 {
        tell(client_num, "print \"No free match slot.\n\"");
    } else {
        // New assignment: other viewers need the prefixed view of this slot,
        // and the creator needs their own match's scores/names.
        reset_slot_stats(client_num);
        broadcast_cs_players_slot(client_num);
        refresh_cs_players_for_viewer(client_num);
        tell(
            client_num,
            &format!(
                "print \"Created match {id}. Team up with /team red|blue, invite others with tffa_join {id}.\n\""
            ),
        );
    }
}

fn cmd_join(client_num: c_int, id: i32) {
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
    if !require_spectator(client_num) {
        return;
    }
    if !(1..=(MAX_MATCHES as i32)).contains(&id) || id as usize > max_matches() {
        tell(
            client_num,
            "print \"Usage: tffa_join <match 1..N> (see tffa_list).\n\"",
        );
        return;
    }
    let id = id as u8;
    let active = state::with_state(|s| s.tffa.matches[id as usize].active);
    if !active {
        tell(client_num, "print \"That match is not active.\n\"");
        return;
    }
    if match_size(id) >= max_group_size() {
        tell(client_num, "print \"That match is full.\n\"");
        return;
    }
    let old = state::with_state(|s| s.tffa.client_match[client_num as usize]);
    if old != id {
        cmd_leave_silent(client_num);
        state::with_state(|s| {
            s.tffa.client_match[client_num as usize] = id;
            // Fresh scoreboard for the new match (the game's PERS_SCORE
            // accumulates across the whole map; the proxy copies are
            // per-match).
            s.tffa.player_score[client_num as usize] = 0;
        });
        reset_slot_stats(client_num);
        broadcast_cs_players_slot(client_num);
        refresh_cs_players_for_viewer(client_num);
    }
    tell(client_num, &format!("print \"Joined match {id}.\n\""));
}

fn cmd_leave(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    if !require_spectator(client_num) {
        return;
    }
    cmd_leave_silent(client_num);
    broadcast_cs_players_slot(client_num);
    refresh_cs_players_for_viewer(client_num);
    tell(client_num, "print \"Left your match (back to main).\n\"");
}

fn cmd_leave_silent(client_num: c_int) {
    state::with_state(|s| {
        s.tffa.client_match[client_num as usize] = MAIN_MATCH;
        s.tffa.player_score[client_num as usize] = 0;
    });
}

fn cmd_list(client_num: c_int) {
    // Hoisted before `with_state` (same non-reentrancy as `cmd_create`).
    let max = max_matches();
    let mut out = String::from("Matches:");
    state::with_state(|s| {
        for (i, m) in s.tffa.matches.iter().enumerate() {
            if i == MAIN_MATCH as usize {
                let size = s.tffa.client_match.iter().filter(|c| **c == 0).count();
                out.push_str(
                    format!("\n0 (main): {size} players, {}-{}^7", m.red, m.blue).as_str(),
                );
                continue;
            }
            if i > max {
                break;
            }
            let id = i as u8;
            if !m.active {
                continue;
            }
            let size = s.tffa.client_match.iter().filter(|c| **c == id).count();
            out.push_str(format!("\n{id}: {size} players, {}-{}^7", m.red, m.blue).as_str());
        }
    });
    out.push('\n');
    tell(client_num, &format!("print \"{out}\""));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::{ENTITYNUM_NONE, ENTITYNUM_WORLD};

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

    #[test]
    fn entity_visible_only_same_match() {
        assert!(entity_visible(1, 1));
        assert!(!entity_visible(1, 2));
        assert!(!entity_visible(MAIN_MATCH, 1));
        assert!(entity_visible(MAIN_MATCH, MAIN_MATCH));
    }

    #[test]
    fn fraglimit_hit_shared_value() {
        assert!(fraglimit_hit(10, 3, 10));
        assert!(fraglimit_hit(3, 10, 10));
        assert!(!fraglimit_hit(9, 9, 10));
        assert!(!fraglimit_hit(10, 10, 0));
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
        // Unified roster: everyone is listed, global PERS_SCORE values
        // (5,7,9) replaced by the per-match copies (3); header is the
        // viewer's own match tallies. Outsider-vs-own separation happens
        // client-side via the CS_PLAYERS team override.
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

    #[test]
    fn entitynum_constants_match_sdk() {
        assert_eq!(ENTITYNUM_NONE, 1023);
        assert_eq!(ENTITYNUM_WORLD, 1022);
    }
}
