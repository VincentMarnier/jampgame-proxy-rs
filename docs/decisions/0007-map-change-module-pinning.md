# D-007 — Map-change crash: pin the proxy module across the engine's `dlclose`

Date: 2026-10-17. Evidence: runtime (docker, `linuxjampded` + `jampgamei386.so`
v0.4.0), gdb crash dumps and watchpoint traces reproduced below; engine source
`original/jedi-academy/codemp/server/sv_game.cpp:1669-1675` (`SV_ShutdownGameProgs`
→ `VM_Call(GAME_SHUTDOWN)` → `VM_Free`), `codemp/qcommon/vm.cpp:605-608`
(`VM_Free` → `Sys_UnloadDll`), `codemp/server/sv_init.cpp:423-440`
(`SV_SendMapChange`), `codemp/qcommon/cvar.cpp:901-916` (vmCvar mirrors are not
retained engine-side).

## Symptom (reported)

Map change kills the server. The reporter's log shows a complete proxy
shutdown (`detached …`, `ShutdownGame`, `jampgame_original.so properly
unloaded`) and then death with no further proxy output.

## Reproduction (observed)

`tools/runtime` map-change storm: 4 dedicated-server instances
(`bot_enable 1`, `g_gametype 0`), 6 consecutive `rcon map mp/ffaN` rounds.
Before the fix: 3 of 4 instances died (SIGSEGV), always within one `map`
command. Plain (non-rcon) map changes and `map_restart` survived in earlier
matrix runs.

## Crash signature (observed)

All three crashes are the same instruction, expressed per instance:

| instance | crash EIP   | old module base | offset |
|----------|-------------|-----------------|--------|
| mcs-1    | 0xee70db57  | 0xee706000      | 0x7b57 |
| mcs-2    | 0xe8c91b57  | 0xe8c8a000      | 0x7b57 |
| mcs-3    | 0xef5a3b57  | 0xef59c000      | 0x7b57 |

The old module base is derived from the last `original::VM_MAIN` watchpoint
address (`base = addr - 0x88738`). Offset `0x7b57` is the instruction right
after the trampoline `call *%eax` inside
`hooks::engine_sv::svc_remote_command` (`0x79f0`):

```
7b55:  ff d0                 call   *%eax        ; trampoline → real SVC_RemoteCommand
7b57:  83 c4 3c              add    $0x3c,%esp   ; <- SIGSEGV here (unmapped)
```

Data watchpoints on `original::VM_MAIN` / `syscall::ENGINE_SYSCALL` show no
corrupting write before the crash: the module's statics hold correct values at
death (post-mortem dump), and the `dlclose`d instance's memory is unmapped.

## Root cause (verified for the rcon path)

An rcon command executes **synchronously inside** our `SVC_RemoteCommand`
wrapper's trampoline call:

1. engine dispatches `SVC_RemoteCommand` → detoured into our wrapper
   (`hooks::engine_sv::svc_remote_command`);
2. the wrapper calls the trampoline, pushing its own return address
   (`0x7b57`) and jumping into the real `SVC_RemoteCommand`;
3. the rcon command `map …` runs inline: `SV_SpawnServer` →
   `SV_ShutdownGameProgs` → our `vmMain(GAME_SHUTDOWN)` (detach + unload) →
   `VM_Free` → `Sys_UnloadDll` → **`dlclose` of our own module** — the wrapper
   code and its return address are unmapped mid-call;
4. the new instance loads and initialises (all of it inside the same call);
5. the real `SVC_RemoteCommand` returns to `0x7b57` — instruction fetch on an
   unmapped page → SIGSEGV.

This also explains why the reporter sees the shutdown output and then silence
(engine `Com_Printf` output after that point is lost in stdio buffering on the
segv). An earlier, differently-shaped crash (`call 0x26000000` through
`original::VM_MAIN`, whose slot held the binary's raw `.debug_aranges` file
bytes) is consistent with stale execution against the dropped instance's
address range after it was reused as an un-zeroed file mapping; both shapes
require the module to be unmapped while its code can still be re-entered.

## Fix

1. **Pin the proxy module for the process lifetime**
   (`original::pin_own_module()`, called from `dllEntry`): a self-`dlopen`
   keeps the reference count ≥ 1, so the engine's `dlclose` can no longer unmap
   us and stale returns complete against mapped code. The engine's next
   `dlopen` resolves to the same object (same path/inode), i.e. the module
   instance is reused across map changes.
2. **Reset per-session state at `GAME_INIT`** (`state::reset_session()`):
   instance reuse must preserve the documented semantics "a full map change
   re-dlopens the module (fresh state)", so the whole `ProxyState` is dropped
   and rebuilt. Hook tables already cycle through `attach_all`/`detach_all`;
   `original::*` caches are rebuilt by `original::load`.

## Consequences / invariants

- `jampgamei386.so` stays mapped until process exit (one module instance per
  process). No new per-map memory growth: the engine reuses the same mapping.
- Everything that used to be reset "for free" by `dlclose`/`dlopen` must now be
  reset explicitly in `game_init`/`game_shutdown`. Any future static state
  outside `state::ProxyState` is subject to this rule.
- The pristine-game module `jampgame_original.so` is still unloaded/reloaded
  per map change as before (`original::unload` in `GAME_SHUTDOWN`).

## Verification

- `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test` (60 tests) pass.
- The map-change storm that killed 3/4 instances before the fix runs clean
  after it (0 crashes over the same 4 × 6 rounds), with the module instance
  reused across reloads.
