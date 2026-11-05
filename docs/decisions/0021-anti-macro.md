# D-021 — Anti-macro input guard

Date: 2026-11-03. Status: accepted (user directive). Evidence: JKA source
(`original/jedi-academy/codemp/`), the Raven SDK headers
(`original/jedi-academy-sdk/`), the original proxy at the pinned commit
`6879974` (`Proxy_g_local.cpp`, recovered from the retained submodule history),
and two operator reports from a live server (the `wait 10` turn-key script and
"still undetected", see "Reported bind"). No capture of legitimate players'
run statistics exists yet (see "Uncertainty").

## Context

Two config-file macros matter on the servers this proxy runs:

- **scripted ground strafe** — A/D (or forward/back) alternation every few
  commands, which makes the player faster than a hand can;
- **view wiggle** — a rapid yaw/pitch oscillation used against the saber's
  per-frame position update (`WP_SaberPositionUpdate`).

The user's rule: hand-played versions stay legal; scripted ones are silently
neutralised. Auto-attack timing and aim snap were named as possible but
ambiguous against skilled players and are **not** part of this decision.

## What the server can observe

The initial idea — "forbid opposite actions" — does not survive the wire. The
client sums opposing keys before sending and clamps the sum
(`codemp/client/cl_input.cpp:1637-1666`):

```c
side += movespeed * CL_KeyState (&in_moveright);
side -= movespeed * CL_KeyState (&in_moveleft);
...
cmd->rightmove = ClampChar( side );
```

So `+moveleft` and `+moveright` held together arrive as `rightmove == 0`, and
"not jumping" is indistinguishable from not pressing a key. Opposing *key
states* are not observable on the server at all. What is observable is the
**sign of the summed channel changing between consecutive commands**, i.e. a
reversal.

Magnitude is not a discriminator. `rightmove`/`forwardmove` are `±127` (run),
`±46` (walk) or `0` for a hand and a macro alike (same lines), and a turn key
rotates the view by `cl_yawspeed * CL_KeyState * frame_time`
(`cl_input.cpp:1550-1566`) — at 200 fps with the stock `cl_yawspeed 140` that is
0.7° per command for both.

`usercmd_t.angles` are `ANGLE2SHORT` values (`cl_input.cpp:2198`,
`q_shared.h:1878`), i.e. 16-bit fixed point with a 0/65536 wrap; the shortest
path is the signed 16-bit difference, and the client's `delta_angles` offset
cancels in a delta.

## Why a *run* of reversals, and why the rate/spread

Regularity is not an invariant of a macro. `wait N` is a frame count — `Cmd_Wait_f`
stores it (`cmd_common.cpp:36-53`) and `Cbuf_Execute` decrements it once per
frame (`cmd_common.cpp:292-299`), while `CL_CreateNewCommands` builds exactly one
usercmd per frame (`cl_input.cpp:2266-2313`) — so a *plain* `wait 10` script
reverses after an exact command count. But a script may rotate through several
wait counts (`wait 8; …; wait 12; …`), which keeps the effect and destroys the
count.

More importantly, **a bind is a burst, not a stream**. The operator's bind is
six holds long: five reversals, then the script ends and the key must be pressed
again. Measuring over a fixed window (the second and third implementations) mixes
those five reversals with the human gap between presses, and the burst vanishes
into the average. The unit must therefore be a **run**: reversals chained while
the interval to the previous one is short, reset by a longer gap.

Within a run, two independent tests are used, and a channel fires on either:

- **rate** — the mean interval is below the hand limit. The unit is **commands**
  for movement (the payoff is per command: ≤ 3) and **milliseconds** for
  view/attack (the payoff is per second: ≤ 65 ms and ≤ 60 ms). Four reversals are
  enough: three frame-exact intervals at 200 fps is 18 alternations/s. The rate
  is the payoff itself:
  - ground acceleration is applied per command (`PM_WalkMove` builds
    `wishvel = forward*fmove + right*smove`, `bg_pmove.c:3262-3272`, then
    `PM_Accelerate` adds along `wishdir`, `bg_pmove.c:1111-1144`), and a command
    that repeats the previous direction contributes nothing (`addspeed <= 0`),
    so a profitable strafe must reverse every few commands;
  - the view wiggle only works by moving the blade back and forth per frame, so
    a slowing script loses its effect in proportion.
  - a **keyboard** can mash two turn keys at 8-12 reversals/s, but a mouse can
    be wiggled faster than that, so rate alone is not enough for the view. A
    config macro drives a turn key (`cl_input.cpp:1530-1531`), which is
    bang-bang: once held, `CL_KeyState` is 1.0 every command
    (`cl_input.cpp:534-566`), so the per-command step is near-constant. A mouse
    wiggle accelerates and decelerates, so its mean step is far below its peak.
    The view rate test therefore also requires the mean step to reach 75 % of
    the peak (a sinusoid sits at ~64 %), and a swing of at least 2° between
    reversals; a smaller oscillation is dominated by the 0.2° noise floor and is
    not the blade-wiggle payoff. Amplitude is used as a floor, never as a
    positive signal.
- **spread** — after four reversals, the hold durations lie within a 10 ms
  range. A config `wait` replays the same frame count, so its holds are exact in
  time at any frame rate; a hand's timing jitters by roughly ten milliseconds.
  This is what catches a slow script, or one on a channel whose payoff does not
  force a high rate.

Randomising the wait counts over a small set keeps the spread narrow, so the
spread test still fires; a wide randomisation widens the spread but leaves the
rate, which is unchanged.

The first implementation measured the range in **commands** (2, calibrated at
200 fps). That is frame-rate dependent: a `wait N` script has a constant *frame
count*, so its time is exact, while a hand's jitter is a constant *duration*. At
the stock `com_maxfps 85`, ±10 ms is under one command, so a player alternating
at 5-6 reversals/s with a two-command range was read as a `wait` script and
locked. The range is now measured in **milliseconds** (10 ms is the same
allowance the old rule gave at 200 fps), which is the frame-rate-independent
form of the same test: a `wait` is still exact, an ordinary rhythm is not.

A second live report (2026-11-05) was a hand-driven **mouse wiggle** at a rate
past the 15/s key-mashing ceiling: the rate test fired on it. The key-shape
test in the rate bullet is the fix; a mouse's step profile is not bang-bang.

After a channel fires it is **wary** for 3 s, and the
next burst is caught after three reversals instead of four, so repeated presses
are neutralised earlier each time.

Meanwhile a hand has a ceiling on *keys*: alternating two keys (or mashing the
two turn keys) tops out at roughly 8-12 reversals per second, and it cannot hold
a movement axis at a reversals-every-1-3-commands cadence at all. A mouse is not
so limited — a hand can wiggle it faster — which is why the view rate test also
needs the key-shape check above.

### Reported bind (operator reports, 2026-11-03)

```text
bind "W" "+right;wait 10;-right;+left;wait 10;-left;+right;wait 10;-right;+left;wait 10;-left;+right;wait 10;-right;+left;wait 10;-left"
```

At 200 fps this is six holds of 11 commands (55 ms each) — **five reversals per
key press**. Three implementation misses were found and fixed:

1. a 1° magnitude floor rejected every 0.7° sample; the floor is now 0.2°;
2. a 40 ms cadence cap rejected the 55 ms interval; cadence is no longer a test;
3. measuring over a 4 s window averaged the five-reversal burst with the gap
   until the next press (`mean=443ms/88cmd holds=5..1364` in the operator's
   dump, from a burst that was present the whole time). The run design above is
   the fix; the unit test reproduces this bind exactly and fires on its fourth
   reversal.

## Prior art in this repository

`proxy_sv_sabersFps` (dropped in D-002) hooked the same area from the other
side: it throttled `WP_SaberPositionUpdate` to `sv_fps / sabersFps` frames per
second (recovered `Proxy_g_local.cpp`, `Proxy_WP_SaberPositionUpdate`). Two
observations:

- it confirms the family (the original proxy also treated frame-rate-driven
  saber updates as an exploit surface);
- its implementation kept a single global `lastProcessedSaberFrame` for all
  players/sabers, so only one entity per frame was updated — evidence that the
  original was never a per-player mechanism, and a reason it was dropped here.

This decision does **not** revive it: the guard acts on the input that drives
the movement, per player, and stays opt-in.

## Design

Module: `rust/src/macroguard.rs` (pure logic, no FFI; fed by the existing
retargeted `call SV_ClientThink` stub — `hooks/netstatus.rs`,
`sv_client_think_stub`). No new hook is attached.

Channels and observations:

| channel | observation | neutralisation while locked |
|---|---|---|
| `forwardmove`, `rightmove`, `upmove` | sign of the raw `i8`, magnitude `>= 90` | the axis keeps its last unlocked value |
| `yaw`, `pitch` | signed 16-bit delta of the raw angle, `>= 36` units (≈0.2°) | the angle keeps its last unlocked value |
| `attack` | `BUTTON_ATTACK`/`BUTTON_ALT_ATTACK` mask toggling | the mask keeps its last unlocked state |

Constants (inferred, see below):

| constant | value | why |
|---|---|---|
| `RUN_MAX_GAP_MS` | 200 | reversals chained within this gap form one run; a press-to-press pause is far longer |
| `RUN_MIN_REVERSALS` | 4 | three frame-exact intervals is beyond a hand |
| `SPREAD_MIN_REVERSALS` | 4 | hold lengths are only trusted as a set after three holds |
| `MAX_HOLD_SPREAD_MS` | 10 | a `wait` replays an exact frame count, so its hold time is exact; a hand's jitter is ~10 ms (2 commands at 200 fps) |
| `WARY_MIN_REVERSALS` / `WARY_MS` | 3 / 3 000 | after a fire, the next burst is caught two intervals in |
| `AXIS_MAX_MEAN_COMMANDS` | 3 | the acceleration payoff needs a reversal every few commands; a hand cannot |
| `VIEW_MAX_MEAN_MS` | 65 | ~15 reversals/s; the reported bind is 55 ms at 200 fps |
| `VIEW_STEP_MEAN_PERCENT` | 75 | a view run must also look like a held key (mean per-command step ≥ 75 % of the peak); a mouse wiggle is not bang-bang |
| `VIEW_MIN_SWING` | 364 | ≈2° peak-to-peak between reversals; below it the shape measure is dominated by the 0.2° floor and the wiggle is negligible |
| `ATTACK_MAX_MEAN_MS` | 60 | ~17 toggles/s; not a human click rate |
| `RELEASE_MS` | 400 | "until they stop": released 400 ms after the last reversal |
| `STALE_GAP_MS` | 400 | a pause/rejoin/clock rewind forgets every run and lock |
| `AXIS_MIN` | 90 | run input is 127, walk 46; keeps walk-speed scripts out |
| `ANGLE_MIN_STEP` | 36 | 0.2°: below it the step is tremor or a partially-held transition frame (ignored, not neutral) |
| `DEBUG_PERIOD_MS` | 1 000 | `proxy_sv_antiMacroLog 2`/`3` statistics cadence |

Behaviour:

- the guard counts the commands the server actually sees; a lost packet removes
  reversals from a run, which lowers the measured rate rather than raising it,
  so a lossy link cannot cause a false lock;
- the observer reads the **raw** command, so rewriting a locked channel cannot
  hide the pattern that keeps the lock alive;
- **view freezing is opt-in** (`proxy_sv_antiMacroView`, default `0`): the view
  verdict is computed and logged, but `yaw`/`pitch` are not rewritten. A hand can
  wiggle a mouse faster and more regularly than it can mash turn keys, so a
  hand-driven wiggle can satisfy the rate and even the spread signature; the
  server cannot reliably tell it from a key-driven wiggle. The default therefore
  keeps hand play untouched, and the movement/attack channels (whose hand limits
  are unambiguous) remain enforced. This is a deliberate departure from the
  original scope after two live false-positive reports;
- the lock is applied to the command **before** `SV_ClientThink`, so the game's
  `pers.cmd` (through `G_GET_USERCMD`) sees the frozen value; the client
  predicted from its own input, so its view/movement is corrected by the next
  snapshot instead of the command being rejected. `serverTime` is untouched, so
  the engine's duplicate filter (`SV_UserMove`,
  `codemp/server/sv_client.cpp:1792`) is unaffected;
- the guard runs *before* the `voidTimeNudge` `serverTime` rewrite, so it
  always observes the client's own clock;
- nothing is sent to the player. `proxy_sv_antiMacroLog` prints to the server
  console only, after the state lock is released (the engine print trap must not
  run under the proxy mutex — same rule as `crate::pps`).

## Observability

`proxy_sv_antiMacroLog 3` prints, once per second and per client, one line per
channel with a run (`1` logs transitions only, `2` only locked channels), so an
operator can see exactly what the server receives:

```text
jampgame-proxy-rs: antiMacro debug client=2 name="..." cmds=6041 yaw \
  rev=4 mean=55ms/11cmd holds=11..11 holdms=55..55 step=127..127 \
  swing=6deg locked=1
```

`cmds` is the command counter (proves the feed is alive), `rev` the reversals in
the current (or most recent) run, `mean` the mean interval, `holds` the
shortest/longest hold in commands, `holdms` the same range in milliseconds (the
spread test runs on this), `step` the mean/peak per-command view step (the
key-shape test runs on this), `swing` the view swing (the key-shape test also
requires it to clear 2°) and `locked` the verdict. With
`proxy_sv_antiMacroView 0`, a view `locked` means "would freeze", not "froze".
This is the instrument used for the measurement phase, and the first thing to
look at if a script is not caught.

## Uncertainty and experiment

The thresholds are **inferred** from the mechanism and the operator reports; no
capture of legitimate players' reversal statistics exists yet. Three
false-positive reports have been investigated (2026-11-05): the command-count
spread test at the stock frame rate (fixed by measuring the range in time), a
hand-driven mouse wiggle past the key-mashing rate (fixed by the key-shape
test), and a steady wiggle that also matched the spread signature. The last one
shows the view cannot be separated reliably enough to freeze by default, so
`proxy_sv_antiMacroView` now gates enforcement. The experiment, before trusting
enforcement of the movement/attack channels:

1. run a populated server with `proxy_sv_antiMacroLog 2` and
   `proxy_sv_antiMacro 0` for a few sessions (duels, CTF, TFFA);
2. read the `debug` lines: a legitimate channel firing (a view run at ≤65 ms
   with `step` mean near its peak, or a 3-command mean on a movement axis) means
   the thresholds are too loose;
3. only then enable `proxy_sv_antiMacro`, keeping the log on.

If the log shows false positives, raise `RUN_MIN_REVERSALS` first and lower
`VIEW_MAX_MEAN_MS`/`AXIS_MAX_MEAN_COMMANDS` second; read `holdms` and `step`
before changing `MAX_HOLD_SPREAD_MS`/`VIEW_STEP_MEAN_PERCENT`. Do not add
per-player exceptions.

Known limits:

- the view rate test needs a bang-bang step profile (a held key) and a swing of
  at least 2°. A macro that drives the view some other way, or a hand that can
  produce constant-speed mouse reversals at that amplitude, is not separated.
  Swing is used only as a floor: an upper bound would depend on `cl_yawspeed`
  and could be evaded by turning faster. Because this is not reliable, view
  freezing is opt-in ([`proxy_sv_antiMacroView`](#cvars)); even then, treat a
  view verdict as a signal to inspect, not proof;
- a `wait 10` view burst at 125 fps reverses every 88 ms, inside hand speed, so
  the *rate* test leaves it alone — but its holds are exact, so the *spread*
  test catches it;
- a script that both randomises its waits widely and stays at hand speed
  defeats both tests; that is indistinguishable from a hand at the input level,
  and it is correspondingly weaker;
- a single aim snap is not a reversal and is not detected;
- attack timing inside the game's own `weaponTime` gate is not detected;
- a modified client can send human-like commands while doing the work
  client-side only when the *effect* is client-side too; movement and saber
  position are server-side, so the profitable part still has to be on the wire.

## State and ABI

- `ClientEntry` gains a proxy-local `MacroState` (plain data, `Copy`): no engine
  or game structure changes, no new symbol, no calling-convention change.
- Reset rules: dropped with the rest of the entry on disconnect (slot reuse),
  explicitly cleared on `GAME_CLIENT_CONNECT` `first_time`
  (`macroguard::reset_client`), and rebuilt by `reset_session` (a `map_restart`
  or map change starts clean — the next burst is re-detected within four
  reversals, ~0.2 s).
- Bots never reach the stub (their commands come from `BOTLIB_USER_COMMAND`),
  so no bot exemption is needed; spectators do reach it, which is intentional
  (a spectator macroing strafe still gets frozen and learns nothing to use).

## Cvars

| cvar | default | meaning |
|---|---|---|
| `proxy_sv_antiMacro` | 0 | freeze locked channels; detection runs while this or the log cvar is on |
| `proxy_sv_antiMacroLog` | 0 | `1` lock/release transitions, `2` plus the statistics of locked channels, `3` plus every channel with a reversal (diagnostic); works alone (reports, does not apply) |
| `proxy_sv_antiMacroView` | 0 | `0` compute and log the view verdict but never freeze `yaw`/`pitch`; non-zero freezes a locked view. Opt-in because a hand mouse wiggle is not reliably separable from a key-driven view wiggle at the wire level |

## Tests

`rust/src/macroguard.rs` unit tests (30):

- the operator's exact bind (six holds → five reversals) fires on its fourth
  reversal, and a second press fires on its third while the channel is wary;
- randomised wait counts inside one burst (`wait 9..13`) fire on the mean;
- a slow exact burst (`wait 30`) fires on the hold spread where the rate test
  would not, and a frame-exact burst at the stock 85 fps still fires;
- a human rhythm at that frame rate (5-6 reversals/s with two commands of
  jitter) stays untouched on the view and attack channels — the regression that
  moved the spread metric from commands to milliseconds;
- a hand-driven mouse wiggle past the key-mashing rate (ramped per-command
  steps) stays untouched, while the same irregular hold lengths driven by a key
  are detected — the key-shape test; a sub-2° bang-bang wiggle stays untouched
  — the swing floor; a steady wiggle with exact hold timings stays untouched —
  every view verdict is gated by the shape;
- `process_with_view(..., apply_view = false)` still detects and locks the view
  for the log but leaves the raw angle alone — the opt-in enforcement;
- a hand-driven turn alternation (4-6 reversals/s, irregular holds), a hand
  strafe rhythm, a smooth turn, walk-speed (`±46`) and a lifted key stay
  untouched; a partially-held (sub-floor) transition frame does not break the
  run; the debug payload reports mean 55 ms / 11 commands / 55 ms holds /
  step 127 / ~7° swing;
- ground strafe every command locks and flattens the axis; randomised `wait 1..3`
  is detected;
- a frame-rate attack toggle locks, a human click rate (irregular holds) does
  not; level 2 stays quiet while nothing is locked, level 3 reports the run;
- release after the reversing stops (and not before `RELEASE_MS`), wrapped yaw
  measured shortest-path, log-only mode never rewrites, clock rewind, forward
  and up independent;
- plus the `PROXY_CVARS` default/name tests in `state.rs`.

`cargo test --lib` (390), `cargo clippy --all-targets`, `cargo fmt --check` and
`./scripts/build.sh` (i686 cdylib) pass.

## Non-goals

- auto-attack / attack-timing filters (ambiguous against skilled players);
- aim-snap detection (not a reversal);
- reviving `proxy_sv_sabersFps`;
- kicking, muting or notifying the player.
