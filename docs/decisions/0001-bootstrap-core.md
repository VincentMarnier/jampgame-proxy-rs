# D-001 — Bootstrap core + trap interceptions milestone

Date: 2026-09-09. Evidence: `docs/reverse-engineering.md` R-020 (RUNTIME),
U-001/U-002/U-007/U-008 status; source authority: AGENTS.md, `Proxy_Main.cpp`,
`Proxy_SharedAPI.cpp`, `Proxy_OriginalAPI_Wrappers.cpp`,
`Proxy_Engine_Wrappers.{hpp,cpp}`, `Proxy_g_main.cpp`.

## Scope

Port from C++ to Rust (in `rust/`), in one milestone:

1. Version gate (`version` cvar `strncmp` against `JAmp: v1.0.1.1 linux-i386
   Nov 10 2003`; exit on mismatch).
2. Original-module load + symbol binding: `dlopen("…/jampgame_original.so")`,
   `dlsym` of `dllEntry`/`vmMain`/`level`/`g_entities`, `dladdr` load base.
3. Engine memory layer: `svs`/`sv` and the 17 cvar-pointer slots read at
   `GAME_INIT` (R-018-verified addresses) + the full reference address tables
   from `Proxy_Engine_Wrappers.hpp` kept as data.
4. Proxy cvar mirror (`proxy_sv_*`, defaults from `Proxy_g_main.cpp:14-26`)
   registered at `GAME_INIT` and refreshed every `GAME_RUN_FRAME`. The original
   registers/updates these from a *game* detour (G_RegisterCvars/G_UpdateCvars);
   since game detours are a later milestone, registration happens right after
   the forwarded `GAME_INIT` and updates on `GAME_RUN_FRAME`. Registration is
   idempotent on the engine side, so this is behavior-equivalent at the default
   cvar values.
5. Trap interceptions in the syscall forwarder (`G_LOCATE_GAME_DATA` recorded,
   `G_GET_USERCMD` sanitised after the engine fills the cmd).
6. `vmMain` dispatch: `GAME_RUN_FRAME` (svsTime + cvar refresh),
   `GAME_CLIENT_CONNECT/BEGIN/DISCONNECT/COMMAND/USERINFO_CHANGED` running the
   proxy's own handlers, `GAME_SHUTDOWN` calling `Com_EndRedirect` then
   forward + unload.

## Decisions

### D-001.1 — i386 stack alignment: disable SSE on the i686 target

The 2003 engine enters game modules with a 4-byte-aligned stack; Rust i686
codegen assumes 16-byte entry alignment and emits `movaps`/`movups`, which
fault on the misaligned stack (observed `SIGSEGV` at `movaps %xmm0,0x480(%esp)`
in `vmMain`; gdb-confirmed). The original proxy builds with
`-msse2 -mstackrealign -mfpmath=sse` (CMakeLists.txt:130-134).

Chosen solution: disable SSE on the `i686-unknown-linux-gnu` target
(`rust/.cargo/config.toml` `-C target-feature=-sse,-sse2`) so no
16-byte-aligned instruction can be emitted anywhere in the module, and compile
the C variadic shim with `-mstackrealign` (`rust/build.rs`) since gcc i386
similarly assumes 16-alignment. Rationale: robust against codegen changes and
no per-function wrappers to maintain; float-heavy paths (netStatus FPS math)
arrive with the hook milestones and can re-introduce `-mfpmath=sse` style
handling there if needed.

Rejected: C `force_align_arg_pointer` entry wrappers (csrc/entry.c) — the
cdylib's dynamic-export mechanism only exports Rust `#[no_mangle]` symbols, and
getting the linker to both keep and export C-defined entry symbols under LTO
was unreliable in this toolchain.

### D-001.2 — netStatus/showNet deferred

`Proxy_Engine_ClientCommand_NetStatus` (and its `client_t`/`playerState_t` /
`server.svs->clients` walking) needs the large server-side struct surface of
the hook milestone. While `proxy_sv_enableNetStatus` is non-zero those commands
are forwarded to the game (a divergence from the original, which intercepts
them) and a stderr note is printed. Default cvar value is 0, so default-observable
behaviour matches the original.

### D-001.3 — Q_stricmp corrected binding

The original proxy binds both `Q_stricmp` and `Q_stricmpn` to `0x1a5184`
(`Q_stricmpn`); the real 2-arg `Q_stricmp` is at `0x1a5304` (U-007: benign in
practice, fragile-by-luck). Rust binds the correct address when the hooks that
call it land; the correction is documented in `rust/src/jampgame.rs`.

## Tests

- `rust/` unit tests: SDK layouts vs the compiled-header oracle, trap/vmMain
  ordinals, engine address range sweeps, cvar table, string helpers.
- `rust/scripts/engine-test.sh`: end-to-end engine load + init + UDP query
  (exit 0 = pass). Retries the probe because the proxy's load markers appear
  before the map/UDP listener is ready.

## Not ported (later milestones)

- Engine/game hook patch layer (22 engine + 8 game detours, call-site
  retarget, 2 inline patches) — U-001 remainder.
- `netStatus`/`showNet`, `client_t`/`playerState_t` struct surface, ping-fix /
  anti-wallhack / anti-HP-teller hooks.