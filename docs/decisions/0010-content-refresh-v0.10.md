# 0010 — Content refresh for proxy v0.10.0

Date: 2026-11-02. Status: accepted. Extends 0002 (content from `main`) and
0009 (v0.9.0 refresh).

## Context

The site was last content-synced against `main` at `1281955` (v0.9.0, ADR
0009). `main` has since moved to `98cb2fd`, through v0.10.0 (`c9cfba1`) and
three commits after it, changing what admins can configure:

- **`proxy_sv_muteMode`** (`45d127b`, D-011 update): the admin mute now has
  three modes — `0` off, `1` the stock drop-and-tell, `2` silent (the speaker
  sees their own line, nobody else receives it, no notice; `voice_cmd` is
  dropped). The `G_Say` log still records silent-mode lines.
- **`proxy_sv_debugLog`** (`9c4ce5a`, D-013): opt-in crash trace, level `1`
  suspicious/blocked events, level `2` every client command, userinfo update
  and lifecycle event; flushed to `proxy_debug.log` before dispatch, rcon
  password redacted.
- **`proxy_sv_spectatorTeamOverlay`** (`2325afa`, D-012): default `1`; feeds
  the stock `tinfo` roster (name, HP, armor, weapon, powerups) for the
  followed player's team to a following spectator once a second, which makes
  TaystJK's `cg_drawTeamOverlay` work while spectating and also populates the
  stock `team info` HUD.
- **`proxy_sv_enableClientPpsLimit`, `proxy_sv_globalMaxPps`,
  `proxy_sv_clientPpsLog`** (`37bcac7`, `cd29788`, D-014): per-client incoming
  packet-rate limiter whose limit is an integer downscale of the client's
  measured FPS ± 2 with a global ceiling, drops spread evenly through the next
  second; the netStatus `packets/s` cell now renders the same verdict (red,
  effective rate when limiting, `actual/limit` otherwise).
- **`proxy_sv_voidTimeNudge`, `proxy_sv_voidTimeNudgeLog`** (`98cb2fd`,
  D-015): server-side `cl_timeNudge` neutralisation — when the estimated range
  excludes 0 for two consecutive refreshes, each usercmd's `serverTime` is
  shifted by the rounded, ±30-clamped estimate, 1 ms per command; the log
  cvar works on its own.
- **TFFA runtime fixes** with no new cvar: shield/saber-clash and other
  owner-less temp events now show outside the main match (`6d1289e`), opening
  the tab scoreboard no longer re-sends every outsider's `CS_PLAYERS` on each
  request (`991ef6e`, D-006.13), spawn-point occupancy is judged per match so
  matches no longer spawn players onto each other (`ed0fbc3`, D-006.11/S11),
  and a populated `map_restart` defers/spreads the proxy's reliable commands
  so clients are not dropped with `a reliable command was cycled out`
  (`dd6f6f9`, D-006.12).

## Decision

- Re-verify the cvar table against `origin/main:README.md` at `98cb2fd`:
  **21** cvars (was 13). Add `proxy_sv_muteMode`, `proxy_sv_debugLog`,
  `proxy_sv_spectatorTeamOverlay`, `proxy_sv_enableClientPpsLimit`,
  `proxy_sv_globalMaxPps`, `proxy_sv_clientPpsLog`, `proxy_sv_voidTimeNudge`
  and `proxy_sv_voidTimeNudgeLog` with the upstream defaults and descriptions;
  existing rows are unchanged.
- Update the two cvar counts (hero stat, features heading) to **21**.
- Extend **Player mute** with the three `proxy_sv_muteMode` behaviours.
- Add **Spectator team overlay**, **Client packet rate**, **Time nudge
  neutralisation** and **Crash diagnostics** subsections to `#features`, each
  with settings/log samples copied from the README.
- Update **Net status**: `pps` column is now `packets/s`, with the limiter
  verdict/colour rules and the `proxy_sv_voidTimeNudge` rendering of the
  `timeNudge` column.
- Update **TFFA**: the "Fight" step names shields/saber clashes and per-match
  spawn occupancy; "Survives map_restart" and the bullet list mention the
  deferred/spread reconnect burst and the deduplicated scoreboard refresh.
- Hero tagline and features heading now mention spectator overlays and network
  control. One CSS tweak: `h3.sub .count` gets `flex: none; white-space:
  nowrap` so a long heading on a phone wraps its title instead of breaking the
  status pill (`OPT-IN`) in two. No dependency, asset or deploy changes, so
  0001/0004 constraints still hold.

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot (0002/0007/0008/0009 stay historical).
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `98cb2fd`, update the cvar count if it changes, and re-check
  the four dropped C++ cvars are still absent from `main`.
