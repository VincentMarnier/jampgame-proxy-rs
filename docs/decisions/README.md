# Decision records

- `0001-bootstrap-core.md` — bootstrap core + trap interceptions milestone:
  scope, the i386 stack-alignment solution, and deferred items.
- `0002-scope.md` — feature scope (wanted / not-wanted lists) and the
  minimal-alteration principle: inject code at call sites and inside functions,
  never rewrite whole game functions.
- `0003-hook-layer.md` — hook-layer decisions: two verified-no-op rewrites
  dropped (`SV_ExecuteClientMessage`, `SV_PacketEvent`), the indirect-call
  encoding fix for engine addresses, and the netStatus packet-identity
  deviation.
- `0004-team-lock.md` — Rust-only `proxy_sv_lockTeams` (TDM/CTF): reconnect-burst
  snapshot, even/>=2 lock rule, `SetTeam` enforcement, and the TDM/CTF scope.
- `0005-team-size-rules.md` — Rust-only `proxy_sv_teamSizeRules`: per-team-size
  `timelimit`/`fraglimit`/`capturelimit` overrides at round start, nearest
  smaller size fallback, independent of the lock boolean.
- `0006-parallel-tffa.md` — parallel TFFA matches: per-match score copies,
  per-viewer `scores`/`cs` rewrites, and physics isolation via trace
  interception.
- `0007-map-change-module-pinning.md` — map-change crash: rcon commands run
  inside our `SVC_RemoteCommand` wrapper, the engine `dlclose`s the proxy
  mid-call, and the stale return faults. Fix: pin the module for the process
  lifetime and reset per-session state at `GAME_INIT`.