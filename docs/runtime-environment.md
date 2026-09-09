# Runtime environment for running the original binaries

> Status: working — the original engine + game module run end-to-end here
> (R-017). This document describes the container setup and how to use it.

## Problem it solves

`docs/uncertainties.md` U-002 and the older R-016 notes reported that the 2003
game module `jampgamei386.so` could not initialize under "modern glibc"
(INIT segfault) and that modern loaders refuse to `dlopen` it (`cannot enable
executable stack`). Those observations came from test harnesses whose *main
executable* was built today (NX stack) running on host userspace (glibc 2.43).

Both problems disappear in a 32-bit Debian userspace container with
**glibc 2.36** (`i386/debian:bookworm`):

- a modern NX-stack main can `dlopen` the pristine 2003 `.so` there; and
- under the real 2003 engine (`linuxjampded`, an `ET_EXEC` without
  `PT_GNU_STACK`, so the kernel grants an executable stack /
  READ_IMPLIES_EXEC) the game module loads, its INIT runs, `GAME_INIT`
  executes, and the dedicated server answers protocol-26 UDP queries.

So for runtime reverse-engineering of the engine/game (address read-back,
hook call stacks, proxy behaviour) **no period-correct 2003 distro is
needed** — use the container described below.

## Files

- `tools/docker/run.Dockerfile` — image definition (i386 Debian bookworm).
- `tools/docker/build.sh` — build the image (`docker build`).
- `tools/docker/run.sh` — run a server or any command in the container.
- `tools/q3_udp_query.py` — UDP `getchallenge`/`getinfo`/`getstatus` probe.
- `.dockerignore` (repo root) — keeps `original/GameData` and submodule
  sources out of the build context/image layers.

## Building the image

```sh
tools/docker/build.sh            # docker build -t jampgame-investigation:i386-bookworm
```

The build context is the repository root; the Dockerfile only COPYies the
unmodified reference binaries from `original/jalinuxded_1.011/` (plus the
Intel C++ runtime `libcxa.so.1` into `/usr/lib`) and installs tooling
(`gcc`, `g++`, `gdb`, `strace`, `binutils`, `file`, `python3`, `procps`).

The user-provided game assets (`original/GameData`, ~1.7 GB) are **never**
baked in and are never written to: they are only mounted read-only at run
time (see below). Nothing in `original/` is modified by this workflow.

## Container layout

Inside the image (`WORKDIR /srv/jka`, `HOME=/srv/jka`):

```
/srv/jka/linuxjampded            <- pristine engine (from original/jalinuxded_1.011)
/srv/jka/base/jampgamei386.so    <- pristine game module (same source)
/srv/jka/base/jampgame_original.so <- same module under the name the proxy dlopen()s
/usr/lib/libcxa.so.1             <- Intel C++ runtime needed by both 2003 binaries
/srv/jka/gamedata/               <- runtime mount point for original/GameData (ro)
```

Why this layout:

- `fs_basepath` defaults to `getcwd` (`/srv/jka`), so the engine finds the
  game module at `<fs_basepath>/base/jampgamei386.so` via `Sys_LoadDll`
  (`unix_main.c`). Keeping `/srv/jka/base/` on a writable image layer means
  the loader always picks *our* pristine module regardless of what the
  mounted assets contain.
- Assets are exposed through `+set fs_cdpath /srv/jka/gamedata`; FS_Startup
  adds `/srv/jka/gamedata/base` to the search path. This keeps `original/
  GameData` read-only and opaque.
- CWD == `fs_basepath`, which is also what the proxy requires later: its
  `dlopen(fs_game + "/jampgame_original.so")` is CWD-relative (U-004) and
  resolves to `/srv/jka/base/jampgame_original.so` (already present in the
  image under that name).

## Running

Start the default duel server (interactive; Ctrl-C to stop):

```sh
tools/docker/run.sh
```

Custom engine command lines / other commands:

```sh
# arbitrary dedicated-server arguments
tools/docker/run.sh ./linuxjampded +set dedicated 1 +set net_port 29099 \
  +set fs_cdpath /srv/jka/gamedata +set sv_pure 0 +map mp/duel1

# interactive shell
tools/docker/run.sh bash

# gdb on the engine (image has gdb; run.sh adds --cap-add=SYS_PTRACE)
tools/docker/run.sh gdb --args ./linuxjampded +set dedicated 1 +set net_port 29099 \
  +set fs_cdpath /srv/jka/gamedata +map mp/duel1
```

Useful environment variables for `tools/docker/run.sh`:

| Variable | Effect |
| --- | --- |
| `JAMP_IMAGE` | image name (default `jampgame-investigation:i386-bookworm`) |
| `JAMP_GAMEDATA` | host path to the read-only assets (default `<repo>/original/GameData`) |
| `JAMP_PROXY_SO` | host path to a proxy `jampgamei386.so` to overlay at `/srv/jka/base/jampgamei386.so` (ro) |
| `JAMP_ARTIFACTS` | host directory mounted rw at `/srv/jka/artifacts` (logs, cores, patched copies) |

Querying the server from the host while it runs (needs the port published,
e.g. `docker run -p 29099:29099/udp`):

```sh
tools/q3_udp_query.py 127.0.0.1 29099
```

## Running the original proxy in the container (R-018)

The original proxy can be built and run in the same container to reproduce
the full-stack proxy behaviour (U-002/U-005/U-008 live evidence in R-018).
The proxy source is immutable, so build from a **copy outside the workspace**
(`/tmp/opencode/proxybuild`), fetching the missing `zycore` submodule at the
pinned gitlink, then overlay the resulting `jampgamei386.so` on the pristine
module (env `JAMP_PROXY_SO` in `tools/docker/run.sh`, or a `-v …:ro` mount).

Full recipe and the gdb experiment scripts live in `tools/runtime/README.md`
(`u002_read.gdb`, `u002_proxy_read.gdb`, `u005_semantic.{gdb,drive.py}`,
`u005_hook_call.gdb`). Notes:

- The proxy logs via `printf`, which is block-buffered on the docker log pipe;
  flush with `docker exec <c> gdb -q -batch -p 1 -ex 'call (int)fflush(0)'
  -ex detach`.
- Engine `.text` read-back after attach shows the expected `E9` detours, the
  25-byte NOP tile at `0x8056b26`, byte patch `0x8056c7d=0x03`, and the call
  retarget at `0x805882d`; the server keeps answering protocol-26 queries.

## Verified behaviour (R-017, observed 2026-09-09)

- `linuxjampded` banner: `JAmp: v1.0.1.1 linux-i386 Nov 10 2003`.
- `+map mp/duel1` → `Sys_LoadDll(/srv/jka/base/jampgamei386.so)...`,
  `found **vmMain** at <base+0x85684>`, `succeeded!`, then
  `Game Initialization` / `gamename: basejka` / `gamedate: Nov 10 2003`.
- Runtime base arithmetic matches `nm`: mapping base + `vmMain` file offset
  `0x85684` == printed `**vmMain**` pointer.
- Server answers `getinfo`/`getstatus` with `\protocol\26\mapname\mp/duel1`.
- No GNU_STACK patching, no LD_PRELOAD, no privilege tricks beyond
  `--cap-add SYS_PTRACE` (needed later for gdb/strace).

## Notes and caveats

- The image kernel is the host kernel (7.0.0-31-generic at the time of
  writing); only userspace is i386. `uname -m` inside the container reports
  `x86_64` because it prints the kernel architecture.
- On the host (Ubuntu glibc 2.43) the same pristine `.so` cannot be dlopen'ed
  from an NX-stack main (`cannot enable executable stack`). Test harnesses
  that must run on the host or on newer-glibc environments should keep the
  R-016 GNU_STACK workaround (test copies only, never `original/`).
- The mounted GameData includes many custom pk3s alongside stock
  `assets0..3.pk3`. For behaviour tests that need a pristine asset base,
  prefer stock-only pk3s (non-blocking; see uncertainties).
