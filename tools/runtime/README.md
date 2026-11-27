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
| `a2_netstatus_groundtruth.sh` + `a2_netstatus_groundtruth.py` + `jka_proto.py` | R-026 / D-007 | Ground-truth harness: synthetic JA MP clients speak the real protocol over UDP to `linuxjampded` + the Rust proxy and reproduce the real client timing loop (`CL_SetCGameTime`, `CL_AdjustTimeDelta`, `CL_CreateCmd`, `CL_WritePacket`, the `cl_maxpackets` packet timer and the LAN/loopback bypass) with `cl_timeNudge`, client fps, `snaps`, `cl_maxpackets`, packet mode, loss, fixed one-way delay (`--delay-ms`) and jitter as controlled inputs. Compares the estimate against the fed-in `cl_timeNudge`. |
| `a3_timenudge_void.sh` + `a3_timenudge_void.gdb` | D-015 | End-to-end check of the server-side `cl_timeNudge` neutralisation: runs one synthetic client (`tn = -15`) with the feature off, samples `level.clients[0].ps.commandTime - level.time` once per `GAME_RUN_FRAME` (`vmMain` breakpoint at `orig_base+0x85684`), enables `proxy_sv_voidTimeNudge` by rcon, samples again and requires the median to move by the correction (about −15 ms). It then disables the feature again so the final netStatus table reports the range, and requires the client's scored row to still show the real −15 (`inside yes`) with no `timeNudge back to zero` line in between (the estimator never converged). |
| `a4_ignore.sh` + `a4_ignore.py` | D-016 | End-to-end check of the player `/ignore` command: three synthetic clients (Ignorer, Speaker, Witness) over the A2 `ClientModel`; the Ignorer sends `ignore <name prefix>`, the Speaker says a marker line, and every `svc_serverCommand` is captured. Requires the ignoring client to get the confirmation and miss the chat while the Speaker keeps its own echo and the Witness receives it; `--mode enabled` then sends `unignore` and requires delivery back, `--mode disabled` requires the `proxy_sv_enableIgnore 0` reply and no filtering. |
| `a5_coinflip.sh` + `a5_coinflip.py` | D-024 | End-to-end check of `callvote coinflip`: two synthetic clients join the FFA, the caller starts the vote and the other votes yes. Requires the normal vote configstrings (`CS_VOTE_STRING` = `coinflip`, yes/no tally), the `VOTEPASSED` message, and the final `svsay` chat (`Server: Coin flip: Heads!` or `Tails!`). |
| `a6_tffa_spectate_join.sh` + `a6_tffa_spectate_join.py` | D-006.17 | End-to-end check of a team request from a TFFA spectator: four synthetic clients on `g_gametype 6`; two create matches (one open, one password-protected), two follow one player each, then `join red` / `team red`. Requires the followers to move into the watched match (`tffa_list` counts), the open one to join its red team, the protected one to receive the `tffa_join <id> <password>` invite (then unlock and join), and a second immediate `join` to hit the base 5-second team-switch debounce. |
| `a8_ping_equalize.sh` + `a8_ping_equalize.gdb` | D-028 | End-to-end check of ping equalisation: two synthetic clients with different fixed one-way delays (`--mode equalize`, `a2 --delay-ms/--slow-delay-ms`); samples both `level.clients[i].ps.commandTime - level.time`, `ps.ping` and engine `cl->ping` at `SV_SendClientMessages` (after the proxy's post-forward `ps.ping` write, before the snapshot). Verifies `ps.ping == cl->ping` with the feature off (the `OFFSET_PS_PING = 524` assumption), then enables `proxy_sv_pingEqualize` by rcon and requires the fast client's shift to move by `-(target - ping)` and its `ps.ping` to read the target while the slow client is untouched, then sets the cap to 0 and requires the ramp back and the log release line. |
| `a9_ping_equalize_real.gdb` | D-028 | Real-client sampling helper: attaches to a live server and samples **one** game client's `ps.commandTime - level.time`, `ps.ping` and engine `cl->ping` once per `SV_SendClientMessages` (`SLOT`, default 2: two synthetic anchors occupy 0/1). Used to validate the queue delay against a real cgame's prediction after the stamp-shift mechanism was disproved (D-028.8). |
| `a10_duel_lone_notice.sh` + `a10_duel_lone_notice.py` | D-025 §6 | End-to-end check of a lone duel-ladder player: one synthetic client on `g_gametype 3` with `proxy_parallel_duels_enable 1` waits in the arena like regular duel (`t=0` in `CS_PLAYERS`, the stock `AddTournamentPlayer` pull let through); a second client forms the round (both in the arena); a late third stays a spectator and its explicit `team f` gets exactly one court-assignment notice (the debounced repeat none); the "waiting for at least two willing players" line appears once (a roster change), not on a timer. Before the message fixes the same run counted 97 notices while alone at `sv_fps 20` and two waiting lines in 9.5 s. |
| `a11_cs_players_name.sh` + `a11_cs_players_name.py` | D-006.18 | End-to-end check of the `CS_PLAYERS` raw name bytes: a client with a legal 34-byte latin-1 name (32 ASCII + two `0xE9`) creates a TFFA match and a second client follows it, forcing the proxy to re-send the stored base. The captured `cs CS_PLAYERS` base must be byte-identical to the 34 raw bytes (no U+FFFD from a lossy UTF-8 decode) and at most `MAX_NETNAME-1` (35) bytes; pre-fix the lossy decode sent 38 bytes containing `EF BF BD` and the client reported `Com_sprintf: Output length 38 too short, require 41 bytes.` |

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
