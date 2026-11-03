# D-022 — FOV cap (`proxy_sv_maxFov`)

Date: 2026-11-03. Status: accepted (user directive). Evidence: SDK
`q_shared.h` (`entityState_t` PC layout, `EF_DEAD`/`EF_NODRAW`,
`playerState_t.viewangles`), `codemp/cgame/cg_players.c`
(`CG_IsMindTricked`, `CG_Player`, `CG_AddRefEntityWithPowerups`),
`codemp/cgame/cg_event.c`/`cg_snapshot.c` (entity event processing),
`codemp/server/sv_snapshot.cpp` (`SV_BuildClientSnapshot` /
`SV_SendClientSnapshot`), `codemp/server/sv_client.cpp`.

## Context

A client can raise `cg_fov` up to 160° and gain peripheral vision the server
cannot observe: `cg_fov` is registered by the **cgame** only
(`codemp/cgame/cg_xcvar.h:305`, SDK `cg_main.c:887`, `CVAR_ARCHIVE`), never by
the client module with `CVAR_USERINFO`, and the server reads neither client
cvars nor a cvar report. There is no server-side detection to base a punish on.

What the server *does* control is the entity stream. If a player outside the
frustum the operator considers legitimate is not sent to that viewer, a wider
`cg_fov` renders empty space in its extra field: the cheat becomes useless
without detecting it. This mirrors the original C++ proxy's `player_in_fov`
idea (`Proxy_sv_snapshot.cpp`), but applies a **cap** instead of a
wall-visibility test, and keeps the players' sounds.

Two constraints shaped the design:

1. **Footsteps/sounds must keep playing.** Removing a player entity from the
   snapshot (`SV_AddEntitiesVisibleFromPoint` compaction, D-006.4) also
   removes the events riding it (`EV_FOOTSTEP`, swings, hits) and
   `CG_Player`'s client-side sounds. The stock client's mind-trick path is the
   one place where the body and the wielded saber blade are skipped while
   `CG_Player` keeps running:
   - `cg_players.c:9944-10004` adds the saber hum loop;
   - `cg_players.c:10010` returns on `iwantout` (mind-tricked, saber not in
     flight), before the blade drawing;
   - `cg_players.c:9353-9356` already ran `CG_TriggerAnimSounds` /
     `CG_PlayerAnimation`, and `cg_snapshot.c:84/145` + `cg_event.c:3568`
     process the entity's server events regardless of the mind-trick bit.
2. **The server must keep sending the events**, so the entity cannot be
   removed. `EF_NODRAW` is the instant hide (cgame returns at the top of
   `CG_Player`, no mind-trick fade) and still lets `CG_CheckEvents` fire the
   entity's server events. The per-viewer mind-trick bit and `iModelScale = 1`
   (1 %, not 0: 0 means 100 %, `cg_players.c:8349`) remain as layers for a
   client that strips `EF_NODRAW`.

## Decision

### D-022.1 — Cvar and view frustum

`proxy_sv_maxFov` (default `0` = off) is the maximum **`cg_fov` value** (the
number a player types). For each viewer, `crate::fov` tests every player entity
in the snapshot list against the camera's view frustum: the horizontal and
vertical half-angles from `proxy_sv_maxFov`/`proxy_sv_maxFovAspect`, around
`ps.viewangles`, from a point `proxy_sv_maxFovCameraRange` behind the engine's
eye/camera origin.

`cg_fov` is not the rendered horizontal angle on modern clients: OpenJK and
TaystJK default to `cg_fovAspectAdjust 1`, which applies
`fov_x = 2*atan(tan(cg_fov/2) * 0.75 * aspect)`. The proxy converts the cap
with `proxy_sv_maxFovAspect` (default `1.7777778`, 16:9) so the configured
number stays in `cg_fov` units and `proxy_sv_maxFov 100` covers `cg_fov 97` on
a 16:9 screen (rendered ≈113°). Set the aspect to `1.3333334` for 4:3/stock
behaviour (where the conversion is the identity) or the actual aspect for
ultrawide. `maxFov >= 360` and values `<= 0` are off. The viewer's own entity
is never touched. The test is in `crate::fov` (pure, unit-tested).

Third person needs its own correction: the client renders from a camera up to
`cg_thirdPersonRange` (default 100) behind the player, so a close target at the
screen edge has a smaller bearing from the camera than from the eye; testing
from the eye alone hides it early ("still hidden on the screen corner", with
the player in third person). `proxy_sv_maxFovCameraRange` (default `100`)
shifts the test point back by that many world units; `0` means eye/first-person
and a larger value covers a larger `cg_thirdPersonRange`. The frustum test covers both axes, so looking up/down no longer hides players
that are still on screen. Above `MAX_PITCH` (45°) the cap pauses entirely: the
stock third-person camera clamps the focus pitch to 45° and halves the
placement pitch, so its rendered pitch can differ from `ps.viewangles` by more
than 20°; a server-side test built on the aim would hide on-screen players
(observed: "if I look down/up and people are behind me, they disappear").
Below the threshold the vertical half-angle gets a 10° margin for the same
focus-pitch difference; the horizontal bound stays exact.

The pass runs **after** the pristine body and after the parallel-TFFA
compaction (so an entity the viewer will not receive is not touched) and on
the main view only (`portal == 0`); portal viewpoints render elsewhere on
screen and use a different origin.

Only clients actually in the game are capped: `SV_SendClientSnapshot` records
the real engine client slot (a following spectator's `frame->ps.clientNum` is
the followed player, so it cannot be used for this), and `apply` returns early
when that slot's `gclient_t.sess.sessionTeam` is `TEAM_SPECTATOR`. Spectator
entities are skipped as targets too. Free and following spectators therefore
see the normal stream.

### D-022.2 — Per-viewer hide

For an outside-frustum player's gentity `s`:

- OR `EF_NODRAW` — the instant hide (cgame skips `CG_Player`, but
  `CG_CheckEvents` still fires the entity's server events);
- set the viewer's bit in `trickedentindex` (viewer 0-15) or
  `trickedentindex2` (16-31), matching `CG_IsMindTricked`
  (`cg_players.c:6369`: 16 clients per field), for clients that strip NODRAW;
- set `iModelScale = 1` (body and hand-bolted weapon models);
- if the player has a **drawn, non-thrown** saber (`weapon == WP_SABER`,
  `saberInFlight == 0`, `saberHolstered < 2`), set `saberHolstered = 2` to hide
  the blade — `iModelScale` does not scale the client-side blade length — and
  set `s.loopSound` to the hilt's `gclient_t.saber[0/1].soundLoop` index, which
  is the same game sound index `w_saber.c:7039` uses; the client's own hum is
  suppressed by `saberHolstered = 2`, so the loop replaces it. A thrown saber is
  left untouched (visible and humming).

Nothing about the player's real position, health, or owner fields changes.

### D-022.3 — Restore point

The engine copies `g_entities[n].s` into `svs.snapshotEntities` at the end of
`SV_BuildClientSnapshot` (`sv_snapshot.cpp:606-617`), i.e. **after**
`SV_AddEntitiesVisibleFromPoint` returns. Restoring at the end of that hook
would therefore lose the mutations. A new `SV_SendClientSnapshot` entry
wrapper (address `0x0805_8e04`, `void (client_t*)`) runs the pristine body
(build + write + send) and then calls `crate::fov::restore`, putting every
saved field back. `apply` also restores any stale record before starting, so
a missed post-hook cannot leak one viewer's hide into another viewer's
snapshot. Portal calls never restore (they happen inside the same build, after
the main call).

This is the same apply/restore shape the original C++ anti-wallhack used for
entity origins (`Proxy_sv_snapshot.cpp`); only the fields differ.

### D-022.4 — Sounds kept, thrown saber untouched

- Footsteps (`EV_FOOTSTEP`/`EV_FOOTSTEP_METAL`, added by `bg_pmove.c:3844`
  with the surface parm) are entity events and still play at the true
  position; `CG_SetEntitySoundPosition` runs before `CG_EntityEvent`
  (`cg_event.c:3568`).
- A drawn saber is marked `saberHolstered = 2`, so the client's own hum loop
  is suppressed; `s.loopSound` at the hilt's index replaces it through
  `CG_EntityEffects` (a client with `cg_ambientSounds 0` hears no entity loops
  at all). Swings/hits/unholsters are server events (`EV_SABER_ATTACK`,
  `cg_event.c:2184`).
- The **wielded** blade is skipped by the `iwantout` return (`:10009`).
- A **thrown** saber (`saberInFlight`) is deliberately left as-is: the
  `iwantout` return is guarded by `!saberInFlight`, so the floating saber is
  visible and its own `s.loopSound` hum plays. Hiding the saber entity would
  remove that hum, which the user rejected.

## Limitations (documented, not bugs)

- Force Sight (`FP_SEE`) makes `CG_IsMindTricked` return 0 in the stock client
  (`cg_players.c:6373`), so a viewer using it sees the hidden player at the
  1 % scale. Force Sight is a stock force power, not a custom-client bypass.
- A determined custom client can ignore the trick bit and normalise
  `iModelScale`; the position is still in the packet. Defeating that requires
  not sending the player entity at all, which would remove the client-side
  footsteps and the saber hum — the opposite of the requirement.
- The cap applies to every viewer equally, so it does not detect who raised
  `cg_fov`; it removes the benefit for everyone. Legitimate FOVs below the cap
  are unaffected.

## Tests

`rust/src/fov.rs` unit tests: cap off outside `(0, 360)`, the
`cg_fov`→aspect conversion (including 100 covering `cg_fov 97` on 16:9),
horizontal and vertical frustum edges, a screen corner, behind the view
plane, the third-person camera correction (a close 70° target outside the eye
frustum but on screen from a camera 100 units back, while a far 70° target
stays capped), the steep-pitch pause, the spectator-viewer gate, the 16-client
bit mapping, the synthetic apply/restore
pass (body scale, trick bit, drawn saber hidden with the hilt hum, thrown saber
untouched, all fields restored). `rust/src/sdk.rs` pins the new offsets
(`iModelScale` 332, `trickedentindex*` 152/156/160/164, `pos.trBase` 28,
`clientSnapshot_t.ps.viewangles` 220) against the compiled SDK layout.

Runtime verification (to be run with the engine stack): set
`proxy_sv_maxFov 100` with the default 16:9 aspect and a client at
`cg_fov 97`; stand outside the cap's frustum and confirm the player is invisible
while their footsteps and saber hum are audible (the hum now comes from
`s.loopSound` for a hidden drawn saber); turn toward them and confirm the body,
blade and native hum return; confirm a corpse is fully hidden; confirm throwing
a saber still shows/plays the thrown saber. On a non-16:9 screen, set
`proxy_sv_maxFovAspect` to the actual aspect and re-check the edge. In third
person, keep `proxy_sv_maxFovCameraRange` at the stock `100` (or match the
player's `cg_thirdPersonRange`) and confirm a close target at the screen edge
stays visible; `0` is the first-person-strict test.

## References

- D-002.1 (minimal alteration; entry wrapper, no body copy).
- D-006.4 (the parallel-TFFA snapshot compaction this pass runs after).
- `docs/inventory.md` (hook map; `SV_SendClientSnapshot` was listed as dropped
  with the C++ anti-wallhack and is re-added here for the restore point).
