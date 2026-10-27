# D-004 — Rust-only team lock (`proxy_sv_lockTeamsAfterXSecs`, TDM/CTF)

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

*Revised by D-004.6: that first frame now opens a grace window instead of
snapshotting immediately; the reconnect-burst trigger is unchanged.*

### D-004.2 — Lock only even rosters with at least two players per team

*Superseded by D-004.5 and then D-004.6: the minimum-size threshold is gone;
the even-roster rule is unchanged, now with a minimum of one player per team
(`red == blue && red >= 1`).*

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
move can set `proxy_sv_lockTeamsAfterXSecs 0` first.

### D-004.4 — TDM/CTF only

`gametype_supported()` reads the `g_gametype` cvar and accepts `GT_TEAM` (6)
and `GT_CTF` (8) only. Siege's `SetTeamQuick`/`siegeDesiredTeam` round logic
and CTY are out of scope.

### D-004.5 — Minimum team size, per instance (revision, 2026-10-26)

*Status: accepted (user directive). Supersedes the boolean gate of D-004.2 and
the TFFA skip of D-006.2.*

`proxy_sv_lockTeams` is now the **minimum team size** that may be locked instead
of a boolean: `0` keeps the feature off (the old default), and `N >= 1` locks a
round only when it starts even and each team fields at least `N` players
(`snapshot_caps(red, blue, minimum)` = `(red, blue)` iff `red == blue && red >=
minimum`, else `(-1, -1)`). `1` therefore locks from `1 vs 1` up, `2` reproduces
the old `>= 2` behavior, `3` waits for `3 vs 3`, and so on.

The caps are per instance: `TeamLock::caps` is one `(red, blue)` pair per TFFA
match slot (index = match id, `0` = main) instead of one global pair. At the
round-start snapshot each instance is evaluated against **its own** roster
(`tffa::match_team_counts`, the same per-match census D-005.5 uses); with TFFA
off the whole server is main. Enforcement in `SetTeam` resolves the joining
player's match (`client_match`) and compares against that match's caps and
census, so a full 2 vs 2 in one instance no longer refuses a join in another.
A match created after the round started has no round-start roster, so it stays
unlocked until the next round; slots keep no caps across a close/promotion
(`teamlock::clear_match`/`promote_match`). The `SetTeam` wrapper therefore no
longer needs to be skipped while TFFA is on (the skip of D-006.2 is removed from
the `GAME_RUN_FRAME` dispatch).

Runtime observation (bot-filled dedicated server, `g_gametype 6`, `mp/duel1`,
4 bots at 2 vs 2, built proxy): `map_restart` logged `teamlock snapshot gt=6
min=2 tffa=false -> main 2 vs 2`; a `forceteam` of a blue bot to red produced
`teamlock join match=0 team=1 old=2 count=2 cap=2 (red=2 blue=2)` (refused); with
`proxy_sv_lockTeams 0` the next restart logged `-> no lock`, and with `1` it
locked the same 2 vs 2. The per-instance path is covered by the
`snapshot_match_caps`/promotion unit tests; a live created-match lock still
needs a real client, since bots cannot issue `tffa_create`.

### D-004.6 — Grace delay before locking, per instance (revision, 2026-10-27)

*Status: accepted (user directive). Replaces the minimum-size value of
D-004.5.*

`proxy_sv_lockTeamsAfterXSecs` is now the **grace delay in seconds** before a
round's teams lock, not a minimum team size. `0` (the default) keeps the feature
off. The first `GAME_RUN_FRAME` after the reconnect burst opens the window and
records the server time; no team is capped while it runs. When the window
expires, each eligible instance is snapshotted at its current roster with the
even-roster rule of D-004.2/D-004.5 minus the minimum-size parameter:
`snapshot_caps(red, blue) = (red, blue)` iff `red == blue && red >= 1`, else
`(-1, -1)`. In other words, a lock requires that nobody joined during the
window; `1 vs 1` locks like any larger even roster.

A successful `SetTeam` that moves a player into `red`/`blue` during the window
cancels that player's instance for the round. The wrapper reads the resulting
session team after the pristine body, so auto picks (`team free`/empty through
`PickTeam`) are included; a same-team no-op is not. Cancellation is per
instance: `TeamLock::armed` marks the instances that existed at the round start
and `TeamLock::joined` marks the ones a join cancelled. A match created after
the round start is not armed, so it stays unlocked until the next round, as in
D-004.5. Closing/promoting a match moves its armed/joined flags with its caps
(`clear_match`/`promote_match`).

Enforcement is unchanged (D-004.3/D-004.5): the caps remain reservations and are
checked per instance in `SetTeam`. Turning the cvar to `0` mid-round drops any
caps and stops the window.

Runtime verification of the new timing is pending. The pure grace/arming/
cancellation rules are unit-tested (`grace_due`, `snapshot_match_caps`); the
timing source is `svs.time` from `GAME_RUN_FRAME` (`sv_main.cpp:911`), the same
value every round-start frame receives.

## Consequences

- New hook in `GAME_HOOKS` (now 6 entries) and a new proxy cvar
  (`proxy_sv_lockTeamsAfterXSecs`, default `0`, opt-in so existing servers keep
  pristine behavior). Since D-004.6 the value is the grace delay in seconds
  (`0` = off) and the caps are per TFFA instance, so the feature works with
  `proxy_tffa_enable`.
- The snapshot/enforcement/grace logic is proxy-local (`rust/src/teamlock.rs`),
  so it is unit-testable (`snapshot_caps`, `snapshot_match_caps`, `grace_due`,
  `requested_team`) without the engine.
- Not yet runtime-verified with a real client; the build/unit gates cover the
  parsing, grace and snapshot rules, and the engine reconnect ordering is
  source-verified.
