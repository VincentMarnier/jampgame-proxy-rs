# D-018 — Engine-call and stack-alignment faults found while verifying D-017

Date: 2026-11-03. Status: fixed. Scope: `engine.rs`, `lib.rs`, `challenge.rs`,
`ratelimit.rs`, `banfile.rs`.

Verifying D-017 required driving the real 1.011 engine's connectionless path
(`getchallenge` → Huffman-compressed `connect`). That surfaced four pre-existing
faults, each fatal on a normal run; the last three only appear once the
connectionless path is actually exercised.

## 1. Direct calls to fixed engine addresses

`engine::functions::*` are absolute addresses of the non-PIE engine
(`0x08048000` base). A call compiled from
`let f: F = transmute(CONST_USIZE); f(...)` becomes a **direct relative `call`**
(`e8 disp32`) whose displacement is computed at link time; at runtime the .so
is loaded at `base`, so the target is `base + address`, not `address`. The
loader applies no text relocation for it (the .so has no `.rel.text`).

Reproduced: `GAME_INIT` → `free_bsp_disk_images` crashed at
`0xfe0204e4` = module base `0xf5fa0000` + `Z_TagFree` `0x080804e4`; gdb's
frame `#1` was `vmMain+0x48d7`, and the disassembly there was
`e8 4d 7b 02 08` (`call 0x80804e4` at link time). The same arithmetic explains
the `0xf46774e4` / `0xfe0204e4` addresses in the first gdb runs.

Sites fixed to use `engine_fn!` (volatile read + indirect call, the pattern
already documented in `engine.rs`):

- `lib.rs::free_bsp_disk_images` → new `engine::z_tag_free`.
- `hooks/engine_sv.rs`: `Com_Printf`, `Com_WriteConfigToFile`,
  `NET_AdrToString`, `SV_DropClient`.
- `challenge.rs`: `NET_OutOfBandPrint` (the stateless-challenge answer — every
  `getchallenge` crashed the server).

`engine_fn!` is now `pub(crate) use`d so the hook modules can use it. Any new
fixed-address call must go through it.

## 2. `RandomState` in `challenge::random_key`

The stateless-challenge key used `std::collections::hash_map::RandomState`.
`RandomState::new` seeds from the OS and the prebuilt `std` spills an
`xmm` with `movaps`; the engine enters the game module with a **4-byte-aligned
stack** (D-001), so the first `getchallenge` was a SIGSEGV in
`hashmap_random_keys` (backtrace: `Once::call_once` → `hashmap_random_keys`).

Fix: `random_key` reads 16 bytes from `/dev/urandom` through libc
(`open`/`read`/`close`), with a clock/counter/module-address fallback when the
device is unavailable. No std RNG.

## 3. `HashMap` in `ratelimit`

The rate-limit table used `HashMap<u32, LeakyBucket>`. The prebuilt `std`
(hashbrown) instantiates an **aligned** `_mm_load_si128` group load. The crate
is built with `-C target-feature=-sse,-sse2`
(`rust/.cargo/config.toml`), but `cfg(target_feature)` in std's generic
`HashMap` code was resolved when *std* was compiled, so the SSE path survives
monomorphisation. Crash: first connectionless packet →
`ratelimit::address_allowed` → `_mm_load_si128` → SIGSEGV. This was why every
`getchallenge`/`getstatus`/`rcon` packet killed the server.

Fix: `BTreeMap<u32, LeakyBucket>`. It has no aligned-SIMD paths; a handful of
comparisons per packet at the 65536-entry cap is cheap.

## 4. `std::fs::read` in `banfile::load`

`std::fs::read` calls `File::metadata` → `try_statx` → `movaps` on a
zero-initialised `statx`, which faults on the engine's stack. The error path
(missing file) does not call `metadata`, so startup was fine and the first
successful `reloadbans` crashed.

Fix: `banfile` reads with libc `open`/`read`/`close` (chunked, no `stat`) and
writes with libc `open`/`write`/`rename`/`unlink`. `diagnostics.rs` still uses
`std::fs::File`/`OpenOptions`, which only call `open`/`write`, not `metadata`.

## Rule for future code

Code reachable from an engine callback (`vmMain`, hook wrappers, syscall
forwarding) assumes a **4-byte-aligned stack**:

- never call a fixed engine address directly — use `engine_fn!`;
- avoid std APIs that may use aligned SIMD: `HashMap`/`HashSet` (hashbrown),
  `RandomState`, `std::fs::read`/`metadata`/`File::metadata`. Prefer
  `BTreeMap`, `Vec`, direct libc, and explicit byte parsing;
- test on the real engine with a real packet flow, not only the load banner:
  three of these faults were reachable only from the connectionless path.

`rust/scripts/engine-test.sh` (load + UDP probe) and
`tools/jka_connect_probe.py` (full `getchallenge` → compressed `connect`) are
the regression checks.
