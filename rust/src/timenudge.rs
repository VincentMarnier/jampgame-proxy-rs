//! Server-side `cl_timeNudge` neutralisation (`proxy_sv_voidTimeNudge`,
//! `proxy_sv_voidTimeNudgeLog`) — D-015.
//!
//! The netStatus estimator ([`crate::netstatus_est`], D-007) reports the
//! client's `cl_timeNudge` as a confidence range. The client never transmits
//! the setting; it only shifts the clock the client stamps into its usercmds
//! (`cl->serverTime = cls.realtime + cl->serverTimeDelta - tn`,
//! `cl_cgame.cpp:2073-2088`). `CL_Frame` copies that clock into
//! `ucmd->serverTime` (`cl_input.cpp:2144`), so the server sees exactly the
//! shifted timeline.
//!
//! When the estimated range **excludes 0** the proxy is confident the player
//! uses a non-zero nudge, and can put the game back on the neutral timeline:
//! the retargeted `call SV_ClientThink` inside `SV_UserMove` rewrites the cmd's
//! `serverTime` by `+tn` (the inverse of the client's `- tn`), so the game
//! processes the move at the time a `cl_timeNudge 0` client would have
//! stamped. The correction is **ramped by 1 ms per usercmd** in either
//! direction: a jump of up to ±30 ms would either exceed the game's
//! `msec < 1` guard (dropping the first commands, inputs included) or produce
//! one oversized `msec` frame; a 1 ms step keeps every command at the client's
//! normal cadence and costs the same total clock shift.
//!
//! `SV_ClientThink` also stores the cmd it receives in `cl->lastUsercmd`, which
//! the pristine `SV_UserMove` uses *before* the call to discard
//! `cl_packetdup` copies (`cmds[i].serverTime <= cl->lastUsercmd.serverTime`).
//! The stub therefore restores `cl->lastUsercmd.serverTime` to the client's
//! raw stamp after the call (the game already read the adjusted value through
//! `trap_GetUsercmd`, which runs inside the call): the duplicate check keeps
//! comparing client stamps, and re-sent commands are still discarded.
//!
//! ## Limits
//!
//! * Only the server side of the clock is corrected. The client's own
//!   prediction/rendering still runs on its shifted clock; the server no
//!   longer accepts the shifted timing.
//! * The correction is clamped to ±[`MAX_SHIFT_MS`] — the 2003 release
//!   client's own `cl_timeNudge` clamp (`cl_cgame.cpp:2075-2081`). A modified
//!   client using a larger value keeps the excess; the proxy never amplifies
//!   an out-of-range reading into an unbounded clock shift.
//! * It needs a settled estimator window (~5 s, D-007) and two consecutive
//!   refreshes whose range excludes 0 (one reading is not trusted: the first
//!   window after a connect can hold startup artifacts), so it starts well
//!   after a player connects and lags a `cl_timeNudge` change by the window's
//!   step detection. A range that contains 0 disengages at once.
//!
//! ## Logging
//!
//! `proxy_sv_voidTimeNudgeLog 1` logs **transitions**, like
//! `proxy_sv_clientPpsLog`: one line when a client's confirmed range excludes 0
//! (the shift the feature applies, or would apply) and one when it stops. It
//! works on its own: with `proxy_sv_voidTimeNudge 0` the estimator and the log
//! still run, but no cmd is rewritten. While the log cvar is `0` no edge state
//! is kept, so enabling it reports the next non-zero reading.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::netstatus_est::Estimate;
use crate::state;

/// The 2003 release client clamps `cl_timeNudge` to `[-30, 30]`
/// (`cl_cgame.cpp:2075-2081`); the server never corrects by more.
pub const MAX_SHIFT_MS: i32 = 30;

/// How often the estimator verdict is refreshed per client. The netStatus
/// table itself is rate-limited to 500 ms and the window is ~5 s, so a faster
/// cadence would only recompute the same window.
pub const REFRESH_MS: i32 = 500;

/// Consecutive refreshes a range must exclude 0 before the correction is
/// applied or logged. The first window after a connect/map load can hold
/// startup artifacts (the ground-truth harness showed a spurious
/// `[-76..-61]` before the window settled), so a single reading is not acted
/// on; a real setting stays outside the range on every refresh. Disengaging
/// does not need confirmation: a range that contains 0 stops the correction at
/// once.
pub const CONFIRM_CHECKS: u8 = 2;

/// Per-client neutralisation state.
#[derive(Debug, Default, Clone, Copy)]
pub struct VoidState {
    /// `svs.time` of the last verdict refresh (0 = never).
    pub last_check_ms: i32,
    /// Correction currently applied to usercmds, ramping toward [`target`].
    ///
    /// [`target`]: Self::target
    pub offset: i32,
    /// Correction the next usercmd moves toward: the verdict while the feature
    /// is on, `0` while it is off or the range contains 0.
    pub target: i32,
    /// Log edge: the last emitted verdict was "the range excludes 0".
    pub active: bool,
    /// Consecutive refreshes whose range excluded 0 (see [`CONFIRM_CHECKS`]).
    pub streak: u8,
}

impl VoidState {
    /// Whether a new estimator verdict is due (also true after the engine clock
    /// goes backwards, e.g. a map restart).
    pub fn due(&self, now: i32) -> bool {
        let elapsed = now.wrapping_sub(self.last_check_ms);
        !(0..REFRESH_MS).contains(&elapsed)
    }

    /// Move the applied offset one millisecond toward the target and return it.
    ///
    /// The 1 ms step bounds the per-command `msec` change to ±1 (see the module
    /// docs); a client that stopped producing commands leaves the offset where
    /// it was, which is correct because its clock has not moved either.
    pub fn step(&mut self) -> i32 {
        self.offset += (self.target - self.offset).signum();
        self.offset
    }

    /// Stop correcting (ramp the applied offset back to zero) without
    /// forgetting the log edge. The confirmation streak starts over: the
    /// window this verdict came from is gone.
    pub fn stop(&mut self) {
        self.target = 0;
        self.streak = 0;
    }

    /// Record one refresh whose range did (or did not) exclude 0 and report
    /// whether the verdict has been confirmed often enough to act on.
    pub fn observe(&mut self, excludes_zero: bool) -> bool {
        self.streak = if excludes_zero {
            self.streak.saturating_add(1)
        } else {
            0
        };
        self.streak >= CONFIRM_CHECKS
    }

    /// Forget everything (connect/disconnect, feature re-enable). A fresh
    /// client slot starts from the neutral clock, so the applied offset can be
    /// dropped without a ramp.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

// ---------------------------------------------------------------------------
// Cvar mirror (set once per `GAME_RUN_FRAME` by `update_proxy_cvars`)
// ---------------------------------------------------------------------------

static ENABLED: AtomicBool = AtomicBool::new(false);
static LOG: AtomicBool = AtomicBool::new(false);

/// Whether the cmd-rewriting feature is on (`proxy_sv_voidTimeNudge`).
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Whether transitions are logged (`proxy_sv_voidTimeNudgeLog`).
pub fn logging() -> bool {
    LOG.load(Ordering::Relaxed)
}

/// Mirror the cvars into lock-free atomics. A `0 -> 1` edge clears every
/// client's correction (nothing was applied while the feature was off, so the
/// stale offset must not leak into the first packet after re-enabling).
pub fn set_config(enabled: i32, log: i32) {
    let on = enabled != 0;
    let was = ENABLED.swap(on, Ordering::Relaxed);
    LOG.store(log != 0, Ordering::Relaxed);
    if on && !was {
        clear_offsets();
    }
}

/// Drop every client's applied correction and verdict (master switch off,
/// `GAME_INIT`, feature re-enable).
pub fn clear_offsets() {
    state::with_state(|s| {
        for client in s.clients.iter_mut() {
            client.nudge_void.clear();
        }
    });
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// The correction for one estimator range: `Some(offset)` only when the range
/// excludes 0, `None` when 0 is inside it (or the range is unusable).
///
/// The offset is the point estimate rounded to the integer `cl_timeNudge`
/// value the client holds, clamped to ±[`MAX_SHIFT_MS`]. The estimator's
/// ±2 ms bias floor means an excluding range implies `|tn| > 2`, so rounding
/// cannot produce 0; the explicit ±1 fallback only guards a future floor
/// change.
pub fn offset_for(tn: f32, lo: f32, hi: f32) -> Option<i32> {
    if !tn.is_finite() || !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    // 0 is in [lo, hi]: the reading is consistent with the default setting.
    if lo <= 0.0 && hi >= 0.0 {
        return None;
    }
    let rounded = tn.round() as i32;
    let clamped = rounded.clamp(-MAX_SHIFT_MS, MAX_SHIFT_MS);
    Some(if clamped == 0 {
        if tn < 0.0 { -1 } else { 1 }
    } else {
        clamped
    })
}

/// One logged verdict transition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transition {
    /// The range excludes 0 (the client is seen using a non-zero nudge).
    pub active: bool,
    /// Whether an estimator range existed at all (`false` = stale/filling).
    pub has_estimate: bool,
    pub lo: f32,
    pub hi: f32,
    /// The correction the feature applies (or would apply while it is off).
    pub shift: i32,
}

/// Store the fresh verdict for one client and return the log transition, if
/// the log is on and the verdict changed.
///
/// `enforce` follows `proxy_sv_voidTimeNudge`: when false the verdict is still
/// computed (the log reports the shift it *would* apply) but the applied
/// correction is ramped back to 0.
pub fn update(
    client_num: usize,
    now: i32,
    est: Option<&Estimate>,
    enforce: bool,
    log: bool,
) -> Option<Transition> {
    let verdict = est.and_then(|e| offset_for(e.tn, e.lo, e.hi));
    state::with_state(|s| {
        let v = &mut s.clients[client_num].nudge_void;
        v.last_check_ms = now;
        // One reading is not enough: the first window after a connect can hold
        // startup artifacts (see CONFIRM_CHECKS).
        let confirmed = v.observe(verdict.is_some());
        v.target = if enforce && confirmed {
            verdict.unwrap_or(0)
        } else {
            0
        };
        let active = confirmed;
        let transition = (log && active != v.active).then(|| Transition {
            active,
            has_estimate: est.is_some(),
            lo: est.map(|e| e.lo).unwrap_or(0.0),
            hi: est.map(|e| e.hi).unwrap_or(0.0),
            shift: verdict.unwrap_or(0),
        });
        // While the log is off no edge state is kept: turning it on reports
        // the next non-zero reading instead of staying silent on a stale edge.
        v.active = log && active;
        transition
    })
}

/// Print one transition after the state lock is released (the engine print
/// trap must not run under the proxy mutex).
///
/// # Safety
///
/// `cl` must be the valid `client_t*` whose usercmd packet was just processed.
pub unsafe fn print_transition(cl: usize, transition: Transition) {
    // SAFETY: cl is a valid client_t; the label reads its name and address.
    let label = unsafe { crate::hooks::engine_sv::client_label(cl) };
    let line = transition_line(transition, &label);
    match std::ffi::CString::new(line) {
        // SAFETY: engine syscall pointer registered by dllEntry; text is
        // NUL-terminated and outlives the call.
        Ok(text) => unsafe { crate::syscall::print(&text) },
        Err(err) => eprintln!(
            "----- proxy-rs: timeNudge log line contained a NUL byte: {}",
            String::from_utf8_lossy(&err.into_vec())
        ),
    }
}

fn transition_line(transition: Transition, label: &str) -> String {
    let range = if transition.has_estimate {
        format!("[{:.1}..{:.1}]", transition.lo, transition.hi)
    } else {
        "-".to_string()
    };
    if transition.active {
        format!(
            "----- proxy-rs: timeNudge non-zero: estimate={range} shift={} {label}\n",
            transition.shift
        )
    } else if transition.has_estimate {
        format!("----- proxy-rs: timeNudge back to zero: estimate={range} {label}\n")
    } else {
        format!("----- proxy-rs: timeNudge no reading: estimate=- {label}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_containing_zero_needs_no_correction() {
        assert_eq!(offset_for(1.2, -2.0, 4.4), None);
        assert_eq!(offset_for(0.0, -2.0, 2.0), None);
        assert_eq!(offset_for(-2.9, -4.9, 0.9), None);
        assert_eq!(offset_for(2.9, -0.9, 4.9), None);
    }

    #[test]
    fn an_excluding_range_yields_the_rounded_setting() {
        assert_eq!(offset_for(-14.2, -17.9, -10.5), Some(-14));
        assert_eq!(offset_for(16.1, 12.6, 19.6), Some(16));
        // 0 is outside a very narrow range, but the point estimate rounds to
        // 0: the sign still forces a correction.
        assert_eq!(offset_for(-0.4, -2.4, -0.1), Some(-1));
        assert_eq!(offset_for(0.4, 0.1, 2.4), Some(1));
    }

    #[test]
    fn the_shift_is_clamped_to_the_release_client_range() {
        assert_eq!(offset_for(-240.0, -243.0, -237.0), Some(-MAX_SHIFT_MS));
        assert_eq!(offset_for(500.0, 497.0, 503.0), Some(MAX_SHIFT_MS));
        assert_eq!(offset_for(-30.2, -33.2, -27.2), Some(-30));
    }

    #[test]
    fn non_finite_readings_are_ignored() {
        assert_eq!(offset_for(f32::NAN, -2.0, 2.0), None);
        assert_eq!(offset_for(1.0, f32::NAN, 2.0), None);
        assert_eq!(offset_for(1.0, -2.0, f32::INFINITY), None);
    }

    #[test]
    fn the_verdict_refreshes_every_500_ms_and_after_a_clock_jump() {
        let mut v = VoidState::default();
        assert!(v.due(1000));
        v.last_check_ms = 1000;
        assert!(!v.due(1499));
        assert!(v.due(1500));
        // svs.time moved backwards (map restart): refresh immediately instead
        // of waiting for it to catch up.
        assert!(v.due(10));
    }

    #[test]
    fn the_applied_offset_ramps_one_millisecond_per_command() {
        let mut v = VoidState {
            target: -3,
            ..Default::default()
        };
        assert_eq!(v.step(), -1);
        assert_eq!(v.step(), -2);
        assert_eq!(v.step(), -3);
        assert_eq!(v.step(), -3);
        v.target = 2;
        assert_eq!(v.step(), -2);
        assert_eq!(v.step(), -1);
        assert_eq!(v.step(), 0);
        assert_eq!(v.step(), 1);
        assert_eq!(v.step(), 2);
        assert_eq!(v.offset, 2);
    }

    #[test]
    fn stop_ramps_back_without_clearing_the_log_edge() {
        let mut v = VoidState {
            offset: -10,
            target: -10,
            active: true,
            ..Default::default()
        };
        v.stop();
        assert_eq!(v.target, 0);
        assert!(v.active);
        assert_eq!(v.step(), -9);
    }

    #[test]
    fn a_single_reading_must_be_confirmed_before_it_is_acted_on() {
        let mut v = VoidState::default();
        assert!(!v.observe(true), "first non-zero reading must not engage");
        assert!(v.observe(true), "second consecutive reading confirms");
        // A range that contains 0 disengages at once (no confirmation).
        assert!(!v.observe(false));
        assert_eq!(v.streak, 0);
        // ... and the next reading starts a new confirmation.
        assert!(!v.observe(true));
        assert!(v.observe(true));
        // A window reset restarts the confirmation but keeps the log edge.
        v.active = true;
        v.stop();
        assert_eq!(v.target, 0);
        assert_eq!(v.streak, 0);
        assert!(v.active);
    }

    #[test]
    fn transition_lines_report_the_range_and_shift() {
        let start = Transition {
            active: true,
            has_estimate: true,
            lo: -17.9,
            hi: -10.5,
            shift: -14,
        };
        assert_eq!(
            transition_line(start, "client=2 name=\"p\""),
            "----- proxy-rs: timeNudge non-zero: estimate=[-17.9..-10.5] shift=-14 \
             client=2 name=\"p\"\n"
        );
        let back = Transition {
            active: false,
            shift: 0,
            ..start
        };
        assert_eq!(
            transition_line(back, "client=2 name=\"p\""),
            "----- proxy-rs: timeNudge back to zero: estimate=[-17.9..-10.5] \
             client=2 name=\"p\"\n"
        );
        let lost = Transition {
            active: false,
            has_estimate: false,
            shift: 0,
            ..start
        };
        assert_eq!(
            transition_line(lost, "client=2 name=\"p\""),
            "----- proxy-rs: timeNudge no reading: estimate=- client=2 name=\"p\"\n"
        );
    }
}
