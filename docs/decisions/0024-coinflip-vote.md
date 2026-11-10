# 0024 — `callvote coinflip`

Date: 2026-11-10

## Context

The server should offer a `coinflip` vote: players call it like any other vote,
vote yes/no normally, and when it passes the server announces a random `Heads`
or `Tails` to everyone through the engine's `svsay` chat. The result is random
and independent of the tally; the vote only decides whether the flip happens.

The stock `Cmd_CallVote_f` only accepts a fixed set of vote names. The proxy
cannot add a name to the pristine function, and it must not modify anything
under `original/`.

## Evidence

Paths are relative to the source trees named by `AGENTS.md` unless stated.

- `Cmd_CallVote_f` (`original/jedi-academy-sdk/codemp/game/g_cmds.c:1883-2065`)
  rejects every name outside `map_restart`, `nextmap`, `map`, `g_gametype`,
  `kick`, `clientkick`, `g_doWarmup`, `timelimit` and `fraglimit`. Before that
  it checks `g_allowVote`, an in-progress vote (`level.voteTime` or
  `level.voteExecuteTime >= level.time`), `pers.voteCount >= MAX_VOTE_COUNT`,
  and spectator votes outside duel/powerduel.
- At vote start it sets `level.voteTime`, `level.voteYes = 1`,
  `level.voteNo = 0`, clears `PSG_VOTED` on every client, marks the caller,
  writes `level.voteString`/`level.voteDisplayString`, and sets
  `CS_VOTE_TIME`/`CS_VOTE_STRING`/`CS_VOTE_YES`/`CS_VOTE_NO`.
- `Cmd_Vote_f` (`g_cmds.c:2067-2109`) is the ordinary yes/no path: it uses
  `level.voteTime`, `level.numVotingClients` and `PSG_VOTED`, and updates
  `CS_VOTE_YES`/`CS_VOTE_NO`.
- `CheckVote` (`g_main.c:3229-3309`) tallies against
  `level.numVotingClients/2`, prints `VOTEPASSED`/`VOTEFAILED`, and on a pass
  waits `level.voteExecuteTime = level.time + 3000`, then calls
  `trap_SendConsoleCommand(EXEC_APPEND, va("%s\n", level.voteString))`.
- `svsay` is the engine's server chat (`SV_ConSay_f`,
  `original/jedi-academy/codemp/server/sv_ccmds.cpp:768-792`): it prepends
  `Server: ` and sends `chat "..."` to every client.
- The `level` field offsets used here were checked against the shipped
  `jampgamei386.so`: `cmd_CallVote_f` reads `level.time` at `0x20` (32),
  `level.voteTime` at `0x8f0` (2288), `level.voteExecuteTime` at `0x8f4`
  (2292), writes `level.voteYes` at `0x8f8` (2296) and `level.voteNo` at
  `0x8fc` (2300), passes `level.voteString` as `0xf0` (240) and
  `level.voteDisplayString` as `0x4f0` (1264), and accesses
  `level.clients[slot].mGameFlags` at `imul $0x1c40` (`sizeof(gclient_t)` =
  7232) + `0x1c30` (7216). `pers.voteCount` is at `+0x6a0` (gclient+1696).

## Decision

1. `callvote coinflip` is claimed in `GAME_CLIENT_COMMAND` before the pristine
   `ClientCommand`; the proxy starts the vote itself and forwards nothing.
2. The start path mirrors the game's checks in the same order (`g_allowVote`,
   in-progress vote, caller vote count, spectator outside duel/powerduel) and
   answers a refusal with a proxy `print`. Any non-zero
   `level.voteExecuteTime` counts as in-progress (the game only rejects
   `>= level.time`; treating the stale window as busy is the safer choice).
3. Starting the vote writes the same `level` fields and configstrings as
   `Cmd_CallVote_f`: start time, yes = 1, no = 0, `PSG_VOTED` cleared on every
   slot and set on the caller. `level.voteString` becomes the proxy-private
   `proxy_coinflip`; `level.voteDisplayString` and `CS_VOTE_STRING` are
   `coinflip`.
4. `Cmd_Vote_f` and `CheckVote` are left untouched: the 30 s vote, the tally,
   `VOTEPASSED`/`VOTEFAILED` and the three-second execution delay are the
   game's own.
5. When the passed vote executes, `G_SEND_CONSOLE_COMMAND` sees
   `EXEC_APPEND proxy_coinflip`. The proxy drops that command and sends
   `svsay ^3Coin flip: ^7Heads!` (or `Tails!`) through
   `trap_SendConsoleCommand(EXEC_NOW, …)`, so every player sees
   `Server: Coin flip: Heads!` as a normal chat line.
6. The side is bit 0 of a `/dev/urandom` byte (exactly 50/50). If the device is
   unavailable, a millisecond/counter mix is used instead.
7. The vote's caller broadcast is a `print` with the colour-stripped name and
   quotes/controls replaced, because a client re-tokenizes `print` and an inner
   `"` would truncate it.

## Consequences and limits

- The result is drawn only after the vote passes; a failed or timed-out vote
  never announces a side.
- The game never increments `pers.voteCount` in the shipped source, so the
  vote-count check rarely fires; the proxy mirrors the check rather than
  inventing a counter.
- The proxy keeps no coinflip state between frames: the game owns `voteTime`,
  `voteExecuteTime`, the configstrings and the vote string. A map change or
  shutdown drops a pending vote as it drops any other vote.
- The proxy's own `set_configstring` call goes straight to the engine, so the
  `votes::on_configstring` slot-address snapshot does not run for a coinflip.
  That snapshot only protects `clientkick` votes, so it is not needed here.
- While `proxy_sv_enable` is `0`, `callvote coinflip` is not claimed and the
  game answers its usual "Invalid vote string".
- Refusal messages are English proxy lines, not the game's localized
  `MP_SVGAME` strings.

## Verification

- `rust/src/coinflip.rs` unit tests cover the command match, the execution
  match, the refusal order, the `svsay` text, name sanitising and the random
  bit mapping.
- `tools/runtime/a5_coinflip.sh` runs two synthetic clients on a live server:
  the caller joins the FFA, starts `callvote coinflip`, the other client votes
  yes, and the run requires `CS_VOTE_STRING = coinflip`, the yes/no
  configstrings, `VOTEPASSED`, and the final `chat "Server: ^3Coin flip:
  ^7Heads!\n"` (or `Tails!`).
- `cargo test` (413 tests), `cargo clippy --all-targets -- -D warnings` and
  the `i686-unknown-linux-gnu` release build pass.
