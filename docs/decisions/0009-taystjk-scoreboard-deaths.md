# 0009 — TaystJK scoreboard deaths

Date: 2026-10-21

## Context

TaystJK clients can display a server-authoritative K/D on the tab scoreboard
(deaths next to the score). The server opts in through a `taystJKinfo` feature
key in the server info and by sending a 15th int per client in the `scores`
command. The proxy should provide both for the pristine game module it loads.

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
   15th int of every entry. Broadcast `scores` (`clientNum == -1`) are handled
   too.
3. The entry width is resolved from the header count when consistent
   (`count * 14 == ints`, or already `count * 15 == ints`, in which case the
   message is left alone), and from divisibility otherwise (the game's header
   count is `level.numConnectedClients`, which can exceed the entries actually
   emitted after its 1022-byte entry budget). Ambiguous sizes are left
   untouched rather than risk treating a jaPRO/JA+ deaths field as stock data.
4. Because the 15-int layout can overflow the engine's 1024-byte reliable
   command, entries are appended whole until the next one would not fit; the
   header count is rewritten to the number of entries emitted, so no truncated
   deaths field shifts a later entry.

## Consequences and limits

- The key is server-wide, so all clients must run a cgame that understands it.
  A stock or JA+/jaPRO client would misparse the 15-int scoreboard. Mixing
  cgames is out of scope (see the userinfo evidence above); a server that needs
  stock clients must run with `proxy_sv_enable 0` or not use the proxy.
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
  budget (whole entries dropped, header count rewritten), ambiguous sizes.
- `rust/dev.sh ./scripts/build.sh` — i686 cdylib builds and the staged
  `jampgamei386.so` is a 32-bit ELF shared object.
- `rust/dev.sh cargo clippy --all-targets -- -D warnings` — clean.
- **Live round** (2026-10-21) against the shipped `linuxjampded` with the
  staged proxy as `base/jampgamei386.so`, through
  `rust/scripts/run-server.sh`:
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
- The mixed-cgame behaviour (a stock cgame misparsing the 15-int layout) is
  documented above but was not exercised: it follows from the client parse
  (`cg_servercmds.c:63-88`) and is the reason the master switch is the escape
  hatch.

## Alternatives considered

- A per-client gate on `cjp_client == "1.4JAPRO"` or on the presence of
  `cp_cosmetics`/`cg_displayNetSettings`: rejected, those keys are also sent by
  JA+/jaPRO cgames, which expect the 14-int layout.
- A new `proxy_sv_scoreboardDeaths` cvar: rejected in favour of the user's
  requested always-on behaviour; the master switch remains the escape hatch.
