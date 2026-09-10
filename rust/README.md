# jampgame-proxy-rs — Rust rewrite of `jampgame_proxy`

This workspace holds the new Rust implementation. Its current milestone is the
**bootstrap core + trap interceptions**: a 32-bit game-module shared object
that the 2003 Jedi Academy dedicated server (`linuxjampded`) loads as
`jampgamei386.so`, runs the version gate, forwards to the pristine module
(`jampgame_original.so`) it loads, initialises the engine memory layer,
intercepts the `G_LOCATE_GAME_DATA`/`G_GET_USERCMD` traps, and runs the proxy's
own `vmMain` event handlers (client connect/begin/disconnect/command/
userinfo + per-frame cvar refresh).

See `docs/` at the repository root for the reverse-engineering evidence
(findings R-001…R-020) and `../AGENTS.md` for the project rules.

## How it works (30 seconds)

```
linuxjampded  ──dlopen──>  jampgamei386.so  (this crate, Rust)
                dlsym "dllEntry" / "vmMain"
                                │
   dllEntry(engineSyscall) ────┘ stores the engine syscall pointer
                                │
   vmMain(GAME_INIT) ───────────┼─ version gate
                                │─ dlopen "base/jampgame_original.so"
                                │   (fs_game-aware, CWD-relative)
                                │─ bind symbols + base (dladdr), hand the
                                │   original its syscall shim
                                │─ engine memory layer (svs/sv, cvar slots)
                                │─ forward GAME_INIT, register proxy cvars
                                ▼
   every vmMain ──────────> jampgame_original.so  (the pristine 2003 module)
   GAME_RUN_FRAME ────────> refresh proxy cvar mirrors, track svsTime
   client events ─────────> Proxy_SharedAPI handlers (filter/sanitise)
   every game trap ───────> C variadic shim ──> Rust forwarder ──> engine
                                │
        G_LOCATE_GAME_DATA ─────┘ recorded (later hooks read it)
        G_GET_USERCMD ─────────── forwarded, then sanitised (forcesel/angles)
```

`vmMain` carries 13 `int`s (command + 12 args) and the syscall interface is
variadic cdecl with up to 1 (command) + 16 words — both forwarded verbatim,
matching the original proxy's fixed-arity forwarders.

## i386 stack-alignment note

The 2003 engine calls game modules with a 4-byte-aligned stack, but Rust i686
codegen assumes 16-byte alignment at entry and emits SSE moves that fault on
such a stack (the original proxy's "-mstackrealign" quirk). The i686 target
therefore disables SSE (`.cargo/config.toml`) and the C shim is compiled with
`-mstackrealign` (build.rs) — see `docs/decisions/0001-bootstrap-core.md`.

## Requirements

- **Docker** (the recommended build route — the artifact is a 32-bit library
  and the whole dev environment is containerised).
- To **run** the proxy end-to-end you additionally need the i386 runtime image
  and the game assets (see [Running under the real engine](#running-under-the-real-engine)).

## Building (dockerised dev environment)

Everything runs from this directory (`rust/`).

```sh
./dev.sh cargo build --target i686-unknown-linux-gnu --release
```

`dev.sh` builds the dev image on first use (Rust 1.85 + the `i686-unknown-linux-gnu`
target + gcc-multilib for `-m32` linking), then runs the given command inside a
container with this directory mounted at `/work`. It runs as your uid, so build
outputs are owned by you. Without arguments it drops you into a shell.

The engine looks for the exact filename `jampgamei386.so` (Cargo names the
cdylib `libjampgamei386.so`), so stage it:

```sh
./dev.sh sh ./scripts/build.sh      # -> ./jampgamei386.so
```

Other useful invocations:

```sh
./dev.sh cargo test                                    # unit tests
./dev.sh sh -c 'cargo fmt --check && cargo clippy --target i686-unknown-linux-gnu --all-targets'
```

The dev image/toolchain is pinned in `Dockerfile`; cargo's crate index is
cached in a named volume so rebuilds are fast.

### Building without Docker (optional)

You need Rust ≥ 1.85, the `i686-unknown-linux-gnu` rustup target, and an x86-64
`gcc-multilib` (provides the 32-bit crt/libc and `-m32`). Then the same two
commands above work directly, without `./dev.sh`.

## Running under the real engine

The runtime target is a separate 32-bit container that already exists for
runtime reverse-engineering (`tools/docker/` at the repository root): it holds
the pristine `linuxjampded` engine, the pristine game module under both names
(`jampgamei386.so` and `jampgame_original.so`), and mounts `original/GameData`
read-only.

End-to-end smoke test (loads the proxy, boots a duel server, checks the log
markers, then probes the server over UDP):

```sh
./dev.sh sh ./scripts/build.sh   # once, to produce ./jampgamei386.so
./scripts/engine-test.sh         # run on the host (needs docker + runtime image + GameData)
```

What a successful run shows in the engine log:

```
Sys_LoadDll(jampgame) found **vmMain** at  0xeef19fd0
----- proxy-rs 0.1.0: dllEntry, engine syscall registered
----- proxy-rs 0.1.0: passthrough skeleton loaded
----- proxy-rs: loading original game library jampgame_original.so ("base/jampgame_original.so")
----- proxy-rs: jampgame_original.so properly loaded
------- Game Initialization -------
gamename: basejka
```

`engine-test.sh` environment variables: `JAMP_IMAGE`, `JAMP_GAMEDATA`,
`JAMP_PORT` (default `29999`), `JAMP_NAME`, `JAMP_KEEP=1` to keep the
container after the run. Without the runtime image or assets it exits with an
explanatory error.

For a persistent server you can join with a real client and fight bots against
the freshly built proxy:

```sh
./scripts/run-server.sh    # rebuilds via dev.sh, then runs the engine
```

It (re)builds the proxy through the dev container, boots a bot-filled FFA
server on `mp/ffa1` in the runtime image with the proxy overlaid, and publishes
the UDP port (default `29070`, the stock JKA port) on all host interfaces so
clients can `connect <host-ip>:29070`. `Ctrl-C` stops it. Env knobs:
`JAMP_PORT`, `JAMP_BIND`, `JAMP_MAP`, `JAMP_GAMETYPE`, `JAMP_BOTS`,
`JAMP_MAXCLIENTS`, `JAMP_IMAGE`, `JAMP_GAMEDATA`, `JAMP_NO_PROXY=1` (pristine
module), `JAMP_SKIP_BUILD=1` (reuse the staged `.so`), `JAMP_KEEP=1`,
`JAMP_EXTRA`.

## Layout

```
Cargo.toml              cdylib crate (name jampgamei386), edition 2024
.cargo/config.toml      i686 rustflags (SSE disabled: 2003 4-byte stack)
build.rs                compiles csrc/syscall_shim.c (cc, -mstackrealign)
csrc/syscall_shim.c     C variadic trap shim (stable Rust can't define one)
src/lib.rs              exported dllEntry / vmMain + GAME_INIT/SHUTDOWN/RUN_FRAME
src/sdk.rs              SDK ABI: repr(C) types, trap ordinals, layout pins
src/state.rs            proxy state (locatedGameData, client data, cvar mirror)
src/original.rs         dlopen/dlsym/dladdr of the original module
src/jampgame.rs         game-module helper function bindings (base+offset)
src/engine.rs           engine address tables + memory-layer init + Com_EndRedirect
src/shared_api.rs       trap interceptions + vmMain client event handlers
src/syscall.rs          engine syscall pointer, trap helpers, forwarder
src/utils.rs            proxy-local string/name helpers
Dockerfile              dev image (rust + i686 target + multilib)
dev.sh                  host front-end into the dev container
scripts/build.sh        stage ./jampgamei386.so
scripts/engine-test.sh  end-to-end test against the real engine
scripts/run-server.sh   persistent bot-filled server for real-player testing
```

## Status / scope

Milestone **bootstrap core + trap interceptions + D-002 hook layer** (docs
`R-020`, `R-021`). Implemented:

- version gate (rejects non-pristine engines),
- original-module load + symbol binding (`dllEntry`/`vmMain`/`level`/
  `g_entities` via `dlsym`, load base via `dladdr`),
- engine memory layer: `svs`/`sv` + the 17 cvar-pointer slots read at
  `GAME_INIT` (R-018-verified addresses), plus the full reference address
  tables from `Proxy_Engine_Wrappers.hpp` as data,
- proxy cvar mirror (`proxy_sv_*`, D-002 keep set) registered at `GAME_INIT`
  and refreshed each `GAME_RUN_FRAME`,
- `G_LOCATE_GAME_DATA`/`G_GET_USERCMD` trap interception,
- `vmMain` dispatch for `GAME_RUN_FRAME` / `GAME_CLIENT_CONNECT` / `BEGIN` /
  `DISCONNECT` / `COMMAND` / `USERINFO_CHANGED` with the proxy's command and
  userinfo filtering, and `GAME_SHUTDOWN` with hook detach + `Com_EndRedirect`
  + unload,
- the D-002 hook layer (R-021): detour machinery (`rust/src/patch.rs`, a
  whole-instruction length decoder regression-pinned against the real hook-site
  bytes), the rcon + q3infoboom + model-length + download-validation +
  `ipAuthorize` + `SV_SvEntityForGentity` + `CNavigator::Load` +
  `SV_SendClientGameState` engine hooks, the game stats / anti-HP / min-jump
  wrappers, and the netStatus feed + table printer. Two original whole-function
  rewrites (`SV_PacketEvent`, `SV_ExecuteClientMessage`) are verified no-ops
  against the shipped binary and dropped (R-021).

Not yet runtime-verified (needs a real client / scenario): the `netStatus`/
`showNet` table rendering, downloads end-to-end, `CNavigator::Load` on a
nav-using map. `Q_stricmp` is bound to the correct `0x1a5304` (U-007).
