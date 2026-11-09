# 0017 — Content refresh for the v0.15 line

Date: 2026-11-09. Status: accepted. Extends 0002 (content from `main`), 0014
(concise feature overview) and 0016 (v0.14 refresh).

## Context

The site was last content-synced against `main` at `031f2ea` (`v0.14.0`,
ADR 0016). `main` has since moved to `df526ac` (`v0.15.0`) through:

- `382f831` (`v0.14.1`): the time-nudge margin now applies to the end of the
  confidence range that faces 0, not only to the range width. A range must
  clear 0 by the full margin before the setting is voided or marked, so a
  printed `[0.3..6.1]` with the default `2` still counts as zero (D-015
  amendment).
- `98162a3` (`v0.15.0`): `proxy_sv_antiWallHack` returns with a new
  implementation (D-023). It reuses the FOV cap's hide path instead of the
  original C++ origin-moving one, and adds `proxy_sv_antiWallHackMargin` and
  `proxy_sv_antiWallHackCameraRange`.

The cvar table therefore grows from 43 to **46** entries; nothing else in the
README changed since `031f2ea`.

## Decision

- Re-verify the cvar table against `origin/main:README.md` at `df526ac`:
  **46** cvars. Add the three anti-wallhack rows after `proxy_sv_maxFovMargin`,
  in README order, with defaults `0`, `0`, `100` and the `fov` tag. Update the
  `#features` count label to **46 · filterable**.
- Update `proxy_sv_voidTimeNudge` and `proxy_sv_timeNudgeMargin` to the
  margin-clears-0 semantics from `382f831`, including the `[0.3..6.1]` example.
- Drop `proxy_sv_antiWallHack` from the "not available here" note. Keep
  `proxy_sv_pingFix`, `proxy_sv_sabersFps` and `proxy_sv_disableKillCmd`, and
  state that the anti-wallhack is back through the new FOV-based
  implementation (D-023), not the original one.
- Add one **Anti-wallhack** feature card after **FOV cap**, per ADR 0014:
  line-of-sight hide, shared FOV hide path and sounds, composition with the
  cap, margin, and the Force Sight/bot/spectator exemptions. No dedicated
  section.
- Do **not** touch the hero stat row: it is frozen by ADR 0015, even though its
  `32` metric is further behind the 46-cvar table. A hero change needs an
  explicit request.
- Correct the stale branch-state guidance in `AGENTS.md`: the branch has
  commits and a working scaffold, the dropped-cvar list now has three entries,
  and the anti-wallhack is documented as supported again.

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot (0002/0007–0016 stay historical).
- The target is `main` at `df526ac` (`v0.15.0`). The site shows no version
  number in its own copy, so nothing has to be renamed when `v0.16` appears.
- The hero's `32 server cvars` stat stays intentionally stale until the user
  asks for a hero edit. Do not "fix" it opportunistically (ADR 0015).
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `df526ac`, update the cvar count if it changes, and re-check the
  three dropped C++ cvars are still absent from `main`.
