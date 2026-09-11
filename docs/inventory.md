# Proxy feature inventory

Per-feature map of the original proxy. Scope: **D-002** (2026-09-09) — see
`decisions/0002-scope.md`. This file is the source of truth for what the Rust
rewrite must implement and *how* (with minimal alteration of the original game
module).

## Implementation techniques (D-002.1)

- **vmMain** — runs in the proxy's own `vmMain` dispatch; game module untouched.
- **trap** — interception in the syscall forwarder (`G_LOCATE_GAME_DATA`,
  `G_GET_USERCMD`).
- **intra** — byte/NOP/immediate patch or jump-splice at a precise offset
  inside an original function.
- **call-site** — retarget one `call` to a proxy stub (stub calls the original
  target, then runs proxy logic).
- **entry wrapper** — detour at entry + original trampoline; small pre/post
  logic, body never duplicated.
- **full rewrite** — original proxy duplicates the whole function body.
  **Rejected by D-002** unless the row says otherwise.

Addresses are the original proxy's constants
(`RuntimePatch/Engine/Proxy_Engine_Wrappers.hpp`); game-module ones are offsets
from `jampgame_original.so`'s base.

## Master switch (Rust addition)

`proxy_sv_enable` (default `1`) is a Rust-only master switch. When set to `0`
at runtime, the proxy detaches every engine/game detour, restores the
`Com_Printf`/`SV_ClientThink` call-site retargets and rewrites the rcon
NOP/byte, `ipAuthorize` and RMG intra patches back to their pristine engine
bytes (captured from `linuxjampded`, see `patch::PRISTINE_*`). Its `vmMain`
dispatch and trap interceptions stay installed but become pure passthrough
(`state::proxy_enabled`). Setting it back to `1` re-attaches everything. When it
is `0` at `GAME_INIT`, nothing is ever hooked. No equivalent exists in the
original proxy.

## Wanted

### Security fixes — explicitly named

| feature | cvar | original implementation | address | technique |
|---|---|---|---|---|
| rcon cooldown | `proxy_sv_enableRconCmdCooldown` | `Proxy_SVC_RemoteCommand`, `Patches/server/Proxy_sv_main.cpp:125-145` | `0x8056b14` | entry wrapper |
| rcon: NOP the timer block ("fix rcon disabler") | — | inline patch, `Proxy_Engine_Patch.cpp:159` | NOP 25 B @ `0x8056b26` | intra |
| rcon: `Com_BeginRedirect` buffer 49136 → 1008 | — | inline patch, `Proxy_Engine_Patch.cpp:162` | byte `0xbff0`→`0x03` @ `0x8056c7d` | intra |
| rcon: `writeconfig` only on `.cfg` | — | `Proxy_SVC_RemoteCommand`, `Proxy_sv_main.cpp:134-142` | `0x8056b14` | entry wrapper |
| max `callvote map_restart` value | `proxy_sv_maxCallVoteMapRestartValue` | callvote validation, `Proxy_SharedAPI.cpp:174-180` | — | vmMain |
| model path length | `proxy_sv_modelPathLength` | `Proxy_SV_UserinfoChanged` `Proxy_sv_client.cpp:404-424` + `Proxy_SharedAPI_ClientUserinfoChanged` `Proxy_SharedAPI.cpp:244-268` | `0x804e144` | entry wrapper (engine) + vmMain (game) |

Notes:

- The rcon inline patches are already "intra-function injection" in the
  original — keep the same technique.
- The callvote validation (length guard + per-cvar range table) is fully in the
  proxy's `vmMain` `GAME_CLIENT_COMMAND` handler and is **already ported** in
  `rust/src/shared_api.rs:204-244`.
- The model-length clamp exists in two places: the engine's `SV_UserinfoChanged`
  (needed at connection time, before the game's `ClientUserinfoChanged` fires)
  and the game-side `ClientUserinfoChanged` via `vmMain` (already ported,
  `shared_api.rs:306-349`). Both are entry wrappers / vmMain; neither duplicates
  a function body.

### Security fixes — same umbrella, kept (confirmed 2026-09-09)

| feature | original implementation | address | technique |
|---|---|---|---|
| q3infoboom guard (`SVC_Status`/`SVC_Info` argv[1] > 128) | `Proxy_sv_main.cpp:87-113` | `0x8056574`, `0x8056784` | entry wrapper |
| `SV_UserinfoChanged`/`ClientUserinfoChanged` bugged-model force ("kyle") + non-ASCII | `Proxy_sv_client.cpp:404-424`, `Proxy_SharedAPI.cpp:244-268` | `0x804e144` / vmMain | entry wrapper + vmMain |
| anti-cheat: `jkaDST_` command prefix → kick | `Proxy_SharedAPI.cpp:77-83` | — | vmMain (already ported) |
| anti-cheat: `darksidetools` model → kick | `Proxy_SharedAPI.cpp:253-257` | — | vmMain (already ported) |
| crash: chat length > 256, `gc`, `npc spawn ragnos/saber_droid`, `team follow1/2`, `callteamvote` off | `Proxy_SharedAPI.cpp:88-129` | — | vmMain (already ported) |
| crash: forcepowers validation | `Proxy_SharedAPI.cpp:270-324` | — | vmMain (already ported) |
| client-command `\r\n` / `;` (non-say) blocking | `Proxy_SharedAPI.cpp:196-204` | — | vmMain (already ported) |
| hack guards in client message (`messageAcknowledge < 0`, `reliableAcknowledge` range) | `Proxy_SV_ExecuteClientMessage` `Proxy_sv_client.cpp:152-186` | `0x804e8c4` | **call-site** or intra; NOT full rewrite |
| download validation (`SV_BeginDownload_f` pk3+traversal, `SV_WriteDownloadToClient` referenced-paks, `SV_{Next,Stop,Done}Download_f` state guards, `SV_UpdateUserinfo_f` empty-arg) | `Proxy_sv_client.cpp:426-518` | `0x804d714`, `0x804d764`, `0x0804d614`, `0x0804d5b4`, `0x804eaa4`, `0x0804e314` | entry wrappers; `WriteDownloadToClient` needs redesign (was full rewrite) |
| anti-DDoS: `ipAuthorize` not dispatched | `Proxy_SV_ConnectionlessPacket` `Proxy_sv_main.cpp:190-193` | `0x8056d64` | **call-site** NOP of the `SV_AuthorizeIpPacket` call |

These are part of the "security fixes the proxy provides" umbrella and are all
kept (user confirmation 2026-09-09). Rows marked **call-site**/**intra** must
be ported without duplicating the original function body (D-002.1).

### Stats at the end of games

Accumulates per-client `GameStats_t` (killed/killedBy/damagesGiven/
damagesTaken + team tallies) and prints personal, global and best-player tables
at intermission. Gated by `proxy_sv_enableEndGameStats` (default on; a Rust
addition — the original proxy always printed them).

| piece | cvar | original implementation | address | technique |
|---|---|---|---|---|
| damage accounting | `proxy_sv_enableEndGameStats` | `Proxy_G_Damage`, `Patches/game/Proxy_g_combat.cpp:6-33` | game `+0x00138554` | entry wrapper |
| death accounting (incl. teamkills) | `proxy_sv_enableEndGameStats` | `Proxy_player_die`, `Proxy_g_combat.cpp:35-55` | game `+0x00133b44` | entry wrapper |
| print tables | `proxy_sv_enableEndGameStats` | `Proxy_BeginIntermission`, `Patches/game/Proxy_g_cmds.cpp:9-144` | game `+0x00087d14` | entry wrapper |

All three are additive entry wrappers over pristine game functions; no body
duplication. Game-state deps: `proxy.clientData`, `proxy.g_entities` (from
`G_LOCATE_GAME_DATA`), `Q_StripColor`/`formatScore`.

### Anti HP teller

| piece | cvar | original implementation | address | technique |
|---|---|---|---|---|
| quantise `EV_PAIN` `eventParm` to 24/49/74/100 | `proxy_sv_antiHpTeller` | `Proxy_G_AddEvent`, `Patches/game/Proxy_g_local.cpp:5-29` | game `+0x0016e564` | entry wrapper |

Only the `EV_PAIN` arm is touched; the wrapper passes everything else through.

### Minimum jump time

| piece | cvar | original implementation | address | technique |
|---|---|---|---|---|
| force a minimum jump duration | `proxy_sv_minJumpTime` | `Proxy_ClientThink_real`, `Proxy_g_local.cpp:31-67` | game `+0x0011c4c4` | entry wrapper |

Reads `proxy.g_entities`, `client->sess.sessionTeam`,
`client->pers.cmd.upmove`, `proxy.level->time`; keeps a `jumpStartTime[]`
table (proxy-local). Disabled when cvar is 0.

### Players net status

`netStatus`/`showNet` print a per-player table (score, ping, rate, fps,
packets, timeNudge, snaps, id, name).

| piece | original implementation | address | technique |
|---|---|---|---|
| command interception | `Proxy_SharedAPI.cpp:206-211` → `Proxy_Engine_ClientCommand_NetStatus` `Proxy_Engine_ClientCommand.cpp` | — | vmMain (interim: forwarded, D-001.2) |
| per-usercmd feed (cmdStats + timenudge) | `Proxy_Engine_Client_UpdateUcmdStats/UpdateTimenudge` `Proxy_Engine_Client.cpp:5-13,76-107`, called from `Proxy_SV_UserMove` `Proxy_sv_client.cpp:129-140` | `call SV_ClientThink` inside `SV_ExecuteClientMessage`; target `0x804e634` | **call-site** injection (stub: original `SV_ClientThink`, then stats if `cl->ping >= 1 && cvar`) |
| fps/packets derivation | `Proxy_Engine_Client_CalcPacketsAndFPS` `Proxy_Engine_Client.cpp:15-42` | — | proxy-local (no engine mod) |
| timenudge magic table | `Proxy_GetTimenudgeMagicOffset` `Proxy_Engine_Client.cpp:45-74` | — | proxy-local |
| ping source | `cl->ping` (pristine `SV_CalcPings`) | — | none needed (ping fix dropped ⇒ no `SV_CalcPings` rewrite) |

Deps: `client_t`/`playerState_t` struct surface + `server.svs->clients` walk —
the reason D-001.2 deferred this to the hook milestone.

## Not wanted (drop)

| feature | cvar | original implementation | address |
|---|---|---|---|
| anti wallhack | `proxy_sv_antiWallHack` | `Proxy_sv_snapshot.cpp` (`SV_SendClientSnapshot` `0x8058e04`, `SV_AddEntitiesVisibleFromPoint` `0x08058404`, `SV_CanSee`/`SV_MoveEntityBehindCamera` helpers, `SV_MoveEntityBehindCamera` state) + call retarget `Proxy_Call_To_SV_AddEntToSnapshot_For_Players` @ `0x0805882d` | drop all |
| disable kill command | `proxy_sv_disableKillCmd` | `Proxy_SharedAPI.cpp:131-134` | drop (already ported arm in `shared_api.rs:198-202` — remove) |
| saber FPS | `proxy_sv_sabersFps` | `Proxy_WP_SaberPositionUpdate` `Proxy_g_local.cpp:69-96`, game `+0x00194c24` | drop |
| ping fix | `proxy_sv_pingFix` | `SV_CalcPings` `0x8057204`, `SV_SendMessageToClient` `0x8058c84`, `SV_UserMove` `messageAcked` | drop all three (two are whole-function rewrites) |

## Kept — additional hooks (user decision 2026-09-09)

Kept but outside the wanted/security lists. Several were whole-function
rewrites in the original and must be redesigned per D-002.1 (no body
duplication).

| feature | original implementation | address | technique |
|---|---|---|---|
| `Com_Printf` hardening (`vsnprintf`, redirect buffer, qconsole.log) | `Patches/qcommon/Proxy_files.cpp` | `0x8072ca4` | **redesign** — verify engine body first; candidate: intra retarget of the engine's `vsprintf` call to a `vsnprintf` shim, keep redirect/logfile logic in the engine |
| `Cmd_TokenizeString` quoted/comment handling | `Patches/qcommon/Proxy_cmd.cpp` | `0x812c454` | hook not needed — port `Cmd_TokenizeString2(text, ignoreQuotes)` as a **proxy-local** helper used by the download-validation call-site stub |
| `SV_SendClientGameState` (CS_PRIMED, old-RMG `WriteShort(0)`, fragment flush) | `Proxy_sv_client.cpp:252-395` | `0x804cee4` | **redesign** — diff against shipped binary first; port only real diffs as intra injections |
| `SV_SvEntityForGentity` array-math hardening | `Patches/server/Proxy_sv_game.cpp` | `0x804ffb4` | entry wrapper: bounds check + `Com_Error`, then original |
| `SV_PacketEvent` (translated-port fix, connectionless dispatch) | `Proxy_sv_main.cpp:215-271` | `0x8057024` | **verify** — likely the pristine `SV_ReadPackets` already does both; drop the hook if it's a no-op |
| `CNavigator::Load` `trap_Nav_Free` | `Patches/server/Proxy_navigator.cpp` | `0x08124ff4` | entry wrapper with the **correct** `(this, filename, checksum) -> bool` shape (fixes U-006; deliberate behavior fix — see D-002.2) |

Already decided / not a choice:

| feature | status |
|---|---|
| `G_RegisterCvars`/`G_UpdateCvars` detours (game `+0x00085f14`/`+0x00086034`) | not ported — replaced by the vmMain cvar mirror (D-001) |
| `G_GET_USERCMD` forcesel/angles sanitisation | trap — already ported, kept |

## Already implemented in Rust (D-001, R-020)

- vmMain dispatch incl. client-command filtering (chat, crash guards,
  anti-cheat, callvote validation, newline/semicolon), userinfo
  name/model/forcepowers sanitisation, `netStatus`/`showNet` interim forward.
- Trap interceptions `G_LOCATE_GAME_DATA`, `G_GET_USERCMD`.
- Engine memory layer (`svs`/`sv` + cvar slots), proxy cvar mirror registered at
  `GAME_INIT` / refreshed per `GAME_RUN_FRAME`.
- Version gate, original-module load/bind, forwarder.

## Already implemented in Rust (R-021, hook milestone)

The D-002 keep set is ported as minimal-alteration detours / intra injections
(see `docs/reverse-engineering.md` R-021 for the runtime evidence):

- rcon: cooldown + `writeconfig` `.cfg`-only entry wrapper, the two inline
  patches (NOP timer block, `Com_BeginRedirect` buffer 49136→1008), the
  `Com_Printf` `vsprintf`→`vsnprintf` call-site retarget.
- q3infoboom: `SVC_Status`/`SVC_Info` argv[1]>128 entry wrappers.
- model path length: engine `SV_UserinfoChanged` entry wrapper (+ the
  game-side vmMain arm already in place).
- download validation: `SV_BeginDownload_f` (`.pk3`+traversal),
  `SV_NextDownload_f`/`SV_StopDownload_f`/`SV_DoneDownload_f` (state guard),
  `SV_UpdateUserinfo_f` (empty-arg), `SV_WriteDownloadToClient`
  (referenced-paks) — all entry wrappers; `Cmd_TokenizeString2` +
  `FS_FilenameCompare` + `FS_CheckDirTraversal` as proxy-local helpers.
- anti-DDoS: `SV_AuthorizeIpPacket` call removed by NOP at `0x8056f64`.
- `SV_SvEntityForGentity` bounds-check + real-index entry wrapper.
- `CNavigator::Load` entry wrapper with the **correct**
  `(this, filename, checksum) -> bool` shape (D-002.2).
- game wrappers: `G_Damage`/`player_die`/`BeginIntermission` (stats),
  `G_AddEvent` (anti-HP), `ClientThink_real` (min jump) — all entry wrappers.
- netStatus: `netStatus`/`showNet` intercepted and printed from the proxy's
  own `vmMain`; per-usercmd feed via the `SV_ClientThink` call-site retarget
  (`0x804e891`); timenudge/fps/packets math. Packet identity: the
  `SV_ExecuteClientMessage` entry wrapper (below) bumps `packet_counter` once
  per usercmd message (`clc_move`/`clc_moveNoDelta`) — the same per-
  `SV_UserMove` capture point as the original's pre-loop `cmdIndex`; the
  per-cmd stub feeds that counter to `UpdateUcmdStats` — exact semantics
  (runtime-verified A1/A1b — `tools/runtime/a1_netstatus.sh`). **Deliberate
  deviation (R-022):** the original's `ping >= 1` gate was a bot discriminator
  (`SV_CalcPings` writes ping 0 for `svFlags & SVF_BOT`) that also excluded
  localhost/LAN real players; the port drops the feed's ping gate (bots never
  reach it — no `clc_move`) and discriminates bots in the printer with the
  engine's own `gentity->r.svFlags & SVF_BOT` check (`0x238`/bit 3,
  objdump-verified at `0x8057273-0x805727d`). Division hardening: the
  original's unguarded `1000 / sv_fps` (SIGFPE at sv_fps 0) and
  `1000 / cl->snapshotMsec` (SIGFPE for ping≥1/snapshotMsec 0) are guarded.
- `SV_SendClientGameState` redesign: entry wrapper (CS_CONNECTED→CS_PRIMED
  guard only) + the RMG else-path intra `je`→`jmp` at `0x804d131`. The
  "[ISM]" fragment-flush is already in the shipped binary (verified) and is
  not re-applied.
- Dropped cvars and the `disableKillCmd` arm removed.

## Rust-only additions (not in the original proxy)

| feature | cvar | address | technique |
|---|---|---|---|
| round-start team lock (TDM/CTF) | `proxy_sv_lockTeams` | game `+0x0012ada4` (`SetTeam`) | entry wrapper + `GAME_CLIENT_CONNECT`/`GAME_RUN_FRAME` bookkeeping |
| per-team-size limits | `proxy_sv_teamSizeRules` | — (`G_CVAR_SET` = 7) | per-frame reconcile of the roster, `trap_Cvar_Set`s `timelimit`/`fraglimit`/`capturelimit` when the effective rule changes (nearest smaller size; restores the captured server limits below the smallest rule) |

`SetTeam(gentity_t*, char*)` is nm-verified (`SetTeam__FP9gentity_sPc`) and its
prologue (`55 8b ec 83 ec 38`, 6 bytes) is detour-safe. The wrapper drops
explicit `red`/`blue` joins to a locked, full team with a message, and also
drops an auto pick (`team free`/empty, which the game routes through `PickTeam`)
once both locked teams are full; spectator/follow/scoreboard requests and the
game's internal `SetTeam` calls pass. The round-start snapshot is taken on the
`GAME_RUN_FRAME` that follows the engine's map-load/map-restart reconnect burst
(`GAME_CLIENT_CONNECT` with `firstTime == qfalse`), when the game has restored
the session teams (see `rust/src/teamlock.rs`, decision 0004).

## Verified no-op / dropped (R-021, corrected R-022)

- `SV_PacketEvent` — the pristine shipped `SV_ReadPackets` (`0x8057024`) already
  does both the connectionless dispatch and the translated-port qport fix
  (re-verified by gdb disassembly, R-022); the proxy's rewrite is verbatim →
  dropped.
- `SV_ExecuteClientMessage` / `SV_UserMove` — the shipped binary already
  contains both hack guards (`messageAcknowledge < 0` early return at
  `0x804e9c5`, `reliableAcknowledge` range clamp at `0x804e9d2`,
  objdump-verified); the original's whole-function rewrite existed only for the
  ping-fix (dropped) and the netStatus feed (now a call-site retarget) → the
  detour itself is dropped. **R-022 correction + milestone port:** the rewrite
  was written from newer ioquake3 source and carried three *additional*
  upstream diffs over the shipped 1.011 gate — `strstr(lastClientCommandString,
  "nextdl")` tolerance, the map_restart range check (`serverId >=
  restartedServerId && < serverId`; shipped: equality at `0x8273ecc`), and the
  `state != CS_ACTIVE` guard on the dropped-gamestate resend (icculus bug
  6324). **All three are now ported** as part of the
  `SV_ExecuteClientMessage` entry wrapper (`hooks/engine_sv.rs`), which also
  marks the netStatus packet start (see the netStatus row).

## Not yet runtime-verified (needs a real client / scenario)

- The `netStatus`/`showNet` table rendering (`client_command_net_status`) — the
  per-usercmd feed is exercised by bots, but printing the table requires a real
  client sending the command.
- Download validation end-to-end (requires `sv_allowDownload` + a client
  needing files).
- `CNavigator::Load` on a nav-using map.

## To do (deferred)

- `Com_Printf`/`SV_SendClientGameState`/download differential tests against the
  original proxy in the R-018 container stack, and per-hook "poked value
  observable" tests (D-002 Tests section).