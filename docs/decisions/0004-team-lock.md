# D-004 — Rust-only team lock (`proxy_sv_lockTeams`, TDM/CTF)

Date: 2026-09-11. Status: accepted. Evidence: SDK `g_cmds.c:661-931`
(`SetTeam`), `g_client.c:1237-1259` (`TeamCount`), `g_session.c` (session
persistence), engine `sv_init.cpp:760-833` / `sv_game.cpp:1711-1724`
(map-load / map-restart reconnect order); binary `nm`/objdump on
`original/jalinuxded_1.011/jampgamei386.so`.

## Context

The original C++ proxy has no team-lock feature. This is a new, opt-in
Rust-only behavior requested for TDM/CTF: when a round starts with an even,
populated roster, cap each team at its round-start size so the match keeps its
`N vs N` shape; an uneven start is left alone.

The game offers a single choke point for every team change:
`void SetTeam(gentity_t*, char*)` (nm `SetTeam__FP9gentity_sPc`, file offset
`0x0012ada4`; prologue `55 8b ec 83 ec 38`, 6 detour-safe bytes). Session
teams persist across level loads/restarts via the `session%i` cvars
(`G_WriteClientSessionData`/`G_ReadSessionData`), and the engine reconnects
every retained client (`GAME_CLIENT_CONNECT`, `firstTime == qfalse`) before
the first `GAME_RUN_FRAME` of the new round, so the roster is observable
exactly then.

## Decisions

### D-004.1 — Snapshot at the reconnect burst, not continuously

A continuous "first time both teams are equal" rule would lock at `2 vs 2`
while a larger roster is still joining, and the round would never reach
`3 vs 3`. The snapshot is therefore event-driven: `GAME_INIT` (the proxy
module is reloaded for both a map load and a `map_restart`, so its state is
fresh) leaves `pending = true`; the first `GAME_CLIENT_CONNECT` with
`firstTime == qfalse` sets `saw_reconnect`; the next `GAME_RUN_FRAME` snapshots
once and clears `pending`. A fresh map with no reconnect burst does not lock
until the next round/restart, which is the point at which a match roster is
defined.

### D-004.2 — Lock only even rosters with at least two players per team

`snapshot_caps(red, blue)` returns `(red, blue)` iff `red == blue && red >= 2`,
else `(-1, -1)`. Caps are reservations: they do **not** shrink when players
leave, so a team that drops to two still admits players up to its original cap.

### D-004.3 — Enforce in `SetTeam`, explicit joins and auto picks

The wrapper parses the explicit `red`/`r`/`blue`/`b` requests. A join to a
locked team already at its cap is dropped and the player gets
`print "You cannot join the <team> team: teams are locked for N vs N.\n"`.

The in-game **"Auto Team"/"Join"** button sends `cmd team free`; `SetTeam` sends
`free` (and empty/unknown strings) through `PickTeam` instead of the
`red`/`blue` arms. That path is therefore also capped: an auto pick is refused
once *both* locked teams are full, so it cannot silently overfill a team. While
one team is still below its cap the pick is allowed (the game's
least-populated choice lands on the team with the free reservation).
Spectator/follow/scoreboard requests (`spectator`, `s`, `score`, `scoreboard`,
`follow1`, `follow2`) and the game's own internal `SetTeam` calls pass
unchanged.

Because the hook sits on the single choke point, the console
`forceteam <player> <team>` command (`Svcmd_ForceTeam_f`, `g_svcmds.c:384-398`)
is also subject to the cap while a lock is active; an admin who needs a forced
move can set `proxy_sv_lockTeams 0` first.

### D-004.4 — TDM/CTF only

`gametype_supported()` reads the `g_gametype` cvar and accepts `GT_TEAM` (6)
and `GT_CTF` (8) only. Siege's `SetTeamQuick`/`siegeDesiredTeam` round logic
and CTY are out of scope.

## Consequences

- New hook in `GAME_HOOKS` (now 6 entries) and a new proxy cvar
  (`proxy_sv_lockTeams`, default `0`, opt-in so existing servers keep pristine
  behavior).
- The snapshot/enforcement logic is proxy-local (`rust/src/teamlock.rs`), so it
  is unit-testable (`snapshot_caps`, `requested_team`) without the engine.
- Not yet runtime-verified with a real client; the build/unit gates cover the
  parsing and snapshot rules, and the engine reconnect ordering is
  source-verified.
