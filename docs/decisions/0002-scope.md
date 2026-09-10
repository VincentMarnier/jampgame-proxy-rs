# D-002 — Feature scope + minimal-alteration principle

Date: 2026-09-09. Status: accepted (user directive). Evidence: source of the
original proxy (`original/jampgame-proxy/src/jampgame_proxy/`); hook tables in
`RuntimePatch/Engine/Proxy_Engine_Patch.cpp`; full per-feature map in
`../inventory.md`. Source authority: AGENTS.md.

## Context

The original proxy attaches 22 engine detours + 8 game detours + 1 call-site
retarget + 2 inline patches, and re-implements *entire* engine functions (e.g.
`SV_CalcPings`, `SV_ConnectionlessPacket`, `SV_ExecuteClientMessage`,
`SV_SendClientGameState`, `SV_SendMessageToClient`, `SV_WriteDownloadToClient`,
`Com_Printf`) to inject small "Proxy -->" changes into a verbatim copy of the
original body. The Rust rewrite's mandate is now narrower and explicitly
**not** to reproduce the original's "copy the whole function" style.

## Scope

Wanted:

- Security fixes provided by the proxy — explicitly named: rcon command
  cooldown, max `callvote map_restart` value, model path length. The rest of
  the security/stability family is kept under this umbrella (confirmed
  2026-09-09): q3infoboom guard, bugged-model force, anti-cheat kicks, crash
  guards, forcepowers validation, command `\r\n`/`;` blocking, client-message
  hack guards, download validation, and the `ipAuthorize` anti-DDoS removal
  (see `../inventory.md`).
- Stats at the end of games (personal + global + best-player tables).
- Anti-HP teller (`proxy_sv_antiHpTeller`).
- Minimum jump time (`proxy_sv_minJumpTime`).
- Players net status (`proxy_sv_enableNetStatus` → `netStatus`/`showNet`).

Not wanted (drop entirely, including their cvars):

- Anti wallhack (`proxy_sv_antiWallHack` + the whole `Proxy_sv_snapshot.cpp`
  visibility machinery + the `SV_AddEntToSnapshot_For_Players` call retarget).
- Disable kill command (`proxy_sv_disableKillCmd`).
- Saber FPS (`proxy_sv_sabersFps`).
- Ping fix (`proxy_sv_pingFix`).

Cvar matrix (10 original cvars, `Proxy_g_main.cpp:14-26`):

| cvar                                  | default | keep |
|---------------------------------------|---------|------|
| `proxy_sv_maxCallVoteMapRestartValue` | 60      | yes  |
| `proxy_sv_modelPathLength`            | 64      | yes  |
| `proxy_sv_enableRconCmdCooldown`      | 0       | yes  |
| `proxy_sv_antiHpTeller`               | 0       | yes  |
| `proxy_sv_minJumpTime`                | 0       | yes  |
| `proxy_sv_enableNetStatus`            | 0       | yes  |
| `proxy_sv_pingFix`                    | 0       | no   |
| `proxy_sv_antiWallHack`               | 0       | no   |
| `proxy_sv_disableKillCmd`             | 0       | no   |
| `proxy_sv_sabersFps`                  | 0       | no   |

## D-002.1 — Minimal alteration: inject, do not rewrite

The original game module must stay as untouched as possible. The original
proxy's *whole-function reimplementation* pattern (detour + verbatim copy of
the original body with small edits) is **rejected** as the default technique.

Preferred techniques, in order of preference:

1. **vmMain / trap interception** — runs entirely in the proxy; the game
   module's code is never modified. Already the home of the client-command and
   userinfo filtering (model length, callvote validation, chat/crash guards)
   and `G_LOCATE_GAME_DATA`/`G_GET_USERCMD`.
2. **Intra-function injection** — byte/NOP/immediate patches and jump-splices
   at a precise offset *inside* an original function. Least invasive; the
   function keeps its own control flow. The original proxy already uses this
   for the two rcon inline patches (NOP the 25-byte `SVC_RemoteCommand` timer
   block at `0x8056b26`; resize the `Com_BeginRedirect` buffer immediate from
   `0xbff0` to `0x03` at `0x8056c7d`).
3. **Call-site injection** — retarget a single `call` inside an original
   function to a proxy stub that runs the proxy logic and then calls the
   original target (or returns). Only the one call site is touched.
4. **Entry wrapper detour** — detour at function entry, run a small
   pre/post-check, then invoke the original through the trampoline. Acceptable
   for small additive hooks where the original body is fine (this is the
   original proxy's own pattern for `SVC_Status`, `SV_UserinfoChanged`,
   `G_Damage`, `BeginIntermission`, `G_AddEvent`, `ClientThink_real`, …).

A technique is only acceptable if the original function body is not duplicated
in the proxy. Anything that would require copying the whole body must be
redesigned as 2 or 3 above, or dropped.

## Consequences

- **Ping fix removed ⇒ three whole-function rewrites disappear.**
  `SV_CalcPings` (`Proxy_sv_main.cpp`), `SV_SendMessageToClient`
  (`Proxy_sv_snapshot.cpp:39-42`) and the `messageAcked`/`messageSent` timing
  inside `Proxy_SV_UserMove` (`Proxy_sv_client.cpp:70-77`) are rewritten by the
  original *only* to implement the ping fix. Dropping `proxy_sv_pingFix` means
  the pristine `SV_CalcPings` and `SV_SendMessageToClient` already produce what
  the net-status table needs (`cl->ping`, `messageSent`).
- **netStatus data feed without `SV_ExecuteClientMessage` rewrite.**
  The original feeds per-usercmd stats/timenudge from inside a full
  reimplementation of `SV_ExecuteClientMessage`/`SV_UserMove`. Instead,
  retarget the single `call SV_ClientThink` in the usercmd loop to a proxy
  stub: call original `SV_ClientThink`, then if
  `cl->ping >= 1 && proxy_sv_enableNetStatus` run
  `UpdateUcmdStats`/`UpdateTimenudge`. See `../inventory.md` → "Players net
  status".
- **Rcon without `SVC_RemoteCommand` rewrite.** The original's
  `Proxy_SVC_RemoteCommand` is a pre-check wrapper (cooldown + `writeconfig`
  `.cfg`-only) plus the two inline patches. Keep the two inline patches; port
  the wrapper as an entry wrapper (technique 4). `Proxy_SV_ConnectionlessPacket`
  (full rewrite) is **not** wanted; the only security bit there (dropping
  `SV_AuthorizeIpPacket`) can be a NOP/call-removal at that one call site.
- **Game-side features stay as entry wrappers** — `G_Damage`, `player_die`,
  `BeginIntermission` (stats), `G_AddEvent` (anti-HP), `ClientThink_real`
  (min jump). These already are small additive wrappers; no body duplication.
- **Anti-wallhack detours and its call-site retarget are dropped** —
  `SV_AddEntitiesVisibleFromPoint`, `SV_SendClientSnapshot`, and the
  `0x805882d` retarget.
- **Game cvar hooks dropped** — `G_RegisterCvars`/`G_UpdateCvars` detours are
  not needed: proxy cvars are already registered at `GAME_INIT` and refreshed
  each `GAME_RUN_FRAME` from the proxy's own `vmMain` (D-001).
- **`netStatus`/`showNet` command handling** moves from "forward to game"
  (D-001.2 interim) to "intercept and print", which requires the
  `client_t`/`playerState_t` struct surface and `Proxy_Engine_Client_*`
  port (timenudge magic table + `CalcPacketsAndFPS`). Default cvar value 0
  keeps default behaviour unchanged either way.
- **Out-of-scope hooks kept (user decision 2026-09-09)** — six hooks outside
  the wanted/security lists are kept: `Com_Printf` (redesign), `Cmd_TokenizeString`
  (local-helper redesign), `SV_SendClientGameState` (redesign), `SV_SvEntityForGentity`
  (entry wrapper), `SV_PacketEvent` (verify necessity), `CNavigator::Load`
  (entry wrapper, correct shape). See `../inventory.md`.

### D-002.2 — `CNavigator::Load` shape fix

The original proxy hooks `CNavigator::Load` as a 2-arg `void` function while
the real shape is `(this, filename, checksum) -> bool` (U-006, STATIC). The
Rust wrapper will use the correct shape — a deliberate behavior fix (original
misfire impact INFERRED, never observed). Recorded here so it isn't mistaken
for scope creep.

## Tests

Unchanged from D-001 (unit + `scripts/engine-test.sh`). New work will add per
feature: a differential check against the original proxy in the R-018 container
stack, and for each wanted hook a "poked value observable" test (e.g. rcon
cooldown timing, callvote clamp, model-length clamp, `EV_PAIN` quantisation,
`upmove` hold time, net-status table contents).

## References

- `../inventory.md` — per-hook keep/drop/technique map.
- `../uncertainties.md` U-001 (hook semantics), U-006 (`Navigator::Load` shape).
- `docs/reverse-engineering.md` R-011 (hook inventory), R-018/R-020 (runtime).