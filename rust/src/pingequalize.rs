//! Server-side ping equalisation (`proxy_sv_pingEqualize`,
//! `proxy_sv_pingEqualizeLog`) — D-028.
//!
//! A player's usercmd `serverTime` is a stamp on the client's clock, and the
//! server processes the move when it arrives. To make everyone play at the same
//! **effective** latency, the proxy therefore has to make the game process a
//! fast player's commands later: it **holds usercmds in a per-client queue** and
//! hands the game the oldest held command when a newer one arrives. The target
//! is the slowest in-game player's `cl->ping`, capped by
//! `proxy_sv_pingEqualize`; a faster player's queue is sized to `target - ping`.
//! A player already slower than the target (or than the cap) is left alone: the
//! server can only add delay, never remove it.
//!
//! The delay must **not** be implemented by rewriting `serverTime` (the D-015
//! `cl_timeNudge` path): processing the move now while stamping it in the past
//! desynchronises `ps.commandTime` from the state it describes, and
//! `CG_PredictPlayerState` then replays commands the server has already applied
//! (`cg_predict.c:1170-1180`) — the client overshoots and snaps back every
//! snapshot. A real high-ping connection has no such mismatch, and neither does
//! the queue: the state and `ps.commandTime` both refer to the released command.
//! The first implementation shifted the stamp and a real-client test showed the
//! result (D-028.8).
//!
//! What is equalised is the input path: the game processes a fast player's
//! moves as late as a slow player's, so both see the same age on the world
//! state that was current when the command was made. The **view** path is not
//! delayed (no snapshot hold): the fast player still sees fresher snapshots.
//!
//! ## Population
//!
//! Members are engine `client_t`s in `CS_ACTIVE`, not bots, with a usable
//! `cl->ping` (`0..999`; `999` is the engine's "no reading" sentinel, while a
//! localhost/LAN 0 is a real reading) and a game session team other than
//! `TEAM_SPECTATOR`. Spectators, connecting clients and bots are ignored and
//! never padded.
//!
//! ## Per parallel match
//!
//! The target is computed **per instance**, using the instancing core's
//! `client_match` table (slot `0` is the main match — everyone while the
//! parallel features are off). Each parallel TFFA/duel match equalises its own
//! roster, so a slow player in one match never pads the players of another;
//! a match with a single player has nothing to equalise. The cap stays a
//! server-wide cvar.
//!
//! ## Tracking
//!
//! The target is recomputed from `cl->ping` at most every
//! [`REFRESH_MS`] (the engine's own value is a 32-snapshot average,
//! `SV_CalcPings`, so it is already smooth) and moves only when it differs by
//! at least [`TARGET_DEADBAND_MS`]. A change of the member set (join, leave,
//! spectator, return to play) or of the configured cap retargets immediately.
//!
//! ## Queue ramp and off semantics
//!
//! The delay is quantised to the client's own command interval (the queue depth
//! is `target / interval`), and the depth moves at most one command per
//! [`RAMP_STEP_MS`]: each step holds or releases one command, i.e. a pause or
//! catch-up of one command interval, never a burst. Setting
//! `proxy_sv_pingEqualize` to `0` sets every target to `0`; the depth ramps down
//! and the held commands drain instead of the clock jumping forward.
//! `proxy_sv_enable 0` detaches the hook layer immediately and clears the
//! queues (as for every feature behind the master switch).
//!
//! ## Display
//!
//! After the pristine `GAME_RUN_FRAME` (and before `SV_SendClientMessages`
//! builds the snapshots), every client with a non-zero applied delay has its
//! game-side `level.clients[i].ps.ping` overwritten with `cl->ping + delay`.
//! That is the value `DeathmatchScoreboardMessage` puts in the `scores` string
//! (`g_cmds.c:52-53`); the shipped cgame has its ping column commented out
//! (`cg_scoreboard.c:245`), so it shows on clients/mods that render it. The
//! proxy's own netStatus table adds an `eff` column, and `status`/rcon
//! `status` keep the engine's real ping.
//!
//! ## Cap
//!
//! The cap is clamped to `0..=250` ms. The client replays commands from
//! `ps.commandTime` to its latest one, so the queue depth is part of a window
//! the client only backs with `CMD_BACKUP = 64` usercmds; when the snapshot's
//! `ps.commandTime` falls behind that window, `CG_PredictPlayerState` freezes
//! prediction ("exceeded PACKET_BACKUP on commands"). At `cl_maxpackets 125`
//! that window is about 512 ms *including* the round trip, so 250 ms leaves
//! margin at every packet rate. Do not raise it without a real-client
//! measurement.
//!
//! `proxy_sv_pingEqualizeLog 1` logs target changes and per-client
//! engage/release transitions. It works without enforcement: with
//! `proxy_sv_pingEqualize 0` it previews what the maximum supported cap would
//! apply (the log appends `(preview)`).

use core::sync::atomic::{AtomicI32, Ordering};

use crate::hooks::engine_sv::{clients_base, is_bot, read_i32, write_i32};
use crate::instances::TOTAL_MATCH_SLOTS;
use crate::sdk::{
    CS_ACTIVE, MAX_CLIENTS, OFFSET_CLIENT_PING, OFFSET_CLIENT_STATE, OFFSET_GCLIENT_PS,
    OFFSET_GCLIENT_SESS, OFFSET_LEVEL_MAXCLIENTS, OFFSET_PS_PING, OFFSET_SESS_SESSION_TEAM,
    SIZEOF_CLIENT_T, SIZEOF_GCLIENT, TEAM_SPECTATOR, Usercmd,
};
use crate::state;
use crate::syscall;

/// Highest enforceable cap, in milliseconds (see the module docs: the
/// `CMD_BACKUP` prediction window).
pub const MAX_CAP_MS: i32 = 250;

/// Target refresh interval. `cl->ping` is already a 32-snapshot average
/// (`SV_CalcPings`), so a faster cadence would only rescan the same value.
pub const REFRESH_MS: i32 = 500;

/// Deadband for moving the target. Ping noise below this would otherwise keep
/// every client ramping.
pub const TARGET_DEADBAND_MS: i32 = 10;

/// Lowest `cl->ping` that counts as a reading.
///
/// `0` is a real reading, not a missing one: a localhost/LAN player rounds to
/// 0 ms. Bots — the other source of a 0 in `SV_CalcPings` — are already
/// excluded by `is_bot`, so a 0 here means the fastest possible player, who
/// must be padded the most. Only [`PING_SENTINEL`] means "no reading"; this is
/// the rule the netStatus feed adopted when it dropped the original's
/// `ping >= 1` gate (D-007, `hooks/netstatus.rs`).
pub const MIN_PING: i32 = 0;

/// The engine's "no reading" sentinel (`SV_CalcPings` writes `999` when the
/// client is not active, has no gentity or has no acked frame); `cl->ping` must
/// stay strictly below it.
pub const PING_SENTINEL: i32 = 999;

/// Deepest delay, in commands. The client can only replay its last
/// `CMD_BACKUP` = 64 commands, so this stays well below that.
pub const MAX_DEPTH: i32 = 48;

/// Queue capacity, in commands (`MAX_DEPTH` plus room for a burst).
pub const QUEUE_CAP: usize = 64;

/// The depth moves at most one command per this many milliseconds of server
/// time: each step holds (or releases) one command, a pause/burst of one
/// command interval, so a target change never fires a whole queue at once.
pub const RAMP_STEP_MS: i32 = 100;

/// Bounds of the measured per-client command interval (ms).
pub const MIN_INTERVAL_MS: i32 = 1;
pub const MAX_INTERVAL_MS: i32 = 100;

/// Command interval assumed until two commands have been seen.
pub const DEFAULT_INTERVAL_MS: i32 = 20;

/// At most this many held commands are released in one usercmd call. A queue
/// that is catching up drains at this rate instead of bursting.
pub const MAX_RELEASES: usize = 4;

/// Frames of the client's 64-command replay ring that the action delay may
/// use. The rest is headroom for the commands the client creates between
/// snapshots and for jitter.
pub const RING_SAFE_FRAMES: i32 = 48;

/// Time a client predicts ahead of the snapshot it renders (one `sv_fps`
/// interval at the default 20), reserved inside [`RING_SAFE_FRAMES`].
pub const AHEAD_MS: i32 = 50;

/// Split a delay into its **view** and **action** halves.
///
/// The view half is applied by shifting the client's snapshot clock back, so
/// the client's whole clock (and its view) lags; the action half holds that
/// many milliseconds of commands in the queue. Only the action half is paid
/// for out of the client's 64-command replay ring, so it is capped by the ring
/// budget (`RING_SAFE_FRAMES` intervals minus one snapshot interval) and by
/// half the delay. The view half takes the rest.
///
/// A real connection of the same round trip has its clock shifted by the
/// one-way delay and its commands held by the other half; this reproduces
/// that, and keeps the queue inside the ring at any frame rate.
pub fn split(delay_ms: i32, interval_ms: i32) -> (i32, i32) {
    let delay = delay_ms.max(0);
    if delay == 0 {
        return (0, 0);
    }
    let interval = interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS);
    let budget = (RING_SAFE_FRAMES * interval - AHEAD_MS).max(0);
    let action = (delay / 2).min(budget);
    (delay - action, action)
}

/// Advance one client's view shift by one snapshot and return the value to
/// apply to this snapshot (0 = none). Called from the snapshot writer stub.
pub fn view_for_snapshot(client_num: usize) -> i32 {
    state::with_state(|s| {
        s.clients
            .get_mut(client_num)
            .map(|c| c.ping_equalize.step_view())
            .unwrap_or(0)
    })
}

/// A small FIFO of usercmds waiting to be processed, oldest first.
#[derive(Debug, Clone, Copy)]
pub struct CmdQueue {
    buf: [Usercmd; QUEUE_CAP],
    head: usize,
    len: usize,
}

impl Default for CmdQueue {
    fn default() -> Self {
        Self {
            buf: [Usercmd::default(); QUEUE_CAP],
            head: 0,
            len: 0,
        }
    }
}

impl CmdQueue {
    pub fn len(&self) -> usize {
        self.len
    }

    /// Append a command. A full queue drops its oldest entry (a burst larger
    /// than the delay can hold), exactly like a link that buffers and loses.
    pub fn push(&mut self, cmd: Usercmd) {
        if self.len == QUEUE_CAP {
            self.head = (self.head + 1) % QUEUE_CAP;
            self.len -= 1;
        }
        let at = (self.head + self.len) % QUEUE_CAP;
        self.buf[at] = cmd;
        self.len += 1;
    }

    pub fn pop(&mut self) -> Option<Usercmd> {
        if self.len == 0 {
            return None;
        }
        let cmd = self.buf[self.head];
        self.head = (self.head + 1) % QUEUE_CAP;
        self.len -= 1;
        Some(cmd)
    }
}

// ---------------------------------------------------------------------------
// Per-client delay state
// ---------------------------------------------------------------------------

/// Per-client ping-equalisation state.
///
/// The delay is implemented by **holding usercmds**, not by shifting their
/// stamps: the queue keeps `depth` commands, and each arriving command releases
/// the oldest held one. The game therefore processes a command one queue
/// interval later, with its original stamp, so the state and
/// `ps.commandTime` stay consistent and client prediction keeps working — a
/// stamp shift would make the client replay moves the server had already
/// applied.
#[derive(Debug, Default, Clone, Copy)]
pub struct PingEqualizeState {
    /// Delay currently in effect, in ms (`depth x interval`; `0` = none).
    pub applied: i32,
    /// Requested delay in ms (`0` = none), set by the frame pass.
    pub target: i32,
    /// View half of the delay currently applied, ms: the snapshot clock shift.
    /// Ramped one millisecond per snapshot so the client's own clock walk
    /// (`CL_AdjustTimeDelta`, +1/-2 ms per snapshot) can follow it.
    pub view_ms: i32,
    /// View half the split asks for, ms.
    pub view_target: i32,
    /// Action half of the delay, ms: the queue's target.
    pub action_ms: i32,
    /// Commands the queue currently holds on purpose.
    pub depth: i32,
    /// Commands the requested delay needs.
    pub depth_target: i32,
    /// Measured command interval (EMA), ms.
    pub interval: i32,
    /// Last raw stamp seen (`0` = none).
    pub last_stamp: i32,
    /// `svs.time` of the last depth step (`0` = none).
    pub last_step_ms: i32,
    /// `svs.time` of the last diagnostic line (`0` = none).
    pub last_debug_ms: i32,
    /// Log edge: an engage line was emitted and no release line yet.
    pub logged: bool,
    /// A real (non-preview) delay has been applied since the last release;
    /// distinguishes "the applied delay ended" from a preview-only edge.
    pub applied_seen: bool,
    /// Commands waiting to be processed, oldest first.
    pub queue: CmdQueue,
}

impl PingEqualizeState {
    fn interval_or_default(&self) -> i32 {
        if self.interval > 0 {
            self.interval
        } else {
            DEFAULT_INTERVAL_MS
        }
    }

    /// Whether the queue path must run for this client (a delay is wanted, is
    /// still in effect, or held commands are waiting).
    pub fn active(&self) -> bool {
        self.target > 0
            || self.view_ms > 0
            || self.view_target > 0
            || self.depth > 0
            || self.queue.len() > 0
    }

    /// Feed the arriving command's raw stamp and refresh the interval EMA.
    pub fn observe_stamp(&mut self, stamp: i32) {
        if self.last_stamp != 0 {
            let delta = stamp.wrapping_sub(self.last_stamp);
            if (MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&delta) {
                let prev = self.interval_or_default();
                self.interval = (prev * 7 + delta) / 8;
            }
        } else {
            self.interval = DEFAULT_INTERVAL_MS;
        }
        self.last_stamp = stamp;
        self.refresh_depth_target();
    }

    /// Recompute how many commands the requested delay needs.
    pub fn refresh_depth_target(&mut self) {
        let interval = self.interval_or_default();
        self.depth_target = if self.action_ms <= 0 {
            0
        } else {
            ((self.action_ms + interval / 2) / interval).clamp(1, MAX_DEPTH)
        };
    }

    /// Move the depth at most one command toward its target per
    /// [`RAMP_STEP_MS`], and refresh the reported delay.
    pub fn ramp(&mut self, now: i32) {
        if self.depth == self.depth_target {
            self.last_step_ms = now;
        } else if self.last_step_ms == 0
            || !(0..RAMP_STEP_MS).contains(&now.wrapping_sub(self.last_step_ms))
        {
            self.last_step_ms = now;
            if self.depth < self.depth_target {
                self.depth += 1;
            } else {
                self.depth -= 1;
            }
        }
        self.sync_applied();
    }

    /// Move the applied view shift one millisecond toward its target and
    /// return it. Called once per snapshot sent to this client.
    pub fn step_view(&mut self) -> i32 {
        self.view_ms += (self.view_target - self.view_ms).signum();
        self.sync_applied();
        self.view_ms
    }

    /// Append the arriving command to the queue.
    pub fn enqueue(&mut self, cmd: Usercmd) {
        self.queue.push(cmd);
    }

    /// How many held commands this call should release (bounded by
    /// [`MAX_RELEASES`]).
    pub fn release_budget(&self) -> usize {
        let held = self.depth.max(0) as usize;
        self.queue.len().saturating_sub(held).min(MAX_RELEASES)
    }

    /// Pop the oldest held command.
    pub fn pop(&mut self) -> Option<Usercmd> {
        let cmd = self.queue.pop();
        self.sync_applied();
        cmd
    }

    fn sync_applied(&mut self) {
        let action = if self.depth > 0 {
            self.depth * self.interval_or_default()
        } else {
            0
        };
        self.applied = self.view_ms.saturating_add(action);
    }

    /// Update the target (and the log edge) for one frame. `reported` is the
    /// delay the feature would apply (differs from `effective` only in the
    /// log-only preview, where `effective` is `0`).
    ///
    /// The engage line is emitted once the first commands are actually held;
    /// the release line only once the queue has drained, so a spectator or a
    /// disabled cap is reported when the client really stops being delayed.
    pub fn update(
        &mut self,
        effective: i32,
        reported: i32,
        ping: i32,
        global_target: i32,
        enforce: bool,
        log: bool,
    ) -> Option<ClientTransition> {
        self.target = effective;
        let (view, action) = split(effective, self.interval_or_default());
        self.view_target = view;
        self.action_ms = action;
        self.refresh_depth_target();
        self.sync_applied();
        if self.applied > 0 {
            self.applied_seen = true;
        }
        let engaged = if enforce { effective > 0 } else { reported > 0 };
        let was = self.logged;
        // While merely previewing (enforcement off), the frame where a real
        // applied delay finishes draining is still a release, even though the
        // preview immediately reports a would-be delay again. A preview that
        // never applied a delay has nothing to release.
        let preview_released =
            log && !enforce && effective == 0 && self.applied == 0 && was && self.applied_seen;
        let transition = if preview_released {
            Some(ClientTransition::Released)
        } else if log && engaged && !was {
            Some(ClientTransition::Engaged {
                delay: self.applied.max(reported),
                ping,
                target: global_target,
            })
        } else if log && !engaged && self.applied == 0 && was {
            Some(ClientTransition::Released)
        } else {
            None
        };
        if matches!(transition, Some(ClientTransition::Released)) {
            self.applied_seen = false;
        }
        // Keep the edge while a previously engaged client drains.
        self.logged = if preview_released {
            false
        } else {
            log && (engaged || (was && self.applied > 0))
        };
        transition
    }

    /// Whether the client has nothing to do (empty queue, no ramp pending).
    pub fn is_idle(&self) -> bool {
        self.applied == 0
            && self.target == 0
            && self.depth == 0
            && self.queue.len() == 0
            && !self.logged
    }

    /// Stop applying the delay: the depth ramps down and the queue drains.
    pub fn stop(&mut self) {
        self.target = 0;
    }

    /// Forget everything (connect/disconnect, `GAME_INIT`, master switch). The
    /// held commands are dropped: a fresh client starts from a fresh clock.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The ping the scoreboard should report while this delay is applied.
    pub fn effective_ping(&self, ping: i32) -> i32 {
        ping.saturating_add(self.applied)
    }
}

// ---------------------------------------------------------------------------
// Cvar mirror (set once per `GAME_RUN_FRAME` by `update_proxy_cvars`)
// ---------------------------------------------------------------------------

static CAP_MS: AtomicI32 = AtomicI32::new(0);
static LOG: AtomicI32 = AtomicI32::new(0);

/// The configured cap in milliseconds (`0` = off). Already clamped.
pub fn cap_ms() -> i32 {
    CAP_MS.load(Ordering::Relaxed)
}

/// Whether transitions are logged (`proxy_sv_pingEqualizeLog`).
pub fn logging() -> bool {
    log_level() >= 1
}

/// Diagnostic level (`2` adds the once-per-second per-client state line).
pub fn log_level() -> i32 {
    LOG.load(Ordering::Relaxed)
}

/// Clamp a cvar value to the supported cap range.
pub fn clamp_cap(cap: i32) -> i32 {
    cap.clamp(0, MAX_CAP_MS)
}

/// Mirror the cvars into lock-free atomics.
///
/// Turning the cap to `0` does **not** clear the applied delays: the targets
/// are set to `0` by the next [`on_run_frame`], and the depth ramp drains the
/// held commands instead of dropping them (D-028.8).
pub fn set_config(cap: i32, log: i32) {
    CAP_MS.store(clamp_cap(cap), Ordering::Relaxed);
    LOG.store(log.max(0), Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Target policy (pure)
// ---------------------------------------------------------------------------

/// `min(cap, max ping)`, or `0` when there is no member (or the cap is off).
///
/// `pings` holds the member pings only; spectators/bots/invalid readings are
/// filtered out by the caller.
pub fn target_for(pings: &[i32], cap: i32) -> i32 {
    let cap = clamp_cap(cap);
    if cap <= 0 {
        return 0;
    }
    match pings.iter().copied().max() {
        Some(max) if max > 0 => max.min(cap),
        _ => 0,
    }
}

/// The delay one member needs to reach `target`: `max(0, target - ping)`.
pub fn delay_for(ping: i32, target: i32) -> i32 {
    if target <= 0 {
        return 0;
    }
    (target - ping).max(0)
}

/// Whether a client belongs to the equalisation population (D-028.4): engine
/// `CS_ACTIVE`, not a bot, a usable `cl->ping` (`0..999`) and a game session
/// team other than `TEAM_SPECTATOR`. Spectators, connecting clients and bots
/// are ignored and never padded.
pub fn is_member(client_state: i32, bot: bool, ping: i32, session_team: i32) -> bool {
    client_state == CS_ACTIVE
        && !bot
        && (MIN_PING..PING_SENTINEL).contains(&ping)
        && session_team != TEAM_SPECTATOR
}

/// One logged per-client transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTransition {
    Engaged { delay: i32, ping: i32, target: i32 },
    Released,
}

// ---------------------------------------------------------------------------
// Target tracking
// ---------------------------------------------------------------------------

/// One logged target change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetChange {
    /// Parallel match the change belongs to (`0` = the main match; when the
    /// parallel features are off every player is in match `0`).
    pub match_id: u8,
    pub target: i32,
    /// Engine slot of the slowest member (`None` when the roster is empty).
    pub slowest: Option<usize>,
    pub slowest_ping: i32,
    /// The slowest member is above the cap, so everybody else was pulled to
    /// the cap and the straggler keeps its own ping.
    pub capped: bool,
}

/// One parallel match's target state: member set, the ping snapshot it was
/// computed from, and the refresh/deadband bookkeeping. Slot `0` is the main
/// match — everyone when the parallel-match features are off.
#[derive(Debug, Default, Clone, Copy)]
pub struct MatchTarget {
    pub target: i32,
    /// Member mask used by the last computation.
    pub members: [bool; MAX_CLIENTS],
    /// Member pings used by the last computation (`0` for non-members).
    pings: [i32; MAX_CLIENTS],
    /// `svs.time` of the last recomputation (`0` = never).
    pub last_check_ms: i32,
    /// The cap the last computation used (also detects cap changes).
    pub cap: i32,
}

impl MatchTarget {
    /// Whether a recomputation is due (also true after the engine clock goes
    /// backwards, e.g. a map restart).
    pub fn due(&self, now: i32) -> bool {
        let elapsed = now.wrapping_sub(self.last_check_ms);
        !(0..REFRESH_MS).contains(&elapsed)
    }

    /// The latched ping of a member from the last recomputation.
    pub fn latched_ping(&self, client_num: usize) -> i32 {
        self.pings.get(client_num).copied().unwrap_or(0)
    }

    /// Recompute the target when due (or immediately on a roster/cap change).
    ///
    /// Returns the new target only when it moved, for the transition log.
    pub fn observe(
        &mut self,
        match_id: u8,
        now: i32,
        members: &[bool; MAX_CLIENTS],
        pings: &[i32; MAX_CLIENTS],
        cap: i32,
    ) -> Option<TargetChange> {
        let roster_changed = *members != self.members;
        let cap = clamp_cap(cap);
        let cap_changed = cap != self.cap;
        if !roster_changed && !cap_changed && !self.due(now) {
            return None;
        }

        let mut member_pings: Vec<i32> = Vec::new();
        let mut slowest = None;
        let mut slowest_ping = 0;
        for i in 0..MAX_CLIENTS {
            self.pings[i] = if members[i] { pings[i] } else { 0 };
            self.members[i] = members[i];
            if members[i] {
                member_pings.push(pings[i]);
                if slowest.is_none() || pings[i] > slowest_ping {
                    slowest = Some(i);
                    slowest_ping = pings[i];
                }
            }
        }

        let new_target = target_for(&member_pings, cap);
        self.members = *members;
        self.cap = cap;
        self.last_check_ms = now;

        let moved = new_target != self.target
            && (roster_changed
                || cap_changed
                || (new_target - self.target).abs() >= TARGET_DEADBAND_MS);
        if !moved {
            return None;
        }
        self.target = new_target;
        Some(TargetChange {
            match_id,
            target: new_target,
            slowest,
            slowest_ping,
            capped: slowest.is_some() && slowest_ping > cap,
        })
    }
}

/// Per-match target tracking: the main match (slot `0`) plus every parallel
/// TFFA/duel match. Each match equalises its own roster, so a slow player in
/// one match does not pad the players of another (D-028.10).
#[derive(Debug, Default, Clone, Copy)]
pub struct TargetState {
    pub slots: [MatchTarget; TOTAL_MATCH_SLOTS],
}

impl TargetState {
    /// The target a client of `match_id` is padded toward (`0` = none).
    pub fn target_of(&self, match_id: u8) -> i32 {
        self.slots
            .get(match_id as usize)
            .map(|slot| slot.target)
            .unwrap_or(0)
    }

    /// The latched ping of one client in its match from the last refresh.
    pub fn latched_ping(&self, match_id: u8, client_num: usize) -> i32 {
        self.slots
            .get(match_id as usize)
            .map(|slot| slot.latched_ping(client_num))
            .unwrap_or(0)
    }

    /// Recompute every match that is due; push one change per match whose
    /// target moved (for the transition log).
    ///
    /// `member_match[i]` is client `i`'s match (`u8::MAX` for a non-member),
    /// so a match's roster is exactly its own members: a slow player in match
    /// `2` never raises match `0`'s target.
    pub fn observe_all(
        &mut self,
        now: i32,
        cap: i32,
        member_match: &[u8; MAX_CLIENTS],
        pings: &[i32; MAX_CLIENTS],
        changes: &mut Vec<TargetChange>,
    ) {
        let mut members = [false; MAX_CLIENTS];
        let mut slot_pings = [0i32; MAX_CLIENTS];
        for match_id in 0..TOTAL_MATCH_SLOTS {
            for i in 0..MAX_CLIENTS {
                let member = member_match[i] == match_id as u8;
                members[i] = member;
                slot_pings[i] = if member { pings[i] } else { 0 };
            }
            if let Some(change) =
                self.slots[match_id].observe(match_id as u8, now, &members, &slot_pings, cap)
            {
                changes.push(change);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime integration
// ---------------------------------------------------------------------------

/// Reset one client's delay (connect/disconnect, slot reuse).
pub fn reset_client(client_num: usize) {
    state::with_state(|s| {
        if let Some(client) = s.clients.get_mut(client_num) {
            client.ping_equalize.clear();
        }
    });
}

/// Drop every client's delay and the target tracking (master switch off,
/// `GAME_INIT`).
pub fn clear_offsets() {
    state::with_state(|s| {
        for client in s.clients.iter_mut() {
            client.ping_equalize.clear();
        }
        s.ping_equalize = TargetState::default();
    });
}

/// Per-`GAME_RUN_FRAME` pass (after the pristine forward, before the engine
/// builds the snapshots): refresh the target, update every client's queue
/// target, and write the effective ping into the game-side player state.
///
/// # Safety
///
/// Runs from the proxy's `GAME_RUN_FRAME` handler: the original module is
/// loaded, the engine memory layer is initialised and `GAME_INIT` has run.
pub fn on_run_frame(now: i32) {
    let cap = cap_ms();
    let log = logging();
    let log_level = log_level();
    let idle = state::with_state(|s| s.clients.iter().all(|c| c.ping_equalize.is_idle()));
    if idle && cap == 0 && !log {
        return;
    }

    let level = crate::original::level_address();
    if level == 0 {
        return;
    }
    let g_clients = state::with_state(|s| s.located_game_data.g_clients);
    if g_clients == 0 {
        return;
    }
    // SAFETY: engine memory layer initialised (hooks run after GAME_INIT).
    let engine_clients = unsafe { clients_base() };
    if engine_clients == 0 {
        return;
    }
    // SAFETY: `level` is the original module's live `level_locals_t`;
    // `maxclients` is a plain int.
    let maxclients =
        unsafe { read_i32(level + OFFSET_LEVEL_MAXCLIENTS) }.clamp(0, MAX_CLIENTS as i32) as usize;
    if maxclients == 0 {
        return;
    }

    let enforce = cap > 0;
    let computation_cap = if enforce {
        cap
    } else if log {
        // Log-only preview: what the maximum supported cap would apply.
        MAX_CAP_MS
    } else {
        0
    };

    // Each client's parallel match (the instancing core; all `0` when the
    // parallel-match features are off).
    let matches: [u8; MAX_CLIENTS] = state::with_state(|s| s.tffa.client_match);

    let mut member_match = [u8::MAX; MAX_CLIENTS];
    let mut pings = [0i32; MAX_CLIENTS];
    for i in 0..maxclients {
        let engine_cl = engine_clients + i * SIZEOF_CLIENT_T;
        // SAFETY: `engine_cl` is slot `i` of `svs->clients` (`i <
        // sv_maxclients`, the array's allocation); all three fields are plain
        // ints/pointers of the live struct.
        let client_state = unsafe { read_i32(engine_cl + OFFSET_CLIENT_STATE) };
        let bot = unsafe { is_bot(engine_cl) };
        let ping = unsafe { read_i32(engine_cl + OFFSET_CLIENT_PING) };
        // `G_LOCATE_GAME_DATA` hands the game's `&level.clients[0].ps`, and
        // `ps` is the first field of `gclient_t` (`OFFSET_GCLIENT_PS = 0`), so
        // the pointer is also the gclient_t array base. `i` is below the
        // latched `level.maxclients`.
        let team = unsafe {
            read_i32(
                g_clients + i * SIZEOF_GCLIENT + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM,
            )
        };
        if !is_member(client_state, bot, ping, team) {
            continue;
        }
        member_match[i] = matches[i].min(TOTAL_MATCH_SLOTS as u8 - 1);
        pings[i] = ping;
    }

    let mut lines: Vec<String> = Vec::new();
    state::with_state(|s| {
        let mut changes: Vec<TargetChange> = Vec::new();
        s.ping_equalize
            .observe_all(now, computation_cap, &member_match, &pings, &mut changes);
        if log {
            for change in changes {
                lines.push(target_line(change, !enforce));
            }
        }
        // The per-client delay uses the ping snapshot of the last refresh, so
        // the 500 ms/deadband rule also smooths the delay target. The display
        // value below uses this frame's fresh reading, as D-028.3 specifies.
        for i in 0..maxclients {
            let slot = member_match[i];
            let (target, latched) = if slot == u8::MAX {
                (0, 0)
            } else {
                (
                    s.ping_equalize.target_of(slot),
                    s.ping_equalize.latched_ping(slot, i),
                )
            };
            let reported = if slot != u8::MAX {
                delay_for(latched, target)
            } else {
                0
            };
            let effective = if enforce { reported } else { 0 };
            let client = &mut s.clients[i].ping_equalize;
            if !enforce {
                // D-028.8: a cap of 0 drains the queue instead of dropping
                // the held commands.
                client.stop();
            }
            if let Some(transition) =
                client.update(effective, reported, latched, target, enforce, log)
            {
                if log {
                    lines.push(client_line(i, transition));
                }
            }
            // Only a member has a meaningful ping to add the delay to: a
            // non-member's slot in `pings` is 0, which is also a real reading
            // for a localhost/LAN member, so the population is the gate here.
            if slot != u8::MAX && client.applied > 0 {
                // SAFETY: `g_clients` is the located `gclient_t` array and `i`
                // is below `level.maxclients`; `ps.ping` is a writable int.
                unsafe {
                    write_i32(
                        g_clients + i * SIZEOF_GCLIENT + OFFSET_GCLIENT_PS + OFFSET_PS_PING,
                        client.effective_ping(pings[i]),
                    );
                }
            }
            // Diagnostic level 2: the queue state once per second per client.
            if log_level >= 2 && client.active() && now.wrapping_sub(client.last_debug_ms) >= 1000 {
                client.last_debug_ms = now;
                lines.push(format!(
                    "----- proxy-rs: pingEqualize debug client={i} match={slot} target={} reported={reported} \
                     view={}/{} action={} depth={}/{} interval={} applied={} queue={} ping={}\n",
                    client.target,
                    client.view_ms,
                    client.view_target,
                    client.action_ms,
                    client.depth,
                    client.depth_target,
                    client.interval,
                    client.applied,
                    client.queue.len(),
                    latched,
                ));
            }
        }
    });

    for line in lines {
        print_line(&line);
    }
}

/// Print one log line (the engine print trap must not run under the state
/// lock).
fn print_line(line: &str) {
    // The line is built from numbers and static text only, so this cannot
    // fail; the `if let` keeps the hot path panic-free without an `expect`.
    // SAFETY: the engine syscall is registered and `text` is NUL-terminated.
    if let Ok(text) = std::ffi::CString::new(line) {
        unsafe { syscall::print(&text) };
    }
}

fn target_line(change: TargetChange, preview: bool) -> String {
    let suffix = if preview { " (preview)" } else { "" };
    match change.slowest {
        Some(client) => format!(
            "----- proxy-rs: pingEqualize target={} match={} (slowest client={client} ping={}{}){suffix}\n",
            change.target,
            change.match_id,
            change.slowest_ping,
            if change.capped { " capped" } else { "" }
        ),
        None => format!(
            "----- proxy-rs: pingEqualize target={} match={} (no players){suffix}\n",
            change.target, change.match_id
        ),
    }
}

fn client_line(client_num: usize, transition: ClientTransition) -> String {
    match transition {
        ClientTransition::Engaged {
            delay,
            ping,
            target,
        } => format!(
            "----- proxy-rs: pingEqualize client={client_num} delay={delay} ping={ping} \
             target={target}\n"
        ),
        ClientTransition::Released => {
            format!("----- proxy-rs: pingEqualize client={client_num} delay=0 (released)\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_all(
        state: &mut TargetState,
        now: i32,
        entries: &[(u8, usize, i32)],
        cap: i32,
    ) -> Vec<TargetChange> {
        let mut member_match = [u8::MAX; MAX_CLIENTS];
        let mut pings = [0i32; MAX_CLIENTS];
        for &(match_id, client, ping) in entries {
            member_match[client] = match_id;
            pings[client] = ping;
        }
        let mut changes = Vec::new();
        state.observe_all(now, cap, &member_match, &pings, &mut changes);
        changes
    }

    /// One frame with every entry in the main match (slot 0).
    fn frame(
        state: &mut TargetState,
        now: i32,
        entries: &[(usize, i32)],
        cap: i32,
    ) -> Option<TargetChange> {
        let entries: Vec<(u8, usize, i32)> = entries
            .iter()
            .map(|&(client, ping)| (0, client, ping))
            .collect();
        frame_all(state, now, &entries, cap).into_iter().next()
    }

    #[test]
    fn an_empty_roster_has_no_target() {
        assert_eq!(target_for(&[], 250), 0);
        let mut state = TargetState::default();
        assert_eq!(
            frame(&mut state, 1000, &[], 250),
            None,
            "0 -> 0 is not a target change"
        );
        // Once a player has set a target, the empty roster drops it at once.
        frame(&mut state, 2000, &[(0, 80)], 250);
        let change = frame(&mut state, 2500, &[], 250).expect("roster change retargets");
        assert_eq!(change.target, 0);
        assert_eq!(change.slowest, None);
        assert_eq!(
            frame(&mut state, 3000, &[], 250),
            None,
            "the empty state does not log every refresh"
        );
    }

    #[test]
    fn one_player_targets_their_own_ping_and_is_never_padded() {
        assert_eq!(target_for(&[80], 250), 80);
        let mut state = TargetState::default();
        let change = frame(&mut state, 1000, &[(0, 80)], 250).unwrap();
        assert_eq!(change.target, 80);
        assert_eq!(change.slowest, Some(0));
        assert!(!change.capped);
        assert_eq!(delay_for(80, change.target), 0);
    }

    #[test]
    fn the_slowest_player_sets_the_target_and_the_rest_are_padded() {
        assert_eq!(target_for(&[20, 95, 40], 250), 95);
        assert_eq!(delay_for(20, 95), 75);
        assert_eq!(delay_for(95, 95), 0);
        assert_eq!(delay_for(120, 95), 0, "a slower player is left alone");
    }

    #[test]
    fn a_player_above_the_cap_does_not_raise_the_target() {
        assert_eq!(target_for(&[20, 300], 100), 100);
        let mut state = TargetState::default();
        let change = frame(&mut state, 1000, &[(0, 20), (1, 300)], 100).unwrap();
        assert_eq!(change.target, 100);
        assert_eq!(change.slowest, Some(1));
        assert_eq!(change.slowest_ping, 300);
        assert!(change.capped);
        assert_eq!(delay_for(20, change.target), 80);
        assert_eq!(delay_for(300, change.target), 0);
    }

    #[test]
    fn the_cap_zero_is_off() {
        assert_eq!(target_for(&[20, 95], 0), 0);
        assert_eq!(delay_for(20, 0), 0);
        assert_eq!(clamp_cap(-10), 0);
        assert_eq!(clamp_cap(500), MAX_CAP_MS);
    }

    #[test]
    fn the_target_refreshes_every_500_ms_and_after_a_clock_jump() {
        let mut state = TargetState::default();
        assert!(state.slots[0].due(1000));
        state.slots[0].last_check_ms = 1000;
        assert!(!state.slots[0].due(1499));
        assert!(state.slots[0].due(1500));
        // svs.time moved backwards (map restart): refresh immediately.
        assert!(state.slots[0].due(10));
    }

    #[test]
    fn the_population_excludes_spectators_bots_and_unreadable_pings() {
        // CS_ACTIVE, human, a usable ping, playing team.
        assert!(is_member(CS_ACTIVE, false, 50, 0));
        assert!(is_member(CS_ACTIVE, false, 50, 1));
        // Spectator team.
        assert!(!is_member(CS_ACTIVE, false, 50, TEAM_SPECTATOR));
        // Bot.
        assert!(!is_member(CS_ACTIVE, true, 50, 0));
        // Still connecting (CS_CONNECTED, CS_ZOMBIE, CS_FREE, CS_PRIMED).
        for state in [0, 1, 2, 3] {
            assert!(!is_member(state, false, 50, 0));
        }
        // A zero ping is a real reading (localhost/LAN), not a missing one:
        // bots are excluded by the bot flag, so 0 ms must be padded the most.
        assert!(is_member(CS_ACTIVE, false, 0, 0));
        assert!(!is_member(CS_ACTIVE, false, MIN_PING - 1, 0));
        assert!(!is_member(CS_ACTIVE, false, PING_SENTINEL, 0));
        assert!(is_member(CS_ACTIVE, false, PING_SENTINEL - 1, 0));
    }

    #[test]
    fn a_zero_ping_localhost_player_is_padded_like_any_other() {
        assert_eq!(target_for(&[0, 60], 250), 60);
        assert_eq!(delay_for(0, 60), 60);
        // A roster of only zero-ping players has nothing to equalise.
        assert_eq!(target_for(&[0, 0], 250), 0);
        let client = PingEqualizeState {
            applied: 60,
            target: 60,
            ..Default::default()
        };
        assert_eq!(client.effective_ping(0), 60);
    }

    #[test]
    fn the_deadband_keeps_a_noisy_target_in_place() {
        let mut state = TargetState::default();
        frame(&mut state, 1000, &[(0, 50), (1, 100)], 250);
        assert_eq!(state.slots[0].target, 100);
        let quiet = frame(&mut state, 1500, &[(0, 50), (1, 105)], 250);
        assert_eq!(quiet, None, "a 5 ms rise is inside the deadband");
        assert_eq!(state.slots[0].target, 100);
        let moved = frame(&mut state, 2000, &[(0, 50), (1, 110)], 250);
        assert_eq!(moved.unwrap().target, 110, "a 10 ms rise moves it");
        assert_eq!(state.slots[0].target, 110);
    }

    #[test]
    fn the_ping_snapshot_refreshes_even_when_the_target_does_not_move() {
        let mut state = TargetState::default();
        frame(&mut state, 1000, &[(0, 50), (1, 100)], 250);
        assert_eq!(state.slots[0].latched_ping(0), 50);
        frame(&mut state, 1500, &[(0, 54), (1, 103)], 250);
        assert_eq!(state.slots[0].target, 100, "still inside the deadband");
        assert_eq!(
            state.slots[0].latched_ping(0),
            54,
            "the delay target follows the ping"
        );
        assert_eq!(state.slots[0].latched_ping(1), 103);
        assert_eq!(state.slots[0].latched_ping(2), 0);
    }

    #[test]
    fn a_roster_change_bypasses_the_deadband() {
        let mut state = TargetState::default();
        frame(&mut state, 1000, &[(0, 50), (1, 100)], 250);
        assert_eq!(state.slots[0].target, 100);
        // A fourth player joins at 105: the target would only move 5 ms, but
        // the member set changed, so it retargets immediately.
        let change = frame(&mut state, 1010, &[(0, 50), (1, 100), (2, 105)], 250);
        assert_eq!(change.unwrap().target, 105);
        assert_eq!(state.slots[0].target, 105);
    }

    #[test]
    fn a_cap_change_bypasses_the_deadband() {
        let mut state = TargetState::default();
        frame(&mut state, 1000, &[(0, 104)], 100);
        assert_eq!(state.slots[0].target, 100);
        let change = frame(&mut state, 1010, &[(0, 104)], 200);
        assert_eq!(change.unwrap().target, 104);
        assert_eq!(state.slots[0].target, 104);
        // The old target was 100, the new one 104: only the cap change makes
        // this an immediate move.
    }

    fn cmd(stamp: i32) -> Usercmd {
        Usercmd {
            server_time: stamp,
            ..Default::default()
        }
    }

    #[test]
    fn the_delay_is_split_between_the_view_clock_and_the_action_queue() {
        // 500 commands/s (2 ms): the ring allows 48 commands minus one
        // snapshot interval, and the view carries the rest.
        let (view, action) = split(150, 2);
        assert_eq!(action, RING_SAFE_FRAMES * 2 - AHEAD_MS);
        assert_eq!(view + action, 150);
        // 50 commands/s (20 ms): the ring is large in time, so half and half.
        assert_eq!(split(150, 20), (75, 75));
        // Off, and intervals at the clamp floor.
        assert_eq!(split(0, 20), (0, 0));
        assert_eq!(split(150, 0), (150, 0));
        assert_eq!(split(150, 1000), (75, 75));
    }

    #[test]
    fn the_view_shift_ramps_one_millisecond_per_snapshot() {
        let mut state = PingEqualizeState {
            view_target: 3,
            ..Default::default()
        };
        assert_eq!(state.step_view(), 1);
        assert_eq!(state.step_view(), 2);
        assert_eq!(state.step_view(), 3);
        assert_eq!(state.step_view(), 3, "clamped at the target");
        state.view_target = 0;
        assert_eq!(state.step_view(), 2);
        assert_eq!(state.step_view(), 1);
        assert_eq!(state.step_view(), 0);
        assert_eq!(state.step_view(), 0);
    }

    #[test]
    fn the_queue_depth_rounds_the_delay_to_the_client_interval() {
        let mut state = PingEqualizeState::default();
        state.observe_stamp(1000);
        state.observe_stamp(1020);
        assert_eq!(state.interval, 20);
        state.target = 100;
        state.action_ms = 100;
        state.refresh_depth_target();
        assert_eq!(state.depth_target, 5, "100 ms at one command per 20 ms");
        // One depth step per RAMP_STEP_MS, never a whole queue at once.
        state.ramp(1000);
        assert_eq!((state.depth, state.applied), (1, 20));
        state.ramp(1050);
        assert_eq!((state.depth, state.applied), (1, 20), "not due yet");
        state.ramp(1100);
        assert_eq!((state.depth, state.applied), (2, 40));
    }

    #[test]
    fn the_queue_holds_commands_and_releases_the_oldest() {
        let mut state = PingEqualizeState {
            interval: 20,
            target: 60,
            depth: 3,
            depth_target: 3,
            last_step_ms: 1,
            ..Default::default()
        };
        for stamp in [1000, 1020, 1040] {
            state.enqueue(cmd(stamp));
            assert_eq!(state.release_budget(), 0, "the queue is filling");
        }
        assert_eq!(state.queue.len(), 3);
        state.enqueue(cmd(1060));
        assert_eq!(state.release_budget(), 1);
        assert_eq!(state.pop().unwrap().server_time, 1000, "oldest first");
        assert_eq!(state.queue.len(), 3);
        assert!(state.active(), "the held commands keep the path running");

        // A full queue drops its oldest entry instead of growing unbounded.
        for stamp in (0..QUEUE_CAP).map(|i| 2000 + i as i32 * 20) {
            state.queue.push(cmd(stamp));
        }
        assert_eq!(state.queue.len(), QUEUE_CAP);
    }

    #[test]
    fn a_steady_queue_holds_the_delay_and_processes_one_command_per_arrival() {
        let mut state = PingEqualizeState {
            interval: 20,
            target: 60,
            action_ms: 60,
            ..Default::default()
        };
        state.refresh_depth_target();
        state.depth = 3;
        state.last_step_ms = 1;
        let mut released = Vec::new();
        let mut sent = Vec::new();
        for k in 0..10 {
            let stamp = 1000 + k * 20;
            state.enqueue(cmd(stamp));
            state.ramp(1_000_000); // steady: depth == depth_target, no step
            for _ in 0..state.release_budget() {
                if let Some(held) = state.pop() {
                    released.push(held.server_time);
                }
            }
            sent.push(stamp);
        }
        // The first three commands build the delay; every later arrival
        // releases exactly one, so the game is always `depth` commands behind.
        assert_eq!(released, sent[..7]);
        assert_eq!(state.queue.len(), 3);
        assert_eq!(state.applied, 60);
    }

    #[test]
    fn disabling_ramps_the_depth_down_and_drains_the_queue() {
        let mut state = PingEqualizeState {
            interval: 20,
            target: 60,
            depth: 3,
            depth_target: 3,
            applied: 60,
            last_step_ms: 1,
            logged: true,
            ..Default::default()
        };
        for stamp in [1000, 1020, 1040] {
            state.enqueue(cmd(stamp));
        }
        state.stop();
        state.refresh_depth_target();
        assert_eq!(state.depth_target, 0);
        state.ramp(10);
        assert_eq!(state.depth, 3, "a step waits RAMP_STEP_MS");
        state.ramp(200);
        assert_eq!(state.depth, 2);
        assert_eq!(state.applied, 40);
        assert_eq!(state.release_budget(), 1, "the held command drains");
        assert_eq!(state.pop().unwrap().server_time, 1000);
        assert!(!state.is_idle(), "the log edge is still set");
        state.clear();
        assert!(state.is_idle());
        assert_eq!(state.queue.len(), 0);
    }

    #[test]
    fn the_effective_ping_is_the_real_ping_plus_the_delay() {
        let state = PingEqualizeState {
            applied: 75,
            target: 75,
            ..Default::default()
        };
        assert_eq!(state.effective_ping(20), 95);
        assert_eq!(PingEqualizeState::default().effective_ping(20), 20);
    }

    #[test]
    fn engaging_and_releasing_log_once_each() {
        let mut state = PingEqualizeState::default();
        let engaged = state.update(75, 75, 20, 95, true, true).unwrap();
        assert_eq!(
            engaged,
            ClientTransition::Engaged {
                delay: 75,
                ping: 20,
                target: 95
            }
        );
        assert!(state.logged);
        assert_eq!(state.update(75, 75, 20, 95, true, true), None);
        // The target drops, but the queue is still draining: no release yet.
        state.depth = 4; // 4 x 20 ms held
        assert_eq!(state.update(0, 0, 20, 0, true, true), None);
        state.depth = 0;
        assert_eq!(
            state.update(0, 0, 20, 0, true, true),
            Some(ClientTransition::Released)
        );
        assert!(!state.logged);
    }

    #[test]
    fn the_log_alone_previews_what_would_be_applied() {
        let mut state = PingEqualizeState::default();
        // Preview: effective target 0 (nothing applied), reported 75.
        let engaged = state.update(0, 75, 20, 95, false, true).unwrap();
        assert_eq!(
            engaged,
            ClientTransition::Engaged {
                delay: 75,
                ping: 20,
                target: 95
            }
        );
        assert_eq!(state.target, 0);
        assert_eq!(state.applied, 0);
        assert_eq!(
            state.update(0, 0, 20, 0, true, true),
            Some(ClientTransition::Released)
        );
    }

    #[test]
    fn a_ramping_delay_releases_before_the_preview_reengages() {
        let mut state = PingEqualizeState::default();
        // Engaged in preview, then a real delay was applied by enforcement.
        state.update(0, 75, 20, 95, false, true).unwrap();
        state.depth = 1; // 20 ms of held commands still draining
        // Enforcement is off, the preview still reports 75: the applied delay
        // is still draining, so there is no release yet.
        assert_eq!(state.update(0, 75, 20, 95, false, true), None);
        state.depth = 0;
        assert_eq!(
            state.update(0, 75, 20, 95, false, true),
            Some(ClientTransition::Released)
        );
        // The preview reports the would-be delay again on the next frame.
        assert!(matches!(
            state.update(0, 75, 20, 95, false, true),
            Some(ClientTransition::Engaged { delay: 75, .. })
        ));
    }

    #[test]
    fn a_preview_only_client_does_not_release_and_reengage_every_frame() {
        let mut state = PingEqualizeState::default();
        assert!(state.update(0, 75, 20, 95, false, true).is_some());
        for _ in 0..5 {
            assert_eq!(
                state.update(0, 75, 20, 95, false, true),
                None,
                "a preview with no applied delay has nothing to release"
            );
        }
    }

    #[test]
    fn no_edge_state_is_kept_while_the_log_is_off() {
        let mut state = PingEqualizeState::default();
        assert_eq!(state.update(75, 75, 20, 95, true, false), None);
        assert!(!state.logged);
        // Turning the log on while the client is already engaged reports it.
        let engaged = state.update(75, 75, 20, 95, true, true);
        assert!(engaged.is_some());
    }

    #[test]
    fn each_parallel_match_equalises_its_own_roster() {
        let mut state = TargetState::default();
        let changes = frame_all(
            &mut state,
            1000,
            &[(0, 0, 40), (0, 1, 55), (2, 2, 120), (2, 3, 90)],
            250,
        );
        assert_eq!(changes.len(), 2, "one change per match");
        assert_eq!(state.slots[0].target, 55);
        assert_eq!(state.slots[2].target, 120);
        // The slow player in match 2 never pads the main match.
        assert_eq!(delay_for(40, state.slots[0].target), 15);
        assert_eq!(delay_for(90, state.slots[2].target), 30);
        // A change in one match retargets only that match.
        let changes = frame_all(
            &mut state,
            2000,
            &[(0, 0, 40), (0, 1, 55), (2, 2, 140), (2, 3, 90)],
            250,
        );
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].match_id, 2);
        assert_eq!(state.slots[0].target, 55);
        assert_eq!(state.slots[2].target, 140);
    }

    #[test]
    fn the_parallel_matches_are_independent_with_the_cap() {
        let mut state = TargetState::default();
        frame_all(
            &mut state,
            1000,
            &[(0, 0, 30), (1, 1, 400), (1, 2, 60)],
            100,
        );
        // Match 0 is alone at 30: its target is its own ping.
        assert_eq!(state.slots[0].target, 30);
        // Match 1 is capped at 100, leaving its 400 ms player alone.
        assert_eq!(state.slots[1].target, 100);
    }

    #[test]
    fn the_main_match_is_slot_zero_when_the_parallel_features_are_off() {
        // `client_match` is 0 for every client while the parallel features are
        // off, so the whole server is one roster: the old behaviour.
        let mut state = TargetState::default();
        let changes = frame_all(&mut state, 1000, &[(0, 0, 30), (0, 1, 80)], 250);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].match_id, 0);
        assert_eq!(state.slots[0].target, 80);
    }

    #[test]
    fn transition_lines_match_the_documented_shape() {
        assert_eq!(
            target_line(
                TargetChange {
                    match_id: 0,
                    target: 110,
                    slowest: Some(3),
                    slowest_ping: 180,
                    capped: true,
                },
                false
            ),
            "----- proxy-rs: pingEqualize target=110 match=0 (slowest client=3 ping=180 capped)\n"
        );
        assert_eq!(
            target_line(
                TargetChange {
                    match_id: 2,
                    target: 95,
                    slowest: Some(2),
                    slowest_ping: 95,
                    capped: false,
                },
                false
            ),
            "----- proxy-rs: pingEqualize target=95 match=2 (slowest client=2 ping=95)\n"
        );
        assert_eq!(
            target_line(
                TargetChange {
                    match_id: 0,
                    target: 0,
                    slowest: None,
                    slowest_ping: 0,
                    capped: false,
                },
                false
            ),
            "----- proxy-rs: pingEqualize target=0 match=0 (no players)\n"
        );
        assert_eq!(
            client_line(
                0,
                ClientTransition::Engaged {
                    delay: 75,
                    ping: 20,
                    target: 95
                }
            ),
            "----- proxy-rs: pingEqualize client=0 delay=75 ping=20 target=95\n"
        );
        assert_eq!(
            client_line(0, ClientTransition::Released),
            "----- proxy-rs: pingEqualize client=0 delay=0 (released)\n"
        );
    }
}
