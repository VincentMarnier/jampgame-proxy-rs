# Uncertainties

Note (2026-09-10): `original/jampgame-proxy/` was removed after the migration
completed. Citations of its paths refer to the upstream repo
(`git clone https://github.com/VincentMarnier/jampgame_proxy <dir>`, pinned
commit `687997412ea6e5ead93f5c7b25db552590a2eeb1`).

This file tracks questions whose answers have not yet been verified. Evidence
labels: **SOURCE** (source code), **STATIC** (binary/static analysis),
**RUNTIME** (observed during execution), **TESTED** (automated test),
**INFERRED** (hypothesis — do not treat as fact).

Severity labels: **blocking** (must be resolved before the affected
implementation work), **important** (should be resolved before porting the
affected hooks / before release), **non-blocking** (decided or safely
deferrable to a ride-along test).

---

## U-001 — Complete proxy surface

### Question

Which functions and symbols does the original proxy actually intercept or replace?

### Classification

**important** — the interception inventory itself is SOURCE-complete (hook
tables enumerated, R-011); what remains is per-hook semantics
(observe/mutate/block), needed before porting each hook body but not before
building the passthrough skeleton (`vmMain`/`dllEntry` + syscall forwarder) or
the hook machinery.

### Current status

Mostly answered at source level; per-hook behavior audit remains.

**Update (RUNTIME, R-018)**: the original proxy now runs end-to-end in the
container under the real engine with all hooks attached, giving a working
differential harness for the per-hook audit, and the connectionless/query/rcon
family is semantically confirmed live (U-005). One caution discovered: the
codemp `sv_main.cpp` is *not* the shipped binary's source for the
connectionless family (`SVC_Info` Xbox variant, `getinfo` commented out), so
that family's per-hook semantics must be read from the shipped binary / proxy
behavior, not from that source verbatim.

**SOURCE**: 22 engine detours + 8 game detours + 1 call-site retarget + 2
inline patches + 2 trap interceptions (`G_LOCATE_GAME_DATA`, `G_GET_USERCMD`)
+ `vmMain` cases (`GAME_INIT/SHUTDOWN/RUN_FRAME/CLIENT_CONNECT/DISCONNECT/
BEGIN/COMMAND/USERINFO_CHANGED`) — see `docs/reverse-engineering.md` R-011 and
`original/jampgame-proxy/src/jampgame_proxy/RuntimePatch/Engine/Proxy_Engine_Patch.cpp:29-70,106`.
`GAME_CLIENT_THINK` is deliberately pass-through (commented out in
`Proxy_Main.cpp:208-219`). `SV_ExecuteClientCommand (0x804e3d4)` is call-only,
not hooked.

### Cheapest experiment

Source-only audit — no binary tools needed (cheapest level: source search).
Walk the two hook tables in `Proxy_Engine_Patch.cpp` against each `Proxy_*`
body in `Patches/{game,qcommon,server}/*.cpp` and tag every hook
observe-only / mutates-args / blocks-or-redirects; record the tags in
`inventory.md`. Concrete start: `grep -rn "Proxy_"` over the `Patches/`
headers plus reading the ~30 small `.cpp` bodies. Per-hook differential
runtime tests come afterwards (see below).

### How to investigate

- Audit each hook body in `Patches/{game,qcommon,server}/*.cpp` against SDK/engine
  behavior; record which hooks only observe, which mutate, which block.
- Add differential tests (proxy vs original) per hook before porting.

---

## U-002 — Runtime addresses

### Question

Which addresses are required at runtime, and which are merely static ELF symbol values?

### Classification

**was blocking — now resolved (RUNTIME, R-018).** Every engine var slot and all
17 cvar-pointer slots in `Proxy_Engine_Wrappers.hpp:135-173` were read back
live from a running `linuxjampded` (steady state and during an rcon redirect),
and again through the original proxy's own initialized memory layer; all match
source-layout expectations and the running configuration. Nothing is bogus;
the Rust memory layer can treat the table as verified data.

### Current status

**Resolved (RUNTIME, R-018).** Two live read-backs done in the container
(scripts: `tools/runtime/u002_read.gdb`, `tools/runtime/u002_proxy_read.gdb`):

- Engine var slots: `svs 0x83121e0` = live `serverStatic_t`
  (`initialized=1`, `time` rising, `numSnapshotEntities=16384` =
  `8·32·64` exactly); `svs.clients 0x83121ec` == `&svs.clients`
  (`+0x0c`, field offset matches engine `server.h`); `sv 0x8273ec0` reads
  `state=2` (`SS_GAME`); `cmd_argc/argv` live with `argv[0]` pointing into
  `cmd_tokenized`; `rd_*`/`logfile` are 0 at steady state and **live during an
  rcon redirect** — at `SV_FlushRedirect`, `rd_buffer` points at the redirect
  stack buffer (content readable) and `rd_buffersize=0xbff0` (49136), exactly
  the `SV_OUTPUTBUF_LENGTH` immediate the proxy patches to 1008.
- All 17 cvar-pointer slots dereference to the correct `cvar_t` (e.g. slot
  `0x8273e84` → `name="sv_fps", string="20", integer=20`; the slot the proxy
  calls `sv_gametype` dereferences to cvar `g_gametype`; `sv_rconPassword`
  → `rconPassword`, matching `sv_init.cpp:998`).
- Proxy-internal state (original proxy built & loaded, R-018):
  `proxy.jampgameAddress == 0xf282d000 == /proc/1/maps` base of
  `jampgame_original.so`; `originalVmMain = base+0x85684`,
  `originalDllEntry = base+0x15e104`, `originalSystemCall = 0x08089324`;
  `locatedGameData.{g_entities,g_clients}` = `base+{0x6ce620,0x695d00}` with
  sizes 1516/7232 — all matching `nm`/layout dumps; `server.svs=0x83121e0`,
  `server.sv=0x8273ec0`.

### Cheapest experiment

**Done** — `tools/runtime/u002_read.gdb` (attach to the running engine,
`gdb -q -batch -p 1`) and `tools/runtime/u002_proxy_read.gdb` (same, proxy
loaded). No new investigation needed for the slot table.

### How to investigate

- ~~Run `linuxjampded` + proxy under GDB…~~ — done (R-018).
- ~~Compare static `nm`/`readelf` values …~~ — done, byte-consistent.

---

## U-003 — ABI requirements

### Question

Which functions and structures require exact C ABI compatibility?

### Classification

**was blocking — now resolved (TESTED), non-blocking.** Layout dumps of 15
boundary types + 9 fields are byte-identical between proxy `src/sdk/` and
pristine SDK, with binary cross-checks (`level` 45440 B, `g_entities`
1024×1516, `g_clients` 32×7232). Struct audit is closed; only the Navigator
shape (U-006, also now decided) remains referenced here.

### Current status

Partially answered. **SOURCE/STATIC**: i386 System V cdecl throughout (`QDECL`
empty on Linux), ILP32 (`int`/pointer 32-bit), `#[repr(C)]` layouts shared
verbatim from `src/sdk/`, variadic cdecl for `Com_Printf`/`SV_SendServerCommand`/
`NET_OutOfBandPrint`. Project scope is i386-only (no x86-64 port).

1. `vmMain` arity — **DECIDED**: 13 `int`s (`command+arg0..arg11`) per the
   original game (SDK `g_main.c:503`; maintainer directive to trust the game).
   The proxy's 12-`int` declaration/forwarding (`Proxy_Main.hpp:12`,
   `vmMainFuncPtr_t`) drops `arg11` and is a proxy deviation, not the spec.
   Rust must implement and forward all 13 args.
2. `CNavigator::Load` hooked as free `void(const char*,int)` while source is
   member `bool CNavigator::Load(const char*,int)` (hidden `this`);
   disassembly at `0x8124ff4` reads 3 stack slots. Mismatch is **INFERRED**,
   not yet proven at call sites.

### Cheapest experiment

Done — `g++ -m32` layout dumps (15 types, 9 fields) diff clean between proxy
`src/sdk/` and pristine SDK; binary sizes cross-check (`level` == `sizeof
level_locals_t`, `g_entities` == 1024 × `sizeof gentity_t`, `g_clients` ==
32 × `sizeof gclient_t`). Promote the dump pair to `tests/` as a layout
regression gate when the Rust structs exist (same dump, SDK vs Rust
`#[repr(C)]`).

### How to investigate

- For (1): no further arity investigation. Add a regression test asserting the
  Rust `vmMain`/`dllEntry` signatures carry 13 `int`s and forward `arg11`
  unchanged to the original. (Forwarder words/ESP already TESTED — U-006.)
- For (2): resolved via U-006 (callee+caller disassembly; correct shape is
  `(this, filename, checksum) -> bool`).
- Struct audit: done (see Cheapest experiment); keep the dump as a
  regression test against the Rust port.

---

## U-004 — Original library load path at runtime

### Question

How does `dlopen("base/jampgame_original.so", RTLD_NOW)` resolve at runtime
(CWD vs `fs_basepath`/`fs_game`), and what happens with non-default `fs_game`?

### Classification

**was important — mechanism now resolved (RUNTIME), non-blocking.** The
`dlopen` path resolves **relative to the process CWD**; absent file fails
closed with `dlerror` (proxy exits fatally — no silent corruption). Remaining
(ride-along, real-deployment check): confirm the server's CWD convention
matches `fs_basepath` in real deployments, since the engine loads
`jampgamei386.so` from absolute `fs_basepath/fs_game/` while the proxy's
relative path would miss if CWD ≠ install root.

### Current status

**RUNTIME** (32-bit harness, no game/engine needed): `dlopen("basedir/
jampgame_original.so", RTLD_NOW)` returns NULL + `dlerror` "cannot open shared
object file" when run from a CWD without that path, and succeeds (with
`dladdr` returning the load base) when the file exists under the CWD. So the
proxy's `fs_game + "/jampgame_original.so"` is a **CWD-relative** path —
unlike the engine's own absolute `fs_basepath/fs_game/jampgamei386.so`
construction (SOURCE, `unix_main.c`). Practical requirement: launch the
server from the install root (typical for dedicated setups; the engine's
`FS_Startup` printout shows it derives `fs_basepath` from `getcwd` at startup
without chdir). The full-stack run in the i386 container (R-017) is already
set up this way (`fs_basepath` == CWD == `/srv/jka`, game module under
`base/`). **Update (RUNTIME, R-018)**: the full original-proxy run in the same
container confirms the end-to-end case — engine loaded the proxy from
`/srv/jka/base/jampgamei386.so`, the proxy's CWD-relative
`dlopen("base/jampgame_original.so")` succeeded (`----- Proxy: … properly
loaded`). Remaining (ride-along, real-deployment check): confirm a real
deployment's CWD matches its `fs_basepath`.

### Cheapest experiment

Done — 32-bit `dlopen` harness run from two CWDs (without/with
`basedir/jampgame_original.so`); resolution and failure modes as above. The
CWD-vs-`fs_basepath` divergence check folds into the first full-stack GDB
session in the i386 container (U-002, R-017).

### How to investigate

- ~~`strace` a `GAME_INIT`~~ — mechanism answered by the harness; the
  remaining strace on period userspace is confirmatory only.

---

## U-005 — Per-address engine function identity

### Question

Is every hard-coded engine address really the function its name claims?

### Classification

**was blocking — detour-safety half resolved (TESTED, mechanical); name-identity
half downgraded to important.** `tools/sweep_engine_addrs.py` (exit 0) proves
every one of the 83 detour targets starts on a whole-instruction boundary with
6–9 stealable bytes, and re-proves the call-site target, NOP tiling (exactly 25
B over 5 whole instructions) and the byte-patch value on every run — attaching
an `E9` detour can no longer land mid-instruction or on data. What the sweep
cannot prove is that each address is *semantically* the function its name
claims (binary is stripped); that remains a per-address source-correlation
task (string xrefs / call graphs), non-blocking for the hook machinery since
the proxy's field-proven table + version gate carry the semantic mapping.

**Update (RUNTIME, R-018): the connectionless/query/rcon family is now
semantically confirmed live** — see Current status. Remaining unconfirmed
addresses are the download/snapshot/userinfo/Navigator families (U-001 per-hook
tests).

### Current status

Mechanically verified (**STATIC**, reproducible): all 83 detour addresses
`push %ebp`-prologued with 6–9 stealable bytes; NOP range tiles 5 whole
instructions exactly (`call Com_Milliseconds` + `mov r32,m32` + `add r32,imm32`
+ `cmp` + `jb` = 25 B); byte at `0x8056c7d` is `0xbf` (imm32 `0xbff0`); call at
`0x805882d` targets `0x80583b4`. Spot semantic anchors (R-010: string xrefs at
`Com_Printf`/`SV_CalcPings`/`Navigator_Load` etc.) unchanged.

**Live semantic identity (RUNTIME, R-018)**: under real UDP traffic in the
container (scripts `tools/runtime/u005_semantic.{gdb,drive.py}`),
`0x8056d64` fires on every connectionless packet, `0x804b9a4` only on
`getchallenge`, `0x8056784` only on `getinfo`, `0x8056574` only on
`getstatus`, `0x8056b14` only on `rcon`, `0x8057b44` on the rcon redirect
flush. Also: with the original proxy loaded, the proxy's own
`Proxy_SV_ConnectionlessPacket` hook (base+0xbbb0) fires on a live `getinfo`
(`tools/runtime/u005_hook_call.gdb`) — live hook execution proven. Note the
codemp `sv_main.cpp` is *not* this binary's source for the connectionless
family (its `SVC_Info` is the Xbox variant and `getinfo` is commented out) —
do not use it verbatim for these hooks.

### Cheapest experiment

- Static sweep: **done** — `python3 tools/sweep_engine_addrs.py`.
- Live family checks: **done** — see Current status (R-018).

### How to investigate

- ~~Cheapest first: objdump sweep~~ — done, promoted to `tools/`, run it after
  any address-table change.
- Remaining per-address semantic confirmation is now folded into the U-001
  per-hook differential tests (download/snapshot/userinfo/Navigator families),
  using the working container stack from R-018 as the harness.

---

## U-006 — Tricky ABI shims (member function + variadics)

### Question

How must `CNavigator::Load` (C++ member, `bool` return) and the `QDECL`
variadic forwarders (`VM_DllSyscall` 16 args, `VM_Call` 11 args) be implemented
in Rust, where `extern "C" ...` variadics are unstable?

### Classification

**was blocking — now resolved (both halves).**

1. `CNavigator::Load` — **STATIC-confirmed**: true disassembly of the callee
   (`0x8124ff4` reads `0x8/0xc/0x10(%ebp)`) and its only caller
   (`call 0x8124ff4` at `0x8053387`, pushes global `0x8379040` + struct fields
   +4/+8, then `movzbl %al,%eax` consumes the `bool`) fix the shape:
   Itanium-i386 `(this, const char*, int) -> bool` with `this` as the first
   stack word. The proxy's 2-arg `void` hook is a real shape mismatch.
   Correct Rust shape: `extern "C" fn(this: *mut c_void, filename: *const
   c_char, checksum: c_int) -> bool`.
2. Variadic forwarders — **TESTED** (32-bit harness): fixed-arity cdecl shims
   forward all 13 `vmMain` words (incl. the dropped-by-proxy `arg11`) and all
   17 syscall words with balanced `ESP`, no C variadics needed.

### Current status

Resolved as above (R-013/R-016 record the evidence).

**Update (RUNTIME, R-018)**: with the original proxy loaded in the container,
`0x8124ff4` (`Navigator_Load`) carries the proxy's `E9` detour — the broken
2-arg `void` hook is live on the running engine. Its observable impact stays
**INFERRED**: `mp/duel1` never invoked the navigator in these runs, so the
misfire's effect (misaligned args ⇒ checksum failure ⇒ `Load` returns false)
was not observed. A nav-using map / NPC scenario in the R-018 harness would
settle it.

### Cheapest experiment

Done — `objdump -D` caller/callee pair (a) and the fixed-arity forwarder unit
test (b), both without an engine run or GDB.

### How to investigate

- ~~Disassemble `CNavigator::Load` callers~~ — done; `this` is the first stack
  word, `bool` return consumed. Impact of the original's broken hook (nav
  checksum failing ⇒ `Load` returns false) stays **INFERRED** until a live
  server test.
- ~~Prototype the forwarders~~ — done; replicate the fixed-arity pattern in
  Rust (a tiny C/asm trampoline only if `extern "C"` fixed-arity proves
  insufficient).

---

## U-007 — `Q_stricmp` misbinding: preserve or fix?

### Question

Proxy binds both `Q_stricmp` and `Q_stricmpn` to `0x1a5184` (`Q_stricmpn`),
while the binary has the real 2-arg `Q_stricmp at 0x1a5304` (**SOURCE+STATIC**
confirmed). Callers include connectionless-packet dispatch
(`Proxy_sv_main.cpp:178-197`) and download validation
(`Proxy_sv_client.cpp:580`). Is the observable behavior of calling 3-arg
`Q_stricmpn` with 2 args (garbage length) load-bearing, or a latent bug?

### Classification

**was important — now resolved (RUNTIME), non-blocking.** Verdict: the
misbinding is benign-in-practice but fragile-by-luck; fix the binding in Rust
(use `0x1a5304`), which is behavior-preserving and removes the fragility.
Record as a decision when implementing.

### Current status

**RUNTIME** (32-bit harness `mmap`ing the game `.so` — `dlopen` impossible on
the host/NX-stack main of the then-current environment, INIT segfault there;
both functions are reloc-free leaf string code, so direct calls are valid.
Note: in the i386 container `dlopen` works (R-017), so a future re-run could
drop the `mmap` trick):

- Sanity anchors pass: `Q_stricmpn("ABC","abc",3)==0`, real `Q_stricmp`
  case-folds, `Q_strncmp` is case-sensitive.
- Semantics discovered: `Q_stricmpn` with `n == 0` returns **0 (equal) for
  every vector** — including mismatched ones (`getstatus` vs `getinfo` ⇒
  "equal"). With a large or negative `n` (e.g. stack garbage `0xdeadbeef`) it
  degenerates to a full case-insensitive compare — which is *exactly* what the
  real `Q_stricmp` computes (`jedi_/red` vs `jedi_/blue`: real 1, garbage-n 1,
  but `n=4` would wrongly give 0).
- On the 12-vector harness (dispatch strings, pk3 names, case variants) the
  proxy's garbage-`n` behavior produced **zero sign divergences** from correct
  `Q_stricmp`.

So: nothing in the proxy depends on the misbinding (it merely *happens* to
behave like a full compare because caller-stack garbage is nonzero/large); the
danger is theoretical — if the 3rd-arg stack slot ever holds `0` or `1`, every
2-arg `Q_stricmp` call returns "equal" (n=0) or compares 1 char (n=1).

### Cheapest experiment

Done — see Current status. Harness compares real `Q_stricmp`, and
`Q_stricmpn` with `n ∈ {0, 4, 2^20, stack-garbage}` over dispatch/pk3 vectors.

### How to investigate

- ~~Differential test~~ — done; evidence above.
- Decision for Rust (record in `docs/decisions/` when implementing): bind
  `Q_stricmp → 0x1a5304` (correct), keep `Q_stricmpn → 0x1a5184`. This
  preserves all observed behavior and eliminates the n=0/n=1 fragility.

---

## U-008 — Trampoline reachability and page protection on modern kernels

### Question

Will 5-byte `E9 rel32` detours + `mmap(RWX)` trampolines + `mprotect` page
flips keep working under modern kernels (ASLR, `mmap_min_addr`, W^X policies)?

### Classification

**non-blocking — now container-verified end-to-end on the real engine (RUNTIME,
R-018).** i386-only scope DECIDED the reachability half (±2 GiB is guaranteed
inside one 32-bit address space, no `MAP_32BIT`/64-bit work). The kernel-policy
half is **RUNTIME-verified in the container** (R-016, R-018): `mmap(RWX|ANON)`,
`RWE↔RE` flips, the `UnProtect/ReProtect` pattern on own `.text`, **and the
full hook smoke test on the engine's real `.text`** all succeed. Only the
target-deployment kernel check remains (environment question, not design).

### Current status

Scoped to i386-only (maintainer directive): engine, game, and proxy all live in
one 32-bit address space, so `E9 rel32` ±2 GiB reachability is guaranteed and no
`MAP_32BIT`/64-bit handling is required (**DECIDED**). **RUNTIME** (container,
32-bit): anonymous `RWX` mmap succeeds, `RWX→RX→RWX` flips succeed, and
`mprotect(RWE→RE)` on a mapped `.text` page succeeds (`mprot_only`, exit 0).
A negative control (naive 6-byte steal splitting a `mov imm32` or a PIC-thunk
prologue) segfaults deterministically — reconfirming that Rust must steal
whole instructions via a decoder (`HookUtils::GetLen` equivalent), never a
fixed length.

**Full hook smoke test (RUNTIME, R-018):** the original proxy (built from a
`/tmp` copy) attached all engine patches on the real `linuxjampded` under the
container kernel — engine `.text` read-back shows the `E9` detours at
`0x8072ca4`/`0x8057204`/`0x8056b14`/`0x8124ff4`, the 25-byte NOP tile at
`0x8056b26`, the byte patch `0x8056c7d=0x03`, and the call retarget at
`0x805882d` — and the server kept answering protocol-26 queries, i.e. the
trampolines execute and `mprotect` on engine `.text` pages succeeded
(`----- Proxy: Engine properly patched`).

### How to investigate

- ~~Runtime test … attach all hooks, confirm `mprotect` … trampolines
  execute~~ — done in the container (R-018; scripts in `tools/runtime/`).
- The target deployment kernel (non-container host) check remains; re-run the
  R-018 smoke test there when a deployment target is chosen.
- If W^X blocks `RWX`, switch to `RW→RX` flip and re-test; document in
  `docs/decisions/`.
