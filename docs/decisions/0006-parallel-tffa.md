# D-006 — Parallel self-organized TFFA matches (`proxy_tffa_*`, GT_TEAM)

Date: 2026-09-11. Status: accepted (user directive). Evidence: SDK
`g_cmds.c:25-100` (`DeathmatchScoreboardMessage`/`Cmd_Score_f`),
`g_combat.c:436-459` (`AddScore`), `g_combat.c:2073-2477` (`player_die`,
`EV_OBITUARY` broadcast), `g_main.c:2815-2835` (`CheckExitRules` fraglimit),
`codemp/server/sv_snapshot.cpp:298-517` (`SV_AddEntToSnapshot` /
`SV_AddEntitiesVisibleFromPoint` / `SV_BuildClientSnapshot`), engine
`sv_snapshot.cpp:740-793` (bot snapshots built but not sent); binary `nm`
(`AddScore__FP9gentity_sPfi` @ `0x1313a4`) + prologue bytes + sweep.

## Context

Operators want several TFFAs (e.g. two 2v2 + one 3v3) on one `g_gametype 6`
(TDM, called TFFA) map/server at the same time. Each group must feel like the
main game: own scoreboard, no cross-group damage, no visibility of other
groups' players or kills. Chat stays global. Limits are shared: every match
uses the server's own `fraglimit` independently, `timelimit` stays global and
ends everything. Players self-organize; when a match hits the fraglimit its
members go back to spectator and may join/spectate another match.

There is no TFFA gametype in base JKA, and the engine/game own a single
`level`/`g_entities`/`teamScores`. The proxy therefore overlays a logical
`match_id` partition; no engine fork, no second game instance.

## Decisions

### D-006.1 — Match overlay, no new limit cvar

`proxy_tffa_enable (0)`, `proxy_tffa_maxMatches (8)`, `proxy_tffa_maxGroupSize
(12)`. No fraglimit/timelimit cvar: per-match win = shared server
`fraglimit`, global end = pristine `timelimit` path untouched.
`client_match[n]` (`0` = lobby) in `ProxyState.tffa`,
fresh per map. Commands via `GAME_CLIENT_COMMAND`: `tffa_create |
tffa_join <id> | tffa_leave | tffa_list | tffa`.

### D-006.2 — Damage gating in the existing wrappers

`G_Damage` returns early (no pristine call, no stats) for player-vs-player
hits across matches; environment (world/trigger, no client) always passes.
`player_die` still runs (death/respawn real) but cross-match kills skip the
personal-stats accounting. `teamlock`/`teamSizeRules` (global-roster
features) are skipped while TFFA is on; the `SetTeam` wrapper stays
installed but inert (caps remain `-1` since the snapshot never runs).

### D-006.3 — Per-match scores via `AddScore` + global neutralisation

New game detour `AddScore` (`0x1313a4`, `55 8b ec 83 ec 08`, steal 6): run
the pristine body (keeps `PERS_SCORE`, warmup guard, `CalculateRanks`), then
add the score to the scorer's match and subtract it back from the global
`level.teamScores` slot (`+48` RED / `+52` BLUE, derived from the
`level_locals_t` field order and pinned by the existing framenum/time
anchors). The pristine `CheckExitRules` therefore never fires early; the
proxy checks each active match against the shared `fraglimit` in
`on_run_frame_post` and moves winners to spectator via the pristine `SetTeam`
trampoline (no recursion, spectator requests always pass).

### D-006.4 — Invisibility as an `SV_AddEntitiesVisibleFromPoint` post-filter

Re-added engine detour `0x08058404` (`55 8b ec 83 ec 58`, steal 6, sweep OK):
run the pristine PVS/area/broadcast body, then compact `eNums` in place,
dropping cross-match player entities (`entNum < 32` with live client) and
owned entities (`r.ownerNum` @ 660 / `s.owner` @ 280 / `s.otherEntityNum*` @
188/192, only when the owner slot holds a live player). `EV_OBITUARY` kill
feeds are broadcast temp entities keyed by victim, so they are hidden the
same way with no trap parsing. World/static entities always kept. Lobby
(`None`) sees lobby. Technique stays D-002.1-compliant (no body copy).

### D-006.5 — Scoreboard rewrite at the trap, chat untouched

`G_SEND_SERVER_COMMAND` forwarder rewrites per-viewer `scores ...` (14 ints
per entry, `g_cmds.c:64`): keep only same-match entries, substitute the
viewer's per-match red/blue. `SendScoreboardMessageToAllClients` thus fans
out correctly with no game detour. Chat (`say`/`say_team`/`tell`) is
deliberately not filtered.

## Consequences

- New hooks: game `AddScore`, engine `SV_AddEntitiesVisibleFromPoint`
  (`GAME_HOOKS` 6→7, `ENGINE_HOOKS` 14→15; steal fixtures pinned in
  `patch.rs` tests; live attach verified in-container, steal 6/6).
- **Crash fix (2026-09-12):** `cmd_list`/`cmd_create` called `max_matches()`
  — which itself takes the state lock — inside a `state::with_state`
  closure; the `Mutex` is non-reentrant, so `tffa_list`/`tffa_create`
  deadlocked the engine main thread (server hang reported as a crash).
  Both call sites hoist the cvar read out of the closure; every other
  `with_state` closure in `tffa.rs` was audited to touch fields only.
  The `tffa_list` reply also had a stray-quote quoting bug (and the other
  `print` messages sent a literal `\n` instead of a newline byte) — fixed.
- **Isolation fixes (2026-09-15, first live-client round of feedback):**
  - *Main TFFA is now match 0* (first-class): the "lobby" was previously
    unassigned and its viewers got the raw `scores` message — they saw
    every player of every match. Every connected client now belongs to the
    always-active main slot (id 0) until they create/join another match,
    and every `scores` message is rewritten per viewer (filtered entries +
    the viewer's per-match red/blue header).
  - *Per-match per-player scores*: the per-entry `PERS_SCORE` (the game's
    global, map-long score) is replaced in the rewrite by proxy-side
    per-player copies (`player_score`), reset on join/create/leave/
    connect/disconnect, so an alternative match no longer shows
    main-TFFA-era scores.
  - *Physics isolation*: the trace-family traps (`G_TRACE` 27 /
    `G_G2TRACE` 28 / `G_TRACECAPSULE` 49, ordinals compiled-oracle
    verified) are intercepted in the syscall forwarder: for each trace,
    `r.contents` of cross-match entities (players + the per-frame cache of
    linked player-owned entities: saber entities, missiles, corpses) is
    zeroed for the duration of the synchronous `SV_Trace`, then restored —
    `SV_ClipMoveToEntities` (`sv_world.cpp:589`) skips them, so cross-match
    players no longer body-block or saber-clash. Requester context comes
    from `passEntityNum` (direct slot or one `r.ownerNum` hop).
- **Second live-client round (2026-09-15): unified scoreboard + per-viewer HUD**
  - *Temp-entity visibility fix*: `G_TempEntity` slots reuse zeroed memory, so
    `s.owner` stays `0` (coincides with client 0) and `EV_SABER_BLOCK` /
    `EV_SABER_CLASHFLARE` leave `otherEntityNum*` at the `0` default. The old
    any-owner-differs filter therefore hid every same-match saber effect
    whenever client 0 was elsewhere. Temp events (`eType >= ET_EVENTS`) are
    now attributed only via `otherEntityNum*`, event-specifically (`BLOCK` via
    attacker only, `CLASHFLARE` broadcast, rest via both); `s.owner` /
    `r.ownerNum` are non-temp only.
  - *Per-viewer mini HUD*: `G_SET_CONFIGSTRING` (20) is intercepted. The game
    broadcast is forwarded first, then per-viewer `cs 6/7` corrections carry
    each viewer's own match tallies — no more shared `1-0` flap.
  - *Unified tab scoreboard*: `scores` now lists everyone (own header +
    per-match per-player scores); per-viewer `CS_PLAYERS + n` corrections show
    playing outsiders as spectator with a `(TFFA N)` prefix (truncated to
    `MAX_NETNAME`), own match + real specs untouched. Base strings are stored
    proxy-side and re-pushed on match moves / begins.
  - *Spectators-only travel*: `tffa_create/join/leave/spec` refuse ingame
    RED/BLUE (`/team spectator` first); moves refresh the mover's view and
    broadcast their slot.
  - *Match end*: every finish is announced to all with final RED-BLUE +
    team lists, members return to main as spectators, and (when
    `proxy_sv_enableEndGameStats` is on) filtered personal/global stats go to
    members + spectators. A finished main promotes the lowest-id still-played
    TFFA to the new main (scores + members migrate); with no candidate the
    server just keeps the unified scoreboard until `timelimit`.
- **Bugfix round (2026-09-1x, six live-client reports):**
  - *Mini HUD stuck*: pristine `AddScore` runs `CalculateRanks` (broadcast +
    per-viewer corrections) *before* the wrapper bumps the proxy tallies, so
    corrections lagged one frag. `on_add_score` now pushes fresh `cs 6/7`
    after the bump.
  - *Stale prefixes after moves*: per-viewer `CS_PLAYERS` corrections now fall
    back to live `G_GET_CONFIGSTRING` when the stored base is empty, and every
    move broadcasts the slot + refreshes the mover.
  - *Spectating adopts the instance*: `effective_match` (scores, snapshots,
    `cs` corrections) now follows `sess.spectatorClient` when
    `spectatorState == SPECTATOR_FOLLOW`, so watching someone shows their
    instance (and their real team) instead of forcing them to spectator.
  - *Escaped `\n`*: end-of-match / travel-refusal `print` messages used C-style
    `\\n`; now real newlines like the SDK's `va("print \"%s\n\"")`.
  - *Fraglimit crash hardening*: end-of-match names are sanitised/truncated for
    the `print` wire format, free slots are never passed to pristine `SetTeam`,
    and fraglimit transitions are logged (`tffa fraglimit finished=...`).
- **Overflow-crash fix (crashlog `artifacts/tffa_instances/crashlog.txt`):**
  `Server command overflow` → mass client drops. Root causes: finished-main
  collection included all 30 free slots (`client_match` defaults to main), and
  every `SetConfigstring`/frag re-pushed unchanged `cs` to every viewer
  (a 30-member main finish burst ~2000+ commands vs the 128-slot reliable
  buffer). Fixes: members/candidates filtered to proxy-connected slots;
  `CS_SCORES1/2` pushes deduped per viewer (`cs_scores_sent`); `CS_PLAYERS`
  base updates skip same-match viewers (engine broadcast already correct);
  `move_match_to_spectator` skips resends when the slot stays main; empty
  finishes announce nothing. Plus `tffa_where` debug (match/spec/team/follow/
  effective) for follow-spectate diagnosis.
- **Stale-artifact warning:** `cargo build` alone does NOT refresh
  `rust/jampgamei386.so` (gitignored staging copy the engine loads) — only
  `rust/dev.sh ./scripts/build.sh` copies it over. A retest without that step
  reruns the previous code. The bootstrap banner now logs a build tag
  (`build.rs` → `PROXY_BUILD_ID`: git hash when available, else a manual tag
  bumped per staged build — current: `build-j`); verify it matches before
  diagnosing. Suspect `JAMP_SKIP_BUILD=1` + a stale staged `.so` whenever the
  banner tag is old or missing expected breadcrumb lines.
- **Pristine-exit race fix (SIGSEGV 139):** `CheckExitRules` runs *inside*
  `CalculateRanks` *inside* pristine `AddScore` — before the wrapper regains
  control — so post-hoc neutralising can never win that race (verified against
  the shipped `jampgamei386.so`: `AddScore@0x1313a4` bumps
  `level+0x2c[idx]` then calls `CalculateRanks`, whose tail calls
  `CheckExitRules`). New `LogExit` game hook (`0x881d4`, steal 6) swallows
  `"Kill limit hit."` under TFFA (timelimit/capture/duel strings pass
  through); the `Red|Blue|<name> …HIT_THE_KILL_LIMIT…` chat prints are dropped
  in the trap forwarder. The proxy's `on_run_frame_post` is now the sole
  fraglimit owner; neutralise stays for configstring hygiene.
- **Solo-main end = pristine intermission** (operator report: silent restart
  with everyone dumped to spec is wrong when main was the only game): with no
  promotion candidate, members keep teams/tallies and `run_begin_intermission`
  runs (final scoreboard + standard stats for all); an `intermission` flag
  (set in the wrapper, cleared on `GAME_INIT`) gates further fraglimit
  handling. Empty-main finishes (churn window) reset tallies silently instead
  of re-firing every frame. Validated locally: 7 consecutive bot-map
  fraglimit-1 cycles, intermission → restart, no crash, no overflow.
- **Sub-finish segfault hunt:** crashlog-confirmed current (`build-j`) dies
  after moves, before any stats output, with `endGameStats 0` clean — inside
  `print_tffa_match_stats` head. Audited every unsafe block on that path
  (bounds, NUL-termination, pointer provenance) with no finding; removed the
  last raw game   reads from the region anyway (spectator recipients now come
  from stored `CS_PLAYERS` bases, always refreshed by `ClientUserinfoChanged`
  including finish-moves). Phased breadcrumbs (`stats start/personal done/
  global done`) isolate sends vs scan on the next run.
- **Sub-finish segfault fix (build-l):** crashlog-isolated to the stats head
  (moves done, no output after, `endGameStats 0` clean): that region held the
  only `HashSet`/`HashMap` in the codebase (spectator scan), running only on
  this path. Replaced with a linear `Vec::contains` — no hasher or
  thread-local machinery inside the engine frame. All other blocks on the
  path (bounds, NUL-termination, pointer provenance, send sizes) audited
  without finding (bounds, NUL-termination, pointer provenance, send sizes
  all verified; offsets cross-checked against shipped disassembly).
- **Finish ordering (empty stats tables):** `move_match_to_spectator` resets
  scores/stats, but ran *before* the end-of-instance print — every table came
  out empty. Order is now announce → stats → move everywhere, including the
  promotion path (main announces, prints, and specs its players *before* the
  candidate migrates in — previously main's final scores/stats never showed
  at all on promotion).
- **Companion hardening (kept):** `g_dontPenalizeTeam` mirroring, per-frame
  stale-global clamp, game `PERS_SCORE/KILLED` reset on moves.
- **Debug scaffolding removed (2026-09-15, user directive):** the
  `tffa_spec <id>` command (never shipped — follow-spectate via the game's
  own `follow` covers it; the `spectate_match` state died with it),
  the `tffa_where` debug command, all `tffa addscore/…` breadcrumb
  `eprintln!`s, and the `PROXY_BUILD_ID` crashlog-triage baking
  (`build.rs`/`lib.rs` banners). Behavior unchanged.
- **Follow-spectate round (game `follow`, `tffa_spec` removed per operator):**
  `effective_match` follows `sess.spectatorClient` on `SPECTATOR_FOLLOW`, and
  `on_run_frame_post` re-pushes prefixed names + scores whenever a connected
  viewer's effective match moves (`viewer_match_cache`), so watching someone
  shows their instance (own teams, no prefix) with no proxy command. Note on
  the red-skin report: `model` (e.g. `cultist/red`) is preserved verbatim in
  overrides — crashlog blue slots carrying `cultist/red` render red
  client-side regardless of `t`; verify the blue players' chosen models carry
  blue skins in base TDM before chasing proxy-side.
- Known limitations (documented, not fixed here): force
  grip/push and force-heal target selection are not trace-based and may
  still cross matches; cross-match telefrag-induced kills still die
  (hidden feed, score lands in the attacker's match — spawn separation is
  future work); the flip path needs mixed-match assignments to observe at
  runtime (bots never issue `tffa_*`).
- Verification: 7 new unit tests (gates, scores filter, fraglimit);
  `cargo test` 56/56; `clippy` clean; i686 release builds; engine smoke
  (proxy overlay boots, queries answer) plus a 70 s TFFA+bot soak on
  `mp/ctf1` (`proxy_tffa_enable 1`, `bot_minplayers 4`): all hooks attach,
  bots connect/enter/fight/die, server keeps answering, no crash.
