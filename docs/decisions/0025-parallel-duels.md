# D-025 — Parallel duels on `g_gametype 3` (`proxy_parallel_duels_enable`)

Date: 2026-11-10. Status: implemented (runtime-verified, see §9).
Evidence: SDK `original/jedi-academy-sdk/codemp/game/` (`g_main.c`,
`g_cmds.c`, `g_combat.c`, `g_client.c`, `g_team.c`, `g_saga.c`), shipped
`original/jalinuxded_1.011/jampgamei386.so` (`nm -D`, `objdump`,
`readelf -r`), the D-006 parallel-TFFA overlay, and a live `linuxjampded` run
with bots.

## 1. What is built

Several 1v1 duels at once on one `GT_DUEL` map, each in its own instance from
the shared instancing core (no cross-court damage, visibility, collision or
sound). Players are shuffled into courts; each court plays one duel to
`fraglimit` kills; then the ladder bumps: winners move up a court, losers move
down, the top winner and the bottom loser stay. A round ends when every court
resolves; the stock intermission shows the board; a real `map_restart` carries
the ladder into the next round. The first to `duel_fraglimit` duel wins is
champion, then a fresh ladder starts.

One cvar: `proxy_parallel_duels_enable` (default `0`). No commands.

## 2. Decisions

1. **Synchronised rounds.** Every court plays one duel per round; finished
   pairs spectate until all courts resolve.
2. **Duel end = `fraglimit` kills.** The server's own cvar, base JA's per-duel
   kill limit. `fraglimit 0` means no per-duel kill limit.
3. **Round-end board = stock intermission.** The client only draws a board on
   TAB or `PM_INTERMISSION`.
4. **Round transition = real `map_restart`** (the stock duel cycle).
5. **Only the proxy's own round restart carries.** A voted or rcon
   `map_restart` is an admin restart → fresh shuffle and reset standings.
6. **Per-round timelimit**, reset with each round; the server's own
   `timelimit` cvar is in minutes and is compared as `minutes * 60000` ms.
   Expiry decides unfinished courts by HP + shield
   (`AdjustTournamentScores`' rule), exact tie random. `timelimit 0` = no
   round cap.
7. **Ladder ends at `duel_fraglimit` wins.** The round finishes first, then
   champion + fresh ladder. `duel_fraglimit 0` = endless ladder.
8. **Ladder-long standings** (W/L) live proxy-side, reset only on a new
   ladder. `sess.wins/losses` are never written.
9. **Courts re-derived every round** from the live roster (joins extend,
   leavers shrink); membership changes never interrupt a running duel.
10. **Bench is a FIFO queue** (base-JA-style): bottom court's loser → back of
    the queue, front of the queue → bottom court. A full roster queues only the
    odd player (or the capacity surplus).
11. **Automatic start** with the cvar and two willing players; enabling
    mid-duel takes over immediately.
12. **Cvar off (or a gametype/map change) tears the ladder down** and returns
    everyone to spectator, so the stock duel tournament resumes.
13. **Willingness is proxy-side**: a connecting client is willing by default
    (the stock "waiting in line" case; a fresh userinfo usually has no `team`
    key, and `team s` is only the "not waiting in line" marker). An explicit
    `/team spectator` request is the only way out: it removes a queued player
    from the bench and gives a court player's opponent a walkover. `team free`
    is refused and never removes anyone. The ladder's own trampoline moves
    never change the flag. An opted-out player is labelled `(Spec)` on the
    scoreboard/team overlay; a bench player is `(Bench)` and a court player
    `(Duel N)`.
14. **Mid-duel leaver → walkover win.**
15. **Bots are drafted like players.**
16. **Court count** = what the map can host, up to `floor(willing/2)` and 16;
    the surplus queues. The shared slate is 16 created slots
    (`TOTAL_MATCH_SLOTS 17`); `proxy_tffa_maxMatches` keeps its 8-match ceiling.
17. **HUD/board**: per-court duel HUD + top score bar; the board is sorted by
    court, bench last, with W/L in the score column (the stock cgame draws
    `ci->wins/ci->losses` on `GT_DUEL`, `cg_scoreboard.c:170,229`).
18. **One shared instancing core**; `tffa` and `duel` are separate policies.

## 3. Architecture

- `rust/src/instances.rs` — the shared core extracted from `tffa.rs`:
  `client_match`, follow-aware `effective_match`, the isolation predicates,
  session-team reads, per-viewer `cs` plumbing (send/dedup/reliable deferral/
  `loaddefered`), sound attribution, the per-instance world (tags, clone pass,
  runtime-spawn attribution, release), trace/touch/link/query gates and the
  acting scope.
- `rust/src/tffa.rs` — team-match policy (teams, rules, passwords, promotion,
  team chat, `tffa_*`, TFFA scoreboard).
- `rust/src/duel.rs` — the pure ladder model (shuffle, formation, bump,
  reconcile, resolution, standings, champion, board order, messages) and its
  lifecycle wiring.
- `rust/src/state.rs` — `InstanceMode`, `instances_enabled()`/`team_mode()`,
  `DuelState`, `MAX_MATCHES 16`.

`instances_enabled()` = master switch && (TFFA on && `GT_TEAM` || duel on &&
`GT_DUEL`). `team_mode()` = TFFA. Instance-wide call sites use the former;
TFFA-only policy uses the latter.

## 4. What the proxy writes into game memory

Only what TFFA already writes: `PERS_SCORE`/`PERS_KILLED` resets on court
entry, the `g_dontPenalizeTeam` mirror, and the two duel-credit compensations
below. `sess.wins/losses` are **never** written; standings are injected per
viewer into `CS_PLAYERS` `w`/`l`, which keeps `DuelLimitHit` inert.

## 5. Stock duel suppression

| Game code | Stock behaviour | Under the ladder |
| --- | --- | --- |
| `LogExit` `0x881d4` | ends the map at fraglimit/duel/timelimit | swallow `"Kill limit hit."`, `""` (`fraglimit 1`), `"Duel limit hit."`, `"Timelimit hit."` |
| `CheckIntermissionExit` `0x882e4` | 2 s loser removal, 4 s `ExitLevel` | no-op; the proxy owns the 4 s board and its restart |
| `AdjustTournamentScores` `0x86f44` | +1 win/loss on the top two | no-op; standings are proxy-side |
| `AddTournamentPlayer` `0x86924` | pulls a spectator in | neutralised by the `TEAM_FREE` gate on `SetTeam` |
| `CheckTournament` `0x89194` | global `CS_CLIENT_DUELISTS`/`HEALTHS`, pull-in | configstrings dropped; per-viewer values sent |
| `ClientDisconnect` duel block `g_client.c:3882` | zeroes the partner's score, bumps `sess.wins` | restored after the pristine call; proxy walkover |
| `player_die` suicide credit `g_combat.c:2494` | credits `sortedClients[0..1]`'s partner | moved to the court opponent after the pristine call |

`CheckIntermissionExit` and `AdjustTournamentScores` are new game detours
(indexes 22/23) with pinned prologues
(`55 8b ec 83 ec 50` / `55 8b ec 83 ec 20`, steal 6) and fixtures.

**Restart classification.** `classify_restart(armed, text)` turns a
`map_restart` into `Round` only when the one-shot round token is armed,
otherwise `Admin`. The `G_SEND_CONSOLE_COMMAND` arm consumes the token for
game-issued commands (a vote). The proxy's own `map_restart` calls the engine
syscall directly, so it never re-enters the game trap: the round carry is
recorded at issue time (`issue_restart`), and the armed-token path stays pinned
by the unit tests. An rcon/voted restart therefore lands in
`GAME_INIT(restart = true)` with no carry → fresh shuffle.

## 6. Court spawning — binary finding and deviation

The plan said "both players spawned via the pristine `SetTeam` trampoline
(`"f"`)". The shipped binary refuses a third `TEAM_FREE` player: `SetTeam`
tests `g_gametype == GT_DUEL && level.numNonSpectatorClients >= 2 →
TEAM_SPECTATOR` at `0x12b267` (`mov level+0x4c,%ecx; cmp $2; jl …; mov $3,%esi`).
With parallel courts the global count is `2 * C`, so `SetTeam("f")` could only
ever field the first court.

The ladder therefore uses the game's own forced placement primitive,
`SetTeamQuick(ent, TEAM_FREE, qtrue)` (`0x159804`, prologue
`55 8b ec 81 ec 20 04 00 00`): it writes `sess.sessionTeam`, the `team`
userinfo key, `ClientUserinfoChanged` and `ClientBegin` (and so `ClientSpawn`)
directly, bypassing the tournament cap. This is the only deviation from the
approved plan.

`SetTeam` still gets the `TEAM_FREE` gate: while the ladder is on, a
non-spectator request is refused (the game's `AddTournamentPlayer` calls
`SetTeam(ent, "f")`), and a spectator request is allowed (it joins the ladder if
the client is not on a court, or leaves it — with a walkover — if it is).

## 7. Bots

`BotConnect` calls `ClientConnect`/`ClientBegin` **inside the game module**
(`g_bot.c:954,1020`), so a bot never passes through the engine's
`GAME_CLIENT_CONNECT`/`GAME_CLIENT_BEGIN` vmMain events. The ladder detects
them from the game's own `r.svFlags & SVF_BOT` + `pers.connected ==
CON_CONNECTED` and mirrors them as connected/willing; a removed bot is cleared.
Court spawns are queued and drained four per frame (D-006.12 deferral).

## 8. Limits and edge cases

- `fraglimit 0`: duels end only by walkover or the round timelimit.
- `timelimit 0`: no round cap.
- `duel_fraglimit 0`: endless ladder.
- `duel_fraglimit` lowered mid-ladder: the next round-end can crown.
- Voted/rcon `map_restart`: admin restart → fresh shuffle.
- Two players reaching the champion threshold in the same round: more wins,
  random on an exact tie.
- A court whose instance cannot be rebuilt after a restart: both players queue.
- Queue longer than one: exactly one in / one out per round.
- A queued player's `team free`: refused, stays queued.
- A player who goes spectator mid-duel: walkover, then leaves the ladder.
- Cvar off mid-duel: teardown, no walkover recorded.
- A full map change ends the ladder (fresh shuffle).

## 9. Verification

- `cargo test`: 441 unit tests (ladder model: shuffle/formation/bump for
  C=1..N with even/odd/queue, reconcile on roster change, each resolution rule,
  standings, champion/tie, board order, restart classifier, LogExit ownership,
  score reorder, credit offsets).
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
- `rust/dev.sh ./scripts/build.sh` (i686 cdylib).
- `rust/scripts/engine-test.sh`: PASS.
- Live `linuxjampded`, `g_gametype 3`, `proxy_parallel_duels_enable 1`,
  `bot_minplayers 4`, `fraglimit 1`, `duel_fraglimit 3`, `mp/duel1`:
  - round 1 (1 court) → round complete → carried `map_restart` → round 2
    (2 courts) → champion after 3 wins → fresh ladder;
  - a joiners run grew the ladder 1 → 2 → 3 courts across round boundaries as
    bots joined (`bot_minplayers` raised mid-ladder), with the bench growing
    and shrinking around it;
  - with `timelimit 1` (one minute) a round expired at 60000 ms from its own
    clock start, and carried rounds were re-measured against their own start,
    so the limit is per match, not per ladder;
  - round/enable/champion messages showed player names (bots' netnames, reset
    to `^7`) rather than slot numbers after the carried round was announced at
    its first spawned frame;
  - `timelimit 1` produced exactly one `Time limit reached` line and zero
    stock `TIMELIMIT_HIT` broadcasts per round;
  - `rcon map_restart` → `classified as Admin`, `GAME_INIT … carry=false`;
  - `rcon proxy_parallel_duels_enable 0` → teardown, the stock duel resumes;
  - re-enable → takeover and a new ladder.
- Announcements are single-byte ASCII with real newlines (`message_safe` +
  `print`), so a stock latin-1 client renders the round board and champion
  text instead of `\n` and UTF-8 mojibake.
- **Scoreboard refresh decision (D-006.13 shape, applied to both modes).** Two
  candidates were measured:
  1. TFFA's per-viewer refresh: the `scores` trap repairs only the requesting
     viewer, deduped against `cs_players_sent` and deferred under reliable
     pressure. Cost is O(slots) for one client.
  2. The duel's earlier all-viewer refresh: every `scores` message refreshed
     every viewer, O(viewers × slots) per send. At an intermission that queued
     up to `viewers × slots` commands per client and is the reliable
     `CL_GetServerCommand: a reliable command was cycled out` source.
  The TFFA shape wins and is now the only one. The repair runs on a TAB
  (`score`/`scoreboard`, recorded from `GAME_CLIENT_COMMAND`) or on the
  intermission's final board; other server-initiated boards send nothing. The
  all-viewer refresh at `end_round` was removed. TFFA's end-game stats tables
  (the `proxy_sv_enableEndGameStats` feature) are a separate intermission
  burst and are not part of the scoreboard path.
- The stock `TIMELIMIT_HIT` print is swallowed while the ladder runs: the game
  re-broadcasts it on every rank calculation after `level.startTime`, which
  flooded reliable commands (a direct source of
  `CL_GetServerCommand: a reliable command was cycled out`) and produced the
  repeated "time over" line. The proxy prints one
  `Time limit reached - this round ends.` per round instead.
- Announced names append `^7` so a player's colour code cannot bleed into the
  rest of the line.
- A connect after a `g_gametype` switch + restart reseeds willingness
  (`should_seed_on_connect`: first connect, idle/fresh ladder, or
  `fresh_restart`); before this, the post-restart reconnect saw `ladder = true`
  with the default false flags and the humans were never drafted.
- Teardown restores every `CS_PLAYERS` base directly per viewer (only where
  the recorded value differs) and clears the duelist/health strings, bypassing
  the reliable-pressure deferral because the instance core is already off.
  An engine `SV_SetConfigstring` write would be skipped as unchanged (it only
  broadcasts a differing value), so the direct per-viewer `cs` sends are the
  path that actually clears a benched player's `(Bench)` prefix.
- The live run found and fixed two real defects: bots bypass
  `GAME_CLIENT_BEGIN` (now adopted via `SVF_BOT`), and reading `PERS_SCORE`/
  `STAT_HEALTH` inside `state::with_state` re-entered the non-reentrant state
  mutex and froze the server (game reads now happen outside the lock).

## 10. Assumptions

1. `duel_fraglimit 0` = endless ladder; `fraglimit 0` = no per-duel kill
   limit; `timelimit 0` = no round cap.
2. Standings are display-only beyond the champion threshold.
3. Queued players keep `client_match = 0` (court 1) but have no body; following
   a watched player resolves through the effective-instance rule.
4. A full map change always ends the ladder.
5. Court 1 runs in the main instance slot (the pristine world).
6. `proxy_parallel_duels_enable` is the only new cvar; the board window is the
   stock 4 s.
7. A `map_restart` reconnects court players into their carried courts through
   the proxy's court assignment at `GAME_INIT`; `SetTeamQuick` then spawns them.
8. Court capacity is the map's entity budget; the surplus queues.
