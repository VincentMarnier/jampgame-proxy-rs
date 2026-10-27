# 0008 — Content refresh for proxy v0.8.0

Date: 2026-10-27. Status: accepted. Extends 0002 (content from `main`) and
0007 (v0.7.x refresh).

## Context

The site was last content-synced against `main` at `8fd7783` (v0.7.x, ADR
0007). `main` then shipped v0.7.2 → v0.8.0 (HEAD `1a0f78a`), changing
user-facing behaviour:

- **New netstatus implementation** (`9068019`): the `netStatus`/`showNet`
  table columns are now `ping rtt jit fps pps timeNudge snaps rate name` —
  the `score`/`id` columns are gone, `rtt` (server-measured round trip) and
  `jit` (jitter) are new, `timeNudge` is now an **estimated range** (e.g.
  `[-17.9..-10.5]`, `-` while there is too little data, never clamped to
  ±30). When `proxy_sv_enableNetStatus` is `0` the feature is completely
  inert.
- **Player mute** (`9296439`): new RCON commands `rcon mute <player>` /
  `rcon unmute <player>` (client number or fuzzy name match), dropping
  `say`/`say_team`/`tell`/`voice_cmd` only; per client slot, kept across
  `map_restart`, cleared on map change; needs `proxy_sv_enable 1`.
- **TFFA sound isolation fix** (`7b3ee6a`): force sounds (force jump, grip,
  knockdown getup) no longer leak across parallel matches.

The old page listed the netstatus columns as "score, ping, rate, fps,
packets, timeNudge, snaps, id, name", which is now wrong.

## Decision

- Re-verify every cvar row against `origin/main:README.md` at `1a0f78a`
  (v0.8.0): still 13 cvars, defaults unchanged; only the
  `proxy_sv_enableNetStatus` description grows the "completely inert when 0"
  note.
- Correct the net-status column list in the TFFA command bullet and add a
  **Net status** subsection: the sample table from `main`'s README plus a
  four-bullet column glossary (ping/rtt/jit, fps/pps, timeNudge range
  semantics, snaps/rate/name), anchored as `#netstatus` and linked from the
  command bullet.
- Add a **Player mute** subsection (RCON commands, fuzzy name matching, what
  a muted player still can do, mute lifetime) and mention the mute in the
  hero tagline.
- TFFA lifecycle "Fight" card: force sounds join the isolated list.
- Content-only change — no layout, dependency or deploy changes, so
  0001/0004 constraints still hold.

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot.
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `1a0f78a`, and update the cvar count if it ever changes.
