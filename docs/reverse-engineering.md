# Reverse engineering — game libraries relevant to `jampgame_proxy`

Note (2026-09-10): `original/jampgame-proxy/` was removed after the migration
completed. Citations of its paths in this document are historical evidence
records — recover the referenced source with
`git clone https://github.com/VincentMarnier/jampgame_proxy <dir>` (pinned
commit `687997412ea6e5ead93f5c7b25db552590a2eeb1`; the submodule's git
history is also retained locally in
`.git/modules/original/jampgame-proxy`). Do not reinterpret the findings
from the Rust code alone.

Scope of this investigation (2026-09-09): identify every relevant ELF binary in
`original/jalinuxded_1.011/`, describe its ELF/ABI properties with
`file`/`nm`/`readelf`/`objdump`, and cross-reference every symbol, address and
ABI assumption used by `original/jampgame-proxy/`. No `original/` file was
modified. No `rust/` code was written. R-001…R-015 are **SOURCE** (proxy/SDK/
engine source) or **STATIC** (binary analysis); R-016…R-018 add **RUNTIME**
observations from the i386 container (see `docs/runtime-environment.md`,
`tools/runtime/`). Hypotheses are marked **INFERRED**.

Related docs: `architecture.md` (component/source authority), `inventory.md`
(source-verified inventory, if populated), `uncertainties.md` (open questions).

Binaries under investigation (proxy targets):

- `original/jalinuxded_1.011/linuxjampded` — engine (dedicated server).
- `original/jalinuxded_1.011/jampgamei386.so` — game module (the library the
  proxy replaces/forwards to as `jampgame_original.so`).

Runtime support library (NOT an investigation target — do not hook, patch, or
reimplement it):

- `original/jalinuxded_1.011/libcxa.so.1` — Intel C++ runtime (`SONAME
  libcxa.so.1`, `ELF32 i386`, not stripped, 142 dynamic symbols), shipped
  alongside because modern systems no longer provide it. Both binaries above
  list it in `NEEDED` (with `libdl.so.2`, `libm.so.6`, `libc.so.6`), and it
  satisfies their Intel-EH imports (`__vec_new/__vec_delete`, `__ex_*`,
  `__eh_*`, `__dla__FPv`/`__dl__FPv`, `__cxa_atexit` — all present, verified
  with `nm -D`). It itself needs only `libc.so.6`. Built with the same
  toolchain family (`GCC 2.96 Red Hat 7.1` + `Intel C++ 5.0.1 Build 01073…`,
  per `.comment`). Needed at runtime (`LD_LIBRARY_PATH` / same directory) to
  execute either binary; it contains no game or engine logic.

---

## R-001 — Relevant binaries: architecture and ELF class

### Finding

Both shipped binaries are 32-bit little-endian x86 (`Intel 80386`), `ELF32`,
`SYSV`, dynamically linked, stripped. The engine is `ET_EXEC` (fixed load
address, non-PIE); the game is `ET_DYN` (position-dependent shared object with
`TEXTREL`, loaded at a runtime base).

### Evidence

- **STATIC** — `file`:
  - `linuxjampded: ELF 32-bit LSB executable, Intel i386, version 1 (SYSV),
    dynamically linked, interpreter /lib/ld-linux.so.2, for GNU/Linux 2.0.0,
    stripped`
  - `jampgamei386.so: ELF 32-bit LSB shared object, Intel i386, version 1
    (SYSV), dynamically linked, stripped`
- **STATIC** — `readelf -h`:
  - `linuxjampded`: `Class ELF32`, `Machine Intel 80386`, `Type EXEC`, entry
    `0x804a1e0`, 6 program headers, 27 section headers.
  - `jampgamei386.so`: `Class ELF32`, `Machine Intel 80386`, `Type DYN`, entry
    `0x85564`, 3 program headers, 28 section headers.

### Confidence

`High`.

### Implications

The Rust replacement for `jampgame_proxy` is i386-only and must build as 32-bit
x86 (`-m32`, `i386`), `ET_DYN`, exposing unmangled `vmMain`/`dllEntry`. The
proxy's absolute engine addresses (e.g. `0x0804xxxx`) only work because the
engine is a non-PIE `ET_EXEC` fixed at `0x08048000`. There is no x86-64 port:
no 64-bit relocations, address truncation, or `MAP_32BIT` handling is required.

### Remaining questions

None on class/arch. See U-002 for runtime-base questions.

---

## R-002 — Sections and loadable segments

### Finding

Engine has two `LOAD` segments; game has two `LOAD` segments plus `TEXTREL`.

### Evidence

- **STATIC** — `readelf -l`:
  - `linuxjampded`:
    - `LOAD 0x08048000 filesz 0x1514c4 R+E` (covers `.interp .note .hash .dynsym
      .dynstr .rel.* .init .plt .text .fini .rodata`)
    - `LOAD 0x0819a4e0 filesz 0x3d57c memsz 0x1fc6d4 RW` (covers `.data .data1
      .ctors .dtors .got .dynamic .bss`; large `BSS`)
    - `DYNAMIC at 0x081d79a4`, `INTERP /lib/ld-linux.so.2`.
  - `jampgamei386.so`:
    - `LOAD 0x00000000 filesz 0x1a66eb R+E` (covers `.hash .dynsym .dynstr
      .rel.text .rel.gdt .rel.edt .rel.data .rel.got .rel.plt .init .plt .text
      .fini`)
    - `LOAD 0x001a7700 filesz 0x3b59c memsz 0x87ad00 RW` (covers `.data .data1
      .ctors .dtors .got .dynamic .bss`; very large `BSS`)
    - `DYNAMIC at 0x001e2bd4`, `TEXTREL` present.
- **STATIC** — `readelf -S` key ranges:
  - Engine `.text 0x0804a1e0 size 0x14f2b4`; `.data 0x0819a4e0 size 0x02fd30`;
    `.bss NOBITS 0x081d7a60 size 0x1bf154`.
  - Game `.text 0x00085564 size 0x121160`; `.data 0x001a7700 size 0x03b338`;
    `.bss NOBITS 0x001e2ca0 size 0x83f760`.
  - All proxy engine function/var/cvar addresses fall inside these ranges
    (checked programmatically; see R-010).

### Confidence

`High`.

### Implications

- Engine absolute addresses in `Proxy_Engine_Wrappers.hpp` are file-virtual
  addresses (no rebasing) — correct only for this `ET_EXEC`.
- Game offsets in the same header (`0x000868a4`, …) are file offsets that must
  be rebased by the runtime base (`proxy.jampgameAddress`); the proxy does this
  for hooks (`originalFunctionAddr += proxy.jampgameAddress`) and for
  `jampgame.functions.*` (`proxy.jampgameAddress + offset`). This matches the
  `ET_DYN` base-`0` layout.
- `TEXTREL` + `R_386_32` relocs in `.text` mean the game `.text` is patched at
  load; the proxy's `mprotect`-based detours operate on already-relocated,
  writable-then-executable pages.

### Remaining questions

See U-002 (verify runtime base via `dladdr` equals `dli_fbase` used by proxy).

---

## R-003 — Dynamic dependencies and version needs

### Finding

Both binaries need the same four shared libraries. No other `NEEDED` entries.

### Evidence

- **STATIC** — `readelf -d` for both:
  - `NEEDED libdl.so.2`, `libm.so.6`, `libcxa.so.1`, `libc.so.6`.
  - Engine `INIT 0x8049a40 FINISH 0x8199494`; game `INIT 0x8550c FINISH
    0x1a66c4`; `PLTGOT`, `JMPREL`/`REL`/`RELSZ` present; game additionally
    `TEXTREL`, `VERDEFNUM 2`.
- **STATIC** — `readelf -V`: `GLIBC_2.0`, `GLIBC_2.1`, `GLIBC_2.1.3` version
  needs (e.g. `__cxa_atexit@GLIBC_2.1.3`, `dlopen@GLIBC_2.1`).

### Confidence

`High`.

### Implications

Rust `jampgame*.so` must keep `NEEDED` minimal and compatible with a 2003-era
loader (`ld-linux.so.2`, `GLIBC_2.0/2.1`). Avoid newer symbol versions unless
the test matrix proves the target loader accepts them.

### Remaining questions

None. Runtime loader search path (`fs_basepath/fs_game` construction) is a
separate engine-behavior question (see R-014).

---

## R-004 — Game exports: `vmMain`, `dllEntry`, `level`, `g_entities`

### Finding

`jampgamei386.so` exports 3036 dynamic symbols (`nm -D` count: 2555 `T`, 339
`B`, 98 `D`, 26 `U`). The four symbols the proxy `dlsym`s all exist with the
sizes below. The proxy itself must export the first two.

### Evidence

- **STATIC** — `nm -D jampgamei386.so | grep`:
  - `00085684 T vmMain`
  - `0015e104 T dllEntry`
  - `0068a3a0 B level`
  - `006ce620 B g_entities`
  - `00695d00 B g_clients` (used indirectly via `G_LOCATE_GAME_DATA`, not
    `dlsym`).
- **STATIC** — `readelf --dyn-syms` sizes:
  - `vmMain size 1632 FUNC GLOBAL DEFAULT sect 15`
  - `dllEntry size 16 FUNC GLOBAL DEFAULT sect 15`
  - `level size 45440 OBJECT GLOBAL DEFAULT sect 24`
  - `g_entities size 0x17b000 OBJECT GLOBAL DEFAULT sect 24`
  - `g_clients size 0x38800 OBJECT GLOBAL DEFAULT sect 24`
- **SOURCE** — proxy contract:
  - `original/jampgame-proxy/src/jampgame_proxy/Proxy_Main.cpp:24,46,58,68`:
    `dlsym(handle,"dllEntry")`, `dlsym(handle,"vmMain")`,
    `dlsym(handle,"level")`, `dlsym(handle,"g_entities")`, each fatal if NULL.
  - `original/jampgame-proxy/src/jampgame_proxy/Proxy_Main.cpp:82,236` and
    `Proxy_Main.hpp:12`: `extern "C" __attribute__((visibility("default")))
    int vmMain(...)` and `void dllEntry(...)`.
  - `original/jampgame-proxy/src/sdk/game/JK2_game.def`: `EXPORTS dllEntry
    vmMain` (Windows `.def`; on Linux visibility attribute + `extern "C"`
    provides the same two unmangled default-visibility exports).
  - `original/jampgame-proxy/CMakeLists.txt:99`: `-fvisibility=hidden` project
    wide, so only the two annotated symbols are exported — intended.
- **SOURCE** — SDK authority (`original/jedi-academy-sdk/codemp/game/`):
  - `g_syscalls.c:14` `void dllEntry(int (QDECL *syscallptr)(int,...))`,
    `g_main.c:503` `int vmMain(int command, int arg0, …, int arg11)`.

### Confidence

`High` for symbol presence/sizes and proxy's four `dlsym` names. `High` that
the proxy must export exactly `vmMain`+`dllEntry`.

### Implications

Rust must export exactly `vmMain` and `dllEntry` unmangled, default visibility,
with C ABI. `level`/`g_entities` are *imported* by the proxy via `dlsym` from
the original — the Rust proxy must look them up the same way, not define its
own, and must use SDK `level_locals_t`/`gentity_t` layouts (`#[repr(C)]`).

### Remaining questions

- U-001 (complete surface — answered for `dlsym`; hook surface is R-011).
- Decided: `vmMain` is 13 `int`s (`command+arg0..arg11`) per the original game
  (SDK `g_main.c:503` authoritative; maintainer directive). The proxy's 12-arg
  declaration/forwarding is a deviation, not the spec — see R-008.

---

## R-005 — Game undefined/imported symbols (only 26)

### Finding

The game imports almost nothing: 26 `U` entries, all low-level libc/compiler
helpers. It does *not* import engine functions — engine services arrive via the
`dllEntry` syscall pointer and `vmMain` args.

### Evidence

- **STATIC** — `nm -D jampgamei386.so | grep " U "` (full list, 26 lines):
  `acos, atoi, __cxa_atexit, __cxa_finalize, __eh_destroy_gdt_section_list,
  __eh_init_first, __eh_term_last, __eh_update_gdt_section_list, __ex_caught,
  __ex_free, __ex_rethrow_q, __ex_skip, fmod, longjmp, ___pseudo_link, _setjmp,
  signal, sscanf, strchr, strncpy, strstr, terminate__3stdFv, tolower, toupper,
  vsprintf, __vtbl__Q2_3std9type_info` (with `GLIBC_2.0/2.1.3` versions where
  applicable).
- **STATIC** — contrast: 2555 `T` game-internal functions (mangled `__F*`
  Itanium-style, e.g. `G_Damage__FP9gentity_sN21PfT4iN26`), confirming
  Intel C++ 5.0.1 build (see R-015).

### Confidence

`High`.

### Implications

Rust game-side shims cannot `extern` engine functions directly; they must go
through the syscall pointer obtained in `dllEntry`, exactly as
`Proxy_OriginalAPI_VM_DllSyscall` does. The small import surface also means a
Rust `jampgame*.so` with the same 4 `NEEDED` libs is plausible.

### Remaining questions

None.

---

## R-006 — Engine dynamic symbols: stripped, import-only

### Finding

`linuxjampded` is stripped: 141 dynamic symbols total — 116 `U` imports, 4 `T`
(`_init/_fini` + two EH landing pads), plus version/weak/GOT markers. There are
no engine function names to resolve hooks by; hard-coded absolute addresses are
the only static reference.

### Evidence

- **STATIC** — `nm -D linuxjampded | wc -l` = 141; `grep -c " U "` = 116;
  `grep -c " T "` = 4 (`_init`, `_fini`, two `$$eh_landing_pad*`).
- **STATIC** — `readelf -r linuxjampded`: 1 `R_386_GLOB_DAT`, 2 `R_386_COPY`
  (`stdout`, `stderr`), 117 `R_386_JUMP_SLOT` (PLT for `dlopen/dlsym/printf/…`).
  No `.rel.text` with engine-internal relocs — expected for `ET_EXEC`.
- **STATIC** — `objdump -d` shows hook targets exist but are labeled only
  `<$$eh_landing_pad…+offset>` (stripped), e.g. `0x8056b14`, `0x8072ca4`,
  `0x8124ff4` all disassemble to valid prologues (`push %ebp; mov %esp,%ebp;…`).

### Confidence

`High`.

### Implications

Rust cannot `dlsym` engine functions (no names). It must either keep the
proxy's hard-coded address table (version-locked to `1.0.1.1`) or implement a
signature scan. Any address change requires re-validation. Tests must pin the
binary (`sha`/size) the addresses were validated against.

### Remaining questions

U-002/U-005: engine variable addresses (`0x83121e0`, …) have no symbol names in
the stripped binary; they are validated only by segment-range + source
correlation, not by name. Runtime `GDB` read-back is still needed for full
confidence.

---

## R-007 — Relocations

### Finding

Game has ~48k relocs (`readelf -r | grep -c R_386` = 48168) including
`R_386_RELATIVE`, `R_386_PC32`, `R_386_32` (hence `TEXTREL`); engine has 120
(`GLOB_DAT` + `COPY` + `JUMP_SLOT`).

### Evidence

- **STATIC** — `readelf -r jampgamei386.so | head`: `.rel.text` alone has 38337
  entries (`R_386_RELATIVE`, `R_386_PC32` against `__ex_*`, `R_386_32` against
  `g_entities`/`gSharedBuffer`, …). Full breakdown dominated by
  `R_386_RELATIVE` (~18k), `R_386_PC32` (~12k), `R_386_32` (~17k) — exact split
  varies with `readelf` line wrapping; total 48168 is exact.
- **STATIC** — `readelf -r linuxjampded`: `.rel.got` 1, `.rel.bss` 2, `.rel.plt`
  117 (all `R_386_JUMP_SLOT` to `libc/libdl/libm/libcxa` PLT).
- **STATIC** — `readelf -d jampgamei386.so` contains `TEXTREL`; `linuxjampded`
  does not.

### Confidence

`High` on totals and `TEXTREL`-vs-not; `Medium` on exact per-type split (locale
line-wrapping in `readelf` output — reproducible via `LC_ALL=C readelf -r`
plus scripted counting in `tools/` if needed).

### Implications

Game hooks must run *after* the loader applies relocs (proxy does: hooks attach
at `GAME_INIT`, long after `dlopen(RTLD_NOW)` of the original). Detour writes
to `.text` require `mprotect` (proxy's `UnProtect/ReProtect`); game `.text`
relocs prove `.text` was already made writable once by the loader.

### Remaining questions

None blocking. A reproducible reloc-count script belongs in `tools/`.

---

## R-008 — Proxy's own ABI surface (what Rust must replicate)

### Finding

The proxy presents exactly the game ABI to the engine and consumes the engine
ABI via syscalls + absolute addresses. Key signatures (all i386 cdecl, i386-only;
`QDECL` is empty on Linux):

- Authoritative `vmMain`: `int vmMain(int command, int arg0…arg11)` (13 `int`s)
  per the original game (SDK `g_main.c:503`). The proxy declares and forwards
  only `command+arg0…arg10` (12 `int`s) — a proxy deviation, not the spec. Rust
  must implement and forward all 13.
- `void dllEntry(int (QDECL *)(int,…))` — stores engine syscall pointer.
- `int QDECL Proxy_OriginalAPI_VM_DllSyscall(int command, …)` — 1+16 forwarded
  args; intercepts `G_LOCATE_GAME_DATA` and `G_GET_USERCMD`, forwards the rest
  via `proxy.originalSystemCall`.
- `int QDECL Proxy_OriginalAPI_VM_Call(intptr_t command, …)` — 1+11 args,
  calls proxy `vmMain` (header spells first param `int command`; `.cpp` spells
  `intptr_t command` — identical on i386 ILP32, see below).

### Evidence

- **SOURCE** — `Proxy_Main.cpp:82-83,236-238`, `Proxy_Main.hpp:12`,
  `Proxy_Header.hpp:40-42` (`systemCallFuncPtr_t`, `vmMainFuncPtr_t` (12 args),
  `dllEntryFuncPtr_t`), `Wrappers/Proxy_OriginalAPI_Wrappers.{hpp,cpp}`,
  `Proxy_Translate_SystemCalls.{hpp,cpp}` (trap wrappers all call
  `proxy.originalSystemCall(G_*)`).
- **SOURCE** — `src/sdk/game/q_shared.h:114` (`#define QDECL` empty on Linux;
  `#define QDECL __cdecl` only on `WIN32`), so all `QDECL` variadics are plain
  cdecl: args on stack, caller cleans, return in `EAX`, `EAX/EDX/ECX`
  caller-saved.
- **SOURCE** — `Proxy_Header.hpp:125` `uint jampgameAddress` (32-bit) holding a
  `dladdr(dli_fbase)` pointer — correct on i386 (`uint`/pointer both 32-bit);
  no 64-bit port is planned, so no truncation concern.
- **SOURCE (authoritative)** — original game `vmMain` takes 13 `int`s
  (`int vmMain(int command, int arg0, …, int arg11)`,
  `original/jedi-academy-sdk/codemp/game/g_main.c:503`; same shape in
  `cgame/cg_main.c:190` and `ui/ui_main.c:258`). Engine's `Sys_LoadDll`/`VM_Create`
  use `int (*)(int,…)` variadic entry points (`unix_main.c:417-418`,
  `vm.cpp:518`), so the proxy's 12-arg call still links/runs, but it drops
  `arg11` — a deviation from the game ABI.
- **DECIDED** — per maintainer directive: trust the original game. 13 args is
  the spec; the proxy's 12 is not to be preserved bug-for-bug.

### Confidence

`High` on signatures and `QDECL`-is-cdecl. `High` on 13-arg authority (SDK game
source + maintainer directive).

### Implications

Rust must declare (i386 `extern "C"` = cdecl):

```rust
#[unsafe(no_mangle)] pub unsafe extern "C" fn vmMain(cmd: c_int, arg0: c_int, arg1: c_int, arg2: c_int, arg3: c_int, arg4: c_int, arg5: c_int, arg6: c_int, arg7: c_int, arg8: c_int, arg9: c_int, arg10: c_int, arg11: c_int) -> c_int
#[unsafe(no_mangle)] pub unsafe extern "C" fn dllEntry(sys: unsafe extern "C" fn(c_int, ...) -> c_int)
```

with `#[repr(C)]` for all shared structs. Forward all 13 `vmMain` args to the
original (do not replicate the proxy's dropped-`arg11` behavior). The base
address fits in `u32`/`usize` on i386; keep the `dladdr(dli_fbase)` + offset
rebasing scheme for game addresses.

### Remaining questions

U-006 (variadic forwarding must be written in asm/C-shim — Rust `...` FFI is
unstable; plan a tiny C trampoline or fixed-arity forwarder like the proxy's
16-arg array).

---

## R-009 — Symbols and strings the proxy depends on (besides hooks)

### Finding

Four `dlsym` names, one `dlopen` path, one version gate, and ~329 syscall
ordinals.

### Evidence

- **SOURCE** — `Proxy_Main.cpp:24,46,58,68`: `"dllEntry"`, `"vmMain"`,
  `"level"`, `"g_entities"` (+ `dladdr` for `jampgameAddress`).
- **SOURCE** — `Proxy_Files.cpp`: `dlopen(fs_game + "/jampgame_original.so",
  RTLD_NOW)` where `fs_game` is the `fs_game` cvar or `base` default
  (`FS_GAME_CVAR`, `DEFAULT_BASE_GAME_FOLDER_NAME`, `PROXY_LIBRARY_NAME` in
  `Proxy_Header.hpp:22-30`). Fatal exit if `dlopen` fails.
- **SOURCE** — `Proxy_Header.hpp:31` `ORIGINAL_ENGINE_VERSION "JAmp: v1.0.1.1
  linux-i386 Nov 10 2003"` checked with `strncmp` against the `version` cvar at
  `GAME_INIT` (`Proxy_Main.cpp:96-105`); mismatch exits.
- **STATIC** — version substrings all present in `linuxjampded` (`strings`:
  `JAmp: v1.0.1.1`, `linux-i386`, `Nov 10 2003`; engine source builds it as
  `va("%s %s %s", Q3_VERSION, CPUSTRING, __DATE__)` in `common.cpp:1511-1512`
  with `CPUSTRING "linux-i386"`).
- **SOURCE** — `Proxy_Translate_SystemCalls.hpp` declares the full trap set
  (`G_PRINT…SVSyscall_Trace`); `architecture.md` records 329 identical
  `gameImport_t` ordinals between SDK and proxy `src/sdk/` (verified
  2026-09-09). The forwarder passes ordinals straight through.
- **SOURCE/STATIC** — engine loader expects the same two names:
  `codemp/unix/unix_main.c:417-418` `dlsym(handle,"dllEntry")` /
  `dlsym(handle,"vmMain")`, format strings `Sys_LoadDll(%s)…/found
  **vmMain**/failed dlsym/dlopen` all present verbatim in `linuxjampded`
  `strings` output.

### Confidence

`High`.

### Implications

Rust must keep the `jampgame_original.so` filename, `fs_game` fallback logic,
version gate string, and trap ordinals identical. Ordinal table comes from the
SDK (`original/jedi-academy-sdk/`), never from `codemp/game/` where ordinals
diverge.

### Remaining questions

U-004: `dlopen` path is relative (`base/jampgame_original.so`) — depends on
engine CWD/`fs_basepath` behavior; confirm with a runtime `strace`/log test.

---

## R-010 — Hard-coded engine addresses (absolute, version-locked)

### Finding

`Proxy_Engine_Wrappers.hpp` hard-codes ~80 engine function addresses, 10
variable addresses, 17 cvar-pointer addresses, 2 inline-patch addresses and 1
call-site address — all absolute `0x0804xxxx–0x813xxxx`. Every one falls inside
the engine's `LOAD` ranges; spot disassembly confirms valid function prologues
and the documented call/imm patch shapes.

### Evidence

- **SOURCE** — full list in
  `original/jampgame-proxy/src/jampgame_proxy/RuntimePatch/Engine/Proxy_Engine_Wrappers.hpp:20-173`
  (representative, see file for all):
  - hooks: `Com_Printf 0x8072ca4`, `SV_CalcPings 0x8057204`,
    `SV_SendMessageToClient 0x8058c84`, `SV_SendClientGameState 0x804cee4`,
    `SV_SvEntityForGentity 0x804ffb4`, `SV_ConnectionlessPacket 0x8056d64`,
    `SVC_Status 0x8056574`, `SVC_Info 0x8056784`, `SVC_RemoteCommand 0x8056b14`,
    `Cmd_TokenizeString 0x812c454`, `SV_UserinfoChanged 0x804e144`,
    `SV_ExecuteClientMessage 0x804e8c4`, `SV_PacketEvent 0x8057024`,
    `SV_BeginDownload_f 0x804d714`, `SV_DoneDownload_f 0x804eaa4`,
    `SV_WriteDownloadToClient 0x804d764`, `SV_AddEntitiesVisibleFromPoint
    0x08058404`, `SV_SendClientSnapshot 0x8058e04`, `SV_NextDownload_f
    0x0804d614`, `SV_StopDownload_f 0x0804d5b4`, `SV_UpdateUserinfo_f
    0x0804e314`, `Navigator_Load 0x08124ff4`;
  - call-site: `call_SV_AddEntToSnapshot_For_Players 0x0805882d`;
  - call-only: `SV_ClientEnterWorld 0x804d444`, `SV_ClientThink 0x804e634`,
    `SV_DropClient 0x804cb84`, `SV_Netchan_Transmit 0x8057db4`,
    `SV_FlushRedirect 0x8057b44`, `SV_UpdateServerCommandsToClient 0x80582c4`,
    `SV_GetChallenge 0x804b9a4`, `SV_DirectConnect 0x804c014`,
    `SV_Netchan_Process 0x8057e14`, `SV_SendServerCommand 0x8056214`,
    `SV_AddEntToSnapshot 0x080583b4`, `Com_DPrintf 0x8072ed4`, … through
    `SV_RateMsec 0x08058c04` (see file lines 70-129);
  - vars: `svs 0x83121e0`, `svsClients 0x83121ec`, `sv 0x8273ec0`,
    `rd_buffer 0x81e90c0`, `rd_buffersize 0x81e90c4`, `rd_flush 0x81e90c8`,
    `logfile 0x831f24c`, `cmd_argc 0x8260e20`, `cmd_argv 0x8260e40`,
    `cmd_tokenized 0x8264440`;
  - cvars (pointer-to-`cvar_t` slots, dereferenced once in
    `Proxy_Engine_Wrappers.cpp:18-28,68-73`): `sv_fps 0x8273e84`, `sv_gametype
    0x83121cc`, `sv_hostname 0x8273e9c`, `sv_mapname 0x8273e90`, `sv_maxclients
    0x8273ea4`, `sv_privateClients 0x8273e80`, `sv_pure 0x83121a8`, `sv_maxRate
    0x831219c`, `sv_rconPassword 0x83121d4`, `sv_floodProtect 0x8273e88`,
    `sv_allowDownload 0x83121b0`, `com_dedicated 0x831f254`, `com_sv_running
    0x831f300`, `com_cl_running 0x831f400`, `com_logfile 0x831f41c`,
    `com_developer 0x831e204`, `fs_gamedirvar 0x838aaec`;
  - inline patches: NOP `0x8056b26 len 25`, single-byte `0x8056c7d → 0x03`.
- **STATIC** — range check (scripted): every function address lies in engine
  `.text 0x0804a1e0–0x08199494`; every var/cvar address lies in RW `LOAD
  0x0819a4e0–0x08396bb4`.
- **STATIC** — spot `objdump -D -j .text` (note: plain `objdump -d` does NOT
  disassemble this binary's `.text` — it prints raw byte dumps; `-D -j .text`
  yields true per-instruction disassembly, verified against a control binary):
  - `0x8056b14` (SVC_RemoteCommand): `push %ebp; mov %esp,%ebp; sub $0xc520,%esp`.
  - `0x8072ca4` (Com_Printf), `0x8057204` (SV_CalcPings), `0x804cee4`
    (SV_SendClientGameState), `0x8124ff4` (Navigator_Load): all begin `push
    %ebp; mov %esp,%ebp; sub …` — whole-instruction prologues, compatible
    with the 5-byte `E9 rel32` detour (see R-012). Full-table proof is now
    automated: `tools/sweep_engine_addrs.py` (exit 0) checks all 83 detour
    addresses start with `push %ebp` and offer 6–9 stealable whole-instruction
    bytes — see U-005.
  - Call-site `0x0805882d`: `E8` with target `0x080583b4` =
    `func_SV_AddEntToSnapshot_addr` (checked by the sweep on every run).
    **STATIC** proof the call hook targets the claimed function.
  - NOP range `0x8056b26 len 25` tiles whole instructions EXACTLY (`call rel32`
    5B + `mov r32,m32` 6B + `add r32,imm32` 6B + `cmp r32,r32` 2B + `jb rel32`
    6B = 25B, verified by the sweep), so blind NOP-ing cannot split an
    instruction. The NOP'd `call` targets `Com_Milliseconds` (`0x80744e4`).
    Single-byte `0x8056c7d` holds `0xbf`, the second byte of imm32 `0xbff0`
    (49136) in `movl $0xbff0,0x4(%esp)` before `Com_BeginRedirect`; patching it
    to `0x03` yields `0x03f0` (1008). Shapes match the comments in the header.
- **SOURCE** — usage: `Proxy_Engine_Wrappers.cpp` casts each address to a typed
  function/var pointer in `Proxy_Engine_Initialize_MemoryLayer()` (called once
  at `GAME_INIT`); `Proxy_Engine_Patch.cpp` detours/inline-patches them.

### Confidence

`High` that addresses are inside valid segments and the two inline patches + one
call target decode as documented. `Medium` that every named address is the
function its name claims — the binary is stripped, so names come only from the
proxy source + engine-source correlation, not from symbols. Spot checks passed;
full per-address disassembly + engine-source line mapping is future work.

### Implications

Rust must treat this table as data version-locked to `JAmp: v1.0.1.1
linux-i386 Nov 10 2003`. Keep addresses in one generated module, gate startup
on the version cvar (already done), and add a test that fails if the engine
binary hash differs. Prefer `dladdr`-independent absolute reads for engine,
base-relative for game.

### Remaining questions

U-002 (runtime read-back of vars/cvars under GDB), U-005 (per-address
function-identity proof).

---

## R-011 — Hard-coded game offsets (base-relative) + proxy hook inventory

### Finding

Game offsets are file offsets (base `0`) rebased at runtime. All 14
`jampgame_func_*` offsets and the 1 variable offset match `nm -D` exactly —
except `Q_stricmp`, which is a source bug pointing at `Q_stricmpn`.

Hook inventory (SOURCE): 22 engine detours + 8 game detours + 1 call-site
retarget + 2 inline patches. Trap-level interceptions: `G_LOCATE_GAME_DATA`,
`G_GET_USERCMD` (+ `vmMain` cases `GAME_INIT/SHUTDOWN/RUN_FRAME/
CLIENT_CONNECT/DISCONNECT/BEGIN/COMMAND/USERINFO_CHANGED`).

### Evidence

- **SOURCE** — `Proxy_Engine_Wrappers.hpp:325-353` game offsets; **STATIC** —
  `nm -D jampgamei386.so`:
  - `Com_Error 0x868a4 ✓`, `Com_Printf 0x868e4 ✓`, `Com_sprintf 0x1a5524 ✓`,
    `Info_SetValueForKey 0x1a5b54 ✓`, `Info_ValueForKey 0x1a5604 ✓`, `Q_strcat
    0x1a53e4 ✓`, `Q_strncmp 0x1a5284 ✓` (`Q_strncmp__FPCcT1i` size 128),
    `Q_strncpyz 0x1a50f4 ✓`, `va 0x1a55b4 ✓`, `VectorNormalize 0x1a35f4 ✓`,
    `AngleVectors 0x1a3904 ✓`, `ConcatArgs 0x129c74 ✓`, `G_Damage 0x138554 ✓`
    (`G_Damage__FP9gentity_sN21PfT4iN26`), `BeginIntermission 0x87d14 ✓`,
    `player_die 0x133b44 ✓`, `G_AddEvent 0x16e564 ✓`, `ClientThink_real
    0x11c4c4 ✓`, `WP_SaberPositionUpdate 0x194c24 ✓`, `G_UpdateCvars 0x86034
    ✓`, `G_RegisterCvars 0x85f14 ✓`, `G_WeaponLogDamage 0x99b640 B ✓`.
  - **BUG (SOURCE+STATIC)**: `jampgame_func_Q_stricmp_addr = 0x001a5184` and
    `jampgame_func_Q_stricmpn_addr = 0x001a5184` are identical, but `nm` shows
    `Q_stricmpn__FPCcT1i at 0x1a5184 size 256` vs `Q_stricmp__FPCcT1 at
    0x1a5304 size 64`. `objdump` confirms `0x1a5284` starts `Q_strncmp` right
    after `Q_stricmpn`'s `ret`s. Callers of 2-arg `Q_stricmp` (e.g.
    `Proxy_sv_main.cpp:178-197` `Q_stricmp(c,"getstatus"/"getinfo"/…)` and
    `Proxy_sv_client.cpp:580`) therefore invoke the 3-arg `Q_stricmpn` with a
    garbage length — **INFERRED** impact (works by accident or corrupts
    comparisons; needs a differential test).
  - Unassigned (SOURCE): `jampgame.functions.Com_Printf` is declared but never
    assigned in `Proxy_Engine_Wrappers.cpp:124-136` (13 of 14 assigned); no
    caller was found (`grep` empty) — latent null pointer. `jampgame.variables
    .G_WeaponLogDamage` likewise never assigned and never read — dead.
- **SOURCE** — hooks: `Proxy_Engine_Patch.cpp:29-52` (`hookEntries[22]`),
  `:61-70` (`jampgameHookEntries[8]`), `:106/:144` (call retarget),
  `:159-162` (NOP + byte patch). `Proxy_OriginalAPI_Wrappers.cpp:24-51`
  (`G_LOCATE_GAME_DATA` → `Proxy_SharedAPI_LocateGameData`,
  `G_GET_USERCMD` → forward-then-`Proxy_SharedAPI_GetUsercmd`).
  `Proxy_Main.cpp:82-233` (`vmMain` switch).
- **STATIC** — game-offset range check: all 22 offsets lie in game `.text`
  `0x85564–0x1a66c4` or `.bss` (`0x99b640`), consistent with `ET_DYN` base-`0`.
- **SOURCE** — rebasing: `Proxy_Engine_Patch.cpp:98`
  (`originalFunctionAddr += proxy.jampgameAddress`) and
  `Proxy_Engine_Wrappers.cpp:124-136`
  (`(proxy.jampgameAddress + offset)`); detach subtracts back (`:139`).

### Confidence

`High` on the offset table and the `Q_stricmp` bug (both sides confirmed).
`High` on hook counts. Impact of the bug is `Low` confidence until tested.

### Implications

Rust must reproduce the offset table (with a `FIXME` + test for `Q_stricmp`:
use `0x1a5304`). Decide explicitly whether to preserve the bug for
compatibility or fix it — document and test either way. Keep unassigned fields
out (no `Com_Printf` shadow, no dead var) unless a test demands them. Preserve
the `GAME_*`/trap interception set exactly; `GAME_CLIENT_THINK` stays
pass-through (commented out in proxy).

### Remaining questions

U-001 (close out: hook list is now complete at source level; remaining is
per-hook signature/behavior audit), U-007 (`Q_stricmp` fix-vs-preserve).

---

## R-012 — Detour/patch mechanism and its ABI assumptions

### Finding

All detours are classic 32-bit `E9 rel32` overwrites with Zydis-computed
prologues (≥5 bytes), `mmap(RWX)` trampolines, and `mprotect` page juggling.
The design assumes i386, relative near jumps, and readable/writable/executable
transitions.

### Evidence

- **SOURCE** — `RuntimePatch/HookUtils/HookUtils.{hpp,cpp}`:
  - `GetLen`: Zydis `LEGACY_32/STACK_32`, accumulate whole instructions until
    `≥5` bytes.
  - `Attach`: `savedOpcodeLength=GetLen`, `trampoline=AllocateMemory`,
    `memcpy` stolen bytes + append `E9 <orig+len − (tramp+len+5)>`, `memset`
    orig with `0x90`, write `E9 <proxy − (orig+5)>` under `UnProtect/ReProtect`.
  - `Detach`: restore stolen bytes, `munmap`.
  - `AllocateMemory`: `mmap(RWX|PRIVATE|ANON, len+5)`; `InlinePatch` rewrites a
    `E8/E9` rel32 operand (`*(p+1) = new − (p+5)`); `Patch`/`Patch_NOP_Bytes`
    single/`memset` writes; `UnProtect/ReProtect`: two-page `mprotect`
    (`RWE` ↔ `RE`).
- **SOURCE** — build: `CMakeLists.txt:53-58` forces `-m32` for 64-bit hosts;
  `:111-112,130-134` add `-msse2 -mstackrealign -mfpmath=sse` for X86 (the
  documented "x86 vm will crash without -mstackrealign" quirk).

### Confidence

`High`.

### Implications

Rust must keep the same primitives: ≥5-byte whole-instruction steal (use a
decoder like `zydis`/`iced-x86`, not a fixed 5 — the sweep proves 6–9 for every
site), `E9 rel32` (engine, proxy, and trampoline all live in the same 32-bit
address space, so ±2 GiB reachability is guaranteed on i386), `mmap(RWX)` or
`mmap(RW)+mprotect(X)` trampolines, and page-granular `mprotect`. The kernel
half is **RUNTIME**-verified in the build container (32-bit `mmap(RWX)` +
`RWE↔RE` flips, including on own `.text`, all succeed — see R-016).
Thread-safety (no stop-the-world in proxy) and re-entrancy (trampoline calls
original) must still be tested end-to-end. This is inherently `unsafe`.

### Remaining questions

U-008 (confirm `mprotect` succeeds on engine `.text` pages and trampolines
execute on the target kernel; no 64-bit/`MAP_32BIT` work needed — i386-only).

---

## R-013 — Calling convention and type-layout assumptions

### Finding

Everything is i386 System V cdecl with 32-bit `int`/pointers, `float` passed
by `PASSFLOAT` bit-cast for traps, and engine/game structs shared verbatim from
`src/sdk/` (`server.h`, `qcommon.h`, `g_local.h`).

### Evidence

- **SOURCE** — `QDECL` empty on Linux (`q_shared.h:114`); `__cdecl` only on
  `WIN32` (`:127`). All engine/game function-pointer tables in
  `Proxy_Engine_Wrappers.hpp:179-307` are plain (non-`stdcall`/`fastcall`)
  pointers; variadics (`Com_Printf`, `SV_SendServerCommand`,
  `NET_OutOfBandPrint`, `Com_DPrintf`) are `QDECL`-variadic cdecl.
- **SOURCE** — hook prototypes mirror engine/game prototypes 1:1 (e.g.
  `Proxy_sv_client.hpp`: `SV_ExecuteClientMessage(client_t*,msg_t*)`;
  `Proxy_g_combat.hpp`: `G_Damage(gentity_t*,…,vec3_t,vec3_t,int,int,int)`;
  `Proxy_navigator.hpp`: see warning below).
- **SOURCE** — `Proxy_Header.hpp:48-141` (`Proxy_t`, `ClientData_t`,
  `LocatedGameData_t`, …) and `Proxy_Engine_Wrappers.hpp` structs are used
  interchangeably with SDK structs (`client_t*`, `msg_t*`, `netadr_t`
  by-value, `vec3_t` arrays) — layout compatibility is assumed, not converted.
- **SOURCE (warning, now STATIC-confirmed)** — `CNavigator::Load` is a C++
  non-static member `bool Load(const char*,int)`
  (`codemp/server/NPCNav/navigator.h:134`, `navigator.cpp:602`), but the proxy
  hooks it as free `void (*)(const char*,int)` (`Proxy_navigator.{hpp,cpp}`).
  True disassembly (`objdump -D -j .text`) proves the mismatch:
  - Callee `0x8124ff4` reads three stack words (`0x8(%ebp)→%esi`,
    `0xc(%ebp)→%eax`, `0x10(%ebp)→%ebx`) = hidden `this` + filename + checksum.
  - Its single caller (`call 0x8124ff4` at `0x8053387`, the only xref in
    `.text`) pushes three words: global `0x8379040` (the **INFERRED**
    `CNavigator` instance — also passed alone to the neighboring call at
    `0x805339d`, consistent with a `this`-only method), filename from struct+4,
    checksum from struct+8 — then consumes the return with `movzbl %al,%eax`.
  - So the proxy's 2-arg `void` hook misaligns every parameter and discards a
    live `bool` return; the original then runs with the caller's return address
    as `checksum`, which fails the nav-file checksum comparison, so
    `CNavigator::Load` effectively always returns false under the proxy
    (**INFERRED** impact — needs a live server to observe; the shape mismatch
    itself is confirmed fact).
  - Correct shape for Rust: `extern "C" fn(this: *mut c_void, filename:
    *const c_char, checksum: c_int) -> bool` (Itanium: `this` as first stack
    word on i386, `bool` in `AL`). See U-006.

### Confidence

`High` on cdecl/ILP32/layout-sharing — layouts are now **TESTED**, not just
reviewed: `g++ -m32` layout dumps of 15 boundary types + 9 representative fields
(`client_t`, `server_t`, `serverStatic_t`, `msg_t`, `netadr_t`,
`sharedEntity_t`, `playerState_t`, `entityState_t`, `usercmd_t`,
`level_locals_t`, `gentity_t`, `gclient_t`, `vmCvar_t`, `cvar_t`, `svEntity_t`)
are byte-identical between `original/jampgame-proxy/src/sdk/` and pristine
`original/jedi-academy-sdk/codemp/{game,server,qcommon}/` (the SDK `game/`,
`server/`, `qcommon/` trees are byte-identical to the dump inputs; the only
substitution, the missing `icarus/*.h` which the pristine SDK merely forward-
declares for pointer arrays, provably touches none of the dumped structs).
Binary cross-checks agree: `level` 45440 B == `sizeof(level_locals_t)`,
`g_entities` 0x17B000 == 1024×1516 (`sizeof(gentity_t)`), `g_clients` 0x38800
== 32×7232 (`sizeof(gclient_t)`). Note the headers only compile as C++ (the
proxy builds with `g++`); plain `gcc` (C mode) fails — relevant if generating
bindings. `High` on the `Navigator_Load` shape mismatch (caller + callee both
disassembled; impact remains **INFERRED**).

### Implications

Rust FFI must use `extern "C"` (cdecl on i386), `c_int/c_float/c_char`, raw
pointers, `#[repr(C)]` structs copied from the SDK (authoritative — now
layout-tested, see Confidence above), and `PASSFLOAT`-style `f32::to_bits` for
float traps. `Navigator_Load` must use the confirmed
`(this, filename, checksum) -> bool` shape — do not copy the proxy's 2-arg
`void` shape. Fixed-arity forwarding (13-word `vmMain`, 17-word syscall incl.
command) is **TESTED**: a 32-bit harness verified all words intact and `ESP`
balanced with no C variadics (see U-006).

### Remaining questions

U-006 (Navigator `thiscall`, variadic forwarding). `vmMain` arity is decided
(13 args) — see R-008.

---

## R-014 — Engine load sequence (why the proxy's init order works)

### Finding

The engine loads the proxy exactly like any game module: `SV_InitGameProgs →
VM_Create("jampgame", SV_GameSystemCalls, …) → Sys_LoadDll("jampgame", …) →
dlopen(<fs_basepath/fs_game/jampgamei386.so>, RTLD_NOW) → dlsym dllEntry/vmMain
→ dllEntry(syscalls)`. The proxy then chains a second load of the original.

### Evidence

- **SOURCE** — `codemp/server/sv_game.cpp:1753`
  (`VM_Create("jampgame", SV_GameSystemCalls, …)`),
  `codemp/qcommon/vm.cpp:471-518` (`VM_Create` → `Sys_LoadDll` → `VM_DllSyscall`
  path for `VMI_NATIVE`, used by PC/dedicated builds),
  `codemp/unix/unix_main.c:323-434` (`Sys_LoadDll`: `fname "%si386.so"`,
  `dlopen(fn, RTLD_NOW)`, `dlsym "dllEntry"/"vmMain"`, `dllEntry(systemcalls)`).
- **STATIC** — every `Sys_LoadDll` format string from that function appears in
  `linuxjampded`: `Sys_LoadDll(%s)...`, `succeeded ...`, `failed dlopen()
  completely!`, `failed dlsym(vmMain): "%s" !`, `failed dlcose: "%s"`,
  `found **vmMain** at  %p`, `succeeded!`, plus `jampgame` and `vmMain`
  literals.
- **SOURCE** — proxy chaining (`Proxy_Main.cpp:18-80,107-124`): engine calls
  proxy `dllEntry` at load (stores `originalSystemCall`); at `GAME_INIT` proxy
  `dlopen`s `jampgame_original.so`, `dlsym`s the four symbols, `dladdr`s the
  base, calls `originalDllEntry(Proxy_OriginalAPI_VM_DllSyscall)`, initializes
  the memory layer, attaches patches. `GAME_SHUTDOWN` reverses (detach,
  forward shutdown, `dlclose`).

### Confidence

`High`.

### Implications

Rust init/drop order must match: `dllEntry` only stores; heavy work happens at
`GAME_INIT` (after syscalls + cvars exist); `GAME_SHUTDOWN` must detach before
`dlclose`. The engine's `RTLD_NOW` + immediate `dllEntry(syscalls)` means Rust
`dllEntry` must not call back into syscalls that require game state yet.

### Remaining questions

U-004 (relative `dlopen` path resolution under various `fs_basepath/fs_game`
  values — runtime `strace` test).

---

## R-015 — Toolchain fingerprints

### Finding

`.comment` shows the shipped binaries were built with `GCC 2.95.3` plus
`Intel(R) C++ 5.0.1 Build 010730D0` (`-tp p6 -long_double …`), explaining the
mangled `__F*` symbols and Itanium-era EH helpers (`__ex_*`, `__eh_*`).

### Evidence

- **STATIC** — `objdump -s -j .comment` for both binaries: `GCC: (GNU) 2.95.3
  19991030 (prerelease)` + `Intel(R) C++ Compiler for 32-bit applications,
  Version 5.0.1 …`.
- **STATIC** — dynamic imports include Intel EH runtime (`__ex_alloc/get/free/
  caught/throw/rethrow`, `__eh_init_first/update_gdt…`) and `libcxa.so.1`.

### Confidence

`High`.

### Implications

Name mangling and EH personality differ from modern GCC/Clang — Rust must treat
all game symbols as opaque addresses (never demangle-depend), and must not link
Intel EH runtimes. Mangled `nm` names in this doc are for human correlation
only; the ABI is the address + prototype.

### Remaining questions

None.

---

## R-016 — Runtime environment observations (first RUNTIME evidence)

### Finding

Investigation graduated from static-only to limited runtime on 2026-09-09
(UTC). The engine starts in this container; the 2003 game `.so` does not
initialize under the modern host libc; the detour kernel primitives do work.
All runs used `original/` binaries unmodified (GNU_STACK workaround copies
lived in `/tmp` — see below).

### Evidence

- **RUNTIME** — engine starts: `linuxjampded` with no assets prints `JAmp:
  v1.0.1.1 linux-i386 Nov 10 2003`, runs `FS_Startup`, then fails closed with
  `Sys_Error: Couldn't load mpdefault.cfg` (0 pk3 files, restricted demo
  mode). Bonus confirmation: the startup banner is byte-identical to the
  proxy's `ORIGINAL_ENGINE_VERSION` gate string.
- **RUNTIME** — modern loader refuses the 2003 `.so`s: `dlopen` of the pristine
  `jampgamei386.so` fails with `cannot enable executable stack as shared object
  requires` (no `PT_GNU_STACK` + `TEXTREL` in 2003 binaries; container kernel
  refuses). Same for pristine `libcxa.so.1`. Workaround for *test copies only*:
  append an empty `PT_GNU_STACK` Phdr in a new table at end-of-file (all
  offsets/vaddrs/symbols byte-identical); never applied to `original/`. The
  shipped proxy would hit the same refusal in such containers — test
  harnesses (and possibly deployment notes) must account for it.
- **RUNTIME** — game `.so` INIT segfaults under modern glibc: after the
  workaround, `dlopen` reaches `calling init: jampgamei386.so` (`LD_DEBUG=libs`)
  then `SIGSEGV` with the fault PC in unmapped memory (GDB: `0x30e4956f`,
  no mapping) — i.e. 2003 INIT code jumps to la-la-land under glibc 2.x of
  2026. Consequence: no `GAME_INIT` (and no proxy load) is reachable in this
  container; engine-half var read-back and end-to-end hook tests need
  period-correct userspace (2003-era distro/VM or 32-bit chroot with glibc
  ~2.2/2.3).
- **RUNTIME** — detour kernel primitives PASS in-container (32-bit): `mmap
  RWX` anonymous + `RWE↔RE` flips + read-back, including the
  `UnProtect/ReProtect` pattern on own `.text` (`mprot_only`, exit 0).
- **RUNTIME** — forwarder logic PASS (32-bit): fixed-arity cdecl shims forward
  all 13 `vmMain` words (incl. `arg11 = 0x7EAD BEEF`, the word the 12-arg proxy
  drops) and all 17 syscall words with balanced `ESP` (`fwd_test`, exit 0).
- **RUNTIME (negative control)** — a naive 6-byte steal that splits `mov
  %eax,imm32` (and, separately, compiler-emitted PIC-thunk prologues in modern
  test binaries) segfaults deterministically. This is exactly the failure mode
  `HookUtils::GetLen` (whole-instruction steal) exists to prevent — do not
  hand-roll steal lengths in Rust; use a decoder.

- **RUNTIME (U-007 verdict, 32-bit harness calling into the real game code)** —
  `dlopen` is impossible (INIT segfault, above), so the harness `mmap`ed the
  text segment (vaddr == file offset) and called the two functions directly
  (both are reloc-free leaf string code): `Q_stricmpn` with `n == 0` returns
  **"equal" for every input** (including `getstatus` vs `getinfo`); with the
  large/negative stack garbage the proxy's cdecl call leaves in the 3rd slot
  it degenerates to a full case-insensitive compare — identical results to the
  real `Q_stricmp` on all 12 vectors (dispatch strings, pk3 names, case
  variants; zero sign divergences). Conclusion: the proxy's
  `Q_stricmp → 0x1a5184` misbinding is **benign-in-practice, fragile-by-luck**
  (an `n` of 0 or 1 on the stack would make every 2-arg compare return
  "equal"). Rust should bind `Q_stricmp → 0x1a5304` (behavior-preserving,
  removes the fragility).
- **RUNTIME (U-004 load-path mechanism, 32-bit `dlopen` harness)** —
  `dlopen("basedir/jampgame_original.so", RTLD_NOW)` resolves **relative to
  the process CWD**: NULL + `dlerror` "cannot open shared object file" when
  the path doesn't exist under CWD, success + valid `dladdr` base when it
  does. The proxy's `fs_game + "/jampgame_original.so"` is therefore
  CWD-relative, whereas the engine loads `jampgamei386.so` from absolute
  `fs_basepath/fs_game/` (`unix_main.c`) — if a deployment's CWD ≠ install
  root, the engine finds the proxy but the proxy cannot find the original.
  The engine does not `chdir` (it derives `fs_basepath` from `getcwd` at
  startup), so launching from the install root is required, as is typical.

### Confidence

`High` (directly observed, commands reproducible; test sources kept with the
agent working notes, candidates for `tests/`/`tools/` promotion).

### Implications

- Rust test harnesses that `dlopen` the game `.so` need the GNU_STACK-noted
  workaround or period userspace; document it with the test, not as a property
  of the shipped artifacts.
- Full runtime verification (U-002 engine half, U-005 call-stack sample,
  Navigator impact) is blocked on period userspace + game assets — see
  uncertainties.
- The `GetLen`-equivalent decoder is load-bearing, not cosmetic (negative
  control proves it).

---

## R-017 — Original engine + game module run end-to-end in an i386 Debian container

### Finding

The full original stack — `linuxjampded` + the 2003 `jampgamei386.so` — runs
end-to-end on modern userspace (i386 Debian bookworm, glibc 2.36): game-module
`dlopen` + `.so` INIT + `GAME_INIT` succeed, and the dedicated server reaches
steady state and answers protocol-26 `getinfo`/`getstatus`. The R-016
"execstack refusal / INIT segfault" findings were environment-specific: they
reproduce on the host userspace (glibc 2.43) but **not** under this container
(glibc 2.36), where everything loads with no GNU_STACK workaround and no
period-correct 2003 userspace. This unblocks U-002's engine-half and U-005's
live-call-stack work inside the container.

### Evidence

**RUNTIME** (container image `jampgame-investigation:i386-bookworm`, Debian
bookworm i386, glibc 2.36-9+deb12u14, `gcc 12.2`, kernel 7.0.0-31-generic —
see `docs/runtime-environment.md` for layout and commands):

- Engine starts; FS_Startup lists mounted assets; `+map mp/duel1` triggers:
  `Sys_LoadDll(/srv/jka/base/jampgamei386.so)...` →
  `Sys_LoadDll(jampgame) found **vmMain** at 0xE9B6D684` →
  `Sys_LoadDll(jampgame) succeeded!` → `Game Initialization` /
  `gamename: basejka` / `gamedate: Nov 10 2003` / `InitGame: \version\JAmp:
  v1.0.1.1 linux-i386 Nov 10 2003 ... mapname\mp/duel1 ...`.
- Server stays up; UDP `getchallenge` → `getinfo`/`getstatus` on port 29099
  returns `statusResponse` with `\protocol\26\mapname\mp/duel1\...`.
- **Runtime rebasing confirmation (U-002 half)**: `nm -D` gives `vmMain` file
  offset `0x85684`; `/proc/1/maps` shows the game module's first `r-xp`
  mapping at `0xe9ae8000`; base + `0x85684` = `0xe9b6d684`, byte-equal to the
  engine's printed `**vmMain**` pointer. First direct RUNTIME confirmation of
  the base-relative (`dladdr(dli_fbase)`) scheme in the real process.
- `/proc/1/maps` shows the game module data mapping as `rwxp` — the 2003
  non-`PT_GNU_STACK` `ET_EXEC` engine runs with READ_IMPLIES_EXEC semantics
  (executable stack + exec-on-readable), which is also why the module's text
  relocations apply cleanly.
- **glibc-version contrast**: same pristine `.so` dlopen'ed from a modern
  `gcc -m32` NX-stack main (PT_GNU_STACK RW) on the host (Ubuntu glibc 2.43)
  fails: `dlopen FAILED: ... cannot enable executable stack as shared object
  requires: Invalid argument`. The identical in-container harness (glibc 2.36)
  prints `dlopen OK`. The refusal is therefore glibc-version-dependent
  (2.43 vs 2.36), not a property of the 2003 `.so` alone, and is absent in the
  container. (No INIT crash was observed in-container at all.)

### Confidence

`High` — directly observed and reproducible: `tools/docker/run.sh`, `docker
logs`, `tools/q3_udp_query.py`, and the host/container dlopen harnesses.

### Implications

- U-002 (engine var/cvar slot read-back at `GAME_INIT`) and U-005 (live hook
  call stacks) are no longer blocked on "period-correct userspace": a GDB
  session at `GAME_INIT` is feasible in this container (needs
  `--cap-add SYS_PTRACE`, already in `tools/docker/run.sh`).
- The GNU_STACK workaround recorded in R-016 is not needed in this container;
  keep it as a documented test-only workaround for host/glibc ≥ 2.4x
  environments and for NX-stack main binaries, never for `original/`.
- The proxy's own `dlopen("base/jampgame_original.so")` runs in the same
  engine process, so it should also load here (prediction; to be confirmed
  when the proxy is built — see U-004/U-002).
- Container recipe that matters (see `docs/runtime-environment.md`): i386
  userspace; binaries COPYied from `original/jalinuxded_1.011`; `libcxa.so.1`
  installed to `/usr/lib`; assets mounted read-only and exposed via
  `+set fs_cdpath /srv/jka/gamedata`; `fs_basepath` stays `/srv/jka`
  (writable), engine CWD == `fs_basepath`, game module at
  `/srv/jka/base/jampgamei386.so`.

### Remaining questions

- Exact glibc version at which the execstack-less-DSO refusal appears (between
  2.36 and 2.43) — cosmetic for this project since the image pins bookworm's
  2.36; noted only so tests can be labeled correctly.
- The mounted GameData contains many custom pk3s alongside stock
  assets0..3.pk3; a stock-only asset set would give a cleaner differential
  base for later behavior tests (non-blocking).

---

## R-018 — Original proxy runs end-to-end in the container; U-002/U-005/U-008 closed with live evidence

### Finding

The original proxy, built unchanged from a `/tmp` copy (nothing under
`original/` modified) in the i386 container, runs end-to-end under the real
engine: `dlopen` of the original game module, version gate, memory layer, and
the full engine-patch attach all succeed, and the patched server keeps
answering protocol-26 queries. This yields the long-missing **live** evidence:
engine var/cvar slots read back through the proxy's own bookkeeping, engine
addresses firing on the correct network events, and the detour/trampoline/
`mprotect` machinery working on the real engine `.text` and the target
(container) kernel.

### Evidence

**RUNTIME** (container `jampgame-investigation:i386-bookworm`; engine
`linuxjampded` `JAmp: v1.0.1.1 linux-i386 Nov 10 2003`; proxy built with
cmake/g++12 from `original/jampgame-proxy` copied to `/tmp/opencode/proxybuild`,
Zydis `04793b1…` + Zycore `75a36c4…` — see `tools/runtime/README.md` for the
recipe and scripts):

1. **Proxy boot sequence** (`docker logs`, flushed via
   `gdb -ex 'call (int)fflush(0)'`): `----- Proxy: jampgame_proxy 0.0.7` /
   `Loading original game library jampgame_original.so` / `properly loaded` /
   `Original engine detected` (version gate passed) / `Initializing memory
   layer` / `Memory layer properly initialized` / `Patching engine` / `Engine
   properly patched`. `Sys_LoadDll` finds proxy `**vmMain**` at
   `base+0x2d20`, matching `nm` on the built `.so`.
2. **Engine `.text` patch read-back** (gdb `x/Nbx` on the running engine):
   - detours `e9 <rel32>` at `0x8072ca4` (Com_Printf), `0x8057204`
     (SV_CalcPings), `0x8056b14` (SVC_RemoteCommand), `0x8124ff4`
     (Navigator_Load); rel32 targets land in the proxy's loaded range;
   - the 25-byte NOP tile at `0x8056b26` is all `0x90` exactly;
   - single-byte patch `0x8056c7d` reads `0x03` (`0xbf`→`0x03`, 49136→1008);
   - call-site retarget `0x805882d` reads `e8 <rel32>` into the proxy range.
   The server answers `getchallenge`/`getinfo`/`getstatus` afterwards ⇒ the
   trampolines execute and `mprotect` page flips succeed on engine `.text`
   under the container kernel (closes U-008's target-kernel check for this
   environment).
3. **U-002 engine var/cvar slots, live** (gdb attach, PID 1):
   - all 17 cvar-pointer slots dereference to the *correct* `cvar_t`
     (`cvar_sv_fps_addr 0x8273e84 → cvar_t name "sv_fps" string "20"`; the
     slot the proxy names `sv_gametype`/`sv_rconPassword` dereferences to
     cvars actually named `g_gametype`/`rconPassword` — matching engine source
     `sv_init.cpp:998 Cvar_Get("rconPassword",…)`; `com_dedicated`→`dedicated`
     etc.); every `integer` matches the running configuration (dedicated 1,
     `sv_maxclients` 8, `g_gametype` 6, `mapname mp/duel1`, …);
   - var slots: `svs 0x83121e0` holds `serverStatic_t` with
     `initialized=1`, `time≈96,600` and rising, `clients` a live heap pointer,
     `numSnapshotEntities=16384` = `8 × 32 × 64` exactly as
     `sv_maxclients·PACKET_BACKUP·MAX_PACKET_ENTITIES`; `svs.clients
     0x83121ec` == `&svs.clients` (`0x83121e0+0xc`, matches `serverStatic_t`
     field offset); `sv 0x8273ec0` reads `state=2` (`SS_GAME`), `serverId=1`;
     `cmd_argc=2`, `cmd_argv[0]` points into `cmd_tokenized`;
   - `rd_*`/`logfile` are 0 at steady state (no active redirect / no logfile),
     and **live during an rcon redirect**: breaking at `SV_FlushRedirect
     (0x8057b44)` during a `rcon` packet shows `rd_buffer 0x81e90c0` pointing
     at the rcon stack buffer whose content is the redirected text, and
     `rd_buffersize=0xbff0` (49136) — exactly the `SV_OUTPUTBUF_LENGTH`
     immediate the proxy patches to 1008 (single byte `0x8056c7d`). Closes
     U-002: every address in `Proxy_Engine_Wrappers.hpp:135-173` is verified
     against live values; no slot is bogus.
4. **U-002 through the proxy's own state** (gdb on the *proxy-loaded* engine):
   `proxy.jampgameHandle≠0`; `proxy.jampgameAddress = 0xf282d000` == the
   `jampgame_original.so` `r-xp` base in `/proc/1/maps` (`dladdr dli_fbase`);
   `originalVmMain = base+0x85684`, `originalDllEntry = base+0x15e104`,
   `originalSystemCall = 0x08089324` (engine `.text`); `locatedGameData`
   after the `G_LOCATE_GAME_DATA` interception: `g_entities` =
   `base+0x6ce620`, `g_entitySize=1516`, `num_entities=61`,
   `g_clients = base+0x695d00`, `g_clientSize=7232` — every base+offset and
   size byte-matches `nm -D`/layout dumps; `server.svs = 0x83121e0` and
   `server.sv = 0x8273ec0` (pointer values equal the engine addresses), and
   reading through them gives live consistent `svs`/`sv` contents.
5. **U-005 live semantic identity** (engine under gdb, breakpoints at the
   table's addresses, driven by real UDP): `0x8056d64` fires on every
   connectionless packet (getchallenge/getinfo/getstatus/rcon), `0x804b9a4`
   only on `getchallenge`, `0x8056784` only on `getinfo`, `0x8056574` only on
   `getstatus`, `0x8056b14` only on `rcon`, `0x8057b44` on the rcon redirect
   flush. The four query-family names are therefore semantically confirmed
   (backtraces return into the same connectionless dispatcher region). With
   the proxy loaded, a breakpoint at the proxy's own
   `Proxy_SV_ConnectionlessPacket` hook (`base+0xbbb0`) hits on a live
   `getinfo` — first direct proof of a proxy hook executing on live traffic.
6. **Engine-source divergence confirmed relevant**: the codemp
   `sv_main.cpp` here is *not* the shipped binary's source for the
   connectionless family — its `SVC_Info` is the Xbox broadcast variant and
   the `getinfo` dispatch is commented out, yet the shipped binary answers
   `getinfo` with `infoResponse`. Reinforces AGENTS.md's "shipped binary
   wins": do not port per-hook behavior from `codemp/server/sv_main.cpp`
   verbatim for `SV_ConnectionlessPacket`/`SVC_Info`/`SVC_Status`.

### Confidence

`High` for every numbered item (directly observed; recipe and scripts
reproducible — see `tools/runtime/README.md`, `tools/runtime/*.gdb`).

### Implications

- The memory-layer address table (U-002) is **no longer blocking**: live
  read-back confirms every var and cvar slot; the Rust implementation can
  treat `Proxy_Engine_Wrappers.hpp:135-173` as verified data.
- U-005's semantic-identity remainder is no longer blocking for the
  connectionless/query/rcon family. Other hook addresses (downloads,
  snapshots, userinfo, Navigator) still lack per-address live confirmation;
  they ride along with future per-hook differential tests (U-001).
- U-008: `mprotect`/detour/trampoline operation on the real engine `.text` is
  confirmed on the container kernel/glibc; target-deployment kernel behavior
  remains an environment question only.
- The container stack (engine + original proxy + original game) is a working
  differential harness for U-001 per-hook behavior tests and for validating
  the Rust passthrough once it exists. The proxy's stdout is block-buffered on
  the log pipe; flush via gdb `fflush` or capture with `stdbuf`.
- `locatedGameData` and `proxy.*` fields give the Rust port concrete
  ground-truth values to assert in differential tests (e.g. sizes 1516/7232,
  offsets `0x6ce620`/`0x695d00`, `0x85684`/`0x15e104`).

### Remaining questions

- `CNavigator::Load` hook impact (U-006 **INFERRED**): the map used here
  (`mp/duel1`) never exercised the navigator, so the misfire's observable
  effect is still unobserved; needs a nav-using map/`NPC` scenario.
- Semantic identity of the non-query engine hook addresses (download/snapshot/
  userinfo/Navigator families) — per-hook differential tests (U-001).
- Whether the exact glibc 2.36↔2.43 execstack refusal boundary matters for
  test labeling only (R-017 remaining question).

## Methods and reproducibility

- `file <bin>`; `readelf -h/-l/-S/-d/-r/-V/--dyn-syms/-p .comment`;
  `nm -D <bin> | grep …`; `objdump -D -j .text/-s -j .comment
  --start-address/--stop-address` (**use `-D -j .text`: plain `-d` prints byte
  dumps, not disassembly, for the stripped `linuxjampded`**);
  `strings <bin> | grep …`;
  `grep -rn … original/jampgame-proxy/src original/jedi-academy/codemp`.
- `tools/sweep_engine_addrs.py` re-verifies every engine address mechanically
  (exit 0 = pass); rerun after any address-table change.
- Layout dumps: `g++ -m32` TUs including the SDK headers, printing
  `sizeof`/`alignof`/`offsetof`; proxy-`src/sdk` vs pristine-SDK outputs must
  `diff` clean (C++ mode required — the headers do not compile as C).
- Locale note: `readelf`/`objdump` localize section headers in this environment
  (French); rerun with `LC_ALL=C` for script-stable output. Counts above used
  `LC_ALL=C` for `nm`/`readelf -r`.
- Binaries are stripped: engine addresses have no symbol names (`objdump`
  prints `<$$eh_landing_pad…+offset>`). Game addresses do (`nm -D` names).
- Runtime notes: the original stack runs end-to-end in the i386 Debian
  container (`docs/runtime-environment.md`, R-017): `linuxjampded` loads the
  2003 `jampgamei386.so` (dlopen + INIT + GAME_INIT) under glibc 2.36 with no
  workarounds; and (R-018) the original proxy, built from a `/tmp` copy, runs
  the full boot sequence and attaches every engine patch on the real engine
  (see `tools/runtime/README.md` for the build recipe and gdb scripts).
  On the host (glibc 2.43) the same `.so` cannot be dlopen'ed
  from an NX-stack main (`cannot enable executable stack`); test harnesses
  running on such environments still need the R-016 GNU_STACK workaround.
  Deeper tools (Ghidra, further engine GDB sessions with assets) reserved
  for the remaining runtime items in uncertainties.

## Most important addresses/symbols (quick reference)

Proxy contract (import via `dlsym`, no rebasing of names):

- `vmMain @ 0x00085684 size 1632 T` + `dllEntry @ 0x0015e104 size 16 T`
  (game file offsets; runtime = base+offset).
- `level @ 0x0068a3a0 size 45440 B`, `g_entities @ 0x006ce620 size 0x17b000 B`.

Inline patches (engine absolute):

- NOP 25 bytes at `0x8056b26` (rcon timer block in `SVC_RemoteCommand`).
- Byte `0x8056c7d: bf→03` (`49136 → 1008` redirect buffer length).
- Call retarget at `0x0805882d` (`E8 fb82ffff` → `0x080583b4`
  `SV_AddEntToSnapshot`).

Hook-entry points (engine absolute, game base-relative — full tables in R-010/R-011;
do not copy from here, copy from `Proxy_Engine_Wrappers.hpp` + this doc's
evidence):

- Engine: `0x8072ca4 0x8057204 0x8058c84 0x804cee4 0x804ffb4 0x8056d64 0x8056574
  0x8056784 0x8056b14 0x812c454 0x804e144 0x804e8c4 0x8057024 0x804e3d4
  0x804d714 0x804eaa4 0x804d764 0x08058404 0x8058e04 0x0804d614 0x0804d5b4
  0x0804e314 0x08124ff4`.
- Game: `0x868a4 0x868e4 0x1a5524 0x1a5b54 0x1a5604 0x1a53e4 [0x1a5304 fix]
  0x1a5184 0x1a5284 0x1a50f4 0x1a55b4 0x1a35f4 0x1a3904 0x129c74 0x138554
  0x87d14 0x133b44 0x16e564 0x11c4c4 0x194c24 0x86034 0x85f14 (+var 0x99b640)`.

Gate: `JAmp: v1.0.1.1 linux-i386 Nov 10 2003` + `jampgame_original.so`.

---

## R-019 — Rust passthrough proxy loads and forwards end-to-end under the real engine

### Finding

The first Rust milestone — a pure passthrough skeleton (`rust/`): a 32-bit Rust
`cdylib` exporting only the two engine entry symbols, which `dlopen`s the
pristine module as `jampgame_original.so`, hands it its own syscall forwarder,
and forwards every `vmMain` word unchanged — loads and runs end-to-end under the
real 2003 engine. The dedicated server starts, the original module's `GAME_INIT`
executes through the proxy, and the server answers protocol-26 queries. This
closes the "Rust module can actually sit in this slot" question with live
evidence.

### Evidence

**RUNTIME** (container `jampgame-investigation:i386-bookworm`, engine
`linuxjampded` `JAmp: v1.0.1.1 linux-i386 Nov 10 2003`; proxy
`rust/jampgamei386.so`, an `ELF 32-bit i386` cdylib cross-built from x86_64
with the `rust:Dockerfile` dev image; rerun with `rust/scripts/engine-test.sh`):

1. **Engine loads the proxy**: `Sys_LoadDll(/srv/jka/base/jampgamei386.so)…`
   then `Sys_LoadDll(jampgame) found **vmMain** at 0xeef19fd0` — matches
   `nm -D` file offset `vmMain 0x3fd0` against the runtime base
   (`0xeef16000`, i.e. the loaded-range math used in R-017 holds for a Rust
   module too).
2. **Proxy `dllEntry` prints on load**: `----- proxy-rs 0.1.0: dllEntry, engine
   syscall registered` (stderr; unbuffered, no `fflush` dance needed).
3. **GAME_INIT path mirrors the original ordering** (R-014): `passthrough
   skeleton loaded` → `loading original game library jampgame_original.so
   ("base/jampgame_original.so")` — the CWD-relative `fs_game`/`base` path
   construction of `Proxy_Files.cpp` works from a Rust caller too — →
   `jampgame_original.so properly loaded`.
4. **Forwarding is live**: `------- Game Initialization -------` +
   `gamename: basejka` prove the pristine module received `GAME_INIT` through
   the proxy's 13-word `vmMain`; the engine-side `InitGame` print proves the
   game's traps reached the engine through the proxy's C variadic shim
   (`csrc/syscall_shim.c`) → Rust fixed-arity forwarder → engine syscall
   pointer.
5. **Server stays up and answers** `getchallenge`/`getinfo`/`getstatus` with
   `\protocol\26\mapname\mp/duel1` ⇒ the whole trap round-trip remains intact.

### Confidence

`High` for "the Rust passthrough skeleton runs in the game-module slot and
forwards engine↔game traffic unchanged". Scope note: this is the *skeleton*
only — no version gate, no `G_LOCATE_GAME_DATA`/`G_GET_USERCMD` interception,
no engine patches — so it does not yet reproduce the original proxy's
observe/mutate hooks (U-001).

### Remaining questions

- Export hygiene: the cdylib currently exports `jampgame_syscall_forward` in
  addition to `dllEntry`/`vmMain` (harmless — the engine dlsym()s by name);
  tightening to exactly two exports (R-004) is deferred.
- Dev/build workflow is dockerised (`rust/dev.sh`, `rust/Dockerfile`); the
  i686 artifact is produced by cross-compilation, so any Rust change that
  affects the *host* test build vs the i686 target build is covered only when
  the target build is re-run (scripts/build.sh does).

---

## R-020 — Rust proxy: bootstrap core + trap interceptions run end-to-end

### Finding

The second Rust milestone (`rust/`) — the version gate, original-module symbol
binding (`dllEntry`/`vmMain`/`level`/`g_entities` + `dladdr` base), the engine
memory layer (svs/sv + cvar-pointer slots, R-018-verified addresses), the proxy
cvar mirror, the `G_LOCATE_GAME_DATA`/`G_GET_USERCMD` trap interceptions, and
the client-event `vmMain` dispatch — loads and runs under the real 2003 engine,
and the patched-flow server answers protocol-26 queries.

### Evidence

**RUNTIME** (container `jampgame-investigation:i386-bookworm`, engine
`linuxjampded` `JAmp: v1.0.1.1 linux-i386 Nov 10 2003`; rerun with
`rust/scripts/engine-test.sh`):

1. Engine loads the proxy and the full init sequence runs:
   `bootstrap core loaded` → `loading original game library jampgame_original.so`
   → `jampgame_original.so properly loaded` → `Original engine detected` →
   `Initializing memory layer` → `Memory layer properly initialized` →
   `------- Game Initialization -------` / `gamename: basejka` /
   `gamedate: Nov 10 2003` → `InitGame: \...\version\JAmp: v1.0.1.1 ...`.
   The version gate passes on the pristine engine and the memory-layer cvar
   slot reads do not fault.
2. The pristine module's `GAME_INIT` traps flow through the proxy's C shim
   (its `G_LOCATE_GAME_DATA` is recorded, `G_GET_USERCMD` sanitised); the
   server stays up and answers `getchallenge`/`getinfo`/`getstatus` with
   `\protocol\26\mapname\mp/duel1\...`.
3. **Stack-alignment quirk reproduced and solved**: the 2003 engine enters the
   module with a 4-byte-aligned stack; the first build's Rust `vmMain` faulted
   with `SIGSEGV` at `movaps %xmm0,0x480(%esp)` (gdb backtrace) — the
   `-mstackrealign` quirk the original proxy documents
   (`CMakeLists.txt:130-134`). Solution (decision D-001): the i686 target
   disables SSE (`rust/.cargo/config.toml`) so no 16-byte-aligned instruction
   can be emitted, and the C shim is built with `-mstackrealign`.

### Confidence

`High` (directly observed; `rust/scripts/engine-test.sh` is the reproducible
harness, exit 0).

### Remaining questions

- The `netStatus`/`showNet` command handler (`Proxy_Engine_ClientCommand_NetStatus`)
  is not ported; while `proxy_sv_enableNetStatus` is non-zero those commands are
  forwarded to the game (documented divergence, `src/shared_api.rs`).
- The engine/game hook patch layer (detours, download/snapshot/userinfo/
  Navigator hooks) remains a later milestone; `Q_stricmp` is bound to the
  correct `0x1a5304` (U-007) when the hooks needing it land.


---

## R-021 — Rust proxy: D-002 hook layer runs end-to-end; two dropped detours verified no-op

### Finding

The D-002 keep set (rcon, q3infoboom, model length, downloads, anti-DDoS
ipAuthorize, `SV_SvEntityForGentity`, `CNavigator::Load`, game stats / anti-HP /
min-jump, netStatus) is ported as minimal-alteration detours / intra injections
and runs under the real engine. Two of the original proxy's whole-function
rewrites are provably no-ops against the shipped binary and were dropped; one
Rust-specific codegen hazard (unrelocated direct calls to absolute engine
addresses) was found and fixed.

### Evidence

**RUNTIME** (same container stack as R-017/R-018/R-020; `rust/scripts/engine-test.sh`
exit 0; bot-filled FFA and duel servers stay up and answer protocol-26 queries):

1. All 13 engine detours + 5 game detours attach, plus the intra patches and
   call-site retargets (`----- proxy-rs: Engine properly patched`). The two rcon
   inline patches, the `ipAuthorize` call NOP (`0x8056f64`), the RMG
   `je`→`jmp` (`0x804d131`) and the two call-site retargets (`vsprintf` →
   vsnprintf at `0x8072cca`, `SV_ClientThink` feed at `0x804e891`) apply
   cleanly.
2. UDP probes: `getchallenge`/`getinfo`/`getstatus` all answered; `rcon` with
   correct password executes (`map: mp/duel1`), wrong password → `Bad
   rconpassword.`, `writeconfig` non-`.cfg` blocked, cooldown
   (`proxy_sv_enableRconCmdCooldown`) blocks rcons inside the 500 ms
   `svs->time` window.
3. Bot-filled FFA runs for minutes: bots connect (`SV_UserinfoChanged`,
   `SV_SendClientGameState`), think (`ClientThink_real`, and the `SV_ClientThink`
   feed with `proxy_sv_enableNetStatus 1`), die (`player_die`), pick items up
   (`G_AddEvent`); an intermission (timelimit) triggers `BeginIntermission` and
   the full detach→attach cycle on `GAME_SHUTDOWN`/`GAME_INIT` map restart —
   server survives. `getstatus` shows the bots in game.
4. `sv` is a global *struct*: `server.sv = (server_t*)0x8273ec0` binds the
   **address** of the global, not the value at it (`0x8273ec0` holds the
   struct's first field, `state`). The Rust `sv_ptr()` initially dereferenced
   the slot and returned 2; fixed to return the global address.

**STATIC (binary diff, why two detours are dropped)**:

5. `SV_ExecuteClientMessage` (`0x804e8c4`) already contains both hack guards in
   the shipped binary: `mov %eax,0x20414(%edi); test %eax,%eax; jl 0x804e9c5`
   (`messageAcknowledge < 0` → early return) and
   `mov 0x20408(%edi),%edx; lea -0x80(%edx),%ecx; cmp %ecx,%eax; jl 0x804e9d2`
   (reliableAcknowledge range → clamp to `reliableSequence` + return). The
   original proxy's full rewrite copied these verbatim; its only real additions
   were the ping-fix (dropped, D-002) and the netStatus feed (now a call-site
   retarget). **The detour is dropped.**
6. `SV_PacketEvent`/`SV_ReadPackets` (`0x8057024`) already has both the
   connectionless dispatch (`msg->cursize >= 4 && *(int*)msg->data == -1` →
   `0x8057162`) and the translated-port qport fix (`movzwl %ax,%edx` on the
   qport, then the `svs.clients`/`qport` walk). The proxy's rewrite is verbatim
   → **dropped**.
7. `SV_SendClientGameState` (`0x804cee4`) already has the "[ISM]" fragment-flush
   loop (`client->state && client->netchan.unsentFragments`); the only real
   diffs vs the proxy rewrite are the `CS_CONNECTED→CS_PRIMED` guard
   (pristine sets `movl $0x3,(%esi)` unconditionally at `0x804cf54`) and the
   unconditional `MSG_WriteShort(&msg, 0)` (pristine branches on
   `TheRandomMissionManager` at `0x804d12a`). Ported as an entry wrapper + the
   RMG intra patch.

**STATIC/CODE (Rust codegen hazard, fixed)**:

8. A Rust direct `call <absolute-engine-address>` (e.g. `engine_fn!` transmuting
   `0x812c264` to a fn pointer) emits an **unrelocated** `rel32`: the .so has no
   `.rel.text` `R_386_PC32` entries, so the loader leaves the linker's
   link-time-base-relative displacement in place and the call lands at
   `base+site + 0x812c6e7`-style addresses under ASLR (crash PC computed exactly
   matches). Fixed by loading the address through a volatile `static` so the
   call is indirect (`call *%reg`). The engine's absolute addresses are correct
   as-is (non-PIE at 0x08048000); only the *encoding* of the call was wrong.
   Game-module calls (`jampgame.functions.*`) were never affected (they use the
   runtime `dladdr` base).

### Confidence

`High` for every numbered item (reproducible: `rust/scripts/engine-test.sh`,
the container UDP/rcon probes, `objdump` diffs above).

### Implications

- The D-002 keep set is ported and running; the remaining per-hook
  differential/observable tests (D-002 Tests section) still need the R-018
  container stack plus a real client for the client-facing commands
  (`netStatus`/`showNet`, downloads).
- `SV_PacketEvent` and `SV_ExecuteClientMessage` are documented as dropped
  no-ops against the shipped binary; the inventory (`docs/inventory.md`) and
  this doc are the record.
- Any future absolute engine-address call in Rust must go through the
  `engine_fn!` volatile-load pattern, never a direct constant transmute.

## R-022 — netStatus live diagnosis (A1/A1b); packet-identity fix; `with_state` stack bomb; tooling note

### Finding

The per-usercmd netStatus feed was **semantically exact but broken for real
clients** on two counts, and carried a latent stack bomb:

1. **Packet identity froze the `packets` column.** The Rust stub used
   `cl->messageAcknowledge` as the `cmdStats` packet identity; a real client's
   `messageAcknowledge` only advances when the *server* sends reliable
   commands (rare during gameplay), so `CalcPacketsAndFPS` counted ~1 packet
   forever and the table went stale between prints. Fixed (`netstatus.rs`):
   the packet boundary is reconstructed from the engine's per-packet fields —
   `deltaMessage` (`0x20908`, set per `SV_UserMove` call by the shipped
   binary, objdump-verified) and `lastPacketTime` (`0x20910`, set per accepted
   packet by `SV_ReadPackets`) — bumping a monotonic per-client counter
   (`state.rs`). Same one-identity-per-packet semantics as the original's
   pre-loop `cmdIndex` capture, exact except for same-frame bursts.
2. **`state::with_state` stack bomb:** the inlined
   `get_or_insert_with(ProxyState::default)` materialised
   `Box<[ClientEntry; MAX_CLIENTS]>: Default` — the whole ~278 KiB per-client
   array — **on the stack**, giving some call sites a ~278 KB stack-probing
   prologue (observed live in `client_command_net_status` and, in another
   build, in `sv_client_think_stub` — fatal on deep engine stacks). Fixed
   (`state.rs`): `clients: Box<[ClientEntry]>` built element-wise on the heap;
   `calc_packets_and_fps` no longer copies the 8 KiB `cmd_stats` array to the
   stack either.
3. The remaining live symptoms are faithful-to-the-original behavior: zero
   columns until `SV_CalcPings` gives `ping >= 1`, and the all-zero-slot
   window artifact (`fps = 1024`) while the client's `serverTime` is < 1000 ms
   after a map load (the original's identical code does the same).

### Evidence

**RUNTIME** (A1/A1b harness, `tools/runtime/a1_netstatus.sh` +
`a1_netstatus_semantic.py`: real engine + proxy in the i386 container under
gdb; a bot client forced to `ping = 20`; `proxy_sv_enableNetStatus 1`):

1. **A1 (semantics):** 40 direct `sv_client_think_stub` inferior calls
   (serverTime +50 ms/call, one packet per two calls via
   `lastPacketTime`/`deltaMessage` writes) → `client_command_net_status`
   prints `fps=21 packets=11`, matching the Python mirror of
   `UpdateUcmdStats`/`CalcPacketsAndFPS` over the exact injected sequence.
2. **A1b (in-vivo retarget):** a crafted `clc_move` message (serverId/acks/
   cmd stream written with the engine's own `MSG_WriteLong` `0x8077ad4`,
   `MSG_WriteByte` `0x8077a24`, `MSG_WriteBits` `0x8077634`; explicit
   serverTime branch, angles passthrough, `lastUsercmd` primed) run through
   the real `SV_ExecuteClientMessage` `0x804e8c4` reaches the Rust stub via
   the retargeted `call` at `0x804e891` (stub-entry breakpoint) and the table
   updates to `fps=41 packets=21` — the engine's own `deltaMessage` write
   bumped the packet counter, as designed.
3. The engine's reliable-command bookkeeping: `SV_SendServerCommand`
   increments `reliableSequence` **before** the `Q_strncpyz`
   (sv_main.cpp:130-149), so the newest command is at
   `[reliableSequence & (MAX_RELIABLE_COMMANDS-1)]`.
4. Engine offsets confirmed by disassembly: `ping` `0x39424`,
   `rate` `0x39428`, `snapshotMsec` `0x3942c` (SV_UserinfoChanged stores),
   `netchan.remoteAddress` `0x3943c` (fixed the stale Rust constant
   `OFFSET_NETCHAN_REMOTE_ADDRESS` 234548 → 234556), `deltaMessage`
   `0x20908`, `lastPacketTime` `0x20910`, `lastUsercmd` `0x20420`,
   `gamestateMessageNum` `0x20418`.

**TOOLING:** `objdump -d` cannot decode this binary's `.text` (the whole text
region is covered by a data-typed `$$eh_landing_pad…` symbol; objdump prints
raw contents, no mnemonics). The working method is **gdb batch disassembly**:
`disassemble /r start,end` (plain `disassemble start,end` returns empty).
R-021's "objdump-verified" addresses were raw-byte reads; the conclusions
stand, now backed by real disassembly.

### Confidence

`High` for 1-2 (reproducible harness), `High` for the tooling note.

### Implications

- The D-002 netStatus row is now live-verified end-to-end at the engine level
  (no UDP client needed yet); the remaining differential-vs-original-proxy
  comparison needs the A2 fake client or a real player session.
- Milestone (pending): port the three upstream `SV_ExecuteClientMessage` gate
  diffs as an entry wrapper (user decision 2026-09-10: yes) — it can also make
  the packet identity exact by marking the packet start per `clc_move`
  message, superseding the burst deviation.

### R-022 follow-up — milestone B port (`SV_ExecuteClientMessage` entry wrapper)

The three upstream gate diffs are ported as the 14th engine detour
(`hooks/engine_sv.rs::sv_execute_client_message`, wrapper over the pristine
body via a 6-byte-steal trampoline; the shipped prologue
`55 8b ec 83 ec 20` is decoder-fixture-pinned). The wrapper:

1. parses the header (serverId / messageAcknowledge / reliableAcknowledge +
   the command byte) with the engine's own `MSG_Bitstream`/`MSG_ReadLong`/
   `MSG_ReadByte` (0x80775d4/0x8077e74/0x8077df4) and restores the msg read
   state before the body runs,
2. applies the newer upstream gate: nextdl `strstr` tolerance (via a
   proxy-local scan), the map_restart range check, and the
   `state != CS_ACTIVE` resend guard (resend = the shipped
   `Com_DPrintf` 0x8072ed4 with the shipped `"%s : dropped gamestate,
   resending\n"` at 0x819b584 + pristine `SV_SendClientGameState`),
3. marks the netStatus packet identity once per `clc_move`/`clc_moveNoDelta`
   message (`netstatus::mark_packet_start` → per-client `packet_counter`),
   the exact per-`SV_UserMove` capture point of the original's pre-loop
   `cmdIndex` — superseding the interim per-packet-field heuristic.

**RUNTIME (A1/A1b, `tools/runtime/a1_netstatus.sh`)**: the crafted
`SV_ExecuteClientMessage` flow reaches the wrapper (detour attach logged,
steal 6), marks the packet (the table's packets column increments through the
real path: A1 direct-stub `fps=21 packets=0` with constant identity — the
original's `lastPacketIndex=0` initial quirk preserved — then the wrapper's
in-vivo message → `fps=41 packets=1`).

**HARNESS NOTES (diff flow):** the differential runs
(`tools/runtime/diff_netstatus.sh rust|original`) surfaced two facts:
(1) the original proxy divides by `cl->snapshotMsec` unguarded once
`ping >= 1` (SIGFPE for ping≥1/snapshotMsec=0 — the Rust printer guards
that branch); (2) the crafted-message flow does not exercise the *original's*
feed either (its detour early-returns on the crafted header), so crafted
inputs cannot fully emulate real-client traffic for either proxy — live
verification must use a real client or the A2 UDP driver.

### R-022 follow-up 2 — localhost ping-0 blind spot (deviation)

**Finding (user observation):** on a local server a real player's computed
ping is 0 (`SV_CalcPings`: `messageAcked - messageSent` within one frame
tick), so the original's `ping >= 1` gates — the per-usercmd feed
(`Proxy_SV_UserMove`: `if (client->ping < 1) continue;`) and the table's
stats branch (`Proxy_Engine_ClientCommand.cpp:56`) — excluded them: fps and
packets stayed 0. The gate is a bot discriminator (`SV_CalcPings` writes ping
0 for `gentity->r.svFlags & SVF_BOT`, binary-verified at `0x8057273-0x805727d`
— loads `cl->gentity` (0x20844), tests bit 3 of `0x238(%eax)`, writes 0), not
a deliberate localhost exclusion.

**Structural note:** bots never reach the per-usercmd feed at all — they send
no `clc_move`; their cmds arrive via the `BOTLIB_USER_COMMAND` syscall
(sv_game.cpp:990) whose `SV_ClientThink` call site is *not* retargeted. So in
the ported design the feed's ping gate could only ever exclude zero-ping real
players.

**Change (deliberate deviation):**
- `sv_client_think_stub`: the feed's ping gate dropped (the cvar gate stays).
- The printer excludes bots using the engine's own check
  (`gentity != 0 && r.svFlags & SVF_BOT`; new pins `OFFSET_SHARED_SVFLAGS =
  0x238`, `SVF_BOT = 8`) instead of `ping >= 1`; bot rows are skipped entirely
  (the original's `snapshotMsec = 1` hack is therefore gone); zero-ping real
  players get live fps/packets/timenudge.
- Division hardening: `1000 / sv_fps` in `UpdateTimenudge` (SIGFPE at
  sv_fps 0) and `1000 / cl->snapshotMsec` (the original's SIGFPE for
  ping≥1/snapshotMsec 0) are guarded.

**RUNTIME (A1/A1b, ping-0 path):** with the bot's natural ping 0 and
`SVF_BOT` cleared (simulating a real localhost player), the feed records at
ping 0 (timenudge computed) and the table shows `fps=21→41`, `packets=0→1`
(direct-stub constant identity → the wrapper's in-vivo bump) — the exact
localhost scenario. With `SVF_BOT` intact the bot row is excluded from the
table (bot behavior).

---

## R-023 — map change crash: non-idempotent RMG `je`→`jmp` intra patch

### Finding

Changing the map (`map <name>`) crashed the dedicated server because
`attach_all` runs once per `GAME_INIT` and the engine reloads the game module
on a map change, but the RMG `0F 8x`→`E9` intra patch at `0x804d131` is not
idempotent: the second pass re-converted its own output and rewrote the branch
into a wild jump.

### Evidence

**ENGINE SOURCE** (`original/jedi-academy/codemp/`, non-authoritative where
the binary diverges, but the flow was also observed live):

1. `SV_SpawnServer` (`sv_init.cpp:761`) → `SV_InitGameProgs` →
   `VM_Create("jampgame", …)`; on a normal map change
   `SV_ShutdownGameProgs` (`sv_game.cpp:1669-1676`) calls
   `VM_Call(GAME_SHUTDOWN)` then `VM_Free` → `Sys_UnloadDll` → `dlclose` of
   the proxy `.so`. `map_restart` (`sv_game.cpp:1711-1724`) uses `VM_Restart`,
   which for a native module is also `VM_Free` + `VM_Create`
   (`qcommon/vm.cpp:398-410`).

**RUNTIME (live i386 container, this repo's `jampgamei386.so`):**

2. Before a map change the engine holds `E9 6B 02 00 00 90` at `0x804d131`
   (pristine `0F 84 6A 02 00 00`, `je 0x804d3a1`, forced to an unconditional
   jump to the "old RMG" else path). After a map change — two
   `Engine properly patched` banners, i.e. a second `attach_all` — a live
   `gdb -p 1` `x/6xb 0x804d131` read `E9 03 00 00 90 90`, decoded by gdb as
   `jmp 0x9804d139` (unmapped): the first time any real client reached
   `SV_SendClientGameState` after the change (the outdated-gamestate resend,
   `sv_client.cpp:1861-1864`) the server would fault.
3. The arithmetic is exact: the second `jcc_to_jmp` read `bytes[2..6]` of the
   already-converted site (`02 00 00 90`) as a `0F 8x` rel32, added the
   opcode-length correction (+1) and wrote `E9 03 00 00 90 90`. The old
   `debug_assert!` in `jcc_to_jmp_bytes` was compiled out in the release
   build, so nothing caught it.

**Why the earlier R-021 "detach→attach cycle survives" observation did not
catch this:** bots are set `CS_ACTIVE` directly by `SV_SpawnServer`
(`sv_init.cpp:811-819`) and never queue a stale `serverId` message, so they
never enter `SV_SendClientGameState`'s resend path. Only real clients
(`state = CS_CONNECTED`, `sv_init.cpp:806-810`) do. R-021's bot-filled server
therefore never executed the corrupted branch.

### Fix

`patch::jcc_to_jmp` (and the pure `is_jcc_rel32`) now treat a site whose first
byte is not `0F 8x` as already patched and leave it untouched — idempotent
across the reload cycle, keeping the original proxy's "intra patches are never
reverted" design. Regression coverage:
`rust/scripts/map-change-test.sh` drives three map changes and asserts
`0x804d131` stays `E9 6B 02 00 00 90`; unit test
`patch::tests::jcc_to_jmp_is_idempotent_across_map_change` pins the byte math.

### Confidence

`High` (root cause proven by live byte read of the running engine; fix
re-verified live across three map changes and by `engine-test.sh`).
