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

  Overrides `original/jedi-academy/codemp/` on any game-ABI divergence (verified 2026-09-09: identical 329-entry `gameImport_t` ordinals in SDK vs the former proxy `src/sdk/`, byte-identical to the pristine SDK — `BOTLIB_AI_*CHAT*` present, `G_G2_COLLISIONDETECTCACHE` present, `BOTLIB_EA_*` wrappers present, `g_entities[MAX_GENTITIES]` static array matching binary `B g_entities size 0x17b000`). Contains no engine implementation (headers only, no `sv_*.c`/`vm_*.c`), so it is not authoritative for `linuxjampded` loader/addresses.

 ---

  ### Jedi Academy source (base engine source for `linuxjampded`)

  Location:

  `original/jedi-academy/`

  Purpose:

  Base Jedi Academy source (engine + game) used to build `linuxjampded`. Authoritative for engine behavior: `codemp/server/` (game-server: `sv_game.cpp:1753` `VM_Create("jampgame", ...)`), `codemp/qcommon/vm.cpp:471-518` (`VM_Create` → `Sys_LoadDll` dynamic path used by PC/dedicated builds via `jk2mp.vcproj:1948`/`WinDed.vcproj:279`), `codemp/unix/unix_main.c:323-434` (`Sys_LoadDll` implementation whose `Sys_LoadDll(%s).../succeeded!/found **vmMain**/failed dlsym/dlopen` format strings match the shipped binary), `codemp/qcommon/common.cpp:1511-1512` (`version` cvar as `va("%s %s %s", Q3_VERSION, CPUSTRING, __DATE__)` with `CPUSTRING "linux-i386"` on Linux). NOT authoritative for game-module ABI where `codemp/game/` diverges from the SDK (known divergences: `BOTLIB_AI_*CHAT*` commented out causing +18 ordinal shift, missing `G_G2_COLLISIONDETECTCACHE`, zero `trap_EA_*` wrappers in `g_syscalls.c` despite `BOTLIB_EA_*` enums, `g_entities` as `gentity_t* = NULL` pointer instead of static array, older saber structs, `server_t` `mLocalSubBSP` commented out). `codemp/qcommon/vm_console.cpp` is the Xbox static-namespace path (`x_exe.vcproj:495`), not the PC/dedicated loader.

  Evidence type:

  **SOURCE** (authoritative for engine; non-authoritative for game ABI)

---

  ### Original jampgame\_proxy (removed)

  Location:

  `original/jampgame-proxy/` — **removed 2026-09-10** after the migration
  completed; recover with
  `git clone https://github.com/VincentMarnier/jampgame_proxy <dir>` (pinned
  source commit `687997412ea6e5ead93f5c7b25db552590a2eeb1`; the submodule's
  git history is also retained locally in
  `.git/modules/original/jampgame-proxy`).

  Purpose (historical):

  Reference implementation the Rust port was built from. Compiles against `src/sdk/`, a light subset derived from the SDK (verified before removal: `g_public.h`/`server.h` differ only by `typedef enum` → `enum` spelling, `_XBOX` stripping keeping the Linux path, and dropped C++ `icarus.h` block — layouts identical on Linux; 329 `gameImport_t` ordinals identical). Its findings are preserved in `docs/reverse-engineering.md`, `docs/inventory.md`, and `docs/decisions/`; treat those docs as the surviving record, not the deleted tree.

  Evidence type:

  **SOURCE** (historical; recovered only via git history)

---

  ### Original game libraries

  Location:

  `original/jalinuxded_1.011/`

  Purpose:

  Original Linux game-server binaries used as binary/runtime reference material. `jampgamei386.so` is the SDK build output to replace/forward to; `linuxjampded` is the engine to patch/call into, built from `original/jedi-academy/codemp/` (the shipped binary wins where source and binary diverge, e.g. `AutoVersion.h` drift). `libcxa.so.1` in the same directory is NOT an investigation target: it is the Intel C++ runtime both binaries `NEEDED`-depend on (provides `__vec_*`/`__ex_*`/`__eh_*`), shipped because modern systems lack it. The SDK contains no engine implementation.

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

See `docs/reverse-engineering.md` (R-001…R-016) for evidence-backed findings
and `docs/uncertainties.md` for the tracked open items.

## Unknowns

Resolved since the initial skeleton (2026-09-09):

- Exact exported ABI — 13-int `vmMain` (decided), cdecl `dllEntry`, fixed-arity
  forwarders (TESTED), `Navigator_Load` shape (STATIC).
- Struct layouts — proxy `src/sdk/` ≡ pristine SDK (TESTED) and match binary
  sizes.
- Engine address table — detour-safe mechanically (sweep TESTED).
- `Q_stricmp` misbinding — benign-in-practice, fix decided (RUNTIME).
- Original-library load path — CWD-relative `dlopen` (RUNTIME).
- Detour page-protection primitives — container-verified (RUNTIME).
- Engine var/cvar slots, live hook identity, and the full proxy attach — the
  original proxy builds and runs end-to-end in the i386 container under the
  real engine (RUNTIME, R-018): every memory-layer address read back,
  connectionless/query/rcon hook addresses fire on the correct traffic, all
  detours/inline patches applied on the real engine `.text`, server still
  answers queries.

Still open:

- Per-hook semantic audit (observe/mutate/block) — source-level, tracked as
  U-001; the R-018 container stack is the differential harness for it.
- Semantic identity of the remaining engine hook addresses (download/snapshot/
  userinfo/Navigator families) — folded into U-001 per-hook tests (U-005
  remainder).
- `CNavigator::Load` hook misfire impact — INFERRED, unobserved because
  `mp/duel1` does not use the navigator (U-006 remainder).

Notice that we're deliberately **not pretending we know things yet** — items
above are only as strong as their evidence labels.