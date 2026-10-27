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
`tffa_join 0` selects the always-active main match (id 0), i.e. it is an
alias of `tffa_leave` (its `enabled()` gate is skipped the same way, so a
match can be left after the cvar/gametype changed under it).

### D-006.2 — Damage gating in the existing wrappers

`G_Damage` returns early (no pristine call, no stats) for player-vs-player
hits across matches; environment (world/trigger, no client) always passes.
`player_die` still runs (death/respawn real) but cross-match kills skip the
personal-stats accounting. `teamlock` runs per instance (D-004.5): each match is
snapshotted against its own round-start roster and a `SetTeam` join is checked
against the caps of the player's own match. `teamSizeRules` keeps running and
reads the per-match rosters
instead of the sum of all matches (D-005.5: one rule when every populated match
agrees on the team size). The `SetTeam` wrapper stays
installed, and is inert for an instance that the round-start snapshot did not
lock (caps remain `-1`).

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

### D-006.5 — Scoreboard rewrite at the trap, global chat untouched

`G_SEND_SERVER_COMMAND` forwarder rewrites per-viewer `scores ...` (14 ints
per entry, `g_cmds.c:64`): keep only same-match entries, substitute the
viewer's per-match red/blue. `SendScoreboardMessageToAllClients` thus fans
out correctly with no game detour. Global chat (`say`) and private `tell` are
deliberately not filtered; team chat is (D-006.7).

### D-006.6 — Per-instance interactive world entities (doors, escalators, triggers)

*Status: decisions accepted (user, 2026-10-18); clone pass, collision gate, touch
gate, link gate, runtime-spawn gate and world-query/item gate landed, plus the
runtime soak (see below).*

The world itself is shared: a `func_door`/`func_plat`/`elevator` or a
`trigger_multiple` is one gentity in `g_entities`, and both `G_UseTargets`
(activates by `targetname` across the whole array, `g_utils.c`) and
`G_TouchTriggers` (`g_active.c:516-572`, ungated) reach every instance. A door
opened in match 1 therefore opens in all of them. This became visible once the
world-visibility fix above put shared movers back in every instance's snapshot.

**Coverage is all-or-nothing per instance.** An interactive entity that is *not*
duplicated is a shared entity any instance can activate for all of them, so a
partial clone set is a leaky instance. Coverage is never traded for instance
count.

**Interactive predicate (structural, decided):** duplicate when any of

- `r.contents & CONTENTS_TRIGGER` (`0x400`, `surfaceflags.h:20`), or
- `s.eType == ET_MOVER` (`6`, `bg_public.h:1189-1208`), or
- any of `touch`/`use`/`think`/`reached`/`blocked` non-null, or
- `targetname`/`target`/`team` non-null, or
- in a team chain (`teamchain`/`teammaster` non-null).

Everything else — spawn points without keys, `misc_model_static`, lights —
stays shared and inert.

**Capacity (decided):** clones are created with the instance (lazily), one set
costing `interactive_count` entity slots; when the free ordinary range cannot
take another full set, `tffa_create` is refused with a message. No silent
reduction of the instance count, no partial set. (`free_world_slots` counts
`[MAX_CLIENTS, ENTITYNUM_MAX_NORMAL)`, the slots `G_Spawn` can still open,
`g_utils.c:800-846`.)

**Mechanism:** a proxy-side tag table `entity_instance: [u8; MAX_GENTITIES]`
(`ENTITY_SHARED` = no instance) plus per-instance copies of the interactive
entities, created with the game's own `G_Spawn` and driven by the pristine
`G_RunEntity`/`G_RunMover`; the snapshot filter, the trace flip
(`trace_flips`), the touch gate (`G_TouchTriggers` + `touch_flips`) and the
**acting scope** — the instance whose game code is running, opened around the
calls that run it (`G_RunThink`, `G_UseTargets2`, `G_RunMissile`,
`ClientThink_real`, the vmMain client command/disconnect paths) and resolved
from the acting entity or, when it carries no instance, its owner — read by the
`G_Find` link gate, the `G_Spawn` runtime-spawn gate and the
`G_EntitiesInBox`/`G_EntityContact` world-query gates — then resolve everything
against the tag, so no game code is reimplemented.

**The collision gate (S5, landed).** `trace_flips` (already zeroing `r.contents`
of cross-match players/owned entities for the duration of one synchronous trace)
now also covers every tagged entity whose instance differs from the requester's:
a player in instance N collides with instance N's own doors/plats/elevators and
with nobody else's, and main's originals are foreign to every created instance.
Only entities whose contents intersect the trace's own `contentmask` are flipped
(the engine skips the rest anyway, `sv_world.cpp:637`), and the mask is the trap's
seventh word — `trap_Trace`, `trap_G2Trace` and `trap_TraceCapsule` all pass
`passEntityNum` then `contentmask` (`g_local.h:1698`), so `a5`/`a6` cover all
three. Entities whose contents are zero (targets, `target_speaker`,
`info_notnull`) need no flip and are skipped.

The save list is a reusable `Vec` held in the state and taken out around the
trap, not a fixed array: the number of entities one trace must ignore grows with
the map and the instance count, and a cap that silently overflowed would leave a
door solid in someone else's instance (the all-or-nothing rule forbids that).
Taking it out also means a nested trace finds it empty instead of clobbering the
outer list, and the capacity is kept, so steady play never allocates inside the
engine frame. The `G_TRACE` arm tests the cvar before touching the buffer, so a
server that has not turned the feature on pays one cvar read per trace and
nothing else.

A requester that resolves to no match flips nothing — the world stays solid for
it, exactly as before this feature. "No match" now includes a *freed* requester
slot, whose `memset` owner fields would otherwise read as client 0, and
`ENTITYNUM_WORLD`/`ENTITYNUM_NONE` (1022/1023), which name no entity: `G_Spawn`
never hands those slots out (`g_utils.c:837`).

**The touch gate (S6, landed).** `G_TouchTriggers` gets an entry wrapper
(`GAME_HOOKS` now 13, prologue `55 8b ec 81 ec 70 11 00 00`, steal 9): for the
duration of the call it clears `CONTENTS_TRIGGER` on every trigger, door trigger
and item of another instance and restores afterwards, so a player touches their
own instance's triggers and nobody else's. This rides the pristine filter: the
touch loop skips an entity without `r.contents & CONTENTS_TRIGGER`
(`g_active.c:548`; in the shipped binary `0x119d46`/`0x119d4c` — `mov
0x25c(%edi),%ecx` then `test $0x4,%ch`, `je` past the entity), so nothing is
reimplemented and the requester's own instance is untouched. The requester is the
calling entity's own match (`touch_requester`): only a live client slot qualifies,
because pristine `G_TouchTriggers` returns immediately for anything else
(`g_active.c:524-531`), and the entity is a player, so no `r.ownerNum` hop is
needed. The same reusable save list as the trace gate is used, taken out of the
state so a trace issued inside the window (an item dropping to the floor) gets an
empty one.

Unlike a trace, this window runs *game* code, so the restore is guarded: a slot is
written back only while it is still `inuse` and still shows the `0` the gate wrote,
which keeps a stale `contents` from being written over a value the game set
meanwhile (`Use_Item` re-arming an item) and never resurrects a slot the game
freed inside the window (`G_FreeEntity` memsets it). This is what makes it safe to
run on every player think of every frame.

**The link gate (S7, landed).** Duplication alone still leaves the `targetname`
link: `G_UseTargets2` resolves a chain with `G_Find(NULL, FOFS(targetname),
string)` and *walks every match* (`g_utils.c:566-601`), so a trigger in main
opens main's door **and** every instance's copy of it. Rather than reimplementing
that loop, the gate filters its search: `G_UseTargets2` gets an entry wrapper
(prologue `55 8b ec 83 ec 20`, steal 6) that opens the **acting scope** — the
firing entity's own instance — around the pristine call, and `G_Find` gets one
(`prologue 55 8b ec 83 ec 18`, steal 6) that, while a scope is open and only for
`fieldofs == FOFS(targetname)` (`0x31c`), skips matches tagged to another
instance.

Every step of the search is delegated to the pristine `G_Find` (`find_visible`
re-calls it from the last hidden match, so its `level.num_entities` bound and
`Q_stricmp` stay the game's own), and **nothing is written into game memory** —
unlike the contents flips of S5/S6, this gate has no save list and no restore, so
there is no stale-write hazard to guard. `G_UseTargets` needs no hook of its own:
objdump shows it reads `ent->target` and calls `G_UseTargets2` at `0x170545`, so
the one hook covers it and every direct chain caller (`target2`, `opentarget`,
`closetarget`, `paintarget`, …). A chain scheduled for later (`multi_wait`
delayed uses) resolves when it fires, with its own scope, so no timer path needs
separate handling.

The scope comes from the *acting* entity, read through the tag table
(`acting_instance`): a live player slot resolves through `client_match` (as the
touch gate does), a tagged world entity resolves to its own tag — main included,
main being a match like any other — and an actor that carries no instance (a
shared/untagged entity, a freed slot, `ENTITYNUM_WORLD`/`ENTITYNUM_NONE`, a
pointer outside the array) **keeps the scope it was already in**. A nested chain
therefore never widens halfway through a cascade, and a top-level call by an
unattributable actor stays ungated, exactly as before this gate. Only
`targetname` lookups are filtered: `classname` searches (`info_player_*`
pickers, terrain, detpack) behave as before, and players and untagged entities
stay reachable from every instance — the same rule the visibility, collision and
touch gates use.

The scope is opened by **two** hooks, not one: `G_UseTargets2` (a chain runs
`target->use`) and `G_RunThink` (`g_main.c:3473`, the only dispatcher of
`ent->think`, prologue `55 8b ec 83 ec 10`, steal 6).
Widening it to think dispatch is what keeps *deferred* activation inside its
instance: `target_scriptrunner_use` may defer `scriptrunner_run` to a think
(`g_target.c:748-758`), ICARUS resolves entity names through
`G_Find(targetname)` (`g_ICARUScb.c:1025,1155,1972,3058` and the fallbacks), train
paths look up their next corner in `G_Find` (`g_mover.c:1826`) and
`asteroid_field_think` looks up its `func_rotating` target (`g_trigger.c:1796`).
`G_RunThink`'s `runicarus` tail runs inside the scope too, so ICARUS task-manager
callbacks (a completed move, a scripted `affect`) resolve against the entity that
owns the task. All of these were previously *unscoped* at think time, i.e. a
script/task started by an instance's clone could look up and act on main's
original.

**Map scripts (decided with S8).** A `target_scriptrunner` is interactive (its
`use` is `target_scriptrunner_use`, `g_target.c:734`), so it is tagged and
duplicated like any other interactive entity, and its `use`/`think` runs in that
copy's scope. The script itself runs on the *activator*
(`trap_ICARUS_RunScript(self->activator, …)`, `g_target.c:821`), i.e. on the
player or entity that fired the chain, so its per-entity state lives with the
player and is already per-instance; its effects on other entities resolve by
`targetname` through `G_Find` and are therefore scoped by the same acting scope.
Nothing extra was needed beyond S7/S8. `mp/ffa_YavinPlaza` is the only
instance-capable map in the shipped set with script runners (four
`target_scriptrunner`s: `hugger`, `hugee`, `typing`, `commander_typing`);
`t3home2` adds `NPC_spawner`s (the spawners are duplicable; the NPC they spawn is
an actor, see below). Neither was run under a scripted differential: what a
script *does* to a named entity is observable only by its effect, and the flag
question — should one instance's script see the other instances' copies at all —
is answered "no" by the scope. The remaining boundary is structural, not a
decision: a script that holds *no* tagged entity (an untagged activator, a global
task) has no instance to scope to and keeps its old reach.

**The runtime-spawn gate (S8, landed).** The clone pass copies the map's
interactive set, but the map's own code also *creates* interactive entities after
`GAME_INIT`: `Think_SpawnNewDoorTrigger` (`g_mover.c:1158`) spawns the
`trigger_door` that makes a touch-openable `func_door` work (a door with no
`targetname`/`health`/locked flag, `g_mover.c:1442-1462`),
`asteroid_field_think` (`g_trigger.c:1856`) spawns the asteroids it counts by
`r.ownerNum`, `misc_faller_create` (`g_misc.c:2789`, a `use` handler) spawns a
falling prop, and `ATST_ManageDamageBoxes` (`g_misc.c:2126`) spawns damage boxes.
Such an entity was untagged, hence shared: on `mp/ctf2` (four `func_door`s
forming one `team blue_door1` chain) main's doors create one `trigger_door` that
any instance's player can touch, i.e. it opens main's door from instance N.

The gate attributes the spawn instead of duplicating it. `G_Spawn` gets an entry
wrapper (`0x16db44`, prologue `55 8b ec 83 ec 30`, steal 6, `GAME_HOOKS` 15 → 17)
that, after the pristine allocation, records the returned slot **only while a
world entity is running game code**; `G_RunThink` and `G_UseTargets2` open the
acting scope around that code. When the outermost call closes, each recorded slot
is classified: a live, interactive, non-event entity (the S2 predicate) gets the
scope's tag, everything else — an inert prop, a temp event entity
(`eType >= ET_EVENTS`: the event/sound machinery's, `freeAfterEvent`), a slot the
game already freed or recycled — stays untagged and shared. A fresh `G_Spawn`
result cannot be classified at spawn time (`G_InitGentity` has only just reset
it; the caller sets the callbacks afterwards), hence the deferred flush at the
end of the acting call, still inside the same frame. No state is written into
game memory here either, and the pending list is a reusable `Vec` in the proxy
state (no fixed cap to silently drop spawns).

No cross-instance cloning is involved: every entity that runs code at runtime is
interactive by the predicate (`think`/`use`/`touch`/`reached`/`blocked`,
`CONTENTS_TRIGGER`, `ET_MOVER`, a name, a team), so each instance already has its
own copy of the *spawner*, and that copy creates its own child. An instance's
`trigger_door` is thus built by the instance's own door clone from the clone's
own bounds, linked and parented (`parent` is an in-set link the clone pass
re-points) to that instance's door. Two orderings both end with one trigger per
instance: a match created *after* main spawned its trigger copies it with the
rest of main's set (a door's `nextthink` has been consumed, so the instance's
door clone does not build a second one), while a match created *before* the spawn
still carries the armed `nextthink` and its door clone builds its own.

Capacity: these children consume slots beyond the `GAME_INIT` measurement
(`world_clone_cost`), they are released with their instance's set, and the
create-time check measures the *live* main-tagged set at `tffa_create` time — so
it stays exact for the instance being created, but cannot pre-reserve the
children a match will spawn later.

**Evidence (S8):** `G_RunThink` `0x8a1b4` (`nm -D`
`G_RunThink__FP9gentity_s`, prologue `55 8b ec 83 ec 10`; the body reads
`nextthink` at `0x4a4` and calls the pointer at `0x4a8`, both pinned offsets, and
reaches `trap_ICARUS_MaintainTaskManager` after the think), `G_Spawn` `0x16db44`
(prologue `55 8b ec 83 ec 30`; its free-slot scan starts at `0x20` =
`MAX_CLIENTS`), `Think_SpawnNewDoorTrigger` `g_mover.c:1158` (`G_Spawn` at
`:1193`, `r.contents = CONTENTS_TRIGGER`, `touch = Touch_DoorTrigger`, `parent =
ent`), `asteroid_field_think` `g_trigger.c:1856`, `SpawnPlatTrigger`
`g_mover.c:1517` (called from `SP_func_plat`, i.e. at map load, hence in the
measured set), `misc_faller_create` `g_misc.c:2789`, `target_scriptrunner_use`
`g_target.c:734-758`, `scriptrunner_run` `g_target.c:754`, `G_TempEntity`
`g_utils.c:1054` (`G_Spawn` + `eType = ET_EVENTS + event`). Runtime matched pair
(`mp/ctf2`, `g_gametype 6`, `proxy_tffa_enable 1`, 6 bots, 100 s, same build, one
run with the attribution bypassed by a temporary env var, instance 1 forced
through the pass at `GAME_INIT`, temporary census of the live interactive
entities by tag): with the gate **off**, 0 attributed spawns and **2 shared
`trigger_door`s for the whole run** (main's and instance 1's door clone's), each
taggable from any instance; with the gate **on**, exactly 2 attributed spawns
(1 main + 1 instance 1), **0 shared `trigger_door`s**, and the tagged interactive
count rises by one per instance (73 → 74 main and instance 1). The control also
shows what stays shared by design: missiles and dropped items (`shared` 3 → 10
over the run) keep their owner-based handling. `mp/ctf1` is inert here — all
eight `func_door`s are named (`Think_MatchTeam`), so a 70 s bot run attributes
nothing and the S5/S6/S7 numbers stand unchanged.

**Evidence:** game addresses (`nm -D`, all inside `.text` `0x85564..0x1a66c4`)
`G_Spawn` `0x16db44`, `G_InitGentity` `0x16d984`, `G_FreeEntity` `0x16de44`,
`G_RunThink` `0x8a1b4` (`G_RunThink__FP9gentity_s`; `nextthink` read at `0x4a4`,
the `think` pointer called at `0x4a8`, `trap_ICARUS_MaintainTaskManager` after it),
`G_RunMissile` `0x14d274` (`G_RunMissile__FP9gentity_s`; prologue
`55 8b ec 81 ec d8 00 00 00`),
`G_Find` `0x16cec4`, `G_UseTargets` `0x170524`, `G_UseTargets2` `0x16d604`,
`G_TouchTriggers`
`0x119bf4` (`0x400` contents test at `0x119d4c`: `mov 0x25c(%edi),%ecx` +
`test $0x4,%ch`, i.e. `r.contents & CONTENTS_TRIGGER` after the slot's `imul
$0x5ec`, the pinned `sizeof(gentity_t)`; the `$0x400` at `0x119c95` is
`MAX_GENTITIES`, the `trap_EntitiesInBox` maxcount argument, not a mask), `G_MoverTeam`
`0x14e6f4`, `G_RunMover` `0x14e974`, `G_SpawnEntitiesFromString` `0x15d094`.
`G_UseTargets2` resolves through `G_Find` with `fieldofs` `0x31c` at `0x16d68e`
(first match) and `0x16d6de` (resume from the last match), and `G_UseTargets`
reads `0x448(%edx)` (`ent->target`) before calling `G_UseTargets2` at
`0x170545` — the pin that makes one hook cover both.
`gentity_t`/`entityState_t` offsets from the i386 layout oracle (`sizeof 1516`,
`r 560`, `client 864` anchors): `inuse 880`, `freetime 972`, `classname 800`,
`targetname 796`, `spawnflags 888`, `flags 960`, `neverFree 956`, `clipmask
996`, `parent 1036`, `nextTrain 1040`, `prevTrain 1044`, `pos1 1048`, `pos2
1060`, `target 1096`, `target2 1100`, `team 1120`, `nextthink 1188`, `think
1192`, `reached 1196`, `blocked 1200`, `touch 1204`, `use 1208`, `die 1216`,
`takedamage 1236`, `count 1360`, `chain 1372`, `teamchain 1388`, `teammaster
1392`, `wait 1408`, `delay 1416`, `genericValue1 1420`, `moverState 1012`;
`s.number 0`, `s.pos 16`, `s.apos 52`, `s.time 88`, `s.time2 92`,
`s.modelindex 224`, `s.modelindex2 228`, `s.solid 268`. Runtime
(`mp/ctf1`, `g_gametype 6`, `proxy_tffa_enable 1`, `sv_maxclients 8`): 115
interactive entities and 822 free slots, i.e. room for 7 further instances —
the live count is below the BSP-lump estimate (168) because lights/point ents
are freed at spawn.

**The world-query and item gate (S9, landed).** Duplicating entities scopes
*what exists*; two things still crossed instances: the interactive entities the
game creates for a player (the weapon a dying player drops, which carries no
owner), and the questions game code asks *about* the world —
`G_EntitiesInBox`/`G_EntityContact` return entity slots, not tags, so
`G_RadiusDamage`'s blast list, `G_MoverTouchPushTriggers`' synthetic touches and
force powers' radius lists reached every instance's copies.

A dropped item is interactive (`r.contents = CONTENTS_TRIGGER`, `touch =
Touch_Item`, `g_items.c:2653-2680`) but carries no `r.ownerNum` — `G_InitGentity`
leaves it at `ENTITYNUM_NONE` (`g_utils.c:833-840`) and `LaunchItem` does not set
one — so the owned-entity machinery never isolated it: it stayed shared, visible
and pickable from every instance. S9 gives it the *dropper's* instance instead.
The three paths that create items run in that player's scope:
`player_die` → `TossClientItems` (`g_combat.c:2644`, drops the held weapon),
`ClientDisconnect` → `TossClientItems` (`g_client.c:3876`, drops what the
leaver carried) and force push → `ForceThrow` → `TossClientWeapon`
(`w_force.c:3609`, inside the pushed player's think). `G_Spawn` records the
slots and the S8 flush tags what is interactive and unowned; `tag_pending` now
also **skips** anything whose owner resolves to a live player, because the
client-think scope would otherwise enrol every missile and saber entity in the
tag table — the owned-entity machinery already scopes those, and a tag would
additionally copy live projectiles into a later match's world.

The acting scope itself grew to cover the code that matters (the same
"instance whose game code runs" rule, no new concept):

- `ClientThink_real` (a player's think: force powers, weapon radius damage,
  `G_TouchTriggers`/`G_TouchEnts`), `GAME_CLIENT_COMMAND` (the game's own
  `ClientCommand`, so a player's `+use` on a named target is looked up in their
  instance — `g_cmds.c:3251`) and `GAME_CLIENT_DISCONNECT`. The vmMain paths
  open it through `open_client_scope`, which reads the proxy's own match table
  because the game may clear `ent->client` inside those calls;
- `acting_instance` resolves an untagged *live* entity through
  `request_match` (one `r.ownerNum` hop to a live player) — so a missile's,
  physics object's or body's code runs in its owner's instance, exactly the
  resolution the trace gate already used for it;
- a new `G_RunMissile` hook (`GAME_HOOKS` 17 → 18) so a missile's trajectory,
  impact (`G_MissileImpact` → `G_RadiusDamage`) and think run in the firing
  entity's scope.

The two world-query traps then filter their results by that scope, reusing the
exact `G_Find` hide rule (`link_hides_tag`): a foreign-tagged entity is dropped
from a box list (compacted in place — the engine still performs the whole box
test) and answers `false` to a contact test, which needs no engine call at all.
A query made with no scope open is pristine, exactly as before this gate, so the
verified touch/trace behaviour of S5–S7 (which runs in client paths) is
unchanged.

**Evidence (S9):** `G_RunMissile` `0x14d274` (`nm -D`
`G_RunMissile__FP9gentity_s`; prologue `55 8b ec 81 ec d8 00 00 00`, 9
detour-safe bytes; `G_RunFrame` routes every `ET_MISSILE` here, `g_main.c:3773`),
trap ordinals enum-counted in `g_public.h` and pinned between verified anchors:
`G_ENTITIES_IN_BOX` 36 and `G_ENTITY_CONTACT` 37 (between `G_UNLINKENTITY` 35
and `G_GET_USERCMD` 40), `G_ENTITY_CONTACTCAPSULE` 50 (right after
`G_TRACECAPSULE` 49). Source: `LaunchItem` `g_items.c:2653`, `Drop_Item`
`:2742`, `TossClientItems` `g_combat.c:567` (called from `player_die` `:2644`
and `ClientDisconnect` `g_client.c:3876`), `TossClientWeapon` `g_combat.c:468`
(from `ForceThrow` `w_force.c:3609`), `G_InitGentity` `g_utils.c:833`, the
box users `G_RadiusDamage` `g_combat.c:5565`, `G_RadiusList` `g_utils.c:251`,
`G_MoverTouchPushTriggers` `g_active.c:601`, `G_KillBox` `g_utils.c:1162`
(damages clients only, which the damage gate already covers),
`SpotWouldTelefrag` `g_client.c:529`. Unit tests: `acting_instance` follows an
untagged entity's owner (and not a freed, absent or out-of-range one), the box
list keeps own/shared/player/non-slot entries and drops only foreign tags.
Runtime matched pair (`mp/ctf1`, `g_gametype 6`, `proxy_tffa_enable 1`,
`bot_enable 1` + `bot_minplayers 8`–`10`, 180 s, instance 1 forced at `GAME_INIT`,
`g_weaponDisable 8` so bot deaths drop the held weapon, one run with the whole
S9 gate set bypassed by a temporary env var, census of `ET_ITEM` entities with
`s.modelindex2 != 0` — what `LaunchItem` marks a dropped item with): bypassed,
**one dropped item stayed shared for the whole run** (`shared=1`, slot 328,
tag 255) — the leak, pickable from any instance; gated, **three dropped items
were tagged with their dropper's match** (`etype=2`, instance 0) and none stayed
shared, while the flush's "1 of 4" lines show the owned missiles it deliberately
left alone. The box gate ran 18 996 scoped queries in the same run and dropped
3 291 foreign-tagged entries from their lists; the bypass run confirms the
counter only moves inside the gate. `G_EntityContact` was never reached with a
scope and a foreign target on `mp/ctf1` (`G_MoverTouchPushTriggers` needs an
`ET_PUSH_TRIGGER`, and no shipped instance-capable map has one), so that arm is
inert here and rests on the source reading plus the shared rule it reuses.

**S10 soak (landed).** 300 s on `mp/ctf1` with **two instance worlds live**
(115 clones each) and one bot placed in each, 10 bots in total: both duplicates
complete, all seven world-entity hooks attached (`G_FreeEntity`, `G_TouchTriggers`,
`G_UseTargets2`, `G_Find`, `G_Spawn`, `G_RunThink`, `G_RunMissile`), 8 kills and
22 item pickups in the log, one runtime-spawn attribution, **no errors and the
server still running at the end**. The public README documents the
per-match-world behaviour under `proxy_tffa_enable`.

**The clone pass (landed).** `tffa::realize_instance(instance)` runs before a
new match is handed to a player: it walks the entities tagged as main's, takes
one `G_Spawn` clone each, copies the *live* source structure byte for byte,
restores the clone's own `s.number` (the engine routes snapshots by it),
re-points in-set links (`parent`/`nextTrain`/`prevTrain`/`target_ent`/`enemy`/
`lastEnemy`/`activator`/`teamchain`/`teammaster`) at the corresponding clone so a
duplicated team/train chain stays inside its instance, and links every clone
through `G_LINKENTITY` (ordinal 34). Pointers out of the set (players, inert
props) are left alone — those stay shared by design.

`G_FreeEntity` is detoured so a clone that leaves the world (a `killtarget`, an
entity whose own think frees it) loses its tag with its slot: `G_Spawn` hands
freed slots to later entities, temp entities included, and a recycled slot
keeping a stale tag would hide its new occupant from every other instance. A
`neverFree` entity is unlinked but stays in use (`g_utils.c:945`), so its tag
stays with it.

An instance's set is released when its match closes (fraglimit, or a match
promoted to main), drained at the next `GAME_RUN_FRAME` start, between frames:
a later match in the same slot starts from a fresh world and the capacity
returns. Clone creation is idempotent, so a slot that never released reuses the
world it left.

**Entity budget and eligibility.** `free_world_slots` counts the slots `G_Spawn`
can still open (`[MAX_CLIENTS, ENTITYNUM_MAX_NORMAL)`, `g_utils.c:800-846`), and
the pass runs synchronously (no temp entity appears mid-pass), so a whole set
fits exactly when the count says so. A map holding an interactive entity with a
shared `ghoul2` model or a dynamically allocated `client` (an NPC/actor:
`is_duplicable_entity`) takes no instance at all: a naive copy would animate as
one body and `G_FreeEntity` would clean the model out from under the other
(`g_utils.c:945-1013`). All-or-nothing again — no instance, rather than an
inconsistent one.

**Landed so far:** the tag table and its snapshot gate, the predicate, the
`GAME_INIT`/lazy capacity scan, the clone pass, the create-time refusal, the
`G_FreeEntity` lifetime detour, the per-trace collision gate, the per-call touch
gate, the per-call link gate, the runtime-spawn gate, the world-query/item gate,
and the tests below. Runtime (`mp/ctf1`,
`g_gametype 6`,
`proxy_tffa_enable 1`, forced through the pass at `GAME_INIT` because only a real
client can issue `tffa_create`): 7
sets of 115 clones (instance 8 refused with "no entity capacity left"), clones
`inuse`/linked with their own `s.number` and classname, and — the load-bearing
observation — the GT_TEAM flags (`team_CTF_blueflag`/`redflag`, interactive and
self-freeing off CTF) freed themselves in all seven instances at the same point,
each copy running its own think. The collision gate ran under real movement
(`bot_enable 1` + `bot_minplayers 4`): 4k–13k trace traps per 200-frame window,
~10 flips per trace (instance 1's solid movers), and the count of instance-1
clones with non-zero `r.contents` flat at 96–97 for the whole run, i.e. every one
of ~500k flips restored exactly. The touch gate ran in a matched pair (6 bots,
180 s, same build, one run with the gate bypassed by a temporary env var): 17172
touch windows each, 1.49M flips gated, and **no instance-1 item clone was ever
picked up by a main player with the gate on (0 of 4 main pickups), against 5 of 5
with it off** — the independent `G_LogPrintf("Item: %i %s")` stream shows exactly
those 5 extra pickups.

The link gate ran on the same map as a matched pair (6 bots, 150 s, same build,
instance 1 forced at `GAME_INIT`, one run bypassed by a temporary env var). The
observable is ctf1's `trigger_multiple` → named `func_door`/`func_plat` chain
(`blueriser`, `t175`, `t87`, `t89`, `bluelift2`, …), read as each mover's
`s.pos.trBase[2]` with a sticky "moved" flag per slot: with the gate on, 115
`G_UseTargets2` calls (107 of them scoped to a definite instance) hid 29 matches
and — the load-bearing observation — **main's movers moved (5 of 10) while
instance 1's stayed at rest (0 of 10)**, with the hidden matches naming the
chains (`window=0 slot=211 target=t175`, `window=0 slot=299 target=t87`,
`window=0 slot=300 target=t89`, `window=0 slot=296 target=bluelift2`: main's
trigger reaching for instance 1's clones). With the gate bypassed, the same
build hid 0 matches and **every main mover that moved moved its instance-1 clone
too (3 of 10 moved in both instances)** — the leak the gate closes. The
symmetric direction was checked by forcing one bot into instance 1 (temporary
env var): instance 1's own chain moved an instance-1 mover, and its windows hid
`slot=51 target=t175` — **main's original** door — so neither instance can reach
the other's copies.

An earlier reading of the same map is worth correcting: the 64 map items arming
themselves in every instance is *not* a chain leak. `G_SpawnItem` defers
(`nextthink = level.time + FRAMETIME * 2`, `think = FinishSpawningItem`,
`g_items.c:2953-2955`), the clone copies that pending think, and each instance's
copy finishes spawning itself; `FinishSpawningItem` then leaves a *targetname*d
item inert (`contents = 0`) and an untargeted one armed (`g_items.c:2944-2948`).
The link gate concerns the `Use_Item` chains that re-arm items by name, not that
initial, per-copy state.

The runtime-spawn gate ran as a matched pair on `mp/ctf2` (6 bots, 100 s, same
build, instance 1 forced at `GAME_INIT`, one run bypassed by a temporary env
var). `mp/ctf2` has one touch-openable door *team* (`team blue_door1`), so main's
doom — i.e. the map's own code — creates exactly one `trigger_door` at runtime;
the forced instance's door clone creates its own. Bypassed: 0 attributed spawns
and **2 shared `trigger_door`s that stay shared for the whole run** (a main
player's touch acts on main's door from any instance). Gate on: **2 attributed
spawns (1 main, 1 instance 1) and 0 shared door triggers**, the instance-tagged
interactive count rising by one on each side (73 → 74), and with the instance
release path unchanged. `mp/ctf1` attributes nothing (all eight doors are named,
so no runtime trigger exists), which is why the S5/S6/S7 numbers above are
unchanged by S8.

The item and world-query gates ran as a matched pair on `mp/ctf1` (8–10 bots,
180 s, instance 1 forced, `g_weaponDisable 8` so a dying bot drops the weapon it
holds; the run is otherwise the same in both halves, one with the S9 gates
bypassed by a temporary env var). Bypassed: the dropped item stayed **shared**
for the whole run, i.e. pickable from any match. Gated: dropped items carried
their dropper's match, and 18 996 scoped box queries dropped 3 291
foreign-tagged entries. The soak (300 s, two instance worlds live with a bot in
each, 10 bots) ended with the server running and no errors.

### D-006.7 — Team chat isolated per match, global chat still global

*Status: landed (user directive, 2026-10-21).*

`say_team` reaches clients as the game's per-recipient `tchat`/`ltchat`
server command (`G_SayTo`, `g_cmds.c:1475-1525`), one trap call per client,
after its own `OnSameTeam` filter. The `G_SEND_SERVER_COMMAND` forwarder now
drops such a command when its recipient belongs to another match, so blue in
main no longer reaches blue in a created match (nor the reverse), and the
spectators — one `TEAM_SPECTATOR` team across the whole server — are split the
same way.

The sender's match is the acting scope the `GAME_CLIENT_COMMAND` forward
already opens (`open_client_scope`, D-006.6 S9), so no new detour is needed
and real clients and bots are both covered: a bot's `say_team` goes through
the engine's `SV_ExecuteClientCommand` → `GAME_CLIENT_COMMAND` path like any
client's (`sv_bot.cpp:584-586`). With no scope open, an unattributable
`tchat`/`ltchat` is left untouched — the same fail-open rule as the link,
query and touch gates. The gate keys on the client's own match assignment
(`client_match`), not on `effective_match`: follow-spectating a player of
another match changes what a spectator sees (scores, snapshots), not which
instance their own team chat belongs to. Global `chat`/`lchat` and private
`tell` keep their base-JKA reach, and the server log still records every
`sayteam:` line.

**Correction (2026-10-27) — the gate must not require UTF-8.** The first
implementation screened `tchat`/`ltchat` *inside* `CStr::to_str()`, the same
gate the `scores` rewrite uses. `G_SayTo` embeds the client's raw chat text in
the command, and that text is a single-byte encoding, not UTF-8: one accented
character (`é` = `0xE9`, any byte ≥ `0x80`) made `to_str()` fail, which skipped
the **whole** `G_SEND_SERVER_COMMAND` interception — team chat, fraglimit prune
and score rewrite alike — and the message was forwarded to every recipient on
the sender's team, i.e. into every instance. Reported by the user as "team
chats are shared across TFFA instances", intermittently because only messages
carrying a non-ASCII byte leaked.

Runtime evidence (2026-10-27, `mp/ctf1`, `g_gametype 6`, `proxy_tffa_enable 1`,
`g_teamAutoJoin 1`, three synthetic protocol clients from
`tools/runtime/a2_netstatus_groundtruth.py`; `alpha` creates match 1, `bravo`
creates match 2, `charlie` stays in main, all RED):

- Before: `alpha` (match 1) `say_team "CAF\xe9_ACCENT"` was **received by
  `bravo` in match 2** (and no `G_SEND_SERVER_COMMAND` interception line
  appeared for it, confirming `to_str()` was the skip); plain-ASCII chats from
  the same clients were correctly isolated.
- After: the same accented message reaches only `alpha`'s own instance, and
  `bravo`'s/`charlie`'s plain chats each stay in their own instance.

Fix: the prefix/visibility gate now runs on `CStr::to_bytes()` *before* (and
independently of) the `to_str()` gate, `tffa::is_team_chat` takes `&[u8]`, and
`syscall::team_chat_dropped` is the single predicate the arm calls. Regression
tests: `tffa::tests::team_chat_recognition_accepts_non_utf8_chat_text` (byte
shapes, `0xE9`/`0xFF` payloads) and
`syscall::tests::non_utf8_team_chat_stays_inside_the_senders_instance` (same
message, `to_str()` asserted to fail, dropped for a foreign instance, delivered
for the sender's own, fail-open for broadcast/no scope). `cargo test` 164/164,
`clippy -D warnings`, `fmt --check`, i686 release build and the runtime A/B
above are green.

Two neighbouring paths were re-checked against the same class of bug and are
**not** affected: `scores ...` messages carry only integers
(`DeathmatchScoreboardMessage`, `g_cmds.c:25-88`), so the per-viewer score
rewrite never sees non-ASCII bytes; and the pristine fraglimit `print` prune
(`is_kill_limit_print`) can only miss a per-player notice whose *name* is
non-ASCII, which leaves one cosmetic chat line while the `LogExit` hook still
owns the ending.

### D-006.8 — Matches survive `map_restart`, not a map change

*Status: landed (user directive, 2026-10-21).*

A `map_restart` sends everyone back to spectator for a fresh round, and the
old proxy-side overlay was wiped with the rest of the session state, so every
player dropped into main and had to recreate and rejoin their match. Now a
`map_restart` keeps the overlay and puts every reconnecting player back into
the match they were in.

**Evidence for the two paths.** `SV_MapRestart_f` restarts the game progs in
place (`codemp/server/sv_ccmds.cpp:306`), runs three settle frames
(`:308-312`), then reconnects every retained client with
`GAME_CLIENT_CONNECT(firstTime = qfalse)` (`:318-349`). `SV_RestartGameProgs`
→ `SV_ShutdownGameProgs` → `GAME_SHUTDOWN` → `VM_Restart` → `SV_InitGameVM`
→ `VM_Call(gvm, GAME_INIT, svs.time, Com_Milliseconds(), restart)`
(`sv_game.cpp:1693`), with `restart = qtrue`. A *full* map change
(`SV_SpawnServer` → `SV_InitGameVM(qfalse)`) passes `restart = qfalse` and
reconnects the clients the same way (`sv_init.cpp:799`). The proxy module is
pinned across both (D-008), so the overlay lives in statics that used to be
reset unconditionally at `GAME_INIT` (`state::reset_session`).

**Decision.** `GAME_INIT` carries the overlay only when `args[2] != 0`
(`vmMain(GAME_INIT, levelTime, randomSeed, restart)`): the active flag of the
created match slots and `client_match[]` (which client was in which instance).
Everything else starts clean, because it is per round or per map:
per-match tallies, `player_score`, the `CS_PLAYERS` base mirrors, the cs score
dedup cache, `owned`, and the per-instance worlds. A full map change
(`restart = false`) closes every created match and starts everyone in main,
exactly as a fresh module load did.

`state::reset_session(restart)` and `tffa::reset_round(restart)` do the carry
(`state.rs` `tffa_after_reset`, `tffa.rs` `reset_round_state`). A carried slot
above the live `proxy_tffa_maxMatches` (an admin may lower it before the
restart) closes, and its members start in main. If the feature gate is off at
`GAME_INIT` (`proxy_tffa_enable 0`, proxy disabled, or not `GT_TEAM`), nothing
is carried.

**Per-instance worlds are rebuilt.** The pristine `GAME_INIT` memsets the
entity array and respawns the map, so no clone and no tag survives the
restart even though the module does. `scan_world_entities` re-measures the
map, and `tffa::restore_instances` — called at the end of `GAME_INIT`, after
`hooks::set_enabled` attached the `G_Spawn` detour and the `G_LINKENTITY`
trampoline, and before any client reconnects — calls `realize_instance` for
every carried created match. It is all-or-nothing per instance like creation:
a match whose set cannot be rebuilt is closed (`discard_instance`, no
broadcasts: nobody is connected yet) and its members return to main. The
players therefore reconnect into a match whose doors, plats and triggers are
already its own again.

**Settle window.** The three settle frames run *before* the reconnect burst,
when every client slot is still empty, so a carried created match looks
member-less and the per-frame sweep would close it. `restart_grace_frames` is
set to 3 (`RESTART_SETTLE_FRAMES`) when a created match was carried; while it
is non-zero `on_run_frame_post` returns before the empty-match sweep and the
limit handling, and each frame decrements it. The first `GAME_CLIENT_CONNECT`
of the reconnect burst clears it too, so the first frame after the burst
sweeps normally. The countdown is the fallback for a restart with no client
left to reconnect (an empty server restarted by rcon, then `addbot`), which no
connect would otherwise end.

**Teams.** No proxy code touches teams across the restart: the base game
writes the session cvars at `GAME_SHUTDOWN` (`G_WriteSessionData`,
`g_main.c:1174`) and restores `sess.sessionTeam` in `ClientConnect` for a
non-first-time connect (`g_client.c:2304-2306`), so humans come back to their
RED/BLUE team. Bots are re-picked by `ClientBegin(allowTeamReset = qtrue)`'s
`PickTeam` (`g_client.c:2426-2430`), balanced inside their instance by the
`TeamCount` hook (D-006).

Observation (one-off runtime experiments, 2026-10-21, bots-only dedicated
server, GT_TEAM, `mp/duel1`, `proxy_tffa_enable 1`, built proxy): an instance
was created for a bot through the real `GAME_CLIENT_COMMAND` path (driven over
rcon + gdb, because no game client was available), then:

- `map_restart` → the log shows the overlay kept, the instance's world
  re-duplicated and the summary; the client table reads `client_match[0] = 1`
  before and after the reconnect burst (the bot is back in instance 1), and
  `client_match[1] = 0`;
- `map mp/duel1` (full map change) → the same state slot reads
  `client_match[0] = 0` after the reload, and no "kept"/rebuild line appears.

```text
----- proxy-rs: parallel TFFA kept across map_restart — instances are rebuilt once the hooks are up
----- proxy-rs: 4 interactive map entities, 961 free entity slots — room for 240 further TFFA instance(s)
----- proxy-rs: instance 1 world: duplicated 4 of the 4 interactive map entities measured at GAME_INIT
----- proxy-rs: parallel TFFA after map_restart: 1 instance(s) rebuilt, 0 closed
```

(`mp/duel1`'s interactive set was 4 entities; only that one carried match had a
member.) The experiments are not part of the automated suite.

### D-006.9 — Turning `proxy_tffa_enable` off closes the created matches

*Status: landed (user directive, 2026-10-21).*

`proxy_tffa_enable` is a live gate: every TFFA hook reads it through
`tffa::enabled()`, so setting it to `0` during a map stops all isolation on the
next frame. Before this change the overlay itself stayed behind — the created
matches kept their slots, their members kept `client_match` ids, clients kept
the `(TFFA n)` prefixes and forced-spectator `CS_PLAYERS` overrides, the
per-match HUD tallies and the per-instance world clones stayed in the map — so
players would have been left in instances that no longer isolated anything,
with dangling doors/triggers duplicated into the shared world.

**Decision.** The per-frame cvar mirror (`update_proxy_cvars` in
`GAME_RUN_FRAME`) is observed by `tffa::on_frame_start` through the
`enable_latched` flag: a non-zero → zero edge runs `disable_parallel_tffa`
before the frame is forwarded. Main (`0`) is implicit and never closes; every
active created slot is deactivated, its tallies and per-player copies reset, and
its per-instance clone set queued for release (`drain_world_releases` now runs
unconditionally at frame start, since `enabled()` is already false on that
frame). Re-latching happens in `reset_round`, so a `GAME_INIT` with the cvar
already `0` does not produce a spurious first-frame sweep.

Every connected member of a closed match is returned to the main game
(`client_match = 0`, stats reset). A member who was playing is moved to the
spectator team through the pristine `SetTeam` trampoline — the same move
`/team spectator` makes, so they suicide and respawn spectating — and receives a
`print` explaining that parallel TFFA was disabled, which match closed, and that
`/team red|blue` rejoins the main game. A member who was already spectating gets
the shorter notice (the match is gone, they are watching main). The engine log
gets one line per closed instance with its final tally and member count.

**Client views.** The last TFFA frame's per-viewer overrides are undone
explicitly (`restore_pristine_views(red, blue)`): every connected client
receives the server's own `CS_PLAYERS + n` strings from the live configstrings
and main's restored `CS_SCORES1/2` tally, so no `(TFFA n)` prefix,
forced-spectator outsider or per-match HUD tally survives. The stored bases,
score dedup cache and viewer-match cache are cleared (`cs_players_base` empty ⇒
a later re-enable falls back to the live strings, exactly like a mid-map
enable). `world_scanned` is cleared too:
gameplay while the feature is off can spawn interactive entities that carry no
tag, so a later re-enable must re-measure the map before it clones it.

**Scores after the disable.** The pristine `level.teamScores` had been kept
neutralised (`0`) by D-006.3 while TFFA owned the scoring, and the main match's
tally lived proxy-side. Leaving them there made the one remaining game read as a
score reset the moment the game owned scoring again, so the disable writes
main's tally back into `level.teamScores` (`restore_main_global_scores` →
`write_global_team_scores`, the same `+48`/`+52` slots `neutralise_global`
subtracts from) and every client's mini HUD is pushed that value
(`restore_pristine_views(red, blue)`). From then on pristine `AddScore`,
`CheckExitRules`, `CalculateRanks`, the scoreboard header and `PickTeam`'s
tie-break all continue from main's running score. This is safe: main's tally is
the exact value the proxy compared against the shared `fraglimit` every frame,
so a main game that reached the limit had already been ended or promoted before
a disable can be seen at the next frame start. The tally is restored even when
no created match was open (an enable/disable on main alone is the same reset
otherwise), and the engine's `CS_SCORES1/2` configstrings are left alone: the
next pristine `CalculateRanks` (any frag, connect, team change — a connect
calls it before the gamestate is sent) rewrites them from the restored value.
Per-player `PERS_SCORE` was reset when each player joined a created match and is
not merged back: for players who never left main it already matches main's tally,
and there is no single number that makes a created match's history agree with
main's.

**Re-enable.** The disable only gives the score *to* the game: main's proxy tally
is left untouched, so `tffa_list` still reports the main game's tally and a later
`proxy_tffa_enable 1` on the same map resumes the overlay from that number (with
`clamp_global_scores` neutralising the game's slot again, exactly like a mid-map
enable). Frags scored while the feature was off are therefore the game's, not the
overlay's — the two scoring systems are never merged.

Tests: `disable_edge_only_fires_on_a_nonzero_to_zero_flip` (only the 1 → 0 edge
fires, once), `a_disable_closes_every_created_match_but_main` (every active
created slot closes and its members return to main; main keeps its tallies),
`disable_notice_explains_the_spectator_move` (both message shapes, wire-safe) and
`disable_restores_main_tally_into_the_global_scores` (main's `5-3` lands on the
RED/BLUE slots, neighbours untouched, a `0-0` neutralisation is replaced).
The spectator move itself is the runtime-verified `move_match_to_spectator`
trampoline path; the disable as a whole needs a live admin + live client on a
real server, which the automated suite cannot drive.

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
  - *Travel works ingame* (2026-10-19): `tffa_create`/`tffa_join`/`tffa_leave`
    no longer refuse RED/BLUE. A playing client goes through the pristine
    `SetTeam` trampoline to spectator first (`set_client_spectator`, the same
    body `move_match_to_spectator` uses — the player suicides under
    `g_dontPenalizeTeam`, so no phantom team point), then the proxy moves their
    instance; they re-pick a team with `/team red|blue`. Creation is the
    exception since 2026-10-21: `tffa_create` joins the creator's side
    immediately (see the create/empty-match entry below). Every check that can
    refuse (bad target, inactive/full match, no free slot) runs before the
    suicide, so a refused command never kills the player; a command naming the
    instance the player is already in (`tffa_join 0` in main, `tffa_join N` in
    N, `tffa_leave` in main) is a no-op with a message and takes the same
    shortcut, so it never kills them either. `tffa_join 0` and `tffa_leave`
    share the same path. Moves refresh the mover's view and broadcast their
    slot. `travel_move` (the team→move decision) is unit-tested (90/90 with
    `cargo test`); the forced-suicide path itself is *not* runtime-verified:
    bots never issue client commands, so it needs a live client, and
    `set_client_spectator` is the same `SetTeam` trampoline call the
    runtime-verified `move_match_to_spectator` already makes.
  - *Per-instance auto team balance* (2026-10-19): the client's Auto Join
    (`team free`/empty → `PickTeam`), `g_teamAutoJoin` and `SetTeam`'s
    `g_teamForceBalance` check all census through the game's `TeamCount`, which
    counted the whole level — so a match's sides could be lopsided while the
    global counts looked even (and the force-balance check could then refuse a
    pick that was right for the instance). New game hook `TeamCount`
    (`FN_TEAM_COUNT` `0x124a24`, prologue `55 8b ec 83 ec 18`, steal 6,
    `GAME_HOOKS` 18 → 19): with TFFA on, `tffa::instance_team_count` censuses one
    instance — the joining client's own match for a client-scoped call
    (`PickTeam(clientNum)` and the force-balance check, which both pass the
    joiner), main for a caller without a client (`-1`: the connect auto-join and
    bot creation, both main-context) — keeping the pristine filters (skip the
    ignored client, connected clients only) and falling through to the pristine
    body when the feature is off or the world is not located. `PickTeam`'s
    tie-break still reads `level.teamScores`, which TFFA keeps neutralised, so an
    exactly even roster resolves to BLUE exactly as base JKA does on 0-0.
    Verification: `cargo test` 90/90 (new
    `instance_team_count_scopes_the_census_to_one_instance` over a fabricated
    roster) plus the pinned steal fixture in `GAME_SITES`; runtime (`mp/ctf1`,
    `g_gametype 6`, `proxy_tffa_enable 1`, `bot_enable 1` + `bot_minplayers 4`)
    the detour attaches (`attached TeamCount @ <base>+0x124a24 (steal 6)`) and
    the bots' auto picks still split main 2-2 (slots 0 BLUE, 1 RED, 2 BLUE, 3
    RED) with no error. Per-instance balancing itself needs two live instances
    (bots never issue `tffa_create`), so the gated path rests on the unit-tested
    census plus this attach.
  - *Match end*: every finish is announced to all with final RED-BLUE +
    team lists, members return to main as spectators, and (when
    `proxy_sv_enableEndGameStats` is on) filtered personal/global stats go to
    members + spectators. A finished main promotes the lowest-id still-played
    TFFA to the new main (scores + members migrate); with no candidate the
    server just keeps the current scoreboard until `timelimit`.
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
- **Sound isolation (2026-10-18, live report: "sounds of a TFFA instance can
  be heard in another instance"):** sounds reach clients only as entity
  events in snapshots (`cg_event.c`), and the temp entities the game's sound
  helpers create carry no owner — so the visibility filter attributed every
  sound temp entity to client 0's match (and kept it for everyone whenever
  client 0 was not a live player). New game detours `G_Sound` (`0x16e824`),
  `G_SoundTempEntity` (`0x16e224`) and `G_TempEntity` (`0x16e0d4`, all steal
  6; `G_SoundTempEntity` has exactly one call site, inside `G_Sound` —
  objdump-verified) attribute **every** temp entity at construction to the
  match that owns its creating entity: `TffaState::sound_source` is stamped
  via the `G_Sound` → `G_SoundTempEntity` handshake, with the same
  `request_match` resolution the trace isolation uses (player slot or one
  `r.ownerNum` hop) taken while the entity is live; `G_TempEntity` stamps
  world so a reused slot never keeps a stale attribution. `GAME_HOOKS` 8→11.
  The snapshot filter then drops, per viewer:
  - `EV_GENERAL_SOUND` (76) created by another match (`G_Sound`);
  - `EV_ENTITY_SOUND` (79) whose `s.clientNum` emitter is in another match
    (`G_EntitySound`/`G_SoundOnEnt`: grip choke, grapple, jump voice);
  - every `EF_PLAYER_EVENT` temp entity (`SendPendingPredictableEvents`,
    `g_active.c:1058`), attributed to its generating player alone
    (`s.otherEntityNum`; `s.otherEntityNum2` is `ps->emplacedIndex`, not an
    owner) — covering the predicted combat sounds `EV_FIRE_WEAPON`/
    `EV_ALT_FIRE`, `EV_SABER_ATTACK`, `EV_JUMP`, `EV_TAUNT`, ... and fixing
    the client-0 coincidence for predicted events of every kind.
  Map/world sounds (`G_SoundAtLoc`, item respawns, doors, NPC chatter) and
  the deliberate broadcasts (`EV_GLOBAL_SOUND` 77, `EV_GLOBAL_TEAM_SOUND` 78,
  `EV_MUTE_SOUND` 74 — muting a sound a match never received is a no-op) stay
  audible everywhere, and sounds riding a filtered entity (`G_AddEvent` on a
  cross-match player) were already hidden with the entity. Stamping is
  unconditional (not TFFA-gated) so the table is current whenever the filter
  activates.
- **World-entity visibility fix (2026-10-18, live report: "people in TFFA
  instances cannot see escalators/doors", fine in main):** the snapshot
  filter's non-temp owner attribution read `s.owner` @280 and
  `s.otherEntityNum*` @188/192 and treated a `0` in any of them as a
  reference to client 0. Those three fields are *not* initialised: `G_InitGame`
  memsets the whole `g_entities` array (`g_main.c:963`) and `G_InitGentity`
  resets only `r.ownerNum` (`g_utils.c:694-700`), so every entity the game
  never gives an owner (movers, items, NPCs) keeps the bare `0` default, was
  attributed to client 0's match, and was dropped from every other instance.
  Only `r.ownerNum` @660 has a real sentinel (`ENTITYNUM_NONE`, re-set on
  every spawn). Runtime evidence (filter instrumented, viewer forced into
  instance 1 while client 0 was a live main player, `mp/duel1` + 6 bots): the
  only non-player entity dropped each frame was
  `ent=48 eType=6 (ET_MOVER) client=0x0 r.ownerNum=1023 s.owner=0 oEN=0
  oEN2=0` — no item/NPC/static to blame the omission on; main-instance
  viewers kept it. Fix: the non-temp branch skips zero-valued `s.*` owner
  fields (`r.ownerNum` keeps `0` meaningful), so client 0's
  missiles/sabers/corpses/dropped items are still hidden from other
  instances while map geometry stays visible everywhere. Cost accepted: an
  entity the game genuinely gives `s.owner == 0` — client 0's `s.owner`-only
  deployments (sentry/shield/e-web, none of which set `r.ownerNum`) — is no
  longer hidden cross-instance.
- **Residual (same root cause, temp branch, deliberately not changed):** the
  temp-event fallback still reads both `s.otherEntityNum*` fields, so a temp
  entity that sets neither is attributed to client 0 and dropped from every
  other instance. Observed owner-less drops in the same run
  (`s.owner=oEN=oEN2=0`, all `SVF_BROADCAST`): `EV_CLIENTJOIN` (1),
  `EV_SET_FREE_SABER` (106), `EV_SET_FORCE_DISABLE` (107),
  `EV_PLAYER_TELEPORT_IN` (64) — so those broadcasts currently reach main
  only. A blanket "zero means unset" is *not* safe here: `EV_OBITUARY`
  (victim `s.otherEntityNum`, killer `s.otherEntityNum2`), `EV_SABER_HIT` and
  the `EF_PLAYER_EVENT` set legitimately carry `0` for client 0, so a
  follow-up needs the event-specific classification the sound round used
  (the `EF_PLAYER_EVENT` / saber special cases above are exactly that).
  `EV_PLAYER_TELEPORT_IN` was fixed that way in the next round (below).
- **Respawn/teleport sound isolation (2026-10-27, live report: "we can hear
  other TFFA instances' respawns"):** the respawn sound is
  `EV_PLAYER_TELEPORT_IN`, a `G_TempEntity` built by `respawn()`
  (`g_client.c:1225-1226`) whose client-side handler plays
  `sound/player/telein.wav` (`cg_event.c:2789`); `EV_PLAYER_TELEPORT_OUT`
  (`teleout.wav`, `cg_event.c:2816`) is the teleport/disconnect counterpart.
  Both carry no `s.owner`/`s.otherEntityNum*` — only `s.clientNum`, which
  every creator sets to the player (`g_client.c:1226,2573,3872`,
  `g_misc.c:189-192`, `g_cmds.c:921`, `g_main.c:1721`, `g_saga.c:849`;
  `ClientSpawn` syncs it from `ps.clientNum` at `g_client.c:3788`) — so the
  temp-event fallback resolved both zero owner fields to client 0 and kept
  the sounds in client 0's match: the main instance heard every other
  instance's respawns, and an instance player's respawn was silenced
  whenever client 0 was not in their match (the residual above).
  `tffa::sound_attribution` now classifies the pair as `Emitter`, the
  `s.clientNum` attribution `EV_ENTITY_SOUND` already used, so the temp
  entity (the sound *and* the spawn effect it also triggers,
  `cg_event.c:2795`) is dropped for every other instance. Classification is
  event-specific, so the other owner-less broadcasts stay as they are.
  Verification: `cargo test` 162/162; the new
  `respawn_teleport_sound_follows_its_player_match` snapshot fixture fails on
  the pre-fix classification at both ends (an instance-1 player's respawn is
  dropped in instance 1 and kept in main); `clippy -D warnings` clean;
  `fmt --check` clean; i686 release build. Cross-match audio still needs the
  live two-instance listen (bots never create split instances).
  Related residual, not addressed here: item entities are per-instance clones
  (D-006.6), so a positional `EV_ITEM_RESPAWN` already drops with its clone,
  but `RespawnItem`'s *global* powerup sound (`g_items.c:2315-2328`,
  `EV_GLOBAL_SOUND`, or `EV_GENERAL_SOUND` when the item sets `speed`, both
  built by plain `G_TempEntity` at the item's origin and therefore stamped
  world/broadcast) still plays in every instance.
- Known limitations (documented, not fixed here): force
  grip/push and force-heal target selection are not trace-based and may
  still cross matches; cross-match telefrag-induced kills still die
  (hidden feed, score lands in the attacker's match — spawn separation is
  future work); the flip path needs mixed-match assignments to observe at
  runtime (bots never issue `tffa_*`); `EV_ENTITY_SOUND` is attributed at
  snapshot time from `s.clientNum`, so a non-player emitter freed within the
  same frame would fall back to the client-0 owner hop (the emitters in
  practice are players and NPCs, which outlive their sounds).
- Verification: 7 new unit tests (gates, scores filter, fraglimit);
  `cargo test` 56/56; `clippy` clean; i686 release builds; engine smoke
  (proxy overlay boots, queries answer) plus a 70 s TFFA+bot soak on
  `mp/ctf1` (`proxy_tffa_enable 1`, `bot_minplayers 4`): all hooks attach,
  bots connect/enter/fight/die, server keeps answering, no crash.
  Sound round (2026-10-18): `cargo test` 64/64 (3 new — sound attribution
  classification, sound visibility gate, gentity-slot math), `clippy -D
  warnings` clean, steal fixtures pinned for the three new sites (6/6/6),
  engine smoke green with all 27 detours attached (live attach lines at
  `base+0x16e824` / `base+0x16e224` / `base+0x16e0d4`). Cross-match audio
  isolation itself still needs a live two-instance listen (bots never create
  split instances).
  World-entity visibility round (2026-10-18): `cargo test` 69/69 (5 new
  `snapshot_keep` fixtures over a fabricated `g_entities` buffer: a mover/item
  with default owner fields stays visible in a foreign instance; all-zero
  `s.*` owner fields are not client 0; `r.ownerNum` still follows its owner's
  match; a non-zero `s.owner` still follows its owner's match; a player entity
  still follows its own match. 4 of the 5 fail on the pre-fix predicate),
  `clippy -D warnings` clean, `fmt --check` clean. Runtime confirmation: same
  instrumented `mp/duel1` + 6-bot run as above re-executed after the fix shows
  no `ET_MOVER` (nor any other non-player world entity) in the drop set; only
  the player `eType=1` entities and the owner-less temp events listed under
  *Residual* remain dropped.
- **Per-instance world entities, clone pass (2026-10-18, D-006.6):** the
  interactive map set is now actually duplicated per instance, and one more
  game detour was added: `G_FreeEntity` (`0x16de44`, steal 6) returns a freed
  slot's tag to `ENTITY_SHARED` so `G_Spawn`'s reuse (temp entities included)
  cannot inherit a stale instance. `GAME_HOOKS` 11→12. New
  `state::TffaState` fields `world_release`/`world_cloned`/`world_clone_blocked`/
  `world_scanned`; `tffa::realize_instance` (clone pass, in-set link remapping,
  `G_LINKENTITY` 34), `tffa::release_instance`/`drain_world_releases` (frame
  start), `tffa::note_entity_freed`, `tffa::interactive_sources`,
  `tffa::is_duplicable_entity`, and `cmd_create`'s refusal path.
  `jampgame::g_spawn`/`g_free_entity` are the two new game-function bindings
  (`FN_G_SPAWN` `0x16db44`, `FN_G_FREE_ENTITY` `0x16de44`).
  Verification: `cargo test` 76/76 (2 new: interactive-source scan bounds incl.
  freed/player slots; NPC-eligibility predicate), `clippy -D warnings` clean,
  `fmt --check` clean, steal fixture pinned for `G_FreeEntity` (6: the
  whole-instruction decoder stops once it has 5 bytes, at `push`/`mov`/`sub`).
  Runtime (`mp/ctf1`, `g_gametype 6`, `proxy_tffa_enable 1`, `sv_maxclients 8`,
  pass forced at `GAME_INIT` by a temporary instrumentation, since only a real
  client can issue `tffa_create`): 7 instances × 115 clones realised
  (`level.num_entities` 206→1005, free slots 822→17), instance 8 refused for
  capacity with no partial set, every clone `inuse`+linked with its own
  `s.number` and classname, the GT_TEAM flags self-freeing identically in all
  7 instances (clones running their own thinks), the `G_FreeEntity` detour
  observed clearing tags (115→113 per instance), no `G_Spawn: no free
  entities`, no crash, protocol-26 queries still answered. The temporary
  instrumentation was removed before the final build (the staged `.so` BuildID
  came back identical to the pre-instrumentation build).
- **Per-instance world entities, collision gate (2026-10-18, D-006.6 S5):**
  `tffa::trace_flips` now also flips every tagged entity of a foreign instance,
  filtered by the trace's `contentmask` (the trap's `a6`; the `G_TRACE` arm no
  longer passes only `a5`), so a player collides with their own instance's
  doors/plats/elevators and nobody else's. The save list is a new
  `state::TffaState::flip_buf: Vec<(usize, i32)>` taken out of the state
  around the trap — unbounded (a fixed 64-entry stack array could silently leave
  a door solid on a big map) and allocation-free once grown. `request_match` now
  refuses a requester outside `[0, ENTITYNUM_MAX_NORMAL)` and a freed one, whose
  `memset` owner fields previously resolved to client 0's match. New pin
  `sdk::CONTENTS_SOLID` (`0x1`, `surfaceflags.h:10`).
  Verification: `cargo test` 78/78 (2 new: foreign-instance flip selection incl.
  the contentmask filter and the requester-own/shared exemptions; no-match
  requesters flip nothing while a live owned entity still resolves through
  `r.ownerNum`), `clippy -D warnings` clean, `fmt --check` clean. Runtime
  (`mp/ctf1`, `proxy_tffa_enable 1`, `bot_enable 1` + `bot_minplayers 4`,
  instance 1 forced at `GAME_INIT` by temporary instrumentation): 4k–13k trace
  traps per 200-frame window with ~10 flips each, the instance-1 clone
  non-zero-`contents` count flat at 96–97 for the whole run (every flip restored
  exactly), no errors, and 0 flips for the no-bot run's 128 world/item traces
  (the intended conservative default).
- **Per-instance world entities, touch gate (2026-10-18, D-006.6 S6):** new game
  hook `G_TouchTriggers` (`GAME_HOOKS` 12→13, `FN_G_TOUCH_TRIGGERS` 0x119bf4,
  prologue `55 8b ec 81 ec 70 11 00 00`, steal 9, fixture pinned in `GAME_SITES`).
  `tffa::touch_flips` reuses the trace gate's selection with the trigger mask
  (`collect_flips`, split out of `flip_list`) and `touch_requester` resolves the
  requester from the calling entity's own match — only a live client slot has one,
  because the pristine body returns immediately otherwise (`g_active.c:524-531`).
  `flip_apply`/`flip_restore` are now shared by both gates; the restore is guarded
  (still `inuse`, still showing the `0` the gate wrote), which a window running
  game code needs and a trace window satisfies trivially. Evidence that the
  pristine loop honours the flip: the shipped binary's touch loop at `0x119d46`
  reads `r.contents` (`mov 0x25c(%edi),%ecx`) and skips on `test $0x4,%ch` — the
  `CONTENTS_TRIGGER` bit our gate clears (the `$0x400` at `0x119c95` is
  `MAX_GENTITIES`, `trap_EntitiesInBox`'s maxcount, not a mask; the slot stride
  `imul $0x5ec` re-confirms `sizeof(gentity_t)`).
  Verification: `cargo test` 80/80 (2 new: requester resolution for live client
  slots only; `flip_apply`/`flip_restore` exactness plus the no-resurrect guard),
  `clippy -D warnings` clean, `fmt --check` clean, attach log
  `attached G_TouchTriggers @ <base>+0x119bf4 (steal 9)`. Runtime, matched pair
  (`mp/ctf1`, `proxy_tffa_enable 1`, `bot_enable 1` + `bot_minplayers 6`, 180 s,
  instance 1 forced at `GAME_INIT` by temporary instrumentation, gate bypassed in
  the control run by a temporary env var, both removed before the final build):
  17172 touch windows and 1.49M flips in the gated run for the same 17172 windows
  in the control; **main players picked up 0 of the 64 instance-1 item clones with
  the gate on (4 main pickups) and 5 with it off (5 main pickups)** — and the
  independent `G_LogPrintf("Item: %i %s")` stream logged exactly those 5 extra
  pickups (11 vs 6 lines), while the diag caught a main item and its clone
  disarmed together only in the control. No errors, no leak, and the standard
  smoke (`rust/scripts/engine-test.sh`, `mp/duel1`, feature off) still passes.
- **Per-instance world entities, acting scope (2026-10-19, D-006.6 S7 + S8):**
  three game hooks make the world per-instance. `G_UseTargets2`
  (`FN_G_USE_TARGETS2` 0x16d604, prologue `55 8b ec 83 ec 20`, steal 6) and
  `G_RunThink` (`FN_G_RUN_THINK` 0x8a1b4, prologue `55 8b ec 83 ec 10`, steal 6)
  open the *acting scope* — the acting entity's own instance — around the
  pristine call; `G_Find` (`FN_G_FIND` 0x16cec4, prologue `55 8b ec 83 ec 18`,
  steal 6) filters its results while one is open; `G_Spawn` (`FN_G_SPAWN`
  0x16db44, prologue `55 8b ec 83 ec 30`, steal 6) records the slots it hands out
  so they can be classified when the call returns (`GAME_HOOKS` 13 → 17, all
  fixtures pinned in `GAME_SITES`). `G_UseTargets` is deliberately not hooked:
  objdump shows it reads `0x448(%edx)` (`ent->target`) and calls `G_UseTargets2`
  at `0x170545`, and `G_UseTargets2` resolves with `G_Find(…, 0x31c
  /*FOFS(targetname)*/, …)` at `0x16d68e` and `0x16d6de`, so one scope covers
  every chain caller. `tffa::acting_instance` resolves the acting entity's
  instance (live player slot → `client_match`, tagged world entity → its tag, main
  included; no tag → keep the enclosing scope),
  `tffa::link_hides`/`find_visible` skip foreign tagged matches while delegating
  every search step to the pristine `G_Find` (its `level.num_entities` bound and
  `Q_stricmp` intact), and `state::TffaState` carries `acting_scope`
  (`SCOPE_NONE`, reset by `scan_world_entities`). Neither gate writes into game
  memory — no save list, no restore, so the stale-write hazard class the contents
  flips have to guard does not exist here. Verification (S7): `cargo test`
  includes the requester resolution, the hide rule and the windowed search;
  `clippy -D warnings` and `fmt` clean; attach log `attached G_Find @
  <base>+0x16cec4 (steal 6)` + `attached G_UseTargets2 @ <base>+0x16d604 (steal
  6)`. Runtime, matched pair (`mp/ctf1`, `proxy_tffa_enable 1`, `bot_enable 1` +
  `bot_minplayers 6`, 150 s, instance 1 forced at `GAME_INIT`, gate bypassed in
  the control by a temporary env var, all instrumentation removed before the
  final build): the observable is the map's `trigger_multiple` → named
  `func_door`/`func_plat` chain, read as `s.pos.trBase[2]` with a sticky moved
  flag. Gated: 115 calls / 107 scoped / 29 matches hidden, **main's movers moved
  (5 of 10), instance 1's never did (0 of 10)**, hidden events naming the chains
  (`window=0 slot=211 target=t175`, `299 target=t87`, `300 target=t89`, `296
  target=bluelift2`). Bypassed: same build, 0 hidden, **3 of 10 movers moved in
  each instance** — main's trigger moving instance 1's clones, the leak itself.
  Positive control (one bot forced into instance 1): instance 1's chain moved an
  instance-1 mover and its window hid `slot=51 target=t175`, main's original —
  both directions verified. The standard smoke (`rust/scripts/engine-test.sh`,
  `mp/duel1`) and a final ctf1 TFFA run with the instrumentation removed both pass
  with no errors.
- **Runtime-spawned interactive children (2026-10-19, D-006.6 S8):** while a
  scope is open, `tffa::note_spawned` records each `G_Spawn` slot with that
  instance (ordinary range only, `MAX_CLIENTS..ENTITYNUM_MAX_NORMAL`), and
  `tffa::close_acting_scope` flushes them at the outermost close: `tag_pending`
  tags the live, interactive, non-event entities (the S2 predicate; `eType >=
  ET_EVENTS` excluded — those are the event/sound machinery's) and leaves inert
  props, temp entities and already-freed/recycled slots shared. The pending list
  is a reusable `Vec` (`TffaState::spawn_pending`), so no fixed cap silently drops
  spawns; it is cleared by `scan_world_entities` and flushed defensively at
  `on_frame_start`. Nothing is cloned across instances: every runtime spawner runs
  code, so it is interactive, so each instance already owns a copy of the spawner
  and that copy creates its own child (a `trigger_door` built from the door
  clone's own bounds, its `parent` re-pointed by the clone pass). The scope is
  opened by `G_RunThink` as well as `G_UseTargets2`, which is also what keeps
  *deferred* activation inside its instance (`target_scriptrunner_use` deferring
  `scriptrunner_run` to a think, ICARUS name lookups through `G_Find`, train
  corners, `asteroid_field_think`'s target) — see the S8 section above for the
  scope's extension and the map-script decision.
  Verification: `cargo test` 85/85 (2 new: `spawn_record` range/scope rules,
  `tag_pending` classification), `clippy -D warnings` clean, `fmt` clean, attach
  log `attached G_Spawn @ <base>+0x16db44 (steal 6)` + `attached G_RunThink @
  <base>+0x8a1b4 (steal 6)`. Runtime matched pair (`mp/ctf2`, `proxy_tffa_enable
  1`, `bot_enable 1` + `bot_minplayers 6`, 100 s, instance 1 forced at
  `GAME_INIT`, attribution bypassed in the control by a temporary env var, census
  instrumentation removed before the final build): the observable is the one
  runtime `trigger_door` created by the map's touch-openable door team
  (`blue_door1`). Bypassed: 0 attributed, **2 shared `trigger_door`s for the whole
  run**; gated: **2 attributed (1 main + 1 instance 1), 0 shared**, tagged
  interactive count 73 → 74 per instance. The detoured `G_Spawn` leaves the clone
  pass alone: a forced instance still duplicated its whole set (75/75 on `ctf2`,
  115/115 on `ctf1`). `mp/ctf1` attributes nothing (all its
  doors are named → `Think_MatchTeam`), so the S5/S6/S7 numbers are unaffected;
  the standard smoke (`rust/scripts/engine-test.sh`, `mp/duel1`), a final ctf2 run
  and a final ctf1 TFFA run all pass with no errors.
- **World queries and dropped items (2026-10-19, D-006.6 S9 + S10):** one more
  game hook, `G_RunMissile` (`FN_G_RUN_MISSILE` 0x14d274, prologue
  `55 8b ec 81 ec d8 00 00 00`, steal 9, `GAME_HOOKS` 17 → 18), so a missile's
  impact runs in the firing entity's scope; two new trap arms
  (`G_EntitiesInBox` 36, `G_EntityContact` 37 and its capsule variant 50,
  enum-counted between the pinned `G_UNLINKENTITY` 35 / `G_GET_USERCMD` 40 and
  after `G_TRACECAPSULE` 49) that filter their results with the same
  `link_hides_tag` rule the `G_Find` link gate uses (`tffa::filter_box_list` /
  `retain_visible_slots`, pure and unit-tested; the engine still performs the
  whole query); `tffa::acting_instance` now follows an untagged live entity's
  owner (`r.ownerNum` → player); the scope is opened in `ClientThink_real`,
  `GAME_CLIENT_COMMAND` and `GAME_CLIENT_DISCONNECT` (through
  `tffa::open_client_scope`, which reads the proxy's own match table because
  those calls may clear `ent->client`) and in `player_die` for the items a dying
  player drops; `tag_pending` leaves anything with a live-player owner to the
  owned-entity machinery instead of tagging live projectiles. Nothing is written
  into game memory — the trap gates only compact the game's own result array.
  Verification: `cargo test` 87/87 (2 new: the owner fallback, the box-list
  retention rule), `clippy -D warnings` and `fmt` clean, attach log
  `attached G_RunMissile @ <base>+0x14d274 (steal 9)`; runtime matched pair on
  `mp/ctf1` (a dropped item stays *shared* with the gate bypassed and carries its
  dropper's match with it on; 18 996 scoped box queries dropping 3 291
  foreign-tagged entries) and a 300 s soak with two instance worlds live (no
  errors, server still running).
- **Live-report round (2026-10-20, three reports):**
  - *Force-jump sound missing in non-main matches.* The force-jump sound is
    `EV_PREDEFSOUND` (`PDSOUND_FORCEJUMP` → `sound/weapons/force/jump.mp3`,
    `cg_event.c:2589-2625`) built by `G_PreDefSound` (`w_force.c:40-49`) with a
    plain `G_TempEntity`, so the temp entity carries no owner.
    `sound_attribution` did not classify the event, so the snapshot filter fell
    through to the generic temp-event owner rule: the memset `s.owner` /
    `s.otherEntityNum*` zeros resolved to client 0, and the sound was dropped
    for every viewer whose match was not client 0's. `EV_PREDEFSOUND` (`40`,
    counted from the pinned `EV_SABER_BLOCK 31`) is now `Stamped`, and the
    `G_TempEntity` detour stamps it with the **open acting scope** — the
    player's think is the scope those calls run in (`ClientThink_real`, S9)
    (corrected 2026-10-27: the force jump runs in `G_RunFrame`'s
    `WP_ForcePowersUpdate`, not the think — see that round below) — via
    `tffa::temp_stamp_source` (the `G_Sound` handshake still wins; no scope
    means the old world-sound behaviour, audible everywhere). Unit tests:
    classification, the pure stamping rule, and a `snapshot_keep` fixture over a
    constructed `EV_PREDEFSOUND` entity (foreign stamp hidden, world stamp kept).
    `G_SoundTempEntity` keeps the pending handshake unchanged.
  - *A player of another match appeared on the scoreboard.* (2026-10-20;
    corrected 2026-10-21 after operator feedback.) The roster stays **unified**:
    an earlier attempt filtered `scores` to the viewer's match, which hid the
    other matches' players entirely and is wrong — the tab is also how players
    see who is where. The real symptom was that a **main player was grouped on
    another instance's BLUE side** while actually playing only in main. The tab
    groups entries by each client's `cgs.clientinfo[].team`, which comes from
    the per-viewer `CS_PLAYERS` override; an override sent when the base changed
    can be lost (reliable-buffer pressure during connect/team churn, a gamestate
    resync, or a base the viewer processed afterwards), and then the outsider is
    grouped with the viewer's team. Every `scores ...` message now re-pushes
    that viewer's demotions first (`tffa::refresh_cs_players_for_scores` →
    pure `demotions_for_viewer`): each playing outsider with a known
    `CS_PLAYERS` base gets the `(TFFA N)` name prefix and `t=3`, ordered before
    the board on the same reliable stream, so the grouping cannot be stale.
    Same-match players and real spectators keep the base string. Tests:
    `scoreboard_demotions_cover_exactly_the_playing_outsiders` over the real
    captured info-string shape (main blue viewed from instance 1, instance red
    viewed from main, a real spectator) plus the unified `filter_scores` test
    (every entry kept, per-match `PERS_SCORE` substituted, out-of-range entry
    untouched).
  - *Timelimit held the map in sudden death at 15-13.* `G_RunFrame` calls
    `CheckExitRules` every frame (shipped `0x8a67f`; the SDK source has the same
    call) and its first test is `ScoreIsTied`, which for team games compares
    `level.teamScores`. TFFA neutralises those globals to `0-0`
    (`clamp_global_scores` / `neutralise_global`), so the check was always
    "tied" and `CheckExitRules` returned before the `timelimit` test — the map
    could never end on time (the pristine global log line read `red:0 blue:0`).
    New game hook `ScoreIsTied` (`0x88a74`, prologue `55 8b ec 83 ec 08 89 75
    fc`, steal 6, sole semantic caller `CheckExitRules`; `GAME_HOOKS` 19 → 20)
    answered "not tied" under TFFA at this point, so the pristine timelimit
    path ran unchanged (TIMELIMIT_HIT print, log, 2 s intermission delay, final
    board). (Superseded 2026-10-21: the hook now answers "tied" and the proxy
    owns the timelimit per match — see below.)
    Runtime differential on `mp/duel1`, `g_gametype 6`, `timelimit 1`,
    `fraglimit 0`, two same-team bots with friendly fire off (no frags, globals
    `0-0`, `numPlayingClients = 2`): the pre-fix build logged no exit after
    60 s; the fixed build logged `Exit: Timelimit hit.` at 60 s. The pristine
    `red:0 blue:0` log line stays — the globals are deliberately neutralised;
    the per-viewer `scores` message carries the real per-match tallies.
  - *Final board for a watching spectator.* `BeginIntermission` calls
    `MoveClientToIntermission` (which stops following) **before**
    `SendScoreboardMessageToAllClients`, so a spectator following a match was
    dropped back to their own (often empty) main match exactly when the final
    `scores` message was built: the header read `TIED AT 0` while the board
    listed the watched match's players and scores. `run_begin_intermission` now
    captures each connected viewer's follow-aware match
    (`tffa::snapshot_intermission_boards`) before the pristine body and the
    rewrite uses it while that match is still active
    (`tffa::pick_board_match`, unit-tested). The intermission flag is also set
    before the pristine call so the rewrite sees it.
  - Verification: `cargo test` 93/93, `clippy -D warnings` clean, `fmt --check`
    clean, i686 release build staged; engine smoke green with all 20 detours
    attached (`attached ScoreIsTied @ <base>+0x88a74 (steal 6)`, steal fixture
    `55 8b ec 83 ec 08 89 75 fc` pinned in `GAME_SITES`). The force-jump sound
    itself still needs a live two-instance listen: bots do not force-jump
    (instrumented `EV_PREDEFSOUND` stamping stayed silent in 35 s on
    `mp/duel1` and 60 s on `mp/ctf2` with 4/6 bots), so that report rests on the
    source classification plus the unit/snapshot tests. (Closed 2026-10-27 —
    the classification was right but the scope bet was not, and the sound was
    still a world stamp: see the `WP_ForcePowersUpdate` round below.)

- **`tffa_list` main player count (2026-10-21, live report).** `client_match`
  defaults to `MAIN_MATCH` for every slot, so the main line counted all free
  slots as players: a 32-slot server with 3 connected players printed
  `0 (main): 32 players`. Counting now goes through the game's authoritative
  connectivity (`pers.connected != CON_DISCONNECTED`, the same filter pristine
  `TeamCount` uses) for main and created matches alike (`connected_clients` /
  `counted_match_size` / `render_match_list`). That also counts bots, which the
  game adds with direct `ClientConnect`/`ClientBegin` calls and so can bypass
  the proxy's `GAME_CLIENT_BEGIN` flag. `match_size` (the `proxy_tffa_maxGroupSize`
  check) uses the same census, so a disconnected slot cannot occupy capacity.
  Test: `tffa_list_counts_only_connected_players` (29 free main slots + 3
  connected → main 3; an assigned-but-disconnected instance slot is not
  counted). Runtime: 32-slot `mp/duel1`, `bot_minplayers 3` →
  `0 (main): 3 players`.

- **Per-match timelimit / tied sudden death (2026-10-21, requested rule).**
  At the shared timelimit a TFFA now ends only when its teams differ; a tied
  match keeps playing (per-match sudden death) until a frag separates the
  teams. The `ScoreIsTied` hook answers **tied** under TFFA so pristine
  `CheckExitRules` never ends the map itself; `tffa::on_run_frame_post` reads
  `level.time`/`level.startTime`/`level.warmupTime` against the `timelimit`
  cvar (`tffa::timelimit_expired`, offsets pinned in `sdk.rs`) and finishes
  every active match whose `match_finish_reason` is `"timelimit"` (untied) or
  `"fraglimit"` (unchanged shared fraglimit). Tie is checked on the proxy
  tallies, so a neutralised global score cannot interfere.
  Finished sub-matches are announced with the reason
  (`TFFA N ended (timelimit): …`), get their stats and are moved to spectator
  as before; a finished main promotes the lowest-id still-active (tied) match
  into the main slot (`promotion_candidate`/`promote_into_main`), so the old
  main's players spectate the surviving game while its players keep playing.
  Once the limit has expired `tffa_create` is refused (`create_allowed`): the
  map must wind down, not grow a fresh match that would keep it alive.
  When no active match is both tied and has **playing** clients
  (`playing_clients` = connected and not spectator; `has_continuing_match`),
  every game is over and the normal intermission runs
  (`run_begin_intermission`) → final scoreboard and the usual map restart. An
  empty main (0-0, everyone back as spectators) therefore cannot keep the map
  alive forever, and a tied match left with only spectators cannot either.
  `timelimit 0` and the fraglimit keep their previous behaviour.
  Tests: `timelimit_ends_untied_matches_only` (15-13 ends at the limit, 15-15
  continues, fraglimit precedence, before-limit no-op),
  `only_a_tied_match_with_players_continues` (tied+players continues, tied
  without players does not, untied/inactive does not) and
  `timelimit_main_ends_and_tied_instance_continues` (main 5-3 closes, its
  players become spectators, the tied 7-7 instance is promoted into main and
  keeps the map waiting, and creation is refused). Runtime (`mp/duel1`,
  `timelimit 1`, `fraglimit 0`, two same-team bots with friendly fire off):
  at 82 s the map was still running (score 0-0, no `Exit: Timelimit hit.`);
  adding a blue bot produced 1-0 and the proxy logged
  `main (TFFA 0) ended (timelimit): RED 1 - BLUE 0. … Final scoreboard
  incoming.` A control with `fraglimit 1` still logs
  `main (TFFA 0) ended (fraglimit): RED 0 - BLUE 1. …`.

- **Create joins the game; empty created matches close (2026-10-21,
  requested behaviour).** `tffa_create` no longer leaves the creator as a
  spectator waiting for `/team red|blue`: the creator is put on the new
  match's smaller side immediately (`create_team`: red on a tie, and a fresh
  slot is 0-0) through the pristine `SetTeam` trampoline (`set_client_team`,
  the same body `set_client_spectator` used; the spectator helper now delegates
  to it). This also keeps the creation meaningful under the second rule: a
  created match with **nobody in game** is closed, so a creator left
  spectating would have been swept shut on the next frame.
  "In game" is the `client_census` playing filter (connected and off the
  spectator team), not merely "assigned": a match whose last player goes
  spectator (`/team spectator`), leaves (`tffa_leave` / `tffa_join`
  elsewhere) or disconnects closes itself, returning any remaining
  spectators to main. It is enforced twice: synchronously in
  `cmd_leave_silent` (`close_if_empty`, before a create/join can need the
  freed slot) and per frame in `on_run_frame_post`
  (`close_empty_created_matches`, which catches moves made by the game with no
  proxy command). Main is implicit and never closes; closing queues the
  instance's clone set for release at the next frame start
  (`world_release`) and zeroes its tallies (`close_instance_state`).
  Re-creating a slot before that release runs cancels it (`claim_world`), so
  the new match keeps the clones it was just handed instead of losing them at
  the next frame start.
  Consequence to keep in mind: a match can only be joined while someone is
  playing there — `tffa_join` into a spectator-only match is impossible
  because such a match no longer exists.
  Tests: `creator_joins_the_smaller_side_red_on_a_tie`,
  `empty_created_matches_close_but_main_stays` (main never reported; a
  spectator-only and an empty created slot are),
  `closing_an_instance_returns_its_spectators_to_main` (inactive 0-0 slot,
  spectators back in main with zeroed copies, main untouched) and
  `reactivating_a_slot_cancels_its_pending_world_release`. The live
  `SetTeam` call itself needs a real client (bots never issue `tffa_create`);
  it is the same trampoline call the runtime-verified spectator move already
  makes, and the join/create travel tests stay unit-level.

- **Join clears end-game stats — now pinned (2026-10-21, requested test).**
  Switching TFFA instances already ran `reset_slot_stats`, but nothing tested
  that the mover's kills, deaths and `proxy_sv_enableEndGameStats` sheet start
  clean in the new match. The reset is now split into testable pieces:
  `reset_client_stats` clears the mover's whole `game_stats` row (what they
  did to others) and column (what others did to them) plus
  `team_kills`/`team_killed`/`team_damages_given`/`team_damages_taken`;
  `assign_client_instance` is the join/create move itself (assign
  `client_match`, zero the per-match `player_score` copy, clear the end-game
  stats); and `reset_game_scores_at` is the `PERS_SCORE`/`PERS_KILLED` write
  (`reset_game_scores` locates the client and calls it). Other players' mutual
  stats and their own team counters are deliberately untouched.
  Tests: `joining_another_instance_resets_end_game_stats` (row and column for
  every slot, team counters, `client_match` and `player_score`; a stat between
  two other players survives) and
  `joining_another_instance_resets_game_score_and_deaths` (both persistant
  slots zeroed on a scratch `gclient_t`, neighbouring bytes unchanged).
  `cargo test` 105/105, `clippy -D warnings`, `fmt --check`, i686 build and
  engine smoke green.

- **Team-chat isolation (2026-10-21, user directive):** `tffa::is_team_chat`
  recognises the two team-chat shapes (`tchat `/`ltchat `) and
  `team_chat_visible_in`/`team_chat_visible` gate a recipient against the
  sender's acting scope over the live `client_match` table. The existing
  `G_SEND_SERVER_COMMAND` arm applies it before the `scores` rewrite; no new
  hook, no game-memory write, and the `enabled()` gate keeps the cost to one
  cvar read per trap when TFFA is off. Tests:
  `team_chat_stays_inside_the_senders_instance` (same match delivered,
  both directions cross-match dropped, no-scope/broadcast left alone) and
  `team_chat_commands_are_the_tchat_shapes_only` (global `chat`/`lchat` and
  unrelated prints untouched); `cargo test` 107/107, `clippy -D warnings`,
  `fmt --check` and the i686 release build are green. The cross-instance
  delivery itself needs two live clients (the server log keeps every
  `sayteam:` line either way), so the gate rests on the unit-tested predicate
  plus the runtime-verified `GAME_CLIENT_COMMAND` acting scope.

- **Team-chat UTF-8 skip fixed (2026-10-27, user bug report "team chats are
  shared across TFFA instances"):** the screening lived inside the
  `G_SEND_SERVER_COMMAND` arm's `CStr::to_str()` gate, so any chat text with a
  non-UTF-8 byte (a latin-1 accented character) skipped the whole interception
  and reached every same-team client in every instance. `tffa::is_team_chat`
  now takes `&[u8]`, and `syscall::team_chat_dropped` screens the raw bytes
  before the `to_str()` gate that the `scores` rewrite still needs. Reproduced
  and re-verified end to end with three synthetic clients on `mp/ctf1` (two
  created matches + main; see D-006.7 correction). Tests:
  `team_chat_recognition_accepts_non_utf8_chat_text` and
  `non_utf8_team_chat_stays_inside_the_senders_instance`; `cargo test`
  164/164, `clippy -D warnings`, `fmt --check`, i686 release build and the
  runtime A/B green.

- **Map-restart persistence (2026-10-21, user directive, D-006.8):**
  `GAME_INIT`'s restart flag now decides whether the parallel-TFFA overlay is
  carried: created matches + `client_match[]` survive a `map_restart`, nothing
  survives a full map change. Scores, per-player copies, cs mirrors/caches
  and the per-instance worlds start clean; `restore_instances` rebuilds each
  carried match's world after the hook layer attaches, before the reconnect
  burst (a rebuild failure closes that match and returns its members to
  main). `restart_grace_frames` keeps the empty-match sweep and limit
  handling off during the engine's three settle frames. Teams are left to
  the base game's session data (humans) and `PickTeam` (bots).
  Tests: `map_restart_carries_only_the_match_overlay`,
  `full_map_change_starts_from_a_fresh_overlay`,
  `map_restart_keeps_open_matches_and_assignments`,
  `reset_round_closes_matches_a_restart_does_not_keep`,
  `restorable_instances_select_active_slots_inside_the_limit`;
  `cargo test` 112/112, `clippy -D warnings`, `fmt --check`, i686 release
  build green, and the carry/no-carry paths were observed on a dedicated
  server (D-006.8).

- **Live disable (2026-10-21, user directive, D-006.9):** setting
  `proxy_tffa_enable` back to `0` closes every created match on the next frame
  before the frame is forwarded, returns its members to main as spectators
  (pristine `SetTeam` trampoline) with a per-client `print`, releases the
  per-instance clone sets, restores every client's base `CS_PLAYERS`, and hands
  main's running tally back to the pristine `level.teamScores` + HUD (so the one
  remaining game does not read as a score reset). Main keeps playing with the
  pristine scoreboard from that score. `enable_latched` (re-latched at
  `GAME_INIT`) makes the 1 → 0 edge fire exactly once; `drain_world_releases`
  moved out of the `enabled()` guard at frame start so the disable can hand its
  clones back. Tests: `disable_edge_only_fires_on_a_nonzero_to_zero_flip`,
  `a_disable_closes_every_created_match_but_main`,
  `disable_notice_explains_the_spectator_move`,
  `disable_restores_main_tally_into_the_global_scores`; `cargo test` 116/116,
  `clippy -D warnings`, `fmt --check` and the i686 release build are green.
  The spectator move + message path needs a live admin and client, so it is
  not runtime-verified.

- **Force-jump sound heard across instances (2026-10-27, live report: "we can
  still hear high jumps sounds from other instances").** The high jump is the
  force jump: pmove sets `ps.fd.forceJumpSound` when the jump passes normal
  height (`bg_pmove.c:1941/2341/2509/2586`), on the wall-jump/flip paths
  (`bg_pmove.c:1725`, `bg_saber.c:1737/1986`), and `WP_ForcePowersUpdate`
  consumes it with `G_PreDefSound(origin, PDSOUND_FORCEJUMP)`
  (`w_force.c:5388-5391` → `sound/weapons/force/jump.mp3`,
  `cg_event.c:2615`). The 2026-10-20 round stamped that event from the open
  acting scope, betting the force jump runs inside the player's think. Source
  and binary say otherwise: `WP_ForcePowersUpdate` is called only from
  `G_RunFrame` (`g_main.c:3949` clients, `3976` NPCs — the only two call sites
  in the shipped module, R_386_PC32 at `0x8aa67`/`0x8af69`, both inside
  `G_RunFrame` `0x8a304..0x8b184`); the `ClientThink_real` call is commented
  out (`g_active.c:2875`). So the event was built with **no** scope open and
  `temp_stamp_source` gave it the world stamp (`SOURCE_WORLD`), i.e. audible in
  every instance — the reported leak, and also why the 2026-10-20 change could
  only move the sound from "client 0's match" to "every match" rather than
  isolate it.
  Fix: new game hook `WP_ForcePowersUpdate` (nm
  `WP_ForcePowersUpdate__FP9gentity_sP9usercmd_s`, offset `0x18a554`, prologue
  `55 8b ec 83 ec 48`, steal 6; `GAME_HOOKS` 20 → 21) opens the acting scope of
  `self` around the pristine body — the same shape as the `ClientThink_real`
  wrapper (game code running on a player's behalf is that player's instance).
  It also covers the other owner-less force sounds built on that path: force
  grip (`5398`), the knockdown getup (`5200`) and `G_SpecialRollGetup`'s
  (`5070`, its only caller is `0x18a928` inside this function). Tests: the
  byte fixture in `GAME_SITES` (decoder steal pin) and two wiring pins
  (`game_hook_order_matches_the_site_fixtures`,
  `game_trampoline_indices_match_the_hook_table`) so the new detour cannot be
  renumbered onto another game function; `cargo test` 146/146 (was 144),
  `clippy -D warnings`, `fmt --check` and the i686 release build green; engine
  smoke attached all 21 detours (`attached WP_ForcePowersUpdate @
  <base>+0x18a554 (steal 6)`).
  Live differential (2026-10-27, `mp/ctf2`, `g_gametype 6` (GT_TEAM),
  `proxy_tffa_enable 1`, bots `Desann`/`Reborn` on RED/BLUE): bots do not
  force-jump, so the jump was driven directly — `ps.fd.forceJumpSound = 1` on
  one bot's `playerState_t` under gdb (offsets `fd` 804 +
  `forcedata_t.forceJumpSound` 328 = 1132 from an `offsetof` build against the
  shipped `q_shared.h`; `g_entities` at module `0x6ce620`, entity 1516,
  `.client` 864) and the next `G_RunFrame` frame played it. A one-line probe in
  `stamp_temp_source_event` (removed after the run) then read
  `EV_PREDEFSOUND ent=58 scope=255 stamp=255` on a build with the wrapper's
  scope suppressed (pre-fix behaviour) and `EV_PREDEFSOUND ent=58 scope=0
  stamp=0` with the fix: the sound goes from a world stamp (heard in every
  instance) to the jumping player's own match, which the snapshot filter keeps
  to that match (the `sound_visible`/`snapshot_keep` rules are the unit-tested
  half of the chain). This closes the "needs a live listen" caveat at the
  attribution level; a headphone listen across two instances still needs real
  clients and was not done.
  Residual (same world-stamp rule, deliberately not changed): an owner-less
  `EV_PREDEFSOUND` built while no scope is open still broadcasts — e.g. a
  mover crush (`G_RunMover` runs `G_MoverTeam` *before* its scoped
  `G_RunThink`, `g_mover.c`) damaging a protect/absorb user
  (`G_Damage` → `PDSOUND_PROTECTHIT`/`PDSOUND_ABSORBHIT`). The reported
  symptom does not ride that path.

- **End-of-match damage-void window (2026-10-27, live report: "when the last
  kill is performed in a TFFA instance, game stops immediately … normally
  there is a small timespan where dmgs should be void").** The proxy swallowed
  the pristine fraglimit exit (`log_exit`, above) and applied the match end in
  the same `on_run_frame_post` as the winning frag: members were moved to
  spectator immediately, losing the base game's `LogExit` →
  `BeginIntermission` interval. Pristine `LogExit` only queues the end
  (`level.intermissionQueued = level.time`, `g_main.c:2239`) and
  `CheckExitRules` applies it `INTERMISSION_DELAY_TIME` later (`g_local.h:41`;
  shipped compare against `0x3e8` at `0x88b8c`, `BeginIntermission` call at
  `0x88b9d`). During that second pristine `G_Damage` returns before touching
  health — "the intermission has allready been qualified for, so don't allow
  any extra scoring" (`g_combat.c:4520-4524`) — which is the void-damage
  timespan the report expected.

  The overlay now mirrors it **per match**, so one instance ending does not
  freeze the others:

  - `TffaMatch::end_queued_at` records `level.time` on the first frame
    `match_finish_reason` fires (`queued_finish`), and the existing
    announce/stats/spectator path runs only after `tffa::END_DELAY_MS` = 1000.
    Until then the match keeps playing (scores frozen because no damage lands)
    and stays on the scoreboard/`tffa_list`.
  - `tffa::damage_void_in` (pure) / the `G_Damage` TFFA gate
    (`tffa_damage_blocked`) void any hit with a player of an ending match on
    either end — both players, or a player's hit on a door/trigger. Other
    matches keep fighting. `tffa_join` into an ending match is refused.
  - The map's final intermission waits for pending ends (`any_match_ending`),
    an ending match is not a `promotion_candidate`, and `promote_into_main`
    clears the stamp on both slots (the new main must not inherit void damage).
    A raised/disabled limit inside the window cancels the pending end and
    restores damage; `close_instance_state`, `reset_round_state` and the
    disable edge clear it with their slot too.
  - New permanent engine-log lines make the transition observable:
    `TFFA N reached <reason> (R-B) — damage void for 1000 ms before the match
    ends` at queue time and `TFFA N ended (<reason>) R-B` at apply time.

  Tests: `a_finished_match_serves_the_pristine_end_delay` (queue → 999 ms still
  running → 1000 ms due; cancellation when the limit no longer applies; a tied
  match never queues), `damage_is_void_inside_a_matches_end_delay` (either
  end of the hit, a door/world end, other matches unaffected, inactive/cleared
  slots) and `an_ending_match_is_not_promoted_into_main`; `cargo test`
  168/168, `clippy -D warnings`, `fmt --check`, i686 release build.

  Runtime (2026-10-27, `mp/duel1`, `g_gametype 6`, `proxy_tffa_enable 1`,
  `bot_enable 1` + `bot_minplayers 4`, `fraglimit 1`, `d_noIntermissionWait 1`,
  dedicated server + `docker logs -t`): five fraglimit cycles logged the queue
  line immediately after the `Kill:` line and the `ended` line
  **998–999 ms later** (e.g. `22:39:17.345923` → `22:39:18.345505`), where the
  pre-fix build ended the match in the kill's own frame. This observes the main
  path (match 0), which shares the decision/finalise loop with created
  instances; the created-instance window and the void damage inside it were
  not runtime-observed (a created match needs a live client to issue
  `tffa_create` and to be the one landing the final hit), and rest on the
  unit-tested `queued_finish`/`damage_void_in` plus the shared
  `G_Damage` wrapper. The standard engine smoke (`rust/scripts/engine-test.sh`,
  `mp/duel1`, feature off) passes on the same build.
