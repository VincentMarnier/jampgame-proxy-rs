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

The delay is applied in the existing `sv_client_think_stub` (the retargeted
`call SV_ClientThink` inside `SV_UserMove`, D-015.2, patch
`CALL_SV_CLIENT_THINK` at `0x804e891`), inbound only: no engine network or
snapshot-path change.

**Mechanism superseded by D-028.8.** The first implementation shifted
`ucmd.serverTime` back by the delay (a negative offset composed with the
timeNudge correction). The scope decision (inbound only) is unchanged, but the
shift itself was wrong for a real client: it desynchronises `ps.commandTime`
from the state, and client prediction replays already-applied moves. The stub
now holds commands in a queue and hands the game an older one; the estimator
still receives the **raw** stamp, and `cl->lastUsercmd.serverTime` is restored
to the newest raw stamp after the call, so the engine's `cl_packetdup`
duplicate filter keeps comparing client stamps.

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
- `cl->ping` in `0..999` (`999` is the engine's "no reading" sentinel). A 0 is
  a real reading — a localhost/LAN player — not a missing one; bots, the other
  source of a 0 in `SV_CalcPings`, are excluded by the bot check above. This is
  the same rule D-007 adopted when the netStatus feed dropped the original's
  `ping >= 1` gate, which had wrongly excluded zero-ping players;
- game session team `!= TEAM_SPECTATOR` (read from the located `gclient_t`
  array, `sess.sessionTeam`).

Spectators, still-connecting clients and bots are ignored and never padded. A
zero-ping player is not: on a mixed LAN/WAN server they are the player who
needs the padding the most (review correction, 2026-11-13 — the first
implementation followed the plan's `1..999` literally and left them out).

### D-028.5 — Tracking

The target is recomputed from `cl->ping` at most every 500 ms (the engine's own
value is a 32-snapshot average, `SV_CalcPings`). It moves only when it differs
by at least `TARGET_DEADBAND_MS = 10`. A change of the member set (join, leave,
spectator, return to play) retargets immediately, bypassing the deadband; a cap
change retargets immediately too (added so an admin change is not held back by
the deadband). The per-client queue depth ramps one command per 100 ms
(D-028.8).

The member pings are latched at each recomputation and the per-client delay
target is derived from that snapshot, so the 500 ms/deadband rule also smooths
the delay target. A frame with no due recomputation keeps the previous target
and pings.

### D-028.6 — Ramp and off semantics

**Superseded by D-028.8.** The 1 ms-per-usercmd ramp belonged to the stamp-shift
mechanism; the queue ramps its depth one command per 100 ms instead. The off
semantics below are unchanged:

- `proxy_sv_pingEqualize 0` sets every target to 0; the depth ramps down and
  the held commands drain instead of the clock jumping forward.
- `proxy_sv_enable 0` detaches the hook layer immediately (no more stub) and
  drops the queues; the one following command is processed immediately. This
  is the same documented consequence as every feature behind the master switch.

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

### D-028.8 — Delay the processing, not the stamp (2026-11-13)

The first implementation rewrote `ucmd.serverTime` by `-delay` (the D-015
`cl_timeNudge` technique). A **real-client** test on the shipped client showed
`eff = 150` in the netStatus table as designed, but movement was not a smooth
delay: it stuttered and rubber-banded.

Cause (engine+cgame source, and the reason the synthetic harness missed it):
`SV_ClientThink` applies the move immediately, but the snapshot then carries a
`ps` whose state already includes those moves while `ps.commandTime` claims it
is 150 ms older. `CG_PredictPlayerState` starts from `cg->snap->ps` and replays
every command with `cmd.serverTime > snap->ps.commandTime`
(`cg_predict.c:1170-1180`), so the client re-applies ~150 ms of commands on top
of an already-current position; the next snapshot snaps it back. Constant
overshoot and correction. A1–A8 used protocol-only clients with no cgame
prediction, so they could not see it.

The delay is now a **per-client FIFO of usercmds**:

- every arriving command (after the anti-macro guard) is queued;
- the stub releases the oldest held command(s) instead of the arriving one:
  `queue.len() - depth`, at most `MAX_RELEASES` (4) per call, so a catching-up
  queue drains at a bounded rate;
- the game therefore processes a command with its **original** stamp one queue
  interval later; `ps.commandTime` and the state stay consistent, exactly like
  a slower link, and the client's prediction replay covers only commands the
  server has not applied yet;
- the queue depth is `round(delay / command_interval)`, where the interval is
  an EMA of the client's own stamp spacing (clamped 1..100 ms), and it moves at
  most one command per `RAMP_STEP_MS` (100 ms). Each step holds or drains one
  command, i.e. a pause/catch-up of one command interval — never a burst and
  never the whole delay at once;
- `MAX_DEPTH` is 48 commands and the queue holds 64; a full queue drops its
  oldest entry. The delivered delay is quantised to the client's command
  interval (8–33 ms), so "150 ms" means the nearest command count;
- the timeNudge correction is applied to the **released** command's stamp (it
  is a per-command clock correction, not a delay); `cl->lastUsercmd.serverTime`
  is set to the newest raw stamp seen, so `SV_UserMove`'s `cl_packetdup` filter
  keeps working and held commands are not re-delivered;
- commands are only released when new ones arrive, so a client that stops
  sending has nothing to process (as on a real link); connect, disconnect,
  `GAME_INIT` and the master switch drop the queue.

The display (D-028.3), population (D-028.4), tracking (D-028.5) and cvars
(D-028.7) are unchanged. `applied` (ms) is now `depth x interval`, so the
`eff` column and the scoreboard value report the delay actually in effect.

### D-028.9 — Split the delay: view clock + action queue (2026-11-13)

The queue-only mechanism hit a hard client limit at high frame rates. A
real-client run with `proxy_sv_pingEqualize 150` showed the lagometer overlay
("Connection Interrupted", `cg_draw.c:5030`) flashing several times a second.
The diagnostic line (log level 2) showed why:

```text
pingEqualize debug client=2 target=150 reported=150 view=0 action=0 depth=48/48
interval=2 applied=96 queue=48 ping=0
```

The client creates one usercmd **per rendered frame**, so at 500 commands/s
(`interval = 2 ms`, the `fps` column's 200 is the packet rate) its
`CMD_BACKUP = 64` replay ring covers only 128 ms. The requested 150 ms needs
75 commands, hits `MAX_DEPTH = 48`, delivers 96 ms, and even that leaves the
ring almost full. `CG_DrawDisconnect` draws its overlay exactly when
`current - 63 > snap->ps.commandTime`, and `CG_PredictPlayerState` stops
predicting at the same boundary ("exceeded PACKET_BACKUP on commands"). Raising
`com_maxfps 125` made the flashing stop, which confirmed the ring — but a
client-side requirement is not acceptable for this feature.

A real connection of the same round trip does not spend the ring: the client's
own clock is shifted by the one-way delay (`CL_AdjustTimeDelta` derives it from
`snap.serverTime`), so its view lags and its command stamps lag with it, and
the replay depth stays at the in-flight commands. The server can reproduce that
without touching the client: shift the **snapshot time** it sends.

The delay is therefore split in two halves:

- **View half** — the `call SV_WriteSnapshotToClient` inside the pristine
  `SV_SendClientSnapshot` (0x8058e9c, target 0x8058034) is retargeted to a stub
  that writes `svs.time - view_ms` for the duration of that call and restores
  it afterwards. The snapshot is built with the shifted time, so the client's
  clock — and its view — lag by `view_ms`, and its command stamps follow; this
  costs no replay-ring depth. `SV_SendMessageToClient` then records
  `frames[].messageSent` from the restored value, so the engine's ping stays
  the real network ping. The message is never parsed or rewritten: the engine
  Huffman-compresses it before the send hook, and rewriting the encoded stream
  corrupted it (the client dropped with "Invalid size ... for areamask"). The
  static writer is called through `engine_fn!`: a constant `transmute` compiles
  to a PC-relative call based on the .so's link-time base, which under ASLR
  lands at `base + address` (observed as a jump to `0xfcb41034` and a SIGSEGV).
- **Action half** — the existing command queue, sized to `action_ms`. Only this
  half is paid for out of the client's ring, so
  `action_ms <= RING_SAFE_FRAMES (48) * interval - AHEAD_MS (50)`, and the view
  half takes the rest. A 150 ms delay at 500 commands/s becomes view 104 /
  action 46 (the 48-frame budget minus one snapshot interval); at 50 commands/s
  it is 75/75 like a real link.

`split(delay, interval)` is a pure function (`pingequalize::split`), pinned by
unit tests. The frame pass sets `view_ms`/`action_ms` from the deadbanded
target; `depth_target` derives from `action_ms`; `applied` (the displayed
effective delay) is `view_ms + depth * interval`.

Two consequences are handled explicitly:

- **The `timeNudge` estimator must subtract the view shift.** Shifting the
  snapshot time is indistinguishable, from the server's side, from a client
  clock nudge of `+view_ms` (`cl_timeNudge` does exactly the same thing to the
  stamped clock). `hooks::netstatus::compute` removes the
  `state.clients[i].ping_equalize.view_ms` offset from `tn`/`lo`/`hi`, so
  `proxy_sv_voidTimeNudge` and the table keep reporting the client's real
  setting instead of fighting the equaliser.
- **The view shift ramps one millisecond per snapshot sent** (20 ms/s at
  `sv_fps 20`), because `CL_AdjustTimeDelta` is built to walk the clock by
  1-2 ms per snapshot: a backward step larger than 100 ms would take the
  fast-adjust path and a step of 75 ms would be absorbed only through
  extrapolation. With the ramp the client follows a 104 ms view shift in about
  5 s without a hitch; the queue still ramps its depth one command per 100 ms.
- **The harness now parses the snapshot header** (server time, delta, flags,
  area-mask length), so a corrupted snapshot fails `a2`/`a8` instead of only a
  real client; that check is what caught the first view-half attempts. `a8`'s
  shift assertion accepts any shift between the action half and the full
  delay, because the gdb stalls during sampling make the client extrapolate and
  undo part of the view absorption; the full applied value is pinned by the
  `ps.ping` check.

The `ps.ping` display, the population, the tracking rules and the cvars are
unchanged; `eff`/the scoreboard report `ping + view + action`, the effective
round trip.

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
- Real-client test of the queue-only mechanism (2026-11-13): with the delay at
  150 ms and a 500 commands/s client, `depth` capped at 48 (96 ms delivered)
  and the lagometer overlay flashed; `com_maxfps 125` removed it, confirming
  the 64-command ring (D-028.9).
- Split mechanism, synthetic check (2026-11-13): with `view` 51 / `action` 51,
  the fast client's newest command stamp moved from -61 ms to -107 ms relative
  to `level.time` (the view shift reached the client's clock) while the queue
  gap stayed at 66 ms (the action half only), and `ps.ping` read
  `ping + view + action`.
- Real-client run with the final mechanism (2026-11-13, 500 commands/s,
  `proxy_sv_pingEqualize 150`): smooth, no lagometer overlay, no
  "exceeded PACKET_BACKUP on commands"; `/netStatus` read `eff` 150 and the
  server log showed `view` ramping to 104 with `action` 46. The two earlier
  real-client runs produced the findings behind D-028.8 and D-028.9.
- Earlier real-client test (2026-11-13), and the finding that changed the
  mechanism:
  with the stamp-shift implementation the `eff` column read 150 ms as designed,
  but movement stuttered. Root cause and fix in D-028.8 (the snapshot's
  `ps.commandTime` no longer matched the state it described, so cgame replayed
  already-applied moves). The queue mechanism is pinned by
  `the_queue_depth_rounds_the_delay_to_the_client_interval`,
  `the_queue_holds_commands_and_releases_the_oldest`,
  `a_steady_queue_holds_the_delay_and_processes_one_command_per_arrival` and
  `disabling_ramps_the_depth_down_and_drains_the_queue`; the real-client retest
  is the immediate next step.
- Review correction (2026-11-13): the population admits `cl->ping == 0`
  (localhost/LAN) and the `ps.ping` overwrite is gated on membership rather
  than on the ping range, so a 0 ms player is padded like any other and a
  ramping-out non-member never rewrites the game-side ping. Pinned by
  `a_zero_ping_localhost_player_is_padded_like_any_other` and the updated
  population test.
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
3. **A real client at the cap**: validated at 150 ms on 2026-11-13 (smooth,
   no overlay, no PACKET_BACKUP). The 250 ms cap was not exercised on a real
   client; the action half is auto-capped by the ring, but the view half's
   250 ms is worth one run before recommending it.
4. **The 32-snapshot average** at `sv_fps 10` (window ~3.2 s) with the 500 ms
   refresh and the 10 ms deadband is assumed smooth enough; a noisy server may
   still retarget on roster changes, which is intentional.
5. `cl->ping` includes the client's ack wait, so "equalised ping" is equality
   of the engine metric (the scoreboard number), not a true one-way delay.
6. The delivered delay is quantised to the client's command interval
   (8-33 ms at 125-30 commands/s) and rounded to the nearest command, so two
   players can differ by up to half an interval (D-028.8).
6. The log-only preview uses the maximum supported cap; this is an
   interpretation of "works without enforcement", not a planned value.
