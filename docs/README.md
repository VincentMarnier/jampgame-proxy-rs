# Documentation

This directory contains knowledge derived from the original implementation and the development of the Rust rewrite.

## Categories

- `architecture.md` — high-level architecture and component relationships (includes source authority: SDK authoritative for game ABI).
- `reverse-engineering.md` — discoveries about symbols, ABI, hooks, binaries, and runtime behavior.
- `uncertainties.md` — unresolved questions and hypotheses.
- `runtime-environment.md` — how to build and run the original binaries in a container for runtime investigation (Dockerfiles, layout, verified behaviour).
- `demo-format.md` — server-side demo recording: Phase 0 evidence (protocol 26
  source-tree drift, the `.dm_26` container/gamestate/snapshot wire layout, the
  static `msg_hData` Huffman fact, `PMF_FOLLOW` spectator evidence, the
  `SV_SendClientMessages` hook point, pinned engine addresses) and the Phase 1
  pure-Rust writer/encoder notes (naming, retention, buffering, RMG refusal,
  generated tables, byte-exact fixture test) and Phase 2 recorder-core notes
  (tick hook, frame ring/scratch, `cs` mirroring, `recorder_build`, gate-A
  artifacts and playback limitation) and the Phase 3 instance-manager notes
  (one recorder per TFFA match/duel court, per-instance subjects and identity,
  per-instance configstrings/scoreboard, the chat rule, `GAME_INIT` lifecycle,
  the 16-recorder cap, gate-B artifacts and the real-client playback run) and
  the Phase 4 publication notes (proxy-hosted HTTP listener and worker model,
  JSON `/demos` and file routes, path safety, served-only-when-closed rule,
  retention and the accepted size/free-space guards, the intermission client
  block, `/demos` in game, gate-C artifacts and deviations). The decisions
  behind the feature, with their evidence level and verification status, are in
  `decisions/0027-server-demos.md`.
- `inventory.md` — per-feature map of the original proxy: keep/drop decision
  per D-002 and the implementation technique for each hook.
- `ybeproxy-2026-delta.md` — the 2026 security/crash fixes of the C++ proxy
  (JKA_YBEProxy) and which of them this rewrite still lacks; tests and
  priorities for the later work.
- `decisions/` — important architectural decisions once the project becomes more mature.

## Evidence levels

Use explicit evidence labels where useful:

- **SOURCE** — directly confirmed by source code.
- **STATIC** — confirmed through binary/static analysis.
- **RUNTIME** — observed during execution.
- **TESTED** — confirmed through an automated test.
- **INFERRED** — a reasonable interpretation but not yet verified.

Do not treat inferred information as verified behavior.