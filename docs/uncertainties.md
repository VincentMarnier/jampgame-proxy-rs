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

---

## U-009 — "Lag spikes on death / while watching scores" (parallel TFFA)

### Question

Players on a parallel-TFFA server (several matches) report a momentary spike
"on death and sometimes when watching scores", felt only by the player
concerned. Is the proxy causing it, and which half?

### Classification

**important — one half fixed and unit-pinned, the other half not reproduced.**

**SOURCE / STATIC (scoreboard request).** Every `scores ...` message triggered
`refresh_cs_players_for_scores`, which re-sent one full `CS_PLAYERS` info
string (~150-250 bytes) **per playing outsider** on every request. A viewer
with ~25 outsiders in other matches therefore received ~5 KB of reliable
commands plus the ~1 KB `scores` message in one frame; reliable commands are
queued ahead of snapshots. This is the shape of a self-inflicted spike on the
requesting client and is what D-006.13 removes (per-viewer record, only
changed values pushed).

**SOURCE (death).** Dying runs pristine `CalculateRanks`, which rewrites only
`CS_SCORES1/2`; the proxy adds at most two small per-viewer corrections, and
the client does not request `scores` on death (`CG_ScoresDown_f` is bound to
`+scores`, `cg_consolecmds.c:66-86`). So no proxy burst exists on death.

**INFERRED (death).** A dead client draws the scoreboard, and that path calls
`CG_LoadDeferredPlayers` → `CG_ActualLoadDeferredPlayers`
(`cg_players.c:1947-1978`, `cg_view.c:2452-2456`), which loads **every**
deferred player model in one frame. That is stock cgame behaviour (the reason
`cg_forceModel` exists) and a client-side frame hitch, not proxy traffic. It is
not claimed to be fixed by D-006.13.

### Cheapest experiment

- Client-side: reproduce a death/scoreboard-open with `cg_deferPlayers 0`
  (or `cg_forceModel 1`). If the death spike disappears and the tab spike
  remains, the death half is the deferred-model load; with D-006.13 the tab
  half should then be empty (no proxy `cs` traffic on a steady roster).
- Server-side: on a live multi-match server, count `G_SEND_SERVER_COMMAND`
  traffic to the requesting client around a `scores` request (the traffic
  should be the `scores` message only after the first refresh) and compare a
  tab press before/after a match move (exactly the moved slot re-pushed).

---

## U-010 — Server-side demo recording: unexercised wire-format cases

### Question

The Phase 0 evidence (`docs/demo-format.md`) pins the common `.dm_26` format,
but four cases are not covered by the 49 shipped demos or by the runtime
capture. Which of them must the recorder handle before release?

### Classification

**important — the common path is verified; these are release-gate scope
questions, not blockers for starting the writer.**

1. **RMG/terrain maps.** When `TheRandomMissionManager` (`0x8394b50`) is
   non-null, the shipped `SV_SendClientGameState` writes a variable gamestate
   tail (two deflated heightmaps, a seed and `SV_WriteRMGAutomapSymbols`)
   instead of `MSG_WriteShort(0)`. No such capture exists; the current plan
   only settles the null case.
2. **Vehicle playerstate.** `SV_WriteSnapshotToClient` writes a second
   `MSG_WriteDeltaPlayerstate(..., qtrue)` block when
   `frame->ps.m_iVehicleNum != 0`. None of the 49 demos has a non-zero
   `m_iVehicleNum` in its first 300 frames, so the block is untested against a
   golden file.
3. **Gamestate `clientNum` choice.** A real following-spectator recording has
   gamestate `clientNum` = the *viewer* slot (5) and snapshot
   `ps.clientNum` = the *subject* (1, `PMF_FOLLOW`). A server-side recorder has
   no real viewer slot; whether to use the viewer-style slot or the subject's
   slot needs a playback check.
4. **Stock 1.01 client playback.** Not run here (no stock client binary and no
   display). The 1.011 server writes the RMG short and the reference client
   reads it; a stock client that did not would misparse the trailer
   (**INFERRED** that it reads it).

### Cheapest experiment

- RMG: load a map that sets `TheRandomMissionManager` (or force the pointer in
  a scratch container), capture `SV_SendClientGameState`, and compare the tail
  with a client-recorded demo of the same map.
- Vehicle: record a demo while a player mounts a vehicle, then run
  `tools/demo_inspect.py --all-snapshots`; extend the decoder with the second
  (vehicle) playerstate block and compare the frame boundaries.
- clientNum: write the same short segment with both choices and play both in a
  stock/TaystJK client; compare the camera, HUD and followed-model rendering.
- Playback: validation gate A (stock 1.01 and TaystJK), as planned.

### Phase 2 status (2026-11-11)

The Phase 2 recorder (`docs/demo-format.md` §11) chose explicit non-fatal
handling for the first two cases and left the playback questions open:

1. **RMG/terrain maps** — refused, not encoded: the encoder only implements
   the `int16 0` tail, so when `TheRandomMissionManager` is non-null the
   recorder logs and starts no segment. Still unverified.
2. **Vehicle playerstate** — unsupported: a frame whose subject has
   `ps.m_iVehicleNum != 0` is discarded (the previous frame stays the delta
   base); no stream is produced that a client would misparse. Still
   unverified against a golden file.
3. **Gamestate `clientNum`** — resolved (2026-11-12): the gamestate names an
   unused slot (the highest slot the server never assigns, else the highest
   free slot with a per-tick watchdog, else the subject itself as a self-view
   POV), never a player other than the followed one. Naming the subject's slot made a player
   who reconnects there the demo's "me", and the newcomer appeared fused with
   the followed player (`docs/demo-format.md` §11). The playback check of a
   *switched* stream is U-011.
4. **Playback** — resolved by the owner (2026-11-11): the unmodified moving
   artifact plays and ends as expected on both TaystJK and a stock client, so
   the `svc_setgame base` frame, the gamestate `clientNum` choice and the
   snapshot chain are accepted by real clients. The development session could
   load the map but not capture the GL window; that limitation no longer
   applies to the release decision.

### Status (Phase 5, 2026-11-12)

- **1 RMG maps — open.** The encoder only emits the `int16 0` tail; with
  `TheRandomMissionManager` non-null the recorder logs and starts no segment.
  No RMG map was recorded or played. Experiment unchanged: load an RMG map,
  capture the server gamestate and compare with a client recording; implement
  the variable tail only if RMG recording is required.
- **2 Vehicle playerstate — open.** Frames whose subject has
  `ps.m_iVehicleNum != 0` are skipped (the previous frame stays the delta
  base); no stream is produced that a client would misparse, but a demo of a
  player driving a vehicle has a gap. Experiment: record a player mounting a
  vehicle and decode/play the result, then implement the second playerstate
  block.
- **3 Gamestate `clientNum` — resolved.** Spare slot (highest unassigned, then
  highest free with watchdog, then self-view); the rule is unit-tested and
  every Phase 2–4 artifact uses `clientNum 31`. The live 32-slot corner is
  U-015.
- **4 Playback — owner-verified** for the Phase 2 moving artifact (TaystJK and
  a stock client). Phase 3/4 files were loaded by the real client to completion
  without parse errors, not visually verified.

### Status (Phase 6, 2026-11-12)

- **1 RMG maps — open.** Unchanged: no segment on a non-null
  `TheRandomMissionManager`.
- **2 Vehicle playerstate — open.** Unchanged: those frames are skipped.
- **3 Gamestate `clientNum` — resolved and exercised.** Ground truth: the
  recorder segment and a real following spectator's demo are identical on all
  120 common ticks (37/37 `ps` fields, same entity set, `PMF_FOLLOW`), and the
  only difference is the gamestate `clientNum` (spare viewer 31 vs the
  spectator's own 1). The 32-slot fallback was also exercised live (U-015).
- **4 Playback — load-level verified on TaystJK; stock still open.** The
  Phase 6 matrix (FFA, team, CTF, stock duel, parallel duels/TFFA, POV switch,
  empty/repopulate, 32-slot self-view/watchdog, 1 MiB cap segment and its
  continuation) all reached `DISCONNECTED` with no truncation or parse error.
  Only TaystJK is installed here; stock 1.01 playback of these files is
  untested, and none of the files was visually watched (Wayland capture).

---

## U-011 — Mid-file follow-target switch: client playback

### Question

`docs/demo-format.md` §11 now switches the recorded POV inside the same file
when the subject leaves, is kicked, or joins spectators (spectator semantics:
the next snapshot's `ps.clientNum` changes). The gamestate still carries the
*original* subject slot, which may be gone or reused by then. Does a stock
client follow the new target cleanly across that point?

### Classification

**minor — the format is the same follow-target change a spectator demo records,
and the engine/cgame handle it while spectating; the risk is limited to the
dangling `cg.clientNum` (a slot that may no longer hold a player) and to
HUD/scoreboard presentation.**

### Evidence so far

- `artifacts/subject-switch/demos/20261111-222520_mp_ffa3_main_s1.dm_26`
  (subject kicked, 0→1 at 13.05 s) and
  `artifacts/subject-spectate/demos/20261111-222902_mp_ffa3_main_s1.dm_26`
  (subject forced to `TEAM_SPECTATOR`, 0→1 at 65.45 s) both validate:
  `max_delta=1`, entity order/range clean, gamestate round-trip identical.
- Phase 3 (`docs/demo-format.md` §12) repeated the switch without bots:
  `artifacts/phase3-life/demos/20261112-094132_mp_ffa3_main_s1.dm_26`
  (`0 -> 1 (left)`, 184 frames) validates; the real TaystJK client played a
  different Phase 3 file to its end without parse errors, but the switched
  file itself has not been played on a client yet.
- The Phase 4 follow-up (JSON listing, intermission block) did not change the
  switch path; the owner's visual playback remains the Phase 2 moving artifact,
  which has no mid-file switch.
- **Phase 6 (2026-11-12):** `artifacts/phase6/demos/20261112-170744_mp_ffa3_main_s3.dm_26`
  contains a live switch (`demo subject switched: 0 -> 1 (left)`, decoded
  `ps.clientNum` 0→1 at tick 2083650) and was played by TaystJK to
  `DISCONNECTED` with no parse error. The camera re-follow and HUD were not
  visually watched, so the visual part stays open.

### Cheapest experiment

Play both files on TaystJK and a stock client; at the switch point check that
the camera re-follows the new player, the HUD keeps the new player's health/
scores, and `disconnect` after the end is clean. If the dangling
`cg.clientNum` misbehaves, the alternative is to write a viewer-style
`clientNum` (U-010 item 3) and retest.

---

## U-012 — Scoreboard inside recorded demos: playback check

### Question

Recorded demos now carry periodic `scores` commands generated by the game's own
`DeathmatchScoreboardMessage` (`docs/demo-format.md` §11), with the stock 14-int
layout and `taystJKinfo` removed from the recorded serverinfo. Does a stock and
a TaystJK client draw the board from that stream across a whole demo?

### Classification

**minor — the payload is the game's own text and the layout matches the
serverinfo the demo carries; the risk is presentation (column alignment,
missing deaths) rather than a parse error.**

### Evidence so far

- `artifacts/demo-scoreboard-final/demos/20261111-225844_mp_ffa3_main_s1.dm_26`
  carries boards for 1 and 2 clients (the earlier development run also covered
  3), each a clean 14-int layout, and validates; the recorded `cs[0]` has no
  `taystJKinfo` while the live server still advertises `128`.
- Phase 3 (`docs/demo-format.md` §12): the TFFA files carry their own board
  text (the subject's match rewrite) and the duel files their own court
  `cs30`/`cs31`; all validate. A real TaystJK client played two of these files
  to completion without a parse error, but no one has pressed TAB on them
  (and the client could not be screen-captured here).
- Phase 4 intermission evidence: the demo keeps 3 s of intermission and its
  final `scores` commands (decoded: `pm_type = PM_INTERMISSION`, scoreboard
  payloads), so a demo now has a full final board even without TAB. The
  client-side TAB rendering is still unchecked.
- Phase 6 (2026-11-12): every matrix file, including the FFA/team/CTF/duel
  files that carry periodic `scores`, played to `DISCONNECTED` without parse
  errors. No one pressed TAB during playback (and the client window could not
  be captured), so the rendering question is still open.

### Cheapest experiment

Play that file (and a longer one with a real match) on a stock client and
TaystJK; hold TAB at several points and check the roster, scores, pings and
teams update, the deaths column is absent (stock layout), and the board does
not shift after a client joins/leaves. If deaths in demos matter, the
alternative is to record with the 15-int layout and strip nothing, accepting
that stock clients then misparse the board (the two cannot share one stream).


---

## U-013 — Demo chat rule

### Question

Which chat lines should a server-side demo carry?

### Classification

**resolved — owner decision, 2026-11-12: broadcasts only.**

### Decision

A demo carries the server-wide `chat`/`lchat` line and nothing else. Team chat
(`say_team` → `tchat`/`ltchat`), a private `tell` and a per-client `print` are
never recorded. The rule is isolated in `recorder::chat_command_for` so a later
change stays a one-function edit plus its unit test.

### Evidence

- `G_Say`/`G_SayTo` (`g_cmds.c:1550-1554`) send chat per recipient, so the
  subject's copy is exactly the broadcast that player received; the game sends
  team chat under a different command name, which the filter excludes.
- `SAY_TELL` wraps the sender's name in `\x19[…\x19` (`g_cmds.c:1601-1613`),
  the only observable that separates a tell from a broadcast line.
- `artifacts/phase3-tffa/demos/20261112-101848_mp_duel1_tffa1_s1.dm_26` and
  `…_tffa2_s1.dm_26` each carry `chat hello-all` once and no `tchat`, although
  both `say_team team-main` and `say_team team-two` were sent by players of
  those matches; `artifacts/phase3-chat/demos/20261112-101757_mp_ffa3_main_s1.dm_26`
  carries the two broadcasts and not the private `tell secret-tell`.
- Unit tests `chat_rule_is_the_copy_addressed_to_the_subject` and
  `chat_rule_records_broadcasts_but_never_team_chat`.

---

## U-014 — Max-file-size guard: runtime trigger not yet observed

### Question

`proxy_sv_demoMaxFileMB` closes a segment that reaches the cap, warns once and
continues with the next segment number. The threshold helper is unit-tested,
but was the close/continue path ever seen at runtime?

### Classification

**resolved (Phase 6, 2026-11-12) — the close/rotate path was triggered at
runtime; the threshold helper and its boundaries stay unit-tested.**

### Evidence so far (2026-11-12, Gate C)

With the cap lowered to `1` MiB, several busy FFA runs were driven with bots
(`sv_fps` 40–125, up to 29 bots and up to five human subjects). The largest
single segment before the subject left was
`artifacts/phase4-size4/demos/20261112-115603_mp_ffa3_main_s1.dm_26`
(853,536 bytes). The synthetic subjects are killed about every 60 s; the game
sets a dead FFA player to `TEAM_SPECTATOR`, and the Phase 3 rule closes the
segment when its only human is spectating, so no segment reached 1 MiB. No
`demo segment reached` line was produced, and no new segment number followed —
i.e. the guard did not fire before the segment closed for the subject-loss
reason.

### Cheapest experiment

On a real server with human players (who respawn and keep moving), set
`proxy_sv_demoMaxFileMB 1` for one busy round; the proxy log should show
`demo segment reached proxy_sv_demoMaxFileMB 1 MB (…)` once and the next
segment should start with the next `s<N>` number. Alternatively build a
throwaway binary whose cap is read in bytes (not MiB) and repeat the bot run.

### Resolution (Phase 6, 2026-11-12)

The real-client variant of the experiment succeeded on the final build: a
TaystJK client as subject on a bot server with `proxy_sv_demoMaxFileMB 1` and
`sv_fps 125` produced

```
----- proxy-rs: demo recording stopped: …_s2.dm_26 (14827 frames, 1048639 bytes)
----- proxy-rs: demo segment reached proxy_sv_demoMaxFileMB 1 MB (1048639 bytes) — closed …_s2.dm_26
----- proxy-rs: demo recording started: …_s3.dm_26 (instance main, subject 7, viewer 31, …)
```

The closed file is 1,048,647 bytes on disk (the count when the cap fired plus
the 8-byte `-1/-1` terminator written while closing) and is at the inclusive
1 MiB threshold (1,048,576). Both `…_s2` and the continuation `…_s3` play to `DISCONNECTED` in
TaystJK without parse errors. Synthetic subjects at 32 clients still die of
`Server command overflow` before the cap, which is why the real client was
needed; that limitation is in the capture tool, not in the recorder.

---

## U-015 — Live 32-slot server: viewer identity and POV switching

### Question

With `sv_maxclients 32` every slot can be assigned, so the recorder cannot name
a spare viewer slot: the gamestate uses the subject's own slot (self-view, no
`PMF_FOLLOW`). Does that play correctly, and what happens when the subject
leaves and the slot is reused?

### Classification

**partially resolved (Phase 6, 2026-11-12) — the mechanics (spare-slot
watchdog, self-view) are runtime-verified on a genuinely full 32-slot server
and both files load to the end; only visual playback remains open.**

### Evidence so far

- Unit tests cover `pick_viewer_slot` (highest unassigned, highest free,
  subject fallback) and `identity_lost` (a stranger on the viewer slot closes
  the segment).
- Phase 6 (2026-11-12) filled the corner for real on the final build:
  `sv_maxclients 32`, 30 bots + one human left one free slot, the recorder
  started `subject 3, viewer 4` (follow stream); adding the 32nd client logged
  `demo viewer slot 4 taken by client 4; closing …` and closed the segment,
  and the next segment started `subject 3, viewer 3` (self-view) on the full
  server. Decoded: the watchdog file has gamestate `clientNum 4` and
  `pm_flags 4672` (`PMF_FOLLOW`); the self-view files have `clientNum 3` and
  `pm_flags 576` (no `PMF_FOLLOW`). All play to `DISCONNECTED` in TaystJK
  without parse errors.
  The earlier (different-build) corner run also showed the POV change followed
  by slot reuse (`subject switched: 0 -> 8`, then watchdog close, then a new
  self-view file).
- Not observed: the camera/HUD at the self-view and the watchdog switch (no
  display capture), and a switch on a full 32-slot server where the old
  subject's slot stays empty (the follow view naming an empty slot), which is
  the case the code leaves open.

### Cheapest experiment

Run a 32-slot server with 32 human clients, record through a subject leaving
and a rejoin into the old slot, then play the files on TaystJK/stock: the
self-view must not fuse the newcomer, and the expected segment close/restart
must be visible in the proxy log.

---

## U-016 — Client receipt of the intermission demo block

### Question

At intermission the recorder closes the files after
`proxy_sv_demoIntermissionSecs` and broadcasts the link block with `print`.
Is the block visible to the players at intermission, exactly once, and does
`/demos` still list the files during that window?

### Classification

**minor — the same `print` broadcast was captured in a synthetic client's
server-message stream after a map change in Phase 4, and the server log now
records the exact lines (`event=demo.intermission.line`). The timing path
(intermission) and the one-shot guard have not been seen by a real client.**

### Evidence so far

- Phase 4 follow-up runtime: `event=demo.intermission 1 file(s) announced at
  intermission, 2 client line(s)` followed by the header and URL; the demo file
  closes 3 s after `BeginIntermission` and contains `PM_INTERMISSION` and the
  final `scores`.
- The synthetic client capture tool failed to produce a decodable stream across
  that map restart (`illegal svc op`), so no client-side capture of the
  intermission block exists; the `print` delivery itself was captured earlier
  for the same block at a map start.
- The generation is marked announced so `GAME_INIT` does not repeat it; this
  guard is code-reviewed, not observed on a client.
- Phase 6 did not add a real-client intermission run: the soak used map
  changes, and the playback matrix starts from recorded files, not from the
  end-of-game block. The question stays open as stated.

### Cheapest experiment

On a real server, end a game with two human clients; confirm both see the block
at intermission (console, not chat), that `/demos` lists the files during the
remaining intermission, and that no second block appears on the next game
start. Keep `proxy_sv_demoIntermissionSecs` below the intermission minimum.
