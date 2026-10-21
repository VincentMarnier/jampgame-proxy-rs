# D-008 — Map-change / `map_restart` nav-graph leak

Date: 2026-10-20. Evidence: upstream proxy commit
`687997412ea6e5ead93f5c7b25db552590a2eeb1` ("feat: added the map restart
memory leak fix"), engine source `original/jedi-academy/codemp/server/NPCNav/navigator.cpp:602-655`
(`CNavigator::Load`), `original/jedi-academy/codemp/game/g_main.c:1034`
(`trap_Nav_Load` at `GAME_INIT`), runtime RSS measurements reproduced below.

## Symptom (reported)

Server memory grows on every map change / `map_restart`; the upstream proxy had
the same issue and fixed it, but the Rust port still showed growth.

## Root cause

`CNavigator::Load` appends the parsed nodes to `m_nodes` and inserts the file's
failed edges into `m_edgeLookupMap` **without clearing either first**
(`navigator.cpp:602-655`). The engine frees them only through
`CNavigator::Free` (`navigator.cpp:583-594`), which the game calls via
`trap_Nav_Free` (`G_NAV_FREE`, 201) — but the pristine game never calls it
before re-loading the nav graph.

The upstream fix hooks `CNavigator::Load` (`0x8124ff4`) and calls
`trap_Nav_Free()` before the original body. The Rust port has the same hook
(`engine_sv::navigator_load`, `ENGINE_HOOKS` entry 11), **but it does not run
for the load that matters**: `game_init` forwards `GAME_INIT` first
(`rust/src/lib.rs:254-255`) and attaches the hook layer only afterwards
(`rust/src/lib.rs:278-282`). Pristine `G_InitGame` loads the nav data during
that forwarded call (`g_main.c:1034`), so every map load/`map_restart` ran with
`navigator_load` *detached* — the old graph stayed in `m_nodes` and the new one
was appended on top.

This is a consequence of a deliberate ordering choice (hooks need the memory
layer and the game's located data), not of a wrong hook.

## Fix

Intercept the game's own nav-load trap in the syscall forwarder, which is
active from `dllEntry` on (`rust/src/syscall.rs`, `G_NAV_LOAD` arm): with the
proxy enabled, call `G_NAV_FREE` (201) before forwarding `G_NAV_LOAD` (202).
`CNavigator::Free` is idempotent, so the existing engine hook (when attached)
simply frees an empty graph.

The intercept is gated on `proxy_sv_enable`, like every other syscall rewrite:
with the master switch off the proxy stays a pure passthrough.

## Verification

The shipped `.nav` files in this checkout are 524-byte placeholders with
`numNodes = 0`, which allocates no node graph and hides the leak. For the
measurement a **synthetic 2000-node nav** was generated for `t3home2` using the
map's own checksum from `taystjk/maps/t3home2.nav` and the engine's on-disk
format (`navigator.cpp:385-475`: `JNV5` header, checksum, node count, then
`NODE` records with `numRanks` rank arrays, then the 32 failed edges). It was
mounted at `/srv/jka/base/maps/t3home2.nav`; both servers loaded
`mp/t3home2` and ran the same `rcon map_restart` storm.

| build | start | +5 restarts | +10 more restarts |
|-------|-------|-------------|-------------------|
| pre-fix (`rust/jampgamei386.so` before this change) | 53 444 kB | 85 064 kB | 132 384 kB |
| fixed | 53 500 kB | 53 536 kB | 53 540 kB |

VmRSS read from `/proc/1/status` inside each container. The pre-fix growth is
the leaked node graph (16 MB of rank arrays per load; RSS reclaims part of it
between samples), the fixed build stays flat. The synthetic nav was not added
to the repository (test fixture only, generated from the shipped file's
checksum).

Unit tests (`rust/src/syscall.rs`): `nav_load_frees_the_previous_nav_graph_first`
installs a recording engine syscall and asserts the forwarder calls
`G_NAV_FREE` **before** `G_NAV_LOAD` and forwards the load's return value; the
existing `engine_syscall_round_trips` now shares a test lock with it because
both manipulate the process-global syscall pointer. `trap_ordinals_match_sdk`
pins `G_NAV_FREE = 201` / `G_NAV_LOAD = 202`.

`cargo test` 99/99, `clippy -D warnings` and `fmt --check` clean; i686 release
staged; engine smoke green.

## Consequence / invariant

Any engine choke point that pristine `GAME_INIT` exercises **before** the hook
layer attaches cannot be covered by a `GAME_HOOKS` entry. Either the hook
registers before `GAME_INIT` is forwarded, or the interception moves to the
always-active syscall forwarder (this fix). Future `GAME_INIT`-time patches
must check which side of the forward/attach boundary they run on.
