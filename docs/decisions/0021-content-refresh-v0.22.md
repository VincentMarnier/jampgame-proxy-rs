# 0021 — Content refresh for the v0.22.0 line

Date: 2026-11-25. Status: accepted. Extends 0002 (content from `main`), 0014
(concise feature overview) and 0020 (v0.19 refresh).

## Context

The site was last content-synced against `main` at `17fa454` (`v0.19.0`,
ADR 0020). `main` has since moved to `fe75a31` (`v0.22.0`), adding:

- `4396c3a` — ping equalisation targets each parallel match's own roster
  (D-028.10).
- `32c0c6b` — the lone willing player waits in the arena in parallel duels,
  and the first round forms when a second player joins (D-025 amendment).
- `f0a43fe` — the in-game `/demos` command and the end-of-game link block are
  removed; a first-time join notice names the demo page instead (D-027 §10).
- `368c721` — `GET /demos` entries gain `metadata` (map, gametype, final
  players/scores, team scores, duration), D-029.
- `808a386`, `3b629cb` — the team shuffle vote (`callvote shuffle <n>`, 30 s
  `/ready` window, equal-team placement) and `proxy_sv_coinflip` (on by
  default), D-030/D-024.
- `8c96fe7` — a `CS_PLAYERS` re-push stays inside `MAX_NETNAME` (D-006.18);
  a bugfix, not site copy.
- The `tffa` alias, the `/join` spelling and `showNet` were removed
  (D-006/D-010 amendments), so player-command copy had to drop them.

`origin/main:README.md` now lists **62** top-level proxy cvars (59 + coinflip,
team shuffle, shuffle minimum team size).

## Decision

- Add `proxy_sv_coinflip` (default `1`, tag `votes`), `proxy_sv_teamShuffle`
  (default `0`) and `proxy_sv_teamShuffleMinTeamSize` (default `2`) after
  `proxy_parallel_tffas_maxGroupSize`, matching the README order; tag the
  shuffle cvars `teams`.
- Player commands: drop `tffa`, `/join red|blue`, `/demos` and `showNet`; add
  `callvote shuffle <players per team>` and `/ready`. The note now explains
  `/ready`'s window and the join notice.
- Feature highlights: add one **Team shuffle** card after **Team control**
  (per ADR 0014); add the default-on coinflip cvar to its card; mention the
  per-match ping-equalisation target and the demo listing metadata, and the
  join-time demo page hint.
- TFFA section: remove the `/join` spelling from its bullets.
- Duels section: note the lone waiting player (lede, lifecycle, `team free`
  exception, and the pull exception when disabling the ladder).
- Demos section: `Publish` now describes metadata and the join-notice page
  URL; a bullet records that in-game demo links and the intermission block
  are gone.
- Update three cvar descriptions that referenced removed behaviour
  (`netStatus`/`showNet`, in-game `/demos` + intermission block, demo links
  sent at intermission).
- Do **not** touch the hero stat row (frozen by ADR 0015), even though its
  cvar count is now stale.

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot (0002/0007–0020 stay historical).
- The site shows no version number in its own copy, so nothing has to be
  renamed when `v0.23` appears.
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `fe75a31`, update the cvar count if it changes, and re-check
  that the dropped C++ cvars and the removed aliases/commands stay absent.
