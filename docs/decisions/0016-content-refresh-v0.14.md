# 0016 — Content refresh for the v0.14 line

Date: 2026-11-06. Status: accepted. Extends 0002 (content from `main`), 0013
(v0.13.1 refresh) and 0014 (concise feature overview).

## Context

The site was last content-synced against `main` at `74cb030` (`v0.13.1`,
ADR 0013). `main` has since moved to `031f2ea` (`v0.14.0`) through `78c2f6c`
(`v0.13.2`) and `31e5e54`. The changes are all in already-documented features;
no new feature section is needed.

- **Anti-macro view freezing became opt-in** (`78c2f6c`, amendment to D-021):
  `proxy_sv_antiMacroView` (default `0`) computes and logs the view verdict but
  does not freeze `yaw`/`pitch`; movement and attack are still frozen. Hand
  mouse wiggles cannot be separated reliably from key-driven view wiggles at
  the wire level, after live false-positive reports.
- **Anti-macro detection was recalibrated and made configurable** (`31e5e54`,
  D-021): the hold-spread test now measures a 10 ms range instead of two
  commands, so it is frame-rate independent, and the view rate test also
  requires a held-key shape (mean step ≥ 75 % of peak) with a ≈2° minimum
  swing. Seven thresholds became cvars:
  `proxy_sv_antiMacroMinReversals` (4),
  `proxy_sv_antiMacroAxisMaxMeanCmds` (3),
  `proxy_sv_antiMacroViewMaxMeanMs` (65),
  `proxy_sv_antiMacroAttackMaxMeanMs` (60),
  `proxy_sv_antiMacroMaxHoldSpreadMs` (10),
  `proxy_sv_antiMacroViewStepPercent` (75) and
  `proxy_sv_antiMacroViewMinSwing` (364). Defaults reproduce the calibration;
  out-of-range values are clamped. The run structure, wary shortcut and noise
  floors stay fixed.
- **Packet-rate limiter thresholds became configurable** (`31e5e54`, D-014):
  `proxy_sv_clientPpsFpsMargin` (default `2` FPS, the former fixed ±2 band) and
  `proxy_sv_clientPpsTolerance` (default `1` packet/s, added after the band and
  the global ceiling). With the defaults a derived limit of `100` accepts `101`.
- **Time-nudge margin became configurable** (`31e5e54`, D-015):
  `proxy_sv_timeNudgeMargin` (default `2` ms, the former fixed bias floor)
  widens the estimate's confidence range, so a larger value acts on fewer
  nudges; `0` reports the statistical interval alone.

## Decision

- Re-verify the cvar table against `origin/main:README.md` at `031f2ea`:
  **43** cvars (was 32). Add the eleven new rows with the upstream defaults and
  concise descriptions, in README order; existing rows keep their defaults and
  tags. The anti-macro master row now says the view verdict is report-only
  unless `proxy_sv_antiMacroView` is on.
- Update the cvar count in the `#features` heading to **43**.
- Do **not** touch the hero stat row: it is frozen by ADR 0015, even though its
  `32` metric is now behind the table. A hero change needs an explicit request.
- Feature highlights (one card each, per ADR 0014):
  - Network tools: note that the limiter and time-nudge neutralisation have
    tunable FPS, tolerance and margin settings.
  - Anti-macro guard: movement/attack freeze, view report-only by default, and
    per-channel thresholds configurable.
- No layout, dependency, asset or deploy changes, so 0001/0004 constraints
  still hold.

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot (0002/0007–0014 stay historical).
- The target is `main` at `031f2ea` (`v0.14.0`). The site shows no version
  number in its own copy, so nothing has to be renamed when `v0.15` appears.
- The hero's `32 server cvars` stat is intentionally stale until the user asks
  for a hero edit. Do not "fix" it opportunistically (ADR 0015).
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `031f2ea`, update the cvar count if it changes, and re-check the
  four dropped C++ cvars are still absent from `main`.
