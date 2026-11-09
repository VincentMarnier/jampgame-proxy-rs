# D-023 — Anti-wallhack (`proxy_sv_antiWallHack`)

Date: 2026-11-09. Status: accepted (user directive). Supersedes the
"anti-wallhack dropped" part of D-002 for the cvar/feature, not for the original
implementation. Evidence: SDK `q_shared.h` (`trace_t`, `playerState_t.fd`,
`FP_SEE`, `MASK_OPAQUE`), `codemp/server/sv_world.cpp` (`SV_Trace`),
`codemp/server/sv_snapshot.cpp` (`SV_AddEntitiesVisibleFromPoint` /
`SV_SendClientSnapshot`), D-022 (`crate::fov` hide path), and the original C++
proxy at upstream commit `6879974`
(`src/jampgame_proxy/RuntimePatch/Engine/Patches/server/Proxy_sv_snapshot.cpp`).

## Context

A wallhack client draws players that the server keeps in the snapshot but no
wall should reveal. The server cannot detect the client, so the proxy removes
the information instead: a player with no line of sight from the viewer is
hidden for that viewer's snapshot. This is the same principle as D-022's FOV
cap, and the original C++ proxy had an anti-wallhack (`SV_CanSee` /
`SV_MoveEntityBehindCamera`). The original moved each hidden player's origin
behind the viewer's camera and forced `EF_TELEPORT_BIT`, which also moved the
client-side sounds and required an unlink/relink every snapshot.

The user asked to reuse the anti-FOV way of hiding players instead, with an
on/off cvar and margin cvar(s). The result keeps the D-022 entity fields and
therefore the D-022 sound guarantees (footsteps, swings, hits, saber hum all
remain at the true position) and the same apply/restore point.

## Decision

### D-023.1 — Cvars

- `proxy_sv_antiWallHack` (default `0` = off): enable the feature.
- `proxy_sv_antiWallHackMargin` (default `0`): world units added around the
  player's sampled body box. `0` is the exact game body box.
- `proxy_sv_antiWallHackCameraRange` (default `100`): maximum third-person
  camera setback used as the trace origin, mirroring
  `proxy_sv_maxFovCameraRange`. `0` traces from the eye. The actual start is
  pulled in by the client's camera collision (D-023.6).

The features compose: with both the FOV cap and the anti-wallhack on, a player
must be inside the frustum **and** in line of sight to be sent. The FOV cap's
steep-pitch pause does not disable the anti-wallhack (occlusion does not depend
on the aim pitch).

### D-023.2 — Visibility test

For each player entity already in the viewer's snapshot list:

1. Run the D-022 body/saber sampler (`crate::fov`): centre and eight corners of
   `gentity_t.r.mins/maxs`, plus hilt/middle/tip of every active blade of a
   drawn, non-thrown saber. The body box is expanded by
   `proxy_sv_antiWallHackMargin`; blade points stay exact.
2. Trace from the camera to each point with the engine's `SV_Trace`
   (`0x805a4c4`, called through `engine_fn!` per D-018), `mins`/`maxs` null,
   `contentmask = MASK_OPAQUE` (`CONTENTS_SOLID|CONTENTS_SLIME|CONTENTS_LAVA|CONTENTS_TERRAIN`),
   `passEntityNum = viewer` so the viewer's own body does not block. Player
   bodies (`CONTENTS_BODY`) do not stop the trace; water and other non-opaque
   contents do not either.
3. The player is visible when any sample point is reached with
   `trace.fraction >= 1.0`. A trace that starts inside solid is treated as
   clear (fail-open): a third-person client pulls its camera forward when a
   wall is behind it, so the server cannot know that the camera is not really
   inside geometry; blanking the whole stream would be worse than missing one
   player.

The camera is the engine's eye `origin` moved back along the view forward by
`proxy_sv_antiWallHackCameraRange`, then clamped to the client's camera
collision (D-023.6).

### D-023.6 — Third-person camera collision

The client does not render from the ideal full setback when something is
behind the player: `CG_UpdateThirdPersonTargetDamp` / the camera update trace a
`±CAMERA_SIZE` (`4`) box from the eye to the ideal location with
`MASK_CAMERACLIP` (`MASK_SOLID|CONTENTS_PLAYERCLIP`) and use `trace.endpos`
(`cg_view.c:13-14, 393-405, 451-458`). A server that kept the full setback
would put its line-of-sight origin behind the wall and hide a player standing
clearly in front — the reported symptom.

The proxy therefore runs the same box trace from the eye to the ideal camera
position (`SV_Trace`, `passEntityNum = viewer`,
`contentmask = MASK_CAMERACLIP`) and uses `endpos` when blocked. A trace that
starts inside solid falls back to the eye. This is an estimate, not the
client's damped position, but it follows the same geometric clamp the client
applies.

### D-023.3 — Who is exempt

- Viewers whose `sess.sessionTeam` is `TEAM_SPECTATOR` (free or following)
  are not affected, and spectator entities are not targets — same as D-022.
- Bot viewers (`r.svFlags & SVF_BOT`) are skipped, as in the original
  anti-wallhack: their snapshots are not rendered and their AI has its own
  perception.
- A viewer with Force Sight active (`ps.fd.forcePowersActive & (1 << FP_SEE)`,
  `frame+916`) is skipped: Force Sight is a stock power that intentionally
  shows players through walls. The FOV cap keeps its existing behaviour.

### D-023.4 — Hide and restore

The hidden player gets exactly the D-022 mutation: `EF_NODRAW`, the viewer's
`trickedentindex*` bit, `iModelScale = 1`, and for a drawn non-thrown saber
`saberHolstered = 2` with `s.loopSound` at the hilt's hum index. Nothing about
the player's position, health or owner changes. `SV_SendClientSnapshot` runs
the pristine body and then `crate::fov::restore`, so the shared `g_entities`
state is only changed while the engine copies this viewer's snapshot.

### D-023.5 — Parallel TFFA

The game-side trace gate (`crate::syscall`) zeroes foreign instances' contents
around each game `G_TRACE`. This pass runs in engine context, outside a game
call, so `crate::fov::apply` takes the same `flip_buf` once per snapshot, calls
`crate::tffa::trace_flips(viewer, MASK_OPAQUE, …)`, applies the flips around
the whole trace pass and restores them before returning. Without that, an
invisible foreign instance's door could block a viewer's line of sight and hide
a player who is visible in the viewer's own instance.

## Differences from the original C++ anti-wallhack

- The original relocated the target's `s.pos.trBase` behind the viewer's
  camera (keeping the distance for sound scaling) and set `EF_TELEPORT_BIT`;
  this port hides with `EF_NODRAW` + mind-trick + 1 % scale + holstered blade,
  at the true origin. This is the user-requested reuse of D-022.
- The original predicted both players' positions 0.1 s ahead and re-checked,
  and cached a visible target for 200 ms. This port tests the current snapshot
  poses only; the margin cvar is the flicker control.
- The original sampled four scaled hitbox offsets and used a ±90° FOV pre-check
  with a 20° pitch pause. This port samples the full body box and saber, and
  leaves angle filtering to the separate FOV cap.
- The original traced with `CM_BoxTrace` for the first test and `SV_Trace` for
  prediction; this port always uses `SV_Trace` so brush movers (doors, plats)
  block sight as they render.

## Limitations (documented, not bugs)

- Force Sight users are exempt by design; a wallhacker could therefore turn
  Force Sight on, but it is a stock, temporary, visible power.
- The third-person camera position is estimated: the collision clamp follows
  the client's box trace and hit point, but not its damping, vertical target
  offset or `cg_thirdPersonAngle`; the `startsolid` eye fallback keeps a
  degenerate viewpoint from blanking the stream.
- A custom client that strips `EF_NODRAW`, ignores the mind-trick bit and
  normalises `iModelScale` can still draw the entity; the position remains in
  the packet (same limit as D-022).
- Like the FOV cap, the test runs on the main view only (`portal == 0`). A
  player visible solely through a portal/mirror is judged by the main camera
  and can be hidden in that viewer's snapshot.
- The replacement hum is an entity loop: the stock 1.01 cgame skips loop
  sounds on `EF_NODRAW`, while TaystJK/OpenJK play them (same as D-022).
- A visible target costs up to nine body traces (centre first, then corners)
  plus up to 48 blade traces; hidden targets always cost all of them. The
  sampler stops at the first clear point.

## Tests

`rust/src/fov.rs` unit tests: body-box centre/corner ordering, expansion by
the margin (and that negative/NaN margins do not shrink it), the
line-of-sight predicate rejecting a fully blocked body and accepting a near
corner that the margin moves into view, the third-person camera setback, and
the camera-collision clamp (full setback when clear, hit point when a wall is
behind, eye when the eye is in solid). D-022's existing tests continue to cover
the hide/restore fields, saber handling, spectator gate and the FOV cap.

`rust/src/sdk.rs` pins the new ABI: `trace_t` size 48 and offsets
(`startsolid` 1, `fraction` 4, `endpos` 8, `plane` 20, `contents` 44),
`MASK_OPAQUE` `0x21003`, `MASK_CAMERACLIP` `0x1011`, `frame+916` for
`ps.fd.forcePowersActive`, and `FP_SEE` 14.

Runtime verification (with the engine stack): set `proxy_sv_antiWallHack 1`
and stand behind a wall from a player; the player must be hidden while their
footsteps and saber hum keep playing, and must reappear when the line of sight
clears. Check a drawn blade crossing a corner (the player stays visible) and a
thrown saber (still visible/audible). Set `proxy_sv_antiWallHackMargin 8`–`16`
and confirm cover-edge popping softens without revealing a player well behind
cover. In third person, compare
`proxy_sv_antiWallHackCameraRange 100` against `0` around a corner. Confirm
Force Sight, spectators and bots still see the normal stream, and, with
parallel TFFA enabled, that another instance's door does not hide a player.

## References

- D-002 (scope; its anti-wallhack drop is superseded for the feature).
- D-018 (`engine_fn!`, indirect engine calls).
- D-022 (FOV cap; the shared sampler, hide fields and restore point).
- `docs/inventory.md` (hook map).
- Original C++ proxy: `Proxy_sv_snapshot.cpp` at upstream commit `6879974`
  (`SV_CanSee`, `is_visible_from_point`, `SV_MoveEntityBehindCamera`).
