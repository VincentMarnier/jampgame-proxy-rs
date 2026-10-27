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

## netStatus harnesses (A1/A2)

| Script | Question | What it does |
| --- | --- | --- |
| `a1_netstatus.sh` + `a1_netstatus_semantic.py` | R-022 | gdb-driven semantic check of the netStatus per-usercmd feed: injects `sv_client_think_stub` calls and one in-vivo `clc_move` (built with the engine's own `MSG_*` writers) into a live server and compares the printed table with a Python mirror of the original C++ arithmetic. |
| `a2_netstatus_groundtruth.sh` + `a2_netstatus_groundtruth.py` + `jka_proto.py` | R-026 / D-007 | Ground-truth harness: synthetic JA MP clients speak the real protocol over UDP to `linuxjampded` + the Rust proxy and reproduce the real client timing loop (`CL_SetCGameTime`, `CL_AdjustTimeDelta`, `CL_CreateCmd`, `CL_WritePacket`, the `cl_maxpackets` packet timer and the LAN/loopback bypass) with `cl_timeNudge`, client fps, `snaps`, `cl_maxpackets`, packet mode, loss and jitter as controlled inputs. Compares the estimate against the fed-in `cl_timeNudge`. |

    # one scenario (15-20 s), prints the table and the estimator's debug rows
    tools/runtime/a2_netstatus_groundtruth.sh --mode single --sv-fps 20 \
        --client-fps 60 --snaps 20 --cl-maxpackets 30 --tn -15 --duration 20

    # the acceptance matrix (sv_fps x client fps x snaps x cl_maxpackets x
    # LAN/non-LAN x tn, plus loss/jitter); artifacts/a2_matrix_results.json
    tools/runtime/a2_netstatus_groundtruth.sh --mode matrix

    # real-client ground truth: publishes the port so a real JA MP client can
    # connect (set cl_timeNudge by hand, ~15 s per value, run /netStatus and
    # copy the table it prints to your client console)
    JAMP_PUBLISH_PORT=29191 tools/runtime/a2_netstatus_groundtruth.sh \
        --mode real --duration 120 --sv-fps 20 --port 29191

`jka_proto.py` is a byte-exact Python port of the engine's wire codec
(`qcommon/msg.cpp` bit IO + `msg_hData` Huffman, `huffman.cpp` adaptive
Huffman incl. the OOB `Huff_Compress` path, `net_chan.cpp` framing and
fragment reassembly, `cl_net_chan.cpp` XOR obfuscation). It is the first
standalone protocol codec in the repo; `a1_*` instead drives the engine's own
`MSG_*` writers through gdb. Scoring reads the printed `netStatus` table —
its `timeNudge` cell `[lo..hi]` is symmetric around the point estimate, and
the harness's own `client_model_terms()` reports the ground-truth-side model
terms (mean cmd age, measured ack delay) for comparison. The proxy exposes no
estimator internals at runtime (`proxy_sv_netStatusDebug` was dropped at the
user's request); a real-client run is scored from the table your client
prints.

**Retired:** `diff_netstatus.sh` / `diff_netstatus_semantic.py` compared the
Rust table with the original proxy's table byte-for-byte. D-007 replaced the
`timeNudge` estimator (the magic `sv_fps` table is deleted) and reworked the
table columns, so the two outputs are no longer comparable and the differential
is retired with the old semantics. Historical notes about it remain in
`docs/reverse-engineering.md` (R-022 "HARNESS NOTES (diff flow)").

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
