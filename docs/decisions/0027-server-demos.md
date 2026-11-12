# D-027 — Server-side demos and publication

Date: 2026-11-12. Status: Phases 1–4 implemented; Phase 5 (this record and the
human-facing documentation) done; Phase 6 not started. Scope: `rust/src/demo/`,
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
  and a source audit (§11), **RUNTIME** by real clients continuing to
  receive snapshots while a recording is active.

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
on TaystJK and a stock client. The 32-slot self-view corner is not exercised
end-to-end (U-015): a POV change continues while the old subject's slot stays
empty (the gamestate names an empty slot); if the slot is reused, the watchdog
closes the segment and a new file starts with the new subject.

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
while the filesystem is short, resuming when it recovers. The size cap's
runtime trigger was not reached before synthetic subjects died (U-014, unit
test only); retention and the free-space guard were forced at runtime.

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

## 6. Verification status

- **TESTED (host `cargo test`)**: demo naming/retention/segments, gamestate
  encoder against the Phase 0 fixture, `MsgWriter` bit/byte conventions,
  size-cap threshold, chat rule, announcement/link shape, HTTP request parsing
  and traversal, JSON listing shape/escaping, end-to-end HTTP server (GET/HEAD,
  listing, open 404, 405/431/501, symlink escape, shutdown).
- **RUNTIME (real 1.011 engine)**: 49-demo framing/round-trip; Phase 2 gate A;
  Phase 3 gate B (TFFA, duel, lifecycle, chat); Phase 4 gate C (HTTP, JSON,
  retention, free-space, negatives); intermission tail and announcement;
  min-free pause/resume. Artifacts under `artifacts/phase3-*`,
  `artifacts/phase4*`, `artifacts/demo-*`.
- **OWNER**: the Phase 2 moving artifact plays and ends on TaystJK and a stock
  client (gameplay, HUD, followed model). Phase 3 artifacts were loaded by the
  real client to completion without parse errors but **not visually verified**;
  Phase 4 artifacts were validated, not played.
- **Open**: RMG maps and vehicle playerstate (U-010), switched-stream playback
  (U-011), TAB board playback (U-012), size-cap runtime trigger (U-014), the
  live 32-slot corner (U-015), client receipt of the intermission block
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
