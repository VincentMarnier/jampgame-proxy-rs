//! Team shuffling: `callvote shuffle <players per team>`.
//!
//! A passed vote sends everyone to spectator, starts a 30 s `/ready` window
//! (centre print + console line) and then spreads the ready players over the
//! main game and as many parallel TFFA matches as needed. Each game gets two
//! equally sized teams, never larger than the vote's team size; when the
//! parallel-TFFA feature is off only the main game is filled, exactly like a
//! TFFA server whose created-match slots are unavailable.
//!
//! The vote itself is a proxy-owned one (D-024 machinery): the game's own vote
//! counter/tally/pass/fail path runs it, the proxy-private `proxy_teamshuffle
//! <n>` execution command starts the ready window, and `/ready` is claimed
//! before the game's `ClientCommand`. No game state is changed inside the
//! `G_SEND_CONSOLE_COMMAND` trap: the start is deferred to the next frame.
//!
//! Placement rule: the largest even number of ready players is placed (an odd
//! one leaves exactly one spectator), using the smallest number of games, with
//! team sizes as close together as possible (`4, 2` → `3, 3`; `4, 3, 2` for 19
//! is not chosen because `3, 3, 3` is the more even split of the same 18
//! players). A team below `proxy_sv_teamShuffleMinTeamSize` is never created.
//!
//! Like every proxy feature it needs `proxy_sv_enable 1`, the vote needs
//! `proxy_sv_teamShuffle 1`, and the command only answers on a team gametype.

use core::ffi::{CStr, c_char, c_int};

use crate::sdk::{EXEC_APPEND, GT_CTF, GT_TEAM, MAX_CLIENTS, TEAM_BLUE, TEAM_RED};
use crate::state::{self, CVAR_TEAM_SHUFFLE, CVAR_TEAM_SHUFFLE_MIN_TEAM_SIZE};
use crate::{syscall, tffa};

/// The proxy-private command the game runs when the shuffle vote passes.
const VOTE_COMMAND: &str = "proxy_teamshuffle";
/// The window between the passed vote and the placement.
pub(crate) const READY_WINDOW_MS: u32 = 30_000;
/// How long before the deadline the last countdown line is printed.
const WARN_AT_MS: u32 = 10_000;
/// Largest team a shuffle can form (`MAX_CLIENTS / 2`).
const MAX_TEAM_SIZE: usize = MAX_CLIENTS / 2;

/// Team-shuffle state (`ProxyState::shuffle`).
#[derive(Debug, Clone)]
pub struct ShuffleState {
    /// A ready window is running (`deadline_ms`/`max_team` are meaningful).
    pub active: bool,
    /// `Sys_Milliseconds` when the ready window ends (wrap-aware compare).
    pub deadline_ms: u32,
    /// Team size the vote asked for.
    pub max_team: u8,
    /// The 10 s warning has been printed.
    pub warned: bool,
    /// Clients that typed `/ready` for this window.
    pub ready: [bool; MAX_CLIENTS],
    /// A passed vote is waiting for the next frame to start its window.
    pub pending_start: Option<u8>,
}

impl Default for ShuffleState {
    fn default() -> Self {
        ShuffleState {
            active: false,
            deadline_ms: 0,
            max_team: 0,
            warned: false,
            ready: [false; MAX_CLIENTS],
            pending_start: None,
        }
    }
}

impl ShuffleState {
    fn clear(&mut self) {
        *self = ShuffleState::default();
    }
}

/// The feature is on, the master switch is on and the map is a team gametype.
fn allowed() -> bool {
    if !state::proxy_enabled() || state::cvar_integer(CVAR_TEAM_SHUFFLE) == 0 {
        return false;
    }
    // SAFETY: engine syscall registered (GAME_INIT ran before any client
    // command or frame hook).
    let gametype = unsafe { syscall::cvar_variable_integer_value(c"g_gametype") };
    gametype == GT_TEAM || gametype == GT_CTF
}

/// `proxy_sv_teamShuffleMinTeamSize`, clamped to something a team can be.
fn min_team_size() -> usize {
    state::cvar_integer(CVAR_TEAM_SHUFFLE_MIN_TEAM_SIZE).clamp(1, MAX_TEAM_SIZE as i32) as usize
}

/// A positive decimal team size (`4`); anything else is refused rather than
/// read as 0 like the engine's `atoi`.
fn parse_team_size(arg: &[u8]) -> Option<usize> {
    let arg = arg.trim_ascii();
    if arg.is_empty() || !arg.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut value = 0usize;
    for &byte in arg {
        value = value.checked_mul(10)?.checked_add((byte - b'0') as usize)?;
    }
    Some(value)
}

/// Claim `/ready`, `callvote shuffle <n>` and leave every other command to the
/// game. Returning `true` means the caller must not forward the command.
pub fn handle_client_command(
    client_num: c_int,
    command: &[u8],
    argument: &[u8],
    team_size: &[u8],
) -> bool {
    if command.eq_ignore_ascii_case(b"ready") {
        if !ready_is_ours() {
            return false;
        }
        handle_ready(client_num);
        return true;
    }
    if !is_shuffle_vote(command, argument) {
        return false;
    }
    // Disabled: let the pristine game reject the unknown vote name.
    if !allowed() {
        return false;
    }
    start_vote(client_num, team_size);
    true
}

/// Whether a `/ready` belongs to the proxy: the feature is on, or a window is
/// still running after an admin turned it off (then the answer explains that).
fn ready_is_ours() -> bool {
    if !state::proxy_enabled() {
        return false;
    }
    allowed() || state::with_state(|s| s.shuffle.active || s.shuffle.pending_start.is_some())
}

fn is_shuffle_vote(command: &[u8], argument: &[u8]) -> bool {
    command.eq_ignore_ascii_case(b"callvote") && argument.eq_ignore_ascii_case(b"shuffle")
}

fn start_vote(client_num: c_int, team_size: &[u8]) {
    let Some(size) = parse_team_size(team_size) else {
        crate::votes::reply(
            client_num,
            "Usage: callvote shuffle <players per team> (for example: callvote shuffle 4).",
        );
        return;
    };
    let min = min_team_size();
    if size < min {
        crate::votes::reply(
            client_num,
            &format!(
                "A shuffle needs at least {min} players per team on this server (proxy_sv_teamShuffleMinTeamSize)."
            ),
        );
        return;
    }
    if size > MAX_TEAM_SIZE {
        crate::votes::reply(
            client_num,
            &format!("A shuffle team can have at most {MAX_TEAM_SIZE} players."),
        );
        return;
    }
    if state::with_state(|s| s.shuffle.active || s.shuffle.pending_start.is_some()) {
        crate::votes::reply(client_num, "A team shuffle is already in progress.");
        return;
    }

    let command = format!("{VOTE_COMMAND} {size}");
    let display = format!("shuffle {size}");
    if let Err(reason) = crate::votes::start_proxy_vote(client_num, &command, &display) {
        crate::votes::reply(client_num, reason.message());
    }
}

/// `G_SEND_CONSOLE_COMMAND` helper: read the text pointer safely (the game may
/// pass null). Returns `false` when the command must be dropped.
pub fn console_command_from_ptr(exec: i32, text: *const c_char) -> bool {
    if text.is_null() {
        return true;
    }
    // SAFETY: the game passes a NUL-terminated command string.
    console_command(exec, unsafe { CStr::from_ptr(text) }.to_bytes())
}

/// A passed shuffle vote executes [`VOTE_COMMAND`]. Drop the command and arm
/// the ready window; the next `GAME_RUN_FRAME` performs the moves, outside the
/// game's own vote/console-command call.
fn console_command(exec: i32, text: &[u8]) -> bool {
    if exec != EXEC_APPEND {
        return true;
    }
    let trimmed = text.trim_ascii();
    if trimmed.len() < VOTE_COMMAND.len()
        || !trimmed[..VOTE_COMMAND.len()].eq_ignore_ascii_case(VOTE_COMMAND.as_bytes())
    {
        return true;
    }
    let rest = &trimmed[VOTE_COMMAND.len()..];
    if !rest.is_empty() && !rest[0].is_ascii_whitespace() {
        return true;
    }
    let size = parse_team_size(rest)
        .and_then(|size| u8::try_from(size).ok())
        .filter(|size| (1..=MAX_TEAM_SIZE as u8).contains(size));
    state::with_state(|s| s.shuffle.pending_start = size);
    false
}

/// What a frame tick found to do; collected under the state lock, executed
/// without it.
enum Tick {
    None,
    Start(u8),
    Warn,
    Cancel(&'static str),
    Finish {
        ready: [bool; MAX_CLIENTS],
        max_team: u8,
    },
}

/// Per-frame tick, called after the game's `GAME_RUN_FRAME` body (so its vote
/// and intermission handling have run).
pub fn on_run_frame_post() {
    if !state::proxy_enabled() {
        return;
    }
    let now = syscall::milliseconds();
    let allowed_now = allowed();
    let tick = state::with_state(|s| {
        if let Some(size) = s.shuffle.pending_start.take() {
            if !allowed_now {
                s.shuffle.clear();
                return Tick::Cancel(
                    "Team shuffle canceled: the feature is disabled or this is not a team gametype.",
                );
            }
            return Tick::Start(size);
        }
        if !s.shuffle.active {
            return Tick::None;
        }
        if !allowed_now {
            s.shuffle.clear();
            return Tick::Cancel(
                "Team shuffle canceled: the feature was turned off. Everyone stays a spectator.",
            );
        }
        if deadline_reached(now, s.shuffle.deadline_ms) {
            let ready = s.shuffle.ready;
            let max_team = s.shuffle.max_team;
            s.shuffle.clear();
            return Tick::Finish { ready, max_team };
        }
        if !s.shuffle.warned && remaining_ms(now, s.shuffle.deadline_ms) <= WARN_AT_MS {
            s.shuffle.warned = true;
            return Tick::Warn;
        }
        Tick::None
    });

    match tick {
        Tick::None => {}
        Tick::Start(size) => begin_ready(size),
        Tick::Warn => {
            crate::votes::send_print(-1, "^3[Team Shuffle]^7 10 seconds left to type ^3/ready^7.")
        }
        Tick::Cancel(text) => crate::votes::send_print(-1, &format!("^3[Team Shuffle]^7 {text}")),
        Tick::Finish { ready, max_team } => finish(&ready, max_team),
    }
}

/// Wrap-safe "now is at or past the deadline" (windows are far below 2^31 ms).
fn deadline_reached(now: u32, deadline: u32) -> bool {
    (now.wrapping_sub(deadline) as i32) >= 0
}

/// Wrap-safe milliseconds left, `0` once the deadline passed.
fn remaining_ms(now: u32, deadline: u32) -> u32 {
    let left = deadline.wrapping_sub(now);
    if (left as i32) < 0 {
        0
    } else {
        left.min(i32::MAX as u32)
    }
}

/// Start the ready window: clear every match, send everyone playing to
/// spectator, then announce the `/ready` invitation on screen and in console.
fn begin_ready(size: u8) {
    let min = min_team_size();
    let size = size as usize;
    if size < min || size > MAX_TEAM_SIZE {
        state::with_state(|s| s.shuffle.clear());
        crate::votes::send_print(
            -1,
            "^3[Team Shuffle]^7 canceled: the team-size limits changed while the vote ran.",
        );
        return;
    }
    if state::with_state(|s| s.tffa.intermission) {
        state::with_state(|s| s.shuffle.clear());
        crate::votes::send_print(
            -1,
            "^3[Team Shuffle]^7 canceled: the map is ending. Everyone stays a spectator.",
        );
        return;
    }

    tffa::evacuate_to_spectator();
    let now = syscall::milliseconds();
    state::with_state(|s| {
        s.shuffle.clear();
        s.shuffle.active = true;
        s.shuffle.deadline_ms = now.wrapping_add(READY_WINDOW_MS);
        s.shuffle.max_team = size as u8;
    });

    center_all(&format!(
        "TEAM SHUFFLE: type /ready within 30 seconds to be placed in a team (up to {size} per team)."
    ));
    crate::votes::send_print(
        -1,
        &format!(
            "^3[Team Shuffle]^7 Vote passed! Everyone is in spectator. Type ^3/ready^7 within 30 seconds to join a team (up to ^3{size}^7 per team, minimum {min})."
        ),
    );
    eprintln!(
        "----- proxy-rs: team shuffle ready window started (max {size} per team, minimum {min}, {READY_WINDOW_MS} ms)"
    );
}

/// `/ready`: remember the client for the placement and acknowledge it.
fn handle_ready(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    let connected = tffa::connected_clients();
    if !connected[client_num as usize] {
        return;
    }
    let (active, already) = state::with_state(|s| {
        if !s.shuffle.active {
            return (false, false);
        }
        let already = s.shuffle.ready[client_num as usize];
        s.shuffle.ready[client_num as usize] = true;
        (true, already)
    });
    if !active {
        crate::votes::reply(client_num, "No team shuffle is in progress.");
        return;
    }
    let ready = state::with_state(|s| {
        s.shuffle
            .ready
            .iter()
            .zip(connected.iter())
            .filter(|(ready, connected)| **ready && **connected)
            .count()
    });
    if already {
        crate::votes::reply(
            client_num,
            &format!("You are already ready ({ready} players ready)."),
        );
    } else {
        crate::votes::reply(
            client_num,
            &format!("You are ready ({ready} players ready). Waiting for the shuffle."),
        );
    }
    center_one(client_num, "READY - wait for the shuffle to begin.");
}

/// End the window: place the ready players in games of equal teams.
fn finish(ready: &[bool; MAX_CLIENTS], max_team: u8) {
    if state::with_state(|s| s.tffa.intermission) {
        crate::votes::send_print(
            -1,
            "^3[Team Shuffle]^7 canceled: the map is ending. Everyone stays a spectator.",
        );
        return;
    }

    let connected = tffa::connected_clients();
    let ready_players: Vec<usize> = ready
        .iter()
        .zip(connected.iter())
        .enumerate()
        .filter(|(_, (ready, connected))| **ready && **connected)
        .map(|(slot, _)| slot)
        .collect();

    let min = min_team_size();
    let size = (max_team as usize).clamp(1, MAX_TEAM_SIZE);
    // The main game always exists; only parallel TFFA adds created slots.
    let parallel = state::team_mode();
    let max_games = if parallel { 1 + tffa::max_matches() } else { 1 };

    // A second sweep: a player who joined a team during the window (or a match
    // someone created) must not survive the placement as a player in main.
    tffa::evacuate_to_spectator();

    let plan = plan_with_realized_slots(ready_players.len(), size, min, max_games, parallel);
    if plan.is_empty() {
        crate::votes::send_print(
            -1,
            &format!(
                "^3[Team Shuffle]^7 Not enough ready players to form a game (at least {} needed). Everyone stays a spectator.",
                min * 2
            ),
        );
        return;
    }

    let mut order = ready_players;
    let ready_count = order.len();
    shuffle_order(&mut order);
    let placed = assign_games(&order, &plan);
    announce_result(&placed, &plan, ready_count);
}

/// The pure placement rule: how many players each game's two teams get.
///
/// Places the largest even number of ready players (an odd ready count leaves
/// one spectator), capped by `max_games * max_team`, in the smallest number of
/// games, with the team sizes differing by at most one. A team smaller than
/// `min_team` is never returned.
pub(crate) fn plan_team_sizes(
    ready: usize,
    max_team: usize,
    min_team: usize,
    max_games: usize,
) -> Vec<usize> {
    if min_team == 0 || max_team < min_team || max_games == 0 {
        return Vec::new();
    }
    let mut side_total = ready / 2;
    if side_total < min_team {
        return Vec::new();
    }
    side_total = side_total.min(max_games.saturating_mul(max_team));
    // A side total is only placeable when the smallest game count that keeps
    // every team at or below `max_team` also keeps every team at or above
    // `min_team`. Drop players (largest representable total) instead of
    // forming a too-small team: 10 ready with 4/3 becomes one 4v4 and two
    // spectators, not a 3v2.
    while side_total >= min_team {
        let games = side_total.div_ceil(max_team);
        if side_total >= games.saturating_mul(min_team) {
            break;
        }
        side_total -= 1;
    }
    if side_total < min_team {
        return Vec::new();
    }
    let games = side_total.div_ceil(max_team);
    let base = side_total / games;
    let remainder = side_total % games;
    (0..games)
        .map(|index| base + usize::from(index < remainder))
        .collect()
}

/// Plan against the created worlds that actually materialised. Creating the
/// per-instance world set can fail (map clone budget); when slot `k` fails, the
/// plan is recomputed for the slots that came up instead of dropping the
/// players silently.
fn plan_with_realized_slots(
    ready: usize,
    max_team: usize,
    min_team: usize,
    max_games: usize,
    parallel: bool,
) -> Vec<usize> {
    if !parallel {
        return plan_team_sizes(ready, max_team, min_team, 1);
    }
    let mut capacity = max_games.max(1);
    loop {
        let plan = plan_team_sizes(ready, max_team, min_team, capacity);
        if plan.len() <= 1 {
            return plan;
        }
        let mut realized = 1; // the main game
        for id in 1..plan.len() {
            if tffa::realize_instance(id as u8).is_err() {
                break;
            }
            realized += 1;
        }
        if realized >= plan.len() {
            return plan;
        }
        eprintln!(
            "----- proxy-rs: team shuffle could only realize {realized} of {} match slots; replanning",
            plan.len()
        );
        capacity = realized;
    }
}

/// Prepare one slot for the shuffle (fresh score, no password, no remembered
/// roster) and return every assignment `(client, slot, team)`.
fn assign_games(order: &[usize], plan: &[usize]) -> Vec<(c_int, u8, i32)> {
    let mut placed = Vec::with_capacity(plan.iter().sum::<usize>() * 2);
    let mut cursor = 0;
    for (index, team_size) in plan.iter().copied().enumerate() {
        let id = index as u8;
        crate::teamlock::clear_match(id);
        state::with_state(|s| {
            let m = &mut s.tffa.matches[id as usize];
            m.active = true;
            m.red = 0;
            m.blue = 0;
            m.end_queued_at = 0;
            m.remembered = [crate::instances::REMEMBERED_NONE; MAX_CLIENTS];
            m.clear_password();
            if id == crate::instances::MAIN_MATCH {
                s.tffa.main_origin = crate::instances::MAIN_MATCH;
            }
        });
        for team in [TEAM_RED, TEAM_BLUE] {
            for _ in 0..team_size {
                if let Some(&slot) = order.get(cursor) {
                    placed.push((slot as c_int, id, team));
                    cursor += 1;
                }
            }
        }
    }

    state::with_state(|s| {
        for &(client_num, id, _) in &placed {
            crate::instances::assign_client_instance(
                &mut s.tffa.client_match,
                &mut s.tffa.home,
                &mut s.tffa.player_score,
                &mut s.clients,
                client_num,
                id,
            );
            s.tffa.pass_ok[client_num as usize] = crate::instances::MAIN_MATCH;
        }
    });

    for &(client_num, id, team) in &placed {
        tffa::forget_return_rosters(client_num);
        crate::instances::reset_game_scores(client_num);
        let team_name: &CStr = if team == TEAM_BLUE { c"blue" } else { c"red" };
        tffa::set_client_team(client_num, team_name);
        crate::instances::broadcast_cs_players_slot(client_num);
        crate::instances::refresh_cs_players_for_viewer(client_num);
        let color = if team == TEAM_BLUE { "^4BLUE" } else { "^1RED" };
        crate::votes::send_print(
            client_num,
            &format!("^3[Team Shuffle]^7 You are in match ^3{id}^7 on {color}^7."),
        );
    }
    tffa::on_set_cs_scores();
    placed
}

/// Fisher-Yates with `/dev/urandom`, and a clock/counter mix when the device is
/// unavailable (a shuffle needs fairness, not cryptography).
fn shuffle_order(order: &mut [usize]) {
    let mut bytes = vec![0u8; order.len().saturating_mul(4)];
    let have_entropy = crate::challenge::fill_random(&mut bytes);
    for index in (1..order.len()).rev() {
        let random = if have_entropy {
            u32::from_le_bytes([
                bytes[index * 4],
                bytes[index * 4 + 1],
                bytes[index * 4 + 2],
                bytes[index * 4 + 3],
            ])
        } else {
            syscall::milliseconds()
                .wrapping_add((index as u32).wrapping_mul(0x9E37_79B9))
                .rotate_left(13)
        };
        let swap = random as usize % (index + 1);
        order.swap(index, swap);
    }
}

/// Announce the result and the per-game sizes to everyone.
fn announce_result(placed: &[(c_int, u8, i32)], plan: &[usize], ready_count: usize) {
    let total_placed = placed.len();
    let spectators = ready_count.saturating_sub(total_placed);
    let sizes = plan
        .iter()
        .map(|size| format!("{size}v{size}"))
        .collect::<Vec<_>>()
        .join(", ");
    crate::votes::send_print(
        -1,
        &format!(
            "^3[Team Shuffle]^7 {total_placed} players in {} match(es): {sizes}. {spectators} spectator(s).",
            plan.len()
        ),
    );
    center_all(&format!(
        "TEAM SHUFFLE COMPLETE: {} players in {} match(es): {sizes}.",
        total_placed,
        plan.len()
    ));
    eprintln!(
        "----- proxy-rs: team shuffle placed {total_placed} players in {} match(es): {sizes} ({spectators} spectator(s))",
        plan.len()
    );
}

fn center_all(text: &str) {
    send_center(-1, text);
}

fn center_one(client_num: c_int, text: &str) {
    send_center(client_num, text);
}

/// `cp "..."`: the cgame's centre print, for one client or every client.
fn send_center(client_num: c_int, text: &str) {
    let safe = text.replace('"', "'");
    let Ok(message) = std::ffi::CString::new(format!("cp \"{safe}\"")) else {
        return;
    };
    // SAFETY: engine syscall registered; `message` is NUL-terminated.
    unsafe { syscall::send_server_command(client_num, &message) };
}

/// Clear a slot's `/ready` when its client disconnects or a new client takes
/// the slot, so a stale ready flag cannot make a newcomer play unasked.
pub fn on_client_connect(client_num: c_int, first_time: bool) {
    if first_time {
        clear_ready(client_num);
    }
}

/// See [`on_client_connect`].
pub fn on_client_disconnect(client_num: c_int) {
    clear_ready(client_num);
}

/// The master switch was turned off: drop a running ready window, so turning
/// the proxy back on cannot place players from a long-expired vote.
pub fn on_proxy_disabled() {
    state::with_state(|s| s.shuffle.clear());
}

fn clear_ready(client_num: c_int) {
    if !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
    state::with_state(|s| s.shuffle.ready[client_num as usize] = false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_the_shuffle_vote_only() {
        assert!(is_shuffle_vote(b"callvote", b"shuffle"));
        assert!(is_shuffle_vote(b"CallVote", b"Shuffle"));
        assert!(!is_shuffle_vote(b"vote", b"shuffle"));
        assert!(!is_shuffle_vote(b"callvote", b"shuffled"));
        assert!(!is_shuffle_vote(b"callvote", b""));
    }

    #[test]
    fn team_size_arguments_are_strict_positive_decimals() {
        assert_eq!(parse_team_size(b"4"), Some(4));
        assert_eq!(parse_team_size(b" 12 "), Some(12));
        assert_eq!(parse_team_size(b""), None);
        assert_eq!(parse_team_size(b"0"), Some(0));
        assert_eq!(parse_team_size(b"-4"), None);
        assert_eq!(parse_team_size(b"4x"), None);
        assert_eq!(parse_team_size(b"+4"), None);
        assert_eq!(
            parse_team_size(b"999999999999999999999999"),
            None,
            "overflow is refused, not wrapped"
        );
    }

    #[test]
    fn plans_match_the_requested_examples() {
        // 15 ready, 4 per team: one 4v4 and one 3v3, one spectator.
        assert_eq!(plan_team_sizes(15, 4, 2, 9), vec![4, 3]);
        // 20 ready, 4 per team: 4v4 + 3v3 + 3v3, nobody spectating.
        assert_eq!(plan_team_sizes(20, 4, 2, 9), vec![4, 3, 3]);
        // 12 ready, 4 per team: 3v3 + 3v3, not 4v4 + 2v2.
        assert_eq!(plan_team_sizes(12, 4, 2, 9), vec![3, 3]);
        // 6 ready, 4 per team: one 3v3.
        assert_eq!(plan_team_sizes(6, 4, 2, 9), vec![3]);
    }

    #[test]
    fn the_odd_player_is_the_only_spectator() {
        // 19 ready: 18 can play, the balanced split is 3v3 + 3v3 + 3v3.
        assert_eq!(plan_team_sizes(19, 4, 2, 9), vec![3, 3, 3]);
        // 5 ready is one 2v2 plus one spectator.
        assert_eq!(plan_team_sizes(5, 4, 2, 9), vec![2]);
    }

    #[test]
    fn the_minimum_team_size_blocks_tiny_games() {
        assert!(plan_team_sizes(3, 4, 2, 9).is_empty(), "no 1v1 by default");
        assert!(plan_team_sizes(2, 4, 2, 9).is_empty());
        assert!(plan_team_sizes(4, 4, 2, 9).contains(&2));
        assert_eq!(plan_team_sizes(2, 4, 1, 9), vec![1]);
        assert_eq!(plan_team_sizes(4, 4, 1, 9), vec![2]);
        assert_eq!(plan_team_sizes(4, 4, 3, 9), Vec::<usize>::new());
        // 10 ready cannot split into teams of 3..4 (5 per side is 3+2), so
        // the largest valid placement is a 4v4 and two spectators.
        assert_eq!(plan_team_sizes(10, 4, 3, 9), vec![4]);
        assert_eq!(plan_team_sizes(14, 4, 3, 9), vec![4, 3]);
    }

    #[test]
    fn without_parallel_instances_only_the_main_game_is_filled() {
        // TFFA off: one game, the rest of the ready players stay spectators.
        assert_eq!(plan_team_sizes(20, 4, 2, 1), vec![4]);
        assert_eq!(plan_team_sizes(6, 4, 2, 1), vec![3]);
        assert_eq!(plan_team_sizes(15, 4, 2, 1), vec![4]);
    }

    #[test]
    fn slot_and_capacity_limits_are_respected() {
        // No game slot at all.
        assert!(plan_team_sizes(8, 4, 2, 0).is_empty());
        // Two slots cap the placement at 16 players (4v4 + 4v4).
        assert_eq!(plan_team_sizes(20, 4, 2, 2), vec![4, 4]);
        // A team larger than the vote is never formed.
        assert_eq!(plan_team_sizes(40, 4, 2, 9), vec![4, 4, 4, 4, 4]);
    }

    #[test]
    fn a_sixteen_player_shuffle_is_four_v_four() {
        assert_eq!(plan_team_sizes(16, 4, 2, 9), vec![4, 4]);
        assert_eq!(plan_team_sizes(10, 4, 2, 9), vec![3, 2]);
    }

    #[test]
    fn the_deadline_compare_survives_the_millisecond_wrap() {
        assert!(deadline_reached(1000, 1000));
        assert!(deadline_reached(1001, 1000));
        assert!(!deadline_reached(999, 1000));
        let deadline = u32::MAX - 10;
        assert!(!deadline_reached(u32::MAX - 110, deadline));
        assert!(deadline_reached(20, deadline));
        assert_eq!(remaining_ms(0, 30_000), 30_000);
        assert_eq!(remaining_ms(30_000, 30_000), 0);
        assert_eq!(remaining_ms(30_001, 30_000), 0);
    }
}
