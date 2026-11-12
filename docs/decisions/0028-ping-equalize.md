# D-028 — Ping equalisation (`proxy_sv_pingEqualize`)

Status: implemented (2026-11-13), runtime harness added; the real-client
measurement remains open.

## Goal

Make every player in a match play at the same **effective** latency by adding
a server-side delay to the low-ping players' inbound usercmds, so they play
under the circumstances of the slowest player present — up to a configurable
cap. The server can only add delay, never remove it: players are padded *up* to
a common target, and a player already slower than the target is left alone.

Explicit non-goals:

- **outbound/snapshot delay** — holding each client's snapshots would also make
  their view as stale as a real high-pinger's; it needs engine send-path work
  (delta encoding, reliable messages, ping accounting) and is not part of this
  record. The low-ping player's view therefore stays fresher than a natural
  high-pinger's; the *input* path is equalised, the perception asymmetry is not;
- lag compensation / unlagged hit detection;
- changing the engine's `status`/`getstatus` ping (kept truthful).

## Decisions

### D-028.1 — Target = `min(cap, slowest in-game ping)`

`cap` is `proxy_sv_pingEqualize` in milliseconds (`0` = off, clamped
`0..=250`). A player slower than the cap does not raise the target: everyone
else is pulled to the cap and the straggler keeps their own ping. An empty or
one-player roster targets that player's own ping (or `0`), so nobody is padded
— a LAN-only match adds nothing.

### D-028.2 — Inbound commands only

The delay is a negative offset on `ucmd.serverTime` in the existing
`sv_client_think_stub` (the retargeted `call SV_ClientThink` inside
`SV_UserMove`, D-015.2, patch `CALL_SV_CLIENT_THINK` at `0x804e891`). The game's
`ps.commandTime` then trails `level.time` by the delay, exactly what a slower
network path produces: pmove integrates the same `msec` per command, the server
processes the moves later. No engine network or snapshot-path change.

The stub composes the total shift as `timenudge_offset - applied_equalize_delay`
(the timeNudge correction stays positive). The estimator continues to receive
the **raw** stamp, and `cl->lastUsercmd.serverTime` is restored after the call,
so the engine's `cl_packetdup` duplicate filter keeps comparing client stamps.

### D-028.3 — Display

After the pristine `GAME_RUN_FRAME` forward, every client with an applied delay
`> 0` and a valid `cl->ping` reading gets
`level.clients[i].ps.ping = cl->ping + applied_delay` (an invalid `999`
reading is left untouched so the sentinel never becomes a fake high ping). That
is the value `DeathmatchScoreboardMessage` builds the `scores` string from
(`g_cmds.c:52-53`) and the end-of-game `score: … ping:` log line.

**Caveat (evidence):** the shipped cgame has its scoreboard ping column
commented out (`cg_scoreboard.c:245`), so the stock client does not draw the
value. It is visible on clients/mods that render `ps.ping`. The proxy's own
netStatus table adds a truthful `eff` column (`ping + applied delay`, `-`
without a reading), and `status`/rcon `status` keep the engine's real ping.
`g_allowHighPingDuelist` and similar rules read `ps.ping`; the equalised value
cannot exceed the 250 ms cap for a valid member reading, so the `>= 999`
threshold is unaffected (a transiently stale non-member reading during
ramp-out is the only theoretical exception).

### D-028.4 — Population

Members of both the target calculation and the padding:

- engine `client_t.state == CS_ACTIVE`;
- not a bot (`SV_CalcPings`' own `gentity->r.svFlags & SVF_BOT` check, now
  shared as `hooks::engine_sv::is_bot`);
- `cl->ping` in `1..999` (`999` is the engine's "no reading" sentinel);
- game session team `!= TEAM_SPECTATOR` (read from the located `gclient_t`
  array, `sess.sessionTeam`).

Spectators, still-connecting clients and bots are ignored and never padded.

### D-028.5 — Tracking

The target is recomputed from `cl->ping` at most every 500 ms (the engine's own
value is a 32-snapshot average, `SV_CalcPings`). It moves only when it differs
by at least `TARGET_DEADBAND_MS = 10`. A change of the member set (join, leave,
spectator, return to play) retargets immediately, bypassing the deadband; a cap
change retargets immediately too (added so an admin change is not held back by
the deadband). The 1 ms-per-usercmd ramp still applies to the per-client delay.

The member pings are latched at each recomputation and the per-client delay
target is derived from that snapshot, so the 500 ms/deadband rule also smooths
the delay target. A frame with no due recomputation keeps the previous target
and pings.

### D-028.6 — Ramp and off semantics

Every client's applied delay moves 1 ms per usercmd, both ways, exactly like
the `cl_timeNudge` void (D-015.3). A jump would trip the game's `msec < 1`
guard or produce one oversized movement frame (`g_active.c:2023-2028`).

- `proxy_sv_pingEqualize 0` sets every target to 0 and ramps the delay away
  instead of snapping the clock forward.
- `proxy_sv_enable 0` detaches the hook layer immediately (no more stub); the
  cleared per-client state means the one following command sees the raw stamp,
  producing one oversized `msec` clamped to 200 by the game. This is the same
  documented consequence as every feature behind the master switch.

### D-028.7 — Cvars

| cvar | default | behaviour |
|---|---|---|
| `proxy_sv_pingEqualize` | `0` | cap in ms; `0` = off; clamped to `0..=250` |
| `proxy_sv_pingEqualizeLog` | `0` | `1` logs target changes and per-client engage/release; works without enforcement |

The cap clamp exists because a client can only replay its last
`CMD_BACKUP = 64` usercmds (`cg_public.h:6`); when the snapshot's
`ps.commandTime` falls behind that window, `CG_PredictPlayerState` freezes
prediction ("exceeded PACKET_BACKUP on commands", `cg_predict.c:1085-1094`). At
`cl_maxpackets 125` the window is about 512 ms *including* the round trip, so
250 ms leaves margin at every packet rate.

Log shape:

```text
----- proxy-rs: pingEqualize target=110 (slowest client=3 ping=180 capped)
----- proxy-rs: pingEqualize target=95 (slowest client=2 ping=95)
----- proxy-rs: pingEqualize client=0 delay=75 ping=20 target=95
----- proxy-rs: pingEqualize client=0 delay=0 (released)
```

**Log-only interpretation.** `proxy_sv_pingEqualize 0` leaves no cap to compute
a target from, so "works without enforcement (reports what would be applied)"
is implemented as a preview at the maximum supported cap: the target and the
per-client delays are computed and logged with the `(preview)` suffix, no
command is rewritten and `applied` stays 0. The per-client engage line reports
the eventual target delay; the release line is emitted only once the applied
delay has ramped to 0 (so a spectator/disabled cap is reported when the client
really stops being delayed). Because the preview immediately reports the
would-be delay again, turning the cap off while the log stays on produces a
`(released)` line followed by a preview engage line on the next frame; the log
thus distinguishes "the real delay ended" from "the preview still sees a
candidate".

## Evidence (verified before implementation)

- The shift path exists and is validated: `rust/src/hooks/netstatus.rs`
  `sv_client_think_stub` already rewrites `serverTime` for D-015, feeds the
  estimator the raw stamp and restores `cl->lastUsercmd.serverTime`.
- The game accepts the shift: `g_active.c:2008-2022` clamps
  `ucmd->serverTime` to `[level.time - 1000, level.time + 200]`; `msec < 1`
  drops the command and `msec > 200` clamps it (`g_active.c:2023-2028`);
  `Pmove` chops frames over 66 ms. The 1 ms/command ramp keeps every command
  inside these guards.
- `cl->ping` is written by the engine's `SV_CalcPings` (average over
  `PACKET_BACKUP = 32` snapshots); `OFFSET_CLIENT_PING = 234532`.
- `SV_Frame` order (`sv_main.cpp:901-926`): `SV_CalcPings()` → the
  `GAME_RUN_FRAME` loop → `SV_SendClientMessages()`. The write therefore lands
  after the engine's ping refresh and before the snapshot that carries it.
- `level.clients` is a `gclient_t*` at `level+0`
  (`OFFSET_LEVEL_CLIENTS = 0`), stride `SIZEOF_GCLIENT = 7232`, `ps` at
  `OFFSET_GCLIENT_PS = 0`, session team at `OFFSET_GCLIENT_SESS +
  OFFSET_SESS_SESSION_TEAM` (1708 + 0).
- `offsetof(playerState_t, ping) = 524` computed with
  `g++ -m32 -fpermissive offsetof` against
  `original/jedi-academy-sdk/codemp/game/q_shared.h` on 2026-11-12; the same
  build produced `stats=244`, `persistant=308`, `weapon=176`, `legsFlip=120`,
  `torsoFlip=124`, `viewheight=196`, `clientNum=172`, `m_iVehicleNum=1436`,
  `sizeof=1552`, matching the existing `sdk.rs` pins. Added as
  `OFFSET_PS_PING` with a pinned unit test.

## Validation

- Unit tests (`rust/src/pingequalize.rs`): empty/one-player target, slowest
  target, above-cap behaviour, cap 0/clamping, refresh/deadband/clock jump,
  latched-ping refresh without a target move, roster- and cap-change bypass,
  the population rule (spectator/bot/connecting/999 exclusion), `delay_for`,
  1 ms ramp both ways, ramp-out via `stop`, effective ping, engage/release
  edges (including log-only preview and log off/on), and the exact log-line
  shapes.
- netStatus table: `eff` column (`ping + applied delay`) after `ping`, header
  and rule pinned by a unit test; runtime parser regex updated.
- Runtime harness: `tools/runtime/a8_ping_equalize.sh` + `.gdb` (modelled on
  a3) runs two synthetic clients through the a2 protocol harness with different
  one-way delays (15 ms and 150 ms), samples both clients'
  `level.clients[i].ps.commandTime - level.time`, `ps.ping` and engine
  `cl->ping` at `SV_SendClientMessages` (after the post-forward write, before
  the snapshot), enables a 200 ms cap by rcon, checks that the fast client's
  clock shift moved by the expected `-(target - ping)` and that its `ps.ping`
  reads the target, that the slow client is untouched, then sets the cap to 0
  and checks the ramp back and the log release line.
- Observed a8 result (2026-11-12, `sv_fps 20`, 30 packets/s, two synthetic
  clients): fast client shift `-107 -> -257` ms (+/-3 ms over 120 samples),
  target `200`, `ps.ping 50 -> 200`, slow client `-405 -> -405`; after
  `proxy_sv_pingEqualize 0` the fast shift returned to `-107`. With the feature
  off, `ps.ping` equalled `cl->ping` for both clients.
- Harness findings. The synthetic clients connect as free spectators (the game
  never auto-joins FFA), so a2's `--mode equalize` sends `team free` after
  connect (`join_after`); without it the population rule correctly excludes
  them and nothing is delayed. Sampling `ps.ping` at `GAME_RUN_FRAME` *entry*
  reads the value `SV_CalcPings` wrote at the top of the frame, not the
  proxy's post-forward value, so a8 samples at the engine's
  `SV_SendClientMessages` instead. The plan said "per GAME_RUN_FRAME"; the
  normalised sample point is the refinement that makes the `ps.ping`
  assertion meaningful.
- Regression: a2 single still produces the same `cl_timeNudge` estimate inside
  the reported range (`-13.65`, true `-15`, `inside yes`) with the new `eff`
  column; a3 still shows the `-14` ms timeNudge correction and the real
  setting in the table.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, the full unit suite
  (544 tests) and the i686 release build pass.

## Open points / assumptions to verify

1. **`ps.ping` offset at runtime.** `OFFSET_PS_PING = 524` is an SDK layout
   result; the a8 harness verified at runtime that `ps.ping == cl->ping` with
   the feature off, and that the write makes it read the target while a delay is
   applied. A stock engine build is what the harness runs (`linuxjampded` from
   the 2003 release assets), so this is verified for that binary; a different
   engine build would need the same check.
2. **The 64-command replay window** is the documented limit at high
   `cl_maxpackets`; measure the actual threshold on a real TaystJK client
   before recommending any cap above 250.
3. **A real client at the cap** must behave like a natural high-ping
   connection (no extra artefacts beyond prediction correction) and must not
   log "exceeded PACKET_BACKUP on commands". This needs one real-client run.
4. **The 32-snapshot average** at `sv_fps 10` (window ~3.2 s) with the 500 ms
   refresh and the 10 ms deadband is assumed smooth enough; a noisy server may
   still retarget on roster changes, which is intentional.
5. `cl->ping` includes the client's ack wait, so "equalised ping" is equality
   of the engine metric (the scoreboard number), not a true one-way delay.
6. The log-only preview uses the maximum supported cap; this is an
   interpretation of "works without enforcement", not a planned value.
