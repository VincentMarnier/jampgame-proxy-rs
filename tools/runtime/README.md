# Runtime experiments (container, gdb)

These scripts reproduce the 2026-09-09 runtime investigations recorded in
`docs/reverse-engineering.md` (R-017, R-018) and close out items in
`docs/uncertainties.md` (U-002, U-005, U-008). They run against the
`jampgame-investigation:i386-bookworm` image (see `tools/docker/run.Dockerfile`
and `docs/runtime-environment.md`).

All of them attach to a running `linuxjampded` (PID 1) inside a container
started with `--cap-add=SYS_PTRACE` and the GameData assets mounted, e.g.:

    docker run -d --name jka-inv --cap-add=SYS_PTRACE \
      -v "$PWD/artifacts":/srv/jka/artifacts \
      -v "$PWD/tools":/tools:ro \
      -v "$PWD/original/GameData":/srv/jka/gamedata:ro \
      jampgame-investigation:i386-bookworm \
      ./linuxjampded +set dedicated 1 +set net_port 29099 \
        +set fs_cdpath /srv/jka/gamedata +set sv_pure 0 \
        +set sv_maxclients 8 +set g_gametype 6 +map mp/duel1

then copy a script into the container (the `-v artifacts` mount above makes
`/srv/jka/artifacts` available) and run:

    docker exec jka-inv gdb -q -batch -x /srv/jka/artifacts/<script> -p 1

| Script | Question | What it does |
| --- | --- | --- |
| `u002_read.gdb` | U-002 | Reads every engine var slot (`svs`, `svs.clients`, `sv`, `rd_*`, `cmd_*`) and dereferences all 17 cvar-pointer slots the proxy uses (`Proxy_Engine_Wrappers.hpp`), printing name/string/integer of each `cvar_t`. |
| `u002_proxy_read.gdb` | U-002 | With the *original proxy* loaded, reads the proxy's own globals (`proxy.jampgameHandle/Address/originalVmMain/originalDllEntry/originalSystemCall`, `locatedGameData`, `server.svs/sv`) and compares them against `/proc/1/maps` and `nm` expectations. |
| `u005_semantic.gdb` | U-005 | Runs a second engine under gdb (port 29101, `+set rconPassword testpw` not `sv_rconPassword`!) with breakpoints at the engine addresses named `SV_ConnectionlessPacket`, `SV_GetChallenge`, `SVC_Info`, `SVC_Status`, `SVC_RemoteCommand`, `SV_FlushRedirect`; drive with `u005_semantic.drive.py`, which sends `getchallenge`/`getinfo`/`getstatus` and one rcon packet. |
| `u005_hook_call.gdb` | U-005/U-001 | With the proxy loaded, breaks at the proxy's own `Proxy_SV_ConnectionlessPacket` hook (`base+0xbbb0`) and detaches on the first real query, proving live hook execution. |

## Building the original proxy (for U-002/U-005-hook runs)

The original proxy source (`original/jampgame-proxy/`) was removed from the
workspace after the migration (2026-09-10). Clone the upstream repo outside
the workspace (build writes nothing under `original/`) and check out the
pinned source commit `687997412ea6e5ead93f5c7b25db552590a2eeb1`:

    rm -rf /tmp/opencode/proxybuild
    git clone https://github.com/VincentMarnier/jampgame_proxy /tmp/opencode/proxybuild
    git -C /tmp/opencode/proxybuild checkout 687997412ea6e5ead93f5c7b25db552590a2eeb1

The vendored Zydis (`src/third_party/zydis`) lacks its `zycore` submodule
(empty `dependencies/zycore`). The proxy pins zydis at
`04793b1c8e3c8b4e6301f3c45a08d9103e417f55`, whose zycore gitlink is
`75a36c45ae1ad382b0f4e0ede0af84c11ee69928`; fetch it into place:

    cd /tmp/opencode/proxybuild/src/third_party/zydis/dependencies/zycore
    git init -q . && git remote add origin https://github.com/zyantific/zycore-c.git
    git fetch -q --depth 1 origin 75a36c45ae1ad382b0f4e0ede0af84c11ee69928
    git checkout -q FETCH_HEAD

Then build (32-bit toolchain; `TARGET_ARCH=x86` makes the target
`jampgamei386.so` on a 64-bit-reporting kernel, see `run.Dockerfile` note):

    docker run --rm -v /tmp/opencode/proxybuild:/proxy i386/debian:bookworm \
      sh -c 'apt-get update -qq && apt-get install -y -qq cmake make g++ git \
             && cmake -B /proxy/build -S /proxy -DTARGET_ARCH=x86 \
             && cmake --build /proxy/build'

Output: `/tmp/opencode/proxybuild/build/src/jampgamei386.so`. Run it by
overlaying it on the pristine module:

    tools/docker/run.sh  # replaced by:
    docker run -d --name jka-proxy --cap-add=SYS_PTRACE \
      -v /tmp/opencode/proxybuild/build/src/jampgamei386.so:/srv/jka/base/jampgamei386.so:ro \
      -v "$PWD/artifacts":/srv/jka/artifacts \
      -v "$PWD/original/GameData":/srv/jka/gamedata:ro \
      jampgame-investigation:i386-bookworm \
      ./linuxjampded +set dedicated 1 +set net_port 29099 \
        +set fs_cdpath /srv/jka/gamedata +set sv_pure 0 +map mp/duel1

The proxy prints `----- Proxy: ...` lines through `printf`, which is
block-buffered on the docker log pipe; flush with
`docker exec jka-proxy gdb -q -batch -p 1 -ex 'call (int)fflush(0)' -ex detach`.

Notes:
- Symbol offsets used by `u002_proxy_read.gdb` / `u005_hook_call.gdb`
  (`proxy`, `server`, `jampgame`, hook at `0xbbb0`) come from `nm` on the
  freshly built, unstripped `.so`; re-derive them after a rebuild:
  `nm <so> | grep -E ' (server|jampgame|proxy)$'`.
- The rcon password cvar is `rconPassword` (the pointer slot the proxy names
  `sv_rconPassword` dereferences to a `cvar_t` named `rconPassword`);
  command-line `+set sv_rconPassword` silently creates a *different* cvar.
