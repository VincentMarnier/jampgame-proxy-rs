# 0009 — Content refresh for proxy v0.9.0

Date: 2026-10-28. Status: accepted. Extends 0002 (content from `main`) and
0008 (v0.8.0 refresh).

## Context

The site was last content-synced against `main` at `1a0f78a` (v0.8.0, ADR
0008). `main` then shipped v0.8.0 → v0.9.0 (HEAD `1281955`), changing
user-facing behaviour:

- **Team lock is now a grace delay** (`f9be08f`, upstream D-004.6):
  `proxy_sv_lockTeams` was **renamed** to `proxy_sv_lockTeamsAfterXSecs` and its
  value changed meaning — it is a delay in seconds, not a minimum team size.
  Teams stay open for that long after a round start (map load or
  `map_restart`); a join into a team in an instance during the window cancels
  that instance's lock for the round; otherwise, when the window expires, each
  team is capped at its current size if the roster is even (`red == blue`,
  minimum one per team — a `1 vs 1` locks like any larger even roster). Caps
  stay reservations, `forceteam` is subject to them, `0` = off, per-instance
  under parallel TFFA. Old configs need the new name.
- **Fraglimit end has a damage-void delay** (`a100c90`, D-006.3 addendum): the
  finished match keeps playing for one second with its damage void (the base
  game's intermission delay), then its members move to spectator. Other matches
  are unaffected; joining an ending match is refused.
- **Finished matches are remembered across `map_restart`** (`a3d038f`/`0aeb72c`
  via D-006.10/D-006.11): a finished match stores its players and their teams
  and is recreated with them on the next `map_restart` (auto-join to the old
  team). Match ids are identities: `TFFA 1` comes back as `TFFA 1` even if its
  game had been promoted into the main slot. A full map change, a lowered
  `proxy_tffa_maxMatches` or disabling `proxy_tffa_enable` drops the memory.
- **Timelimit map end keeps each instance's scoreboard** (`0aeb72c`,
  D-006.11 upstream log): when the timelimit expires and no tied match with
  players can still untie, the whole map ends with every match intact — each
  player reads their own instance's final scoreboard instead of being sent to
  main — and the next `map_restart` resumes the same games. A tied match that
  keeps the map alive still finishes its untied siblings per match.
- Smaller fixes with no separate site claim: spawn sounds no longer leak across
  TFFA instances (`3198745`), team chat stays isolated even when the chat text
  is not valid UTF-8 (`e5135da`, D-006.7 correction), and the main game is
  handled as one more TFFA instance (`e0dcd5d`).

## Decision

- Re-verify every cvar row against `origin/main:README.md` at `1281955`
  (v0.9.0): still **13** cvars; only the team-lock row is renamed and
  re-described. Net status, mute, TaystJK K/D, install steps and hardening
  list are unchanged.
- Rewrite the **Round-start team lock** subsection for the grace-delay
  semantics (window, join cancels the instance's lock, even-roster cap at
  expiry, per-instance TFFA) and replace the upgrade note: the cvar was
  renamed *and* changed meaning, so the old "set `2` to keep 2v2-and-up" advice
  is gone.
- Update `proxy_sv_teamSizeRules` copy to name the new cvar.
- TFFA: revise the lifecycle "End" card (one-second damage-void window, tied
  sudden death, map end with per-instance final scoreboards), the
  "Survives map_restart" card (own id + team, finished matches recreated from
  their remembered group), and add two bullets — finished-match memory and the
  timelimit map end. Update the auto-team bullet to the new lock semantics.
- Content-only change — no layout, dependency or deploy changes, so
  0001/0004 constraints still hold.

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot (0002/0007/0008 stay historical).
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `1281955`, and update the cvar count if it ever changes.
