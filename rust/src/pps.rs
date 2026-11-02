//! Per-client incoming packet-rate limiting
//! (`proxy_sv_enableClientPpsLimit`, `proxy_sv_globalMaxPps`,
//! `proxy_sv_clientPpsLog`).
//!
//! The engine calls `SV_Netchan_Process(client_t *, msg_t *)` once for every
//! sequenced UDP packet that matches a known client (address + qport), before
//! the message is parsed (`sv_main.cpp:623`). Returning `qfalse` from there is
//! exactly the engine's own "drop this packet" path for out-of-order/duplicate
//! traffic: the packet is not decoded, `lastPacketTime` is not refreshed and
//! no client message runs — the same effect as a packet lost on the wire,
//! which the netchan and the client's `cl_packetdup` redundancy already handle.
//! That is the hook point this module uses.
//!
//! ## Limit derived from the client's FPS
//!
//! A stock client sends at most one packet per generated usercmd (one per
//! rendered frame, `CL_SendCmd`), and `cl_maxpackets` can only make it send
//! *fewer*. So the client's own command rate is the legitimate upper bound on
//! its packet rate. The limiter therefore derives its limit from the client's
//! **computed FPS** — the exact same per-second usercmd count the netStatus
//! table reports (`netstatus_est::fps_pps`).
//!
//! `proxy_sv_globalMaxPps` (default `100`) is combined with the FPS as an
//! integer downscale:
//!
//! ```text
//! limit = max over f in fps-2 ..= fps+2 of
//!             min(f / ceil(f / globalMaxPps), globalMaxPps)
//! ```
//!
//! so the accepted rate is always an exact `1/K` fraction of the client's
//! rate (`K = ceil(f / globalMaxPps)`), and the FPS estimate's `±2` window
//! never lowers the limit: the highest value the formula produces over the
//! five samples wins. `144 fps` with a cap of `100` evaluates `142..=146`
//! (`71, 71, 72, 72, 73`) and enforces `73`. If the FPS cannot be trusted
//! yet, the global ceiling alone is enforced (and `0` there means fail open);
//! a global ceiling of `0` means "no ceiling" and the highest sample
//! (`fps + 2`) is used.
//!
//! This targets the interesting cheat shape: a modified client that sends more
//! packets than it produces commands (duplicated/empty packets). Packets that
//! carry genuinely new usercmds raise the measured FPS and therefore the
//! limit; that is a cmd-rate question, not a packet-rate one.
//!
//! ## Why a one-second window instead of a token bucket
//!
//! Packets are read in per-server-frame batches (`Com_EventLoop` runs before
//! `SV_Frame`, `common.cpp:1812`), so a legitimate client sending exactly
//! `cl_maxpackets` packets per second reaches us as several packets with
//! (almost) the same processing timestamp. A tight per-packet token bucket
//! would drop part of every legit batch. The limiter instead counts a client's
//! whole second and uses **that** observed rate for the next second:
//!
//! 1. the first window is pure measurement (a stock client is never filtered
//!    on connect/handshake bursts);
//! 2. every later packet is accepted/dropped according to a deterministic
//!    accumulator so the dropped packets are spread evenly through the
//!    second instead of truncating its tail.
//!
//! With `limit = 63`, a client sending 100 packets/s observes 100 in the
//! window, then the next window accepts exactly 63 and drops 37, interleaved
//! (roughly two accepted, one dropped) rather than "first 63 accepted, last 37
//! dropped".
//!
//! ## When the limit is unknown
//!
//! Until the command stream spans enough time to trust the FPS estimate
//! ([`FPS_MIN_SPAN_MS`]) the limit is `0`, meaning "fail open": packets are
//! accepted and nothing is logged. The first pps window is measurement-only
//! anyway, so this only affects the reconnect/startup edge.
//!
//! ## Logging
//!
//! `proxy_sv_clientPpsLog 1` logs **transitions**, not every window: one line
//! when a client is first seen above its limit, one line when a completed
//! window brings it back under, and another line if it starts again. A client
//! that stays on the same side of its limit is silent. It works on its own:
//! the log cvar keeps the accounting and the FPS feed running without
//! `proxy_sv_enableClientPpsLimit`, so it reports the verdict the limiter
//! *would* apply. While the cvar is `0` no transition state is kept, so
//! enabling it reports the next over-limit window instead of staying silent on
//! a stale edge.
//!
//! ## One shared over-limit verdict
//!
//! The accounting runs whenever **either** `proxy_sv_enableClientPpsLimit`,
//! `proxy_sv_enableNetStatus` or `proxy_sv_clientPpsLog` is on; enforcement
//! (returning `qfalse`) only happens when the limiter cvar is on. This lets
//! the netStatus table render
//! the *real* incoming packet rate — the netStatus record ring only counts
//! packets that carried new usercmds, so a client flooding empty/duplicate
//! packets is invisible there. The completed window is kept as a
//! [`PpsSample`], and [`PpsSample::over_limit`] is the single predicate behind
//! both the `proxy_sv_clientPpsLog` line and the hand-coloured `pps` cell in
//! the table: the two surfaces cannot disagree on whether a client used too
//! many packets. The limit is reported even while enforcement is off, so the
//! table flags an over-limit client without the limiter being active.

use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use crate::hooks::engine_sv::{client_number, svs_time};
use crate::netstatus_est;
use crate::sdk::MAX_CLIENTS;
use crate::state::{self, NetstatusState};

/// Window over which packets are counted and a rate is derived.
const WINDOW_MS: i32 = 1000;

/// A client idle for this long has no current rate: a stale window must not
/// filter the packets it sends when it comes back.
const IDLE_RESET_MS: i32 = 2 * WINDOW_MS;

/// Minimum span of the client's command clock before the FPS estimate is
/// trusted. The netStatus fps is a raw per-window count, so a partial window
/// (just connected, just resumed) reads as a lower frame rate; such a window
/// is not used for the limit.
const FPS_MIN_SPAN_MS: i32 = 900;

/// The FPS estimate is uncertain by ±2 fps. The allowed pps is the **highest**
/// value the formula produces over `fps-2 ..= fps+2`, so the uncertainty never
/// causes a below-legitimate limit.
const FPS_ESTIMATE_MARGIN: i32 = 2;

/// The FPS estimate (and therefore the allowed pps) is refreshed once a second.
const FPS_REFRESH_MS: i32 = 1000;

// ---------------------------------------------------------------------------
// Cvar mirror (set once per `GAME_RUN_FRAME` by `update_proxy_cvars`)
// ---------------------------------------------------------------------------

static ENABLED: AtomicI32 = AtomicI32::new(0);
/// Accounting gate: on when the limiter **or** the netStatus table is on (the
/// table renders this module's measured rate). Enforcement still follows
/// [`ENABLED`].
static MEASURE: AtomicI32 = AtomicI32::new(0);
static GLOBAL_MAX_PPS: AtomicI32 = AtomicI32::new(100);
static LOG: AtomicI32 = AtomicI32::new(0);

/// Set when accounting was seen off, so the next measured packet starts from
/// fresh windows instead of filtering on a pre-disable rate.
static MEASURE_WAS_OFF: AtomicBool = AtomicBool::new(true);

/// Set when enforcement was seen off while accounting ran: the off -> on edge
/// starts a fresh measurement window, so the first second of enforcement is
/// measurement-only (D-014).
static ENFORCE_WAS_OFF: AtomicBool = AtomicBool::new(true);

/// Mirror the cvars into lock-free atomics so a fully idle feature costs one
/// packet-path check. `measure` is `enableClientPpsLimit ||
/// enableNetStatus`. Values are clamped here, not at the call site.
pub fn set_config(enabled: i32, measure: i32, global_max_pps: i32, log: i32) {
    ENABLED.store(i32::from(enabled != 0), Ordering::Relaxed);
    MEASURE.store(i32::from(measure != 0), Ordering::Relaxed);
    GLOBAL_MAX_PPS.store(global_max_pps.clamp(0, 100_000), Ordering::Relaxed);
    LOG.store(i32::from(log != 0), Ordering::Relaxed);
}

/// Whether the limiter is currently dropping the excess
/// (`proxy_sv_enableClientPpsLimit` on). The netStatus table uses this to
/// decide between "just the current rate" and "rate/limit".
pub fn enforcing() -> bool {
    ENABLED.load(Ordering::Relaxed) != 0
}

// ---------------------------------------------------------------------------
// FPS estimate
// ---------------------------------------------------------------------------

/// The client's FPS **exactly as the netStatus table computes it**
/// (`netstatus_est::fps_pps`), plus the guards the limiter needs before using
/// it to decide policy:
///
/// * the newest command must be fresh (the client is still sending);
/// * the commands in the 1 s window must span at least
///   [`FPS_MIN_SPAN_MS`], otherwise a partially filled window would read as a
///   lower frame rate and create a false limit.
///
/// Returns `0` when either guard fails, meaning "fail open".
fn trusted_fps(ns: &NetstatusState, now: i32) -> i32 {
    let records: Vec<_> = ns.records().copied().collect();

    let mut newest_cmd = i32::MIN;
    let mut newest_arrival = i32::MIN;
    for record in &records {
        if record.count > 0 && record.newest > newest_cmd {
            newest_cmd = record.newest;
            newest_arrival = record.arrival;
        }
    }
    if newest_cmd == i32::MIN {
        return 0;
    }
    // A silent client keeps its last record for replay; never derive a limit
    // from a command stream that stopped (the pps window resets on idle).
    let age = now.wrapping_sub(newest_arrival);
    if !(0..=IDLE_RESET_MS).contains(&age) {
        return 0;
    }

    let mut oldest_cmd = i32::MAX;
    for record in &records {
        if record.count == 0 || record.newest + netstatus_est::FPS_WINDOW_MS < newest_cmd {
            continue;
        }
        oldest_cmd = oldest_cmd.min(record.oldest);
    }
    if oldest_cmd == i32::MAX || newest_cmd.saturating_sub(oldest_cmd) < FPS_MIN_SPAN_MS {
        return 0;
    }

    netstatus_est::fps_pps(&records).0
}

/// The integer-downscale formula for one FPS sample:
/// `min(f / ceil(f / globalMaxPps), globalMaxPps)`.
fn allowed_pps_at(fps: i32, global_max_pps: i32) -> i32 {
    let fps = fps.max(1);
    let global = global_max_pps.max(0);
    if global == 0 {
        return fps;
    }
    let divisor = fps.saturating_add(global - 1) / global;
    (fps / divisor).min(global)
}

/// The allowed pps over the FPS estimate's `±2` uncertainty: the highest value
/// the formula produces for `fps-2 ..= fps+2`.
fn allowed_pps(fps: i32, global_max_pps: i32) -> i32 {
    let center = fps.max(1);
    let low = center.saturating_sub(FPS_ESTIMATE_MARGIN).max(1);
    let high = center.saturating_add(FPS_ESTIMATE_MARGIN);
    (low..=high)
        .map(|sample| allowed_pps_at(sample, global_max_pps))
        .max()
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Per-client limiter state
// ---------------------------------------------------------------------------

/// One completed measurement window, for the `proxy_sv_clientPpsLog` line and
/// the netStatus `pps` cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PpsSample {
    /// Rate seen in the window (packets per second, normalized by its length).
    pub rate: u32,
    /// Length of the window in milliseconds (usually ~1000).
    pub window_ms: i32,
    /// Packets observed in the window (accepted + dropped).
    pub observed: u32,
    /// Packets the limiter dropped in the window.
    pub dropped: u32,
    /// FPS estimate the limit was derived from (`0` = unknown).
    pub fps: i32,
    /// Allowed packets per second (`0` = fail open).
    pub limit: i32,
}

impl PpsSample {
    /// The single over-limit verdict shared by the log line and the netStatus
    /// table: the client used more packets than the FPS-derived limit allows
    /// (or the limiter had to drop some). A `0` limit means "fail open", so it
    /// never flags.
    pub fn over_limit(&self) -> bool {
        self.limit > 0 && (self.rate > self.limit as u32 || self.dropped > 0)
    }
}

/// Per-client state: the current counting window, the last completed window
/// (kept for the shared over-limit verdict), the FPS-derived limit and the
/// accumulator that spreads drops through the current window.
#[derive(Debug, Default, Clone, Copy)]
pub struct PpsState {
    started: bool,
    window_start_ms: i32,
    last_packet_ms: i32,
    /// Packets observed in the current window (accepted + dropped).
    observed: u32,
    /// Packets dropped in the current window.
    dropped: u32,
    /// Rate of the last completed window, the rate enforced this window.
    prev_rate: u32,
    /// Drop scheduling accumulator: units are "packets per second".
    drop_credit: u64,
    /// FPS estimate used for the current limit (`0` = not measured).
    fps: i32,
    /// Packets per second allowed by the FPS/global-max formula
    /// (`0` = fail open).
    limit: i32,
    fps_valid: bool,
    fps_ms: i32,
    /// Last completed window (rate/limit/drops), shared with the netStatus
    /// table. Cleared on connect, idle reset and time discontinuities.
    last: Option<PpsSample>,
    /// `svs.time` of the last completed window.
    last_ms: i32,
    /// Whether the last completed window was over the limit: the log's
    /// edge-trigger state, so `proxy_sv_clientPpsLog` prints once when a
    /// client starts using too many packets and once when it stops.
    over: bool,
}

impl PpsState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether the FPS estimate is due for a refresh.
    pub fn fps_due(&self, now_ms: i32) -> bool {
        if !self.fps_valid {
            return true;
        }
        let elapsed = now_ms.wrapping_sub(self.fps_ms);
        !(0..FPS_REFRESH_MS).contains(&elapsed)
    }

    /// Store a fresh FPS estimate and derive the allowed pps from it.
    ///
    /// The enforced limit is the **highest** value the integer-downscale
    /// formula produces over the estimate's `±2` uncertainty:
    ///
    /// ```text
    /// limit = max over f in fps-2 ..= fps+2 of
    ///             min(f / ceil(f / globalMaxPps), globalMaxPps)
    /// ```
    ///
    /// so the accepted rate is always `f / K` for an integer `K`, and the
    /// estimate uncertainty never lowers the limit. `fps == 0` means "not
    /// measured": only the global ceiling applies (`0` there means fail open).
    /// A global ceiling of `0` means "no ceiling"; the highest sample
    /// (`fps + 2`) is then used.
    pub fn refresh_fps(&mut self, now_ms: i32, fps: i32, global_max_pps: i32) {
        self.fps = fps.max(0);
        self.limit = if self.fps == 0 {
            global_max_pps.max(0)
        } else {
            allowed_pps(self.fps, global_max_pps)
        };
        self.fps_ms = now_ms;
        self.fps_valid = true;
    }

    pub fn limit(&self) -> i32 {
        self.limit
    }

    /// Whether the last completed window was over the limit (the logging
    /// edge state).
    pub fn over(&self) -> bool {
        self.over
    }

    /// Forget the edge state without touching the measurement windows: used
    /// while `proxy_sv_clientPpsLog` is off, so turning it on reports the next
    /// over-limit window.
    pub fn clear_over(&mut self) {
        self.over = false;
    }

    /// The last completed window, if it is recent enough to describe the
    /// client's current traffic. `None` while the first window fills, after a
    /// 2 s silence (the rate would be stale) and after a reset.
    pub fn status(&self, now_ms: i32) -> Option<PpsSample> {
        let sample = self.last?;
        let age = now_ms.wrapping_sub(self.last_ms);
        (0..=IDLE_RESET_MS).contains(&age).then_some(sample)
    }

    /// Account one incoming packet against `limit` (packets/second).
    ///
    /// `enforce == false` keeps the accounting/limit (the netStatus table
    /// renders them) but never drops: it is the measurement-only mode used
    /// while `proxy_sv_enableClientPpsLimit` is off.
    ///
    /// Returns whether it may be processed, plus the sample of a window that
    /// just completed (if any). `limit == 0` disables enforcement (FPS not
    /// measured yet, or the feature just re-enabled).
    pub fn on_packet(
        &mut self,
        now_ms: i32,
        limit: i32,
        enforce: bool,
    ) -> (bool, Option<PpsSample>) {
        let mut sample = self.advance(now_ms);

        let limit = limit.max(0);
        let rate = u64::from(self.prev_rate);
        let accept = if !enforce || limit == 0 || rate <= limit as u64 {
            true
        } else {
            // Desired drop ratio = (rate - limit) / rate. Add the numerator
            // per packet; drop when the accumulator reaches one whole packet
            // of rate. This interleaves drops instead of truncating the tail
            // of the window.
            self.drop_credit += rate - limit as u64;
            if self.drop_credit >= rate {
                self.drop_credit -= rate;
                false
            } else {
                true
            }
        };

        self.observed = self.observed.saturating_add(1);
        if !accept {
            self.dropped = self.dropped.saturating_add(1);
        }
        if let Some(sample) = sample.as_mut() {
            sample.fps = self.fps;
            sample.limit = limit;
            self.last = Some(*sample);
            self.last_ms = now_ms;
            self.over = sample.over_limit();
        }
        (accept, sample)
    }

    /// Advance/roll the measurement window and return the sample of a window
    /// that completed naturally.
    fn advance(&mut self, now_ms: i32) -> Option<PpsSample> {
        if !self.started {
            self.started = true;
            self.window_start_ms = now_ms;
            self.last_packet_ms = now_ms;
            return None;
        }

        let since_last = now_ms.wrapping_sub(self.last_packet_ms);
        if since_last < 0 {
            // Engine time went backwards (map restart / wrap): never filter on
            // a window that spans the discontinuity, and discard its partial
            // count instead of reporting a bogus rate. The log edge state is
            // bookkeeping, not a rate: keep it so a restart does not duplicate
            // an "over limit" line (and still reports the "back under" one).
            *self = Self {
                started: true,
                window_start_ms: now_ms,
                last_packet_ms: now_ms,
                over: self.over,
                ..Self::default()
            };
            return None;
        }

        if since_last >= IDLE_RESET_MS {
            // Idle across two whole windows: drop the stale rate (a returning
            // player must not be filtered on what they sent before they went
            // quiet) and start a fresh window with this packet. Keep the log
            // edge state: when the returning player completes an under-limit
            // window, the "back under limit" line is still due.
            *self = Self {
                started: true,
                window_start_ms: now_ms,
                last_packet_ms: now_ms,
                over: self.over,
                ..Self::default()
            };
            return None;
        }

        self.last_packet_ms = now_ms;
        let elapsed = now_ms.wrapping_sub(self.window_start_ms);
        if elapsed < WINDOW_MS {
            return None;
        }

        let elapsed = elapsed.max(1) as u64;
        let rate = ((u64::from(self.observed) * 1000) / elapsed).min(u64::from(u32::MAX)) as u32;
        let sample = PpsSample {
            rate,
            window_ms: elapsed.min(i32::MAX as u64) as i32,
            observed: self.observed,
            dropped: self.dropped,
            fps: self.fps,
            limit: self.limit,
        };
        self.prev_rate = rate;
        self.observed = 0;
        self.dropped = 0;
        self.drop_credit = 0;
        self.window_start_ms = now_ms;
        Some(sample)
    }
}

/// Reset one client's limiter (fresh connect / slot reuse).
pub fn reset_client(client_num: usize) {
    state::with_state(|s| {
        if let Some(client) = s.clients.get_mut(client_num) {
            client.pps.reset();
        }
    });
}

/// Reset every client's limiter (measurement or enforcement (re)started).
pub fn reset_all() {
    state::with_state(|s| {
        for client in s.clients.iter_mut() {
            client.pps.reset();
        }
    });
}

/// The shared over-limit verdict for one client (see [`PpsSample`]): the same
/// completed window the limiter logs and enforces from. `None` while the first
/// window fills or when the traffic is stale.
pub fn status(client_num: usize, now_ms: i32) -> Option<PpsSample> {
    state::with_state(|s| {
        s.clients
            .get(client_num)
            .and_then(|client| client.pps.status(now_ms))
    })
}

// ---------------------------------------------------------------------------
// Packet hook entry
// ---------------------------------------------------------------------------

/// Account one incoming packet for `cl` and decide whether it may be
/// processed. Called by the `SV_Netchan_Process` entry wrapper.
///
/// `true` = hand the packet to the pristine function, `false` = drop it.
/// Accounting runs while either the limiter or the netStatus table needs it;
/// packets are only dropped while the limiter cvar is on.
///
/// # Safety
///
/// `cl` must be the valid `client_t*` the engine passed to
/// `SV_Netchan_Process`.
pub unsafe fn on_incoming_packet(cl: usize) -> bool {
    if MEASURE.load(Ordering::Relaxed) == 0 {
        MEASURE_WAS_OFF.store(true, Ordering::Relaxed);
        return true;
    }
    let enforce = ENABLED.load(Ordering::Relaxed) != 0;
    if MEASURE_WAS_OFF.swap(false, Ordering::Relaxed) {
        reset_all();
        ENFORCE_WAS_OFF.store(!enforce, Ordering::Relaxed);
    } else if enforce {
        if ENFORCE_WAS_OFF.swap(false, Ordering::Relaxed) {
            // Enforcement started while accounting already ran: keep D-014's
            // measurement-only first second instead of filtering on an old
            // rate.
            reset_all();
        }
    } else {
        ENFORCE_WAS_OFF.store(true, Ordering::Relaxed);
    }

    // A stale/foreign pointer (e.g. after a disconnect) must fail open rather
    // than touch an out-of-range slot in the move-packet hot path.
    let Some(client_num) = client_number(cl) else {
        return true;
    };
    if client_num >= MAX_CLIENTS {
        return true;
    }

    let global_max_pps = GLOBAL_MAX_PPS.load(Ordering::Relaxed);
    let log = LOG.load(Ordering::Relaxed) != 0;
    // SAFETY: engine memory layer initialised (hooks run after GAME_INIT).
    let now = unsafe { svs_time() };
    let (accept, sample, was_over) = state::with_state(|s| {
        let client = &mut s.clients[client_num];
        if !log {
            // No history is kept while logging is off: turning it on must
            // report the next over-limit window, not a stale edge.
            client.pps.clear_over();
        }
        if client.pps.fps_due(now) {
            let fps = trusted_fps(&client.netstatus, now);
            client.pps.refresh_fps(now, fps, global_max_pps);
        }
        let was_over = client.pps.over();
        let limit = client.pps.limit();
        let (accept, sample) = client.pps.on_packet(now, limit, enforce);
        (accept, sample, was_over)
    });

    if log {
        if let Some(sample) = sample {
            // Edge-triggered: one line when the client starts using too many
            // packets, one when a completed window brings it back under, and
            // another if it starts again. Staying on the same side is silent.
            let over = sample.over_limit();
            if over != was_over {
                print_transition(cl, sample, over);
            }
        }
    }
    accept
}

/// Print one over-limit / back-under-limit transition after the state lock is
/// released (the engine print trap must not run under the proxy mutex).
fn print_transition(cl: usize, sample: PpsSample, over: bool) {
    // SAFETY: cl is the client_t* whose packet we just accounted.
    let label = unsafe { crate::hooks::engine_sv::client_label(cl) };
    let line = transition_line(sample, over, &label);
    match std::ffi::CString::new(line) {
        // SAFETY: engine syscall pointer registered by dllEntry; text is
        // NUL-terminated and outlives the call.
        Ok(text) => unsafe { crate::syscall::print(&text) },
        // The formatted line never contains a NUL; keep the fallback visible
        // rather than silently dropping a measurement.
        Err(err) => eprintln!(
            "----- proxy-rs: pps log line contained a NUL byte: {}",
            String::from_utf8_lossy(&err.into_vec())
        ),
    }
}

/// The text of one transition line: only the rates and the client identity.
/// The raw observed/accepted/dropped counters are limiter internals, not
/// operator information.
fn transition_line(sample: PpsSample, over: bool, label: &str) -> String {
    let what = if over {
        "pps over limit"
    } else {
        "pps back under limit"
    };
    format!(
        "----- proxy-rs: {what}: rate={} limit={} fps={} window_ms={} {label}\n",
        sample.rate, sample.limit, sample.fps, sample.window_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `count` packets `step_ms` apart starting at `start_ms`; returns the
    /// accept decisions.
    fn feed(
        state: &mut PpsState,
        start_ms: i32,
        count: u32,
        step_ms: i32,
        limit: i32,
    ) -> Vec<bool> {
        feed_with(state, start_ms, count, step_ms, limit, true)
    }

    /// [`feed`] with an explicit enforcement flag (`false` = measurement only).
    fn feed_with(
        state: &mut PpsState,
        start_ms: i32,
        count: u32,
        step_ms: i32,
        limit: i32,
        enforce: bool,
    ) -> Vec<bool> {
        (0..count)
            .map(|i| {
                let now = start_ms + (i as i32) * step_ms;
                state.on_packet(now, limit, enforce).0
            })
            .collect()
    }

    /// A netStatus record stream with one cmd per packet at `fps` commands per
    /// second, covering a full second.
    fn netstatus_at(fps: i32) -> NetstatusState {
        let mut ns = NetstatusState::default();
        for i in 0..fps {
            let t = i * 1000 / fps;
            ns.mark_packet_start(t, 0, 0);
            ns.feed_cmd(t);
        }
        // Commit the last open packet.
        ns.mark_packet_start(1000, 0, 0);
        ns
    }

    #[test]
    fn fps_is_the_netstatus_value_with_guards() {
        assert_eq!(trusted_fps(&NetstatusState::default(), 0), 0);
        // The raw per-window count `fps_pps` uses: one cmd per packet.
        assert_eq!(trusted_fps(&netstatus_at(60), 1000), 60);
        assert_eq!(trusted_fps(&netstatus_at(125), 1000), 125);
    }

    #[test]
    fn a_stale_command_stream_is_not_trusted() {
        let ns = netstatus_at(60);
        // The newest committed record arrived before 990 ms; reading the
        // stream long after it stopped must fail open.
        assert_eq!(trusted_fps(&ns, 10_000), 0);
    }

    #[test]
    fn a_short_command_stream_is_not_trusted() {
        let mut ns = NetstatusState::default();
        for i in 0..3 {
            ns.mark_packet_start(i * 50, 0, 0);
            ns.feed_cmd(i * 50);
        }
        ns.mark_packet_start(200, 0, 0);
        assert_eq!(trusted_fps(&ns, 200), 0, "partial window must fail open");
    }

    #[test]
    fn allowed_pps_uses_the_highest_value_over_the_fps_margin() {
        // The formula itself (single sample).
        assert_eq!(allowed_pps_at(144, 100), 72);
        assert_eq!(allowed_pps_at(146, 100), 73);
        assert_eq!(allowed_pps_at(200, 100), 100);
        assert_eq!(allowed_pps_at(201, 100), 67);

        // ±2 window: every sample 142..=146, highest wins (146 -> 73).
        assert_eq!(allowed_pps(144, 100), 73);
        // 58..=62 all use K = 1, so the highest sample (62) wins.
        assert_eq!(allowed_pps(60, 100), 62);
        // The window 98..=102 includes 100, whose K = 1 gives 100.
        assert_eq!(allowed_pps(100, 100), 100);
        // 99..=103 includes 100 as well.
        assert_eq!(allowed_pps(101, 100), 100);
        // 248..=252: K = 3 for all; 252 / 3 = 84 is the highest.
        assert_eq!(allowed_pps(250, 100), 84);
        // No ceiling: the highest sample (fps + 2) wins.
        assert_eq!(allowed_pps(200, 0), 202);
        assert_eq!(allowed_pps(1, 0), 3);
    }

    #[test]
    fn refresh_fps_handles_unknown_fps_and_the_ceiling() {
        let mut s = PpsState::default();
        s.refresh_fps(0, 144, 100);
        assert_eq!(s.limit(), 73);
        s.refresh_fps(0, 0, 100);
        assert_eq!(s.limit(), 100);
        s.refresh_fps(0, 0, 0);
        assert_eq!(s.limit(), 0);
    }

    #[test]
    fn fps_refreshes_once_a_second() {
        let mut s = PpsState::default();
        assert!(s.fps_due(0));
        s.refresh_fps(0, 60, 100);
        assert!(!s.fps_due(999));
        assert!(s.fps_due(1000));
    }

    #[test]
    fn first_window_is_measurement_only() {
        let mut s = PpsState::default();
        let accepted = feed(&mut s, 0, 200, 10, 63);
        // 2 s at 100 pps: the first window is never filtered; the second is.
        assert!(accepted[..100].iter().all(|&a| a));
        let second = &accepted[100..];
        assert_eq!(second.iter().filter(|&&a| a).count(), 63);
    }

    #[test]
    fn at_the_limit_everything_passes() {
        let mut s = PpsState::default();
        assert!(feed(&mut s, 0, 63, 15, 63).iter().all(|&a| a));
        assert!(feed(&mut s, 1000, 63, 15, 63).iter().all(|&a| a));
    }

    #[test]
    fn one_over_the_limit_is_filtered() {
        let mut s = PpsState::default();
        assert!(feed(&mut s, 0, 67, 14, 63).iter().all(|&a| a));
        let second = feed(&mut s, 1000, 67, 14, 63);
        assert_eq!(second.iter().filter(|&&a| a).count(), 63);
    }

    #[test]
    fn drops_are_spread_not_the_tail() {
        let mut s = PpsState::default();
        let first = feed(&mut s, 0, 100, 10, 63);
        assert!(first.iter().all(|&a| a));
        let second = feed(&mut s, 1000, 100, 10, 63);
        assert_eq!(second.iter().filter(|&&a| a).count(), 63);
        // The tail is not truncated: the last third still carries accepted
        // packets, and no long run of accepts precedes the first drop.
        assert!(second[67..].iter().any(|&a| a));
        assert!(second[..10].iter().any(|&a| !a), "first drop is early");
        let mut run = 0usize;
        let mut longest = 0usize;
        for &a in &second {
            run = if a { run + 1 } else { 0 };
            longest = longest.max(run);
        }
        assert!(longest <= 3, "accept run {longest} is not spread");
    }

    #[test]
    fn sample_reports_when_a_client_is_over_its_limit() {
        let mut s = PpsState::default();
        s.refresh_fps(0, 60, 100);
        let limit = s.limit();
        let mut sample = None;
        // 100 packets over the first second, then the packet that rolls the
        // window (the roll happens before that packet is counted).
        for i in 0..=100 {
            let (_, s2) = s.on_packet(i * 10, limit, true);
            if let Some(s2) = s2 {
                sample = Some(s2);
            }
        }
        let sample = sample.expect("a window completed");
        assert_eq!(sample.rate, 100);
        assert_eq!(sample.limit, 62);
        assert_eq!(sample.fps, 60);
        assert!(sample.rate > sample.limit as u32, "must be over the limit");
    }

    #[test]
    fn a_client_inside_its_limit_is_silent() {
        let mut s = PpsState::default();
        s.refresh_fps(0, 60, 100);
        let limit = s.limit();
        let mut sample = None;
        // 60 packets over ~1 s (17 ms apart), then the rolling packet.
        for i in 0..=60 {
            let (_, s2) = s.on_packet(i * 17, limit, true);
            if let Some(s2) = s2 {
                sample = Some(s2);
            }
        }
        let sample = sample.expect("a window completed");
        assert_eq!(sample.rate, 58);
        assert!(sample.rate <= sample.limit as u32);
    }

    #[test]
    fn idle_gap_resets_the_stale_rate() {
        let mut s = PpsState::default();
        assert!(feed(&mut s, 0, 100, 10, 63).iter().all(|&a| a));
        // Five seconds of silence, then a fresh 100 pps second: it must be
        // measured again, not filtered on the pre-idle window.
        let after = feed(&mut s, 6000, 100, 10, 63);
        assert!(after.iter().all(|&a| a), "post-idle window was filtered");
    }

    #[test]
    fn time_going_backwards_discards_the_window() {
        let mut s = PpsState::default();
        assert!(feed(&mut s, 5000, 100, 10, 63).iter().all(|&a| a));
        // Engine time discontinuity (map restart / wrap): fail open.
        let (accept, sample) = s.on_packet(10, 63, true);
        assert!(accept);
        assert!(sample.is_none());
    }

    #[test]
    fn measurement_without_enforcement_never_drops_but_status_flags_the_excess() {
        let mut s = PpsState::default();
        s.refresh_fps(0, 60, 100);
        let limit = s.limit();
        // 100 pps over a second, enforcement off: every packet passes ...
        let accepted = feed_with(&mut s, 0, 101, 10, limit, false);
        assert!(accepted.iter().all(|&a| a));
        // ... and the completed window reports the same over-limit verdict the
        // limiter's log line uses.
        let status = s.status(1000).expect("a window completed");
        assert_eq!(status.rate, 100);
        assert_eq!(status.limit, limit);
        assert_eq!(status.dropped, 0);
        assert!(status.over_limit());
    }

    #[test]
    fn inside_the_limit_status_does_not_flag() {
        let mut s = PpsState::default();
        s.refresh_fps(0, 60, 100);
        let limit = s.limit();
        feed_with(&mut s, 0, 60, 17, limit, false);
        let status = s.status(1003).expect("a window completed");
        assert!(status.rate <= status.limit as u32);
        assert!(!status.over_limit());
    }

    #[test]
    fn a_fail_open_limit_never_flags() {
        let sample = PpsSample {
            rate: 600,
            window_ms: 1000,
            observed: 600,
            dropped: 0,
            fps: 0,
            limit: 0,
        };
        assert!(!sample.over_limit());
    }

    #[test]
    fn status_is_none_while_filling_and_expires_when_idle() {
        let mut s = PpsState::default();
        s.refresh_fps(0, 60, 100);
        let limit = s.limit();
        // First window still filling: no completed sample yet.
        feed_with(&mut s, 0, 50, 10, limit, false);
        assert!(s.status(500).is_none());
        // Complete it, then the sample is current ...
        feed_with(&mut s, 500, 60, 10, limit, false);
        assert!(s.status(1100).is_some());
        // ... but a silent client must not keep showing a stale red cell.
        assert!(s.status(4000).is_none());
    }

    #[test]
    fn an_idle_reset_clears_the_shared_status() {
        let mut s = PpsState::default();
        s.refresh_fps(0, 60, 100);
        let limit = s.limit();
        feed_with(&mut s, 0, 101, 10, limit, false);
        assert!(s.status(1000).is_some());
        // The packet after a 2 s silence resets the windows: the status must
        // describe the new traffic, not the pre-idle one.
        s.on_packet(6000, limit, false);
        assert!(s.status(6000).is_none());
    }

    #[test]
    fn the_log_edge_state_follows_over_then_under_then_over() {
        let mut s = PpsState::default();
        s.refresh_fps(0, 60, 100);
        let limit = s.limit();
        // First over-limit window: starts the log edge.
        feed_with(&mut s, 0, 101, 10, limit, false);
        assert!(s.over(), "the first over-limit window must arm the edge");
        // A full under-limit second clears it (one "back under" line).
        feed_with(&mut s, 1000, 51, 20, limit, false);
        assert!(!s.over(), "an under-limit window must clear the edge");
        // Over again: the next window re-arms it (a second "over" line).
        feed_with(&mut s, 2000, 101, 10, limit, false);
        assert!(s.over(), "the edge must re-arm after a clean window");
        // Connect/disconnect (full reset) clears it.
        s.reset();
        assert!(!s.over());
    }

    #[test]
    fn clearing_the_edge_does_not_touch_the_measurement_windows() {
        let mut s = PpsState::default();
        s.refresh_fps(0, 60, 100);
        let limit = s.limit();
        feed_with(&mut s, 0, 101, 10, limit, false);
        assert!(s.over());
        // The log cvar turning off only forgets the edge.
        s.clear_over();
        assert!(!s.over());
        assert!(s.status(1000).is_some());
        assert_eq!(s.limit(), limit);
    }

    #[test]
    fn an_idle_gap_keeps_the_log_edge_state() {
        let mut s = PpsState::default();
        s.refresh_fps(0, 60, 100);
        let limit = s.limit();
        feed_with(&mut s, 0, 101, 10, limit, false);
        assert!(s.over());
        // The windows restart on the packet after the silence, but a returning
        // player still owes the "back under limit" line once they are clean.
        s.on_packet(6000, limit, false);
        assert!(s.over());
    }

    #[test]
    fn transition_lines_report_the_rates_and_identity_only() {
        let over = PpsSample {
            rate: 100,
            window_ms: 1000,
            observed: 120,
            dropped: 37,
            fps: 60,
            limit: 62,
        };
        assert_eq!(
            transition_line(over, true, "client=2 name=\"p\""),
            "----- proxy-rs: pps over limit: rate=100 limit=62 fps=60 window_ms=1000 \
             client=2 name=\"p\"\n"
        );
        let under = PpsSample { rate: 40, ..over };
        assert_eq!(
            transition_line(under, false, "client=2 name=\"p\""),
            "----- proxy-rs: pps back under limit: rate=40 limit=62 fps=60 window_ms=1000 \
             client=2 name=\"p\"\n"
        );
    }
}
