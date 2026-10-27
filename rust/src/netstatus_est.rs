//! netStatus `cl_timeNudge` estimator (D-007) — pure, unit-tested math.
//!
//! The client's `cl_timeNudge` is never transmitted to the server: its only
//! effect is the clock the client stamps into its usercmds
//! (`cl->serverTime = cls.realtime + cl->serverTimeDelta - tn`, cl_cgame.cpp).
//! The estimator recovers it per usercmd packet and reports a **range** (CI of
//! the window mean widened by a fixed bias floor).
//!
//! # Derivation (re-derived from engine source before coding, 2026-10-26)
//!
//! Notation per usercmd packet `k` (one `clc_move`/`clc_moveNoDelta` message,
//! new cmds only — `cl_packetdup` duplicates are dropped by `SV_UserMove`
//! before the feed):
//!
//! * `P_k`   — the wall instant the packet's datagram was sent;
//! * `n_k`   — processed cmds in the packet, `Tbar_k` their mean `serverTime`,
//!   `s_max_k` their newest `serverTime`;
//! * `A_k`   — `svs.time` at the server frame processing the packet;
//! * `S_j`   — `cl->frames[messageAcknowledge & PACKET_MASK].messageSent`, the
//!   `svs.time` sample of the snapshot message the packet acked;
//! * `d`     — the server→client one-way delay baked into `serverTimeDelta`
//!   (`CL_FirstSnapshot`: `serverTimeDelta = snap.serverTime - cls.realtime`);
//! * `f`     — the client's cmd rate; `I_cmd = 1000/f`;
//! * `ackdelay_j = P_k - w'_r(j)` — how long the ack of message `j` sat in the
//!   client before the packet went out (`w'_r(j)` = the message's arrival);
//! * `δ_s`   — the send-side quantization of `S_j` (true send instant minus the
//!   `svs.time` sample it carries);
//! * `ν`     — the one-way jitter of message `j`;
//! * `E`     — the client's `serverTimeDelta` drift (`CL_AdjustTimeDelta`),
//!   unobservable from the server.
//!
//! A cmd created at client wall instant `w` carries `serverTime = S(w) - d -
//! tn + E`, and — because `CL_SendCmd()` runs *before* `CL_SetCGameTime()` in
//! `CL_Frame` (cl_main.cpp:1846/1852) — the newest cmd of a packet is stamped
//! with the *previous* frame's clock value, i.e. it is already one frame period
//! old at send. With `age_i = P_k - w_i`:
//!
//! ```text
//! Tbar_k      = S(P_k) - mean_age_k - d - tn + E,  mean_age_k = (s_max - Tbar) + I_cmd
//! S_j         = S(T_sent_j) - δ_s
//! S(T_sent_j) - S(P_k)    = -(ackdelay_j + d + ν)
//! ```
//!
//! and, because `A_k` cancels exactly (`messageAcked` and `A_k` are the same
//! `svs.time` sample — `SV_UserMove` writes it before the `SV_ClientThink`
//! loop), the per-packet raw quantity is
//!
//! ```text
//! raw_k := -(delay_k + rtt_j) = S_j - Tbar_k
//!        = tn + mean_age_k - ackdelay_j - ν - δ_s - E
//! ```
//!
//! so
//!
//! ```text
//! tn_hat_k = raw_k - mean_age_k + E[ackdelay_j] + (residuals)
//! ```
//!
//! with `E[ackdelay_j] = min(I_pkt, I_msg) / 2`: the ack of message `j` goes
//! out on the next packet: its age at the packet's send is the wait for that
//! packet. Only the **first** packet that acks a message is sampled — its wait
//! is bounded by the packet interval even when the message was delayed or an
//! earlier packet was lost — and the wait is modelled as `(I_pkt - I_cmd)/2`
//! (the residual wait on the client's frame grid). Later packets acking the
//! same message are not sampled: their wait is correlated with their group
//! size (an early message is acked sooner *and* by more packets), which biases
//! a constant-wait model. Ground truth over the whole synthetic matrix
//! (2026-10-26) gives a 0.67 ms mean bias for this pairing, vs 3.86 ms for an
//! all-packet lattice model and 9.07 ms for `min(I_pkt, I_msg)/2`.
//!
//! The wait is the residual of the packet grid and is bounded by the message
//! spacing when messages are denser than packets, so it is
//! `min(I_pkt, I_msg)/2 - I_cmd/2`. `I_msg` is measured from `cl->frames[]`
//! `messageSent` spacing (observed traffic — `cl_maxpackets` is never
//! transmitted and `cl->snapshotMsec` is the *server's* pacing),
//! `I_pkt = n_k * I_cmd`.
//!
//! `δ_s` is the send-side quantization of `S_j`: `SV_Frame` runs a stepped
//! `svs.time` (`+= frameMsec` in the residual loop) and sends snapshots in the
//! same call, so `δ_s` is the `NET_Sleep` overshoot (~0..1 ms) — modelled as
//! 0 and covered by the bias floor (the `frame_msec/2` variant is logged as
//! `tn_q2`).
//!
//! `E` is the client's `serverTimeDelta` walk (`CL_AdjustTimeDelta`): it is
//! *not* a small drift, it is the steady state of the `+1`/`-2` walk that
//! keeps `cls.realtime + serverTimeDelta` just below `cl->snap.serverTime - 5`
//! (cl_cgame.cpp:2099-2100). The walk only reacts at snapshot arrivals while
//! the condition is checked every frame, so in its slow-adjust regime
//!
//! ```text
//! E = I_msg + EXTRAPOLATION_MARGIN_MS - I_cmd
//! ```
//!
//! (`I_msg - I_cmd` because the condition is tested on the frame grid before
//! the next snapshot refreshes `cl->snap`). Measured ground truth
//! (a2_netstatus_groundtruth.py, 2026-10-26): predicted 38.3 / 54.3 / 57.3 ms
//! vs measured 40.0 / 58.1 / 57.3 ms at sv_fps 20 / 30 / 60.
//!
//! Past [`FAST_ADJUST_MS`] the `deltaDelta > 100` branch (cl_cgame.cpp:1913)
//! halves `serverTimeDelta` instead of stepping it, and the gap saturates into
//! a sawtooth over `[FAST_ADJUST_MS/2, FAST_ADJUST_MS]` (mean
//! [`FAST_ADJUST_MEAN_MS`], measured 71.9 / 70.9 / 73.9 ms at `snaps 10`
//! where the slow model predicts 130 / 106 / 102). `E` is subtracted like
//! every other correction; the walk sawtooth (sd 1-6 ms slow, ~15 ms
//! saturated) stays in the bias floor.
//!
//! Deviations from the planning session's formula, agreed with the user
//! 2026-10-26 and each measured against the ground-truth matrix while the
//! per-term debug logging existed (it was removed with the
//! `proxy_sv_netStatusDebug` cvar afterwards):
//!
//! * `mean_age_k = (s_max - Tbar) + I_cmd`, not `(n_k-1)/(2f)` — the extra
//!   `I_cmd` is the one-frame-stale stamp (a stable `+1/f` bias);
//! * `ackdelay` is the first-packet wait above, not `n_k/(2f)` — the paired
//!   RTT carries the age of the newest *message*, and only its wait component
//!   is unknown;
//! * `E` is modelled (the plan called it unobservable and left it to the bias
//!   floor — measurement says it is a 40-75 ms systematic term);
//! * `q_send = 0` (the residual is covered by the bias floor).
//!
//! The "plan-literal" variant (`(n_k-1)/(2f)`, `n_k/(2f)`, `frame_msec/2`, no
//! `E`) is still computed as `tn_plan` and pinned by a unit test, so the
//! deviation from the plan stays measurable.

use crate::state::PacketRecord;

/// Estimation window (~5 s, decision 6). The per-packet ring holds 512 records
/// (see [`crate::state::PACKET_RING_LEN`]), which covers this window at up to
/// ~100 packets/s.
pub const WINDOW_MS: i32 = 5000;

/// Minimum window span before a value is shown (a partially filled window is
/// displayed as `-`, per D-007's cell syntax).
pub const MIN_WINDOW_MS: i32 = 1000;

/// Minimum packet samples before a value is shown.
pub const MIN_SAMPLES: usize = 8;

/// Fixed bias floor added to the CI (decision 11). The unobservable residual
/// (`E` drift, `δ_s`, network jitter) is not statistical, so the range is the
/// CI of the mean widened by this constant. Set from the ground-truth runs.
pub const BIAS_FLOOR_MS: f32 = 2.0;

/// fps/pps window (decision 13): cmds/packets per second over the last 1 s of
/// cmd `serverTime` (the same clock the original proxy used).
pub const FPS_WINDOW_MS: i32 = 1000;

/// Trimmed-mean fraction dropped at each end of the sorted sample set.
const TRIM: f32 = 0.1;

/// `cl->snap.serverTime - 5` extrapolation margin (cl_cgame.cpp:2099-2100):
/// the `CL_SetCGameTime` condition that drives the `serverTimeDelta` walk.
pub const EXTRAPOLATION_MARGIN_MS: f32 = 5.0;

/// `CL_AdjustTimeDelta` fast-adjust threshold (cl_cgame.cpp:1913-1918): when
/// `|newDelta - serverTimeDelta|` exceeds this the walk is cut in half
/// (`serverTimeDelta = (serverTimeDelta + newDelta) >> 1`) instead of stepped,
/// which clamps the clock gap instead of letting it follow `I_msg`.
pub const FAST_ADJUST_MS: f32 = 100.0;

/// Mean of the fast-adjust sawtooth: the `+1`/`-2` walk grows the gap by 2 per
/// snapshot until it passes [`FAST_ADJUST_MS`], then halves it, so the gap
/// cycles over `[FAST_ADJUST_MS/2, FAST_ADJUST_MS]`.
pub const FAST_ADJUST_MEAN_MS: f32 = 75.0;

/// Step-change threshold (decision 6): the two half-window means must differ
/// by more than `STEP_MIN_MS` and by more than `STEP_SE_FACTOR` standard
/// errors to reset the window.
const STEP_MIN_MS: f32 = 4.0;
const STEP_SE_FACTOR: f32 = 3.0;

/// Everything the estimator computed for one window. The table renders
/// `tn`/`lo`/`hi`/`rtt_*`/`step`; the per-term decomposition fields
/// (`raw_mean`, `lag`, `ackdelay`, `clock_gap`, `tn_plan`, …) are the
/// documented model of D-007.3 and are pinned by the unit tests below — they
/// are not exposed at runtime (the `proxy_sv_netStatusDebug` cvar was dropped
/// at the user's request), so the lib build does not read them.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct Estimate {
    /// Packet samples in the window.
    pub samples: usize,
    /// Distinct acked server messages (the effective independent samples).
    pub messages: usize,
    /// `I_cmd = 1000/f`.
    pub cmd_interval: f32,
    /// Mean cmd age at send (`mean_age_k`), ms.
    pub lag: f32,
    /// Modelled ack delay (`min(I_pkt, I_msg)/2`), ms.
    pub ackdelay: f32,
    /// Modelled `CL_AdjustTimeDelta` clock gap (`E = I_msg + 5 - I_cmd`), ms.
    pub clock_gap: f32,
    /// Mean per-packet RTT `A_k - S_j`, ms.
    pub rtt_mean: f32,
    /// Standard deviation of the per-packet RTT samples, ms.
    pub rtt_dev: f32,
    /// Point estimate (refined model), ms.
    pub tn: f32,
    /// Plan-literal variant (`(n-1)/(2f)`, `n_k/(2f)`, `frame_msec/2`), ms.
    pub tn_plan: f32,
    /// Mean raw sample `S_j - Tbar_k`, ms.
    pub raw_mean: f32,
    /// Standard error of the window mean, ms.
    pub se: f32,
    /// Reported range (CI + bias floor).
    pub lo: f32,
    pub hi: f32,
    /// A step change was detected in the window (the caller resets it).
    pub step: bool,
}

/// Mean of the samples, dropping `TRIM` at each end (robust to loss bursts and
/// map-change leftovers).
fn trimmed_mean(samples: &mut [f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    let n = samples.len();
    let cut = ((n as f32) * TRIM) as usize;
    let body = &samples[cut..n - cut];
    let body = if body.is_empty() { &samples[..] } else { body };
    body.iter().sum::<f32>() / body.len() as f32
}

/// Sample standard deviation.
fn stddev(samples: &[f32], mean: f32) -> f32 {
    if samples.len() < 2 {
        return 0.0;
    }
    let var = samples
        .iter()
        .map(|s| {
            let d = s - mean;
            d * d
        })
        .sum::<f32>()
        / (samples.len() - 1) as f32;
    var.sqrt()
}

/// Two-sided 95% Student-t critical value (df 1..=30, 1.96 beyond).
fn t95(df: usize) -> f32 {
    const T: [f32; 30] = [
        12.71, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160,
        2.145, 2.131, 2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056,
        2.052, 2.048, 2.045, 2.042,
    ];
    if df == 0 {
        return T[0];
    }
    if df <= T.len() { T[df - 1] } else { 1.96 }
}

/// fps/pps over the last [`FPS_WINDOW_MS`] of cmd `serverTime` (decision 13):
/// the window is expiry-based (records older than the newest sample's stamp
/// minus 1 s are dropped), which removes the post-map-load `fps = 1024`
/// artifact of the original's never-written ring slots.
pub fn fps_pps(records: &[PacketRecord]) -> (i32, i32) {
    let newest = records.iter().map(|r| r.newest).max();
    let Some(newest) = newest else {
        return (0, 0);
    };
    let mut fps = 0i32;
    let mut pps = 0i32;
    for record in records {
        if record.count == 0 {
            continue;
        }
        if record.newest + FPS_WINDOW_MS >= newest {
            fps += record.count as i32;
            pps += 1;
        }
    }
    (fps, pps)
}

/// Compute the estimate over `records` (all committed packets, any order).
///
/// `now` is the current `svs.time`; `frame_msec` is `1000/sv_fps` (only used
/// for the logged `frame_msec/2` variant); `msg_interval` is the message
/// spacing measured from `cl->frames[]` (`None` → derived from the window).
///
/// Returns `None` when the window is stale or still filling (the cell shows
/// `-`).
pub fn estimate(
    records: &[PacketRecord],
    now: i32,
    frame_msec: f32,
    msg_interval: Option<f32>,
) -> Option<Estimate> {
    // live window: committed packets seen within WINDOW_MS of `now`
    let live: Vec<&PacketRecord> = records
        .iter()
        .filter(|r| {
            r.count > 0 && {
                let age = now.wrapping_sub(r.arrival);
                (0..=WINDOW_MS).contains(&age)
            }
        })
        .collect();
    if live.len() < MIN_SAMPLES {
        return None;
    }
    let oldest_arrival = live.iter().map(|r| r.arrival).min()?;
    let newest_arrival = live.iter().map(|r| r.arrival).max()?;
    if newest_arrival - oldest_arrival < MIN_WINDOW_MS {
        return None;
    }

    // cmd rate from the contiguous stamp sequence: `M` cmds span `M-1` gaps
    let cmds: i64 = live.iter().map(|r| r.count as i64).sum();
    let stamp_min = live.iter().map(|r| r.oldest).min()?;
    let stamp_max = live.iter().map(|r| r.newest).max()?;
    let stamp_span = (stamp_max - stamp_min) as f32;
    let cmd_interval = if cmds > 1 && stamp_span > 0.0 {
        stamp_span / (cmds - 1) as f32
    } else {
        return None;
    };

    // measured message spacing: `cl->frames[]` when given, else the window's
    // distinct messageSent samples
    let msg_interval = msg_interval.filter(|v| *v > 0.0).or_else(|| {
        let mut sends: Vec<i32> = live
            .iter()
            .map(|r| r.message_sent)
            .filter(|s| *s > 0)
            .collect();
        sends.sort_unstable();
        sends.dedup();
        if sends.len() >= 2 {
            let span = (sends[sends.len() - 1] - sends[0]) as f32;
            Some(span / (sends.len() - 1) as f32)
        } else {
            None
        }
    })?;
    if msg_interval <= 0.0 {
        return None;
    }

    // Per-message samples: only the *first* packet that acks a message is
    // used. Its ack delay is the wait for that packet after the message was
    // processed — bounded by the packet interval even when the message was
    // delayed or the previous packets were lost — and its mean is
    // `(I_pkt - I_cmd)/2` (the residual wait on the client's frame grid).
    // Later packets acking the same message carry a send offset that is
    // observable, but their wait is correlated with the group size (an early
    // message is acked sooner *and* by more packets), which biases a constant
    // wait model: ground truth over the whole matrix (2026-10-26) gives a
    // 0.67 ms mean bias here vs 3.86 ms for the all-packet lattice model and
    // 9.07 ms for `min(I_pkt, I_msg)/2`.
    let slow_gap = msg_interval + EXTRAPOLATION_MARGIN_MS - cmd_interval;
    let clock_gap = if slow_gap > FAST_ADJUST_MS {
        // the `deltaDelta > 100` fast-adjust halves the walk instead of
        // stepping it: the gap then cycles over [50, 100] rather than
        // following I_msg (measured 71.9/70.9/73.9 at snaps 10)
        FAST_ADJUST_MEAN_MS
    } else {
        slow_gap
    };

    let mut tn_samples = Vec::with_capacity(live.len());
    let mut plan_samples = Vec::with_capacity(live.len());
    let mut rtt_samples = Vec::with_capacity(live.len());
    let mut lag_sum = 0.0f32;
    let mut raw_sum = 0.0f32;
    let mut ackdelay_sum = 0.0f32;
    let mut used = 0usize;
    let mut group_ack = i32::MIN;
    for record in &live {
        if record.message_sent <= 0 || record.msg_ack == group_ack {
            continue;
        }
        group_ack = record.msg_ack;
        let n = record.count as f32;
        let tbar = record.stamp_sum as f32 / n;
        let lag = (record.newest as f32 - tbar) + cmd_interval;
        let raw = record.message_sent as f32 - tbar;
        let pkt_interval = n * cmd_interval;
        // the wait for the first packet after the message was processed is the
        // residual of the packet grid, bounded by the message spacing when the
        // messages are denser than the packets (each packet then acks the
        // newest message, whose age stays under I_msg)
        let ackdelay = (pkt_interval.min(msg_interval) - cmd_interval) / 2.0;
        tn_samples.push(raw - lag + ackdelay - clock_gap);
        plan_samples
            .push(raw - (n - 1.0) * cmd_interval / 2.0 + pkt_interval / 2.0 - frame_msec / 2.0);
        rtt_samples.push((record.arrival - record.message_sent) as f32);
        lag_sum += lag;
        raw_sum += raw;
        ackdelay_sum += ackdelay;
        used += 1;
    }
    if used < MIN_SAMPLES {
        return None;
    }
    let messages = used;

    let mut tn_sorted = tn_samples.clone();
    let tn = trimmed_mean(&mut tn_sorted);
    // one sample per acked message: the sample count is already the number of
    // independent ack-delay draws
    let se = stddev(&tn_samples, tn) / (messages as f32).sqrt();
    let half = t95(messages.saturating_sub(1)) * se + BIAS_FLOOR_MS;

    // step change: older half vs newer half of the window (arrival order)
    let mut older: Vec<f32> = Vec::new();
    let mut newer: Vec<f32> = Vec::new();
    let mid = tn_samples.len() / 2;
    for (i, sample) in tn_samples.iter().enumerate() {
        if i < mid {
            older.push(*sample);
        } else {
            newer.push(*sample);
        }
    }
    let mut older_sorted = older.clone();
    let mut newer_sorted = newer.clone();
    let older_mean = trimmed_mean(&mut older_sorted);
    let newer_mean = trimmed_mean(&mut newer_sorted);
    let pooled = (stddev(&older, older_mean).powi(2) / older.len().max(1) as f32
        + stddev(&newer, newer_mean).powi(2) / newer.len().max(1) as f32)
        .sqrt();
    let step = !older.is_empty()
        && !newer.is_empty()
        && (older_mean - newer_mean).abs() > STEP_MIN_MS
        && (older_mean - newer_mean).abs() > STEP_SE_FACTOR * pooled;

    let rtt_mean = rtt_samples.iter().sum::<f32>() / used as f32;
    Some(Estimate {
        samples: used,
        messages,
        cmd_interval,
        lag: lag_sum / used as f32,
        ackdelay: ackdelay_sum / used as f32,
        clock_gap,
        raw_mean: raw_sum / used as f32,
        rtt_mean,
        rtt_dev: stddev(&rtt_samples, rtt_mean),
        tn,
        tn_plan: trimmed_mean(&mut plan_samples),
        se,
        lo: tn - half,
        hi: tn + half,
        step,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(arrival: i32, message_sent: i32, stamps: &[i32]) -> PacketRecord {
        PacketRecord {
            stamp_sum: stamps.iter().map(|s| *s as i64).sum(),
            arrival,
            message_sent,
            newest: *stamps.iter().max().unwrap(),
            oldest: *stamps.iter().min().unwrap(),
            msg_ack: arrival,
            count: stamps.len() as u16,
        }
    }

    /// An ideal window: contiguous cmds spaced `cmd_interval`, `n` per packet,
    /// every packet acking a message of its own whose ack delay is exactly the
    /// model's first-packet wait `(I_pkt - I_cmd)/2`, with the
    /// `CL_AdjustTimeDelta` clock gap at its slow-walk equilibrium
    /// (`I_msg + 5 - I_cmd`). No jitter, no drift, no quantization: the
    /// estimator must recover `tn` from it.
    ///
    /// Per-packet identity the estimator inverts:
    /// `tn = message_sent - s_max - I_cmd + ackdelay - clock_gap`.
    fn ideal_window(
        tn: f32,
        cmd_interval: f32,
        n: usize,
        i_msg: f32,
        packets: usize,
    ) -> Vec<PacketRecord> {
        let mut out = Vec::new();
        let mut s_max = 100_000.0f32;
        let ackdelay = (n as f32 - 1.0) * cmd_interval / 2.0;
        let clock_gap = i_msg + EXTRAPOLATION_MARGIN_MS - cmd_interval;
        for i in 0..packets {
            let stamps: Vec<i32> = (0..n)
                .map(|j| (s_max - j as f32 * cmd_interval).round() as i32)
                .collect();
            let message_sent = (tn + s_max + cmd_interval - ackdelay + clock_gap).round() as i32;
            let arrival = message_sent + 2; // a constant 2 ms RTT
            let mut record = packet(arrival, message_sent, &stamps);
            record.msg_ack = i as i32; // one message per packet
            out.push(record);
            s_max += n as f32 * cmd_interval;
        }
        out
    }

    #[test]
    fn recovers_tn_from_an_ideal_window() {
        for (cmd_interval, n, i_msg, tn) in [
            (1000.0 / 60.0, 2, 50.0, -15.0),
            (1000.0 / 125.0, 4, 33.0, 30.0),
            (1000.0 / 30.0, 1, 50.0, 0.0),
            (1000.0 / 30.0, 3, 100.0, 15.0),
        ] {
            let records = ideal_window(tn, cmd_interval, n, i_msg, 120);
            let now = records.iter().map(|r| r.arrival).max().unwrap();
            let est = estimate(&records, now, 50.0, Some(i_msg)).expect("window");
            assert!(
                (est.tn - tn).abs() <= 1.0,
                "tn = {} want {} (raw {}, lag {}, ackdelay {}, I_cmd {})",
                est.tn,
                tn,
                est.raw_mean,
                est.lag,
                est.ackdelay,
                est.cmd_interval
            );
        }
    }

    #[test]
    fn plan_variant_carries_the_frame_staleness() {
        // the plan-literal variant drops the +I_cmd stamp lag and models
        // ackdelay as the packet interval: at I_pkt != I_msg it differs from
        // the refined estimate (logged as tn_plan, never displayed)
        let records = ideal_window(0.0, 1000.0 / 60.0, 2, 100.0, 120);
        let now = records.iter().map(|r| r.arrival).max().unwrap();
        let est = estimate(&records, now, 50.0, Some(100.0)).expect("window");
        assert!((est.tn - 0.0).abs() <= 1.0, "refined {}", est.tn);
        assert!(
            (est.tn_plan - est.tn).abs() > 5.0,
            "plan variant {} vs refined {}",
            est.tn_plan,
            est.tn
        );
    }

    #[test]
    fn stale_or_filling_windows_are_not_reported() {
        let records = ideal_window(0.0, 1000.0 / 60.0, 2, 50.0, 120);
        let now = records.iter().map(|r| r.arrival).max().unwrap();
        // stale: `now` far beyond the window
        assert!(estimate(&records, now + 10_000, 50.0, Some(50.0)).is_none());
        // filling: only a few samples
        assert!(estimate(&records[..6], now, 50.0, Some(50.0)).is_none());
    }

    #[test]
    fn range_is_ci_plus_bias_floor() {
        let records = ideal_window(5.0, 1000.0 / 60.0, 2, 50.0, 120);
        let now = records.iter().map(|r| r.arrival).max().unwrap();
        let est = estimate(&records, now, 50.0, Some(50.0)).expect("window");
        let half = est.hi - est.tn;
        assert!(half >= BIAS_FLOOR_MS - 1e-3);
        assert!(half <= BIAS_FLOOR_MS + t95(est.samples - 1) * est.se + 1e-3);
        assert!(est.lo <= est.tn && est.tn <= est.hi);
    }

    #[test]
    fn step_change_is_detected_across_a_tn_jump() {
        let mut records = ideal_window(0.0, 1000.0 / 60.0, 2, 50.0, 60);
        let mut late = ideal_window(30.0, 1000.0 / 60.0, 2, 50.0, 60);
        // continue the stamp sequence and the arrivals seamlessly
        let shift = records.last().unwrap().arrival - late.first().unwrap().arrival + 100;
        let stamp_shift = records.last().unwrap().newest + 34 - late.first().unwrap().newest;
        for record in &mut late {
            record.arrival += shift;
            record.message_sent += shift;
            record.newest += stamp_shift;
            record.oldest += stamp_shift;
            record.stamp_sum += stamp_shift as i64 * record.count as i64;
        }
        records.append(&mut late);
        let now = records.iter().map(|r| r.arrival).max().unwrap();
        let est = estimate(&records, now, 50.0, Some(50.0)).expect("window");
        assert!(est.step, "expected a step detection (tn {})", est.tn);
    }

    #[test]
    fn only_the_first_packet_of_each_message_is_sampled() {
        // two packets acking the *same* message: only the first is sampled,
        // so the second one's stale pairing cannot shift the estimate
        let cmd_interval = 1000.0 / 60.0;
        let i_msg = 50.0;
        let n = 2usize;
        let clock_gap = i_msg + EXTRAPOLATION_MARGIN_MS - cmd_interval;
        let wait = (n as f32 - 1.0) * cmd_interval / 2.0;
        let mut out = Vec::new();
        let mut s_max = 200_000.0f32;
        for (msg, _pair) in (0..60).enumerate() {
            let msg = msg as i32;
            // the message of this pair is sent `wait` after its first packet's
            // stamp anchor: both packets of the pair ack it
            let pair_first = s_max;
            let message_sent = (3.0 + pair_first + cmd_interval - wait + clock_gap).round() as i32;
            for _slot in 0..2 {
                let stamps: Vec<i32> = (0..n)
                    .map(|j| (s_max - j as f32 * cmd_interval).round() as i32)
                    .collect();
                let arrival = message_sent + 2;
                let mut record = packet(arrival, message_sent, &stamps);
                record.msg_ack = msg;
                out.push(record);
                s_max += n as f32 * cmd_interval;
            }
        }
        let now = out.iter().map(|r| r.arrival).max().unwrap();
        let est = estimate(&out, now, 50.0, Some(i_msg)).expect("window");
        assert!(
            (est.tn - 3.0).abs() <= 1.0,
            "tn = {} (samples {}, messages {})",
            est.tn,
            est.samples,
            est.messages
        );
    }

    #[test]
    fn clock_gap_saturates_at_the_fast_adjust_regime() {
        // snaps 10: I_msg 108-145 puts the walk past the `deltaDelta > 100`
        // fast-adjust threshold, where the gap cycles over [50, 100] instead
        // of following I_msg
        let records = ideal_window(0.0, 1000.0 / 60.0, 2, 145.0, 120);
        let now = records.iter().map(|r| r.arrival).max().unwrap();
        let est = estimate(&records, now, 1000.0 / 30.0, Some(145.0)).expect("window");
        assert!((est.clock_gap - FAST_ADJUST_MEAN_MS).abs() < 1e-3);
    }

    #[test]
    fn fps_pps_window_expires() {
        let records = ideal_window(0.0, 1000.0 / 60.0, 2, 50.0, 120);
        let (fps, pps) = fps_pps(&records);
        // 1 s window at 2 cmds per ~33 ms packet: ~60 cmds, ~30 packets
        assert!((fps - 60).abs() <= 12, "fps {fps}");
        assert!((pps - 30).abs() <= 6, "pps {pps}");
        // no samples at all -> zero (and never the 1024 artifact)
        assert_eq!(fps_pps(&[]), (0, 0));
    }

    #[test]
    fn trimmed_mean_drops_outliers() {
        let mut samples = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1000.0];
        assert!((trimmed_mean(&mut samples) - 1.0).abs() < 1e-3);
    }
}
