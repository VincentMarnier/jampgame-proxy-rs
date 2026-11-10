//! Parallel duels on `GT_DUEL` — the pure ladder model (D-025).
//!
//! This module owns the policy of the parallel duel ladder: willingness, the
//! Fisher-Yates shuffle, court formation, the per-round bump, resolution of a
//! court, the ladder-long standings, the champion check and the board order.
//! Everything here is pure: no engine memory, no game traps, no proxy state
//! lock. The lifecycle wiring lives in `crate::duel`'s callers and the hooks.
//!
//! The shared instancing core (`crate::instances`) supplies the assignment
//! slate the courts are written into: court 1 runs in the main instance slot
//! (the pristine world), courts 2..C in the created clone slots.

use core::ffi::{CStr, c_int};

use crate::hooks::engine_sv::{read_i32, read_usize, write_i32};
use crate::original;
use crate::sdk::{
    CON_CONNECTED, CS_CLIENT_DUELHEALTHS, CS_CLIENT_DUELISTS, CS_SCORES1, CS_SCORES2, EXEC_APPEND,
    MAX_CLIENTS, OFFSET_GCLIENT_PERS, OFFSET_GCLIENT_PS, OFFSET_GCLIENT_SESS,
    OFFSET_GENTITY_CLIENT, OFFSET_LEVEL_SORTED_CLIENTS, OFFSET_LEVEL_TIME, OFFSET_PERS_CONNECTED,
    OFFSET_PS_PERSISTANT, OFFSET_PS_STATS, OFFSET_SESS_LOSSES, OFFSET_SESS_SESSION_TEAM,
    OFFSET_SESS_WINS, OFFSET_SHARED_SVFLAGS, PERS_SCORE, SIZEOF_GENTITY, STAT_ARMOR, STAT_HEALTH,
    SVF_BOT, TEAM_FREE, TEAM_SPECTATOR,
};
use crate::state::{self, DuelPhase, InstanceMode};
use crate::syscall;

/// Maximum number of courts the ladder will run, matching the shared instance
/// slate (`crate::instances::MAX_MATCHES`).
pub const MAX_COURTS: usize = 16;

/// Which side of a court a client occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    A,
    B,
}

impl Side {
    /// The other side.
    pub fn other(self) -> Side {
        match self {
            Side::A => Side::B,
            Side::B => Side::A,
        }
    }
}

/// One court of a round. `a`/`b` are client numbers (the same slots the game
/// uses). `winner`/`loser` are filled when the court resolves; a void court
/// keeps both players for a requeue (instance rebuild failure) and is never
/// counted as a win or loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Court {
    pub a: Option<u8>,
    pub b: Option<u8>,
    pub winner: Option<u8>,
    pub loser: Option<u8>,
    pub resolved: bool,
    /// The court could not run (its instance could not be rebuilt): both
    /// players go to the back of the queue instead of playing.
    pub void: bool,
    /// The resolved pair has finished its void window and moved to spectator;
    /// the round may only end once every court is settled.
    pub settled: bool,
}

impl Court {
    /// A live court for two players.
    pub fn new(a: u8, b: u8) -> Court {
        Court {
            a: Some(a),
            b: Some(b),
            ..Court::default()
        }
    }

    /// Both players, in court order.
    pub fn players(&self) -> impl Iterator<Item = u8> + '_ {
        self.a.into_iter().chain(self.b)
    }

    /// Whether `client_num` is playing on this court.
    pub fn holds(&self, client_num: u8) -> bool {
        self.a == Some(client_num) || self.b == Some(client_num)
    }

    /// The opponent of `client_num` on this court.
    pub fn opponent_of(&self, client_num: u8) -> Option<u8> {
        if self.a == Some(client_num) {
            self.b
        } else if self.b == Some(client_num) {
            self.a
        } else {
            None
        }
    }

    /// Record the outcome and mark the court resolved.
    pub fn resolve(&mut self, winner: u8, loser: u8) {
        self.winner = Some(winner);
        self.loser = Some(loser);
        self.resolved = true;
        self.void = false;
    }

    /// Resolve without a winner (both players gone): the court is void and
    /// contributes nothing to the ladder.
    pub fn abandon(&mut self) {
        self.winner = None;
        self.loser = None;
        self.resolved = true;
        self.void = true;
    }

    /// The colour-stripped truth the bump needs: `(a, b, winner, loser)`.
    fn resolved_pair(&self) -> ResolvedCourt {
        ResolvedCourt {
            a: self.a,
            b: self.b,
            winner: if self.void { None } else { self.winner },
            loser: if self.void { None } else { self.loser },
            void: self.void,
        }
    }
}

/// The pure input to [`bump`]: one resolved court.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResolvedCourt {
    pub a: Option<u8>,
    pub b: Option<u8>,
    pub winner: Option<u8>,
    pub loser: Option<u8>,
    pub void: bool,
}

impl From<&Court> for ResolvedCourt {
    fn from(court: &Court) -> Self {
        court.resolved_pair()
    }
}

/// One pair of the next round, before it becomes a [`Court`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pair {
    pub a: Option<u8>,
    pub b: Option<u8>,
}

impl Pair {
    pub fn new(a: u8, b: u8) -> Pair {
        Pair {
            a: Some(a),
            b: Some(b),
        }
    }
}

/// A formation result: the courts of the round and the bench in queue order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Formation {
    pub courts: Vec<Pair>,
    pub queue: Vec<u8>,
}

/// Deterministic xorshift32, seeded from `level.time` plus a round counter.
/// Zero is remapped: an all-zero state would lock the generator.
#[derive(Debug, Clone, Copy)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Rng {
        Rng(if seed == 0 { 0x9e37_79b9 } else { seed })
    }

    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform-enough value in `0..bound` for a player shuffle.
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u32() as usize) % bound
    }
}

/// Fisher-Yates shuffle of the roster. Pure and deterministic for a seed, so
/// a round's formation is reproducible in the unit tests.
pub fn shuffle(roster: &[u8], seed: u32) -> Vec<u8> {
    let mut out = roster.to_vec();
    let mut rng = Rng::new(seed);
    for i in (1..out.len()).rev() {
        let j = rng.below(i + 1);
        out.swap(i, j);
    }
    out
}

/// Number of courts a roster of `willing` player can fill, given the map's
/// instance capacity (main slot included). Surplus players queue.
pub fn court_count(willing: usize, capacity: usize) -> usize {
    (willing / 2).min(capacity).min(MAX_COURTS)
}

/// Shuffle the roster into adjacent pairs, forming `court_count` courts; the
/// remainder is the initial bench in shuffled order.
pub fn form(roster: &[u8], capacity: usize, seed: u32) -> Formation {
    let shuffled = shuffle(roster, seed);
    let count = court_count(shuffled.len(), capacity);
    let mut courts = Vec::with_capacity(count);
    for chunk in shuffled[..count * 2].chunks_exact(2) {
        courts.push(Pair::new(chunk[0], chunk[1]));
    }
    Formation {
        courts,
        queue: shuffled[count * 2..].to_vec(),
    }
}

/// The ladder bump after every court resolved (D-025 §3.5). `courts` are in
/// ladder order (index 0 = court 1 = top). Every court must be resolved; a
/// void court's players are queued instead of playing (see [`requeue_voids`]).
///
/// - court 1 = `{W1, W2}`;
/// - court i = `{L(i-1), W(i+1)}`;
/// - bottom court = `{L(C-1), front of the queue}`, and `L(C)` goes to the back
///   of the queue; with an empty queue the bottom stays `{L(C-1), L(C)}`.
///
/// One court keeps the winner and takes the queue front when one exists (its
/// own loser takes the back); with no queue both players stay. This is the
/// plain bump for C = 1, not a second bench rule.
pub fn bump(courts: &[ResolvedCourt], queue: &[u8]) -> (Vec<Pair>, Vec<u8>) {
    let c = courts.len();
    let mut next = vec![Pair::default(); c];
    let mut bench: Vec<u8> = queue.to_vec();
    if c == 0 {
        return (next, bench);
    }
    let winner = |i: usize| courts[i].winner;
    let loser = |i: usize| courts[i].loser;

    if c == 1 {
        let w = winner(0);
        let l = loser(0);
        if bench.is_empty() {
            next[0] = Pair {
                a: w.or(l),
                b: if w.is_some() { l } else { None },
            };
        } else {
            let front = bench.remove(0);
            next[0] = Pair {
                a: w,
                b: Some(front),
            };
            if let Some(l) = l {
                bench.push(l);
            }
        }
        return (next, bench);
    }

    next[0] = Pair {
        a: winner(0),
        b: winner(1),
    };
    for (i, pair) in next.iter_mut().enumerate().take(c - 1).skip(1) {
        *pair = Pair {
            a: loser(i - 1),
            b: winner(i + 1),
        };
    }
    let last = c - 1;
    if bench.is_empty() {
        next[last] = Pair {
            a: loser(last - 1),
            b: loser(last),
        };
    } else {
        let front = bench.remove(0);
        next[last] = Pair {
            a: loser(last - 1),
            b: Some(front),
        };
        if let Some(l) = loser(last) {
            bench.push(l);
        }
    }
    (next, bench)
}

/// Append the players of void courts to the queue, preserving court order.
/// Used when a court could not run (its instance failed to rebuild): both
/// players wait their turn instead of a walkover being invented.
pub fn requeue_voids(courts: &[ResolvedCourt], queue: &[u8]) -> Vec<u8> {
    let mut out = queue.to_vec();
    for court in courts.iter().filter(|c| c.void) {
        out.extend(court.a);
        out.extend(court.b);
    }
    out
}

/// Remove `client_num` from a queue (a leaver), keeping the order.
pub fn queue_without(queue: &[u8], client_num: u8) -> Vec<u8> {
    queue.iter().copied().filter(|&c| c != client_num).collect()
}

/// Append a newcomer to the back of the queue (test-only helper).
#[cfg(test)]
pub fn queue_push(queue: &mut Vec<u8>, client_num: u8) {
    if !queue.contains(&client_num) {
        queue.push(client_num);
    }
}

/// Remove one occurrence from the front (the player the bump pulled in).
pub fn queue_pop_front(queue: &mut Vec<u8>) -> Option<u8> {
    if queue.is_empty() {
        None
    } else {
        Some(queue.remove(0))
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Which side wins a duel from the two `PERS_SCORE` values. `None` = still
/// running. `fraglimit <= 0` means no per-duel kill limit. If both reached the
/// limit in the same frame the higher score wins (a tie is impossible for
/// integer kills unless the limit is 0).
pub fn fraglimit_winner(a_score: i32, b_score: i32, fraglimit: i32) -> Option<Side> {
    if fraglimit <= 0 {
        return None;
    }
    match (a_score >= fraglimit, b_score >= fraglimit) {
        (true, false) => Some(Side::A),
        (false, true) => Some(Side::B),
        (true, true) => {
            if a_score >= b_score {
                Some(Side::A)
            } else {
                Some(Side::B)
            }
        }
        (false, false) => None,
    }
}

/// Per-round timelimit winner: total HP + shield, the game's own
/// `AdjustTournamentScores` rule. `None` is an exact tie; the caller breaks it
/// randomly.
pub fn health_winner(a_hp: i32, a_armor: i32, b_hp: i32, b_armor: i32) -> Option<Side> {
    let a = a_hp.max(0) + a_armor.max(0);
    let b = b_hp.max(0) + b_armor.max(0);
    match a.cmp(&b) {
        core::cmp::Ordering::Greater => Some(Side::A),
        core::cmp::Ordering::Less => Some(Side::B),
        core::cmp::Ordering::Equal => None,
    }
}

/// Break a timelimit tie deterministically from a random bit (the caller uses
/// the ladder RNG so the result is reproducible in tests).
pub fn break_tie(random_bit: bool) -> Side {
    if random_bit { Side::B } else { Side::A }
}

/// Walkover: the player still present wins, and the court reports the leaver
/// as the loser.
pub fn walkover(still_present: Side) -> (Side, Side) {
    (still_present, still_present.other())
}

// ---------------------------------------------------------------------------
// Standings and champion
// ---------------------------------------------------------------------------

/// Record one duel result in the ladder-long W/L table.
pub fn record_result(
    wins: &mut [i32; crate::sdk::MAX_CLIENTS],
    losses: &mut [i32; crate::sdk::MAX_CLIENTS],
    winner: u8,
    loser: u8,
) {
    let w = winner as usize;
    let l = loser as usize;
    if w < wins.len() && w != l {
        wins[w] += 1;
    }
    if l < losses.len() && w != l {
        losses[l] += 1;
    }
}

/// Clear the ladder-long standings for a fresh ladder.
pub fn reset_standings(
    wins: &mut [i32; crate::sdk::MAX_CLIENTS],
    losses: &mut [i32; crate::sdk::MAX_CLIENTS],
) {
    *wins = [0; crate::sdk::MAX_CLIENTS];
    *losses = [0; crate::sdk::MAX_CLIENTS];
}

/// The champion of the ladder: the roster member with the most wins once
/// someone reached `duel_fraglimit`. On a tie the client named by
/// `tie_pick % tied.len()` is crowned. `duel_fraglimit <= 0` is an endless
/// ladder and never crowns anybody.
pub fn champion(
    wins: &[i32; crate::sdk::MAX_CLIENTS],
    roster: &[u8],
    duel_fraglimit: i32,
    tie_pick: u32,
) -> Option<u8> {
    if duel_fraglimit <= 0 {
        return None;
    }
    let threshold = duel_fraglimit;
    let mut candidates: Vec<(u8, i32)> = roster
        .iter()
        .copied()
        .filter(|&c| wins.get(c as usize).copied().unwrap_or(0) >= threshold)
        .map(|c| (c, wins[c as usize]))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let best = candidates.iter().map(|(_, w)| *w).max().unwrap_or(0);
    candidates.retain(|(_, w)| *w == best);
    let pick = if candidates.len() == 1 {
        0
    } else {
        (tie_pick as usize) % candidates.len()
    };
    Some(candidates[pick].0)
}

/// Reconcile the bump's pairs with the live roster (D-025 §3.1): a player who
/// disconnected or opted out leaves; a newcomer joins the back of the queue;
/// a missing court slot is filled from the front of the queue; and the court
/// count is capped to the map's capacity (surplus pairs queue). Order is
/// preserved throughout, so the ladder stays stable.
pub fn reconcile(
    pairs: &[Pair],
    queue: &[u8],
    roster: &[u8],
    capacity: usize,
) -> (Vec<Pair>, Vec<u8>) {
    let in_roster = |c: u8| roster.contains(&c);
    let mut placed: Vec<u8> = Vec::new();
    // Keep the surviving side of each pair.
    let mut slots: Vec<[Option<u8>; 2]> = pairs
        .iter()
        .map(|p| {
            let mut s = [None, None];
            for (i, c) in [p.a, p.b].into_iter().enumerate() {
                if let Some(c) = c.filter(|c| in_roster(*c) && !placed.contains(c)) {
                    s[i] = Some(c);
                    placed.push(c);
                }
            }
            s
        })
        .collect();
    // Queue: roster members only, no duplicates, in order.
    let mut bench: Vec<u8> = Vec::new();
    for &c in queue {
        if in_roster(c) && !placed.contains(&c) && !bench.contains(&c) {
            bench.push(c);
        }
    }
    // Newcomers not placed and not queued join the bench in roster order, so
    // they can fill a court slot a leaver opened before anyone is dropped.
    for &c in roster {
        if !placed.contains(&c) && !bench.contains(&c) {
            bench.push(c);
        }
    }
    // Fill missing slots from the front of the queue.
    for slot in slots.iter_mut() {
        for side in slot.iter_mut() {
            if side.is_none() {
                if let Some(c) = queue_pop_front(&mut bench) {
                    *side = Some(c);
                    placed.push(c);
                }
            }
        }
    }
    // A pair with fewer than two players cannot run: its player queues.
    let mut kept: Vec<Pair> = Vec::new();
    for slot in slots {
        match slot {
            [Some(a), Some(b)] => kept.push(Pair::new(a, b)),
            [Some(a), None] | [None, Some(a)] => bench.push(a),
            [None, None] => {}
        }
    }
    // Cap the court count; surplus pairs queue in ladder order.
    let cap = capacity.min(MAX_COURTS);
    while kept.len() > cap {
        if let Some(pair) = kept.pop() {
            bench.push(pair.a.unwrap());
            bench.push(pair.b.unwrap());
        }
    }
    // Joins extend the ladder: promote pairs from the bench into new courts
    // (appended below the existing ones) while the map can host them and the
    // roster has a full pair for every court.
    while kept.len() < cap && kept.len() < roster.len() / 2 && bench.len() >= 2 {
        let a = queue_pop_front(&mut bench).unwrap();
        let b = queue_pop_front(&mut bench).unwrap();
        kept.push(Pair::new(a, b));
    }
    (kept, bench)
}

// ---------------------------------------------------------------------------
// Board / HUD order
// ---------------------------------------------------------------------------

/// One row of the round board: a court (1-based), its two players, or a bench
/// entry (`court == 0`). `winner`/`loser` are set after the court resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardRow {
    pub court: usize,
    pub client: u8,
    pub opponent: Option<u8>,
    pub queued: bool,
}

/// Board order: courts top to bottom, each pair in court order, then the
/// bench in queue order (D-025 §2.17). Pure so the scoreboard rewrite's order
/// is unit-tested.
pub fn board_rows(courts: &[Court], queue: &[u8]) -> Vec<BoardRow> {
    let mut rows = Vec::new();
    for (i, court) in courts.iter().enumerate() {
        if let Some(a) = court.a {
            rows.push(BoardRow {
                court: i + 1,
                client: a,
                opponent: court.b,
                queued: false,
            });
        }
        if let Some(b) = court.b {
            rows.push(BoardRow {
                court: i + 1,
                client: b,
                opponent: court.a,
                queued: false,
            });
        }
    }
    for &client in queue {
        rows.push(BoardRow {
            court: 0,
            client,
            opponent: None,
            queued: true,
        });
    }
    rows
}

/// The client order the `scores` message should present for a viewer: courts
/// top to bottom, both players of each court, then the bench. The caller
/// appends any connected client missing here (a spectator) at the end.
pub fn score_order(courts: &[Court], queue: &[u8]) -> Vec<u8> {
    board_rows(courts, queue)
        .into_iter()
        .map(|r| r.client)
        .collect()
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// The per-round announcement: one line per court plus the bench.
pub fn round_announcement(courts: &[Court], queue: &[u8], names: &[String; MAX_CLIENTS]) -> String {
    let mut out = String::from("Parallel duels - this round:");
    for (i, court) in courts.iter().enumerate() {
        match (court.a, court.b) {
            (Some(a), Some(b)) => {
                out.push_str(&format!(
                    "\n court {}: {} vs {}",
                    i + 1,
                    name_label(names, a),
                    name_label(names, b)
                ));
            }
            _ => {
                out.push_str(&format!("\n court {}: -", i + 1));
            }
        }
    }
    if queue.is_empty() {
        out.push_str("\n bench: empty.");
    } else {
        let bench: Vec<String> = queue.iter().map(|&c| name_label(names, c)).collect();
        out.push_str(&format!("\n bench: {}.", bench.join(", ")));
    }
    out
}

/// Champion announcement.
pub fn champion_announcement(champion: &str, wins: i32) -> String {
    format!("Ladder champion: {champion} with {wins} duel win(s) - a fresh ladder starts now.")
}

// ---------------------------------------------------------------------------
// Lifecycle wiring (engine/game memory; no pure rules live below)
// ---------------------------------------------------------------------------

/// How long the stock intermission board stays up before the proxy issues its
/// own `map_restart` (D-025: the stock 4 s window).
pub const BOARD_MS: i32 = 4000;
/// Void-damage window after a duel is decided, mirroring the pristine
/// `LogExit` → `BeginIntermission` delay.
pub const RESOLVE_VOID_MS: i32 = 1000;
/// Base JA's `timelimit` cvar is in **minutes**; the round clock compares it
/// against `level.time` in milliseconds.
const MS_PER_MINUTE: i32 = 60_000;
/// Court players spawned per frame after a restart burst (D-006.12 deferral).
const SPAWN_PER_FRAME: usize = 4;

/// Whether the parallel-duel feature owns the game right now.
pub fn mode_on() -> bool {
    state::instance_mode() == InstanceMode::Duel
}

/// Whether a ladder is configured and running (possibly waiting for players).
pub fn ladder_on() -> bool {
    state::with_state(|s| s.duel.ladder) && mode_on()
}

fn g_entities() -> usize {
    state::with_state(|s| s.located_game_data.g_entities)
}

fn level_time() -> i32 {
    let level = original::level_address();
    if level == 0 {
        return 0;
    }
    // SAFETY: `level` is the loaded game module's `level_locals_t`.
    unsafe { read_i32(level + OFFSET_LEVEL_TIME) }
}

fn svs_time() -> i32 {
    state::with_state(|s| s.svs_time)
}

fn client_of(client_num: c_int) -> usize {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) || g_entities() == 0 {
        return 0;
    }
    let ent = g_entities() + client_num as usize * SIZEOF_GENTITY;
    // SAFETY: ent is inside the g_entities array; client is a pointer field.
    unsafe { crate::hooks::engine_sv::read_usize(ent + OFFSET_GENTITY_CLIENT) }
}

fn ps_int(client_num: c_int, offset: usize) -> i32 {
    let client = client_of(client_num);
    if client == 0 {
        return 0;
    }
    // SAFETY: client came from a live gentity's `client` pointer.
    unsafe { read_i32(client + OFFSET_GCLIENT_PS + offset) }
}

/// `client->ps.persistant[PERS_SCORE]` (the per-duel kill count).
fn pers_score(client_num: c_int) -> i32 {
    ps_int(client_num, OFFSET_PS_PERSISTANT + PERS_SCORE * 4)
}

fn stat(client_num: c_int, index: usize) -> i32 {
    ps_int(client_num, OFFSET_PS_STATS + index * 4)
}

fn session_team(client_num: c_int) -> Option<i32> {
    let client = client_of(client_num);
    if client == 0 {
        return None;
    }
    // SAFETY: client is a live gclient_t; the field is an int.
    Some(unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) })
}

fn connected(client_num: c_int) -> bool {
    (0..MAX_CLIENTS as c_int).contains(&client_num)
        && state::with_state(|s| s.clients[client_num as usize].is_connected)
}

fn is_bot(client_num: c_int) -> bool {
    (0..MAX_CLIENTS as c_int).contains(&client_num)
        && state::with_state(|s| s.clients[client_num as usize].is_bot)
}

/// Display name of a client: the name the game last put in `CS_PLAYERS`
/// (colour codes kept), else the live `pers.netname` cleaned, else `#slot`.
fn client_name(client_num: u8) -> String {
    let slot = client_num as usize;
    if slot < MAX_CLIENTS {
        let from_cs =
            state::with_state(|s| crate::instances::info_value(&s.tffa.cs_players_base[slot], "n"));
        if let Some(name) = from_cs.filter(|n| !n.is_empty()) {
            return name;
        }
    }
    let mut buf = [0u8; crate::sdk::MAX_NETNAME];
    crate::hooks::game::client_clean_netname(client_num as c_int, &mut buf);
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..len]).trim().to_owned();
    if name.is_empty() {
        format!("#{client_num}")
    } else {
        name
    }
}

fn client_names() -> [String; MAX_CLIENTS] {
    core::array::from_fn(|i| client_name(i as u8))
}

fn name_label(names: &[String; MAX_CLIENTS], client_num: u8) -> String {
    let name = names
        .get(client_num as usize)
        .filter(|n| !n.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("#{client_num}"));
    // The name may carry its own `^n` colour codes; reset to white before the
    // rest of the sentence so one player's colour does not leak into it.
    format!("{}^7", name.trim_end_matches('^'))
}

fn cvar(name: &CStr) -> i32 {
    // SAFETY: engine syscall registered (GAME_INIT ran before any frame).
    unsafe { syscall::cvar_variable_integer_value(name) }
}

fn fraglimit() -> i32 {
    cvar(c"fraglimit")
}

fn timelimit() -> i32 {
    cvar(c"timelimit")
}

fn duel_fraglimit() -> i32 {
    cvar(c"duel_fraglimit")
}

/// In-game `print` payload: quote-safe and single-byte. The 2003 client parses
/// the command as bytes and renders latin-1, so non-ASCII (an em dash, a
/// curly quote) would arrive as mojibake; the string ends the command with a
/// real newline, as every stock `print` does.
fn message_safe(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '"' => '\'',
            c if c.is_ascii() => c,
            _ => '?',
        })
        .collect()
}

fn tell(client_num: c_int, text: &str) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let safe = message_safe(text);
    let Ok(msg) = std::ffi::CString::new(format!("print \"{safe}\n\"")) else {
        return;
    };
    // SAFETY: engine syscall registered; msg NUL-terminated.
    unsafe { syscall::send_server_command(client_num, &msg) };
}

fn announce(text: &str) {
    let safe = message_safe(text);
    let Ok(msg) = std::ffi::CString::new(format!("print \"{safe}\n\"")) else {
        return;
    };
    // SAFETY: engine syscall registered; msg NUL-terminated.
    unsafe { syscall::send_server_command(-1, &msg) };
}

/// Mirror the bots the game adds itself. `BotConnect` calls `ClientConnect`
/// and `ClientBegin` *inside* the game module (`g_bot.c:954`, `:1020`), so a
/// bot never passes through the engine's `GAME_CLIENT_CONNECT` /
/// `GAME_CLIENT_BEGIN` vmMain events. Detect them from the game's own
/// `r.svFlags & SVF_BOT` (the engine's bot discriminator) and their
/// `pers.connected`, and clear the mirror when a bot is removed.
fn adopt_game_bots() {
    let g = g_entities();
    if g == 0 {
        return;
    }
    let mut present = [false; MAX_CLIENTS];
    for (i, slot) in present.iter_mut().enumerate() {
        let ent = g + i * SIZEOF_GENTITY;
        // SAFETY: ent is inside the g_entities array.
        let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
        if client == 0 {
            continue;
        }
        // SAFETY: the engine's own bot discriminator lives at r.svFlags.
        let svflags = unsafe { read_i32(ent + OFFSET_SHARED_SVFLAGS) };
        if svflags & SVF_BOT as i32 == 0 {
            continue;
        }
        // SAFETY: client is a live gclient_t.
        let state_ = unsafe { read_i32(client + OFFSET_GCLIENT_PERS + OFFSET_PERS_CONNECTED) };
        if state_ == CON_CONNECTED {
            *slot = true;
        }
    }
    state::with_state(|s| {
        // During the `map_restart` settle frames the game's entity array is
        // not repopulated yet, so an absent bot is not a leaver: its carried
        // court spawn must survive until it reconnects.
        let preserve = s.tffa.restart_burst
            || (s.duel.phase == DuelPhase::Playing && !s.duel.round_clock_started);
        for (i, &is_present) in present.iter().enumerate() {
            if is_present {
                s.clients[i].is_bot = true;
                s.clients[i].is_connected = true;
                s.duel.willing[i] = true;
            } else if s.clients[i].is_bot {
                // Bot removed by the game (bot_minplayers, `kick`): drop it.
                s.clients[i].is_connected = false;
                s.duel.willing[i] = false;
                if !preserve {
                    s.duel.spawn_pending.retain(|&c| c as usize != i);
                    s.duel.queue.retain(|&c| c as usize != i);
                }
            }
        }
    });
}

/// The live roster: connected clients that are willing (bots always are).
fn roster() -> Vec<u8> {
    state::with_state(|s| {
        (0..MAX_CLIENTS)
            .filter(|&i| s.clients[i].is_connected && (s.duel.willing[i] || s.clients[i].is_bot))
            .map(|i| i as u8)
            .collect()
    })
}

fn roster_contains(client_num: u8) -> bool {
    roster().contains(&client_num)
}

fn court_of(client_num: u8) -> Option<usize> {
    state::with_state(|s| {
        s.duel
            .courts
            .iter()
            .take(s.duel.court_count)
            .position(|c| c.holds(client_num))
    })
}

/// Seed willingness for every connected client (cvar turned on mid-map, so
/// the connects happened while the feature was off).
fn seed_all_willing() {
    state::with_state(|s| {
        for i in 0..MAX_CLIENTS {
            if s.clients[i].is_connected {
                s.duel.willing[i] = true;
            }
        }
    });
}

/// `GAME_CLIENT_CONNECT` bookkeeping: seed this client's willingness. A fresh
/// connect always reseeds; a reconnect on a carried ladder keeps the flag the
/// round was formed with (the userinfo `team` key is rewritten by the game's
/// own duel join, so it is not authoritative mid-ladder).
pub fn on_client_connect(client_num: c_int, first_time: bool) {
    if !mode_on() || !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let n = client_num as usize;
    let bot = is_bot(client_num);
    // A normal connect is base JA's "waiting in line" case (no `team s`
    // marker): the client asked to play, so it is willing. Only the ladder's
    // own spectator move (a court player leaving) clears the flag.
    let willing = true;
    state::with_state(|s| {
        // A carried ladder keeps each reconnect's carried flag; a fresh
        // ladder (first start, map change, admin restart, or a gametype switch
        // whose clients reconnect after the restart) reseeds everybody.
        if should_seed_on_connect(first_time, s.duel.ladder, s.duel.fresh_restart) {
            s.duel.willing[n] = willing;
            s.duel.spawned[n] = false;
            s.tffa.client_match[n] = crate::instances::MAIN_MATCH;
        }
        // Bots are added by the game (`BotConnect` → `ClientBegin` directly),
        // so they never pass through the engine's `GAME_CLIENT_BEGIN`. Mark
        // them connected here: the roster, scoreboard and chat gates all use
        // this mirror, and a disconnected bot is cleared by ClientDisconnect.
        if bot {
            s.clients[n].is_connected = true;
        }
        s.duel.spawn_pending.retain(|&c| c as usize != n);
    });
}

/// Whether a connect seeds willingness. A first connect, an idle/fresh ladder
/// and a fresh-restart carry do; a reconnect into a running carried round
/// keeps the flag it was drafted with.
fn should_seed_on_connect(first_time: bool, ladder: bool, fresh_restart: bool) -> bool {
    first_time || !ladder || fresh_restart
}

/// `GAME_CLIENT_DISCONNECT` bookkeeping: a leaver loses their court by
/// walkover (checked in `on_run_frame_post`) and leaves the queue.
pub fn on_client_disconnect(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let n = client_num as usize;
    let id = client_num as u8;
    state::with_state(|s| {
        s.duel.queue = queue_without(&s.duel.queue, id);
        s.duel.spawn_pending.retain(|&c| c != id);
        s.duel.willing[n] = false;
        s.duel.spawned[n] = false;
    });
    // A disconnect inside a running court is a walkover for the opponent; the
    // per-frame resolver applies it so a reconnect burst cannot double-count.
    resolve_walkover(id);
}

/// `GAME_CLIENT_BEGIN`: after the pristine begin, queue the client's court
/// spawn (deferred past the restart burst), or make sure a non-court client
/// stays a spectator.
pub fn on_client_begin(client_num: c_int) {
    if !mode_on() || !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let id = client_num as u8;
    let (playing, in_court) = state::with_state(|s| {
        (
            s.duel.phase == DuelPhase::Playing && s.duel.ladder,
            court_of_locked(s, id).is_some(),
        )
    });
    if playing && in_court {
        queue_spawn(id);
        return;
    }
    // Not a court player: the base game's GT_DUEL cap may have begun one or
    // two of them; keep everyone else out of the arena.
    if session_team(client_num) != Some(TEAM_SPECTATOR) && client_of(client_num) != 0 {
        crate::tffa::set_client_spectator(client_num);
    }
}

fn court_of_locked(s: &state::ProxyState, client_num: u8) -> Option<usize> {
    s.duel.courts[..s.duel.court_count]
        .iter()
        .position(|c| c.holds(client_num))
}

fn queue_spawn(client_num: u8) {
    state::with_state(|s| {
        if !s.duel.spawn_pending.contains(&client_num) {
            s.duel.spawn_pending.push(client_num);
        }
    });
}

/// Spawn up to [`SPAWN_PER_FRAME`] queued court players through the game's own
/// `SetTeamQuick(ent, TEAM_FREE, qtrue)` (see `crate::jampgame` for why not
/// `SetTeam("f")` on the shipped binary).
fn drain_spawns() {
    if original::base() == 0 {
        return;
    }
    let burst = state::with_state(|s| s.tffa.restart_burst);
    if burst {
        return;
    }
    for _ in 0..SPAWN_PER_FRAME {
        let next = state::with_state(|s| {
            if s.duel.spawn_pending.is_empty() {
                None
            } else {
                Some(s.duel.spawn_pending.remove(0))
            }
        });
        let Some(id) = next else { break };
        let cn = id as c_int;
        if !connected(cn) || client_of(cn) == 0 || !roster_contains(id) {
            continue;
        }
        if session_team(cn) == Some(TEAM_FREE) && court_of(id).is_some() {
            // Already in the arena (a reconnect that carried the team).
            state::with_state(|s| s.duel.spawned[id as usize] = true);
            continue;
        }
        // Fresh duel: zero the per-duel score before the first body exists.
        crate::instances::reset_game_scores(cn);
        let ent = g_entities() + cn as usize * SIZEOF_GENTITY;
        // SAFETY: ent is a live client gentity; the game is not iterating
        // g_entities at this point (between frames / after GAME_CLIENT_BEGIN).
        unsafe { crate::jampgame::set_team_quick(ent, TEAM_FREE, 1) };
        state::with_state(|s| s.duel.spawned[id as usize] = true);
    }
}

/// Move a resolved court's players back to spectator (the stock duel cycle)
/// without touching their willingness.
fn spectator_court(court_index: usize) {
    let players: Vec<c_int> = state::with_state(|s| {
        s.duel
            .courts
            .get(court_index)
            .map(|c| c.players().map(|p| p as c_int).collect())
            .unwrap_or_default()
    });
    for cn in players {
        if connected(cn) && session_team(cn) != Some(TEAM_SPECTATOR) {
            crate::tffa::set_client_spectator(cn);
        }
        if (0..MAX_CLIENTS as c_int).contains(&cn) {
            state::with_state(|s| s.duel.spawned[cn as usize] = false);
        }
    }
}

/// Walkover for a court whose player left (disconnect or voluntary spectator).
fn resolve_walkover(leaver: u8) {
    let now = svs_time();
    state::with_state(|s| {
        if s.duel.phase != DuelPhase::Playing || !s.duel.ladder {
            return;
        }
        let Some(i) = s.duel.courts[..s.duel.court_count]
            .iter()
            .position(|c| c.holds(leaver))
        else {
            return;
        };
        let court = s.duel.courts[i];
        if court.resolved || court.void {
            return;
        }
        let leaver_side = if court.a == Some(leaver) {
            Side::A
        } else {
            Side::B
        };
        let (winner_side, _loser_side) = walkover(leaver_side.other());
        let (winner, loser) = if winner_side == Side::A {
            (court.a, court.b)
        } else {
            (court.b, court.a)
        };
        if let (Some(winner), Some(loser)) = (winner, loser) {
            s.duel.courts[i].resolve(winner, loser);
            s.duel.resolved_at[i] = now;
        }
    });
}

/// Configstring identity prefix for a court (`(Duel N) `); the shared core's
/// `rewrite_cs_players_for_viewer` uses the same prefix helper.
pub fn court_prefix(court: usize) -> String {
    format!("(Duel {court}) ")
}

/// The `GAME_INIT` post-pass (after the hook layer and the world scan): restore
/// the carried round's courts and instance worlds, or leave the ladder idle for
/// a fresh formation once the clients reconnect.
pub fn on_game_init(restart: bool) {
    if !mode_on() {
        return;
    }
    let carry = state::with_state(|s| {
        s.duel.ladder && restart && s.duel.carry_round && !s.duel.pending_courts.is_empty()
    });
    eprintln!("----- proxy-rs: parallel duels GAME_INIT restart={restart} carry={carry}");
    if carry {
        install_carried_courts();
    } else {
        state::with_state(|s| {
            if s.duel.ladder {
                s.duel.phase = DuelPhase::Idle;
                s.duel.carry_round = false;
                s.duel.pending_courts.clear();
                s.duel.pending_queue.clear();
            }
        });
    }
}

fn install_carried_courts() {
    let (pairs, queue) = state::with_state(|s| {
        (
            core::mem::take(&mut s.duel.pending_courts),
            core::mem::take(&mut s.duel.pending_queue),
        )
    });
    let installed_at = svs_time();
    let mut bench = queue;
    let mut courts: Vec<Court> = Vec::with_capacity(pairs.len());
    for pair in pairs.iter() {
        // Realize the instance world before the clients reconnect; court 1 is
        // the pristine main slot (no clone). The instance id is the compacted
        // court index, so a failed court never desynchronises the two.
        let instance = courts.len() as u8;
        if instance != crate::instances::MAIN_MATCH
            && crate::instances::realize_instance(instance).is_err()
        {
            // All-or-nothing: void the court, both players queue.
            bench.extend([pair.a, pair.b].into_iter().flatten());
            continue;
        }
        state::with_state(|s| {
            for player in [pair.a, pair.b].into_iter().flatten() {
                s.tffa.client_match[player as usize] = instance;
                s.duel.spawned[player as usize] = false;
            }
        });
        if let (Some(a), Some(b)) = (pair.a, pair.b) {
            courts.push(Court::new(a, b));
        }
    }
    state::with_state(|s| {
        s.duel.courts = [Court::default(); MAX_COURTS];
        for (i, court) in courts.iter().enumerate() {
            s.duel.courts[i] = *court;
        }
        s.duel.court_count = courts.len();
        s.duel.queue = bench;
        s.duel.phase = DuelPhase::Playing;
        s.duel.restart_pending = false;
        s.duel.carry_round = false;
        s.duel.round_clock_started = false;
        s.duel.round_start = 0;
        s.duel.carried_at = installed_at;
        s.duel.resolved_at = [0; MAX_COURTS];
        s.duel.spawn_pending.clear();
        for court in s.duel.courts.iter().take(courts.len()) {
            for player in court.players() {
                s.duel.spawn_pending.push(player);
            }
        }
    });
}

/// One instance capacity: the main slot plus the clone sets the map can host,
/// capped by the shared slate and `MAX_COURTS`.
fn instance_capacity() -> usize {
    let (g_entities, scanned) =
        state::with_state(|s| (s.located_game_data.g_entities, s.tffa.world_scanned));
    if !scanned {
        crate::instances::scan_world_entities();
    }
    let (cost, cloned) = state::with_state(|s| (s.tffa.world_clone_cost, s.tffa.world_cloned));
    // The free-slot count excludes the clone sets already live for this
    // round; add their cost back so capacity is the map's total, not what is
    // left after the current courts (otherwise the ladder could never grow).
    let live_cost = cost.saturating_mul(cloned.count_ones() as usize);
    let total_free = crate::instances::free_world_slots(g_entities).saturating_add(live_cost);
    let clones = if cost == 0 {
        crate::instances::MAX_MATCHES
    } else {
        total_free / cost
    };
    (1 + clones.min(crate::instances::MAX_MATCHES)).min(MAX_COURTS)
}

/// Form and start a round from the live roster (fresh ladder, admin restart or
/// the first round after the cvar was enabled).
fn start_round() {
    let roster = roster();
    if roster.len() < 2 {
        return;
    }
    let capacity = instance_capacity().max(1);
    let (serial, seed) = state::with_state(|s| {
        let serial = s.duel.round_serial;
        let seed = (level_time() as u32)
            .wrapping_mul(2_654_435_761)
            .wrapping_add(serial.wrapping_mul(0x9e37_79b9));
        (serial, seed)
    });
    let formation = form(&roster, capacity, seed);
    if formation.courts.is_empty() {
        return;
    }
    let mut bench = formation.queue;
    let mut courts: Vec<Court> = Vec::with_capacity(formation.courts.len());
    for pair in formation.courts.iter() {
        // The instance id is the compacted court index: a court whose world
        // failed must not leave later courts realized under a different id.
        let instance = courts.len() as u8;
        if instance != crate::instances::MAIN_MATCH
            && crate::instances::realize_instance(instance).is_err()
        {
            bench.extend([pair.a, pair.b].into_iter().flatten());
            continue;
        }
        state::with_state(|s| {
            for player in [pair.a, pair.b].into_iter().flatten() {
                s.tffa.client_match[player as usize] = instance;
                s.duel.spawned[player as usize] = false;
            }
        });
        if let (Some(a), Some(b)) = (pair.a, pair.b) {
            courts.push(Court::new(a, b));
        }
    }
    if courts.is_empty() {
        return;
    }
    let now = level_time();
    state::with_state(|s| {
        // A fresh ladder clears the standings and the champion; a carried
        // ladder keeps them.
        if s.duel.fresh_restart {
            // A fresh shuffle (first ladder, champion, or an admin restart)
            // starts with clean standings.
            reset_standings(&mut s.duel.wins, &mut s.duel.losses);
            s.duel.champion = None;
            s.duel.queue.clear();
        }
        s.duel.fresh_restart = false;
        s.duel.ladder = true;
        s.duel.courts = [Court::default(); MAX_COURTS];
        for (i, court) in courts.iter().enumerate() {
            s.duel.courts[i] = *court;
        }
        s.duel.court_count = courts.len();
        s.duel.queue = bench;
        s.duel.phase = DuelPhase::Playing;
        s.duel.round_serial = serial.wrapping_add(1);
        s.duel.seed = seed;
        s.duel.round_start = now;
        s.duel.round_clock_started = true;
        s.duel.resolved_at = [0; MAX_COURTS];
        s.duel.restart_pending = false;
        s.duel.carry_round = false;
        s.duel.spawn_pending.clear();
        for court in s.duel.courts.iter().take(courts.len()) {
            for player in court.players() {
                s.duel.spawn_pending.push(player);
            }
        }
    });
    let (courts, queue, serial) = state::with_state(|s| {
        (
            s.duel.courts[..s.duel.court_count].to_vec(),
            s.duel.queue.clone(),
            s.duel.round_serial,
        )
    });
    eprintln!(
        "----- proxy-rs: parallel duels round {} - {} court(s), {} queued (timelimit {} min)",
        serial,
        courts.len(),
        queue.len(),
        timelimit()
    );
    let names = client_names();
    announce(&round_announcement(&courts, &queue, &names));
    refresh_all_cs_players();
    push_hud();
}

/// Enabling the ladder mid-duel takes over immediately: the stock duel's
/// current players are willing (they were playing) and become spectators, and
/// the ladder forms its first round from the roster.
fn take_over_stock_duel() {
    let connected: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|&cn| s.clients[cn as usize].is_connected)
            .collect()
    });
    let players: Vec<c_int> = connected
        .into_iter()
        .filter(|&cn| session_team(cn) != Some(TEAM_SPECTATOR))
        .collect();
    for cn in &players {
        state::with_state(|s| s.duel.willing[*cn as usize] = true);
    }
    for cn in players {
        crate::tffa::set_client_spectator(cn);
    }
}

/// Keep every non-court client out of the arena: the base game can restore
/// up to two `TEAM_FREE` players from session data across a restart (and a
/// bot's own connect path joins before the ladder drafts it), which would let
/// an undrafted player fight court 1. Court players are left alone.
fn park_non_court_players() {
    if !ladder_on() {
        return;
    }
    let non_court: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|&cn| {
                s.clients[cn as usize].is_connected && court_of_locked(s, cn as u8).is_none()
            })
            .collect()
    });
    for cn in non_court {
        if session_team(cn) != Some(TEAM_SPECTATOR) {
            crate::tffa::set_client_spectator(cn);
        }
    }
}

/// Whether a carried round can start its clock: every connected court player
/// has its body (`spawned`), and at least one court has a connected player.
/// A disconnected player is treated as ready (the court resolves as a
/// walkover); this keeps the full round timelimit for the players who did
/// reconnect instead of starting it at the first reconnect.
fn round_can_start() -> bool {
    state::with_state(|s| {
        let mut any_connected = false;
        for court in s.duel.courts.iter().take(s.duel.court_count) {
            for player in court.players() {
                if s.clients[player as usize].is_connected {
                    any_connected = true;
                    if !s.duel.spawned[player as usize] {
                        return false;
                    }
                }
            }
        }
        any_connected
    })
}

/// `GAME_RUN_FRAME` pre-pass.
pub fn on_frame_start() {
    if !mode_on() {
        if state::with_state(|s| s.duel.ladder || s.duel.phase != DuelPhase::Idle) {
            teardown();
        }
        // A gametype change (or a master-switch change) re-arms the cvar edge,
        // so returning to GT_DUEL with the cvar still on starts a fresh ladder.
        state::with_state(|s| s.duel.enable_latched = false);
        return;
    }
    let now_on = state::with_state(|s| s.cvars[state::CVAR_PARALLEL_DUELS_ENABLE].integer != 0);
    let (was_on, turned_on) = state::with_state(|s| {
        let was = s.duel.enable_latched;
        s.duel.enable_latched = now_on;
        (was, now_on && !was)
    });
    if !now_on {
        if was_on || state::with_state(|s| s.duel.ladder) {
            teardown();
        }
        return;
    }
    if turned_on {
        seed_all_willing();
        take_over_stock_duel();
        state::with_state(|s| {
            s.duel.ladder = true;
            s.duel.phase = DuelPhase::Idle;
            s.duel.fresh_restart = true;
            s.duel.courts = [Court::default(); MAX_COURTS];
            s.duel.court_count = 0;
            s.duel.queue.clear();
            s.tffa.client_match = [crate::instances::MAIN_MATCH; MAX_CLIENTS];
        });
        let roster = roster();
        let names = client_names();
        let players: Vec<String> = roster.iter().map(|&c| name_label(&names, c)).collect();
        if players.len() >= 2 {
            announce(&format!(
                "Parallel duels enabled - {} willing player(s): {}.",
                players.len(),
                players.join(", ")
            ));
        } else {
            announce("Parallel duels enabled - waiting for at least two willing players.");
        }
    }
    if !state::with_state(|s| s.duel.ladder) {
        return;
    }
    adopt_game_bots();
    let (phase, pending, started) = state::with_state(|s| {
        (
            s.duel.phase,
            s.duel.restart_pending,
            s.duel.round_clock_started,
        )
    });
    if phase == DuelPhase::Playing && !started {
        if round_can_start() {
            let now = level_time();
            let tl = timelimit();
            eprintln!(
                "----- proxy-rs: parallel duels round clock start level.time={now} timelimit={tl} min"
            );
            state::with_state(|s| {
                s.duel.round_start = now;
                s.duel.round_clock_started = true;
                s.duel.carried_at = 0;
            });
            // Announce once the clients are back: at `GAME_INIT` the game
            // slots are still empty, so names would fall back to slot numbers.
            let names = client_names();
            announce(&state::with_state(|s| {
                round_announcement(&s.duel.courts[..s.duel.court_count], &s.duel.queue, &names)
            }));
            refresh_all_cs_players();
            push_hud();
        } else {
            // A carried round nobody reconnected into (every client left while
            // the board was up): fall back to a fresh formation instead of
            // stalling on a court that can never spawn.
            let carried_at = state::with_state(|s| s.duel.carried_at);
            let now = svs_time();
            if carried_at != 0 && now.wrapping_sub(carried_at) > 10_000 {
                state::with_state(|s| {
                    s.duel.courts = [Court::default(); MAX_COURTS];
                    s.duel.court_count = 0;
                    s.duel.queue.clear();
                    s.duel.spawn_pending.clear();
                    s.duel.pending_courts.clear();
                    s.duel.pending_queue.clear();
                    s.duel.phase = DuelPhase::Idle;
                    s.duel.fresh_restart = true;
                    s.duel.carried_at = 0;
                    s.duel.restart_pending = false;
                    s.tffa.client_match = [crate::instances::MAIN_MATCH; MAX_CLIENTS];
                });
                announce("Parallel duels: carried round abandoned - forming a fresh ladder.");
            }
        }
    }
    if phase == DuelPhase::Idle && !pending {
        if roster().len() >= 2 {
            start_round();
        } else {
            let now = svs_time();
            let due = state::with_state(|s| now.wrapping_sub(s.duel.last_roster_check) >= 5000);
            if due {
                state::with_state(|s| s.duel.last_roster_check = now);
                announce("Parallel duels: waiting for at least two willing players.");
            }
        }
    }
    if phase == DuelPhase::Board {
        let now = svs_time();
        let due = state::with_state(|s| now >= s.duel.board_until);
        if due {
            issue_restart();
        }
    }
    // Spawn after any new round was formed above, so the first game frame
    // already sees the court's bodies (and the resolver cannot mistake a
    // just-formed court for a double walkover).
    drain_spawns();
    park_non_court_players();
}

fn issue_restart() {
    // The proxy calls the engine syscall directly, so its own `map_restart`
    // never passes through the game's `G_SEND_CONSOLE_COMMAND` trap (we are
    // that trap). The round carry is therefore recorded here, at issue time;
    // `on_console_command` still classifies any *game-issued* `map_restart`
    // (a vote) as an admin restart, and the armed-token form it also supports
    // is pinned by `classify_restart`'s tests.
    state::with_state(|s| {
        s.duel.restart_pending = true;
        s.duel.carry_round = true;
        s.duel.restart_armed = false;
        s.duel.board_until = 0;
    });
    // SAFETY: engine syscall registered; the command is a static literal.
    unsafe { syscall::send_console_command(EXEC_APPEND, c"map_restart 0") };
}

/// `GAME_RUN_FRAME` post-pass: resolve courts, end the round, run the board.
pub fn on_run_frame_post() {
    if !mode_on() || !state::with_state(|s| s.duel.ladder) {
        return;
    }
    let (phase, count, started) =
        state::with_state(|s| (s.duel.phase, s.duel.court_count, s.duel.round_clock_started));
    if phase != DuelPhase::Playing || !started {
        // A carried round starts its clock only once a court player has
        // reconnected (`on_frame_start`); resolving before that would read
        // every preserved court as a double walkover.
        return;
    }
    let now = svs_time();
    let fl = fraglimit();
    let tl = timelimit();
    let round_start = state::with_state(|s| s.duel.round_start);

    // Per-round timelimit: every unresolved court decides on HP + shield now.
    let unresolved = state::with_state(|s| {
        s.duel.courts[..count]
            .iter()
            .any(|c| !c.resolved && !c.void)
    });
    if tl > 0
        && unresolved
        && level_time().wrapping_sub(round_start) >= tl.saturating_mul(MS_PER_MINUTE)
    {
        eprintln!(
            "----- proxy-rs: parallel duels round timelimit expired elapsed={}ms limit={}ms",
            level_time().wrapping_sub(round_start),
            tl.saturating_mul(MS_PER_MINUTE)
        );
        announce("Time limit reached - this round ends.");
        let mut pick = state::with_state(|s| s.duel.seed).wrapping_add(now as u32);
        for i in 0..count {
            let court = state::with_state(|s| s.duel.courts[i]);
            if court.resolved || court.void {
                continue;
            }
            let Some(a) = court.a else { continue };
            let Some(b) = court.b else { continue };
            let side = health_winner(
                stat(a as c_int, STAT_HEALTH),
                stat(a as c_int, STAT_ARMOR),
                stat(b as c_int, STAT_HEALTH),
                stat(b as c_int, STAT_ARMOR),
            );
            let winner_side = side.unwrap_or_else(|| {
                pick = pick.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                break_tie(pick & 1 == 1)
            });
            let (winner, loser) = match winner_side {
                Side::A => (a, b),
                Side::B => (b, a),
            };
            state::with_state(|s| {
                s.duel.courts[i].resolve(winner, loser);
                s.duel.resolved_at[i] = now;
            });
        }
    }

    for i in 0..count {
        let court = state::with_state(|s| s.duel.courts[i]);
        if court.void {
            continue;
        }
        if !court.resolved {
            let Some(a) = court.a else { continue };
            let Some(b) = court.b else { continue };
            let (a_ready, b_ready) =
                state::with_state(|s| (s.duel.spawned[a as usize], s.duel.spawned[b as usize]));
            // Give a just-formed court its spawn frame before judging a
            // walkover; a player that cannot spawn is still caught by the
            // round timelimit (and the 10 s grace avoids a stuck court when
            // no timelimit is set).
            if (!a_ready || !b_ready)
                && level_time().wrapping_sub(state::with_state(|s| s.duel.round_start)) < 10_000
            {
                continue;
            }
            let a_gone = !connected(a as c_int)
                || session_team(a as c_int) == Some(TEAM_SPECTATOR)
                || !roster_contains(a);
            let b_gone = !connected(b as c_int)
                || session_team(b as c_int) == Some(TEAM_SPECTATOR)
                || !roster_contains(b);
            let outcome = if a_gone && b_gone {
                None
            } else if a_gone {
                Some((b, a))
            } else if b_gone {
                Some((a, b))
            } else {
                fraglimit_winner(pers_score(a as c_int), pers_score(b as c_int), fl).map(|side| {
                    match side {
                        Side::A => (a, b),
                        Side::B => (b, a),
                    }
                })
            };
            match outcome {
                Some((winner, loser)) => state::with_state(|s| {
                    s.duel.courts[i].resolve(winner, loser);
                    s.duel.resolved_at[i] = now;
                }),
                None if a_gone && b_gone => state::with_state(|s| {
                    s.duel.courts[i].abandon();
                    s.duel.resolved_at[i] = now;
                }),
                None => {}
            }
        }
        // One-second void window, then the pair watches the remaining courts.
        let (resolved, at, void) = state::with_state(|s| {
            (
                s.duel.courts[i].resolved,
                s.duel.resolved_at[i],
                s.duel.courts[i].void,
            )
        });
        if resolved && !void && now.wrapping_sub(at) >= RESOLVE_VOID_MS {
            let done = state::with_state(|s| s.duel.courts[i].winner.is_some());
            if done {
                spectator_court(i);
                // Mark the window handled so this runs once.
                state::with_state(|s| {
                    s.duel.courts[i].settled = true;
                    s.duel.resolved_at[i] = now.wrapping_sub(i32::MAX / 2);
                });
            }
        }
    }

    push_hud();
    if round_complete() {
        end_round();
    }
}

fn round_complete() -> bool {
    state::with_state(|s| {
        (0..s.duel.court_count).all(|i| {
            let c = s.duel.courts[i];
            c.resolved && (c.void || (c.winner.is_some() && c.settled))
        })
    })
}

fn end_round() {
    let (count, mut queue) = state::with_state(|s| (s.duel.court_count, s.duel.queue.clone()));
    let resolved: Vec<ResolvedCourt> = state::with_state(|s| {
        s.duel.courts[..count]
            .iter()
            .filter(|c| !c.void)
            .map(ResolvedCourt::from)
            .collect()
    });
    let courts_for_bump: Vec<ResolvedCourt> = resolved
        .iter()
        .copied()
        .filter(|c| c.winner.is_some())
        .collect();
    // A void court (instance rebuild failure) queues both players.
    queue = requeue_voids(&resolved, &queue);
    // Record the ladder-long standings once per court.
    state::with_state(|s| {
        for court in &courts_for_bump {
            if let (Some(w), Some(l)) = (court.winner, court.loser) {
                record_result(&mut s.duel.wins, &mut s.duel.losses, w, l);
            }
        }
    });
    push_hud();
    let roster_now = roster();
    let threshold = duel_fraglimit();
    let tie_pick = state::with_state(|s| s.duel.seed).wrapping_add(svs_time() as u32);
    let champion = state::with_state(|s| champion(&s.duel.wins, &roster_now, threshold, tie_pick));
    let capacity = instance_capacity().max(1);
    let (next_pairs, next_queue) = if courts_for_bump.is_empty() {
        (Vec::new(), queue.clone())
    } else {
        let (pairs, bench) = bump(&courts_for_bump, &queue);
        reconcile(&pairs, &bench, &roster_now, capacity)
    };
    // Everyone leaves the arena for the board.
    for i in 0..count {
        spectator_court(i);
    }
    let mut all_out = true;
    state::with_state(|s| {
        for court in s.duel.courts.iter().take(count) {
            if court.winner.is_some() {
                all_out = false;
            }
        }
    });
    let _ = all_out;
    let paused = next_pairs.is_empty() || roster_now.len() < 2;
    let now = svs_time();
    if let Some(champ) = champion {
        let wins = state::with_state(|s| s.duel.wins[champ as usize]);
        eprintln!(
            "----- proxy-rs: parallel duels champion client {} with {} win(s)",
            champ, wins
        );
        let champion_name = format!("{}^7", client_name(champ).trim_end_matches('^'));
        announce(&champion_announcement(&champion_name, wins));
        announce_center(&champion_announcement(&champion_name, wins));
        state::with_state(|s| {
            s.duel.champion = Some(champ);
            s.duel.fresh_restart = true;
            s.duel.pending_courts.clear();
            s.duel.pending_queue.clear();
            s.duel.queue.clear();
            s.duel.phase = DuelPhase::Board;
            s.duel.board_until = now + BOARD_MS;
            s.duel.carry_round = true;
        });
        run_intermission();
        return;
    }
    if paused {
        state::with_state(|s| {
            s.duel.phase = DuelPhase::Idle;
            s.duel.pending_courts.clear();
            s.duel.pending_queue.clear();
            s.duel.queue = next_queue;
            s.duel.carry_round = false;
            s.duel.fresh_restart = false;
        });
        announce("Parallel duels paused - waiting for at least two willing players.");
        return;
    }
    eprintln!(
        "----- proxy-rs: parallel duels round complete — next round {} court(s), {} queued",
        next_pairs.len(),
        next_queue.len()
    );
    state::with_state(|s| {
        s.duel.pending_courts = next_pairs;
        s.duel.pending_queue = next_queue.clone();
        s.duel.queue = next_queue;
        s.duel.phase = DuelPhase::Board;
        s.duel.board_until = now + BOARD_MS;
        s.duel.carry_round = true;
        s.duel.fresh_restart = false;
    });
    run_intermission();
}

fn run_intermission() {
    if original::base() == 0 {
        return;
    }
    // Set the intermission state *before* the pristine body, which builds and
    // sends the final scoreboard from inside the call: the per-viewer refresh
    // and the pre-move board capture must already see it.
    state::with_state(|s| s.tffa.intermission = true);
    crate::tffa::snapshot_intermission_boards();
    // SAFETY: the pristine BeginIntermission trampoline is attached at
    // GAME_INIT and the game is between frames.
    let original = crate::hooks::original_call(crate::hooks::game_hook_original_begin_intermission);
    unsafe { original() };
}

/// Cvar off, wrong gametype or full teardown: release the courts and return
/// everyone to spectator so the stock duel tournament takes over.
pub fn teardown() {
    if !state::with_state(|s| s.duel.ladder || s.duel.phase != DuelPhase::Idle) {
        return;
    }
    let count = state::with_state(|s| s.duel.court_count);
    for i in 0..count {
        spectator_court(i);
    }
    for instance in 1..count {
        crate::instances::release_instance(instance as u8);
    }
    let players: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS)
            .filter(|&i| s.tffa.client_match[i] != crate::instances::MAIN_MATCH)
            .map(|i| i as c_int)
            .collect()
    });
    for cn in players {
        if session_team(cn) != Some(TEAM_SPECTATOR) {
            crate::tffa::set_client_spectator(cn);
        }
    }
    state::with_state(|s| {
        s.tffa.client_match = [crate::instances::MAIN_MATCH; MAX_CLIENTS];
        s.duel = state::DuelState {
            enable_latched: s.duel.enable_latched,
            ..state::DuelState::default()
        };
    });
    eprintln!("----- proxy-rs: parallel duels torn down");
    crate::instances::restore_pristine_views();
    announce("Parallel duels disabled - the stock duel tournament resumes.");
}

/// Whether the stock duel intermission machinery must be no-opped: the
/// ladder runs the board window and the restart itself.
pub fn suppress_stock_intermission() -> bool {
    mode_on() && state::with_state(|s| s.duel.ladder)
}

/// Pristine exits the ladder owns (D-025 §5). `""` is `fraglimit 1`'s
/// `LogExit` (`g_main.c:2796-2812`); `Duel limit hit.` would only fire if a
/// surviving `sess.wins` writer reached `duel_fraglimit` (we keep it inert,
/// but swallow it defensively).
pub fn swallow_log_exit(string: &[u8]) -> bool {
    suppress_stock_intermission() && log_exit_owned(string)
}

/// Pure part of [`swallow_log_exit`]: the exit strings the ladder owns.
pub fn log_exit_owned(string: &[u8]) -> bool {
    matches!(
        string,
        b"" | b"Kill limit hit." | b"Duel limit hit." | b"Timelimit hit."
    )
}

/// Pristine fraglimit/win-limit chat notices (the `LogExit` companions).
pub fn swallow_print(text: &str) -> bool {
    suppress_stock_intermission()
        && (text.contains("HIT_THE_KILL_LIMIT")
            || text.contains("hit the win limit")
            // `CheckExitRules` re-broadcasts this on every rank calculation
            // once the stock `level.startTime` timer passes; the ladder emits
            // one round-level message instead.
            || text.contains("TIMELIMIT_HIT"))
}

/// Restart classification (D-025 §5): only the proxy's own round transition
/// arms the one-shot round token; everything else is an admin restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartKind {
    Round,
    Admin,
}

/// Classify a `map_restart` console command. `armed` is the one-shot token set
/// immediately before the proxy's own command.
pub fn classify_restart(armed: bool, text: &[u8]) -> Option<RestartKind> {
    let command = text
        .split(|b| b.is_ascii_whitespace())
        .find(|t| !t.is_empty())?;
    if !command.eq_ignore_ascii_case(b"map_restart") {
        return None;
    }
    Some(if armed {
        RestartKind::Round
    } else {
        RestartKind::Admin
    })
}

/// `G_SEND_CONSOLE_COMMAND` hook: consume the round token or classify the
/// restart as an admin one. Called for every console command the game issues.
pub fn on_console_command(text: &[u8]) -> Option<RestartKind> {
    if !mode_on() {
        return None;
    }
    let armed = state::with_state(|s| core::mem::replace(&mut s.duel.restart_armed, false));
    let kind = classify_restart(armed, text)?;
    eprintln!("----- proxy-rs: map_restart classified as {kind:?}");
    state::with_state(|s| match kind {
        RestartKind::Round => {
            s.duel.carry_round = true;
        }
        RestartKind::Admin => {
            s.duel.carry_round = false;
            s.duel.fresh_restart = true;
            s.duel.pending_courts.clear();
            s.duel.pending_queue.clear();
            s.duel.wins = [0; MAX_CLIENTS];
            s.duel.losses = [0; MAX_CLIENTS];
            s.duel.champion = None;
            s.duel.queue.clear();
        }
    });
    Some(kind)
}

/// `SetTeam` wrapper verdict for a duel-ladder request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetTeamVerdict {
    Allow,
    Refuse,
}

/// Own every `SetTeam` request while the ladder runs.
///
/// - A spectator request is allowed; it joins the ladder if the client is not
///   on a court (willing), or leaves it if they are (the opponent gets a
///   walkover next frame).
/// - A team/`free` join is refused: the ladder assigns courts, and refusing
///   here also neutralises the game's own `AddTournamentPlayer`, which calls
///   `SetTeam(ent, "f")` (`g_main.c:1226-1275`).
pub fn on_set_team_request(ent: usize, team: &CStr) -> Option<SetTeamVerdict> {
    if !ladder_on() || ent == 0 {
        return None;
    }
    let slot = if g_entities() != 0 && ent >= g_entities() {
        let idx = (ent - g_entities()) / SIZEOF_GENTITY;
        (idx < MAX_CLIENTS).then_some(idx as c_int)
    } else {
        None
    };
    let request = team.to_bytes();
    let spectator = matches!(
        request,
        b"spectator" | b"s" | b"scoreboard" | b"score" | b"follow1" | b"follow2"
    );
    if !spectator {
        if let Some(cn) = slot {
            tell(
                cn,
                "Parallel duels assign courts automatically; use team spectator to leave.",
            );
        }
        return Some(SetTeamVerdict::Refuse);
    }
    if let Some(cn) = slot {
        // Only a client already in the world can explicitly opt out; a
        // connect-time automatic `team s` must not silently remove a joiner.
        if !connected(cn) {
            return Some(SetTeamVerdict::Allow);
        }
        let n = cn as usize;
        let id = cn as u8;
        let in_court = state::with_state(|s| court_of_locked(s, id).is_some());
        // The only explicit way out of the ladder: joining the spectator
        // team. A player who does it on a court also gives the opponent a
        // walkover; one who does it from the bench leaves the queue.
        state::with_state(|s| {
            s.duel.willing[n] = false;
            s.duel.queue.retain(|&c| c != id);
            s.duel.spawn_pending.retain(|&c| c != id);
        });
        if in_court {
            resolve_walkover(id);
        }
    }
    Some(SetTeamVerdict::Allow)
}

/// Cross-instance damage is already blocked; this adds the per-court void
/// window after a duel is decided (pristine `level.intermissionQueued` shape).
pub fn damage_void(client_num: c_int) -> bool {
    if !mode_on() {
        return false;
    }
    let now = svs_time();
    state::with_state(|s| {
        let Some(id) = u8::try_from(client_num).ok() else {
            return false;
        };
        (0..s.duel.court_count).any(|i| {
            let c = s.duel.courts[i];
            c.holds(id)
                && c.resolved
                && !c.void
                && now.wrapping_sub(s.duel.resolved_at[i]) < RESOLVE_VOID_MS
        })
    })
}

/// The client's court (1-based) for the HUD/board, or `None` (spectator/bench).
pub fn court_number(client_num: u8) -> Option<usize> {
    state::with_state(|s| court_of_locked(s, client_num).map(|i| i + 1))
}

/// Standings injected per viewer into `CS_PLAYERS` (w/l).
pub fn standings(client_num: u8) -> (i32, i32) {
    let n = client_num as usize;
    if n >= MAX_CLIENTS {
        return (0, 0);
    }
    state::with_state(|s| (s.duel.wins[n], s.duel.losses[n]))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Pristine duel-credit compensation (D-025 §5)
// ---------------------------------------------------------------------------

/// Pre-`player_die` snapshot of a duel suicide's stock credit target.
#[derive(Debug, Clone, Copy)]
pub struct SuicideSnapshot {
    pub self_num: u8,
    pub opponent: Option<u8>,
    pub self_score: i32,
    pub opponent_score: i32,
    sorted: [i32; 2],
    sorted_scores: [i32; 2],
}

/// Snapshot the stock suicide credit (`g_combat.c:2494-2516`): on a self-kill
/// in `GT_DUEL` the game awards +1 to `level.sortedClients[0]`'s or
/// `[1]`'s partner — the global top two, which need not be this court's
/// opponent. Returns `None` for a normal kill or when the ladder is off.
pub fn suicide_snapshot(self_ent: usize, attacker_ent: usize) -> Option<SuicideSnapshot> {
    if !mode_on() || !state::with_state(|s| s.duel.ladder) {
        return None;
    }
    if self_ent == 0 || self_ent != attacker_ent {
        return None;
    }
    let base = g_entities();
    if base == 0 || self_ent < base {
        return None;
    }
    let self_num = (self_ent - base) / SIZEOF_GENTITY;
    if self_num >= MAX_CLIENTS {
        return None;
    }
    let self_num = self_num as u8;
    let opponent = court_of(self_num).and_then(|i| {
        state::with_state(|s| s.duel.courts.get(i).and_then(|c| c.opponent_of(self_num)))
    });
    let level = original::level_address();
    if level == 0 {
        return None;
    }
    // SAFETY: `level` is the live level_locals_t; sortedClients is an int array.
    let sorted = unsafe {
        [
            read_i32(level + OFFSET_LEVEL_SORTED_CLIENTS),
            read_i32(level + OFFSET_LEVEL_SORTED_CLIENTS + 4),
        ]
    };
    let sorted_scores = sorted.map(|cn| {
        if (0..MAX_CLIENTS as i32).contains(&cn) {
            pers_score(cn)
        } else {
            0
        }
    });
    Some(SuicideSnapshot {
        self_num,
        opponent,
        self_score: pers_score(self_num as c_int),
        opponent_score: opponent.map(|c| pers_score(c as c_int)).unwrap_or_default(),
        sorted,
        sorted_scores,
    })
}

/// Undo the stock suicide credit and award it to the court opponent instead.
/// Only `PERS_SCORE` is corrected; `CalculateRanks` recomputes the sort next
/// frame.
pub fn compensate_suicide(snap: &SuicideSnapshot) {
    for (i, cn) in snap.sorted.iter().copied().enumerate() {
        if !(0..MAX_CLIENTS as i32).contains(&cn)
            || cn as u8 == snap.self_num
            || Some(cn as u8) == snap.opponent
        {
            continue;
        }
        if pers_score(cn) == snap.sorted_scores[i] + 1 {
            write_score(cn, snap.sorted_scores[i]);
        }
    }
    if let Some(opponent) = snap.opponent {
        if pers_score(opponent as c_int) == snap.opponent_score {
            write_score(opponent as c_int, snap.opponent_score + 1);
        }
    }
    // The fallback branch subtracts one from the suicide's own score.
    if pers_score(snap.self_num as c_int) < snap.self_score {
        write_score(snap.self_num as c_int, snap.self_score);
    }
}

fn write_score(client_num: c_int, value: i32) {
    let client = client_of(client_num);
    if client == 0 {
        return;
    }
    // SAFETY: client came from a live gentity's `client` pointer; the
    // persistant slot is a plain int.
    unsafe {
        write_i32(
            client + OFFSET_GCLIENT_PS + OFFSET_PS_PERSISTANT + PERS_SCORE * 4,
            value,
        )
    };
}

/// Pre-`ClientDisconnect` snapshot of the stock duel win credit
/// (`g_client.c:3882-3897`): it zeroes the partner's `PERS_SCORE` and bumps
/// `sess.wins`, which would both wipe a live court score and revive
/// `DuelLimitHit`. The ladder restores the values after the pristine call.
#[derive(Debug, Clone, Copy)]
pub struct DisconnectSnapshot {
    pub partner: Option<u8>,
    wins: i32,
    losses: i32,
    score: i32,
}

/// Snapshot the partner the stock disconnect block would credit, or `None`
/// when the leaver is not one of the global top two or the ladder is off.
pub fn disconnect_snapshot(client_num: c_int) -> Option<DisconnectSnapshot> {
    if !mode_on() || !state::with_state(|s| s.duel.ladder) {
        return None;
    }
    let level = original::level_address();
    if level == 0 {
        return None;
    }
    // SAFETY: `level` is the live level_locals_t; sortedClients is an int array.
    let sorted = unsafe {
        [
            read_i32(level + OFFSET_LEVEL_SORTED_CLIENTS),
            read_i32(level + OFFSET_LEVEL_SORTED_CLIENTS + 4),
        ]
    };
    let partner = if sorted[0] == client_num {
        sorted[1]
    } else if sorted[1] == client_num {
        sorted[0]
    } else {
        return None;
    };
    if !(0..MAX_CLIENTS as c_int).contains(&partner) {
        return None;
    }
    let client = client_of(partner);
    if client == 0 {
        return None;
    }
    // SAFETY: client is a live gclient_t.
    let (wins, losses, score) = unsafe {
        (
            read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_WINS),
            read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_LOSSES),
            read_i32(client + OFFSET_GCLIENT_PS + OFFSET_PS_PERSISTANT + PERS_SCORE * 4),
        )
    };
    Some(DisconnectSnapshot {
        partner: Some(partner as u8),
        wins,
        losses,
        score,
    })
}

/// Restore the partner's pre-disconnect game-side score and W/L, then re-push
/// the proxy standings so the clientinfos the stock block just broadcast are
/// corrected per viewer.
pub fn compensate_disconnect(snap: &DisconnectSnapshot) {
    if let Some(partner) = snap.partner {
        let cn = partner as c_int;
        let client = client_of(cn);
        if client != 0 {
            // SAFETY: client is a live gclient_t.
            unsafe {
                write_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_WINS, snap.wins);
                write_i32(
                    client + OFFSET_GCLIENT_SESS + OFFSET_SESS_LOSSES,
                    snap.losses,
                );
                write_i32(
                    client + OFFSET_GCLIENT_PS + OFFSET_PS_PERSISTANT + PERS_SCORE * 4,
                    snap.score,
                );
            }
        }
    }
    refresh_all_cs_players();
}

// ---------------------------------------------------------------------------
// Per-viewer HUD and board (D-025 §2.17)
// ---------------------------------------------------------------------------

/// `scores ...` entries carry 14 ints (`g_cmds.c:64-74`); the TaystJK variant
/// appends a 15th (`PERS_KILLED`, D-009).
const SCORE_ENTRY_INTS: usize = 14;
const SCORE_ENTRY_INTS_DEATHS: usize = 15;

/// Reorder `scores` entries (`width` ints each) to the ladder order: courts top
/// to bottom, both players each, then the bench, then everyone not named (the
/// real spectators) in their original order. Pure for the tests.
pub fn reorder_scores(order: &[u8], entries: &[i32], width: usize) -> Vec<i32> {
    if width == 0 || entries.len() % width != 0 {
        return entries.to_vec();
    }
    let mut chunks: Vec<(&[i32], usize)> = entries
        .chunks_exact(width)
        .enumerate()
        .map(|(i, c)| (c, i))
        .collect();
    chunks.sort_by_key(|(chunk, original)| {
        let cn = chunk[0];
        if (0..MAX_CLIENTS as i32).contains(&cn) {
            order
                .iter()
                .position(|&c| c as i32 == cn)
                .unwrap_or(usize::MAX / 2 + *original)
        } else {
            usize::MAX / 2 + *original
        }
    });
    chunks
        .into_iter()
        .flat_map(|(c, _)| c.iter().copied())
        .collect()
}

/// The `scores ...` command rebuilt in ladder order for one viewer. Returns
/// `None` when the text is not a scores command or does not parse.
pub fn rewrite_scores_for_viewer(text: &str, _viewer: c_int) -> Option<String> {
    if !mode_on() {
        return None;
    }
    let body = text.strip_prefix("scores ")?;
    let mut nums: Vec<i32> = Vec::with_capacity(3 + MAX_CLIENTS * SCORE_ENTRY_INTS);
    for token in body.split_whitespace() {
        nums.push(token.parse::<i32>().ok()?);
    }
    if nums.len() < 3 {
        return None;
    }
    let entries = &nums[3..];
    let width = if entries.len() % SCORE_ENTRY_INTS == 0 {
        SCORE_ENTRY_INTS
    } else if entries.len() % SCORE_ENTRY_INTS_DEATHS == 0 {
        SCORE_ENTRY_INTS_DEATHS
    } else {
        return None;
    };
    let (courts, bench) = state::with_state(|s| {
        (
            s.duel.courts[..s.duel.court_count].to_vec(),
            s.duel.queue.clone(),
        )
    });
    let order = score_order(&courts, &bench);
    let reordered = reorder_scores(&order, entries, width);
    let count = reordered.len() / width;
    let mut out = format!("scores {count} {} {}", nums[1], nums[2]);
    for value in reordered {
        out.push(' ');
        out.push_str(&value.to_string());
    }
    Some(out)
}

/// Rewrite a player's `CS_PLAYERS` info string for the duel board: court or
/// bench label in the name, ladder W/L in the `w`/`l` keys the stock cgame
/// draws instead of frags on `GT_DUEL` (`cg_scoreboard.c:170,229`). Court
/// players stay in the arena (the board lists every court, unike TFFA's
/// cross-match demotion).
pub fn rewrite_cs_players(base: &str, slot: usize) -> Option<String> {
    if !mode_on() || base.is_empty() || slot >= MAX_CLIENTS {
        return None;
    }
    let id = slot as u8;
    court_number(id);
    let (court, queued) =
        state::with_state(|s| (court_of_locked(s, id), s.duel.queue.contains(&id)));
    if court.is_none() && !queued {
        return None;
    }
    let original = crate::instances::info_value(base, "n").unwrap_or_default();
    let label = match court {
        Some(i) => court_prefix(i + 1),
        None => "(Bench) ".to_owned(),
    };
    let named = crate::instances::prefixed_name_with(&label, &original);
    let mut out = crate::instances::info_set(base, "n", &named);
    let (wins, losses) = standings(id);
    out = crate::instances::info_set(&out, "w", &wins.to_string());
    out = crate::instances::info_set(&out, "l", &losses.to_string());
    (out != base).then_some(out)
}

/// Push every connected viewer's `CS_PLAYERS` overrides again so the board and
/// team overlay show the fresh court labels and W/L.
pub fn refresh_all_cs_players() {
    if !mode_on() {
        return;
    }
    let viewers: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|v| s.clients[*v as usize].is_connected)
            .collect()
    });
    for viewer in viewers {
        crate::instances::refresh_cs_players_for_viewer(viewer);
    }
}

/// Per-viewer duel HUD: the viewer's own court pair in
/// `CS_CLIENT_DUELISTS`, the pair's frags in `CS_SCORES1/2`, and (when
/// `g_showDuelHealths` is set) `CS_CLIENT_DUELHEALTHS`. Everything is deduped
/// against the last value queued, so a steady court costs nothing.
pub fn push_hud() {
    if !mode_on() || !state::with_state(|s| s.duel.ladder) {
        return;
    }
    let show_health = cvar(c"g_showDuelHealths") >= 1;
    let viewers: Vec<c_int> = state::with_state(|s| {
        (0..MAX_CLIENTS as c_int)
            .filter(|v| s.clients[*v as usize].is_connected)
            .collect()
    });
    for viewer in viewers {
        let vm = crate::instances::effective_match(viewer);
        // Read the pair under the lock, then the game memory outside it: the
        // `pers_score`/`stat` helpers take the state lock themselves.
        let court = state::with_state(|s| {
            s.duel
                .courts
                .get(vm as usize)
                .copied()
                .and_then(|c| match (c.a, c.b) {
                    (Some(a), Some(b)) => Some((a, b)),
                    _ => None,
                })
        });
        let scores = match court {
            Some((a, b)) => (pers_score(a as c_int), pers_score(b as c_int)),
            None => (0, 0),
        };
        let healths = if show_health {
            court.map(|(a, b)| {
                format!(
                    "{}|{}|!",
                    stat(a as c_int, STAT_HEALTH),
                    stat(b as c_int, STAT_HEALTH)
                )
            })
        } else {
            None
        };
        let duelists = match court {
            Some((a, b)) => format!("{a}|{b}"),
            None => "-1|-1".to_owned(),
        };
        let viewer_idx = viewer as usize;
        let changed_scores = state::with_state(|s| s.duel.hud_scores[viewer_idx] != scores);
        if changed_scores
            && crate::instances::send_cs_to_client(viewer, CS_SCORES1, &scores.0.to_string())
            && crate::instances::send_cs_to_client(viewer, CS_SCORES2, &scores.1.to_string())
        {
            state::with_state(|s| s.duel.hud_scores[viewer_idx] = scores);
        }
        let changed_duelists = state::with_state(|s| s.duel.hud_duelists[viewer_idx] != duelists);
        if changed_duelists
            && crate::instances::send_cs_to_client(viewer, CS_CLIENT_DUELISTS, &duelists)
        {
            state::with_state(|s| s.duel.hud_duelists[viewer_idx] = duelists);
        }
        if let Some(healths) = healths {
            let changed = state::with_state(|s| s.duel.hud_healths[viewer_idx] != healths);
            if changed
                && crate::instances::send_cs_to_client(viewer, CS_CLIENT_DUELHEALTHS, &healths)
            {
                state::with_state(|s| s.duel.hud_healths[viewer_idx] = healths);
            }
        }
    }
}

fn announce_center(text: &str) {
    let safe = text.replace('"', "'");
    let Ok(msg) = std::ffi::CString::new(format!("cp \"{safe}\"")) else {
        return;
    };
    // SAFETY: engine syscall registered; msg NUL-terminated.
    unsafe { syscall::send_server_command(-1, &msg) };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(n: u8) -> Vec<u8> {
        (0..n).collect()
    }

    fn resolved(a: u8, b: u8, winner: u8) -> ResolvedCourt {
        ResolvedCourt {
            a: Some(a),
            b: Some(b),
            winner: Some(winner),
            loser: Some(if winner == a { b } else { a }),
            void: false,
        }
    }

    #[test]
    fn connects_seed_willingness_on_a_fresh_ladder() {
        assert!(should_seed_on_connect(true, true, false));
        assert!(should_seed_on_connect(false, false, false));
        assert!(should_seed_on_connect(false, true, true));
        assert!(!should_seed_on_connect(false, true, false));
    }

    #[test]
    fn name_labels_reset_to_white() {
        let mut names: [String; MAX_CLIENTS] = core::array::from_fn(|_| String::new());
        names[3] = "^1Bob".to_owned();
        assert_eq!(name_label(&names, 3), "^1Bob^7");
        names[4] = "^2Team ^3Alice^".to_owned();
        assert_eq!(name_label(&names, 4), "^2Team ^3Alice^7");
        names[5] = String::new();
        assert_eq!(name_label(&names, 5), "#5^7");
    }

    #[test]
    fn shuffle_is_a_permutation_and_seed_stable() {
        let r = roster(16);
        let a = shuffle(&r, 12345);
        let b = shuffle(&r, 12345);
        let c = shuffle(&r, 99);
        assert_eq!(a, b, "same seed, same shuffle");
        assert_ne!(a, c, "different seed, different order");
        let mut sorted = a.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, r, "no player lost or duplicated");
        // Zero seed is remapped instead of locking the generator.
        assert_eq!(shuffle(&r, 0).len(), 16);
    }

    #[test]
    fn formation_pairs_the_shuffle_and_queues_the_remainder() {
        let r = roster(7);
        let f = form(&r, 16, 7);
        assert_eq!(f.courts.len(), 3);
        assert_eq!(f.queue.len(), 1);
        let mut all: Vec<u8> = f
            .courts
            .iter()
            .flat_map(|p| [p.a.unwrap(), p.b.unwrap()])
            .chain(f.queue.iter().copied())
            .collect();
        all.sort_unstable();
        assert_eq!(all, r);
        // Capacity caps the courts; the surplus queues in shuffled order.
        let f = form(&r, 2, 7);
        assert_eq!(f.courts.len(), 2);
        assert_eq!(f.queue.len(), 3);
    }

    #[test]
    fn court_count_caps_at_sixteen_and_capacity() {
        assert_eq!(court_count(0, 16), 0);
        assert_eq!(court_count(1, 16), 0);
        assert_eq!(court_count(2, 16), 1);
        assert_eq!(court_count(7, 16), 3);
        assert_eq!(court_count(40, 16), 16);
        assert_eq!(court_count(40, 4), 4);
        assert_eq!(court_count(9, 3), 3);
    }

    #[test]
    fn bump_ladder_moves_winners_up_and_losers_down() {
        // Three courts: 0v1, 2v3, 4v5; a(0) beats b(1), 3 beats 2, 4 beats 5.
        let courts = vec![resolved(0, 1, 0), resolved(2, 3, 3), resolved(4, 5, 4)];
        let (next, queue) = bump(&courts, &[]);
        // Court 1 = W1 + W2, court 2 = L1 + W3, court 3 = L2 + L3.
        assert_eq!(next[0], Pair::new(0, 3));
        assert_eq!(next[1], Pair::new(1, 4));
        assert_eq!(next[2], Pair::new(2, 5));
        assert!(queue.is_empty(), "even roster, no queue");
    }

    #[test]
    fn bump_with_a_queue_is_one_in_and_one_out() {
        // Two courts, 6 beats 7 at the bottom, one player queued.
        let courts = vec![resolved(0, 1, 0), resolved(2, 3, 2)];
        let (next, queue) = bump(&courts, &[9, 10]);
        assert_eq!(next[0], Pair::new(0, 2), "W1 + W2");
        assert_eq!(next[1], Pair::new(1, 9), "L1 + queue front");
        assert_eq!(queue, vec![10, 3], "L(C) joins the back, order kept");
    }

    #[test]
    fn bump_single_court_keeps_both_without_a_queue() {
        let courts = vec![resolved(4, 5, 4)];
        let (next, queue) = bump(&courts, &[]);
        assert_eq!(next[0], Pair::new(4, 5));
        assert!(queue.is_empty());
    }

    #[test]
    fn bump_single_court_rotates_the_queue() {
        let courts = vec![resolved(4, 5, 4)];
        let (next, queue) = bump(&courts, &[8]);
        assert_eq!(next[0], Pair::new(4, 8), "winner keeps court, queue fills");
        assert_eq!(queue, vec![5], "the loser goes to the back");
    }

    #[test]
    fn void_court_requeues_both_players() {
        let courts = vec![
            resolved(0, 1, 0),
            ResolvedCourt {
                a: Some(2),
                b: Some(3),
                winner: None,
                loser: None,
                void: true,
            },
        ];
        assert_eq!(requeue_voids(&courts, &[9]), vec![9, 2, 3]);
        // A void court contributes no W/L to the bump.
        let (next, queue) = bump(&courts[..1], &[9]);
        assert_eq!(next[0], Pair::new(0, 9));
        assert_eq!(queue, vec![1]);
    }

    #[test]
    fn queue_helpers_keep_order_and_dedupe() {
        let mut q = vec![3, 4];
        queue_push(&mut q, 5);
        assert_eq!(q, vec![3, 4, 5]);
        queue_push(&mut q, 3);
        assert_eq!(q, vec![3, 4, 5], "no duplicate bench entry");
        assert_eq!(queue_without(&q, 4), vec![3, 5]);
        assert_eq!(queue_pop_front(&mut q), Some(3));
        assert_eq!(q, vec![4, 5]);
    }

    #[test]
    fn fraglimit_resolution() {
        assert_eq!(fraglimit_winner(2, 1, 3), None);
        assert_eq!(fraglimit_winner(3, 1, 3), Some(Side::A));
        assert_eq!(fraglimit_winner(1, 3, 3), Some(Side::B));
        // Both over the limit in one frame: the higher score wins.
        assert_eq!(fraglimit_winner(4, 3, 3), Some(Side::A));
        assert_eq!(fraglimit_winner(3, 4, 3), Some(Side::B));
        // 0 = no per-duel kill limit.
        assert_eq!(fraglimit_winner(99, 0, 0), None);
        assert_eq!(fraglimit_winner(99, 0, -1), None);
    }

    #[test]
    fn health_resolution_uses_hp_plus_shield_and_ties() {
        // 40 hp + 25 shield = 65 vs 50 hp + 10 = 60.
        assert_eq!(health_winner(40, 25, 50, 10), Some(Side::A));
        assert_eq!(health_winner(50, 10, 40, 25), Some(Side::B));
        assert_eq!(health_winner(50, 0, 20, 30), None);
        // Negative values (dead) clamp to 0.
        assert_eq!(health_winner(-5, 0, 0, 0), None);
        assert_eq!(break_tie(false), Side::A);
        assert_eq!(break_tie(true), Side::B);
    }

    #[test]
    fn walkover_awards_the_present_player() {
        assert_eq!(walkover(Side::A), (Side::A, Side::B));
        assert_eq!(walkover(Side::B), (Side::B, Side::A));
    }

    #[test]
    fn standings_track_wins_and_losses() {
        let mut wins = [0; crate::sdk::MAX_CLIENTS];
        let mut losses = [0; crate::sdk::MAX_CLIENTS];
        record_result(&mut wins, &mut losses, 3, 4);
        record_result(&mut wins, &mut losses, 3, 5);
        record_result(&mut wins, &mut losses, 4, 3);
        assert_eq!(wins[3], 2);
        assert_eq!(losses[3], 1);
        assert_eq!(wins[4], 1);
        assert_eq!(losses[4], 1);
        assert_eq!(losses[5], 1);
        reset_standings(&mut wins, &mut losses);
        assert!(wins.iter().all(|&w| w == 0));
        assert!(losses.iter().all(|&l| l == 0));
    }

    #[test]
    fn champion_needs_the_threshold_and_breaks_ties() {
        let mut wins = [0; crate::sdk::MAX_CLIENTS];
        wins[1] = 2;
        wins[5] = 2;
        wins[9] = 3;
        let roster = vec![1, 5, 9];
        // 9 has more wins than everybody, so it is the unique champion.
        assert_eq!(champion(&wins, &roster, 2, 0), Some(9));
        // Tie between 1 and 5: the tie pick chooses.
        wins[9] = 0;
        assert_eq!(champion(&wins, &roster, 2, 0), Some(1));
        assert_eq!(champion(&wins, &roster, 2, 1), Some(5));
        // Endless ladder never crowns.
        assert_eq!(champion(&wins, &roster, 0, 0), None);
        // Threshold not reached.
        assert_eq!(champion(&wins, &roster, 3, 0), None);
        // Only roster members can be champion (a leaver's wins do not count).
        assert_eq!(champion(&wins, &[9], 2, 0), None);
    }

    #[test]
    fn reconcile_grows_the_ladder_when_players_queue_up() {
        // One court, seven queued players, room for four: the next round opens
        // four courts (joins extend the ladder at the round boundary).
        let roster: Vec<u8> = (0..8).collect();
        let pairs = vec![Pair::new(0, 1)];
        let queue = vec![2, 3, 4, 5, 6, 7];
        let (courts, bench) = reconcile(&pairs, &queue, &roster, 16);
        assert_eq!(
            courts,
            vec![
                Pair::new(0, 1),
                Pair::new(2, 3),
                Pair::new(4, 5),
                Pair::new(6, 7)
            ]
        );
        assert!(bench.is_empty());
        // Odd roster: the last player stays benched.
        let roster: Vec<u8> = (0..7).collect();
        let (courts, bench) = reconcile(&pairs, &[2, 3, 4, 5, 6], &roster, 16);
        assert_eq!(courts.len(), 3);
        assert_eq!(bench, vec![6]);
        // Capacity still caps the growth.
        let roster: Vec<u8> = (0..8).collect();
        let (courts, bench) = reconcile(&pairs, &[2, 3, 4, 5, 6, 7], &roster, 2);
        assert_eq!(courts.len(), 2);
        assert_eq!(bench, vec![4, 5, 6, 7]);
    }

    #[test]
    fn round_message_uses_real_newlines_and_single_byte_text() {
        let names = core::array::from_fn(|i| format!("P{i}"));
        let text = round_announcement(&[Court::new(1, 2), Court::new(3, 4)], &[7], &names);
        assert!(text.is_ascii(), "client renders latin-1: {text:?}");
        assert!(text.contains("\n court 1: P1^7 vs P2^7"), "{text:?}");
        assert!(text.contains("\n court 2: P3^7 vs P4^7"), "{text:?}");
        assert!(text.contains("\n bench: P7^7"), "{text:?}");
        // A UI dash or curly quote becomes single-byte, never a literal
        // backslash-n or UTF-8 mojibake.
        let safe = message_safe("a \u{2014} \u{201c}b\u{201d}");
        assert_eq!(safe, "a ? ?b?");
        assert!(safe.is_ascii());
    }

    #[test]
    fn reconcile_drops_leavers_queues_newcomers_and_fills_slots() {
        // Two courts, one bench. Client 1 leaves, client 2 stops being willing,
        // client 9 is new.
        let pairs = vec![Pair::new(0, 1), Pair::new(2, 3)];
        let roster = vec![0, 3, 4, 5, 9];
        let (courts, queue) = reconcile(&pairs, &[4], &roster, 16);
        // Pair 0 loses 1: court 0 keeps 0 and takes the queue front 4.
        assert_eq!(courts[0], Pair::new(0, 4));
        // Pair 1 loses 2: court 1 keeps 3 in the B slot, then takes 5 (newcomer queue).
        assert_eq!(
            courts[1],
            Pair {
                a: Some(5),
                b: Some(3)
            }
        );
        // Newcomer 9 waits.
        assert_eq!(queue, vec![9]);
        // Capacity caps courts; the surplus pair's players queue.
        let pairs = vec![Pair::new(0, 3), Pair::new(4, 5), Pair::new(9, 10)];
        let roster = vec![0, 3, 4, 5, 9, 10];
        let (courts, queue) = reconcile(&pairs, &[], &roster, 2);
        assert_eq!(courts.len(), 2);
        assert_eq!(queue, vec![9, 10]);
        // A pair with only one surviving player is dropped and that player
        // queues instead of playing alone.
        let pairs = vec![Pair::new(0, 1)];
        let (courts, queue) = reconcile(&pairs, &[], &[0], 16);
        assert!(courts.is_empty());
        assert_eq!(queue, vec![0]);
    }

    #[test]
    fn board_order_is_courts_then_bench() {
        let courts = vec![Court::new(1, 2), Court::new(3, 4)];
        let rows = board_rows(&courts, &[7, 8]);
        let order: Vec<(usize, u8, bool)> =
            rows.iter().map(|r| (r.court, r.client, r.queued)).collect();
        assert_eq!(
            order,
            vec![
                (1, 1, false),
                (1, 2, false),
                (2, 3, false),
                (2, 4, false),
                (0, 7, true),
                (0, 8, true)
            ]
        );
        assert_eq!(score_order(&courts, &[7, 8]), vec![1, 2, 3, 4, 7, 8]);
    }

    #[test]
    fn restart_classifier_only_the_armed_token_is_a_round() {
        assert_eq!(
            classify_restart(true, b"map_restart 0"),
            Some(RestartKind::Round)
        );
        assert_eq!(
            classify_restart(false, b"map_restart"),
            Some(RestartKind::Admin)
        );
        assert_eq!(
            classify_restart(false, b"  MAP_RESTART 0\n"),
            Some(RestartKind::Admin)
        );
        // Any other console command is not a restart.
        assert_eq!(classify_restart(true, b"map mp/duel1"), None);
        assert_eq!(classify_restart(true, b""), None);
        assert_eq!(classify_restart(false, b"svsay hi"), None);
    }

    #[test]
    fn log_exit_ownership_covers_every_ladder_end() {
        // fraglimit > 1, fraglimit 1 (empty string), the win limit and the
        // map timelimit; anything else (a real map end) still passes.
        assert!(log_exit_owned(b"Kill limit hit."));
        assert!(log_exit_owned(b""));
        assert!(log_exit_owned(b"Duel limit hit."));
        assert!(log_exit_owned(b"Timelimit hit."));
        assert!(!log_exit_owned(b"Capturelimit hit."));
        assert!(!log_exit_owned(b"Duel forfeit."));
    }

    #[test]
    fn scores_are_reordered_by_ladder_position() {
        // 14-int entries: (client, score, ...). Order: court 1 players 4,5,
        // then court 2 players 0,1, then bench 9.
        let mut entries = Vec::new();
        for cn in [9, 2, 1, 0, 5, 4] {
            let mut e = vec![0i32; 14];
            e[0] = cn;
            e[1] = cn * 10;
            entries.extend(e);
        }
        let order = [4u8, 5, 0, 1, 9];
        let out = reorder_scores(&order, &entries, 14);
        let clients: Vec<i32> = out.chunks(14).map(|c| c[0]).collect();
        // Court players first in court order; the unlisted client 2 keeps its
        // relative place at the end (spectator).
        assert_eq!(clients, vec![4, 5, 0, 1, 9, 2]);
        // 15-int (TaystJK) entries are reordered the same way.
        let mut entries = Vec::new();
        for cn in [3, 2, 1] {
            let mut e = vec![0i32; 15];
            e[0] = cn;
            entries.extend(e);
        }
        let out = reorder_scores(&[1, 2], &entries, 15);
        let clients: Vec<i32> = out.chunks(15).map(|c| c[0]).collect();
        assert_eq!(clients, vec![1, 2, 3]);
    }

    #[test]
    fn messages_name_courts_and_bench() {
        let courts = vec![Court::new(1, 2), Court::new(3, 4)];
        let names = core::array::from_fn(|i| format!("P{i}"));
        let text = round_announcement(&courts, &[7], &names);
        assert!(text.contains("court 1: P1^7 vs P2^7"));
        assert!(text.contains("court 2: P3^7 vs P4^7"));
        assert!(text.contains("bench: P7^7"));
        assert!(round_announcement(&courts, &[], &names).contains("bench: empty"));
        assert!(champion_announcement("P5^7", 3).contains("P5^7"));
    }
}
