# D-014 — Per-client packet-rate limiting derived from FPS

Date: 2026-11-02. Status: accepted (implemented). Evidence:
`original/jedi-academy/codemp/server/sv_main.cpp:575-638`,
`original/jedi-academy/codemp/server/sv_net_chan.cpp:164-178`,
`original/jedi-academy/codemp/qcommon/net_chan.cpp:217-330`,
`original/jedi-academy/codemp/client/cl_input.cpp:2370-2600`,
binary `original/jalinuxded_1.011/linuxjampded` (`SV_Netchan_Process` @
`0x8057e14`), `rust/src/pps.rs`, `rust/src/hooks/netstatus.rs`.

## Context

The user wants one cvar that switches on a per-client incoming packet-rate
limiter. When active, each client's **maximum allowed pps is computed from its
own measured FPS**, not from a fixed server-wide number; the per-client value
is an integer downscale of that FPS, capped by a global maximum.
The limiter must drop excess packets evenly through the second, and
`proxy_sv_clientPpsLog` must report when a client starts and stops being above
its limit (not once per window).

An earlier iteration exposed fixed caps (`proxy_sv_maxClientPps`,
`proxy_sv_maxAllowedPps`); this record replaces that design.

## Why FPS is the legitimate bound

A stock client sends **at most one packet per generated usercmd**: `CL_SendCmd`
runs once per frame and calls `CL_WritePacket` at most once, while
`cl_maxpackets` only throttles it further (`CL_ReadyToSendPacket`,
`cl_input.cpp:2327-2395`). One usercmd is generated per frame
(`CL_CreateNewCommands`). Therefore a legitimate client's packet rate cannot
exceed its frame rate, regardless of `cl_packetdup` (duplicates are carried
inside packets, not as extra packets).

A client that sends more packets than it produces commands is sending
duplicated/empty packets: the anti-cheat shape this feature targets. Packets
that carry genuinely new usercmds raise the measured FPS and therefore the
limit; limiting the command rate itself is a different concern.

## Where the rate is observed and filtered

The engine's `SV_PacketEvent` calls
`SV_Netchan_Process(client_t *client, msg_t *msg)` once per sequenced packet
that matched a known client by address and qport (`sv_main.cpp:598-632`). That
is the first point with the `client_t*`, so it is the observation/filter point.

Returning `qfalse` is the engine's own "do not process this packet" path
(out-of-order/duplicate or incomplete fragment, `net_chan.cpp:227-330`): no
decode, no `SV_ExecuteClientMessage`, no `lastPacketTime` refresh. To the
netchan and client it is a lost UDP packet, which the protocol already
tolerates (UDP loss, reliable retransmission, `cl_packetdup` usercmd
redundancy). The pristine body is called through the trampoline for accepted
packets (entry wrapper, D-002.1).

The binary at `0x8057e14` starts `55 8b ec 83 ec 18 89 7d f4 89 75 f8`
(`push %ebp; mov %esp,%ebp; sub $0x18,%esp; ...`), a 6-byte whole-instruction
steal, already in the verified engine address table
(`engine::functions::SV_NETCHAN_PROCESS`, R-010/R-018) and pinned by the
`patch.rs` decoder fixture.

## Decisions

### D-014.1 — One activation cvar; the limit is per client

- `proxy_sv_enableClientPpsLimit` (default `0`): `0` off, non-zero on.
- `proxy_sv_globalMaxPps` (default `100`): hard global ceiling per client
  (`0` = no ceiling).
- `proxy_sv_clientPpsLog` (default `0`): over-limit logging.
- `proxy_sv_clientPpsFpsMargin` (default `2`): half-width, in FPS, of the
  uncertainty band around the measured FPS (the fixed `2` of the original
  design, now operator-tunable).
- `proxy_sv_clientPpsTolerance` (default `1`): packets/second of slack added
  after the FPS band and the global ceiling, so a legitimate client that sends
  one packet more than the derived limit is not filtered.

For each client the effective limit is an integer downscale of the FPS,
evaluated over the estimate's `±margin` uncertainty with the **highest** result
winning, plus the tolerance:

```text
limit = max over f in estimated_fps-margin ..= estimated_fps+margin of
            min(f / ceil(f / globalMaxPps), globalMaxPps)
        + tolerance
```

The accepted rate is therefore an exact `1/K` fraction of the client's rate
(`K = ceil(f / globalMaxPps)`) plus a fixed slack, so the eviction pattern is a
clean "drop one out of every K packets", and neither the uncertainty nor the
slack can lower the limit. With `144 fps`, a ceiling of `100` and the defaults,
the samples `142..146` produce `71, 71, 72, 72, 73`, so the limit is `74`
(highest + tolerance). Below the ceiling `K = 1` and the highest sample is
`fps + margin` (plus tolerance). `globalMaxPps = 0` means no ceiling
(limit = `fps + margin + tolerance`). A margin of `0` uses the measured FPS
alone: the tightest limit, and the most exposed to a single bad reading; the
value is clamped to `0..=60`, and the tolerance to `0..=1000`. If the FPS
cannot be trusted yet, the global ceiling plus the tolerance is enforced (with
`0` there, the limiter fails open and accepts everything, because `0` is the
fail-open sentinel and is never raised by the tolerance).

The tolerance is what makes a `101`-packet/s client acceptable against the
default ceiling of `100`, and it also covers the case where the FPS is
*measured* a little low: at `98` measured fps the `±2` band already lifts the
limit to `100`, and the tolerance accepts `101`. Set it to `0` to enforce the
exact derived limit.

### D-014.2 — FPS comes from the shared usercmd record feed

The netStatus machinery already records, per packet, the new (non-duplicate)
usercmds the engine processed (`feed_cmd` runs from `SV_ClientThink`; duplicates
are dropped by `SV_UserMove` before the feed). To keep the limiter independent
of `proxy_sv_enableNetStatus`, the record feed now runs when **any** of
`proxy_sv_enableNetStatus`, `proxy_sv_enableClientPpsLimit` or
`proxy_sv_clientPpsLog` is on.

**There is one FPS calculation.** The limiter calls
`netstatus_est::fps_pps` — the exact function behind the netStatus table's
`fps` column — so the two features cannot disagree. The limiter adds only two
guards before trusting the value:

- the newest committed usercmd must have arrived within 2 s (a silent client
  keeps its last record and must not be filtered on a stale rate);
- the commands in the one-second window must span at least `FPS_MIN_SPAN_MS`
  (900 ms). `fps_pps` is a raw per-window count, so a partially filled window
  would read as a lower FPS and create a false limit; such a window yields `0`
  (fail open).

The value is refreshed once per second. There is no span normalization and no
FPS cap (the earlier `FPS_MAX = 1000` was a guard for the deleted normalized
estimate); the enforced limit is bounded by the configurable
`proxy_sv_globalMaxPps` instead.

Dropping packets can, in principle, remove commands from the measured stream.
In practice the targeted traffic is duplicated/empty packets: the same commands
remain in the surviving packets, so the FPS estimate stays representative
rather than collapsing.

### D-014.3 — One-second windows and evenly spread drops

Packets are read in per-server-frame batches (`Com_EventLoop` runs before
`SV_Frame`, `common.cpp:1780-1812`), so a legit client's packets share nearly
the same processing timestamp. A tight per-packet token bucket would drop part
of every legit batch. Instead:

- count every packet that reaches the hook (accepted + dropped) over a window;
- when the window is at least 1 s old, derive
  `rate = observed * 1000 / window_ms` and use it for the next window;
- the first window is measurement only; a 2 s silence discards the window and
  the stale rate; a backwards `svs.time` discontinuity also discards it;
- above the limit, each packet adds `rate - limit` to an accumulator and is
  dropped when the accumulator reaches `rate`, i.e. a deterministic
  drop-every-`rate/(rate-limit)`-th-packet pattern. With `limit = 63`,
  `rate = 100`, exactly 63 of 100 packets are accepted and 37 dropped,
  interleaved (about two accepted, one dropped), never a tail truncation.

`svs.time` is the clock: it is the engine's own server clock and the same value
every other proxy feature uses.

### D-014.4 — Edge-triggered logging

`proxy_sv_clientPpsLog 1` logs **transitions**, not every window: one line when
a client is first seen above its limit, one when a completed window brings it
back under, and another if it starts again. A client staying on the same side
of its limit produces no output. The line carries the observed rate, the limit,
the FPS estimate, the window length and the client identity (the raw
observed/accepted/dropped counters are limiter internals and are not printed).
The log cvar is sufficient on its own: it keeps the accounting and the
per-usercmd FPS feed running without `proxy_sv_enableClientPpsLimit`, so it
reports the limit the limiter *would* enforce. While the cvar is `0` no edge
state is kept, so enabling it reports the next over-limit window.

### D-014.6 — One shared over-limit verdict; the netStatus table renders it

The rate accounting (counting every incoming sequenced packet over a one-second
window) runs whenever **any** of `proxy_sv_enableClientPpsLimit`,
`proxy_sv_enableNetStatus` or `proxy_sv_clientPpsLog` is on; only the dropping
of the excess requires the limiter cvar. The last completed window is kept as a
[`PpsSample`](../../rust/src/pps.rs) and `PpsSample::over_limit()` is the single
predicate behind both surfaces, so the log line and the table cannot disagree:

- `proxy_sv_clientPpsLog` logs the `over_limit` **transitions**: one line when
  a client starts using too many packets, one when a completed window brings
  it back under, and another if it starts again (same-side windows are silent);
- the netStatus `packets/s` cell shows the limiter's measured rate instead of
  the `netstatus_est::fps_pps` packet count, because the record ring only
  counts packets that carried **new usercmds**. A client flooding
  empty/duplicate packets (the shape D-014 targets) left the old cell at
  `pps = fps`; the limiter's counter sees those packets. Before the first
  completed window the cell falls back to the record-ring value.
- When the client is over its limit the cell is red (`^1`), in a right-aligned
  9-column. With `proxy_sv_enableClientPpsLimit` on it shows the **effective**
  rate — the observed rate capped at the limit, i.e. what the limiter lets
  through: 200 pps observed with a limit of 100 renders
  `^1      100^7`. With the limiter off there is no effective rate, so it
  appends the limit the limiter would enforce in the default colour (`^7`
  before it, `^1   100^7/73`), flagging over-use without pretending the packets
  are being dropped.
- A completed window older than 2 s is stale (the client went silent) and is
  neither shown nor flagged; resetting the windows (connect, idle, backwards
  `svs.time`, accounting off→on, enforcement off→on) clears the shared status.

### D-014.5 — Scope and fail-open rules

- Every client is measured/filtered, LAN/loopback included.
- Bots never send client packets, so they never reach the hook.
- An unknown/out-of-range `client_t*` fails open.
- With all three cvars off the wrapper is a plain passthrough behind one atomic
  check; with only the netStatus table or `proxy_sv_clientPpsLog` on it
  accounts packets without ever dropping; `proxy_sv_enable 0` detaches the
  hook entirely.
- Per-client state is reset on first connect and dropped with the slot on
  disconnect; starting accounting or enforcement resets every window.
- Enabling the feature does not require enabling the netStatus table.

## Limitations (explicit)

- The FPS-derived part of the limit follows the **processed unique usercmd
  rate**. A modified client that floods genuinely new commands raises its own
  FPS estimate; the `proxy_sv_globalMaxPps` ceiling bounds the damage, but a
  command-rate problem is not addressed here.
- This limits packet count per second, not bytes or usercmds per packet;
  bandwidth remains `rate`/`sv_maxrate`'s job.
- Only sequenced packets from a matched client are accounted; connectionless
  and unknown-address traffic keeps the engine's own handling.
- The first second (and the first second after a 2 s silence) is measurement
  only, so a short burst is not filtered by design.
- The FPS estimate follows the client's command stream; a client that stops
  producing commands fails open rather than being filtered.

## Verification

- `rust/src/pps.rs` unit tests: FPS from the shared `fps_pps` calculation,
  rejection of short/stale command streams,
  `limit = max over f in fps-2..=fps+2 of
  min(f / ceil(f / globalMaxPps), globalMaxPps)`, one-second refresh,
  first-window measurement, exact-limit pass, one-over-limit filtering, even
  drop spread (no tail truncation, longest accept run), over-limit vs
  inside-limit samples, idle-gap reset and backwards-time reset, the shared
  status: measurement-only (no drops) reports the excess, a fail-open limit
  never flags, the status is absent while the first window fills and expires
  on silence, and an idle reset clears it; and the log edge: over → under →
  over transitions arm/clear/re-arm, a full reset clears it, clearing it while
  the log cvar is off leaves the measurement windows intact, and an idle gap
  keeps it; the log line reports only the rate, limit, FPS, window length and
  client identity (no raw observed/accepted/dropped counters).
- `rust/src/hooks/netstatus.rs` unit tests: the `packets/s` cell stays 9
  visible columns, is plain inside the limit, is red and rate-only while the
  limiter is on, renders `^1<actual>^7/<limit>` while it is off (including
  drops on an at-limit window) and never truncates a wide limit.
- `rust/src/patch.rs`: `SV_Netchan_Process` steal-length fixture (6 bytes).
- Runtime differential validation on the i386 engine is not yet recorded; the
  over-limit log is the intended first live check.
