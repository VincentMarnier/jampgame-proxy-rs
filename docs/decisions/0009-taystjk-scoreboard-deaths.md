# 0009 — TaystJK scoreboard deaths

Date: 2026-10-21
Updated: 2026-10-23 — the 15-int layout is now selected per client from the
capability the cgame advertises in its userinfo.

## Context

TaystJK clients can display a server-authoritative K/D on the tab scoreboard
(deaths next to the score). The server opts in through a `taystJKinfo` feature
key in the server info and by sending a 15th int per client in the `scores`
command. The proxy should provide both for the pristine game module it loads,
without corrupting the scoreboard of clients whose cgame only understands the
stock 14-int layout.

## Evidence

All paths are relative to the source trees named by `AGENTS.md`.

- TaystJK cgame reads the server info key in `CG_ParseServerinfo`:
  `cgs.taystJKinfo = atoi(Info_ValueForKey(info, "taystJKinfo"))`
  (`TaystJK/codemp/cgame/cg_servercmds.c:250`).
- `CG_ParseScores` selects the entry width from that key:
  `scoreOffset = 15` when `cgs.taystJKinfo & TAYSTJK_INFO_SCORE_DEATHS`, else
  `14`, and reads `deaths` at `CG_Argv(i * 15 + 18)`
  (`TaystJK/codemp/cgame/cg_servercmds.c:58-67`).
- The bit is `TAYSTJK_INFO_SCORE_DEATHS (1<<7)`
  (`TaystJK/codemp/game/bg_public.h:557`), commented as "server sends a 15th
  per-client scoreboard int (deaths)".
- The scoreboard draws `score->deaths` only when the server is authoritative
  for deaths, and `cg_scoreDeaths` defaults to `1`
  (`TaystJK/codemp/cgame/cg_scoreboard.c:186-195`,
  `TaystJK/codemp/cgame/cg_xcvar.h:70`).
- The pristine game module writes 14 ints per entry, in the order
  `client, score, ping, time, scoreFlags, powerups, accuracy, impressive,
  excellent, gauntlet, defend, assist, perfect, captures`, and never appends
  deaths (`original/jedi-academy-sdk/codemp/game/g_cmds.c:32-85`).
- A game module can add a key to the server info with a `CVAR_SERVERINFO` cvar:
  the engine rebuilds `CS_SERVERINFO` from `Cvar_InfoString( CVAR_SERVERINFO )`
  when `cvar_modifiedFlags` says so (`original/jedi-academy/codemp/qcommon/cvar.cpp:333-338`,
  `original/jedi-academy/codemp/server/sv_main.cpp:885-887`). `Cvar_Get` from the game module accepts
  the flags and marks the info dirty
  (`original/jedi-academy/codemp/qcommon/cvar.cpp:210-228`).
- `SV_SpawnServer` calls `SV_InitGameProgs` (which runs `GAME_INIT`) before it
  writes `CS_SERVERINFO` (`original/jedi-academy/codemp/server/sv_init.cpp:755-758`, `871-872`), so a
  key set during `GAME_INIT` is part of the first server info of the map.
- The reliable command buffer is `char[MAX_STRING_CHARS]` copied with
  `Q_strncpyz` (`original/jedi-academy/codemp/server/sv_main.cpp:SV_AddServerCommand`), i.e. a
  command longer than 1023 bytes is truncated.
- No client userinfo field reliably identifies a TaystJK cgame: its
  `cjp_client` value is the JA+ plugin marker `1.4JAPRO`
  (`TaystJK/codemp/cgame/cg_consolecmds.c:565`,
  `TaystJK/codemp/cgame/cg_xcvar.h:204`), which JA+/jaPRO clients send too,
  and those cgames do not understand the `taystJKinfo` key.
- The parse branch is recent. `TAYSTJK_INFO_SCORE_DEATHS` handling in
  `CG_ParseScores` was added by commit `7097ab819` (2026-10-21); a cgame built
  before it does not read `taystJKinfo` in that function at all. Verified by
  disassembly of the 2025-09-29 build: `CG_ParseScores` selects 15 ints only
  for `cgs.serverMod` JA+ (with `cjp_client == "1.4JAPRO"`) or jaPRO, and the
  14-int stride otherwise; the 2026-10-21 build adds
  `test BYTE [cgs+0x66e0], 0x80` before choosing the stride. The scoreboard
  draw path is gated the same way (`serverDeaths = … || (cgs.taystJKinfo &
  TAYSTJK_INFO_SCORE_DEATHS)`), so without the bit a client shows no deaths
  even if it parsed them.
- The capability is now advertised by the cgame itself: commit `30fc5979b`
  (2026-10-23) adds `XCVAR_DEF( taystJKcgflags, "0", CG_UpdateTaystJKCGFlags,
  CVAR_ROM|CVAR_INTERNAL|CVAR_USERINFO )` and
  `TAYSTJK_CGFLAG_SCORE_DEATHS (1<<0)` (`bg_public.h`); the cvar's update
  callback writes the bitmask, which makes the client send a reliable
  `userinfo "…"` update (`original/jedi-academy/codemp/client/cl_main.cpp:1630-1635`
  builds it from `Cvar_InfoString(CVAR_USERINFO)`); `CG_Shutdown` resets it to
  `0`. The flag only says "my cgame parses the 15-int layout" and is unrelated
  to the server's `taystJKinfo` key.
- Reproduction against the real client code (2026-10-22): a harness `dlopen`s
  the cgame, sets `trap->Cmd_Argv` to a stub filling a supplied argv,
  writes `cgs.taystJKinfo` / `cgs.serverMod`, patches the function's tail
  `jmp CG_SetScoreSelection` to `ret`, and calls `CG_ParseScores` on a
  6-player, 15-int payload (the exact shape this proxy emits). On the
  2026-10-21 build with `taystJKinfo = 128`, `cg.scores[]` holds clients
  `0,1,2,3,4,5` with the expected teams and deaths. With the key absent, and
  on the 2025-09-29 build with any key value, every row after the first reads
  its client number from the previous row's trailing ints (`deaths`,
  `captures`, `perfect`, `assist`, `defend`); with the early-round values
  (all `0`) all six rows read client `0`, i.e. one RED client repeated, empty
  BLUE column.

Observed facts are the source lines above. The engine behaviour is taken from
`original/jedi-academy/codemp`, the source base of the shipped dedicated
server; it was not additionally verified against the 2003 binary for this
feature (the APIs used are the ordinary game traps).

## Decision

The feature is part of the proxy's compatibility layer and is **always on while
`proxy_sv_enable` is 1** (no dedicated cvar), as requested for TaystJK
compatibility:

1. A `taystJKinfo` cvar is registered at `GAME_INIT` with
   `CVAR_SERVERINFO | CVAR_ROM`, default `0`. `scoreboard::sync_serverinfo`
   runs after the proxy cvars are refreshed at `GAME_INIT` and on every
   `GAME_RUN_FRAME`, and writes `current | (1<<7)` while the master switch is
   on and `current & !(1<<7)` while it is off. Other bits another game module
   may advertise are preserved. `GAME_SHUTDOWN` clears bit 7 so a pristine
   module loaded later does not inherit the advertisement.
2. `G_SEND_SERVER_COMMAND` rewrites a `scores ...` command after the parallel
   TFFA per-viewer rewrite (TFFA parses the stock 14-int layout, so it must run
   first), appending `g_entities[n].client->ps.persistant[PERS_KILLED]` as the
   15th int of every entry **only when the recipient's cgame advertises the
   layout**. The capability comes from the client's own userinfo:
   `taystJKcgflags & TAYSTJK_CGFLAG_SCORE_DEATHS` (`cg_xcvar.h`,
   `bg_public.h`), which the TaystJK cgame sets to `1` and re-sends as a
   userinfo update. It is read live at send time with `trap_GetUserinfo`
   (`G_GET_USERINFO`), so there is no cache to invalidate and a client that
   joins with the flag is picked up on its next `scores` message. A client
   without the flag (older TaystJK, stock JKA, JA+/jaPRO) keeps the pristine
   14-int message.
3. A broadcast `scores` (`clientNum == -1`) has no single layout: it is fanned
   out with `trap_SendServerCommand` per connected client, each getting 15 or
   14 ints from its own userinfo. Unrecognised layouts (already 15-int,
   malformed) are forwarded unchanged, so a game module that owns the layout
   keeps working.
4. The entry width is resolved from the header count when consistent
   (`count * 14 == ints`, or already `count * 15 == ints`, in which case the
   message is left alone), and from divisibility otherwise (the game's header
   count is `level.numConnectedClients`, which can exceed the entries actually
   emitted after its 1022-byte entry budget). Ambiguous sizes are left
   untouched rather than risk treating a jaPRO/JA+ deaths field as stock data.
5. Because the 15-int layout can overflow the engine's 1024-byte reliable
   command, entries are appended whole until the next one would not fit; the
   header count is rewritten to the number of entries emitted, so no truncated
   deaths field shifts a later entry.

## Consequences and limits

- The server info key is server-wide, but the **layout is per client**: a
  capable cgame (TaystJK from commit `30fc5979b`, 2026-10-23 or later) parses
  15 ints because it sees bit 7, and gets the 15-int message; every other
  cgame — earlier TaystJK releases, stock JKA, JA+, jaPRO — ignores the key,
  keeps parsing 14 ints, and now receives the pristine 14-int message. Mixed
  rosters therefore work: only incapable clients lose the deaths column,
  which they never had.
- The gate is read live from the userinfo, so a client that has not sent
  `taystJKcgflags` yet is treated as incapable (the safe default). The cgame
  sets the flag during `CG_Init`, before the cgame can request or receive its
  first scoreboard; the update still travels as an ordinary client userinfo
  command, so a board queued in the same frame could in theory reach it as 14
  ints. No such case was observed (the first `scores` is either requested by
  the client on TAB or queued 500 ms later in duel/intermission).
- A client can spoof the flag; the only consequence is that it receives the
  15-int layout and misparses its own scoreboard.
- If the loaded game module already sends 15-int entries (JA+/jaPRO gamename),
  `append_deaths` recognises the layout and the proxy leaves the message
  untouched; the per-client gate only governs the layout the proxy itself
  adds.
- In CTF and Duel the TaystJK scoreboard does not display the deaths field
  (`cg_scoreboard.c:192`), but the 15th int is still sent so the 15-int parse
  remains valid.
- In parallel TFFA matches the value follows the game's per-match
  `PERS_KILLED`, which the proxy zeroes when a player changes match, so the
  deaths shown match the score shown by the same board.
- `CVAR_ROM` keeps the value out of an admin's hands but not out of the
  proxy's: the proxy sets it with a forced `Cvar_Set`
  (`original/jedi-academy/codemp/qcommon/cvar.cpp:340-355`).

## Verification

- `cargo test` — pure rewrite tests: 15th field position, per-client lookup,
  already-15 layouts, malformed messages, count/entry mismatch, 1024-byte
  budget (whole entries dropped, header count rewritten), ambiguous sizes, and
  the `taystJKcgflags` capability gate (bit 0 set / clear / missing / other
  bits).
- `rust/dev.sh ./scripts/build.sh` — i686 cdylib builds and the staged
  `jampgamei386.so` is a 32-bit ELF shared object.
- `rust/dev.sh cargo clippy --all-targets -- -D warnings` — clean.
- **Client parse reproduction** (2026-10-22) against the real cgame binaries
  (method above): the 2026-10-21 `CG_ParseScores` reads the proxy's 6-player,
  15-int payload as clients `0..5` with `taystJKinfo = 128`, and collapses
  every row onto client 0 with the key absent; the 2025-09-29 build collapses
  in both cases.
- **Live round** (2026-10-21) against the shipped `linuxjampded` with the
  staged proxy as `base/jampgamei386.so`, through
  `rust/scripts/run-server.sh`. This capture predates the per-client gate, and
  the clients were bots (which advertise no `taystJKcgflags`), so with the
  gate in place the same messages would stay at 14 ints:
  - `getstatus` on a proxy-enabled spawn shows `\taystJKinfo\128` in the
    server info; starting with `+set proxy_sv_enable 0` shows `\taystJKinfo\0`,
    a runtime `proxy_sv_enable 0` clears it to `0`, and a runtime
    `proxy_sv_enable 1` sets it back to `128` (hooks re-attached).
  - A `g_gametype 3` (duel) map with three bots (the game queues a scoreboard
    when ranks change) produced `scores ...` commands in the clients'
    `reliableCommands` ring: every capture had `3 + n * 15` ints and the
    entries' fields kept the pristine order, with the mirrored `perfect` flag
    confirming the death count. One capture, after a duel frag:

    ```text
    scores 3 0 0
      1  1 0 0 0 0 0 0 0 0 0 0 1 0 0   <- client 1, 1 frag, 0 deaths
      0  0 0 0 0 0 0 0 0 0 0 0 0 0 1   <- client 0, 0 frags, 1 death
      2  0 0 0 0 0 0 0 0 0 0 0 0 1 0   <- client 2, 0 frags, 0 deaths
    ```

    A later capture had client 0's deaths at `2` while `perfect` read `0`,
    matching the game's own `PERS_KILLED`/`PERS_RANK`.
- The mixed-cgame case is now handled by construction: a client that does not
  advertise `taystJKcgflags` bit 0 receives the pristine 14-int message. The
  gate is unit-tested; a live two-client round (2025-09-29 cgame next to a
  cgame built from `30fc5979b`) is the remaining end-to-end check and has not
  been run in this session.

## Alternatives considered

- A per-client gate on `cjp_client == "1.4JAPRO"` or on the presence of
  `cp_cosmetics`/`cg_displayNetSettings`: rejected, those keys are also sent by
  JA+/jaPRO cgames, which expect the 14-int layout.
- A new `proxy_sv_scoreboardDeaths` cvar: rejected in favour of the user's
  requested always-on behaviour; the master switch remains the escape hatch.
- Sending the 15-int layout to every client regardless of its cgame (the first
  implementation): rejected after the scoreboard collapse was reproduced with a
  2025-09-29 cgame. The layout is now selected per client from the capability
  the cgame advertises in its userinfo.
