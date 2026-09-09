# Architecture

> Status: Initial investigation

 ## Components

  ### Jedi Academy SDK (authoritative game source)

  Location:

  `original/jedi-academy-sdk/`

  Purpose:

  Raven Jedi Academy MP SDK 1.01 — build source for the shipped `jampgamei386.so` (game/cgame/ui + shared `server.h`/`qcommon.h` headers). Authoritative for all game-module ABI: `gameImport_t` trap ordinals, `gameExport_t`, `vmMain`/`dllEntry` signatures, `level`/`g_entities` definitions, and `g_*`/`bg_*` struct layouts.

  Evidence type:

  **SOURCE**

  Authority:

  Overrides `original/jedi-academy/codemp/` and `original/jampgame-proxy/src/sdk/` on any game-ABI divergence (verified 2026-09-09: identical 329-entry `gameImport_t` ordinals in SDK vs proxy `src/sdk/`; `BOTLIB_AI_*CHAT*` present, `G_G2_COLLISIONDETECTCACHE` present, `BOTLIB_EA_*` wrappers present, `g_entities[MAX_GENTITIES]` static array matching binary `B g_entities size 0x17b000`). Contains no engine implementation (headers only, no `sv_*.c`/`vm_*.c`), so it is not authoritative for `linuxjampded` loader/addresses.

 ---

  ### Jedi Academy source (base engine source for `linuxjampded`)

  Location:

  `original/jedi-academy/`

  Purpose:

  Base Jedi Academy source (engine + game) used to build `linuxjampded`. Authoritative for engine behavior: `codemp/server/` (game-server: `sv_game.cpp:1753` `VM_Create("jampgame", ...)`), `codemp/qcommon/vm.cpp:471-518` (`VM_Create` → `Sys_LoadDll` dynamic path used by PC/dedicated builds via `jk2mp.vcproj:1948`/`WinDed.vcproj:279`), `codemp/unix/unix_main.c:323-434` (`Sys_LoadDll` implementation whose `Sys_LoadDll(%s).../succeeded!/found **vmMain**/failed dlsym/dlopen` format strings match the shipped binary), `codemp/qcommon/common.cpp:1511-1512` (`version` cvar as `va("%s %s %s", Q3_VERSION, CPUSTRING, __DATE__)` with `CPUSTRING "linux-i386"` on Linux). NOT authoritative for game-module ABI where `codemp/game/` diverges from the SDK (known divergences: `BOTLIB_AI_*CHAT*` commented out causing +18 ordinal shift, missing `G_G2_COLLISIONDETECTCACHE`, zero `trap_EA_*` wrappers in `g_syscalls.c` despite `BOTLIB_EA_*` enums, `g_entities` as `gentity_t* = NULL` pointer instead of static array, older saber structs, `server_t` `mLocalSubBSP` commented out). `codemp/qcommon/vm_console.cpp` is the Xbox static-namespace path (`x_exe.vcproj:495`), not the PC/dedicated loader.

  Evidence type:

  **SOURCE** (authoritative for engine; non-authoritative for game ABI)

---

  ### Original jampgame\_proxy

  Location:

  `original/jampgame-proxy/`

  Purpose:

  Reference implementation of the existing proxy. Compiles against `src/sdk/`, a light subset derived from the SDK (verified: `g_public.h`/`server.h` differ only by `typedef enum` → `enum` spelling, `_XBOX` stripping keeping the Linux path, and dropped C++ `icarus.h` block — layouts identical on Linux; 329 `gameImport_t` ordinals identical). Shows proxy intent; never overrides the SDK on ABI.

  Evidence type:

  **SOURCE**

---

  ### Original game libraries

  Location:

  `original/jalinuxded_1.011/`

  Purpose:

  Original Linux game-server binaries used as binary/runtime reference material. `jampgamei386.so` is the SDK build output to replace/forward to; `linuxjampded` is the engine to patch/call into, built from `original/jedi-academy/codemp/` (the shipped binary wins where source and binary diverge, e.g. `AutoVersion.h` drift). The SDK contains no engine implementation.

  Evidence type:

  **BINARY**

 These binaries are particularly important for:

 - exported symbols
- ELF structure
- function addresses
- relocations
- calling conventions
- compiled code
- runtime loading behavior
- validating assumptions made from source code

 The exact binary being analyzed must always be identified when recording an address or other binary-specific fact.

### Rust implementation

Location:

`rust/`

Purpose:

New implementation.

## Current understanding

This document will be updated as the original implementation is investigated.

## Unknowns

- Exact runtime loading sequence.
- Exact exported ABI.
- Complete list of hooks.
- Runtime address requirements.
- Which behavior is implemented by the proxy versus the original game code.
- Which binary interfaces must remain compatible.

Notice that we're deliberately **not pretending we know things yet**.