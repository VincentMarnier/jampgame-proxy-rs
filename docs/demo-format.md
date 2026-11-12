# Server-side demo recording — format and engine evidence (Phase 0)

Status: Phase 0 of the `proxy_sv_enableDemoRecord` plan. No recorder code yet.
This document records what was *observed*, *read from the binaries/source* and
*validated*, and separates that from what is still open.

Evidence labels used below: **SOURCE**, **STATIC**, **RUNTIME**, **TESTED**,
**INFERRED**. See `docs/README.md`.

## Artifacts and tools

- Reference client (not stripped): `original/GameData/taystjk.x86_64`.
- Shipped engine (stripped, 1.011): `original/jalinuxded_1.011/linuxjampded`.
- Engine source (reference, with drift — see §7):
  `original/jedi-academy/codemp/`.
- Real client demos: `original/GameData/taystjk/demos/*.dm_26` (49 files).
- Runtime: `jampgame-investigation:i386-bookworm` (see
  `docs/runtime-environment.md`).
- `tools/runtime/demo_capture.py` — connects a synthetic protocol-26 client and
  writes the received server messages in demo framing (the first frame is the
  server's real gamestate).
- `tools/demo_inspect.py` — validates the framing, decodes the gamestate and
  snapshots, and can re-encode a gamestate byte-for-byte from its decoded
  values (`--verify-gamestate`).
- `tests/fixtures/demo/server-ffa3-phase0.dm_26` — the Phase 0 server capture
  (see its README), SHA-256
  `d59520264fe5e2f75554c4a0b4bead9d574a6bd8774ac5c721d664206f274ce0`.

`objdump -d` in this environment prints the raw bytes of this stripped i386
binary instead of decoding `.text`; `objdump -D -j .text` (the form used by
`tools/sweep_engine_addrs.py`) does decode it. The disassembly below is
reproducible with either that form or `gdb -q -batch -ex 'file …' -ex
'disassemble …'`.

## 1. Protocol version is 26 (source tree says 25)

**STATIC (binary):** the shipped server compares the connect protocol against
`0x1a` (26) at `0x804c078` (`cmp $0x1a,%ebx`) and builds the `protocol` server
cvar from the same constant in the `SV_Init` path around `0x804c020`–`0x804c0aa`.

**RUNTIME:** a pristine server started with `+map mp/ffa3` reports
`\protocol\26` in its `getinfo` reply and in the gamestate configstring 0
(CS_SERVERINFO). A synthetic protocol-25 connect is refused:

```
OOB: print\nServer uses protocol version 26.\n
```

while the same client with `\protocol\26` receives the gamestate.

**SOURCE:** `original/jedi-academy/codemp/qcommon/qcommon.h:212` says
`PROTOCOL_VERSION 25`. That tree is not the source of the shipped 1.011 binary
for this constant.

Conclusion: the server protocol is **26**, so demo files use the **`.dm_26`**
extension and the recorder must not take the number from `qcommon.h`.
**STATIC:** `CL_Record_f` (`0xb4c80`) formats the extension from the literal
`26` at `0xb4d43` (`dm_%d`, string `0x185409`) and builds the path with
`demos/%s.%s` (`0x185488`); only the JK2 compatibility mode
(`protocolswitch == 2`) takes the 25/`.dm_` path.

## 2. Demo container framing

**STATIC (reference client):**

- `CL_WriteDemoMessage(msg, headerBytes)` (`0xb5ac0`, x86-64) writes, in this
  order:
  1. `int32` = `clc.serverMessageSequence` (`clc+0x20430`),
  2. `int32` = `msg->cursize - headerBytes`,
  3. `msg->data + headerBytes` for that length.
- `CL_PacketEvent` (`0xbc800`) obtains `headerBytes` from `msg->readcount`
  after `CL_Netchan_Process` and before `CL_ParseServerMessage`; the recorded
  payload is therefore the server message with the netchan header removed and
  the XOR layer undone — exactly the bytes `CL_ParseServerMessage` parses.
- `CL_StopRecord_f` (`0xb4100`) writes `0xffffffff` and closes the file; the
  shipped code path writes the four bytes twice, i.e. the `-1, -1`
  terminator.

**STATIC (reference client reader):** `CL_ReadDemoMessage` (`0xb5dc0`) reads
the sequence first, then the length, treats a length of `-1` as completion
(only the first `-1` is tested), and calls `CL_ParseServerMessage` with
`MSG_Init`/`MSG_Bitstream`. It does **not** validate that the sequence is
contiguous, so gaps are legal.

**TESTED:** all 49 shipped demos pass the framing check: terminators present,
sequences strictly increasing, no trailing bytes.

```
frame   := int32 sequence, int32 length, byte[length] payload
file    := frame*, int32 -1, int32 -1
```

Notes that matter for the writer:

- In the reference client the order is **sequence then length** (the Q3/OpenJK
  writer uses length-first); the 49 shipped demos confirm the reference order.
- Each payload is an independent `MSG_*` bitstream. Whole bytes are encoded as
  symbols of the **static** `msg_hData` Huffman tree: `MSG_initHuffman`
  preloads the compressor/decompressor trees and `MSG_WriteBits`/`MSG_ReadBits`
  only walk them (`Huff_offsetTransmit`/`Huff_offsetReceive` do not call
  `Huff_addRef` per message). There is no adaptive state to carry between
  frames. **SOURCE** + **TESTED** (all 49 demos decode from the initial tree).

## 3. First frame: the gamestate

The first frame is a complete gamestate message followed by `svc_EOF` and the
RMG tail. The server itself produces the same shape; `SV_Netchan_Transmit`
(`codemp/server/sv_netchan.cpp`, around the `MSG_WriteByte(msg, svc_EOF)` in
`SV_Netchan_Transmit`) appends `svc_EOF` to every server→client
message, and the client's `CL_Record_f` reproduces it.

**STATIC (reference client):** `CL_Record_f` (`0xb4c80`) builds the first frame
with `MSG_Init` + `MSG_Bitstream` and this call order:

```
int32  lastClientCommand          // CL_Record_f writes clc.reliableSequence
byte   svc_gamestate              // 2
int32  reliableSequence           // clc.serverCommandSequence
{ byte svc_configstring (3), int16 index, bigstring }*   // non-empty only
{ byte svc_baseline (4), MSG_WriteDeltaEntity(nullstate, baseline, force) }*
byte   svc_EOF                    // 10
int32  clientNum                  // the recording client's own slot
int32  checksumFeed
int16  rmg                        // 0 when TheRandomMissionManager is null
byte   svc_EOF                    // 10 (terminates the demo message)
```

**RUNTIME (captured server gamestate):** the same layout, from the pristine
1.011 server on `mp/ffa3`:

```
payload 4102 B, 62 configstrings, 49 baselines, clientNum 0,
checksumFeed 0xd8e9a7d8, rmg 0, tail svc_EOF (10)
```

All 49 real demos confirm: first op `2`, `rmg == 0`, and the trailing
`svc_EOF` (the re-encode below includes it and matches). The gamestate's
`clientNum` is the *recording client's* slot; snapshots carry the
*followed/playing player's* playerstate (see §5).

**RMG tail — binary vs source:** `original/jedi-academy/codemp/server/
sv_client.cpp` comments the RMG block out, but the shipped binary contains it
(`SV_SendClientGameState` `0x804cee4`):

- if `TheRandomMissionManager` (global pointer at `0x8394b50`) is non-null,
  write a variable block: deflate + `MSG_WriteShort(length)` +
  `MSG_WriteBits(1,1)` + `MSG_WriteData(heightmap)`, a second map, then
  `MSG_WriteLong(seed)` and `SV_WriteRMGAutomapSymbols` (`0x8085e54`);
- otherwise `MSG_WriteShort(0)` (`0x804d3a1`–`0x804d3b2`).

**RUNTIME:** on `mp/ffa3` the pointer is `0`, so the common tail is the `int16
0` above. **STATIC:** the reference client's `CL_ParseGamestate` (`0xbfae0`)
reads `MSG_ReadShort` right after `checksumFeed` (call at `0xbfbeb`), matching
the server. RMG/terrain maps would take the variable path; this is **not yet
covered** by the golden files (open item §8).

**Byte-level validation:** the decoder re-encodes the first frame from its
decoded values and compares the payload bytes. `--verify-gamestate` reports
49/49 demos **meaningful-byte identical** (the server capture and all shipped
demos; the meaningful range includes the trailing `svc_EOF`); four demos carry
one extra, uninitialised trailing byte from the client's stack buffer
(`CL_Record_f` writes `msg.cursize`; the byte beyond the final message bit is
never written), which is padding, not data. This is the Phase 0 gate for the
gamestate layout.

## 4. Snapshot frames

**SOURCE + STATIC:** `SV_SendClientMessages` (`0x8059034`; source
`codemp/server/sv_snapshot.cpp:829`) builds one message per client:

```
int32 lastClientCommand
[ svc_serverCommand (5), int32 sequence, string ]*   // reliable commands
byte  svc_snapshot (7)
int32 svs.time
byte  lastframe                    // delta base, 0 = no delta
byte  snapFlags
byte  areabytes
byte[areabytes] areamask           // each byte via MSG_WriteByte
MSG_WriteDeltaPlayerstate(...)     // 0x80799c4
[ vehicle playerstate delta ]*     // only when ps.m_iVehicleNum != 0
SV_EmitPacketEntities(...)         // 0x8057e64
[ optional voice-proximity block ] // XBOX-only opcodes 11..20; PC enum has none
byte  svc_EOF (10)                 // appended by SV_Netchan_Transmit
```

Decoder details recovered from the shipped binaries and validated against the
demos:

- `MSG_WriteDeltaPlayerstate` (`0x80799c4`) is the 4-argument
  `(msg, from, to, isVehiclePS)` form. The binary has
  `_OPTIMIZED_VEHICLE_NETWORKING`: in the **non-vehicle** path it writes a
  leading 1-bit *pilot playerstate* flag (1 = pilot/vehicle pilot table, 0 =
  normal). `SV_WriteSnapshotToClient` has five call sites, matching the source's
  main/vehicle branches (`0x8058132`, `0x805816f`, `0x8058198`, `0x8058203`,
  `0x8058236`).
- Playerstate field layout (per the 4-arg function): `byte lc` (number of
  fields to consider), then for each `i < lc`: 1-bit changed; when changed and
  `bits != 0` the value follows directly in `bits` bits (there is **no**
  zero/non-zero sub-bit here); when `bits == 0` (float): `0` + 13-bit biased
  integer, or `1` + full 32-bit value. Then the arrays block: one "any
  arrays" bit, and for each of stats/persistant/ammo/powerups a present bit, a
  16-bit mask and the values (`STAT_WEAPONS` in `MAX_WEAPONS` = 19 bits;
  powerups 32 bits; others 16).
- `MSG_WriteDeltaEntity` (`0x8078d74`) is `(msg, from, to, force)`. For each
  entity: 10-bit number, 1-bit removed, 1-bit has-delta; if delta, `byte lc`,
  then per field 1-bit changed and, for integer fields, a zero/non-zero bit and
  the value (float fields use the same 0 / 1+13 / 1+32 scheme). The packet
  entity list ends with the `MAX_GENTITIES-1` (1023) number marker.
- The recording client's own entity is **not** in the entity list.
  `SV_BuildClientSnapshot` (`codemp/server/sv_snapshot.cpp:532-601`) pre-marks
  `sv.svEntities[frame->ps.clientNum]` with `sv.snapshotCounter` ("never send client's own entity, because it can be
  regenerated from the playerstate"). For a following spectator,
  `frame->ps.clientNum` is the **followed player**, so that player's entity is
  absent; the cgame regenerates it from the playerstate. The real spectator
  demo confirms this: `ps.clientNum = 1`, `pm_flags = PMF_FOLLOW (4096)`, and
  entity `1` is absent from the lists. Phase 2 must reproduce this exclusion.
  **SOURCE:** `CG_AddPacketEntities` (`codemp/cgame/cg_ents.c:3412-3485`)
  regenerates that entity from the playerstate via `BG_PlayerStateToEntityState(ps,
  &cg_entities[ps.clientNum].currentState, …)` and adds it, which is why the
  server can omit it.

**TESTED:** `tools/demo_inspect.py --all-snapshots` walks every frame of all 49
shipped demos (817,600 frames, including the gamestate and every snapshot) and
reports **0 decode errors**. This exercises the snapshot header, playerstate,
entity and terminator layouts against real recordings.

## 5. Spectator demos and PMF_FOLLOW

`demo2025-11-27_21-56-38.dm_26` (SHA-256
`545bbaeb5f2dfd0c4d00397021857eb14aebbb45b6c5e3c623d775d355b053ed`) is a
real following-spectator recording:

- gamestate `clientNum = 5` (the recording client's own slot);
- first snapshot playerstate `clientNum = 1`, `pm_flags = 4096`
  (`PMF_FOLLOW`, `bg_public.h`), and entity 1 absent — the followed player is
  represented by the playerstate, exactly what the recorder will emit by
  copying the watched player's `ps` and OR-ing `PMF_FOLLOW` (the same thing
  `SpectatorClientEndFrame`, `codemp/game/g_active.c:3710`, does for a
  spectator).

A non-following golden recording for comparison:
`demo2025-09-12_13-17-36.dm_26` (SHA-256
`8ed1210b7f229b69bff0927fdb9f793dee0911fd452b6f1c6b682334a978cc10`).

The gamestate `clientNum` and the snapshot `ps.clientNum` are therefore two
different things; see the open item in §8.

## 6. Server frame order and the hook point

**SOURCE** (`codemp/server/sv_main.cpp:820-928`) and **STATIC** (shipped
binary):

```
SV_Frame (0x8057844)
  … svs.timeResidual / SV_BotFrame …
  while (residual >= frameMsec):
      svs.time += frameMsec
      VM_Call(gvm, GAME_RUN_FRAME, svs.time)      // 0x80579fe, VM_Call 0x80e1b34, arg 8
  SV_CheckTimeouts()                              // 0x8057a31 -> 0x8057614
  SV_SendClientMessages()                         // 0x8057a36 -> 0x8059034
  SV_CheckCvars()                                 // 0x8057a3b -> 0x80577b4
  SV_MasterHeartbeat()                            // 0x8057a40 -> 0x8056304
```

`SV_SendClientMessages` is called **after** the game simulation chunks and
after `SV_CheckTimeouts`, once per server frame. Hooking it and running the
recorder builds after the original returns gives a post-simulation,
post-engine-snapshot point without touching `svs.snapshotEntities` or any real
client's `clientSnapshot_t` ring. `svs.time` (`0x83121e4`) is the snapshot
clock; `snapshotMsec = 1000 / sv_fps`.

## 7. Pinned addresses (added to `rust/src/engine.rs`)

All within the executable LOAD segment `.text` = `0x0804a1e0 .. 0x08199494`
(`readelf -S`; enforced by the unit tests).

| symbol | address | role |
|---|---|---|
| `MSG_WriteBits` | `0x08077634` | bit writer (Huffman whole bytes; ends `svc_packetentities`) |
| `MSG_WriteDeltaPlayerstate` | `0x080799c4` | exact playerstate encoder (4-arg, pilot bit) |
| `SV_SendClientMessages` | `0x08059034` | recorder tick hook point (run after original) |
| `SV_Frame` | `0x08057844` | reference only |
| `SV_WriteSnapshotToClient` | `0x08058034` | reference only; layout replicated |
| `SV_EmitPacketEntities` | `0x08057e64` | reference only; replicated over the recorder's storage |

Already present and reused: `MSG_Init` `0x080774a4`, `MSG_Bitstream`
`0x080775d4`, `MSG_WriteByte` `0x08077a24`, `MSG_WriteShort` `0x08077aa4`,
`MSG_WriteLong` `0x08077ad4`, `MSG_WriteString` `0x08077b34`,
`MSG_WriteBigString` `0x08077c04`, `MSG_WriteData` `0x08077a54`,
`MSG_WriteDeltaEntity` `0x08078d74`, `SV_AddEntitiesVisibleFromPoint`
`0x08058404`.

## 8. Source-tree drift and open items

Source-tree drift found while pinning (the shipped binary wins):

| item | source tree | shipped binary / runtime |
|---|---|---|
| `PROTOCOL_VERSION` | 25 (`qcommon/qcommon.h:212`) | 26 (`cmp $0x1a`; serverinfo) |
| entity fields | 113 (`msg.cpp`) | **132** (server table `0x081a15a0`, client symbol `entityStateFields`) |
| playerstate fields | 113 normal / 116 pilot / 52 vehicle (`msg.cpp`) | **137** normal; vehicle **69**; pilot path uses the first **58** entries of the client's 140-entry pilot table (server writer count `0x3a`) |
| RMG gamestate tail | commented out (`sv_client.cpp`) | compiled in; `int16 0` when the manager is null |

`tools/demo_inspect.py` reads the field tables from the binaries themselves for
this reason (i386 `netField_t` = 12 bytes, bits at `+8`; x86-64 client entry =
24 bytes, bits at `+0x10`).

Open items (not blockers for the Phase 0 gate; to settle in Phase 1/2):

1. **RMG maps**: the variable gamestate tail when `TheRandomMissionManager`
   (`0x8394b50`) is non-null. No such capture exists. If the recorder is
   expected to support RMG maps, it must reproduce that block (engine helpers:
   deflate sequence at `0x804d137`–`0x804d242`, `SV_WriteRMGAutomapSymbols`
   `0x8085e54`) or record only the null case. The common case is settled.
   **Phase 1:** `demo::gamestate::encode_gamestate` refuses this case with
   `GamestateError::RmgUnsupported`; the Phase 2 caller must skip the segment
   (no wrong tail is ever written).
2. **Vehicle playerstate**: the 49 demos contain no non-zero
   `ps.m_iVehicleNum` (checked in the first 300 frames of each); the extra
   vehicle playerstate block is therefore not exercised by the golden files.
   Phase 2 should call the engine's 4-arg writer with `isVehiclePS = 1` for
   the vehicle block, as `SV_WriteSnapshotToClient` does.
3. **Gamestate `clientNum` for a server-side demo**: a real following-spectator
   demo writes the *viewer* slot there while snapshots carry the *subject's*
   `ps` (clientNum = subject, `PMF_FOLLOW`). The recorder has no real viewer
   slot; the choice (viewer-style slot vs subject slot) needs a playback check
   in validation gate A. The plan's "clientNum chosen for the demo viewer"
   leaves this open.
4. **First-frame `lastClientCommand`**: the demo's first long is
   `CL_ParseServerMessage`'s reliable-command acknowledge. `CL_Record_f` fills
   it with the client's own `clc.reliableSequence` (verified: `CL_AddReliableCommand`
   increments `clc+0x428`). The recorder has no client commands; `0` is the
   planned value and must be validated in gate A.
5. **Sequence numbering**: `CL_ReadDemoMessage` does not validate continuity;
   the recorder can use its own increasing frame counter.
6. **`/demos` when recording is disabled**, size/free-space guard values and
   chat-content scope remain the plan's assumptions (Phase 4), not Phase 0
   facts.
7. **Stock 1.01 client playback** was not run in this environment (no stock
   1.01 client binary and no display). The shipped 1.011 server writes the RMG
   short and the reference (TaystJK) client reads it; a stock client that did
   not read it would misparse the trailing bytes, so **INFERRED** it reads it
   too. Gate A must confirm playback in stock 1.01 and TaystJK.

## 9. Reproduce the evidence

```sh
# 1. pristine server (container image from docs/runtime-environment.md)
docker run -d --name jka-demo0 -p 29120:29120/udp \
  -v "$PWD/original/GameData":/srv/jka/gamedata:ro \
  jampgame-investigation:i386-bookworm \
  ./linuxjampded +set dedicated 1 +set net_port 29120 \
    +set fs_cdpath /srv/jka/gamedata +set sv_pure 0 \
    +set sv_maxclients 8 +set g_gametype 0 \
    +set timelimit 0 +set fraglimit 0 +map mp/ffa3

# 2. real gamestate + message stream (protocol 26; 25 is refused)
python3 tools/runtime/demo_capture.py 127.0.0.1 29120 \
  --out artifacts/demo-phase0/server-ffa3.dm_26 --duration 4
python3 tools/runtime/demo_capture.py 127.0.0.1 29120 \
  --out /tmp/opencode/proto25.dm_26 --duration 2 --protocol 25 --verbose

# 3. gamestate layout, meaningful-byte round-trip, full-frame decode
python3 tools/demo_inspect.py tests/fixtures/demo/server-ffa3-phase0.dm_26 \
  --frames 1 --verify-gamestate
python3 tools/demo_inspect.py original/GameData/taystjk/demos/demo2025-09-12_13-17-36.dm_26 \
  --all-snapshots        # any of the 49 shipped .dm_26 files

# 4. addresses / call sites
objdump -D -j .text --start-address=0x80799c4 --stop-address=0x8079a90 \
  original/jalinuxded_1.011/linuxjampded      # MSG_WriteDeltaPlayerstate
objdump -D -j .text --start-address=0x8057844 --stop-address=0x8057a45 \
  original/jalinuxded_1.011/linuxjampded      # SV_Frame order + hook call
# (gdb -q -batch -ex 'file …' -ex 'disassemble …' works too)
```

Observed summary (2026-11-11):

- 49/49 shipped demos: framing valid, 817,600 frames decoded, **0 errors**.
- 49/49 shipped gamestates and the captured server gamestate re-encoded
  **meaningful-byte identical** from decoded values (4 demos carry one
  uninitialised trailing padding byte, explained in §3).
- Server capture: protocol 26; real gamestate with the `int16 0` RMG tail and
  trailing `svc_EOF`; `TheRandomMissionManager` is null at runtime.
- Protocol 25 connect: `Server uses protocol version 26.`

## 10. Phase 1 implementation notes (pure-Rust writer and gamestate encoder)

Code: `rust/src/demo/` (no engine dependency; `cargo test` on the host).

| module | contents |
|---|---|
| `file.rs` | `DemoSink`/`FileSink`, buffered `DemoWriter`, UTC `Timestamp`, `DemoName` (naming), `Retention` |
| `gamestate.rs` | `encode_gamestate`, `ConfigString`, `Baseline` (raw 532-byte `entityState_t`), RMG refusal |
| `msg.rs` | `MsgWriter` (`MSG_WriteBits`) and the static `msg_hData` Huffman code table |
| `fields.rs` | generated `ENTITY_FIELDS` (132 entries) and `HUFFMAN_CODES` (256 entries) |

Design points and deviations from the Phase 1 prompt, all deliberate:

- **Buffering.** `DemoWriter` appends `[seq][len][payload]` to a preallocated
  `Vec` (default 64 KiB, `DEFAULT_BUFFER_BYTES`) and calls the sink only when
  the buffer reaches the threshold or on `finish()`. `FileSink::create` uses
  `create_new` (never overwrites); terminating is explicit because `Drop`
  cannot report I/O errors.
- **Payload length padding.** `MsgWriter` emits `ceil(bit/8)` bytes. The
  engine's `MSG_WriteBits` sets `cursize = (bit>>3)+1`, which adds an
  extra, uninitialised byte when the final bit lands exactly on a byte
  boundary (the source of the four padding bytes in the Phase 0 demos). That
  byte is not message data and the demo reader ignores it; the writer
  deliberately does not reproduce it. The fixture's last bit is not
  byte-aligned, so its encoded length still matches the original exactly.
- **Naming timezone.** The name timestamp is **UTC** (`YYYYmmdd-HHMMSS`).
  The reference client's demos use local time, but the proxy names its own
  files and UTC avoids a dependency on the server's timezone/DST. The
  retention sweep uses the same embedded timestamp, so the two agree.
- **Sanitised map.** Characters outside `[A-Za-z0-9_-]` become `_`, runs
  collapse, leading/trailing `_` are trimmed, the result is capped at 64
  characters, and an empty result becomes `map`. `mp/ffa3` → `mp_ffa3`.
  `DemoName::parse` requires `sanitize_map(map) == map`, which is what makes
  foreign files unmatchable.
- **Instances and segments.** `main`, `tffa<N>`, `duel<N>`, `s<N>`; `N` and the
  segment are 1-based with no leading zero, so every generated name parses
  back exactly.
- **Retention.** `Retention { hours }`: `0` keeps forever; otherwise a file is
  expired when `now - embedded_timestamp >= hours`, and only names accepted by
  `DemoName::parse` are candidates. A file timestamped in the future (clock
  adjustment) is never deleted.
- **Gamestate inputs.** `Baseline` is a copy of the engine `entityState_t`
  (532 bytes); the encoder recomputes `lc` from the shipped field table, writes
  the configstrings (non-empty, sorted by index), sorts baselines by entity
  number, rejects duplicates/out-of-range numbers, writes `svc_EOF`,
  `clientNum`, `checksumFeed`, `int16 0`, and the trailing `svc_EOF`.
  `clientNum` is a caller parameter (U-010). `RmgTail::ManagerPresent` returns
  `GamestateError::RmgUnsupported` before any output; Phase 2 must skip that
  segment.
- **Generated data.** `tools/gen_demo_tables.py` produces `fields.rs` from the
  shipped server (field table at `0x81a15a0`) and the verified static Huffman
  tree. Both tables have FNV-1a hash-pin tests so a hand edit fails.
- **Byte-exact test.** `tests/fixtures/demo/server-ffa3-phase0.gamestate.bin`
  is the Phase 0 decoder's dump of the fixture's first frame (the Rust dev
  container has no Python, and Phase 1 must not grow a second parser). The Rust
  test feeds it to `encode_gamestate` and compares the meaningful bits
  `[0, meaningful_bits)` against the original `.dm_26` frame, including the
  trailing `svc_EOF`; the padding-bit convention is §3. The sidecar and the
  original frame are both in `tests/fixtures/demo/`; run
  `./dev.sh cargo test demo::`.
- **Test harness.** `rust/dev.sh` now also mounts the repository root
  read-only at `/repo` (`JAMP_REPO_ROOT`) so the containerised test can read
  the Phase 0 fixture; a plain host `cargo test` falls back to
  `../tests/fixtures/demo`. CI checks out the whole repository, so its
  `cargo test` uses the fallback unchanged.

## 11. Phase 2 implementation notes (recorder core, main view)

Code: `rust/src/demo/recorder.rs`; hook glue in `rust/src/hooks/engine_sv.rs`,
`rust/src/hooks/mod.rs`, `rust/src/patch.rs`; configstring capture in
`rust/src/syscall.rs`; lifecycle calls in `rust/src/lib.rs`.

### State (proxy memory only)

- `Msg` — a 32-byte, i386 `msg_t`-shaped struct with a fixed-width `u32` data
  pointer (the host test checks the same layout the engine sees).
- `ClientSnapshot` — the 3160-byte `clientSnapshot_t` layout (areabytes 0,
  areabits 4, ps 36, vps 1588, num_entities 3140, first_entity 3144,
  messageSent/Acked/Size 3148/3152/3156). A 32-slot ring is allocated at
  segment start; only the last written frame and the candidate are used.
- Two `entityState_t` scratch slots (`2 × 1024 × 532` bytes) plus our own
  `snapshotEntityNumbers_t` (4 + 4096 bytes). `first_entity` is
  `slot * MAX_SNAPSHOT_ENTITIES`; the frame ring never points at
  `svs.snapshotEntities`.
- A 16 KiB `msg_buf`, a `VecDeque` reliable-command queue (64 entries) and a
  1700-entry configstring mirror.
- No `svs.clients` mutation; the only engine writes are
  `sv.snapshotCounter` and the `svEntities[n].snapshotCounter` dedupe token
  that `SV_BuildClientSnapshot` itself writes. `svs.snapshotEntities`,
  `svs.nextSnapshotEntities` and `svs.clients` are never written (grep in
  §11.4).

### Tick

`SV_SendClientMessages` (`0x8059034`) is appended to `ENGINE_HOOKS`; the
wrapper runs the pristine body first, then `recorder::on_sv_send_client_messages`.
The tick:

1. requires `proxy_sv_enable` + `proxy_sv_enableDemoRecord`, `sv.state ==
   SS_GAME`, and `svs.time >= next_tick` (`next_tick = svs.time + 1000/sv_fps`);
2. picks the lowest client slot that is in the world, has a `gentity` and is
   not on `TEAM_SPECTATOR` (`level.clients[n].sess.sessionTeam`); a bot is a
   valid subject, and the temporary empty-map viewer is the fallback;
3. copies `level.clients[slot].ps`, ORs `PMF_FOLLOW` (0x1000), and discards the
   frame if `ps.m_iVehicleNum != 0` (vehicle PS block not implemented, U-010);
4. bumps `sv.snapshotCounter`, pre-marks `svEntities[ps.clientNum]` (the
   subject's own entity is regenerated by cgame), zeros the candidate frame,
   and calls the **`SV_AddEntitiesVisibleFromPoint` wrapper** with
   `ps.origin + ps.viewheight`. A `RecorderBuildGuard` sets
   `state.fov.recorder_build` for the call; `fov::apply` returns before the FOV
   cap / anti-wallhack, while the wrapper's instance filter still runs;
5. sorts the returned entity numbers, copies each `gentity->s` (532 bytes) into
   the current scratch slot, inverts the area mask (as `SV_BuildClientSnapshot`
   does), and emits the message with the engine's own `MSG_Init`,
   `MSG_WriteByte/Long/Data/Bits`, `MSG_WriteDeltaPlayerstate` and
   `MSG_WriteDeltaEntity` against `sv.svEntities[n].baseline` for first
   appearances. The playerstate delta base is the previous **written** frame
   (0 on the first snapshot); entity deltas are the `SV_EmitPacketEntities`
   three-way walk over the previous scratch slot and the current one.
6. writes `svc_EOF`, checks `msg.overflowed`; an overflow discards the whole
   frame and keeps the previous frame as the delta base. The next written frame
   reuses the same sequence number, so the delta distance is 1 while the ring
   stays within 32. A successful write drains the command queue and advances
   the sequence.

### Subject switching (spectator semantics)

The POV follows the subject the way a spectator follows a player, so it
switches **inside the same file**: when the current subject disconnects, is
kicked, or joins spectators, the next tick picks the lowest eligible slot and
keeps writing — same file, same sequence numbers, same delta base. A spectator
demo switches follow targets by changing `ps.clientNum` in the next snapshot
(the engine does exactly that when a spectator cycles targets), so the stream
stays a single valid delta chain. The switch is logged:

```
----- proxy-rs: demo subject switched: 0 -> 1 (left), continuing <path>
----- proxy-rs: demo subject switched: 0 -> 1 (spectator), continuing <path>
```

A slot is eligible when it is connected (`>= CS_PRIMED`), has a `gentity`, and
its `sess.sessionTeam` is not `TEAM_SPECTATOR`. **(Phase 3 supersedes this
rule with "in the world" `>= CS_ACTIVE` plus human-only and instance
membership; see §12.)** The spectator check matters
because a spectating client's `ps` is `SpectatorClientEndFrame`'s copy of the
player they follow (with `PMF_FOLLOW` and that player's `clientNum`): recording
it would silently follow someone else's POV instead of switching. The check is
skipped (slot stays eligible) if the game client array is not located yet.

Only when **no** eligible player is left does the segment close (`-1/-1`) and a
later eligible player start a new file. The temporary empty-map viewer is
never replaced mid-file.

Because the POV can move away from the slot named in the gamestate, the
gamestate names an unused viewer slot rather than the first subject (see "The
gamestate `clientNum`" above): if the first subject's slot is later reused by a
reconnecting player, a client that conflates "me" with that entity would draw
the newcomer at the followed player's position. On a full 32-slot server the
viewer is the subject itself and a POV change becomes a new segment (the
identity check closes the old one).

Runtime evidence (build `f07a4b11`, `mp/ffa3`, two bots):

| artifact | scenario | result |
|---|---|---|
| `artifacts/subject-switch/demos/20261111-223335_mp_ffa3_main_s1.dm_26` | subject `Alora` (slot 0) kicked while `Desann` (slot 1) plays | one start, `subject switched: 0 -> 1 (left)`, one stop; 259 snapshots; `ps.clientNum` 0→1 at 13.20 s; validates, SHA-256 `f65eeca3…` |
| `artifacts/subject-spectate/demos/20261111-223408_mp_ffa3_main_s1.dm_26` | subject slot 0 forced to `TEAM_SPECTATOR` in memory (gdb) | one start, `subject switched: 0 -> 1 (spectator)`, one stop; 303 snapshots; `ps.clientNum` 0→1 at 13.35 s; validates, SHA-256 `3f58aa95…` |

Both files pass `--validate-stream --verify-gamestate` (`max_delta=1`,
`bad_order=0`, gamestate round-trip identical). Client playback of a switched
stream is the same follow-target change a spectator demo records; it has not
been re-checked visually in this environment.

The first frame is the Phase 1 gamestate (`sequence 1`). A separate
`svc_setgame` frame (`sequence 2`, `svc_setgame + fs_gamedir + NUL + svc_EOF`)
matches the engine's one-shot `sentGamedir` message; the first snapshot is
`sequence 3`, `lastframe = 0`.

The gamestate `clientNum` (the slot a client treats as "me": cgame's
`cg.clientNum`) must never name a player other than the one being followed.
A client-recorded spectator demo satisfies that because its slot is the
spectator's own; a recorder has no such slot, and using the initial subject's
slot is only safe until that slot is reused. A player who reconnects there
becomes the demo's "me", and a client that renders that slot from the followed
player's state draws the newcomer at the followed player's position
(observed: player 0 reconnects into slot 0 after the POV moved to player 1 and
appears fused with player 1). The rule is therefore:

1. `sv_maxclients < MAX_CLIENTS`: the viewer slot is the **highest** slot the
   engine never assigns (`MAX_CLIENTS - 1`, i.e. 31). With `sv_maxclients 8`
   the gamestate says `clientNum 31`, and the stream is a spectator-follow
   stream (`ps.clientNum` = the subject, `PMF_FOLLOW` set).
2. All slots assignable (`sv_maxclients == MAX_CLIENTS`): the viewer slot is
   the **highest free** slot at segment start, and every tick — before a
   snapshot is built — the recorder checks it. If that slot is ever occupied
   by a client that is not the followed player, the segment closes (the file
   ends with `-1/-1` and a new segment starts on the next tick). Because the
   check runs before `build_snapshot`, no frame containing the collision can
   reach the file.
3. No free slot at all (a full 32-slot server): the viewer slot is the
   subject's own slot, so `ps.clientNum == clientNum` and the stream is a
   plain client-style POV of that player — no `PMF_FOLLOW`, the followed
   entity excluded exactly as a client demo excludes its own. A POV change
   remains safe while the old slot stays empty (the viewer then names an empty
   slot); if someone reconnects there, the same check closes the segment.

The engine reads the gamestate `clientNum` only to pass it to `CG_Init`; cgame
indexes `cgs.clientinfo[clientNum]` (within `MAX_CLIENTS`) and, for an empty
slot, takes the "no info" branch.

### ABI-aligned entry (D-001.1)

`-C target-feature=-sse,-sse2` reaches only this crate: the precompiled std is
built for the i386 16-byte stack ABI and zeroes stack locals with `movaps`.
The engine enters detours with a 4-byte-aligned stack, so a tick that reaches
std file-system code can fault depending on the call depth. That was observed
here: `SIGSEGV` at `movaps %xmm0,-0x68(%ebp)` in
`std::sys::pal::unix::fs::try_statx`, reached from
`std::fs::create_dir_all` when the recorder created its output directory
(gdb-confirmed, module base + `0xe8ab3`). The trap path never had the problem
because `jampgame_vm_dllsyscall` (`csrc/syscall_shim.c`) is compiled with
`force_align_arg_pointer`; the recorder now does the same: its three entries
(tick, `GAME_INIT`, `GAME_SHUTDOWN`) go through
`jampgame_aligned_entry(fn, op)` in that file, which realigns the stack and
then calls the Rust entry. Any future engine detour that reaches std
file-system/env code must use the same shim.

### Frame length convention (`msg_t.cursize`)

The engine's `MSG_WriteBits` ends with `msg->cursize = (msg->bit >> 3) + 1`
(`msg.cpp:207`), i.e. a byte is reserved for the next symbol. When the last
symbol ends exactly on a byte boundary the message is therefore one byte
longer than its bits, and the client's `MSG_ReadByte` check
(`readcount > cursize`) needs that byte: without it the trailing `svc_EOF`
read returns `-1` and `CL_ParseServerMessage` rejects the message. Phase 1's
`MsgWriter` originally returned `ceil(bits/8)` and produced such a short
gamestate roughly once in eight segments (`SIGSEGV` fix run,
`map_restart` segment, payload 4,883 B where 4,884 B were required). It now
reserves the byte the same way the engine does, with a unit test over all 256
symbols; `artifacts/soak-fixed/demos/20261111-214019_mp_ffa3_main_s1.dm_26`
is a real byte-boundary gamestate (payload 4,877 B, `end_bit` 39,008) that
re-encodes byte-identically.

### Scoreboard

A demo viewer cannot ask for the scoreboard: cgame's `+scores` handler skips
the `scores` client command while `cg.demoPlayback` is set, and even a demo
that did send it has no server to answer. A client-recorded demo therefore
only contains the `scores` commands the recording player happened to trigger.
A server-side recorder has no such player, so it asks the game itself:

- At `GAME_INIT` the proxy resolves the game module's own
  `DeathmatchScoreboardMessage(gentity_t*)` (`g_cmds.c`) by its exported name
  `DeathmatchScoreboardMessage__FP9gentity_s`. Both the shipped 1.011 module
  and the proxy's `jampgame_original.so` export it; a module without it records
  demos without a scoreboard (one log line).
- Every `1000 ms` (`SCOREBOARD_INTERVAL_MS`), and on the first recorded tick,
  the recorder calls it with the subject's `gentity_t*`. The game formats the
  board from `level` and calls `trap_SendServerCommand`, which the
  `G_SEND_SERVER_COMMAND` interceptor recognises as the probe's own message:
  it is captured (one trailing newline added, as `SV_AddServerCommand` would)
  and **not forwarded**, so the live client's reliable queue is untouched.
- The captured text is queued as a normal reliable command, so it rides in the
  next written snapshot. Identical text is not repeated (a still board costs
  one command), and a text longer than `MAX_STRING_CHARS - 2` is dropped
  exactly like `SV_AddServerCommand` drops it.

The text is the stock 14-int per-client layout (`client, score, ping, time,
scoreFlags, powerups, accuracy, impressive, excellent, gauntlet, defend,
assist, perfect, captures`). The proxy's live serverinfo advertises
`taystJKinfo` bit 7 so capable cgames parse a 15th `deaths` int, but a demo has
one stream for every future cgame and a stock cgame always parses 14. The
recorder therefore removes `taystJKinfo` from the **recorded** `CS_SERVERINFO`
(gamestate and any mirrored `cs 0` update): TaystJK's `CG_ParseScores` then
selects the 14-int stride from the demo's own serverinfo, and both stock and
TaystJK clients read the same board. Live clients keep bit 7 and their deaths
column.

Evidence (`artifacts/demo-scoreboard-final/demos/20261111-225844_mp_ffa3_main_s1.dm_26`,
build `86a9be87`, SHA-256 `d758efd3…`): the live server's cvar reads
`taystJKinfo 128` and its `sv.configstrings[0]` contains the key; the recorded
gamestate's `cs[0]` does not; the stream carries `scores` commands for 1 and 2
clients as the bots joined, every one a clean 14-int layout; the file validates
(`max_delta=1`, `bad_order=0`, round-trip identical). The previous run
(`artifacts/demo-scoreboard/…`) also covered the 3-client board, and its live
capture client joining showed the `cs` mirror continuing alongside.

Limits: the demo has no deaths column (the stock layout), and on a
parallel-TFFA/duel server the board is the game's global one — the per-viewer
instance rewrites cannot apply to a stream with no viewer. The board also
inherits the engine's command-size caps, so a full 30-client board is
truncated/dropped exactly as it would be for a live client.

### Configstrings

Runtime changes are captured **after** the engine has stored the new value
(`GAME_SET_CONFIGSTRING` forwarding in `syscall.rs`), compared with the
recorder's mirror, and queued as the engine's own command text:

```
cs <index> "<value>"\n
```

with no escaping (the 49 demos contain raw info-string backslashes and
quote-delimited names). Values of `MAX_STRING_CHARS - 24` (1000) bytes or more
use the engine's `bcs0`/`bcs1`/`bcs2` chunk protocol. Each queued command is
written once in the next successfully written snapshot; there is no ack logic.
The initial gamestate carries the current values, so the mirror starts from
`sv.configstrings` at segment start.

### CVars and lifecycle

- `proxy_sv_enableDemoRecord` (0), `proxy_sv_demoDir` (`demos`).
- **Temporary Phase 2 debug**: `proxy_sv_demoRecordEmpty` (0) starts a
  recording with no players. It is not a README feature and **was removed in
  Phase 3** (§12); the human-subject rule replaces it.
- `GAME_INIT` and `GAME_SHUTDOWN` close an open file with `-1/-1`; disabling
  the cvar or the master switch does the same. The file writer buffers 64 KiB
  and flushes at the threshold or at close.

### Validation artifacts (Phase 2 gate A)

Recorded against the pristine 1.011 engine with the final proxy build on
`mp/ffa3`, 30 s each:

| artifact | command/setup | result |
|---|---|---|
| `artifacts/demo-phase2-final/empty/demos/20261112-075531_mp_ffa3_main_s1.dm_26` | `proxy_sv_demoRecordEmpty 1`, cvar enabled at launch, no clients | 691 frames, 18,497 B, SHA-256 `d91a4d7a…`, gamestate `clientNum 31` |
| `artifacts/demo-phase2-final/moving/demos/20261112-075613_mp_ffa3_main_s1.dm_26` | `addbot Alora 2`, cvar enabled, 30 s | 601 frames, 34,282 B, SHA-256 `f1c4143e…`, gamestate `clientNum 31` |

`tools/demo_inspect.py --validate-stream --verify-gamestate` on both:

```
empty:  gamestate first=True snapshots=708 max_delta=1 bad_order=0 bad_range=0
        cs_commands=0 bad_cs=0 commands=0; round-trip identical
moving: gamestate first=True snapshots=659 max_delta=1 bad_order=0 bad_range=0
        cs_commands=0 bad_cs=0 commands=0; round-trip identical
```

The moving file carries the bot's changing playerstate and a visible entity
set (up to 18 entities, 32 non-empty snapshots). The `svc_setgame` frame is
sequence 2 with gamedir `base`.

Stability soak (the scenario that used to fault): a server started under gdb
with `proxy_sv_enableDemoRecord 1` and `proxy_sv_demoRecordEmpty 1`; three
synthetic protocol-26 clients joined and left the running recording, a bot was
added, the cvar was toggled off/on and `map_restart` was issued 14 times. No
`SIGSEGV`; the server stayed up and all 8 segments validate
(`artifacts/soak-fixed/demos/`). One of them is the byte-boundary gamestate
case described above.

Earlier Phase 2 runs also exercised a synthetic protocol-26 subject
(`/tmp/opencode/subject-rec.dm_26`, 159 frames, gamestate 4166 B) and a
runtime `cs` capture: the empty recording that saw a client connect produced
four `cs` commands (e.g. `cs 1131 "n\RecSubject5\t\0\model\kyle…"`), all
matching the engine's wording and parsed by `--validate-stream`.

### Real-client traffic

The recorder never writes `svs.snapshotEntities`, `svs.nextSnapshotEntities` or
`svs.clients`; the source contains no such stores (the `svs.clients` accesses
are reads of the pointer and of `state`/`gentity`). A synthetic protocol-26
client connected while a recording was active and its captured server message
stream parsed normally (159 frames, 4166-byte gamestate, snapshot chain), so
the engine's real-client path kept producing snapshots. A server started with
the recorder cvar off answers `getinfo` normally (`\protocol\26`).

### Playback evidence

**Confirmed by the owner on 2026-11-11**: the unmodified moving artifact plays
and ends as expected on both TaystJK and a stock client, i.e. the
`svc_setgame base` frame, the gamestate `clientNum` choice and the snapshot
chain are accepted by real clients. (The remote session used for development
could load the map and logged no truncation/illegible/delta error, but could
not capture the GL window, which is why the earlier report listed playback as
unverified.)

The development command used (a playback copy without the `svc_setgame` frame,
so the mod's `taystjk` cgame stays loaded):

```sh
export DISPLAY=:0 XAUTHORITY=/run/user/1000/.mutter-Xwaylandauth.ODHWY3
cd /tmp/opencode/jka-home
mkdir -p taystjk/demos
cp <artifact> taystjk/demos/gate-bot.dm_26
/home/vincent/Documents/jampgame-proxy-rs/original/GameData/taystjk.x86_64 \
  +set fs_basepath /home/vincent/Documents/jampgame-proxy-rs/original/GameData \
  +set fs_homepath /tmp/opencode/jka-home +set fs_game taystjk \
  +set r_fullscreen 0 +set r_mode 3 +set s_initsound 0 +demo gate-bot
```

That client loaded `maps/mp/ffa3.bsp` and logged no `Demo file was truncated`,
`Illegible server message`, read-past-end or delta warning; its window could
not be captured in the remote session (screenshots were black), which is why
the owner's run above is the visual confirmation.

### Phase 2 deviations / limits

- Subject selection is the lowest eligible slot (bots included); spectators are
  skipped and the POV switches in place when the subject leaves or joins
  spectators (see "Subject switching" above). **(Phase 3 supersedes this with
  human-only subjects, per-instance recorders and per-instance configstrings;
  see §12.)**
- One `main` segment per `GAME_INIT`; a map change/restart opens a new file.
- Vehicle playerstate frames are discarded, not emitted (U-010).
- RMG gamestates are refused by the Phase 1 encoder; the segment is skipped.
- The empty-map debug cvar exists only for gate A.
- No retention, `/demos`, announcements, size/free-space guards (Phase 4).

## 12. Phase 3 implementation notes (instances and lifecycle)

Code: `rust/src/demo/recorder.rs` (manager, instance view, chat, lifecycle);
`rust/src/duel.rs` (`court_hud`, shared by the live HUD push and the recorder);
`rust/src/instances.rs` (`cs_generation`); `rust/src/syscall.rs` (chat tap
after the engine accepted a `G_SEND_SERVER_COMMAND`).

### Instance set and ownership

`Recorder` owns a `Vec<InstanceRecorder>`; each entry is one instance
(`DemoInstance::{Main, Tffa(u8), Duel(u8)}`) plus its currently open segment.
No instance bookkeeping was added: membership comes from the existing tables
used by the live per-viewer path.

| Gametype / feature | Instance set | Membership source |
| --- | --- | --- |
| No instancing (`instance_mode() == Off`) | one `main` | every connected slot |
| Parallel TFFA (`proxy_parallel_tffas_enable`, `GT_TEAM`) | `tffa<id+1>` | `state.tffa.client_match[slot]` (written for both modes) |
| Parallel duels (`proxy_parallel_duels_enable`, `GT_DUEL`) | `duel<court+1>` | `state.duel.courts[..court_count]`, `Court::players()` |

A duel client that is benched or spectating belongs to no court and is not a
subject; a TFFA client is always in exactly one match id (`0` = the main
slot's match). The file label is the Phase 1 1-based label (`match 0` →
`tffa1`, `court 0` → `duel1`); the value handed to the per-viewer rewrites is
the raw table id (`DemoInstance::viewer_match`).

### Subject per instance

A subject is a client that is **in the world** (`client_t.state >= CS_ACTIVE`;
a connecting client already has a `gentity`, assigned at direct connect, but
must not count), has a `gentity`, is not on `TEAM_SPECTATOR`, and is not a bot
(`ClientState::is_bot`, set from `GAME_CLIENT_CONNECT`). The recorder follows
the lowest such slot of its instance. A server with only bots therefore records
nothing.

The Phase 2 transition table is unchanged, now scoped to the instance:

- current subject still eligible → keep;
- left / not in world / no `gentity` → switch to the instance's lowest
  eligible slot, same file (spectator semantics: next snapshot's
  `ps.clientNum` changes);
- joined spectators → same switch, reason `spectator`;
- still playing but moved to another match/court → switch, reason `moved
  instance`;
- no eligible slot left in the instance → close the segment (`-1/-1`);
  repopulation opens the next segment number for that instance.

### Naming and viewer identity

Each recorder runs Phase 2's identity rule independently
(`pick_viewer_slot(subject, sv_maxclients, slots)`): with
`sv_maxclients < MAX_CLIENTS` the highest slot the server never assigns;
otherwise the highest free slot; otherwise the subject itself (self view, no
`PMF_FOLLOW`). The gamestate's `clientNum` is that viewer slot and
`ps.clientNum` is the subject. A per-tick watchdog closes the segment if the
viewer slot becomes occupied by a client other than the subject, so one
recorder's fallback can never poison another's identity. All Phase 3 artifacts
carry `clientNum 31`.

Segments are numbered per instance and reset at `GAME_INIT`
(`main_s1`, `main_s2`, …; `tffa1_s1`, `tffa2_s1`, …). A `map_restart` also
starts at `s1` again because it runs `GAME_INIT`.

### Per-instance visibility

The recorder build is unchanged: it wraps `SV_AddEntitiesVisibleFromPoint` and
sets `state.fov.recorder_build` for the call, so `fov::apply` returns before the
FOV cap and the anti-wallhack traces (spectator exemption), while the existing
instance filter still runs and compacts the build to the subject's match/court.
No second filter was written.

Evidence (entity stream of each Phase 3 file, decoded with
`tools/demo_inspect.py`; entity `clientNum` is netfield 32, `eType` 8):

- TFFA (pairs `{1,3}` and `{2,0}` from the `CS_PLAYERS` teams): `tffa1`
  (subject 1) carries only player entity `clientNum 3`, `tffa2` (subject 2)
  carries only `clientNum 0` — each file has exactly the other member of its
  own match and no player of the other match.
- Duel round 2: `duel1` (subject 2, court `{2,3}`) carries `cs30 "3|2"` and
  only player entity 3; `duel2` (subject 0, court `{0,1}`) carries
  `cs30 "0|1"` and only player entity 1. No court sees the other's pair.
- The subject's own entity is deliberately pre-marked as sent (Phase 2 rule:
  cgame regenerates the followed player), so each stream shows the partner(s),
  not the subject.

### Per-instance configstrings

`instance_configstring(instance, matches, index, base)` maps the engine's
global value to the value that instance's viewer would receive:

- `CS_SERVERINFO` — the Phase 2 rule (`taystJKinfo` removed for playback);
- `CS_PLAYERS + slot` — `instances::rewrite_cs_players_for_slot(base, slot,
  client_match[slot], instance.viewer_match())`, i.e. the same per-viewer
  rewrite a live client gets (TFFA demotes and prefixes outsiders, the duel
  ladder labels courts and injects W/L);
- `CS_SCORES1/2` — `instances::view_scores(match)` for TFFA (per-match tally),
  `duel::court_hud(court)` for a duel court;
- `CS_CLIENT_DUELISTS` / `CS_CLIENT_DUELHEALTHS` — `duel::court_hud(court)`;
- every other index — the global engine value.

Runtime updates are mirrored after the engine stored them (`on_set_configstring`
in the forwarded `GAME_SET_CONFIGSTRING`), so a recorder receives the same
values it would receive live. The volatile per-viewer entries are also
recomputed when `instances::cs_generation()` moves (bumped by
`refresh_cs_players_for_viewer` and `duel::push_hud`) or when the match table
changes, so a slot that joins/leaves a match (or a court forming) reaches the
demo even though the recorder is not a client. The live per-viewer pushes are
untouched: the proxy still writes the real clients' configstrings.

Observed per-instance values:

- `tffa1` demo: `cs 1131` (slot 0) = `(TFFA 1) Two_Blue…` (the outsider,
  demoted to team 3), its own players unprefixed; `tffa2` demo: `cs 1134` /
  `cs 1132` = `(TFFA 0)` on main's players, its own unprefixed.
- Duel `duel1`: `cs30 "3|2"`, `duel2`: `cs30 "0|1"`, each holding only that
  court's pair.

**Naming observation (deviation, not changed).** The live `(TFFA n)` prefix
uses the raw match id (`(TFFA 0)` = main, `(TFFA 1)` = the first created
match), while the Phase 1 file label is 1-based (`tffa1` = main, `tffa2` = the
first created match). A demo named `tffa2` therefore shows `(TFFA 1)` on
outsiders. The rewrite was reused as-is; aligning the two is a separate
product decision (the owner may prefer `(TFFA 1)` to mean `tffa1`).

### Per-instance scoreboard

The Phase 2 probe is unchanged: once per second the recorder calls the game's
`DeathmatchScoreboardMessage(subject gentity)` and its `scores` trap is
captured instead of forwarded. The captured text now goes through the same
per-viewer rewrite the live `G_SEND_SERVER_COMMAND` path uses, with the
subject as viewer: `tffa::rewrite_scores_for_viewer` for a TFFA match and
`duel::rewrite_scores_for_viewer` for a court (`main` passes the game text
through). Identical text is still deduped, and an over-long command is dropped
like `SV_AddServerCommand` would.

### Chat

**Owner decision (2026-11-12): broadcasts only.** A demo carries the
server-wide `chat`/`lchat` line and nothing else: team chat
(`say_team` → `tchat`/`ltchat`), a private `tell` and a per-client `print` are
never recorded. The rule lives in one function, `recorder::chat_command_for`:

- the game does not broadcast chat with `-1`; `G_Say` calls `G_SayTo` per
  client and sends each client its own `chat` command
  (`g_cmds.c:1550-1554`, `G_SayTo`). The demo therefore takes the copy
  **addressed to the subject** (or a `-1` command, for a sender that uses it);
  that copy is by construction the broadcast that player received. Team chat
  is a different command name and is filtered by name, so it never enters the
  stream even though the subject receives it as well.
- a private `tell` is not a distinct command: `G_Say`'s `SAY_TELL` case wraps
  the sender's name in `ESC '[' … ESC ']'` (`Q_COLOR_ESCAPE` = `0x19`,
  `g_cmds.c:1601-1613`) and `Cmd_Tell_f` addresses it to the target and the
  speaker only. A `chat`/`lchat` line whose quoted name starts with `\x19[` is
  a tell and is excluded.
- a per-client `print` is not a chat candidate at all.

The tap runs in `syscall.rs` after the engine accepted the command (a message
the proxy dropped for this recipient never reaches it) and appends to a bounded
256-entry queue drained by the next recorder tick; the recorder mutex is never
re-entered from the trap.

The rule stays one function so a future owner decision (e.g. including
localised broadcasts or spectator-team chat) is a one-function edit; the
accepted behaviour and its rationale are recorded in `docs/uncertainties.md`
(U-013, resolved).

### Lifecycle

- `GAME_INIT` (a full `map` change *and* `map_restart`) closes every open
  segment with the `-1/-1` terminator and clears the per-instance segment
  counters; nothing crosses a map boundary and the new map starts at `s1`.
- `GAME_SHUTDOWN`, `proxy_sv_enableDemoRecord 0` and the proxy master switch
  close every segment the same way.
- The Phase 2 empty-map debug cvar (`proxy_sv_demoRecordEmpty`) was removed;
  the human-subject rule replaces it. `PROXY_CVARS` is back to 51 entries.

### Resources

`MAX_DEMO_RECORDERS = 16` concurrent segments (TFFA has at most 9 instance
slots including main; the duel ladder allows 16 courts). When a new instance
cannot get a recorder, one log line names it and the manager continues without
it; the flag resets once capacity is available. Per-recorder buffers (frame
ring, two entity scratch slots, message buffer, writer buffer) are allocated at
segment start and do not grow with the roster.

### Validation artifacts (Phase 3 gate B)

Build `jampgamei386.so` SHA-256
`544849237c80c8bb71f290aadd1eeb287e86c3ccbd977372ef904b906f957986`
(commit state: uncommitted Phase 3 work). All files pass
`tools/demo_inspect.py --frames 1 --validate-stream` with
`max_delta=1`, `bad_order=0`, `bad_range=0`, `bad_cs=0`, and the gamestate
round-trip is identical.

| File | SHA-256 | Evidence |
| --- | --- | --- |
| `artifacts/phase3-tffa/demos/20261112-101848_mp_duel1_tffa1_s1.dm_26` | `3246008a…` | main match, subject 1, 360 snapshots; entity stream only partner `clientNum 3`; `chat hello-all` once, `team-main` team chat absent; `cs` demotes the other match's players |
| `artifacts/phase3-tffa/demos/20261112-101848_mp_duel1_tffa2_s1.dm_26` | `529cbfb2…` | created match, subject 2, 360 snapshots; only partner `clientNum 0`; same `chat` once, `team-two` team chat absent; `(TFFA 0)` on main's players |
| `artifacts/phase3-duel/demos/20261112-094002_mp_duel1_duel1_s1.dm_26` | `ffb63912…` | round 1, one court (subject 0, partner 1); closed at the `map_restart` with `-1/-1` (143 frames) |
| `artifacts/phase3-duel/demos/20261112-094010_mp_duel1_duel1_s1.dm_26` | `1a4108fb…` | round 2 court 1, subject 2, `cs30 "3|2"`, only entity 3 |
| `artifacts/phase3-duel/demos/20261112-094010_mp_duel1_duel2_s1.dm_26` | `2e5b944c…` | round 2 court 2, subject 0, `cs30 "0|1"`, only entity 1 |
| `artifacts/phase3-life/demos/20261112-094132_mp_ffa3_main_s1.dm_26` | `5d56e1af…` | `main`, subject switch `0 → 1 (left)`, same file; closed when the instance emptied |
| `artifacts/phase3-life/demos/20261112-094152_mp_ffa3_main_s2.dm_26` | `44e4f144…` | repopulation, new segment label `main_s2`; closed at the full `map mp/ffa3` |
| `artifacts/phase3-life/demos/20261112-094225_mp_ffa3_main_s1.dm_26` | `233aa3f0…` | new map (`checksumFeed 0x5bea9380` ≠ `0xd8e9a7d8`), counters reset to `s1` |
| `artifacts/phase3-chat/demos/20261112-101757_mp_ffa3_main_s1.dm_26` | `28972121…` | `chat hello-broadcast` and `chat other-broadcast`; the private `tell secret-tell` is absent; no `tchat`/`print` |

Log excerpts (`docker logs`): `demo recording started … (instance tffa1,
subject 1, viewer 31)`, `(instance tffa2, subject 2, viewer 31)`, the duel
round-2 pair `duel1`/`duel2`, `demo subject switched: 0 -> 1 (left),
continuing …`, `demo recording stopped … (184 frames)` at the empty instance,
and `demo recording stopped … (43 frames)` at the map change.

### Playback (real client, 2026-11-12)

Both a TFFA and a duel file were played by the real TaystJK client
(`original/GameData/taystjk.x86_64`, latest-2ab07a797, `fs_game base`,
homepath copy under `/tmp/opencode/jka-home3/base/demos`):

- `p3-tffa` (= `tffa1` file): `CL_InitCGame` at 10:48:04, `DISCONNECTED` at
  10:48:23 — 19 s for 362 snapshots at ~20 fps — then the main menu.
- `p3-duel` (= `duel2` file): `CL_InitCGame` at 10:47:11, `DISCONNECTED` at
  10:47:27 — 16 s for 301 snapshots at ~20 fps.

Neither run logs `Demo file was truncated`, `Illegible server message`, a
delta/read-past-end error or a parse error; the files load, decode to their
end and return to the menu. The client renders through Wayland on this
machine, and the session's X11 capture only sees a *different* client's
Xwayland window, so no visual frames of this playback could be captured here.
This is playback evidence, **not** a claim of visual verification; the owner's
Phase 2 visual check remains the reference for presentation. Exact commands:

```sh
mkdir -p /tmp/opencode/jka-home3/base/demos
cp <tffa-demo> /tmp/opencode/jka-home3/base/demos/p3-tffa.dm_26
DISPLAY=:0 ./original/GameData/taystjk.x86_64 \
  +set fs_basepath "$PWD/original/GameData" \
  +set fs_homepath /tmp/opencode/jka-home3 +set fs_game base \
  +set r_fullscreen 0 +set r_mode 3 +set s_initsound 0 \
  +set logfile 2 +demo p3-tffa
```

### Phase 3 deviations and limits

- **Chat is broadcast-only** (owner decision 2026-11-12, U-013). It records the
  subject's copy of server-wide `chat`; team chat, a `tell` to the subject and
  per-client `print` never appear, so a demo also has no chat from the moment
  every eligible player leaves.
- **Phase 4 disk guards accepted (not implemented)**: `proxy_sv_demoMaxFileMB`
  128 and `proxy_sv_demoMinFreeMB` 512, accepted by the owner on 2026-11-12.
  Phase 3 does not read them; they belong to the retention/guard phase.
- **`(TFFA n)` prefix vs file label** differ by one (see above); the live
  rewrite was reused unchanged.
- **`cs30/31` in TFFA demos** carry the engine's global value when the game
  writes them (the live TFFA path does not rewrite them either); the duel
  instance value comes from `court_hud`.
- **16-recorder cap**: beyond it an instance is not recorded (one log line).
- **Vehicles and RMG** are still unsupported/refused (U-010).
- **Duels**: a segment follows its court; when the court dissolves at a round
  boundary the segment closes and the next formed court opens `s1` again after
  the `GAME_INIT` of the round transition. Carried `map_restart` rounds
  therefore do not chain one file across rounds.
- No retention, announcements, `/demos`, disk guards (Phase 4).
