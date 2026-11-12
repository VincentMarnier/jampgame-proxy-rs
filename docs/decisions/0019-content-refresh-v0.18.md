# 0019 — Content refresh for the v0.17.1–v0.18.1 line

Date: 2026-11-12. Status: accepted. Extends 0002 (content from `main`), 0014
(concise feature overview) and 0018 (v0.17 refresh).

## Context

The site was last content-synced against `main` at `65ac4fc` (`v0.17.0`,
ADR 0018). `main` has since moved to `acb817d` (`v0.18.1`) through:

- `3541dd9`, `e234a6c`, `5886379` — `v0.17.1`/`v0.18.1`: saber-clash,
  landing and splash effects are scoped to their instance. No cvar change; the
  TFFA isolation copy gains sounds and effects.
- `c2fbd62` — a leaver that shrinks the duel roster to an odd count compacts
  the ladder: highest-ranked survivors keep their courts, the lowest-ranked
  player takes the bench.
- `d4385d2`, `360b971`, `0a1f37b`, `f6c1b05`, `3923a2c`, `5912f23`,
  `f48a95f` — server-side demos (D-027): per-instance `.dm_26` recording, the
  built-in HTTP server (`/demos` JSON and file download), the in-game
  `/demos` command, the intermission link block, retention/size/free-space
  guards and intermission recording.
- `acb817d` — `v0.18.1` release commit.

`origin/main:README.md` now lists **57** top-level proxy cvars: 11 demo cvars
were added and `proxy_sv_enableRconCmdCooldown` was removed from both the
README table and the code (it is no longer registered).

## Decision

- Re-verify the cvar table against `origin/main:README.md` at `acb817d`:
  **57** cvars. Remove `proxy_sv_enableRconCmdCooldown`; add the 11 demo cvars
  with tag `demos` after the anti-wallhack block. Render the count from
  `cvars.length` so it cannot drift again.
- Add a **Server-side demos** feature-highlight card and `/demos` to the player
  commands.
- Add a `#demos` section after `#duels` plus nav and footer links: one
  lifecycle row (record, follow, publish, keep), three hook cards (no fake
  client, the instance's view, standard demo files), the minimal `server.cfg`
  snippet and four bullets covering the console `//` rule, plain HTTP, the
  unrecorded chat/RMG/vehicle cases, and the download guards plus the 32-slot
  self-view.
- Update the TFFA "Fight" step to name sounds and effects, and add the duel
  ladder compaction rule to the duels bullets.
- Update the cvar explorer hint and empty state to suggest `demo`.
- Do **not** touch the hero stat row (frozen by ADR 0015); its `32` metric and
  the `20+`/`0` stats stay as-is.

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot (0002/0007–0018 stay historical).
- The target is `main` at `acb817d` (`v0.18.1`). The site shows no version
  number in its own copy, so nothing has to be renamed when `v0.19` appears.
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `acb817d`, update the cvar count if it changes, and re-check the
  three dropped C++ cvars are still absent from `main`.
