//! Per-viewer visibility filter: the FOV cap (`proxy_sv_maxFov`) and the
//! anti-wallhack (`proxy_sv_antiWallHack`) hide players while keeping their
//! sounds.
//!
//! A stock JKA client never tells the server its `cg_fov` (the cvar is
//! registered by cgame only, `cg_main.c:887`), and the server cannot see a
//! wallhack either. Instead, the proxy caps what any client can *receive*:
//! when `proxy_sv_maxFov` is `N`, a player outside the rendered view frustum
//! corresponding to `cg_fov N` is hidden for that viewer. Raising `cg_fov` above `N` therefore shows no extra players.
//! Modern clients widen `cg_fov` for the screen aspect (`cg_fovAspectAdjust`);
//! `proxy_sv_maxFovAspect` (default 16:9) is the reference used for that
//! conversion. A player is visible when any sampled point is inside the view:
//! the body box's corners and centre, plus the hilt/middle/tip of a drawn
//! (non-thrown) saber blade, so bodies and sabers at the screen border do not
//! pop in on the feet coordinate alone. `proxy_sv_maxFovMargin` expands the
//! volume outward by that many world units on every side.
//!
//! The anti-wallhack applies the same idea to occlusion instead of angles:
//! `proxy_sv_antiWallHack` traces from the viewer's camera to the same player
//! sample points and hides the player when no line reaches any of them.
//! `proxy_sv_antiWallHackMargin` grows the sampled body box by that many world
//! units, and `proxy_sv_antiWallHackCameraRange` places the trace start behind
//! the eye for third person. The two features compose: when both are on, a
//! player must be inside the frustum **and** in line of sight to stay visible.
//!
//! The hide is per viewer and instant: `EF_NODRAW` makes cgame's `CG_Player`
//! return at the top (no mind-trick fade). The entity stays in the snapshot, so
//! `CG_CheckEvents` still fires its server events (footsteps, weapon/force
//! sounds) at the true position. The viewer's bit in `trickedentindex*` and
//! `iModelScale = 1` (1 %) remain as layers for a client that ignores NODRAW:
//! the mind-trick path skips the body and wielded blade, and the tiny scale
//! makes the body/weapon hard to see. `iModelScale` does not scale the
//! client-side blade length, so a drawn, non-thrown saber is also marked
//! `saberHolstered = 2` (hidden) and its hum is replaced by `s.loopSound` at
//! the hilt's own game sound index; a thrown saber is left untouched.
//!
//! The mutations live on the shared `g_entities` state for exactly the time
//! the engine needs to copy the entity states into the viewer's snapshot:
//! applied at the end of `SV_AddEntitiesVisibleFromPoint` and restored right
//! after `SV_SendClientSnapshot` returns (D-022, D-023).

use core::ffi::c_int;

use crate::hooks::engine_sv::{read_i32, read_usize, write_i32};
use crate::sdk::{
    EF_DEAD, EF_NODRAW, FP_SEE, MASK_CAMERACLIP, MASK_OPAQUE, MAX_BLADES, MAX_CLIENTS, MAX_SABERS,
    MAX_SNAPSHOT_ENTITIES, OFFSET_BLADE_ACTIVE, OFFSET_BLADE_LENGTH, OFFSET_BLADE_MUZZLE_DIR,
    OFFSET_BLADE_MUZZLE_POINT, OFFSET_GCLIENT_SABER, OFFSET_GCLIENT_SESS, OFFSET_GENTITY_CLIENT,
    OFFSET_GENTITY_R_MAXS, OFFSET_GENTITY_R_MINS, OFFSET_GENTITY_S, OFFSET_GENTITY_S_EFLAGS,
    OFFSET_GENTITY_S_IMODEL_SCALE, OFFSET_GENTITY_S_LOOP_SOUND, OFFSET_GENTITY_S_POS_TRBASE,
    OFFSET_GENTITY_S_SABER_HOLSTERED, OFFSET_GENTITY_S_SABER_IN_FLIGHT, OFFSET_GENTITY_S_TRICKED_1,
    OFFSET_GENTITY_S_TRICKED_2, OFFSET_GENTITY_S_TRICKED_3, OFFSET_GENTITY_S_TRICKED_4,
    OFFSET_GENTITY_S_WEAPON, OFFSET_SABER_BLADE, OFFSET_SABER_NUM_BLADES, OFFSET_SABER_SOUND_LOOP,
    OFFSET_SESS_SESSION_TEAM, OFFSET_SHARED_SVFLAGS, OFFSET_SNAPSHOT_ENUMS_COUNT,
    OFFSET_SNAPSHOT_ENUMS_LIST, OFFSET_SNAPSHOT_FRAME_PS_FORCE_POWERS_ACTIVE,
    OFFSET_SNAPSHOT_FRAME_PS_VIEWANGLES, SIZEOF_BLADE_INFO, SIZEOF_GENTITY, SIZEOF_SABER_INFO,
    SVF_BOT, TEAM_SPECTATOR, Trace, WP_SABER,
};
use crate::state;

/// One player entity whose snapshot fields the proxy changed for the viewer
/// currently being built. Kept so `restore` can put the shared `g_entities`
/// state back once the engine has copied it.
#[derive(Debug, Clone, Copy)]
pub struct SavedEntity {
    /// Game entity slot (`0..MAX_CLIENTS`), `-1` for an unused record.
    pub ent: i32,
    pub e_flags: i32,
    pub i_model_scale: i32,
    pub trick: [i32; 4],
    /// Per-viewer saber hide (`saberHolstered = 2`) and the hum index that
    /// replaces the client-side loop it suppresses.
    pub saber_holstered: u8,
    pub loop_sound: u8,
}

impl Default for SavedEntity {
    fn default() -> Self {
        SavedEntity {
            ent: -1,
            e_flags: 0,
            i_model_scale: 0,
            trick: [0; 4],
            saber_holstered: 0,
            loop_sound: 0,
        }
    }
}

/// Per-viewer mutations pending until `SV_SendClientSnapshot` has copied the
/// entity states. At most one player entity per client slot is changed.
#[derive(Debug, Clone, Copy)]
pub struct FovState {
    pub saved: [SavedEntity; MAX_CLIENTS],
    pub count: usize,
    /// Actual engine client slot of the snapshot being built (not
    /// `frame->ps.clientNum`, which is the followed player's slot for a
    /// spectating spectator). Set by the `SV_SendClientSnapshot` wrapper.
    pub viewer_client: i32,
    /// True while the demo recorder builds its own snapshot. The visibility
    /// filter (FOV cap + anti-wallhack) is for real viewers only; the recorder
    /// build runs in the server-side recorder's own viewer and must not mutate
    /// shared entity states. The recorder sets/clears this around its single
    /// `SV_AddEntitiesVisibleFromPoint` call.
    pub recorder_build: bool,
}

impl Default for FovState {
    fn default() -> Self {
        FovState {
            saved: [SavedEntity::default(); MAX_CLIENTS],
            count: 0,
            viewer_client: -1,
            recorder_build: false,
        }
    }
}

/// Maximum view pitch (degrees) at which the cap is applied. Beyond it the
/// client's third-person camera pitch diverges from the server's aim (the
/// stock camera clamps the focus pitch to 45° and halves the placement pitch),
/// so a yaw/frustum test built on `ps.viewangles` would hide players that are
/// still on screen. The wide-FOV advantage is mostly horizontal at normal
/// pitches, so the cap simply pauses while looking steeply up/down.
pub const MAX_PITCH: f32 = 45.0;

/// Half horizontal and vertical angles (radians) of the rendered view for a
/// maximum **`cg_fov`** value, or `None` when the cap is off (`0`, negative,
/// or no useful cap at/above a full turn).
///
/// `cg_fov` is the value a player types. Modern clients (OpenJK/TaystJK,
/// `cg_fovAspectAdjust 1` by default) turn it into the rendered horizontal FOV:
///
/// ```text
/// fov_x = 2 * atan(tan(cg_fov/2) * 0.75 * aspect)
/// ```
///
/// with `aspect = width/height`; the vertical FOV follows the same perspective
/// (`tan(v/2) = tan(h/2) / aspect`). The default reference aspect is 16:9, so
/// `proxy_sv_maxFov 100` allows a `cg_fov 97` client on that screen (rendered
/// horizontal ≈113°). `0.75` is the 4:3 base the client formula uses; for a
/// 4:3/stock reference pass `aspect = 4/3` and the horizontal conversion is the
/// identity.
pub fn view_half_angles(max_fov: i32, aspect: f32) -> Option<(f32, f32)> {
    if max_fov <= 0 || max_fov >= 360 {
        return None;
    }
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect.clamp(0.5, 4.0)
    } else {
        16.0 / 9.0
    };
    // The client clamps `cg_fov` to 160 (`cg_view.c`), so a larger configured
    // cap is effectively "as wide as the client can go"; clamping also keeps
    // `tan(half)` finite and positive (it flips sign at 180).
    let capped = max_fov.min(160) as f32;
    let half = (capped * 0.5).to_radians();
    let h = (half.tan() * 0.75 * aspect).atan();
    let v = (h.tan() / aspect).atan();
    Some((h, v))
}

/// JKA `AngleVectors` for `[pitch, yaw, roll]` in degrees (`q_shared.c`).
pub fn view_vectors(angles: [f32; 3]) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let [pitch, yaw, roll] = angles;
    let (sp, cp) = pitch.to_radians().sin_cos();
    let (sy, cy) = yaw.to_radians().sin_cos();
    let (sr, cr) = roll.to_radians().sin_cos();
    let forward = [cp * cy, cp * sy, -sp];
    let right = [-sr * sp * cy + cr * sy, -sr * sp * sy - cr * cy, -sr * cp];
    let up = [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp];
    (forward, right, up)
}

/// A camera view frustum precomputed for one viewer snapshot.
///
/// The half-angles are stored as plane normals (`forward*sin - axis*cos`)
/// rather than tangents, so a 90° half-angle (the vertical margin can reach it)
/// stays finite. `margin` offsets every plane outward by that many world units,
/// which enlarges the visible volume on every axis: a point within `margin` of
/// the true frustum counts as inside.
#[derive(Debug, Clone, Copy)]
pub struct ViewFrustum {
    cam: [f32; 3],
    /// Inward unit normals of the right, left, top, bottom and near planes.
    planes: [[f32; 3]; 5],
    margin: f32,
}

impl ViewFrustum {
    /// Build the frustum for a camera placed `camera_range` units behind the
    /// eye (`origin`).
    ///
    /// The camera setback matters in third person: the client renders from a
    /// point up to `cg_thirdPersonRange` (default 100) behind the player, so a
    /// close target near the screen edge has a smaller bearing from the camera
    /// than from the eye (`proxy_sv_maxFovCameraRange`; `0` = first-person/eye).
    pub fn new(
        origin: [f32; 3],
        angles: [f32; 3],
        h_half: f32,
        v_half: f32,
        camera_range: f32,
        margin: f32,
    ) -> ViewFrustum {
        let (forward, right, up) = view_vectors(angles);
        let range = if camera_range.is_finite() {
            camera_range.max(0.0)
        } else {
            0.0
        };
        let cam = [
            origin[0] - forward[0] * range,
            origin[1] - forward[1] * range,
            origin[2] - forward[2] * range,
        ];
        // A half-angle beyond 90° would flip a plane normal and make the
        // "inside" test wrong; clamp to the valid range (a 90° half-angle is a
        // half-space on that axis, which `sin`/`cos` handle exactly).
        let half = |a: f32| {
            if a.is_finite() {
                a.clamp(0.0, core::f32::consts::FRAC_PI_2)
            } else {
                0.0
            }
        };
        let (sh, ch) = half(h_half).sin_cos();
        let (sv, cv) = half(v_half).sin_cos();
        // Side plane through the camera: inside when the point is on the
        // forward side of the plane, i.e. `dot(d, forward)*sin >= dot(d, axis)*cos`.
        let plane = |axis: [f32; 3], sin: f32, cos: f32| {
            [
                forward[0] * sin - axis[0] * cos,
                forward[1] * sin - axis[1] * cos,
                forward[2] * sin - axis[2] * cos,
            ]
        };
        let planes = [
            plane(right, sh, ch),
            // Left/top planes are the same construction with the axis flipped.
            [
                forward[0] * sh + right[0] * ch,
                forward[1] * sh + right[1] * ch,
                forward[2] * sh + right[2] * ch,
            ],
            plane(up, sv, cv),
            [
                forward[0] * sv + up[0] * cv,
                forward[1] * sv + up[1] * cv,
                forward[2] * sv + up[2] * cv,
            ],
            forward,
        ];
        let margin = if margin.is_finite() {
            margin.max(0.0)
        } else {
            0.0
        };
        ViewFrustum {
            cam,
            planes,
            margin,
        }
    }

    /// Whether `target` lies inside the frustum or within `margin` world units
    /// of it. A target at (or essentially at) the camera counts as visible so a
    /// degenerate direction never hides a player; a target behind the view
    /// plane is only visible within the margin.
    pub fn contains(&self, target: [f32; 3]) -> bool {
        let d = [
            target[0] - self.cam[0],
            target[1] - self.cam[1],
            target[2] - self.cam[2],
        ];
        // A corrupt/NaN position must not read as "inside" (NaN comparisons are
        // always false, which would otherwise pass every plane test).
        if !d.iter().all(|c| c.is_finite()) {
            return false;
        }
        let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
        if len2 <= 1.0 {
            return true;
        }
        for n in &self.planes {
            // Each normal is unit length, so the dot product is the signed
            // distance to that plane. The margin expands every plane outward.
            if d[0] * n[0] + d[1] * n[1] + d[2] * n[2] < -self.margin {
                return false;
            }
        }
        true
    }
}

/// Whether any sample point is inside the view, i.e. some visible part of the
/// player (body or saber) is on screen.
pub fn any_point_visible(frustum: &ViewFrustum, points: &[[f32; 3]]) -> bool {
    points.iter().any(|point| frustum.contains(*point))
}

/// `(trickedentindex* field, bit)` for a client number: clients 0-15 use
/// `trickedentindex`, 16-31 `trickedentindex2`, and so on
/// (`CG_IsMindTricked`, `cg_players.c:6369`).
pub fn trick_mask(viewer: i32) -> (usize, i32) {
    let field = (viewer.max(0) / 16) as usize;
    let field = field.min(3);
    let bit = 1i32 << (viewer.max(0) % 16);
    (field, bit)
}

/// Maximum sample points for one player: the body box (centre + eight corners)
/// plus `MAX_SABERS * MAX_BLADES` blades at three points each.
pub const MAX_PLAYER_POINTS: usize = 9 + MAX_SABERS * MAX_BLADES * 3;

/// Nine body sample points: the centre first, then the eight corners of the
/// `mins..maxs` box around `origin`, enlarged by `margin` world units on every
/// side. Centre-first matters for the anti-wallhack: a visible player usually
/// clears on the first trace.
pub fn body_points(origin: [f32; 3], mins: [f32; 3], maxs: [f32; 3], margin: f32) -> [[f32; 3]; 9] {
    let margin = if margin.is_finite() {
        margin.max(0.0)
    } else {
        0.0
    };
    let lo = [mins[0] - margin, mins[1] - margin, mins[2] - margin];
    let hi = [maxs[0] + margin, maxs[1] + margin, maxs[2] + margin];
    let mut body = [[0.0f32; 3]; 9];
    body[0] = [
        origin[0] + (lo[0] + hi[0]) * 0.5,
        origin[1] + (lo[1] + hi[1]) * 0.5,
        origin[2] + (lo[2] + hi[2]) * 0.5,
    ];
    let mut n = 1;
    for x in [lo[0], hi[0]] {
        for y in [lo[1], hi[1]] {
            for z in [lo[2], hi[2]] {
                body[n] = [origin[0] + x, origin[1] + y, origin[2] + z];
                n += 1;
            }
        }
    }
    body
}

/// Fill `out` with the points used to decide whether a player is visible: the
/// body box (centre + eight corners, expanded by `margin`) plus the hilt,
/// middle and tip of every active blade of a drawn, non-thrown saber.
///
/// The body box comes from `gentity_t.r.mins/maxs`, which the game sets to
/// `playerMins`/`playerMaxs` (`{-15,-15,-24}` .. `{15,15,40}`, `g_client.c`).
/// The saber blade positions are the server-side values
/// `WP_SaberPositionUpdate` (`w_saber.c:7655`, run from `g_main.c:4051`)
/// computes from the hand bolt, so the test follows the part of the player that
/// protrudes furthest into the view.
///
/// # Safety
///
/// `ent` must be a live `gentity_t` slot and `client` its live `gclient_t*`.
unsafe fn player_sample_points(
    ent: usize,
    client: usize,
    origin: [f32; 3],
    margin: f32,
    out: &mut [[f32; 3]; MAX_PLAYER_POINTS],
) -> usize {
    // SAFETY: ent is a live gentity; r.mins/r.maxs are vec3_t.
    let mins = unsafe { read_vec3(ent + OFFSET_GENTITY_R_MINS) };
    let maxs = unsafe { read_vec3(ent + OFFSET_GENTITY_R_MAXS) };
    let body = body_points(origin, mins, maxs, margin);
    out[..9].copy_from_slice(&body);
    let mut n = 9;
    if client == 0 {
        return n;
    }
    // A corpse has no live held saber; a thrown saber is a separate entity with
    // its own visibility; a holstered one is not rendered.
    // SAFETY: the fields below are plain ints/bytes in the live gentity.
    let e_flags = unsafe { read_i32(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_EFLAGS) };
    let weapon = unsafe { read_u8(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_WEAPON) };
    let saber_in_flight =
        unsafe { read_u8(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_SABER_IN_FLIGHT) } != 0;
    let saber_holstered =
        unsafe { read_u8(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_SABER_HOLSTERED) };
    if e_flags & EF_DEAD != 0 || weapon != WP_SABER || saber_in_flight || saber_holstered >= 2 {
        return n;
    }
    for saber in 0..MAX_SABERS {
        let saber_info = client + OFFSET_GCLIENT_SABER + saber * SIZEOF_SABER_INFO;
        // SAFETY: the gclient_t holds MAX_SABERS saberInfo_t entries.
        let num_blades = unsafe { read_i32(saber_info + OFFSET_SABER_NUM_BLADES) };
        let num_blades = num_blades.clamp(0, MAX_BLADES as c_int) as usize;
        for blade in 0..num_blades {
            let info = saber_info + OFFSET_SABER_BLADE + blade * SIZEOF_BLADE_INFO;
            // SAFETY: blade < MAX_BLADES, so info is inside this saberInfo_t.
            if unsafe { read_i32(info + OFFSET_BLADE_ACTIVE) } == 0 {
                continue;
            }
            // SAFETY: the blade's fields are plain scalars/vec3_t.
            let length = unsafe { read_f32(info + OFFSET_BLADE_LENGTH) };
            let base = unsafe { read_vec3(info + OFFSET_BLADE_MUZZLE_POINT) };
            let dir = unsafe { read_vec3(info + OFFSET_BLADE_MUZZLE_DIR) };
            if !length.is_finite() || length <= 0.0 {
                continue;
            }
            out[n] = base;
            out[n + 1] = [
                base[0] + dir[0] * length * 0.5,
                base[1] + dir[1] * length * 0.5,
                base[2] + dir[2] * length * 0.5,
            ];
            out[n + 2] = [
                base[0] + dir[0] * length,
                base[1] + dir[1] * length,
                base[2] + dir[2] * length,
            ];
            n += 3;
        }
    }
    n
}

/// Whether any visible part of a player is inside the view.
///
/// # Safety
///
/// See [`player_sample_points`].
unsafe fn player_visible(
    frustum: &ViewFrustum,
    ent: usize,
    client: usize,
    origin: [f32; 3],
) -> bool {
    let mut points = [[0.0f32; 3]; MAX_PLAYER_POINTS];
    // SAFETY: ent/client are live game structures.
    let n = unsafe { player_sample_points(ent, client, origin, 0.0, &mut points) };
    any_point_visible(frustum, &points[..n])
}

/// Whether any part of a player has a clear line of sight from the viewer.
/// `clear` decides one ray endpoint; the body box is enlarged by `margin`.
///
/// # Safety
///
/// See [`player_sample_points`].
unsafe fn player_in_line_of_sight<F: FnMut([f32; 3]) -> bool>(
    ent: usize,
    client: usize,
    origin: [f32; 3],
    margin: f32,
    mut clear: F,
) -> bool {
    let mut points = [[0.0f32; 3]; MAX_PLAYER_POINTS];
    // SAFETY: ent/client are live game structures.
    let n = unsafe { player_sample_points(ent, client, origin, margin, &mut points) };
    points[..n].iter().any(|point| clear(*point))
}

/// Whether the engine's `SV_Trace` finds no opaque geometry between `start` and
/// `end`. `pass_entity` is the viewer's entity number, so its own body does not
/// block the ray. A trace that starts inside solid returns `true`: a camera
/// point on a wall surface or a degenerate viewpoint must not blank the whole
/// stream.
///
/// # Safety
///
/// Engine memory layer and game module must be live; called from the
/// `SV_AddEntitiesVisibleFromPoint` wrapper.
unsafe fn line_clear(start: [f32; 3], end: [f32; 3], pass_entity: c_int) -> bool {
    let mut trace = Trace::default();
    // SAFETY: the engine's SV_Trace writes the whole trace_t; start/end are
    // stack vec3_t values and a zero box is the point trace.
    unsafe {
        crate::engine::sv_trace(
            &mut trace,
            start,
            [0.0; 3],
            [0.0; 3],
            end,
            pass_entity,
            MASK_OPAQUE,
        )
    };
    trace.startsolid != 0 || trace.fraction >= 1.0
}

/// `CAMERA_SIZE` (`cg_view.c:14`): the client's third-person camera collision
/// box is `±CAMERA_SIZE` on every axis.
pub const CAMERA_SIZE: f32 = 4.0;

/// Pick the camera position from the desired full setback and the engine's
/// camera-collision trace. The client traces its camera box from the eye to the
/// ideal third-person location and uses the hit point (`cg_view.c:393-405`), so
/// when a wall is behind the player the real camera is much closer than
/// `range`. `start_solid` falls back to the eye: no trustworthy camera point.
pub fn clamp_camera(
    eye: [f32; 3],
    desired: [f32; 3],
    start_solid: bool,
    blocked: bool,
    endpos: [f32; 3],
) -> [f32; 3] {
    if start_solid {
        eye
    } else if blocked {
        endpos
    } else {
        desired
    }
}

/// The camera position used by the anti-wallhack: the full third-person setback
/// from [`camera_origin`], clamped in by the client's own camera collision
/// (a `±CAMERA_SIZE` box trace with `MASK_CAMERACLIP`).
///
/// # Safety
///
/// Engine memory layer and game module must be live; called from the
/// `SV_AddEntitiesVisibleFromPoint` wrapper.
unsafe fn wallhack_camera(eye: [f32; 3], angles: [f32; 3], range: f32, viewer: c_int) -> [f32; 3] {
    let desired = camera_origin(eye, angles, range);
    let mut trace = Trace::default();
    // SAFETY: engine trace against the live world; the viewer is passed so its
    // own body cannot push the camera forward.
    unsafe {
        crate::engine::sv_trace(
            &mut trace,
            eye,
            [-CAMERA_SIZE; 3],
            [CAMERA_SIZE; 3],
            desired,
            viewer,
            MASK_CAMERACLIP,
        )
    };
    clamp_camera(
        eye,
        desired,
        trace.startsolid != 0,
        trace.fraction < 1.0,
        trace.endpos,
    )
}

/// The trace start: the eye `origin` moved `camera_range` world units backwards
/// along the view direction (the third-person camera).
pub fn camera_origin(origin: [f32; 3], angles: [f32; 3], camera_range: f32) -> [f32; 3] {
    let (forward, _, _) = view_vectors(angles);
    let range = if camera_range.is_finite() {
        camera_range.max(0.0)
    } else {
        0.0
    };
    [
        origin[0] - forward[0] * range,
        origin[1] - forward[1] * range,
        origin[2] - forward[2] * range,
    ]
}

/// Apply the per-viewer visibility filter to every player entity in `e_nums`:
/// hide it when the FOV cap (`proxy_sv_maxFov`) says it is outside the view or
/// the anti-wallhack (`proxy_sv_antiWallHack`) finds no line of sight. Called
/// after the pristine (and parallel-TFFA) snapshot pass, for the main view only
/// (`portal == false`).
///
/// # Safety
///
/// Called from the `SV_AddEntitiesVisibleFromPoint` entry wrapper with the
/// engine's `origin` and `clientSnapshot_t*`/`snapshotEntityNumbers_t*`; all
/// three point at valid engine structures and `g_entities` is the located
/// game array.
pub unsafe fn apply(
    viewer: c_int,
    origin: usize,
    frame: usize,
    e_nums: usize,
    g_entities: usize,
    portal: c_int,
) {
    // A prior build should have restored already; do it again so a missed
    // post-hook can never leak a mutation into another viewer's snapshot.
    // Portal calls happen after the main call in the same build and must not
    // undo its mutations before the engine copies the entity states.
    if portal != 0 {
        return;
    }
    if g_entities == 0 || origin == 0 || frame == 0 || e_nums == 0 {
        return;
    }
    if !(0..MAX_CLIENTS as c_int).contains(&viewer) {
        return;
    }
    let (
        max_fov,
        aspect,
        fov_camera_range,
        fov_margin,
        anti_wallhack,
        wallhack_margin,
        wallhack_camera_range,
        requested_viewer,
        recorder_build,
    ) = state::with_state(|s| {
        (
            s.cvars[state::CVAR_MAX_FOV].integer,
            s.cvars[state::CVAR_MAX_FOV_ASPECT].value,
            s.cvars[state::CVAR_MAX_FOV_CAMERA_RANGE].value,
            s.cvars[state::CVAR_MAX_FOV_MARGIN].value,
            s.cvars[state::CVAR_ANTI_WALLHACK].integer,
            s.cvars[state::CVAR_ANTI_WALLHACK_MARGIN].value,
            s.cvars[state::CVAR_ANTI_WALLHACK_CAMERA_RANGE].value,
            s.fov.viewer_client,
            s.fov.recorder_build,
        )
    });
    if recorder_build {
        return;
    }
    restore();
    let viewer_client = if (0..MAX_CLIENTS as i32).contains(&requested_viewer) {
        requested_viewer
    } else {
        viewer
    };
    // The feature only applies to players actually in the game. A following
    // spectator's `frame->ps.clientNum` is the followed player, so use the
    // engine client slot recorded by `SV_SendClientSnapshot`.
    // SAFETY: g_entities is the located game array.
    if unsafe { is_spectator(g_entities, viewer_client) } {
        return;
    }
    let fov_angles = view_half_angles(max_fov, aspect);
    // Bots' snapshots are not rendered and their AI has its own perception, so
    // the original anti-wallhack skipped them too; hiding entities from a bot
    // packet buys nothing.
    // SAFETY: g_entities is the located game array.
    let wallhack_on = anti_wallhack != 0 && !unsafe { is_bot(g_entities, viewer_client) };
    if fov_angles.is_none() && !wallhack_on {
        return;
    }
    // SAFETY: `frame` is a valid clientSnapshot_t; viewangles is a vec3_t.
    let pitch = unsafe { read_f32(frame + OFFSET_SNAPSHOT_FRAME_PS_VIEWANGLES) };
    let yaw = unsafe { read_f32(frame + OFFSET_SNAPSHOT_FRAME_PS_VIEWANGLES + 4) };
    let roll = unsafe { read_f32(frame + OFFSET_SNAPSHOT_FRAME_PS_VIEWANGLES + 8) };
    if !pitch.is_finite() || !yaw.is_finite() || !roll.is_finite() {
        return;
    }
    let angles = [pitch, yaw, roll];
    // SAFETY: `origin` is the engine's vec3_t viewpoint argument.
    let viewpoint = unsafe { read_vec3(origin) };
    // The FOV cap pauses at steep pitch (D-022.1); the anti-wallhack does not
    // depend on the aim's pitch, only on the camera position and occlusion.
    let frustum = match fov_angles {
        Some((h_half, v_half)) if pitch.abs() <= MAX_PITCH => {
            // The stock third-person camera pitches toward its focus point, so
            // its vertical view can sit a few degrees off `ps.viewangles` even
            // below the pause threshold. A small vertical margin keeps
            // on-screen players visible; the horizontal bound (the FOV cheat's
            // axis) stays exact.
            let v_half = (v_half + 10.0f32.to_radians()).min(core::f32::consts::FRAC_PI_2);
            Some(ViewFrustum::new(
                viewpoint,
                angles,
                h_half,
                v_half,
                fov_camera_range,
                fov_margin,
            ))
        }
        _ => None,
    };
    // Force Sight intentionally shows players through walls; a viewer using it
    // is not wallhacking. The original anti-wallhack had the same exemption.
    // SAFETY: frame is a valid clientSnapshot_t.
    let force_see = wallhack_on
        && (unsafe { read_i32(frame + OFFSET_SNAPSHOT_FRAME_PS_FORCE_POWERS_ACTIVE) }
            & (1 << FP_SEE))
            != 0;
    let wall_cam = if wallhack_on && !force_see {
        // The client's third-person camera stops at the wall behind the player
        // instead of staying at the full `cg_thirdPersonRange`. A camera
        // estimate that ignored that would sit behind the wall and hide a
        // player standing clearly in front.
        // SAFETY: engine memory layer and game module are live here.
        Some(unsafe { wallhack_camera(viewpoint, angles, wallhack_camera_range, viewer_client) })
    } else {
        None
    };
    if frustum.is_none() && wall_cam.is_none() {
        return;
    }
    // SAFETY: `e_nums` is a valid snapshotEntityNumbers_t.
    let count = unsafe { read_i32(e_nums + OFFSET_SNAPSHOT_ENUMS_COUNT) };
    if count <= 0 {
        return;
    }
    let count = (count as usize).min(MAX_SNAPSHOT_ENTITIES);
    // In parallel TFFA the world is per instance. The game-side trace gate
    // (`crate::syscall`) zeroes foreign instances' contents around each game
    // trap; this pass runs outside a game call, so it owns one window for the
    // whole snapshot. Without it, an invisible foreign door could block a
    // viewer's line of sight and hide a player who is visible in their own
    // instance.
    let mut wall_flips = state::with_state(|s| core::mem::take(&mut s.tffa.flip_buf));
    let wall_flips_n = if wall_cam.is_some() && crate::state::instances_enabled() {
        crate::tffa::trace_flips(viewer_client, MASK_OPAQUE, &mut wall_flips)
    } else {
        0
    };
    if wall_flips_n > 0 {
        crate::tffa::flip_apply(&wall_flips[..wall_flips_n]);
    }
    let (trick_field, trick_bit) = trick_mask(viewer);
    let mut local = [SavedEntity::default(); MAX_CLIENTS];
    let mut saved_count = 0usize;
    for i in 0..count {
        // SAFETY: the list holds `count` entity numbers.
        let ent_num = unsafe { read_i32(e_nums + OFFSET_SNAPSHOT_ENUMS_LIST + i * 4) };
        if ent_num < 0 || ent_num >= MAX_CLIENTS as c_int || ent_num == viewer {
            continue;
        }
        let ent = g_entities + ent_num as usize * SIZEOF_GENTITY;
        // Only slots that really hold a player: a free client slot keeps a
        // zero `client` pointer (`snapshot_keep` uses the same test).
        // SAFETY: ent is inside the g_entities array; client is a pointer.
        let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
        if client == 0 {
            continue;
        }
        // Never hide a spectator's entity: the feature only concerns the
        // players actually in the game.
        // SAFETY: `client` is the live gclient_t of this slot.
        if unsafe { is_spectator_client(client) } {
            continue;
        }
        // SAFETY: `ent` is a live gentity slot; pos.trBase is a vec3_t.
        let origin = unsafe { read_vec3(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_POS_TRBASE) };
        let mut visible = true;
        if let Some(frustum) = frustum.as_ref() {
            // SAFETY: `ent` and `client` are live game structures; the gentity
            // bounds and the server-side saber blade positions are maintained by
            // the game module.
            if !unsafe { player_visible(frustum, ent, client, origin) } {
                visible = false;
            }
        }
        if visible {
            if let Some(cam) = wall_cam {
                // SAFETY: `ent` and `client` are live game structures; the
                // engine trace runs against the live world.
                if !unsafe {
                    player_in_line_of_sight(ent, client, origin, wallhack_margin, |point| {
                        line_clear(cam, point, viewer_client)
                    })
                } {
                    visible = false;
                }
            }
        }
        if visible {
            continue;
        }
        if saved_count >= MAX_CLIENTS {
            break;
        }
        // SAFETY: the fields below are plain ints/bytes inside the live gentity.
        let saved = SavedEntity {
            ent: ent_num,
            e_flags: unsafe { read_i32(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_EFLAGS) },
            i_model_scale: unsafe {
                read_i32(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_IMODEL_SCALE)
            },
            trick: [
                unsafe { read_i32(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_TRICKED_1) },
                unsafe { read_i32(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_TRICKED_2) },
                unsafe { read_i32(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_TRICKED_3) },
                unsafe { read_i32(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_TRICKED_4) },
            ],
            saber_holstered: unsafe {
                read_u8(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_SABER_HOLSTERED)
            },
            loop_sound: unsafe { read_u8(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_LOOP_SOUND) },
        };
        local[saved_count] = saved;
        saved_count += 1;
        // SAFETY: saved fields are plain ints/bytes in the live gentity.
        let weapon = unsafe { read_u8(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_WEAPON) };
        let saber_in_flight =
            unsafe { read_u8(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_SABER_IN_FLIGHT) } != 0;
        // Mind-trick this viewer: stock clients skip the body and the wielded
        // saber blade, but keep the saber hum and the entity's server events.
        let trick_off = match trick_field {
            0 => OFFSET_GENTITY_S_TRICKED_1,
            1 => OFFSET_GENTITY_S_TRICKED_2,
            2 => OFFSET_GENTITY_S_TRICKED_3,
            _ => OFFSET_GENTITY_S_TRICKED_4,
        };
        unsafe {
            write_i32(
                ent + OFFSET_GENTITY_S + trick_off,
                saved.trick[trick_field] | trick_bit,
            );
            // 0 means 100% on the client; 1 means 1%.
            write_i32(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_IMODEL_SCALE, 1);
            // NODRAW is the instant hide: cgame's `CG_Player` returns at the
            // top instead of fading the mind-trick alpha over ~0.5 s. The
            // entity stays in the snapshot, so `CG_CheckEvents` still fires its
            // server events (footsteps, weapon/force sounds) at the true
            // position. The mind-trick bit and 1% scale stay as layers for a
            // client that ignores NODRAW.
            write_i32(
                ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_EFLAGS,
                saved.e_flags | EF_NODRAW,
            );
            if saved.e_flags & EF_DEAD == 0
                && weapon == WP_SABER
                && !saber_in_flight
                && saved.saber_holstered < 2
            {
                // `iModelScale` scales the hand/bolt origin but not the
                // client-side blade length, so hide the blade the only way the
                // entityState allows (for a client that ignores NODRAW).
                // `saberHolstered = 2` also suppresses the client's own hum
                // loop; replace it with `s.loopSound` at the hilt's hum index
                // so the sound stays. A thrown saber is left exactly as-is.
                write_u8(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_SABER_HOLSTERED, 2);
                let hum = saber_hum_index(client);
                if hum != 0 {
                    write_u8(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_LOOP_SOUND, hum);
                }
            }
        }
    }
    if wall_flips_n > 0 {
        crate::tffa::flip_restore(&wall_flips[..wall_flips_n]);
    }
    state::with_state(|s| {
        wall_flips.clear();
        s.tffa.flip_buf = wall_flips;
        s.fov.saved = local;
        s.fov.count = saved_count;
    });
}

/// Put every field changed by [`apply`] back on the shared `g_entities` state.
/// Called right after `SV_SendClientSnapshot` has copied the entity states
/// into the viewer's snapshot.
pub fn restore() {
    let (g_entities, saved, count) = state::with_state(|s| {
        let g_entities = s.located_game_data.g_entities;
        let saved = s.fov.saved;
        let count = s.fov.count;
        if count != 0 {
            s.fov.saved = [SavedEntity::default(); MAX_CLIENTS];
            s.fov.count = 0;
        }
        (g_entities, saved, count)
    });
    if count == 0 || g_entities == 0 {
        return;
    }
    for saved in saved.iter().take(count.min(MAX_CLIENTS)) {
        if saved.ent < 0 || saved.ent >= MAX_CLIENTS as i32 {
            continue;
        }
        let ent = g_entities + saved.ent as usize * SIZEOF_GENTITY;
        // SAFETY: the slot was a live gentity when saved and no engine call
        // between `apply` and `restore` frees or reuses a player slot.
        unsafe {
            write_i32(
                ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_EFLAGS,
                saved.e_flags,
            );
            write_i32(
                ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_IMODEL_SCALE,
                saved.i_model_scale,
            );
            write_i32(
                ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_TRICKED_1,
                saved.trick[0],
            );
            write_i32(
                ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_TRICKED_2,
                saved.trick[1],
            );
            write_i32(
                ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_TRICKED_3,
                saved.trick[2],
            );
            write_i32(
                ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_TRICKED_4,
                saved.trick[3],
            );
            write_u8(
                ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_SABER_HOLSTERED,
                saved.saber_holstered,
            );
            write_u8(
                ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_LOOP_SOUND,
                saved.loop_sound,
            );
        }
    }
}

/// Whether a game client is on the spectator team (`TEAM_SPECTATOR`).
///
/// # Safety
///
/// `client` must be a live `gclient_t*`.
unsafe fn is_spectator_client(client: usize) -> bool {
    // SAFETY: caller guarantees a live gclient_t; the session team is an int.
    unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) == TEAM_SPECTATOR }
}

/// Whether the player in `slot` is a spectator (including a following one,
/// whose playerState is the followed player's but whose session team stays
/// spectator).
///
/// # Safety
///
/// `g_entities` must be the located game array and `slot` in `0..MAX_CLIENTS`.
unsafe fn is_spectator(g_entities: usize, slot: i32) -> bool {
    if g_entities == 0 || !(0..MAX_CLIENTS as i32).contains(&slot) {
        return false;
    }
    let ent = g_entities + slot as usize * SIZEOF_GENTITY;
    // SAFETY: slot is inside the g_entities array; client is a pointer.
    let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
    client != 0 && unsafe { is_spectator_client(client) }
}

/// Whether the player in `slot` is a bot (`r.svFlags & SVF_BOT`, the engine's
/// own discriminator).
///
/// # Safety
///
/// `g_entities` must be the located game array and `slot` in `0..MAX_CLIENTS`.
unsafe fn is_bot(g_entities: usize, slot: i32) -> bool {
    if g_entities == 0 || !(0..MAX_CLIENTS as i32).contains(&slot) {
        return false;
    }
    let ent = g_entities + slot as usize * SIZEOF_GENTITY;
    // SAFETY: slot is inside the g_entities array; svFlags is an int field.
    unsafe { read_i32(ent + OFFSET_SHARED_SVFLAGS) & SVF_BOT as i32 != 0 }
}

/// Read an `f32` engine/game struct field.
///
/// # Safety
///
/// `addr` must be a readable float field.
unsafe fn read_f32(addr: usize) -> f32 {
    // SAFETY: caller guarantees a readable 4-byte field.
    f32::from_bits(unsafe { read_i32(addr) } as u32)
}

/// Read a `vec3_t` field.
///
/// # Safety
///
/// `addr` must point at three readable floats.
unsafe fn read_vec3(addr: usize) -> [f32; 3] {
    // SAFETY: caller guarantees twelve readable bytes.
    unsafe { [read_f32(addr), read_f32(addr + 4), read_f32(addr + 8)] }
}

/// Read a `byte` engine/game struct field.
///
/// # Safety
///
/// `addr` must be a readable byte field.
unsafe fn read_u8(addr: usize) -> u8 {
    // SAFETY: caller guarantees one readable byte.
    unsafe { core::ptr::read_unaligned(addr as *const u8) }
}

/// Write a `byte` engine/game struct field.
///
/// # Safety
///
/// `addr` must be a writable byte field the proxy saved and is restoring.
unsafe fn write_u8(addr: usize, value: u8) {
    // SAFETY: caller guarantees a writable byte field.
    unsafe { core::ptr::write_unaligned(addr as *mut u8, value) }
}

/// The hilt's saber-hum game sound index for a player (`gclient_t.saber[0/1]`),
/// or `0` when the slot has no usable saber sound.
///
/// `s.loopSound` is a byte and `cgs.gameSounds` is indexed by these game sound
/// indexes; `w_saber.c:7039` copies the same value to a floating saber entity.
///
/// # Safety
///
/// `client` must be the live `gclient_t*` of a player slot.
unsafe fn saber_hum_index(client: usize) -> u8 {
    if client == 0 {
        return 0;
    }
    for saber in 0..2 {
        // SAFETY: `client` is a live gclient_t; `saber` is one of its two
        // saberInfo_t entries and `soundLoop` is a plain int.
        let index = unsafe {
            read_i32(
                client + OFFSET_GCLIENT_SABER + saber * SIZEOF_SABER_INFO + OFFSET_SABER_SOUND_LOOP,
            )
        };
        if (1..=u8::MAX as i32).contains(&index) {
            return index as u8;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f32::consts::PI;

    /// The client's aspect conversion (`cg_fovAspectAdjust`): a `cg_fov`
    /// value becomes `2*atan(tan(cg_fov/2) * 0.75 * aspect)` rendered degrees.
    fn rendered_half(fov: f32, aspect: f32) -> f32 {
        ((fov * 0.5).to_radians().tan() * 0.75 * aspect).atan()
    }

    #[test]
    fn cap_is_off_outside_one_turn() {
        assert_eq!(view_half_angles(0, 16.0 / 9.0), None);
        assert_eq!(view_half_angles(-10, 16.0 / 9.0), None);
        assert_eq!(view_half_angles(360, 16.0 / 9.0), None);
        assert_eq!(view_half_angles(400, 16.0 / 9.0), None);
    }

    #[test]
    fn cap_uses_the_cg_fov_to_aspect_conversion() {
        // 4:3 is the client formula's base: the horizontal cap is the value.
        let (h, v) = view_half_angles(100, 4.0 / 3.0).unwrap();
        assert!((h - (50.0f32 * PI / 180.0)).abs() < 1e-6);
        assert!((v.tan() - h.tan() * 3.0 / 4.0).abs() < 1e-6);
        // 16:9 widens it, so `proxy_sv_maxFov 100` still covers `cg_fov 97`
        // (whose rendered horizontal FOV is about 113 degrees).
        let (h, v) = view_half_angles(100, 16.0 / 9.0).unwrap();
        assert!((h - rendered_half(100.0, 16.0 / 9.0)).abs() < 1e-6);
        assert!((v.tan() - h.tan() * 9.0 / 16.0).abs() < 1e-6);
        let fov_97 = rendered_half(97.0, 16.0 / 9.0) * 2.0;
        let cap_100 = rendered_half(100.0, 16.0 / 9.0) * 2.0;
        assert!(
            cap_100 > fov_97,
            "100 cap {cap_100} must cover cg_fov 97 {fov_97}"
        );
        // A missing or zero aspect falls back to 16:9.
        assert_eq!(
            view_half_angles(100, 0.0),
            view_half_angles(100, 16.0 / 9.0)
        );
    }

    #[test]
    fn view_vectors_match_angle_vectors() {
        let (f, r, u) = view_vectors([0.0, 0.0, 0.0]);
        assert!((f[0] - 1.0).abs() < 1e-6 && f[1].abs() < 1e-6 && f[2].abs() < 1e-6);
        assert!(r[0].abs() < 1e-6 && (r[1] + 1.0).abs() < 1e-6 && r[2].abs() < 1e-6);
        assert!(u[0].abs() < 1e-6 && u[1].abs() < 1e-6 && (u[2] - 1.0).abs() < 1e-6);
        let (f, _, _) = view_vectors([90.0, 0.0, 0.0]);
        assert!((f[2] + 1.0).abs() < 1e-6);
        let (f, _, _) = view_vectors([0.0, 90.0, 0.0]);
        assert!(f[0].abs() < 1e-6 && (f[1] - 1.0).abs() < 1e-6);
    }

    fn view(
        origin: [f32; 3],
        angles: [f32; 3],
        h_half: f32,
        v_half: f32,
        camera_range: f32,
        margin: f32,
    ) -> ViewFrustum {
        ViewFrustum::new(origin, angles, h_half, v_half, camera_range, margin)
    }

    #[test]
    fn frustum_keeps_front_and_drops_outside() {
        let o = [0.0, 0.0, 0.0];
        let a = [0.0, 0.0, 0.0];
        let h = 50.0f32.to_radians();
        let v = 40.0f32.to_radians();
        let f = view(o, a, h, v, 0.0, 0.0);
        // Straight ahead.
        assert!(f.contains([100.0, 0.0, 0.0]));
        // Horizontal 40 (inside) and 60 (outside).
        let d = 40.0f32.to_radians();
        assert!(f.contains([d.cos() * 100.0, d.sin() * 100.0, 0.0]));
        let d = 60.0f32.to_radians();
        assert!(!f.contains([d.cos() * 100.0, d.sin() * 100.0, 0.0]));
        // Vertical 30 (inside) and 50 (outside); JKA negative pitch looks up.
        let d = 30.0f32.to_radians();
        assert!(f.contains([d.cos() * 100.0, 0.0, -d.sin() * 100.0]));
        let d = 50.0f32.to_radians();
        assert!(!f.contains([d.cos() * 100.0, 0.0, -d.sin() * 100.0]));
        // A screen corner (horizontal 40, vertical 30) is inside.
        let hd = 40.0f32.to_radians();
        let vd = 30.0f32.to_radians();
        assert!(f.contains([
            hd.cos() * vd.cos() * 100.0,
            hd.sin() * vd.cos() * 100.0,
            -vd.sin() * 100.0
        ]));
        // Behind the view plane.
        assert!(!f.contains([-100.0, 0.0, 0.0]));
        // At the camera never hides.
        assert!(f.contains(o));
    }

    #[test]
    fn margin_expands_the_volume_on_every_axis() {
        let o = [0.0, 0.0, 0.0];
        let a = [0.0, 0.0, 0.0];
        let h = 50.0f32.to_radians();
        let v = 40.0f32.to_radians();
        let exact = view(o, a, h, v, 0.0, 0.0);
        let expanded = view(o, a, h, v, 0.0, 8.0);
        // 100 units ahead, the horizontal edge sits at 100*tan(50) = 119.18.
        // A point 3 units beyond it is outside the exact frustum but inside the
        // volume enlarged by 8 world units.
        let edge = 100.0 * h.tan();
        let just_out = [100.0, edge + 3.0, 0.0];
        assert!(!exact.contains(just_out));
        assert!(expanded.contains(just_out));
        // Same on the vertical axis (the edge is `100*tan(40)` there).
        let edge = 100.0 * v.tan();
        let just_out = [100.0, 0.0, -(edge + 3.0)];
        assert!(!exact.contains(just_out));
        assert!(expanded.contains(just_out));
        // And behind the camera: the near plane also moves back by the margin.
        assert!(!exact.contains([-3.0, 0.0, 0.0]));
        assert!(expanded.contains([-3.0, 0.0, 0.0]));
        // A point far outside is still hidden.
        assert!(!expanded.contains([0.0, 1000.0, 0.0]));
    }

    #[test]
    fn third_person_camera_keeps_near_edge_targets() {
        let o = [0.0, 0.0, 0.0];
        let a = [0.0, 0.0, 0.0];
        let h = 50.0f32.to_radians();
        let v = 40.0f32.to_radians();
        // A close target 70 degrees off the eye, 100 units away: outside the
        // eye frustum...
        let d = 70.0f32.to_radians();
        let target = [d.cos() * 100.0, d.sin() * 100.0, 0.0];
        assert!(!view(o, a, h, v, 0.0, 0.0).contains(target));
        // ...but the client renders it from a camera 100 units behind the
        // player, where its bearing is about 35 degrees: on screen.
        assert!(view(o, a, h, v, 100.0, 0.0).contains(target));
        // A far target at the same eye bearing is still capped.
        let far = [d.cos() * 2000.0, d.sin() * 2000.0, 0.0];
        assert!(!view(o, a, h, v, 100.0, 0.0).contains(far));
    }

    #[test]
    fn trick_bit_matches_the_client_mapping() {
        assert_eq!(trick_mask(0), (0, 1));
        assert_eq!(trick_mask(15), (0, 1 << 15));
        assert_eq!(trick_mask(16), (1, 1));
        assert_eq!(trick_mask(31), (1, 1 << 15));
    }

    struct Hidden {
        entities: Vec<u8>,
        _gclient: Vec<u8>,
        _viewer_gclient: Vec<u8>,
        trick: i32,
        scale: i32,
        e_flags: i32,
        saber_holstered: u8,
        loop_sound: u8,
    }

    /// Write a `vec3_t` into a byte buffer standing in for game memory.
    fn write_vec3(buf: &mut [u8], v: [f32; 3]) {
        for (i, c) in v.iter().enumerate() {
            buf[i * 4..(i + 1) * 4].copy_from_slice(&c.to_ne_bytes());
        }
    }

    /// Configure one blade of the fake `gclient_t.saber[]`.
    fn set_blade(
        gclient: &mut [u8],
        saber: usize,
        blade: usize,
        active: bool,
        length: f32,
        base: [f32; 3],
        dir: [f32; 3],
    ) {
        let client = gclient.as_mut_ptr() as usize;
        let info = client + OFFSET_GCLIENT_SABER + saber * SIZEOF_SABER_INFO;
        // SAFETY: the buffer holds SIZEOF_GCLIENT bytes; `saber`/`blade` are
        // inside MAX_SABERS/MAX_BLADES and every field offset is pinned.
        unsafe {
            core::ptr::write_unaligned(
                (info + OFFSET_SABER_NUM_BLADES) as *mut i32,
                (blade + 1) as i32,
            );
            let b = info + OFFSET_SABER_BLADE + blade * SIZEOF_BLADE_INFO;
            core::ptr::write_unaligned(
                (b + OFFSET_BLADE_ACTIVE) as *mut i32,
                if active { 1 } else { 0 },
            );
            core::ptr::write_unaligned((b + OFFSET_BLADE_LENGTH) as *mut f32, length);
            for (i, c) in base.iter().enumerate() {
                core::ptr::write_unaligned((b + OFFSET_BLADE_MUZZLE_POINT + i * 4) as *mut f32, *c);
            }
            for (i, c) in dir.iter().enumerate() {
                core::ptr::write_unaligned((b + OFFSET_BLADE_MUZZLE_DIR + i * 4) as *mut f32, *c);
            }
        }
    }

    /// Synthesise the engine buffers the hook receives and run the real
    /// apply/restore pass over them. `saber` arms the player with a drawn
    /// saber using a fake `gclient_t` whose `saber[0].soundLoop` is 42.
    #[allow(clippy::too_many_arguments)]
    fn run_hide(
        place: [f32; 3],
        max_fov: i32,
        aspect: f32,
        camera_range: f32,
        margin: f32,
        pitch: f32,
        saber: bool,
        viewer_spectator: bool,
    ) -> Hidden {
        run_hide_with(
            place,
            max_fov,
            aspect,
            camera_range,
            margin,
            pitch,
            saber,
            viewer_spectator,
            |_, _, _| {},
        )
    }

    /// [`run_hide`] plus a hook to fill extra game fields (body bounds, saber
    /// blades) before the pass runs. `setup(entities, gclient, base)` receives
    /// the player slot's entity byte offset.
    #[allow(clippy::too_many_arguments)]
    fn run_hide_with<F: FnOnce(&mut [u8], &mut [u8], usize)>(
        place: [f32; 3],
        max_fov: i32,
        aspect: f32,
        camera_range: f32,
        margin: f32,
        pitch: f32,
        saber: bool,
        viewer_spectator: bool,
        setup: F,
    ) -> Hidden {
        let _guard = state::TEST_STATE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut entities = vec![0u8; MAX_CLIENTS * SIZEOF_GENTITY];
        let mut gclient = vec![0u8; crate::sdk::SIZEOF_GCLIENT];
        let mut viewer_gclient = vec![0u8; crate::sdk::SIZEOF_GCLIENT];
        let ent = 1usize;
        let base = ent * SIZEOF_GENTITY;
        // Slot 1 holds a live player: point it at the fake gclient.
        entities[base + OFFSET_GENTITY_CLIENT
            ..base + OFFSET_GENTITY_CLIENT + core::mem::size_of::<usize>()]
            .copy_from_slice(&(gclient.as_mut_ptr() as usize).to_ne_bytes());
        if viewer_spectator {
            let viewer = viewer_gclient.as_mut_ptr() as usize;
            // SAFETY: the buffer has the pinned gclient session field.
            unsafe {
                core::ptr::write_unaligned(
                    (viewer + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) as *mut i32,
                    TEAM_SPECTATOR,
                )
            };
            entities[OFFSET_GENTITY_CLIENT..OFFSET_GENTITY_CLIENT + core::mem::size_of::<usize>()]
                .copy_from_slice(&viewer.to_ne_bytes());
        }
        if saber {
            let client = gclient.as_mut_ptr() as usize;
            let hum = client + OFFSET_GCLIENT_SABER + OFFSET_SABER_SOUND_LOOP;
            // SAFETY: the buffer is large enough for the pinned saber field.
            unsafe { core::ptr::write_unaligned(hum as *mut i32, 42) };
            entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_WEAPON] = WP_SABER;
        }
        // Position seen from the viewer at the origin, looking down +X.
        for (i, v) in place.iter().enumerate() {
            entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_POS_TRBASE + i * 4
                ..base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_POS_TRBASE + (i + 1) * 4]
                .copy_from_slice(&v.to_ne_bytes());
        }
        setup(&mut entities, &mut gclient, base);
        // Viewer 0: viewangles = [pitch, yaw, roll], yaw 0.
        let mut frame = vec![0u8; 512];
        frame[208..212].copy_from_slice(&0i32.to_ne_bytes());
        frame[220..224].copy_from_slice(&pitch.to_ne_bytes());
        frame[224..228].copy_from_slice(&0.0f32.to_ne_bytes());
        frame[228..232].copy_from_slice(&0.0f32.to_ne_bytes());
        let origin = [0.0f32, 0.0, 0.0];
        let mut origin_bytes = [0u8; 12];
        for (i, v) in origin.iter().enumerate() {
            origin_bytes[i * 4..(i + 1) * 4].copy_from_slice(&v.to_ne_bytes());
        }
        let mut e_nums = vec![0u8; 8 + 4 * 4];
        e_nums[0..4].copy_from_slice(&1i32.to_ne_bytes());
        e_nums[4..8].copy_from_slice(&1i32.to_ne_bytes());

        state::with_state(|s| {
            s.cvars[state::CVAR_MAX_FOV].integer = max_fov;
            s.cvars[state::CVAR_MAX_FOV_ASPECT].value = aspect;
            s.cvars[state::CVAR_MAX_FOV_CAMERA_RANGE].value = camera_range;
            s.cvars[state::CVAR_MAX_FOV_MARGIN].value = margin;
            // The wallhack test drives the pure sampler; `apply` must stay off
            // the engine path in unit tests.
            s.cvars[state::CVAR_ANTI_WALLHACK].integer = 0;
            s.cvars[state::CVAR_ANTI_WALLHACK_MARGIN].value = 0.0;
            s.cvars[state::CVAR_ANTI_WALLHACK_CAMERA_RANGE].value = 0.0;
            s.fov.viewer_client = 0;
            s.located_game_data.g_entities = entities.as_mut_ptr() as usize;
        });
        let g_entities = entities.as_mut_ptr() as usize;
        unsafe {
            apply(
                0,
                origin_bytes.as_mut_ptr() as usize,
                frame.as_mut_ptr() as usize,
                e_nums.as_mut_ptr() as usize,
                g_entities,
                0,
            );
        }
        let read_i32_buf = |offset: usize| {
            i32::from_ne_bytes(
                entities[base + offset..base + offset + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        let hidden = Hidden {
            trick: read_i32_buf(OFFSET_GENTITY_S + OFFSET_GENTITY_S_TRICKED_1),
            scale: read_i32_buf(OFFSET_GENTITY_S + OFFSET_GENTITY_S_IMODEL_SCALE),
            e_flags: read_i32_buf(OFFSET_GENTITY_S + OFFSET_GENTITY_S_EFLAGS),
            saber_holstered: entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_SABER_HOLSTERED],
            loop_sound: entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_LOOP_SOUND],
            entities,
            _gclient: gclient,
            _viewer_gclient: viewer_gclient,
        };
        restore();
        state::with_state(|s| {
            s.located_game_data.g_entities = 0;
            s.fov.viewer_client = -1;
            s.cvars[state::CVAR_MAX_FOV].integer = 0;
            s.cvars[state::CVAR_MAX_FOV_ASPECT].value = 0.0;
            s.cvars[state::CVAR_MAX_FOV_CAMERA_RANGE].value = 0.0;
            s.cvars[state::CVAR_MAX_FOV_MARGIN].value = 0.0;
            s.cvars[state::CVAR_ANTI_WALLHACK].integer = 0;
            s.cvars[state::CVAR_ANTI_WALLHACK_MARGIN].value = 0.0;
            s.cvars[state::CVAR_ANTI_WALLHACK_CAMERA_RANGE].value = 0.0;
        });
        hidden
    }

    #[test]
    fn an_outside_player_is_mind_tricked_scaled_and_restored() {
        // Behind the viewer: hidden.
        let hidden = run_hide(
            [-100.0, 0.0, 0.0],
            100,
            16.0 / 9.0,
            0.0,
            0.0,
            0.0,
            false,
            false,
        );
        assert_eq!(hidden.trick, 1, "viewer 0's mind-trick bit must be set");
        assert_eq!(hidden.scale, 1, "the hidden player is scaled to 1%");
        assert_ne!(
            hidden.e_flags & EF_NODRAW,
            0,
            "NODRAW makes the hide instant"
        );
        // No saber in hand: no saber fields touched.
        assert_eq!(hidden.saber_holstered, 0);
        assert_eq!(hidden.loop_sound, 0);
        // Restored: every changed field is back to zero.
        let base = SIZEOF_GENTITY;
        for offset in [OFFSET_GENTITY_S_IMODEL_SCALE, OFFSET_GENTITY_S_TRICKED_1] {
            assert_eq!(
                &hidden.entities
                    [base + OFFSET_GENTITY_S + offset..base + OFFSET_GENTITY_S + offset + 4],
                &[0, 0, 0, 0],
                "offset {offset} must be restored"
            );
        }
    }

    #[test]
    fn an_inside_player_is_untouched() {
        let hidden = run_hide(
            [100.0, 0.0, 0.0],
            100,
            16.0 / 9.0,
            0.0,
            0.0,
            0.0,
            false,
            false,
        );
        assert_eq!(hidden.trick, 0);
        assert_eq!(hidden.scale, 0);
        assert_eq!(hidden.e_flags & EF_NODRAW, 0);
        assert_eq!(hidden.saber_holstered, 0);
        assert_eq!(hidden.loop_sound, 0);
    }

    #[test]
    fn a_body_box_edge_on_screen_keeps_the_player_visible() {
        let a = 53.0f32.to_radians();
        let place = [a.cos() * 100.0, a.sin() * 100.0, 0.0];
        // The feet origin alone is outside the 50° horizontal half-angle...
        let origin_only = run_hide(place, 100, 4.0 / 3.0, 0.0, 0.0, 0.0, false, false);
        assert_eq!(origin_only.trick, 1);
        // ...but the front-inner corner of the body box (still on screen) keeps
        // the whole player visible.
        let bounded = run_hide_with(
            place,
            100,
            4.0 / 3.0,
            0.0,
            0.0,
            0.0,
            false,
            false,
            |entities, _, base| {
                write_vec3(
                    &mut entities[base + OFFSET_GENTITY_R_MINS..base + OFFSET_GENTITY_R_MINS + 12],
                    [-15.0, -15.0, -24.0],
                );
                write_vec3(
                    &mut entities[base + OFFSET_GENTITY_R_MAXS..base + OFFSET_GENTITY_R_MAXS + 12],
                    [15.0, 15.0, 40.0],
                );
            },
        );
        assert_eq!(bounded.trick, 0);
    }

    #[test]
    fn a_drawn_saber_tip_on_screen_keeps_the_player_visible() {
        // Body origin at 60° from the view axis: hidden without a blade.
        let a = 60.0f32.to_radians();
        let place = [a.cos() * 100.0, a.sin() * 100.0, 0.0];
        let no_blade = run_hide(place, 100, 4.0 / 3.0, 0.0, 0.0, 0.0, true, false);
        assert_eq!(no_blade.trick, 1);
        // A drawn blade running from the hand back across the view puts its
        // middle/tip inside the 50° half-angle, so the player is visible.
        let hand = [place[0], place[1], 26.0];
        let tip = [75.0f32, 60.0, 26.0];
        let dx = tip[0] - hand[0];
        let dy = tip[1] - hand[1];
        let length = (dx * dx + dy * dy).sqrt();
        let dir = [dx / length, dy / length, 0.0];
        let with_blade = run_hide_with(
            place,
            100,
            4.0 / 3.0,
            0.0,
            0.0,
            0.0,
            true,
            false,
            |_, gclient, _| set_blade(gclient, 0, 0, true, length, hand, dir),
        );
        assert_eq!(with_blade.trick, 0);
        assert_eq!(with_blade.saber_holstered, 0, "the blade is not hidden");
    }

    #[test]
    fn a_corpse_does_not_use_a_stale_saber_blade() {
        let a = 60.0f32.to_radians();
        let place = [a.cos() * 100.0, a.sin() * 100.0, 0.0];
        let hand = [place[0], place[1], 26.0];
        let tip = [75.0f32, 60.0, 26.0];
        let dx = tip[0] - hand[0];
        let dy = tip[1] - hand[1];
        let length = (dx * dx + dy * dy).sqrt();
        let dir = [dx / length, dy / length, 0.0];
        let hidden = run_hide_with(
            place,
            100,
            4.0 / 3.0,
            0.0,
            0.0,
            0.0,
            true,
            false,
            |entities, gclient, base| {
                entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_EFLAGS
                    ..base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_EFLAGS + 4]
                    .copy_from_slice(&EF_DEAD.to_ne_bytes());
                set_blade(gclient, 0, 0, true, length, hand, dir);
            },
        );
        assert_eq!(hidden.trick, 1, "a corpse keeps the body-only test");
    }

    #[test]
    fn a_margin_keeps_a_near_edge_player_visible() {
        let a = 53.0f32.to_radians();
        let place = [a.cos() * 100.0, a.sin() * 100.0, 0.0];
        let exact = run_hide(place, 100, 4.0 / 3.0, 0.0, 0.0, 0.0, false, false);
        assert_eq!(exact.trick, 1);
        // The point is ~5 world units outside the side plane; an 8-unit
        // margin covers it.
        let expanded = run_hide(place, 100, 4.0 / 3.0, 0.0, 8.0, 0.0, false, false);
        assert_eq!(expanded.trick, 0);
    }

    #[test]
    fn a_hidden_drawn_saber_loses_its_blade_but_keeps_its_hum() {
        let hidden = run_hide(
            [-100.0, 0.0, 0.0],
            100,
            16.0 / 9.0,
            0.0,
            0.0,
            0.0,
            true,
            false,
        );
        assert_eq!(hidden.trick, 1);
        assert_eq!(hidden.scale, 1);
        assert_eq!(hidden.e_flags & EF_NODRAW, EF_NODRAW);
        assert_eq!(hidden.saber_holstered, 2, "blade hidden for this viewer");
        assert_eq!(hidden.loop_sound, 42, "hilt hum replaces the native loop");
        // And restored for the next viewer.
        let base = SIZEOF_GENTITY;
        assert_eq!(
            hidden.entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_SABER_HOLSTERED],
            0
        );
        assert_eq!(
            hidden.entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_LOOP_SOUND],
            0
        );
    }

    #[test]
    fn a_thrown_saber_is_left_alone() {
        let _guard = state::TEST_STATE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut entities = vec![0u8; MAX_CLIENTS * SIZEOF_GENTITY];
        let mut gclient = vec![0u8; crate::sdk::SIZEOF_GCLIENT];
        let base = SIZEOF_GENTITY;
        entities[base + OFFSET_GENTITY_CLIENT
            ..base + OFFSET_GENTITY_CLIENT + core::mem::size_of::<usize>()]
            .copy_from_slice(&(gclient.as_mut_ptr() as usize).to_ne_bytes());
        let client = gclient.as_mut_ptr() as usize;
        // SAFETY: the fake gclient is large enough for the pinned field.
        unsafe {
            core::ptr::write_unaligned(
                (client + OFFSET_GCLIENT_SABER + OFFSET_SABER_SOUND_LOOP) as *mut i32,
                42,
            )
        };
        entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_WEAPON] = WP_SABER;
        entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_SABER_IN_FLIGHT] = 1;
        // Behind the viewer, so the cone would otherwise hide this player.
        for (i, v) in [-100.0f32, 0.0, 0.0].iter().enumerate() {
            entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_POS_TRBASE + i * 4
                ..base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_POS_TRBASE + (i + 1) * 4]
                .copy_from_slice(&v.to_ne_bytes());
        }
        let mut frame = vec![0u8; 512];
        let mut origin_bytes = [0u8; 12];
        let mut e_nums = vec![0u8; 8 + 4 * 4];
        e_nums[0..4].copy_from_slice(&1i32.to_ne_bytes());
        e_nums[4..8].copy_from_slice(&1i32.to_ne_bytes());
        state::with_state(|s| {
            s.cvars[state::CVAR_MAX_FOV].integer = 100;
            s.cvars[state::CVAR_MAX_FOV_ASPECT].value = 16.0 / 9.0;
            s.cvars[state::CVAR_MAX_FOV_CAMERA_RANGE].value = 0.0;
            s.cvars[state::CVAR_ANTI_WALLHACK].integer = 0;
            s.cvars[state::CVAR_ANTI_WALLHACK_MARGIN].value = 0.0;
            s.cvars[state::CVAR_ANTI_WALLHACK_CAMERA_RANGE].value = 0.0;
            s.located_game_data.g_entities = entities.as_mut_ptr() as usize;
        });
        unsafe {
            apply(
                0,
                origin_bytes.as_mut_ptr() as usize,
                frame.as_mut_ptr() as usize,
                e_nums.as_mut_ptr() as usize,
                entities.as_mut_ptr() as usize,
                0,
            );
        }
        assert_eq!(
            entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_SABER_HOLSTERED],
            0,
            "a thrown saber keeps its blade"
        );
        assert_eq!(
            entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_LOOP_SOUND],
            0
        );
        // The player itself is still hidden (body and 1% scale): only the
        // floating saber is left alone.
        assert_ne!(
            i32::from_ne_bytes(
                entities[base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_TRICKED_1
                    ..base + OFFSET_GENTITY_S + OFFSET_GENTITY_S_TRICKED_1 + 4]
                    .try_into()
                    .unwrap()
            ),
            0
        );
        restore();
        state::with_state(|s| {
            s.located_game_data.g_entities = 0;
            s.cvars[state::CVAR_MAX_FOV].integer = 0;
            s.cvars[state::CVAR_MAX_FOV_ASPECT].value = 0.0;
            s.cvars[state::CVAR_MAX_FOV_CAMERA_RANGE].value = 0.0;
            s.cvars[state::CVAR_ANTI_WALLHACK].integer = 0;
            s.cvars[state::CVAR_ANTI_WALLHACK_MARGIN].value = 0.0;
            s.cvars[state::CVAR_ANTI_WALLHACK_CAMERA_RANGE].value = 0.0;
        });
    }

    #[test]
    fn a_spectator_viewer_is_not_affected() {
        // Same hidden target behind the viewer, but the actual client (slot 0)
        // is on the spectator team: the cap must not apply.
        let spectator = run_hide(
            [-100.0, 0.0, 0.0],
            100,
            16.0 / 9.0,
            0.0,
            0.0,
            0.0,
            false,
            true,
        );
        assert_eq!(spectator.trick, 0);
        assert_eq!(spectator.scale, 0);
        assert_eq!(spectator.e_flags & EF_NODRAW, 0);
        // A playing viewer in the same setup is capped.
        let playing = run_hide(
            [-100.0, 0.0, 0.0],
            100,
            16.0 / 9.0,
            0.0,
            0.0,
            0.0,
            false,
            false,
        );
        assert_eq!(playing.trick, 1);
    }

    #[test]
    fn a_steep_view_pauses_the_cap() {
        // Behind the viewer at a normal pitch: hidden.
        let level = run_hide(
            [-100.0, 0.0, 0.0],
            100,
            16.0 / 9.0,
            0.0,
            0.0,
            0.0,
            false,
            false,
        );
        assert_eq!(level.trick, 1);
        // The same target while looking steeply down/up stays visible: the
        // client's third-person camera pitch diverges from `ps.viewangles`
        // there, so the server frustum is not trustworthy.
        let steep = run_hide(
            [-100.0, 0.0, 0.0],
            100,
            16.0 / 9.0,
            0.0,
            0.0,
            80.0,
            false,
            false,
        );
        assert_eq!(steep.trick, 0);
        assert_eq!(steep.scale, 0);
    }

    #[test]
    fn the_wallhack_margin_expands_the_body_box_on_every_axis() {
        let mins = [-15.0, -15.0, -24.0];
        let maxs = [15.0, 15.0, 40.0];
        let exact = body_points([0.0, 0.0, 0.0], mins, maxs, 0.0);
        // Centre first, then the eight corners.
        assert_eq!(exact[0], [0.0, 0.0, 8.0]);
        assert_eq!(exact[1], [mins[0], mins[1], mins[2]]);
        assert_eq!(exact[8], [maxs[0], maxs[1], maxs[2]]);
        let expanded = body_points([0.0, 0.0, 0.0], mins, maxs, 8.0);
        assert_eq!(
            expanded[0], exact[0],
            "the centre does not move with a symmetric expansion"
        );
        assert_eq!(expanded[1], [mins[0] - 8.0, mins[1] - 8.0, mins[2] - 8.0]);
        assert_eq!(expanded[8], [maxs[0] + 8.0, maxs[1] + 8.0, maxs[2] + 8.0]);
        // A negative/NaN margin never shrinks the box.
        assert_eq!(body_points([0.0; 3], mins, maxs, -5.0), exact);
        assert_eq!(body_points([0.0; 3], mins, maxs, f32::NAN), exact);
    }

    #[test]
    fn the_wallhack_keeps_a_player_with_any_body_point_in_line_of_sight() {
        let mut entities = vec![0u8; MAX_CLIENTS * SIZEOF_GENTITY];
        let base = SIZEOF_GENTITY;
        write_vec3(
            &mut entities[base + OFFSET_GENTITY_R_MINS..base + OFFSET_GENTITY_R_MINS + 12],
            [-15.0, -15.0, -24.0],
        );
        write_vec3(
            &mut entities[base + OFFSET_GENTITY_R_MAXS..base + OFFSET_GENTITY_R_MAXS + 12],
            [15.0, 15.0, 40.0],
        );
        let ent = entities.as_mut_ptr() as usize + base;
        // The target's body spans x 45..75; a clear ray is one whose endpoint
        // is before x = 40.
        let clear_before_40 = |point: [f32; 3]| point[0] < 40.0;
        // SAFETY: the fake entity carries the pinned body-box fields and the
        // closure supplies the trace verdict.
        let exact =
            unsafe { player_in_line_of_sight(ent, 0, [60.0, 0.0, 0.0], 0.0, clear_before_40) };
        assert!(!exact, "every body point is behind the wall");
        // An 8-unit margin moves the near corner to x = 37, in line of sight.
        // SAFETY: as above.
        let expanded =
            unsafe { player_in_line_of_sight(ent, 0, [60.0, 0.0, 0.0], 8.0, clear_before_40) };
        assert!(expanded, "the margin lets the near corner clear the wall");
    }

    #[test]
    fn the_wallhack_camera_sits_behind_the_eye() {
        // Looking down +X, a 100-unit setback lands 100 units behind the eye.
        assert_eq!(
            camera_origin([100.0, 0.0, 0.0], [0.0, 0.0, 0.0], 100.0),
            [0.0, 0.0, 0.0]
        );
        // Looking straight down, the third-person camera is above the eye.
        let cam = camera_origin([0.0, 0.0, 0.0], [90.0, 0.0, 0.0], 100.0);
        assert!((cam[2] - 100.0).abs() < 1e-4);
        // First person and a negative/NaN range both stay at the eye.
        assert_eq!(
            camera_origin([1.0, 2.0, 3.0], [0.0, 0.0, 0.0], 0.0),
            [1.0, 2.0, 3.0]
        );
        assert_eq!(
            camera_origin([1.0, 2.0, 3.0], [0.0, 0.0, 0.0], f32::NAN),
            [1.0, 2.0, 3.0]
        );
    }

    #[test]
    fn the_wallhack_camera_pulls_in_to_the_client_camera_collision_hit() {
        let eye = [100.0, 0.0, 0.0];
        let desired = [0.0, 0.0, 0.0];
        // Nothing behind the player: keep the full setback.
        assert_eq!(
            clamp_camera(eye, desired, false, false, [50.0, 0.0, 0.0]),
            desired
        );
        // A wall behind: the client camera stops at the trace hit, not at the
        // ideal location behind the wall.
        assert_eq!(
            clamp_camera(eye, desired, false, true, [40.0, 0.0, 0.0]),
            [40.0, 0.0, 0.0]
        );
        // The eye itself is in solid: no trustworthy camera estimate.
        assert_eq!(
            clamp_camera(eye, desired, true, true, [40.0, 0.0, 0.0]),
            eye
        );
    }
}
