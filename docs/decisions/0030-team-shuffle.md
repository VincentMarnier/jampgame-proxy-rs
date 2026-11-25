# 0030 — Team shuffle vote

Date: 2026-11-25

## Context

On a team server a group often wants to mix the players into fresh, balanced
games without an admin. The requested feature is a vote:

```text
callvote shuffle 4
```

`4` is the maximum players per team. When the vote passes, everyone is sent to
spectator, a 30-second window starts (centre print + console line) and players
type `/ready`. At the end the ready players are spread over as many equal-team
games as needed, using parallel TFFA matches when that feature is on. A cvar
must enable or disable the vote, and a minimum team size (default `2`, so no
1v1) bounds the games.

## Evidence

The vote machinery is the one already established by D-024 (`callvote
coinflip`):

- `Cmd_CallVote_f` (`original/jedi-academy-sdk/codemp/game/g_cmds.c:1883-2065`)
  only accepts a fixed set of names, so the proxy claims `callvote shuffle` in
  `GAME_CLIENT_COMMAND`, writes the same `level`/`CS_VOTE_*` state and lets the
  game's own `Cmd_Vote_f`/`CheckVote` run the 30 s vote and the tally.
- On a pass, `CheckVote` runs `level.voteString` through
  `trap_SendConsoleCommand(EXEC_APPEND, …)`. A proxy-private command
  (`proxy_teamshuffle <n>`) is therefore seen by the existing
  `G_SEND_CONSOLE_COMMAND` hook.
- The player-teleport primitive is the pristine `SetTeam` trampoline
  (`tffa::set_client_team`), already used by `tffa_create`/`tffa_join`, the
  instance close path and the live-disable path. `/ready` is a new
  proxy-owned command; the pristine game has no `ready` command.
- Parallel matches use the existing assignment slate (`client_match`, `home`,
  `player_score`), `realize_instance` for per-instance worlds and the existing
  per-viewer `CS_PLAYERS` plumbing. The main game is always available; created
  slots are `1..=proxy_parallel_tffas_maxMatches`.

## Decision

1. New cvars:
   - `proxy_sv_teamShuffle` (default `0`): allow the vote and the window.
   - `proxy_sv_teamShuffleMinTeamSize` (default `2`): smallest team a shuffle
     may create, clamped to `1..=MAX_CLIENTS/2`.
2. The vote is claimed only on `GT_TEAM`/`GT_CTF` while the feature is on.
   With the feature off the game keeps rejecting `shuffle` as an unknown vote.
3. `callvote shuffle <n>` validates a strict positive decimal `n`, bounded by
   the minimum team size and `MAX_CLIENTS/2` (16). A missing/invalid value is
   answered with a usage line. A running window refuses another shuffle vote.
4. The vote is started through the shared proxy-vote path (extracted from the
   D-024 coinflip module into `votes.rs`), so `g_allowVote`, an in-progress
   vote, `MAX_VOTE_COUNT` and the spectator rule keep the game's own order.
5. The passed vote's `proxy_teamshuffle <n>` is dropped and only arms a
   pending start; the next `GAME_RUN_FRAME` starts the window. No game state is
   changed from inside the console-command trap.
6. Starting the window calls `tffa::evacuate_to_spectator`: every playing
   client is moved to spectator through the pristine `SetTeam` body, every
   assignment returns to main, every created match closes, the main tally is
   reset, remembered rosters/passwords are dropped, the game's global
   `level.teamScores` is zeroed and per-player scores/end-game stats start
   fresh. This also covers a server running with parallel TFFA off.
7. `/ready` is claimed before the game and marks the sender for the current
   window. The flag is cleared on disconnect and for a new client taking a
   slot. `/ready` outside a window answers that no shuffle is running.
8. A per-frame tick runs after the game's frame body. A timelimit intermission
   wins over a placement; turning the feature or the master switch off cancels
   the window. Ten seconds before the deadline a countdown line is printed.
   The centre display is a multi-line MOTD (title, `/ready` invitation,
   remaining seconds, ready count and team-size limit), refreshed once per
   second so the cgame's `cg_centertime` does not hide it; `CG_CenterPrint`
   treats `\n` as a line break (`cg_draw.c:5110-5136`). The console keeps the
   one-time invitation and the 10 s warning.
9. At the deadline the game's `level.teamScores`/per-player state is reset
   again (so a player who joined a team during the window cannot stay in main)
   and the ready players are placed:

   ```text
   side_total = ready / 2                    (an odd player is a spectator)
   cap        = max_games * max_team
   side_total = min(side_total, cap)
   games      = ceil(side_total / max_team)
   sizes      = split side_total into `games` parts, larger parts first,
                differing by at most one
   ```

   `max_games` is `1 + proxy_parallel_tffas_maxMatches` with parallel TFFA on,
   else `1`. Created slots are realized before use; if a per-instance world
   cannot be built, the plan is recomputed for the slots that did. No team
   below `proxy_sv_teamShuffleMinTeamSize` is created; too few ready players
   means no game and everyone stays a spectator.
10. Ready players are shuffled with `/dev/urandom` (clock/counter fallback),
    the first half of each game's slice goes RED and the second BLUE, and the
    existing assignment/`SetTeam`/`CS_PLAYERS` refresh path makes the move
    visible to every viewer.
11. The join notice lists `/callvote shuffle <players per team>` when
    `proxy_sv_teamShuffle` is on and the map is a team gametype, next to the
    always-present `/callvote coinflip` line.

### Placement examples

With `max_team = 4`, minimum `2`:

| Ready | Plan | Spectators |
|-------|------|------------|
| 20    | 4v4, 3v3, 3v3 | 0 |
| 19    | 3v3, 3v3, 3v3 | 1 |
| 15    | 4v4, 3v3 | 1 |
| 12    | 3v3, 3v3 | 0 |
| 6     | 3v3 | 0 |
| 3     | no game | 3 |

The request listed `19 → 4v4, 3v3, 2v2`. That also places 18 with one
spectator, but `3v3, 3v3, 3v3` is the more even split of the same players, so
the balanced rule is used. The two differ only in how the same 18 players are
grouped; the spectator count is identical. The plan is a pure function with
tests, so switching to the largest-first variant is a one-line policy change if
that preference is confirmed.

## Consequences and limits

- A shuffle is disruptive by design: it empties every match, resets scores and
  passwords, and starts the new games from zero.
- With parallel TFFA off only the main game is filled; extra ready players are
  spectators (`20 ready, team 4 → one 4v4`).
- The number of games is bounded by `proxy_parallel_tffas_maxMatches` and by
  the map's per-instance world capacity. A map that cannot duplicate its
  interactive entities falls back to the main game.
- The vote's team size is the authority for the games it creates;
  `proxy_parallel_tffas_maxGroupSize` keeps governing later `tffa_create`/
  `tffa_join` capacity. A voted size above that cvar's half therefore fills
  the match directly, and denies further joins until players leave.
- Bots never type `/ready`, so they stay spectators (the game may still make
  them auto-join a team; the second evacuation at placement undoes it).
- The 30-second window and the ten-second warning are fixed constants; only the
  enable flag, minimum team size and the voted team size are configurable.
- The window ends on the proxy's next frame after the deadline and is
  wrap-safe against the engine's 32-bit millisecond clock.

## Verification

- `rust/src/shuffle.rs` tests cover the command match, strict team-size
  parsing, the placement rule (the table above, TFFA off, slot/minimum limits),
  the three-line centre MOTD and the wrap-safe deadline compare.
- `rust/src/votes.rs` tests keep the shared vote-refusal order (moved from the
  coinflip module, behaviour unchanged) and the name sanitising.
- `rust/src/shared_api.rs` tests pin the join notice's command list, including
  `/callvote shuffle <players per team>` only when its feature is on.
- `cargo test` 579/579 and `cargo fmt --check` are clean; the project dev image
  also passes `cargo clippy --all-targets -- -D warnings` and the
  `i686-unknown-linux-gnu` release build.
- No live client/server run was done for this round: the new paths are the
  vote start (identical to the runtime-verified coinflip path), the pristine
  `SetTeam` trampoline (runtime-verified by the existing TFFA/duel moves) and
  the existing instance assignment/realization code. A real `callvote shuffle
  4` with several ready clients, both with parallel TFFA on and off, remains the
  runtime confirmation to run.
