# 0007 — Content refresh for proxy v0.7.x

Date: 2026-10-26. Status: accepted. Extends 0002 (content from `main`).

## Context

The site was last content-synced on 2026-10-14. `main` then shipped
v0.5.1 → v0.7.1 (HEAD `8fd7783`), changing user-facing behaviour:

- `proxy_sv_lockTeams` became a **minimum** team size (default `0`, off)
  and now locks **per TFFA instance**; `proxy_sv_teamSizeRules` also works
  per match and no longer depends on the team lock.
- Parallel TFFA gained per-match worlds (doors, plats, triggers, items),
  `map_restart` persistence, isolated team chat, auto team balancing,
  sudden-death tie breakers and clean shutdown on
  `proxy_tffa_enable 0`. Maps with NPC/vehicle actors reject parallel
  matches.
- The TaystJK scoreboard-deaths compatibility layer (ADR 0009 upstream)
  became always-on with `proxy_sv_enable 1`, selected per client.
- `proxy_sv_enable 0` no longer disables RCON.

The old page said lockTeams/teamSizeRules "stay off while TFFA is on" and
"chat stays global", both now wrong.

## Decision

- Re-verify every cvar row against `origin/main:README.md` at `8fd7783`;
  the cvar count stays at 13 and defaults are unchanged.
- Correct the stale TFFA claims and promote the current behaviour:
  a 3-card "own world / map_restart / closes cleanly" grid, a revised
  4-step lifecycle, and bullets for mid-game join, per-match team lock and
  rules, and the all-matches scoreboard.
- Add a Round-start team lock subsection with the upgrade note
  (`proxy_sv_lockTeams 1` used to mean "on"; set `2` to keep 2v2-and-up) and
  a TaystJK K/D subsection (client-side opt-in, mixed rosters safe, no cvar).
- Hero copy mentions the TaystJK K/D and the install step notes RCON keeps
  working; the change is content-only — no layout, dependency or deploy
  changes, so 0001/0004 constraints still hold.

## Consequences

- `app/page.jsx` remains the single source of page copy; `docs/decisions/
  0002-content-from-main.md` keeps its historical snapshot and this ADR is
  the current one.
- Next refresh: re-run `git show origin/main:README.md`, diff `docs/decisions/`
  since `8fd7783`, and update the cvar table count if it ever changes.
