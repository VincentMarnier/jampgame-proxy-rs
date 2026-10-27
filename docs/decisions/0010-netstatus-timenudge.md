# D-007 — Reliable netStatus `timeNudge` estimation

Date: 2026-10-26. Status: accepted (user-approved plan + three user decisions
taken during implementation, 2026-10-26). Evidence: `docs/reverse-engineering.md`
R-026 (the estimator derivation and every engine constant below),
`tools/runtime/a2_netstatus_groundtruth.{sh,py}` (the ground-truth harness),
`rust/src/netstatus_est.rs` (the estimator, unit-tested).

> Numbering: the planning session proposed `docs/decisions/0007-*`; 0007-0009
> were already taken, so this record is `0010`.

## Context

The `netStatus`/`showNet` table's `timeNudge` column reported the player's
`cl_timeNudge` through the original proxy's magic formula:

```text
timenudge = -(mean(delay) + mean(ping) - magicOffset(sv_fps) + 1000/sv_fps)
```

`magicOffset` is a hardcoded 61-entry table indexed by `sv_fps` alone, with
"DO NOT USE" holes at 31-34, 41-44, 59, 60-62 (original C++ proxy,
`Proxy_Engine_Client.cpp:45-107`). Everything the correction term must absorb
is **per-client** — the client's cmd rate, its packet pacing, the
`cl_maxpackets` rate that is *never transmitted to the server*, and the
LAN/loopback client whose packet timer is bypassed entirely — so a table
indexed by `sv_fps` cannot know it. Nothing validated the number against
ground truth: `tools/runtime/a1_netstatus_semantic.py` recomputed the same
formula in Python, which proves the Rust port matches the C++ arithmetic, not
that the arithmetic is right.

`cl_timeNudge` is never transmitted. Its only effect is the clock the client
stamps into its usercmds (`cl->serverTime = cls.realtime +
cl->serverTimeDelta - tn`, clamped to ±30 in release builds), so it can only
be inferred.

## Decisions

### D-007.1 — What the column reports

The **`cl_timeNudge` setting** of the player (a diagnostic), not a derived
"effective offset". Out-of-range readings are displayed **raw, never clamped**
to ±30: an out-of-range reading is evidence of a modified client.

### D-007.2 — Model-based estimator, magic table deleted

Deliberate deviation from the original proxy: `get_timenudge_magic_offset` and
its table are **deleted**. The estimator is a model of the client's clock
construction, per usercmd packet `k` (one `clc_move`/`clc_moveNoDelta` message,
new cmds only — `cl_packetdup` duplicates are dropped by `SV_UserMove` before
the feed):

```text
raw_k    = S_j - Tbar_k                      (S_j: messageSent of the acked
                                              message, Tbar_k: mean cmd
                                              serverTime in the packet)
tn_hat_j = raw_j - mean_age_j + ackdelay_j - clock_gap
mean_age_j = (s_max_j - Tbar_j) + I_cmd      (mean cmd age at send)
ackdelay_j = (min(I_pkt, I_msg) - I_cmd) / 2 (wait for the first packet that
                                             acks the message)
clock_gap = I_msg + 5 - I_cmd                (CL_AdjustTimeDelta steady state,
                                             saturating at 75 ms)
```

Derivation: R-026. Reported as the **robust mean** of `tn_hat_k` over a ~5 s
window (10% trimmed mean), with the range = the 95% CI of the mean widened by
a **fixed bias floor** (±2 ms, settable from the ground-truth runs).

### D-007.3 — Correction terms (user decisions, 2026-10-26)

Several terms differ from the planning session's derivation. Each was
re-derived from engine source, approved by the user, and measured against the
ground-truth matrix (the per-term debug logging used for that attribution was
removed with `proxy_sv_netStatusDebug`):

| term | plan | implemented | why |
|---|---|---|---|
| `lag_k` | `(n_k-1)/(2f)` | `(s_max - Tbar) + I_cmd` | `CL_SendCmd()` runs before `CL_SetCGameTime()` in `CL_Frame` (cl_main.cpp:1846/1852), so the newest cmd is stamped with the *previous* frame's clock: one frame period (`I_cmd`) older than the plan's term |
| `ackdelay` | `n_k/(2f)` (packet interval) | `(min(I_pkt, I_msg) - I_cmd)/2`, first-acking packet only | the paired RTT carries the age of the newest server→client *message* at the packet's send. Only the first packet that acks a message is sampled — its wait is bounded by the packet interval even when the message was delayed or an earlier packet was lost — and the wait is the residual of the packet grid on the client's frame grid. Later packets acking the same message are not sampled: their wait correlates with their group size. Matrix bias: 0.67 ms here vs 3.86 ms (all-packet lattice) and 9.07 ms (`min(I_pkt, I_msg)/2`) |
| `E` | unobservable, in the ±2 ms bias floor | `I_msg + 5 - I_cmd`, saturating at 75 ms | measurement: `E` is the steady state of `CL_AdjustTimeDelta`'s `+1`/`-2` walk (~40 ms, not a drift); the `5` is the engine's `cl->snap.serverTime - 5` extrapolation margin (cl_cgame.cpp:2099-2100). Past `deltaDelta > 100` (cl_cgame.cpp:1913-1918) the walk is halved instead of stepped, and the gap cycles over [50, 100] (mean 75) — measured 71.9/70.9/73.9 ms at `snaps 10` where the slow model predicts 130/106/102 |
| `q_send` | `frame_msec/2` | `0` (floor covers it) | `SV_Frame` runs a stepped `svs.time` (`+= frameMsec` in the residual loop) and sends snapshots in the same call, so the send-side quantization is the `NET_Sleep` overshoot (~0..1 ms) |

The plan-literal variant is still computed (`tn_plan`) and pinned by a unit
test, so the deviation from the plan's derivation stays measurable. One sample
per acked message, so the CI's sample count is the number of distinct
messages.

### D-007.4 — What is measured, and from where

* `cl->frames[]` is the RTT source (not `cl->ping`): per packet,
  `messageSent` of the frame the packet acks
  (`cl->frames[messageAcknowledge & PACKET_MASK]`, stride 3160,
  `PACKET_BACKUP` 32 — both binary-verified, R-026). `messageAcknowledge` is
  taken from *this* message's header (parsed by the `SV_ExecuteClientMessage`
  wrapper): `cl->messageAcknowledge` in memory is still the previous message's
  ack at that point.
* `I_msg`, the server→client message spacing, is measured from `frames[]`
  `messageSent` spacing — observed traffic. **`cl->snapshotMsec` (the `snaps`
  userinfo key) is never used for a correction** and `cl_maxpackets` is never
  transmitted to the server (R-026), so any packet-side quantity must come
  from the observed cmd stream (`I_cmd`, `I_pkt = n_k * I_cmd`).
* fps/pps: cmds and packets per second over the last 1 s of cmd `serverTime`
  (expiry-based ring; the original's never-written ring slots produced the
  post-map-load `fps = 1024` artifact).

### D-007.5 — Window, range syntax, change detection

* ~5 s estimation window, fixed-size ring (512 packets, covers the window up
  to ~100 packets/s), nothing allocates on the move-packet hot path.
* Cell syntax `[-3..-1]`; `-` when the window is stale (no recent cmds), still
  filling (<1 s of samples) or holds too few samples (<8).
* Step-change detection: the two half-window means differ by more than 4 ms
  **and** by more than 3 pooled standard errors → the pre-step half of the
  window is dropped, so a `cl_timeNudge` change shows within ~2 s.

### D-007.6 — Table and surface

Columns `ping rtt jit fps pps timeNudge snaps rate name` (user directive
2026-10-27: the `score`/`id` columns of the plan's list are dropped — the id
is only of interest inside the proxy and the score is on the scoreboard
already — while the name keeps its trailing position). `rtt` is the round trip from `cl->frames[]`, `jit`
the standard deviation of the per-packet RTT samples over the window, `pps`
the observed packets per second (never `cl_maxpackets`, which the server
cannot see).
`netStatus` and `showNet` share the table. Kept from the original: the
≤1012-byte `print "..."` chunking, name escaping (`"`/`\`/newline), the 500 ms
rate limit, the `gentity->r.svFlags & SVF_BOT` bot exclusion (R-022
deviation), and zero-ping localhost/LAN players staying included.

~~New cvar `proxy_sv_netStatusDebug`~~ — **dropped at the user's request
(2026-10-26, overriding the plan's decision 12)**: no estimator internals are
exposed at runtime and no extra cvar is registered. The estimator keeps its
per-term decomposition internally (pinned by the unit tests) and the table is
the only surface. The validation below was measured *with* the debug rows
while the instrument existed; today the same numbers would be scored from the
printed table's `timeNudge` cell (`[lo..hi]`, symmetric around the estimate),
which is what `tools/runtime/a2_netstatus_groundtruth.py` now does.

### D-007.6b — Completely inert when disabled (user directive, 2026-10-26)

With `proxy_sv_enableNetStatus 0` nothing runs: the per-cmd feed, the
per-packet record work in the `SV_ExecuteClientMessage` wrapper, the
`netStatus`/`showNet` handler and every estimator computation are all gated
on the cvar. The only remaining cost is one cvar mirror read per usercmd
message. The off→on edge resets every window (see D-007.7), so nothing stale
is shown after re-enabling.

### D-007.7 — State reset rules

Any of these invalidates the window: map change/restart (`GAME_INIT`, the
R-023 precedent), client connect/disconnect, `sv_fps` change, the client's
`snapshotMsec` change, `proxy_sv_enableNetStatus` off→on.

## Validation

Ground truth = the `cl_timeNudge` fed to the synthetic client of
`tools/runtime/a2_netstatus_groundtruth.py`, which speaks the real protocol
and reproduces the real client timing loop (`CL_SetCGameTime`,
`CL_AdjustTimeDelta`, `CL_CreateCmd`, `CL_WritePacket`, the `cl_maxpackets`
timer and the LAN/loopback bypass). Matrix: `sv_fps` 20/30/40/60/80, client
fps 30/60/125, `snaps` 10/20/30, `cl_maxpackets` 15/30/100, LAN vs non-LAN
packet mode, `tn` ∈ {-30,-15,0,+15,+30}, plus loss/jitter cases — 250 client
runs, 239 scored (the rest lost their debug rows to the run's stalls).

Measured over the matrix (2026-10-26, `tools/runtime/a2_netstatus_analyse.py`):

| class | n | mean error | p95 \|err\| | max | range width mean |
|---|---|---|---|---|---|
| all | 239 | +1.84 ms | 10.19 | 28.69 | 9.6-14.2 ms |
| clean (no loss/jitter) | 175 | +1.74 | 10.32 | 28.69 | 9.6 ms |
| 10% loss | 18 | +3.28 | 10.16 | 10.16 | 10.7 ms |
| 5% loss + 15 ms jitter | 21 | +2.98 | 20.50 | 23.72 | 14.2 ms |
| 20 ms jitter | 25 | +0.61 | 3.11 | 3.80 | 10.1 ms |

Median \|error\| 2.44 ms; 43% of the cells are within 2 ms.

The acceptance (≤2 ms synthetic, range width ≤5 ms) is **not met**; the
residual is attributed to two identified families, both under active
investigation (report-and-decide, user 2026-10-26):

1. **`snaps 10` cells** (errors −21..−29 ms): the `E` regime boundary needs the
   *snapshot* spacing, and `cl->frames[]` mixes snapshot and non-snapshot
   messages (`messageSent` spacing 108 ms vs the snapshot spacing 116.9 ms in
   m043), so a `slow_gap` of 96 ms is classified slow when the truth (102 ms)
   is already in the fast-adjust regime (75 ms).
2. **`cl_maxpackets 15` cells** (errors +22..+24 ms): when packets are much
   sparser than messages the pairing shifts to non-snapshot messages, whose
   `messageSent` differs from the snapshot's `serverTime` the client-side
   ground truth uses (16.5 ms in m015), and the window shrinks (19 samples vs
   ~150) because the step detector fires on the shifted level. The
   first-packet wait itself matches the client-side measurement (22.5 ms
   measured vs 24.7 modelled).

**Real-client runs (2026-10-26, `--mode real`, the plan's step 5 second
half):** **TaystJK** (the production client — a modern x86_64 rebuild; player
name `Definetly not myway`, 200 fps via `com_maxfps`, one packet per client
frame, i.e. the LAN bypass — against `sv_fps 20`, `snaps 20`) set
`cl_timeNudge` to 0 / -30 / -15 / 0 / +15 / +30, one settled segment each:

| set | estimate (mean of segment) | sd | error |
|---|---|---|---|
| 0 | +1.01 | 0.68 | +1.01 |
| -30 | -28.88 | 0.46 | +1.12 |
| -15 | -13.92 | 0.59 | +1.08 |
| 0 | +1.11 | 0.65 | +1.11 |
| +15 | +16.07 | 0.44 | +1.07 |
| +30 | +30.97 | 0.02 | +0.97 |

**Acceptance: met** for the real-client criteria — every point estimate is
within **1.12 ms** of the true setting (≤ 3 ms target), and the true value is
inside the reported range for all six segments. The residual is a stable
**+1.07 ms** bias, consistent with the send-side quantization `q_send` that
the model sets to 0 (the `NET_Sleep` overshoot; D-007.3) and covered by the
±2 ms bias floor. The range width is 6.7-7.0 ms (the ±2 floor dominates the
CI here: `se` is only 0.4-0.7 ms); hitting the ≤ 5 ms width target would mean
narrowing the floor to ±1.5 ms, which still covers the observed bias.

Every engine fact the model rests on held on this run: the one-frame-stale
stamp (`lag` = 5.0 = `I_cmd` at 200 fps), the LAN bypass (`I_pkt` = `I_cmd`),
the `CL_AdjustTimeDelta` walk (`egap` = 50 = `I_msg + 5 - I_cmd`), and the
per-message ack pairing (`samples` = `msgs`). That is direct evidence that
**TaystJK keeps the 1.01 engine's client timing loop**, at least for these
four constants in the LAN/loopback regime — its `cl_maxpackets` packet-timer
path (non-LAN) and its `cl_timeNudge` clamp were not exercised here.

## Consequences

* `netStatus` output is not compatible with the original proxy's table
  (accepted: no compat contract). `tools/runtime/diff_netstatus*`, which
  compared the two tables, is retired with it.
* The estimator is pure (`rust/src/netstatus_est.rs`) and unit-tested against
  synthetic windows whose ground truth is known exactly.
