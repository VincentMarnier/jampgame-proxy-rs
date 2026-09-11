# D-005 — Per-team-size game rules (`proxy_sv_teamSizeRules`)

Date: 2026-09-11. Status: accepted (user directive). Evidence: SDK
`g_main.c:316-321` (cvar registration of `fraglimit`/`timelimit`/
`capturelimit`), `g_main.c:2801-2895` (win conditions), `g_main.c:3566-3715`
(`G_RunFrame` → `G_UpdateCvars`), engine `qcommon/cvar.cpp:921-950`
(`Cvar_Update` copies the string), `server/sv_game.cpp:541-549` (`G_CVAR_SET`);
`crate::teamlock::on_run_frame` (round-start snapshot).

## Context

The team-lock feature (D-004) defines the shape of a round at the reconnect
burst that follows a map load/map_restart. Operators want the limits of a match
to follow that shape: a `2 vs 2` may want a short `timelimit` and a low
`fraglimit`, a `5 vs 5` something else. No such feature exists in the original
proxy.

The engine lets a game-module trap set any cvar (`G_CVAR_SET` = 7,
`Cvar_Set`). The game registers `fraglimit`/`timelimit`/`capturelimit` as its
own cvars (`g_main.c` cvar table) and refreshes them every frame through
`G_UpdateCvars` at the end of `G_RunFrame`, so a value written from the game's
`GAME_RUN_FRAME` entry is visible to the game's own win-condition checks
(`g_main.c:2815`, `:2878`) on the following frames.

## Decisions

### D-005.1 — Single string cvar, applied at round start

One cvar, `proxy_sv_teamSizeRules`, holds every rule:

```text
set proxy_sv_teamSizeRules "2:20:30:5,3:15:40:0"
```

Rules are separated by spaces/commas/semicolons; each is
`size:timelimit:fraglimit:capturelimit`. This keeps the cvar table small
(one entry) instead of ~45 per-size cvars. The value is read from the proxy's
`vmCvar_t` mirror after the usual per-frame `trap_Cvar_Update`.

### D-005.2 — Reconcile every frame, independent of the lock boolean

The rules run on every `GAME_RUN_FRAME` (`rules::on_run_frame`, called from the
proxy's `vmMain` dispatch next to `teamlock::on_run_frame`). They are **not**
gated on `proxy_sv_lockTeams`: the roster is recomputed each frame and the
effective rule follows it, so a match that becomes `2 vs 2` mid-game adopts the
`2 vs 2` limits. The feature remains gated on the master `proxy_sv_enable`
switch (the whole handler only runs while the proxy is enabled).

To keep this cheap and quiet, the proxy remembers the rule it last wrote
(`ProxyState::last_team_rule`) and only issues `trap_Cvar_Set` when the effective
decision changes (a different rule, or a restore). An uneven roster keeps the
previous state, so it does not flap between the two teams' sizes. The state is
fresh on each map load/`map_restart` (the native game DLL is unloaded and
reloaded), so a new round always applies its rule once.

### D-005.3 — Nearest smaller size, no clamp, restore below the smallest

`select(rules, N)` picks the defined size nearest to and not above `N`
(5 → 4 → 3 → …; a `5 vs 5` rule never applies to a `4 vs 4`). When `N` is below
every defined size (1 vs 1 with rules starting at 2), no rule matches and the
proxy restores the `timelimit`/`fraglimit`/`capturelimit` values that were in
effect before it first overrode them.

The base values are captured once per round, lazily, immediately before the
first rule is applied (`capture_base_limits`), and written back when the roster
drops below the smallest rule (`restore_base_limits`). If the proxy never
applied a rule, there is nothing to restore and the limits are left untouched.
Because game cvars persist across maps (they are not reset on a map load), the
override is also undone on `GAME_SHUTDOWN` (`restore_on_shutdown`), so a rule
value cannot leak into the next map. Within a rule, an empty or `-` field is
left untouched; the rule is taken as a whole (no per-field fallback to a smaller
size).

Only even rosters of at least one per team (`red == blue >= 1`) are considered.
The one-per-team floor keeps the all-spectator/FFA case (`0 vs 0`) from applying
or restoring team-size limits while still honoring an explicit `1:...` rule.
Uneven rosters keep the previously applied rule (no flapping).

### D-005.4 — Set only the fields the matched rule defines

For each of `timelimit`, `fraglimit`, `capturelimit` the proxy issues
`trap_Cvar_Set` only when the matched rule defines it. All three are written
regardless of gametype; the game itself decides which limit applies (`fraglimit`
for `< GT_SIEGE`, `capturelimit` for `>= GT_CTF`).

## Consequences

- New proxy cvar `proxy_sv_teamSizeRules` (default empty, so the feature is a
  no-op until configured) and a new `G_CVAR_SET` trap helper (`syscall::cvar_set`).
- Parsing/selection/decision live in `rust/src/rules.rs` and are unit-tested
  without the engine (`parse`, `select`, `decide`); `on_run_frame` is the only
  engine-touching path and reads the cvar/state under short locks before issuing
  the trap calls lock-free.
- The proxy captures the server limits before its first override
  (`ProxyState::base_limits`) and restores them when the roster drops below every
  rule, so it does not leave a stale rule value in place. A value it set stays
  until the roster resolves to a different rule, a restore, or an admin change.
