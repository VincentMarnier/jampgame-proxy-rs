# Migration report: jampgame-proxy (C++) → Rust

Date: 2026-09-10. Cross-checked `docs/inventory.md` (source of truth) against
the actual Rust code (`rust/src/`) and the original proxy source. They are
consistent. Status as of R-022 / D-003.

## Migrated (ported and present in `rust/src/`)

### Core module (D-001)

- `vmMain` dispatch + `dllEntry`/ABI layer (`shared_api.rs`, `jampgame.rs`)
- Trap syscall forwarder with interceptions of `G_LOCATE_GAME_DATA`,
  `G_GET_USERCMD` (`syscall.rs`)
- Engine memory layer: `svs`/`sv`/`client_t` surface, proxy cvar mirror
  registered at `GAME_INIT`, refreshed per frame (`engine.rs`, `state.rs`,
  `lib.rs`)
- Version gate, original `jampgamei386.so` load/bind, patch installation
  engine (`original.rs`, `patch.rs` — trampoline detours, NOP/byte patches,
  call retargets, branch flips)

### Security fixes — engine hooks (R-021, `hooks/engine_sv.rs`)

- rcon cooldown + `writeconfig` `.cfg`-only entry wrapper; NOP of timer block;
  `Com_BeginRedirect` buffer 49136→1008 byte patch
- `Com_Printf` hardening: `vsprintf`→`vsnprintf` call-site retarget via shim
  (`csrc/syscall_shim.c`)
- q3infoboom guards on `SVC_Status`/`SVC_Info`
- Model path length: engine `SV_UserinfoChanged` entry wrapper
- Download validation: `SV_BeginDownload_f` (`.pk3`+traversal),
  `SV_{Next,Stop,Done}Download_f` state guards, `SV_UpdateUserinfo_f`
  empty-arg, `SV_WriteDownloadToClient` referenced-paks; proxy-local helpers
  `cmd_tokenize_string2`, `FS_FilenameCompare`, `FS_CheckDirTraversal`
  (`hooks/compat.rs`)
- anti-DDoS: `SV_AuthorizeIpPacket` call removed (NOP)
- `SV_SvEntityForGentity` bounds-check entry wrapper
- `CNavigator::Load` entry wrapper with the correct
  `(this, filename, checksum) -> bool` shape (D-002.2)
- `SV_SendClientGameState` redesign: CS_CONNECTED→CS_PRIMED guard + RMG
  else-path `je`→`jmp`
- `SV_ExecuteClientMessage` entry wrapper: the three upstream diffs ported
  (`nextdl` tolerance, map_restart `restartedServerId` range, `state !=
  CS_ACTIVE` resend guard)

### Security fixes — vmMain (R-020, `shared_api.rs`)

- Chat/crash guards, forcepowers validation, anti-cheat (`jkaDST_`,
  `darksidetools`), callvote `map_restart` range, newline/semicolon blocking,
  name/model userinfo sanitisation

### Game wrappers (`hooks/game.rs`)

- `G_Damage`/`player_die`/`BeginIntermission` (stats tables)
- `G_AddEvent` (anti-HP teller)
- `ClientThink_real` (min jump time)

### netStatus (`hooks/netstatus.rs`)

- Command interception, per-usercmd feed via the `SV_ClientThink` call-site
  retarget, fps/packets/timenudge math — runtime-verified (A1/A1b,
  `tools/runtime/a1_netstatus.sh`), with documented R-022 deviations
  (ping gate dropped, bot discrimination via `SVF_BOT` in the printer,
  division hardening).

### Cvars

- All 6 kept cvars mirrored (`state.rs:PROXY_CVARS`).

## Deliberately dropped (not "missing")

- `proxy_sv_antiWallHack` (whole `sv_snapshot` family)
- `proxy_sv_pingFix` (whole-function rewrites of `SV_CalcPings` /
  `SV_SendMessageToClient` / `SV_UserMove`)
- `proxy_sv_sabersFps`
- `proxy_sv_disableKillCmd` (arm removed; cvar dropped)
- `SV_PacketEvent` hook — verified no-op (pristine `SV_ReadPackets` already
  covers it; D-003.1)
- `G_RegisterCvars`/`G_UpdateCvars` detours (replaced by the vmMain cvar
  mirror)

## Not yet verified / remaining work

- **Runtime-unverified** (needs a real client/scenario):
  - netStatus table rendering end-to-end
  - download validation with `sv_allowDownload` + a client needing files
  - `CNavigator::Load` on a nav-using map
- **Deferred tests** (D-002 Tests section): `Com_Printf` /
  `SV_SendClientGameState` / download differential tests against the original
  proxy in the R-018 container stack; per-hook "poked value observable" tests.
  `tests/` is currently empty.

## Summary

The entire D-002 keep set is ported; no wanted feature is missing. Remaining
work is verification (differential/runtime tests), not implementation. Note:
D-002 rejected full function-body duplication; the two features that
originally used it (`SV_WriteDownloadToClient`, `Com_Printf`) were redesigned
as wrappers/retargets, so nothing remains unported in the keep set.
