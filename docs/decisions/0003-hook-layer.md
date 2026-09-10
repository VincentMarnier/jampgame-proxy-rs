# D-003 — Hook layer: dropped verified-no-op rewrites + call-encoding fix

Date: 2026-09-10. Status: accepted. Evidence: `docs/reverse-engineering.md`
R-021 (RUNTIME + STATIC objdump diffs against the shipped binary); source
authority: AGENTS.md (shipped binary wins where source/binary diverge).

## Context

Porting the D-002 keep set surfaced three facts not covered by the source-only
scope analysis:

1. The shipped `SV_ExecuteClientMessage` already contains both hack guards.
2. The shipped `SV_ReadPackets` (`SV_PacketEvent`) already has both the
   connectionless dispatch and the translated-port fix.
3. A Rust direct `call <absolute engine address>` is not text-relocated in a
   shared object, so it lands wrong under ASLR.

## Decisions

### D-003.1 — Drop `SV_ExecuteClientMessage` and `SV_PacketEvent` detours

`SV_ExecuteClientMessage`: the binary at `0x804e8c4` has the
`messageAcknowledge < 0` early return (`0x804e9c5`) and the `reliableAcknowledge`
range clamp (`0x804e9d2`) already. The original proxy's rewrite copied them
verbatim; its only real additions were the ping-fix (dropped, D-002) and the
netStatus per-usercmd feed, which is now the `SV_ClientThink` call-site
retarget (`0x804e891`). `SV_PacketEvent` (`0x8057024`): the pristine body
already dispatches connectionless packets and does the qport translated-port
fix; the proxy's rewrite is verbatim. Both detours are dropped; the inventory
(`docs/inventory.md`) records the rationale. No behavior loss vs the original
proxy on a pristine engine.

### D-003.2 — Engine-address calls must be indirect (volatile-loaded)

A Rust transmute of a constant engine address to a function pointer compiles
to a direct `call rel32` with no `.rel.text` relocation in the .so; the loader
leaves the link-time-base-relative displacement, so the call targets
`base+site+rel32` garbage under ASLR (crash PC matched the formula exactly).
All engine-function calls use `engine_fn!` (`rust/src/engine.rs`), which loads
the absolute address through a volatile `static` so the call is indirect.
Game-module calls are unaffected (they rebase by the runtime `dladdr` base).

### D-003.3 — netStatus packet identity uses `cl->messageAcknowledge`

The original fed `packetIndex = cmdIndex (pre-loop)` from inside its
`SV_UserMove` rewrite. The call-site stub (D-002.1) has no message-loop
context, so it uses `cl->messageAcknowledge` as the packet identity: stable
across one message's usercmds, distinct across messages, so `CalcPacketsAndFPS`
still counts packets (not cmds). The stored value differs from the original's
only where it is compared for inequality. Documented in `netstatus.rs`.

## Consequences

- D-002.1 "minimal alteration" holds: no whole-function body was copied; the
  netStatus feed and the `SV_SendClientGameState` guard are call-site / entry
  injections, and the two originally-whole-function hooks are gone.
- New absolute engine-address calls must use `engine_fn!`, never a direct
  constant transmute.