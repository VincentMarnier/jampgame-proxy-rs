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
  snapshot, minimum-team-size rule (per TFFA instance since D-004.5), `SetTeam`
  enforcement, and the TDM/CTF scope.
- `0005-team-size-rules.md` — Rust-only `proxy_sv_teamSizeRules`: per-team-size
  `timelimit`/`fraglimit`/`capturelimit` overrides at round start, nearest
  smaller size fallback, independent of `proxy_sv_lockTeams`; with parallel TFFA it
  uses the per-match sizes and applies one rule only when every populated match
  agrees.
- `0006-parallel-tffa.md` — parallel TFFA matches: per-match score copies,
  per-viewer `scores`/`cs` rewrites, physics isolation via trace
  interception, and per-match team chat.
- `0007-map-change-module-pinning.md` — map-change crash: rcon commands run
  inside our `SVC_RemoteCommand` wrapper, the engine `dlclose`s the proxy
  mid-call, and the stale return faults. Fix: pin the module for the process
  lifetime and reset per-session state at `GAME_INIT`.
- `0008-nav-load-leak.md` — map-change / `map_restart` nav-graph leak:
  `CNavigator::Load` appends without clearing, and the port's
  `CNavigator::Load` hook runs after the forwarded `GAME_INIT` already loaded
  the nav data. Fix: free through the always-active `G_NAV_LOAD` syscall arm.
- `0009-taystjk-scoreboard-deaths.md` — TaystJK compatibility: advertise
  `taystJKinfo` bit 7 (`TAYSTJK_INFO_SCORE_DEATHS`) in the server info and
  append `PERS_KILLED` as the 15th `scores` int only to clients whose userinfo
  advertises `taystJKcgflags` bit 0 (`TAYSTJK_CGFLAG_SCORE_DEATHS`, cgame from
  `30fc5979b`); every other client keeps the 14-int layout.
- `0010-netstatus-timenudge.md` — netStatus `timeNudge` estimator (D-007):
  model-based estimation of the never-transmitted `cl_timeNudge` as a range,
  the magic `sv_fps` table deleted, the correction terms re-derived from
  engine source (cmd-stamp staleness, message-vs-packet ack delay, the
  `CL_AdjustTimeDelta` clock gap) and the ground-truth harness; the
  `proxy_sv_netStatusDebug` cvar the plan proposed was dropped at the user's
  request (no estimator internals at runtime).
