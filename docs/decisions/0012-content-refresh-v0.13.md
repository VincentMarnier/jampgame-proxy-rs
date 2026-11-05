# 0012 — Content refresh for the v0.13 line

Date: 2026-11-05. Status: accepted. Extends 0002 (content from `main`) and
0011 (v0.11 line refresh).

## Context

The site was last content-synced against `main` at `9ad9c99` (ADR 0011).
`main` has since moved to `f51c077` (`v0.13.0`), through `v0.12.0` (`7e0fd0d`)
and `v0.13.0` (`f51c077`). Two user-facing features and two TFFA corrections
landed:

- **FOV cap** (`943696c`, D-022; spectator exemption `d92f3a7`):
  `proxy_sv_maxFov` (default `0`) caps what a viewer can benefit from by
  hiding players outside the corresponding view frustum per viewer. Stock
  clients never report `cg_fov`, so this is a stream-side cap, not detection.
  `proxy_sv_maxFovAspect` (default `1.7777778`, 16:9) converts the configured
  `cg_fov` value to the rendered frustum; `proxy_sv_maxFovCameraRange`
  (default `100`) compensates for the third-person camera. Spectators (free or
  following) are exempt, the vertical axis is covered, and the cap pauses
  above 45° of pitch. Hidden players keep their footsteps, weapon/force sounds
  and (when thrown) their saber; the per-viewer changes are restored after the
  snapshot copy.
- **Password-protected TFFA matches** (`0dfebac`, D-006.15):
  `tffa_create [password]`, `tffa_join <id> [password]` and
  `tffa_password <password>`. The password gates joining a team only — anyone
  may spectate without it, `tffa_list` marks the match `[password]`, and the
  password travels with its match across `map_restart` and promotion.
  Passwords are 1–32 bytes, case-sensitive, one argument (quoted spaces
  allowed), and redacted from the debug log.
- **Spectator team chat crosses matches** (`fa33ccf`, D-006 correction): a
  spectator's `say_team` reaches spectators in every match, while a playing
  player's team chat stays in their own match. The site's earlier "team chat
  stays in its match" line was too broad.
- **Deferred TFFA player updates** (`7d24492`): replayed base player updates
  cover the instance-switch model/animation path already documented in the
  v0.11 refresh; no new copy required.

## Decision

- Re-verify the cvar table against `origin/main:README.md` at `f51c077`:
  **31** cvars (was 28). Add `proxy_sv_maxFov`, `proxy_sv_maxFovAspect` and
  `proxy_sv_maxFovCameraRange` with the upstream defaults and descriptions;
  existing rows are unchanged.
- Update the two cvar counts (hero stat, features heading) to **31**.
- Add an **FOV cap** subsection to `#features` with the frustum, aspect,
  third-person, sound-preservation and limitation notes, plus a `server.cfg`
  sample.
- Hero tagline and status pill name the FOV cap.
- TFFA: password-qualified create/join, a `tffa_password` command chip, the
  spectate-without-password rule, the `[password]` list marker, and the
  spectator-chat exception in the lifecycle and the bullet list. The
  `map_restart` card notes that passwords/authorisations travel with their
  match.
- No layout, dependency, asset or deploy changes, so 0001/0004 constraints
  still hold.

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot (0002/0007–0011 stay historical).
- The target is `main` at `f51c077` (`v0.13.0`). The site shows no version
  number, so nothing has to be renamed when `v0.14` appears.
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `f51c077`, update the cvar count if it changes, and re-check
  the four dropped C++ cvars are still absent from `main`.
