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
`proxy_sv_clientPpsLog` must only print something when a client is above its
limit.

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

For each client the effective limit is an integer downscale of the FPS,
evaluated over the estimate's `±2` uncertainty with the **highest** result
winning:

```text
limit = max over f in estimated_fps-2 ..= estimated_fps+2 of
            min(f / ceil(f / globalMaxPps), globalMaxPps)
```

The accepted rate is therefore an exact `1/K` fraction of the client's rate
(`K = ceil(f / globalMaxPps)`), so the eviction pattern is a clean "drop one
out of every K packets", and the uncertainty can only raise the limit. With
`144 fps` and a ceiling of `100`, the samples `142..146` produce
`71, 71, 72, 72, 73`, so the limit is `73` (the highest). Below the ceiling
`K = 1` and the highest sample is `fps + 2`. `globalMaxPps = 0` means no
ceiling (limit = `fps + 2`). If the FPS cannot be trusted yet, the global
ceiling alone is enforced (with `0` there, the limiter fails open and accepts
everything).

### D-014.2 — FPS comes from the shared usercmd record feed

The netStatus machinery already records, per packet, the new (non-duplicate)
usercmds the engine processed (`feed_cmd` runs from `SV_ClientThink`; duplicates
are dropped by `SV_UserMove` before the feed). To keep the limiter independent
of `proxy_sv_enableNetStatus`, the record feed now runs when **either**
`proxy_sv_enableNetStatus` or `proxy_sv_enableClientPpsLimit` is on.

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

### D-014.4 — Logging only above the limit

`proxy_sv_clientPpsLog 1` prints a line only for a completed window whose
measured rate is above its effective limit (FPS-derived, capped by the global
maximum); clients inside their limit produce no output. The line carries the
observed rate, accepted and dropped counts, the limit, the FPS estimate, the
window length and the client identity.

### D-014.5 — Scope and fail-open rules

- Every client is measured/filtered, LAN/loopback included.
- Bots never send client packets, so they never reach the hook.
- An unknown/out-of-range `client_t*` fails open.
- With the feature off the wrapper is a plain passthrough behind one atomic
  check; `proxy_sv_enable 0` detaches the hook entirely.
- Per-client state is reset on first connect and dropped with the slot on
  disconnect; the feature off→on edge resets every window.
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
  inside-limit samples, idle-gap reset and backwards-time reset.
- `rust/src/patch.rs`: `SV_Netchan_Process` steal-length fixture (6 bytes).
- Runtime differential validation on the i386 engine is not yet recorded; the
  over-limit log is the intended first live check.
