# D-027 — Server-side demos and publication

Date: 2026-11-12. Status: Phases 1–4 implemented; Phase 5 (this record and the
human-facing documentation) done; Phase 6 (validation and release gate) done
without feature changes. Scope: `rust/src/demo/`,
`rust/src/http/`, `rust/src/state.rs` demo cvars, `rust/src/shared_api.rs`
(`/demos`), `rust/src/hooks/game.rs` (`BeginIntermission`), `csrc/syscall_shim.c`
(`statvfs`), `rust/scripts/run-server.sh`.

Evidence levels used here: **SOURCE** (source code), **STATIC** (shipped
binary), **RUNTIME** (real engine/container run), **TESTED** (host unit test),
**OWNER** (owner's visual playback), **INFERRED**. The measurements and the
raw wire-format work are in `docs/demo-format.md` §1–§13; this record separates
what was recovered from what was decided.

## 1. What was recorded, and how the format was recovered

**STATIC — reference client.** The container was recovered from the shipped
reference client's symbols and the 49 shipped demos, and cross-checked against
the client recording code in `original/jedi-academy/codemp/client/` where the
source is present:

- `CL_WriteDemoMessage` (`0xb5ac0`): `int32 serverMessageSequence`,
  `int32 (msg->cursize - headerBytes)`, then the payload (`msg->data +
  headerBytes`), i.e. the server message with the netchan header removed and
  the XOR layer undone.
- `CL_StopRecord_f` (`0xb4100`): two `-1` words, the file terminator.
- `CL_ReadDemoMessage` (`0xb5dc0`): sequence, length, `CL_ParseServerMessage`;
  a length of `-1` ends the file, and sequence gaps are legal.
- `CL_Record_f` (`0xb4c80`): builds the first (gamestate) frame, including
  `svc_gamestate`, `svc_configstring`, `svc_baseline`, `svc_EOF`, `clientNum`,
  `checksumFeed`, the RMG short and the trailing `svc_EOF`.

**TESTED — the 49 shipped demos** all pass the framing check (terminator,
increasing sequences, no trailing data) and decode to **817,600 frames with 0
errors**; 49/49 gamestates re-encode meaningful-byte identical from their
decoded values. The Phase 0 fixture
`tests/fixtures/demo/server-ffa3-phase0.dm_26` and its
`.gamestate.bin` sidecar pin the layout for the Rust test (the Rust encoder is
compared against it, not against a second parser).

**STATIC + RUNTIME — protocol 26, not the source tree's 25.** The shipped
server compares the connect protocol with `0x1a` (`0x804c078`) and builds the
`protocol` cvar from the same constant; a pristine server reports
`\protocol\26`, refuses a protocol-25 connect with “Server uses protocol
version 26.”, and the reference client's `CL_Record_f` hard-codes the `.dm_26`
extension. `qcommon.h` says `PROTOCOL_VERSION 25`; that tree is not the source
of the shipped binary for this constant. Conclusion: `.dm_26` and protocol 26.

**RUNTIME — gamestate shape.** A captured server gamestate matches the
reference layout: configstrings, baselines, `svc_EOF`, `clientNum`,
`checksumFeed`, `int16 0` (RMG null) and the trailing `svc_EOF`.

**STATIC — the RMG block is compiled in even though the source comments it
out.** `SV_SendClientGameState` (`0x804cee4`) contains the variable block
(two deflated heightmaps, a seed and `SV_WriteRMGAutomapSymbols`) when
`TheRandomMissionManager` (`0x8394b50`) is non-null, and `MSG_WriteShort(0)`
otherwise. All captured normal maps take the `int16 0` path. The proxy's
encoder only implements that path; on an RMG map the recorder logs
`RMG gamestate tail unsupported` and starts no segment (U-010).

**RUNTIME — `PMF_FOLLOW` from a real spectator demo.** A client-recorded
following-spectator demo carries the watched player's playerstate with
`PMF_FOLLOW` (0x1000) set and `ps.clientNum` = the watched slot, while the
gamestate `clientNum` is the spectator's own slot. The recorder reproduces the
first half and needs its own viewer-identity rule for the second (§3).

## 2. No fake client: shadow state and the engine's own encoders

The recorder never registers a client, never allocates a `client_t`, and never
mutates `svs.clients`. **Conclusion:** a real client slot would need a
`client_t` the engine maintains (userinfo, netchan, snapshots, downloads,
auth); faking one would collide with `sv_maxclients`, the real client on that
slot, and the engine's netStatus/download paths. Instead the recorder keeps, in
proxy memory:

- its own 32-slot `clientSnapshot_t`-shaped ring (frame/entity delta bases),
  two 1024-entry `entityState_t` scratch slots, its own
  `snapshotEntityNumbers_t`, a 16 KiB `msg_t` buffer, a reliable-command queue
  and a configstring mirror;
- **the engine's own MSG encoders** (`MSG_Init`, `MSG_WriteByte/Long/Data/Bits`,
  `MSG_WriteDeltaPlayerstate`, `MSG_WriteDeltaEntity`) through engine pointers,
  against `sv.svEntities[n].baseline` for first appearances. The recorder
  therefore emits the same wire bytes the engine would, not a second
  implementation of the delta rules;
- no copy of the engine's `svs.snapshotEntities` /
  `svs.nextSnapshotEntities` ring. Writing there is shared global state used by
  `SV_BuildClientSnapshot`/`SV_EmitPacketEntities` for real clients: the
  recorder's frame would corrupt or interleave the engine's own packet-entity
  lists (and the engine's free-index cursor). The recorder's scratch slots and
  ring are private; the only engine writes are `sv.snapshotCounter` and the
  per-entity `svEntities[n].snapshotCounter` dedupe token that
  `SV_BuildClientSnapshot` writes itself. **TESTED** (host) by layout assertions
  and a source audit (`docs/demo-format.md` §11), **RUNTIME** by real clients
  continuing to receive snapshots while a recording is active.

**Phase 6 audit — no writes to the engine's snapshot rings or client array.**
The demo module never mentions `svs.snapshotEntities` or
`svs.nextSnapshotEntities`: their offsets are not imported anywhere under
`rust/src/demo/`, so no code path can address them. `svs.clients` is only read
(its pointer through `OFFSET_SVS_CLIENTS`, then `client_t` fields for slot
state/userinfo/PS). The only writes to engine memory in `recorder.rs` are the
two `snapshotCounter` dedupe tokens above; `write_i32_bytes` targets the
recorder's own local `ps` buffer. Real clients kept receiving normal snapshots
in every Phase 6 run while one to four recorders were active — the failure mode
this rule avoids (interleaved packet-entity lists) did not occur.

**RUNTIME — bug 1: `movaps` on the engine's stack.** A recorder tick that
reached `std::fs::create_dir_all` faulted with `SIGSEGV` at
`movaps %xmm0,-0x68(%ebp)` in `std::sys::pal::unix::fs::try_statx`
(gdb-confirmed): the precompiled i686 std assumes a 16-byte-aligned stack, the
engine enters the module with 4-byte alignment (D-001.1). The fix is the C
`jampgame_aligned_entry(fn, op)` shim; every recorder entry (tick,
`GAME_INIT`, `GAME_SHUTDOWN`, `BeginIntermission`) runs through it. This is
also why the Phase 4 HTTP listener and maintenance thread are started from
those aligned entries.

**RUNTIME — bug 2: byte-boundary frame truncation.** The engine's
`MSG_WriteBits` ends with `msg->cursize = (msg->bit >> 3) + 1`, reserving the
byte the next symbol would start in. `MsgWriter` originally emitted
`ceil(bits/8)`, so a message whose last symbol ended exactly on a byte
boundary was one byte short; the client's trailing `svc_EOF` read returned
`-1` and `CL_ParseServerMessage` rejected the frame. It occurred in roughly one
segment in eight (payload 4,883 B where 4,884 B were required). `MsgWriter` now
reserves the byte exactly like the engine; a test covers all 256 Huffman
symbols and
`artifacts/soak-fixed/demos/20261111-214019_mp_ffa3_main_s1.dm_26` is a real
byte-boundary gamestate that re-encodes identically.

## 3. Subject and viewer identity

**Decision — subject.** The POV is the lowest eligible human of the recorder's
instance: in the world (`client_t.state >= CS_ACTIVE`), has a `gentity`, not on
`TEAM_SPECTATOR`, not a bot. Bot detection uses the engine's
`gentity->r.svFlags & SVF_BOT` plus the proxy's connect mirror (Phase 4 found
that `addbot` bots get the flag but never pass through `GAME_CLIENT_CONNECT`;
the engine flag is authoritative). A subject that leaves, joins spectators or
moves to another instance is replaced by the lowest eligible player of the same
instance **inside the same file** (spectator semantics: the next snapshot's
`ps.clientNum` changes; `PMF_FOLLOW` is set when viewer ≠ subject). An empty
instance closes the segment; repopulation starts the next segment number.

**Decision — viewer identity.** The gamestate `clientNum` (what cgame treats
as “me”) must never name a player other than the followed one:

1. `sv_maxclients < MAX_CLIENTS`: the highest slot the server never assigns
   (`MAX_CLIENTS - 1`, usually 31), a spectator-follow stream.
2. All slots assignable: the highest **free** slot at segment start, plus a
   per-tick watchdog; if a client takes it, the segment closes before a frame
   with the collision is written.
3. Full 32-slot server: the subject's own slot (self-view, no `PMF_FOLLOW`).

**RUNTIME — why.** Using the initial subject's slot as the gamestate
`clientNum` made a player who reconnected there the demo's “me”; the newcomer
appeared fused with the followed player (observed in Phase 2). The spare-slot
rule removes the collision; the watchdog covers slot reuse. The **OWNER**
confirmed the moving Phase 2 artifact (clientNum 31) plays and ends correctly
on TaystJK and a stock client.

**RUNTIME (Phase 6) — the live 32-slot corner.** On `sv_maxclients 32`, 30
bots plus one human left one free slot; the recorder started a follow stream
naming it (`viewer 4`, `subject 3`). Adding the 32nd client took that slot:
`demo viewer slot 4 taken by client 4; closing …` closed the segment before a
colliding frame was written, and the next segment started as a self-view
(`subject 3, viewer 3`; decoded `clientNum 3`, `pm_flags 576`, no
`PMF_FOLLOW`). The watchdog file decodes as `clientNum 4`, `pm_flags 4672`.
Both files reached `DISCONNECTED` in TaystJK without truncation or parse
errors; the camera/HUD at
the switch were not visually watched, so U-015 stays open only for that visual
check.

## 4. Instances, content and chat (Phase 3)

**Decision.** One recorder per populated instance: `main` when instancing is
off, `tffa<N>` per parallel TFFA match (membership from
`tffa.client_match`), `duel<N>` per duel court (`Court::players()`); labels are
1-based. A recorder's build uses the existing instance compaction
(`snapshot_keep`/sound attribution) and the `recorder_build` flag skips the FOV
cap and anti-wallhack for the recorder; no separate visibility filter exists.
The instance's state is mirrored as its view: `CS_PLAYERS` through
`instances::rewrite_cs_players_for_slot`, `CS_SCORES1/2` and the duel HUD from
`instances::view_scores`/`duel::court_hud`, with `taystJKinfo` stripped from the
recorded serverinfo.

**Decision — scoreboard.** A demo viewer cannot request a TAB board (cgame skips
`scores` in demo playback), so the recorder calls the game's
`DeathmatchScoreboardMessage(subject)` once per second, captures its `scores`
trap instead of forwarding it, and rewrites the text for the instance with the
same viewer function the live path uses (`tffa::rewrite_scores_for_viewer`,
`duel::rewrite_scores_for_viewer`). The text is the stock **14-int** layout;
`taystJKinfo` bit 7 is removed from the recorded serverinfo so stock and
TaystJK cgames read the same stride. **TESTED** (layout/parser unit tests,
per-viewer rewrites) and **RUNTIME** (boards in the gate artifacts); no client
has pressed TAB on a recorded board (U-012).

**Decision — chat (owner, 2026-11-12).** Broadcast only: the subject's copy of
`chat`/`lchat` (the game sends one copy per client; `G_Say`/`G_SayTo`), never
team chat (`tchat`/`ltchat`), never a private `tell` (recognised by the
`ESC '['` name marker) and never a per-client `print`. The rule lives in
`recorder::chat_command_for`. **TESTED**; **RUNTIME** in the TFFA/chat gate
(broadcast present, team chat and `tell` absent). U-013 is resolved.

**Decision — lifecycle and bounds.** `GAME_INIT` (map change and
`map_restart`), shutdown, the recording switch going off, an empty instance and
the guards close segments with `-1/-1`; segment numbers reset at `GAME_INIT`.
At most 16 recorders run (one line when the limit is hit); the per-instance
cleanup keeps the game running.

## 5. Publication (Phase 4)

**Owner decision B — the proxy hosts the HTTP server.** There is no
external-hosting mode. `proxy_sv_enableHttp` + `proxy_sv_enableDemoDownload`
start a listener (bind `proxy_sv_httpBind`, default `127.0.0.1`; port
`proxy_sv_httpPort`, default `8090`); with either download route disabled the
listener is stopped, so OFF means no socket. `proxy_sv_demoUrl` is only the
link base: when empty the listener's own `http://<bind>:<port>` is used (a
wildcard bind is shown as `127.0.0.1`). The API is **plain HTTP**; TLS and
authentication belong to a reverse proxy.

Routes (GET/HEAD; `Connection: close`, no keep-alive, no `Range`):

| route | result |
| --- | --- |
| `/demos` | JSON array `{name, url, size}` for servable files (`application/json`) |
| `/demos/<file>` | the file (`application/octet-stream`, `Content-Length`, HEAD headers only) |
| `/api…` | `501` (reserved) |
| anything else | `404`; non-GET/HEAD `405` |

Path safety: percent-decode before routing, only names accepted by
`DemoName::parse`, `canonicalize`d inside the demo directory (symlink escape
rejected). The canonicalise→open TOCTOU window is accepted. A segment is
served only after it is closed **and** ends with `FF×8`; the open set is
published by the recorder. Request headers are capped at 8 KiB; four workers
with an 8-slot queue answer, then `503`; read/write timeouts are 5 s/15 s.
A bind failure (EADDRINUSE/EACCES/invalid port) logs one line and leaves the
feature off until the configuration changes; it never blocks or crashes the
game. **RUNTIME** verified with curl across the Phase 4 runs (Gate C, then the
final JSON/listing re-run): JSON, GET/HEAD, byte-identical SHA-256,
traversal/encoded traversal/foreign/decoy `404`, `/api` `501`, oversized
`431`, symlink `404`, 8 concurrent `200`, 20 concurrent → 12×`200` + 8×`503`,
disable → connection refused, port in use → feature off.

**Decision — `/demos` client command.** The in-game command (intercepted in
`shared_api::client_command`) prints the current generation's closed, still
existing files with their links, up to 32, or one “downloads disabled” line.
It is a `print`, so it is never recorded. **RUNTIME** verified server-side (the link log); no clean client capture of
the in-game reply was obtained with the synthetic tool.

**Decision — retention and accepted guards.**
`proxy_sv_demoRetentionHours` (24; `0` = keep forever) sweeps the demo
directory on the maintenance thread at start and each `GAME_INIT`: only
`DemoName::parse` names older than the window are deleted, open segments are
skipped, foreign files and directories are untouched, and one summary line is
logged. `proxy_sv_demoMaxFileMB` (128) closes a segment at the inclusive MiB
cap, warns once per generation and starts the next segment number on the next
tick. `proxy_sv_demoMinFreeMB` (512) closes all segments and suppresses starts
while the filesystem is short, resuming when it recovers. **RUNTIME (Phase 6) —
size cap:** with the cap at `1` MiB, a real client as subject and `sv_fps 125`,
the log showed `demo segment reached proxy_sv_demoMaxFileMB 1 MB (1048639
bytes) — closed …_s2`, `…_s3` started on the next tick, and both segments
played to the end. U-014 is resolved (the 1,048,647-byte file on disk is the
1,048,639 bytes counted when the cap fired plus the 8-byte `-1/-1` terminator
written while closing the segment). Retention and the free-space guard were forced at runtime in Phases
4–6 (see §6).

**Phase 6 defaults-off behaviour.** The maintenance thread now exists only
while a demo feature is on (`proxy_sv_enableDemoRecord` or
`proxy_sv_enableDemoDownload`); with every demo cvar at its default it is
stopped, so no directory is scanned and no sweep can delete anything. The
regression suite also asserts that no `proxy_sv_demoRecordEmpty`-style
recording switch is registered: production cannot record an empty server (a
human subject is required).

**Decision — intermission and the client block.** `BeginIntermission` starts
`proxy_sv_demoIntermissionSecs` (3 s, `0` = immediate). The recorder keeps
writing through the intermission so the demo shows the final `scores` and the
intermission screen (`ps.pm_type = PM_INTERMISSION`), then closes the segments
and sends the links to the clients with `print` (console text, the end-game
stats mechanism, never `svsay`, never the server console): header, at most 12
links, then a `/demos` hint. The generation is marked announced so the next
`GAME_INIT` does not repeat it; if the map leaves before the timer, the block
is queued for the next game start. **RUNTIME** verified server-side and in the
demo (`pm_type 7` for the last 3 s, final `scores` present, debug lines
`demo.intermission`); the synthetic capture tool did not produce a clean client
stream across that map restart, so client receipt is open (U-016).

## 6. Phase 6 validation and release gate

Phase 6 changed no feature behaviour. The temporary Phase 2 no-players switch
was already gone; Phase 6 adds a regression test that no such cvar is
registered and stops/starts the maintenance thread only with a demo feature
(`record || download`), so all-default-off starts no thread, scans no
directory, opens no socket and writes no file.

### 6.1 Build and defaults

- **TESTED** — `cargo fmt --check`, clippy for `i686-unknown-linux-gnu` with
  `-D warnings`, the full suite (521 tests) and a release build
  (`jampgamei386.so` SHA-256 `8f6733cd43affae5efeeca425389e3ca6c59d3b372ae7e6b7191ede950ea7bec`).
  Stop the runtime containers before rebuilding: the staged module is
  bind-mounted, and an in-place copy truncates the mapped file (all such
  servers die with SIGSEGV; not a proxy fault).
- **RUNTIME — all demo cvars at their defaults (off)**, final module in the
  shipped `linuxjampded` (container `jka-p6def`, port 29290): no demo/HTTP/
  maintenance log line, no `demos/` directory created, TCP 8090 refused,
  `getchallenge`/`getinfo` answering, stable at ~1.9% CPU. The cvar table's 11
  demo defaults are asserted by `default_cvar_values_match_proxy`.

### 6.2 Ground truth — recorder vs a real following spectator

- **RUNTIME.** A real TaystJK client joined as spectator, followed the recording
  subject (`Subject`, slot 0) and recorded its own client demo. Both demos share
  120 ticks (411750–417700):
  - 120/120 ticks: `ps` field-by-field identical — 37/37 fields, zero
    differences (including `pm_flags 4672`, i.e. `PMF_FOLLOW`),
  - identical entity-number sets on every tick,
  - the only difference is the gamestate `clientNum`: the recorder names its
    spare viewer slot (31), the real spectator names itself (1). That is the
    documented viewer-identity rule (§3), not a POV difference.
- This is the strongest available evidence that the recorded POV is the real
  following-spectator POV, not a synthesised camera.

### 6.3 Playback matrix

- **RUNTIME (load), final build.** Every file below was opened by TaystJK
  (`+demo`, real renderer) and reached `DISCONNECTED` with no truncation,
  illegible-frame or parse error:

| case | file | notes |
| --- | --- | --- |
| FFA | `20261112-163719_mp_ffa3_main_s2` | human subject, follow stream |
| team (GT_TEAM) | `20261112-164337_mp_ffa3_main_s1` | RED/BLUE, follow stream |
| CTF | `20261112-164424_mp_ctf1_main_s1` | RED/BLUE, follow stream |
| stock duel | `20261112-164500_mp_duel1_main_s1` | `g_gametype 3`, ladder |
| parallel duels | `20261112-164614_…_duel1_s1`, `20261112-164621_…_duel1_s1`, `…_duel2_s1` | two courts, round change |
| parallel TFFA | `20261112-164538_mp_duel1_tffa1_s1`, `…_tffa2_s1` | one file per match |
| POV switch | `20261112-170744_mp_ffa3_main_s3` | `ps.clientNum` 0→1 in one file |
| empty/repopulate | `20261112-164722_mp_ffa3_main_s2` | closes on empty, new segment on repopulate |
| 32-slot watchdog | `20261112-172546_mp_ffa3_main_s1` | viewer slot taken, closed |
| 32-slot self-view | `20261112-172559_…_s2`, `20261112-172710_…_s3` | `subject == viewer` |
| size-cap + continuation | `20261112-173912_…_s2` (1 MiB), `20261112-174111_…_s3` | rotation at the cap |

- **Not visually watched:** this environment renders through Wayland and the X11
  capture sees another window, so camera/HUD/scoreboard appearance was not
  observed. Stock 1.01 playback is not available here (only the TaystJK
  reference client is installed). U-011/U-012/U-015 remain open for those
  visual checks; loading and stream termination are verified.

### 6.4 Performance (numbers)

- **RUNTIME**, GT_TEAM with 4 bots, `sv_fps 20`, 10 s windows: CPU 2.5% with 0
  recorders, 2.5% with 1, 3.5% with 2, 4.2% with 4 (container `/proc/1/stat`
  utime+stime); `getinfo` round-trip 0.14–0.30 ms throughout.
- With 4 recorders: 23–121 KB per file over 42 s, 20.0 frames/s measured from
  the snapshot timestamps; closing the four segments flushed 126,911 bytes.
- HTTP while the server was busy: 20 concurrent GETs of an 868,892-byte demo
  gave 16×`200` (byte-identical SHA-256) and 4×`503` (worker/queue bound);
  `getinfo` stayed at 0.17–0.29 ms with one UDP timeout at a 2 s deadline
  during the burst.
- **Engine-state audit:** demo code does not reference
  `svs.snapshotEntities`/`nextSnapshotEntities` and only reads `svs.clients`
  (§2); the two `snapshotCounter` writes are the engine-compatible dedupe
  tokens. Real clients kept receiving snapshots with 1–4 recorders active.

### 6.5 Soak

Container `jka-p6soak` (port 29320), final build: recording on with two
subjects, a 4→5 subject switch inside one file, empty close, repopulate, full
`map` and `map_restart`, HTTP JSON/GET `200`, download off → refused with the
listener stopped and back on → `200`, missing demo directory → one
`directory … is not readable` line and no crash, min-free forced → paused +
closed then resumed with `s2`, port 8194 already held → one `cannot bind …
Address already in use` line and the feature off, restore `200`. No crash, no
stuck listener (the restored port answered), file handles 41 before → 40 after
with no demo file descriptor left open. Retention decoys: an old
(`20200101…`) matching file was deleted, a future (`20400101…`) matching file
and `foreign.txt` were kept (`deleted 1, kept 6`). The future/old pair
simulates a forward and a backward clock step for the retention decision; a
real `CLOCK_REALTIME` jump was not performed (the container lacks
`CAP_SYS_TIME`). The size cap was forced separately (§5): 1,048,639 bytes, log
line, next segment, both playable.

### 6.6 32-client corner

- **RUNTIME.** `sv_maxclients 32` with 30 bots + one human (31 clients): the
  recorder started with `viewer 4` (the last free slot); the 32nd client (a
  bot) filled it and the watchdog logged `demo viewer slot 4 taken by client
  4; closing`, closing the segment; the next segment recorded the human as a
  self-view (`subject 3, viewer 3`) on a full server. The watchdog and
  self-view files load to the end in TaystJK. Playback appearance is not
  visually watched (U-015).

### 6.7 Status

- **Resolved by Phase 6:** size-cap runtime trigger (U-014); the live 32-slot
  self-view/watchdog mechanics (U-015 runtime part); the POV ground-truth
  question; defaults-off regression; maintenance-thread gating.
- **Still open (none blocks a feature-complete release, but they are not
  verified):** RMG maps and vehicle playerstate (U-010 items 1–2), visual
  playback of switched/32-slot/scoreboard streams (U-011, U-012, U-015 visual),
  stock 1.01 playback (U-010 item 4), client receipt of the intermission block
  (U-016).

## 7. Owner decisions recorded

1. The proxy hosts the HTTP server; `proxy_sv_enableDemoDownload` really stops
   the listener (no socket when no route is enabled). No external hosting.
2. `proxy_sv_demoUrl` is a link base, not a hosting switch.
3. Downloads are opt-in; bind defaults to loopback.
4. Chat in demos is broadcast-only (U-013 resolved).
5. Guards accepted: retention 24 h, max file 128 MB, min free 512 MB (`0`
   disables each; retention `0` = keep forever).
6. End of game: record 3 s of intermission by default, then send the client
   block; fallback to the next game start if the intermission is shorter.
7. `/demos`: JSON over HTTP, links in game.

## 8. Recorded limitations

- The demo is the **subject's view**, not a free camera; the POV is a human
  player in the world.
- No team chat, private `tell` or per-client `print` in a demo.
- RMG/terrain maps are refused (no segment); vehicle playerstate frames are
  discarded (the stream resumes after the vehicle).
- The map and its assets must be installed on the client that plays the demo;
  the `.dm_26` does not carry them.
- The stock `/download`/`sv_allowDownload` path serves files from the engine's
  game filesystem, not the proxy's demo output directory; the proxy's HTTP
  `/demos/<file>` is the download path.
- With `sv_maxclients 32` there is no spare viewer slot: the demo is a
  self-view. A POV change continues while the old slot stays empty; if the
  slot is reused, the watchdog closes the segment (new file).
- The API is plain HTTP with no built-in TLS or authentication; use a reverse
  proxy.
- A console `set proxy_sv_demoUrl http://…` truncates at `//` (the engine's
  command tokenizer); set it from a cfg.

## 9. Implementation constraints

- HTTP and maintenance threads never call engine functions or read engine
  memory: they get a config snapshot and the proxy-memory open-name set.
- All std reached from the game thread (bind, thread spawn, mutex, `statvfs`)
  runs through the C realign shim; filesystem scans run on the maintenance
  thread.
- Only terminated files are served; pattern names only; symlink escapes
  rejected; the TOCTOU window is accepted.
- The size cap is inclusive MiB and rotates; the free-space guard pauses and
  resumes.
- A console `set` of a URL is unsafe (`//` comment); `run-server.sh` writes the
  value into an exec'ed cfg.
