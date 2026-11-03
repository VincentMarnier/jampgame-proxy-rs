//! FOV cap (`proxy_sv_maxFov`): hide players outside a viewer's allowed
//! horizontal FOV while keeping their sounds.
//!
//! A stock JKA client never tells the server its `cg_fov` (the cvar is
//! registered by cgame only, `cg_main.c:887`), so the server cannot know a
//! client's real FOV. Instead, the proxy caps what any client can *receive*:
//! when `proxy_sv_maxFov` is `N`, a player outside the rendered view frustum
//! corresponding to `cg_fov N` is hidden for that viewer. Raising `cg_fov` above `N` therefore shows no extra players.
//! Modern clients widen `cg_fov` for the screen aspect (`cg_fovAspectAdjust`);
//! `proxy_sv_maxFovAspect` (default 16:9) is the reference used for that
//! conversion.
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
//! after `SV_SendClientSnapshot` returns (D-022).

use core::ffi::c_int;

use crate::hooks::engine_sv::{read_i32, read_usize, write_i32};
use crate::sdk::{
    EF_DEAD, EF_NODRAW, MAX_CLIENTS, MAX_SNAPSHOT_ENTITIES, OFFSET_GCLIENT_SABER,
    OFFSET_GENTITY_CLIENT, OFFSET_GENTITY_S, OFFSET_GENTITY_S_EFLAGS,
    OFFSET_GENTITY_S_IMODEL_SCALE, OFFSET_GENTITY_S_LOOP_SOUND, OFFSET_GENTITY_S_POS_TRBASE,
    OFFSET_GENTITY_S_SABER_HOLSTERED, OFFSET_GENTITY_S_SABER_IN_FLIGHT, OFFSET_GENTITY_S_TRICKED_1,
    OFFSET_GENTITY_S_TRICKED_2, OFFSET_GENTITY_S_TRICKED_3, OFFSET_GENTITY_S_TRICKED_4,
    OFFSET_GENTITY_S_WEAPON, OFFSET_SABER_SOUND_LOOP, OFFSET_SNAPSHOT_ENUMS_COUNT,
    OFFSET_SNAPSHOT_ENUMS_LIST, OFFSET_SNAPSHOT_FRAME_PS_VIEWANGLES, SIZEOF_GENTITY,
    SIZEOF_SABER_INFO, WP_SABER,
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
}

impl Default for FovState {
    fn default() -> Self {
        FovState {
            saved: [SavedEntity::default(); MAX_CLIENTS],
            count: 0,
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

/// Whether `target` lies inside the camera frustum with horizontal/vertical
/// half-angles `h_half`/`v_half` around `angles`, from a camera placed
/// `camera_range` units behind the eye (`origin`).
///
/// The camera setback matters in third person: the client renders from a point
/// up to `cg_thirdPersonRange` (default 100) behind the player, so a close
/// target near the screen edge has a smaller bearing from the camera than from
/// the eye (`proxy_sv_maxFovCameraRange`; `0` = first-person/eye). A target at
/// (or essentially at) the camera counts as visible so a degenerate direction
/// never hides a player. A target behind the view plane is never on screen.
pub fn visible_in_view_frustum(
    origin: [f32; 3],
    angles: [f32; 3],
    target: [f32; 3],
    h_half: f32,
    v_half: f32,
    camera_range: f32,
) -> bool {
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
    let d = [target[0] - cam[0], target[1] - cam[1], target[2] - cam[2]];
    let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    if len2 <= 1.0 {
        return true;
    }
    let f = d[0] * forward[0] + d[1] * forward[1] + d[2] * forward[2];
    if f <= 0.0 {
        return false;
    }
    let r = d[0] * right[0] + d[1] * right[1] + d[2] * right[2];
    let u = d[0] * up[0] + d[1] * up[1] + d[2] * up[2];
    r.abs() <= f * h_half.tan() && u.abs() <= f * v_half.tan()
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

/// Apply the FOV cap to every player entity in `e_nums` that falls outside the
/// viewer's view frustum. Called after the pristine (and parallel-TFFA)
/// snapshot pass, for the main view only (`portal == false`).
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
    restore();
    if g_entities == 0 || origin == 0 || frame == 0 || e_nums == 0 {
        return;
    }
    if !(0..MAX_CLIENTS as c_int).contains(&viewer) {
        return;
    }
    let (max_fov, aspect, camera_range) = state::with_state(|s| {
        (
            s.cvars[state::CVAR_MAX_FOV].integer,
            s.cvars[state::CVAR_MAX_FOV_ASPECT].value,
            s.cvars[state::CVAR_MAX_FOV_CAMERA_RANGE].value,
        )
    });
    let Some((h_half, v_half)) = view_half_angles(max_fov, aspect) else {
        return;
    };
    // SAFETY: `frame` is a valid clientSnapshot_t; viewangles is a vec3_t.
    let pitch = unsafe { read_f32(frame + OFFSET_SNAPSHOT_FRAME_PS_VIEWANGLES) };
    let yaw = unsafe { read_f32(frame + OFFSET_SNAPSHOT_FRAME_PS_VIEWANGLES + 4) };
    let roll = unsafe { read_f32(frame + OFFSET_SNAPSHOT_FRAME_PS_VIEWANGLES + 8) };
    if !pitch.is_finite() || pitch.abs() > MAX_PITCH {
        return;
    }
    // The stock third-person camera pitches toward its focus point, so its
    // vertical view can sit a few degrees off `ps.viewangles` even below the
    // pause threshold. A small vertical margin keeps on-screen players visible;
    // the horizontal bound (the FOV cheat's axis) stays exact.
    let v_half = (v_half + 10.0f32.to_radians()).min(core::f32::consts::FRAC_PI_2);
    let angles = [pitch, yaw, roll];
    // SAFETY: `origin` is the engine's vec3_t viewpoint argument.
    let viewpoint = unsafe { read_vec3(origin) };
    // SAFETY: `e_nums` is a valid snapshotEntityNumbers_t.
    let count = unsafe { read_i32(e_nums + OFFSET_SNAPSHOT_ENUMS_COUNT) };
    if count <= 0 {
        return;
    }
    let count = (count as usize).min(MAX_SNAPSHOT_ENTITIES);
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
        // SAFETY: `ent` is a live gentity slot; pos.trBase is a vec3_t.
        let target = unsafe { read_vec3(ent + OFFSET_GENTITY_S + OFFSET_GENTITY_S_POS_TRBASE) };
        if visible_in_view_frustum(viewpoint, angles, target, h_half, v_half, camera_range) {
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
    state::with_state(|s| {
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

    #[test]
    fn frustum_keeps_front_and_drops_outside() {
        let o = [0.0, 0.0, 0.0];
        let a = [0.0, 0.0, 0.0];
        let h = 50.0f32.to_radians();
        let v = 40.0f32.to_radians();
        // Straight ahead.
        assert!(visible_in_view_frustum(o, a, [100.0, 0.0, 0.0], h, v, 0.0));
        // Horizontal 40 (inside) and 60 (outside).
        let d = 40.0f32.to_radians();
        assert!(visible_in_view_frustum(
            o,
            a,
            [d.cos() * 100.0, d.sin() * 100.0, 0.0],
            h,
            v,
            0.0
        ));
        let d = 60.0f32.to_radians();
        assert!(!visible_in_view_frustum(
            o,
            a,
            [d.cos() * 100.0, d.sin() * 100.0, 0.0],
            h,
            v,
            0.0
        ));
        // Vertical 30 (inside) and 50 (outside); JKA negative pitch looks up.
        let d = 30.0f32.to_radians();
        assert!(visible_in_view_frustum(
            o,
            a,
            [d.cos() * 100.0, 0.0, -d.sin() * 100.0],
            h,
            v,
            0.0
        ));
        let d = 50.0f32.to_radians();
        assert!(!visible_in_view_frustum(
            o,
            a,
            [d.cos() * 100.0, 0.0, -d.sin() * 100.0],
            h,
            v,
            0.0
        ));
        // A screen corner (horizontal 40, vertical 30) is inside.
        let hd = 40.0f32.to_radians();
        let vd = 30.0f32.to_radians();
        assert!(visible_in_view_frustum(
            o,
            a,
            [
                hd.cos() * vd.cos() * 100.0,
                hd.sin() * vd.cos() * 100.0,
                -vd.sin() * 100.0
            ],
            h,
            v,
            0.0
        ));
        // Behind the view plane.
        assert!(!visible_in_view_frustum(
            o,
            a,
            [-100.0, 0.0, 0.0],
            h,
            v,
            0.0
        ));
        // At the camera never hides.
        assert!(visible_in_view_frustum(o, a, o, h, v, 0.0));
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
        assert!(!visible_in_view_frustum(o, a, target, h, v, 0.0));
        // ...but the client renders it from a camera 100 units behind the
        // player, where its bearing is about 35 degrees: on screen.
        assert!(visible_in_view_frustum(o, a, target, h, v, 100.0));
        // A far target at the same eye bearing is still capped.
        let far = [d.cos() * 2000.0, d.sin() * 2000.0, 0.0];
        assert!(!visible_in_view_frustum(o, a, far, h, v, 100.0));
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
        trick: i32,
        scale: i32,
        e_flags: i32,
        saber_holstered: u8,
        loop_sound: u8,
    }

    /// Synthesise the engine buffers the hook receives and run the real
    /// apply/restore pass over them. `saber` arms the player with a drawn
    /// saber using a fake `gclient_t` whose `saber[0].soundLoop` is 42.
    fn run_hide(
        place: [f32; 3],
        max_fov: i32,
        aspect: f32,
        camera_range: f32,
        pitch: f32,
        saber: bool,
    ) -> Hidden {
        let _guard = state::TEST_STATE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut entities = vec![0u8; MAX_CLIENTS * SIZEOF_GENTITY];
        let mut gclient = vec![0u8; crate::sdk::SIZEOF_GCLIENT];
        let ent = 1usize;
        let base = ent * SIZEOF_GENTITY;
        // Slot 1 holds a live player: point it at the fake gclient.
        entities[base + OFFSET_GENTITY_CLIENT
            ..base + OFFSET_GENTITY_CLIENT + core::mem::size_of::<usize>()]
            .copy_from_slice(&(gclient.as_mut_ptr() as usize).to_ne_bytes());
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
        };
        restore();
        state::with_state(|s| {
            s.located_game_data.g_entities = 0;
            s.cvars[state::CVAR_MAX_FOV].integer = 0;
            s.cvars[state::CVAR_MAX_FOV_ASPECT].value = 0.0;
            s.cvars[state::CVAR_MAX_FOV_CAMERA_RANGE].value = 0.0;
        });
        hidden
    }

    #[test]
    fn an_outside_player_is_mind_tricked_scaled_and_restored() {
        // Behind the viewer: hidden.
        let hidden = run_hide([-100.0, 0.0, 0.0], 100, 16.0 / 9.0, 0.0, 0.0, false);
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
        let hidden = run_hide([100.0, 0.0, 0.0], 100, 16.0 / 9.0, 0.0, 0.0, false);
        assert_eq!(hidden.trick, 0);
        assert_eq!(hidden.scale, 0);
        assert_eq!(hidden.e_flags & EF_NODRAW, 0);
        assert_eq!(hidden.saber_holstered, 0);
        assert_eq!(hidden.loop_sound, 0);
    }

    #[test]
    fn a_hidden_drawn_saber_loses_its_blade_but_keeps_its_hum() {
        let hidden = run_hide([-100.0, 0.0, 0.0], 100, 16.0 / 9.0, 0.0, 0.0, true);
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
        });
    }

    #[test]
    fn a_steep_view_pauses_the_cap() {
        // Behind the viewer at a normal pitch: hidden.
        let level = run_hide([-100.0, 0.0, 0.0], 100, 16.0 / 9.0, 0.0, 0.0, false);
        assert_eq!(level.trick, 1);
        // The same target while looking steeply down/up stays visible: the
        // client's third-person camera pitch diverges from `ps.viewangles`
        // there, so the server frustum is not trustworthy.
        let steep = run_hide([-100.0, 0.0, 0.0], 100, 16.0 / 9.0, 0.0, 80.0, false);
        assert_eq!(steep.trick, 0);
        assert_eq!(steep.scale, 0);
    }
}
