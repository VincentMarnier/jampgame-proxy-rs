# D-015 — Server-side `cl_timeNudge` neutralisation

Date: 2026-11-02. Status: accepted (user decisions taken before
implementation: neutralise the effect server-side, behind a new cvar defaulting
to off, with a transition log that works without the feature). Evidence:
`docs/reverse-engineering.md` R-026 (the estimator), `rust/src/netstatus_est.rs`
(the estimated range), `rust/src/timenudge.rs` (the policy, unit-tested), the
runtime checks recorded under *Validation* below.

## Context

`cl_timeNudge` is client-only. The release client clamps it to ±30
(`cl_cgame.cpp:2075-2081`) and subtracts it from the clock it stamps into every
usercmd:

```text
cl->serverTime = cls.realtime + cl->serverTimeDelta - tn      (cl_cgame.cpp:2088)
cmd->serverTime = cl->serverTime                              (cl_input.cpp:2144)
```

The netStatus estimator (D-007) recovers `tn` as a range. A range that clears
0 by the configured margin is a confident reading of a non-zero setting; the
user asked to be able to neutralise that setting on the server instead of only
reporting it (D-015.6 tightened the margin rule at the user's request).

## Decisions

### D-015.1 — The correction

When the estimated range clears 0 by the configured margin (D-015.6), rewrite
the cmd's `serverTime` by
`+tn_hat` before the game sees it: `raw = neutral - tn`, so `raw + tn = neutral`
is the clock a `cl_timeNudge 0` client would have stamped. The client's own
setting is untouched; the server simply stops processing its moves on the
shifted timeline.

* `tn_hat` is the estimator's point estimate rounded to the integer cvar value,
  clamped to ±30 (the release client's clamp). A larger reading is evidence of
  a modified client, but the proxy will not amplify it into an unbounded clock
  shift; the excess stays uncorrected.
* Only `serverTime` is rewritten. Angles/buttons/weapon are untouched.

### D-015.2 — Where the rewrite happens

In `sv_client_think_stub`, the retarget of the `call SV_ClientThink` inside the
pristine `SV_UserMove` (D-002.1 technique 3, `0x804e891`). This is the only
per-usercmd call the engine makes for real clients; bots reach
`SV_ClientThink` through `BOTLIB_USER_COMMAND` and are not affected.

Two details matter:

1. **The estimator is fed the raw stamp.** `feed_cmd` runs after the pristine
   call and receives the original `serverTime`. Feeding the adjusted value
   would make the estimator converge to 0 and switch the correction off
   (verified at runtime: the table kept reporting `[-16.2..-11.9]`, true −15,
   while the correction was applied).
2. **`cl->lastUsercmd.serverTime` is restored to the raw stamp** after the
   pristine call. `SV_UserMove` compares the next packet's `cl_packetdup`
   copies against it *before* calling the stub (`sv_client.cpp:1792`); leaving
   the adjusted value there would either re-execute old copies (negative
   offset) or discard genuinely new commands whose raw stamp is within the
   offset (positive offset). The game has already read the adjusted value
   through `trap_GetUsercmd` inside `SV_ClientThink`, so restoring afterwards
   only affects the engine's duplicate bookkeeping.
   `offsetof(client_t, lastUsercmd)` = 132128 (`cl+0x20420`, the 28-byte copy
   at the top of `SV_ClientThink`, objdump-verified on the shipped
   `linuxjampded`).

### D-015.3 — Confirmation and ramp

* The verdict is refreshed at most every 500 ms per client (the netStatus
  table's own rate limit; the window is ~5 s, so faster recomputation would
  read the same data).
* **Two consecutive refreshes must clear 0 by the margin** before the
  correction is applied or logged. The ground-truth harness showed a spurious
  `[-76.4..-60.6]` reading in the first window after a connect (before the
  window settled); acting on it would have shifted the player by the ±30 clamp
  for about a second. A real setting stays outside the range on every refresh.
  A range that contains 0, or stays within the margin of it, disengages at
  once.
* The applied offset moves **1 ms per usercmd** toward the target. A jump of up
  to ±30 ms either fails the game's `msec < 1` guard for several commands
  (`g_active.c:2089-2095`, dropping inputs) or produces one oversized `msec`
  frame. A 1 ms step keeps every command on the client's normal cadence; the
  total clock shift (and therefore the total movement cost) is the same.
* A window reset (`sv_fps`/`snapshotMsec` change, D-007.7) stops the target
  immediately and ramps the offset away; connect/disconnect, feature
  re-enable, and the master switch clear it outright (nothing was applied while
  the feature was off, and a new client starts from a fresh clock).

### D-015.4 — Cvars and log

| cvar | default | behaviour |
|---|---|---|
| `proxy_sv_voidTimeNudge` | 0 | apply the rewrite |
| `proxy_sv_voidTimeNudgeLog` | 0 | log transitions |
| `proxy_sv_timeNudgeMargin` | 2 | margin, in ms, added to the estimate's confidence range and required between the range and 0 (D-015.6) |

The estimator's non-statistical bias floor (D-007's fixed 2 ms) became the
`proxy_sv_timeNudgeMargin` cvar. It widens the reported range, and D-015.6
additionally applies it to the range's end facing 0, so it is the knob that
trades sensitivity for false positives: a larger value leaves small settings
"consistent with 0" (no rewrite, white cell), a smaller value lets the verdict
act on a smaller reading and on more estimator noise. `0` reports the
statistical interval alone; values are clamped to `0..=100` ms. The margin
feeds both the neutralisation verdict and the netStatus `timeNudge` cell, which
use the same `timenudge::offset_for` predicate and therefore cannot disagree.

The log has the `proxy_sv_clientPpsLog` shape: it keeps the estimator feed and
the verdict running on its own, prints one line when a client's confirmed range
starts clearing 0 by the margin (with the shift the feature applies *or would
apply*), one when it goes back to zero, and one when the reading is lost
(stale/filling window). A client on the same side of zero is silent. While the
log cvar is 0 no edge state is kept, so enabling it reports the next non-zero
reading.

```text
----- proxy-rs: timeNudge non-zero: estimate=[-17.0..-11.1] shift=-14 client=0 name="client0" adr="127.0.0.1:50052"
----- proxy-rs: timeNudge back to zero: estimate=[-1.2..3.1] client=0 name="client0" adr="127.0.0.1:…"
----- proxy-rs: timeNudge no reading: estimate=- client=0 name="client0" adr="127.0.0.1:…"
```

The feature is inert unless `proxy_sv_voidTimeNudge`,
`proxy_sv_voidTimeNudgeLog`, `proxy_sv_enableNetStatus`,
`proxy_sv_enableClientPpsLimit` or `proxy_sv_clientPpsLog` is on.

### D-015.5 — The netStatus `timeNudge` cell

The table keeps reporting the estimator's range, coloured like the `packets/s`
cell:

* **red** when the whole range is outside the release client's clamp
  (`hi < -30` or `lo > 30`);
* **yellow** when the range clears 0 by the configured margin but is not
  entirely outside the clamp (a confidently non-zero setting);
* **white** when 0 is in the range **or within the margin of it** (consistent
  with the default setting, even if the printed range excludes 0 — e.g.
  `[0.3..6.1]` with the default 2 ms margin).

With `proxy_sv_voidTimeNudge` active the column instead reports the value the
server enforces: `0` for every client, **red** on the clients whose range clears
0 by the margin (the timenudges being voided) and white on the rest. A
stale/filling window keeps the `-` placeholder (white). Colour codes are not
counted in the column width, so the cells stay aligned; `^7` restores the
default colour for the following columns.

Turning the feature off at runtime stops the rewrite at once (the next command
is raw again); the 1 ms ramp covers verdict changes while the feature stays on,
not an operator disabling it. The estimator keeps its raw feed throughout.

### D-015.6 — The margin also guards the zero-facing end (2026-11-06)

User report: with `proxy_sv_timeNudgeMargin 2` the log emitted
`timeNudge non-zero: estimate=[0.3..6.1] shift=3`. The margin was already in
the range (the CI was widened by it), but the verdict only tested
`0 ∈ [lo, hi]`, so any range that excluded 0 acted. The user wants the margin
applied to the range's minimum or maximum: `[0.3..6.1]` is within 2 ms of 0 on
its lower end and must count as "consistent with 0".

The policy is now:

```text
consistent with 0  ⇔  0 ∈ [lo - margin, hi + margin]
```

For a positive range this tests only the minimum (`lo - margin <= 0`), for a
negative range only the maximum (`hi + margin >= 0`), so the margin is applied
to the end facing 0 as requested. The printed range is unchanged (and still
includes the margin), which means a range must clear the printed lower/upper
bound by the margin: the effective threshold for a zero-width CI is `2 ×
margin` from the point estimate. That is deliberate — the user asked for the
printed `[0.3..6.1]` to be accepted with `2` — and `proxy_sv_timeNudgeMargin 0`
restores the old plain `0 ∈ [lo, hi]` rule.

`timenudge::offset_for` takes the margin, `timenudge::update` receives it from
`hooks::netstatus::compute`, and `tn_cell` colours on the same predicate, so the
verdict, the log and the table cannot disagree. Unit tests pin the reported
case, the boundary (`lo == margin` is inside), both signs, a zero margin, and a
malformed/negative margin. This supersedes the D-015.4 wording that the margin
only widens the range.

## Validation

**Unit tests** (`rust/src/timenudge.rs`): range-includes-0, rounded/clamped
offset, non-finite guards, the margin guard on the zero-facing end (the reported
`[0.3..6.1]` case, both signs, boundary, zero/negative/malformed margin),
refresh cadence and clock jumps, the 1 ms ramp in both directions,
confirmation, stop-keeps-edge, and the three log lines.
**Unit tests** (`rust/src/hooks/netstatus.rs`): the `timeNudge` cell colours on
the same verdict (the reported range stays white with the default margin and
turns yellow with `0`).

**Runtime (2026-11-02, the a2 synthetic-client harness in the i386 container,
`--mode single --tn -15 --lan`, proxy built from this change):**

* Verdict/log: only the settled `[-17.0..-11.1] shift=-14` line is emitted; the
  startup artifact seen without confirmation (`[-76.4..-60.6]`) no longer
  triggers an action.
* `tn = 0` run: no transition line at all (the range `[-1.2..3.1]` contains 0).
* Log-only (`proxy_sv_voidTimeNudge 0`, log 1): the line is emitted with the
  shift, proving the cvar works on its own.
* The table rendition was checked directly: with the feature on from boot, the
  row shows `^1            0^7` for the tn = −15 client (red zero); after the
  a3 run toggles the feature off, the scored row reports
  `[-16.2..-11.9]` (true −15, `inside yes`) again — the estimator never
  converged to 0 while the correction was active (raw feed).
* **The rewrite itself**, measured by `tools/runtime/a3_timenudge_void.sh`
  (reproducible): it reads `level.clients[0].ps.commandTime`
  (`level.clients` is at `level+0` `g_local.h:813`, `ps.commandTime` at
  `gclient_t+0`) against `level.time` (`level+32`) once per `GAME_RUN_FRAME`,
  120 frames before and after an rcon toggle of `proxy_sv_voidTimeNudge` in the
  same run. Median `commandTime - level.time` moved from **−25 ms to −39 ms**,
  exactly the −14 ms correction. The script then disables the feature again and
  requires the client's scored netStatus row to report −14.05 (true −15,
  `inside yes`), with no `timeNudge back to zero` line in between: the
  estimator never converged while the correction was active. (`level`/
  `g_entities` are dynamic symbols; `level` at `orig_base+0x68a3a0`, `vmMain`
  at `+0x85684`.)

## Not verified / open

* The effect on a **real client's** prediction and hit registration is not
  measured. The server now processes the moves on the neutral clock while the
  client's own cgame still runs on the shifted one, so the client may see more
  reconciliation movement; that is inherent to a server-only correction.
  Proposal: reuse `tools/runtime/a2_netstatus_groundtruth.sh --mode real` (a
  real TaystJK client) with the feature on and compare perceived
  movement/aim against the same session with it off.
* A modified client that ignores the snapshot-derived clock (or bypasses the
  release clamp) can still stamp arbitrary times; the proxy can only shift by
  the ±30 it can estimate. Out-of-range readings are logged but not corrected
  beyond ±30.
* The log has no separate "enforcing" field: with the feature off the line
  reports the shift that *would* be applied, like the pps log reports the
  verdict without enforcement.

## Consequences

* Raw usercmd timing is no longer what the game sees for flagged clients; the
  engine's own duplicate filtering stays on raw stamps (D-015.2).
* The correction is opt-in and off by default: existing servers keep the
  pristine timeline exactly.
