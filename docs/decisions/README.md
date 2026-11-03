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
- `0004-team-lock.md` — Rust-only `proxy_sv_lockTeamsAfterXSecs` (TDM/CTF):
  reconnect-burst round start, per-instance grace delay (a join cancels the
  lock), even-roster caps at expiry, `SetTeam` enforcement, and the TDM/CTF
  scope.
- `0005-team-size-rules.md` — Rust-only `proxy_sv_teamSizeRules`: per-team-size
  `timelimit`/`fraglimit`/`capturelimit` overrides at round start, nearest
  smaller size fallback, independent of `proxy_sv_lockTeamsAfterXSecs`; with parallel TFFA it
  uses the per-match sizes and applies one rule only when every populated match
  agrees.
- `0006-parallel-tffa.md` — parallel TFFA matches: per-match score copies,
  per-viewer `scores`/`cs` rewrites, physics isolation via trace
  interception, per-match team chat, per-instance world entities, per-match
  spawn-point occupancy and per-event snapshot attribution (sounds, shields,
  obituaries, effects); the `map_restart` reliable-burst guard (D-006.12),
  the per-viewer `CS_PLAYERS` refresh dedup (D-006.13) and the animation
  restart that keeps idle players out of bind pose after an instance switch
  (D-006.14).
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
- `0011-player-mute.md` — admin `mute`/`unmute` console (rcon) commands: claimed
  at `GAME_CONSOLE_COMMAND`, target by client number or name (exact, then unique
  prefix, colour codes ignored), per-slot flag cleared on disconnect and carried
  across `map_restart`, enforced by dropping `say`/`say_team`/`tell`/`voice_cmd`
  in `GAME_CLIENT_COMMAND`; `proxy_sv_muteMode` selects off / stock (drop + tell)
  / silent (the speaker sees their own line, nobody else receives it, no notice).
- `0012-spectator-team-overlay.md` — spectator team overlay feed: the pristine
  game only sends `tinfo` (the roster `cg_drawTeamOverlay`/`CG_TEAMINFO` draw
  from) to teammates, so the proxy sends the stock six-int roster for the
  followed player's team to a following spectator once per second
  (`proxy_sv_spectatorTeamOverlay`, default on); team-scoped and
  parallel-TFFA-instance-scoped, stock-cgame compatible by construction.
- `0013-crash-diagnostics.md` — opt-in `proxy_sv_debugLog` trace: blocked
  commands, client commands/userinfo, rcon/download/query attempts and client
  lifecycle, flushed to `proxy_debug.log` before dispatch for crash
  attribution; the new `SV_ExecuteClientCommand` entry-wrapper detour.
- `0014-client-pps-limit.md` — per-client incoming packet-rate limiting
  derived from the client's measured FPS:
  `proxy_sv_enableClientPpsLimit` / `proxy_sv_globalMaxPps` (default 100,
  hard ceiling; the enforced limit is the highest
  `f / ceil(f / globalMaxPps)` over `FPS ± 2`) /
  `proxy_sv_clientPpsLog`; `SV_Netchan_Process` entry wrapper, the shared
  `fps_pps` calculation (same as the netStatus table), one-second measurement
  windows, deterministic evenly-spread drops (never a tail drop), fail-open
  guards for short/stale command streams, and edge-triggered logging (starts /
  stops being over the limit). The
  accounting also runs while only the netStatus table is on, and the table's
  `packets/s` cell renders the same `over_limit` verdict: red, showing the
  effective rate with the limiter on and `<actual>/<limit>` (red actual,
  default limit colour) with it off — see D-014.6.
- `0015-void-timenudge.md` — opt-in server-side `cl_timeNudge`
  neutralisation (D-015): `proxy_sv_voidTimeNudge` rewrites each usercmd's
  `serverTime` by the estimator's setting when the reported range excludes 0
  (clamped to ±30, 1 ms per command ramp, two-reading confirmation), and
  `proxy_sv_voidTimeNudgeLog` reports the non-zero / back-to-zero transitions
  without applying the shift; the engine's duplicate filtering keeps the raw
  stamps (`lastUsercmd.serverTime` restored after `SV_ClientThink`).
- `0016-player-ignore.md` — player `/ignore`/`/unignore` client commands: the
  same target shape as `rcon mute` (shared `player_target` resolver), a
  per-client slot mask on `ClientEntry::ignored`, cleared on disconnect in both
  directions and carried across `map_restart`; enforced in the
  `G_SEND_SERVER_COMMAND` trap with the speaker taken from the synchronous
  `GAME_CLIENT_COMMAND` acting scope, gated by `proxy_sv_enableIgnore` (default
  off).
- `0017-file-ban-list.md` — file-backed IP bans (`proxy_sv_enableBanFile` /
  `proxy_sv_banFile`): the file is the only source of truth, `addip`/`removeip`
  edit real IPv4 CIDR ranges and persist atomically, `listip`/`reloadbans`,
  the connect gate uses the engine-forced `ip` userinfo and returns the stock
  `Banned.`; the pristine `g_banIPs` is never written (its 256-byte mirror
  fatally drops the server above ~25 entries).
- `0018-engine-call-and-stack-faults.md` — four pre-existing runtime faults
  found while verifying D-017: direct `call <absolute>` to engine addresses
  under ASLR (fixed with `engine_fn!`), `RandomState`/`hashmap_random_keys`
  movaps, `HashMap`/hashbrown `_mm_load_si128`, and `std::fs::read`/`try_statx`
  movaps — the last three because prebuilt `std` keeps SSE2 paths while the
  engine enters the module with a 4-byte-aligned stack (D-001). Includes the
  rule for future engine-reachable code.
- `0019-guid-bans-and-ban-command.md` — `ja_guid` ban registry
  (`proxy_sv_enableGuidBan` / `proxy_sv_guidBanFile`) kept in its own strictly
  validated file (a client-asserted value can only add a GUID, never an IP
  range), enforced at connect, plus `rcon ban <client|name>`: IP through the
  active `addip`, GUID when enabled, then kick; `addguid`/`removeguid`/
  `listguid`; `reloadbans` reloads both registries.
- `0020-shipped-banlist.md` — the ready-made cloud/hosting/VPN range list
  (`rust/proxy_bans.txt`) and its generator (`tools/update_banlist.py`):
  provider feeds plus the X4BNet datacenter aggregate, reserved space
  stripped, adjacent ranges merged, a consumer-ISP guard that refuses a broken
  source, and a parse test that fails if the shipped file stops loading.
- `0021-anti-macro.md` — opt-in anti-macro input guard (`proxy_sv_antiMacro` /
  `proxy_sv_antiMacroLog`): the server cannot see opposing key states
  (`CL_CreateCmd` sums and clamps them), so the guard watches sign reversal of
  `forwardmove`/`rightmove`/`upmove`, the `yaw`/`pitch` deltas and the attack
  bits, and freezes a channel when a **run** of reversals (chained within 200 ms)
  is beyond a hand — four reversals at ≤3 commands (movement) or ≤65 ms (view),
  or frame-exact `wait` holds (spread ≤2) — plus a 3 s *wary* period that catches
  the next burst sooner; run-based because a bind is a short burst, and rate-based
  because randomised wait counts keep the mean. Includes the
  `proxy_sv_antiMacroLog` transition/statistics levels, a never-notify rule, and
  thresholds to be checked with the log-only mode first.
