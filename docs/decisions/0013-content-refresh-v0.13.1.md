# 0013 — Content refresh for the v0.13.1 line

Date: 2026-11-05. Status: accepted. Extends 0002 (content from `main`) and
0012 (v0.13 line refresh).

## Context

The site was last content-synced against `main` at `f51c077` (ADR 0012).
`main` has since moved to `74cb030` (`v0.13.1`) through `aabbd6c` and
`de403b3`. Two user-facing changes landed; the release commit itself only
bumps `rust/Cargo.toml`.

- **FOV cap tests the whole player, not the feet** (`aabbd6c`, D-022): the
  frustum test now samples the eight corners and centre of the body box from
  `gentity_t.r.mins/maxs` (`{-15,-15,-24}` .. `{15,15,40}`), plus the hilt,
  middle and tip of every active blade of a drawn, non-thrown saber, using the
  server-side blade position. A corpse and a thrown saber keep the body-only
  test. This removes the "player pops in at the screen border" artefact.
  A new cvar, `proxy_sv_maxFovMargin` (default `0`), offsets every frustum
  plane (including the near plane) outward by that many world units, so a body
  or saber just outside the cap stays visible near the border; `32` is roughly
  one player width.
- **`tffa_password` and `tffa_leave` folded into `tffa_join`** (`de403b3`,
  amendment to D-006.1/D-006.15): the separate `tffa_password <password>`
  command is gone, and `tffa_leave` is gone. `tffa_join <id> <password>`
  authorises a match the client is already in (in place, no travel), which also
  covers a protected match promoted into the main slot — name its id from main,
  running `tffa_join 0` first when coming from another match. `tffa_join 0`
  returns to the main game and closes the match being left when nobody is left
  playing there. The password still travels with its match across
  `map_restart`; passwords are still redacted from the debug log.

## Decision

- Re-verify the cvar table against `origin/main:README.md` at `74cb030`:
  **32** cvars (was 31). Add `proxy_sv_maxFovMargin` with the upstream default
  and description; existing rows are unchanged.
- Update the two cvar counts (hero stat, features heading) to **32**.
- FOV cap section: add the body-box/drawn-saber paragraph and the margin
  explanation, and add `seta proxy_sv_maxFovMargin 32` to the `server.cfg`
  sample.
- TFFA: replace the `tffa_password` and `tffa_leave` command chips with
  `tffa_join 0`; reword the lifecycle join card, the command bullet and the
  password bullet for in-place authorisation and `tffa_join 0`. Password
  markers (`[password]`), the spectate-without-password rule and the chat rules
  are unchanged.
- Hero tagline/status pill already name the FOV cap and need no change.
- No layout, dependency, asset or deploy changes, so 0001/0004 constraints
  still hold.

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot (0002/0007–0012 stay historical).
- The target is `main` at `74cb030` (`v0.13.1`). The site shows no version
  number, so nothing has to be renamed when `v0.14` appears.
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `74cb030`, update the cvar count if it changes, and re-check
  the four dropped C++ cvars are still absent from `main`.
