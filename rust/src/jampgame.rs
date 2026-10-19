//! Calls into the original game module's own helper functions
//! (`jampgame.functions.*`, `Proxy_Engine_Wrappers.hpp:325-370`).
//!
//! Game offsets are file offsets (base 0) rebased by the module load base at
//! call time — the same `proxy.jampgameAddress + offset` scheme as
//! `Proxy_Engine_Wrappers.cpp:124-136`.
//!
//! Q_stricmp binding: the original proxy binds both `Q_stricmp` and
//! `Q_stricmpn` to `0x1a5184` (which is `Q_stricmpn`); the real 2-arg
//! `Q_stricmp` lives at `0x1a5304`. Per U-007 the Rust port binds the correct
//! address (behavior-preserving, removes the n=0/n=1 fragility).

use core::ffi::{c_char, c_int};

use crate::original;

// ---------------------------------------------------------------------------
// Game function offsets (`Proxy_Engine_Wrappers.hpp:325-338`; nm -D verified)
// ---------------------------------------------------------------------------

pub const FN_Q_STRICMPN: usize = 0x001a_5184;
/// Real 2-arg `Q_stricmp` (U-007): the proxy misbound it to `0x1a5184`.
pub const FN_Q_STRICMP: usize = 0x001a_5304;
const FN_COM_SPRINTF: usize = 0x001a_5524;
pub const FN_INFO_VALUE_FOR_KEY: usize = 0x001a_5604;
pub const FN_INFO_SET_VALUE_FOR_KEY: usize = 0x001a_5b54;
pub const FN_CONCAT_ARGS: usize = 0x0012_9c74;

// ---------------------------------------------------------------------------
// Game hook targets (`Proxy_Engine_Wrappers.hpp:340-347`; nm -D verified)
// ---------------------------------------------------------------------------

pub const FN_G_DAMAGE: usize = 0x0013_8554;
pub const FN_PLAYER_DIE: usize = 0x0013_3b44;
pub const FN_BEGIN_INTERMISSION: usize = 0x0008_7d14;
pub const FN_G_ADD_EVENT: usize = 0x0016_e564;
pub const FN_CLIENT_THINK_REAL: usize = 0x0011_c4c4;
/// `void SetTeam(gentity_t*, char*)` (nm-verified `SetTeam__FP9gentity_sPc`),
/// the single choke point every team change funnels through. Hooked by
/// `proxy_sv_lockTeams` to enforce the round-start caps.
pub const FN_SET_TEAM: usize = 0x0012_ada4;
/// `void AddScore(gentity_t*, vec3_t, int)` (nm `AddScore__FP9gentity_sPfi`,
/// file offset `0x1313a4`; prologue `55 8b ec 83 ec 08`, 6 detour-safe bytes).
/// `vec3_t` decays to `float*`, so the wrapper takes an origin pointer.
pub const FN_ADD_SCORE: usize = 0x0013_13a4;
/// `void LogExit(const char*)` (nm `LogExit__FPCc`, file offset `0x881d4`;
/// prologue `55 8b ec 83 ec 30`, 6 detour-safe bytes). Hooked by the
/// parallel-TFFA feature to swallow pristine fraglimit exits (`"Kill limit
/// hit."` only — timelimit/capture/duel exits pass through): pristine
/// `CheckExitRules` runs inside `CalculateRanks` inside `AddScore`, i.e.
/// before the `AddScore` wrapper can neutralise the bump, so neutralising
/// alone can never win the race.
pub const FN_LOG_EXIT: usize = 0x0008_81d4;
/// `void G_Sound(gentity_t*, int, int)` (nm `G_Sound__FP9gentity_siT2`, file
/// offset `0x16e824`; prologue `55 8b ec 83 ec 20`, 6 detour-safe bytes).
/// Parallel-TFFA sound isolation: records which match's action owns the temp
/// entity the pristine body is about to construct (the
/// `G_SoundTempEntity` handshake).
pub const FN_G_SOUND: usize = 0x0016_e824;
/// `gentity_t *G_SoundTempEntity(vec3_t, int, int)` (nm
/// `G_SoundTempEntity__FPfiT2`, file offset `0x16e224`; prologue `55 8b ec
/// 83 ec 38`, 6 detour-safe bytes). The sole creator of `G_Sound`'s temp
/// entity — one call site at `0x16e84e`, inside `G_Sound` (objdump-verified
/// over the shipped binary) — so it is where the handshake lands.
pub const FN_G_SOUND_TEMP_ENTITY: usize = 0x0016_e224;
/// `gentity_t *G_TempEntity(vec3_t, int)` (nm `G_TempEntity__FPfi`, file
/// offset `0x16e0d4`; prologue `55 8b ec 83 ec 38`, 6 detour-safe bytes).
/// The only other temp-entity creator (objdump: all 7 call sites are the
/// `g_utils.c` event helpers); stamps world attribution so a reused slot
/// never keeps a stale sound attribution.
pub const FN_G_TEMP_ENTITY: usize = 0x0016_e0d4;

// ---------------------------------------------------------------------------
// Parallel-TFFA per-instance world-entity support (`nm -D` verified 2026-10-18,
// original/jalinuxded_1.011/jampgamei386.so; all inside .text 0x85564..0x1a66c4).
// Used by the clone pass (S3), the touch/link gates (S6/S7), the
// runtime-spawn attribution (S8) and the world-query gates (S9). Pinned ahead of
// the implementation, so allowed dead until the callers land.
// ---------------------------------------------------------------------------

/// `gentity_t *G_Spawn(void)` (nm `G_Spawn__Fv`; prologue `55 8b ec 83 ec 30`,
/// 6 detour-safe bytes) — allocates a free gentity slot (reusing a freed one, or
/// opening a new slot and bumping `level.num_entities`) and runs
/// `G_InitGentity` on it. The per-instance clone pass uses it rather than a
/// hand-rolled allocator so slot accounting matches the pristine game, and it is
/// detoured (index 15) so an entity the *game* spawns while a world entity runs
/// its code (a door's `Think_SpawnNewDoorTrigger` → `trigger_door`, an asteroid
/// field's `func_rotating` asteroids) is remembered until its callbacks are set,
/// then tagged to that entity's instance (S8).
pub const FN_G_SPAWN: usize = 0x0016_db44;
/// `void G_InitGentity(gentity_t*)` (nm `G_InitGentity__FP9gentity_s`).
#[allow(dead_code)]
pub const FN_G_INIT_GENTITY: usize = 0x0016_d984;
/// `void G_FreeEntity(gentity_t*)` (nm `G_FreeEntity__FP9gentity_s`). Detoured
/// (index 11) so a clone that leaves the world loses its instance tag with the
/// slot (a recycled slot must not inherit a stale tag).
pub const FN_G_FREE_ENTITY: usize = 0x0016_de44;
/// `void G_UseTargets(gentity_t*, gentity_t*)` (nm `G_UseTargets__FP9gentity_sT1`)
/// — resolves `ent->target` by `targetname` across all of `g_entities`, so
/// duplication alone would still cross-fire between instances. Not hooked:
/// objdump shows it is a tail that reads `ent->target` (`0x448(%edx)`) and
/// calls `G_UseTargets2` at `0x170545`, so the one hook on `G_UseTargets2`
/// covers it and every direct chain caller.
#[allow(dead_code)]
pub const FN_G_USE_TARGETS: usize = 0x0017_0524;
/// `void G_UseTargets2(gentity_t*, gentity_t*, const char*)` (nm
/// `G_UseTargets2__FP9gentity_sT1PCc`, file offset `0x16d604`; prologue
/// `55 8b ec 83 ec 20`, 6 detour-safe bytes). The funnel every target chain
/// goes through: hooked (index 13) to scope the `targetname` search it drives
/// to the firing entity's own instance.
pub const FN_G_USE_TARGETS2: usize = 0x0016_d604;
/// `gentity_t *G_Find(gentity_t *from, int fieldofs, const char *match)` (nm
/// `G_Find__FP9gentity_siPCc`, file offset `0x16cec4`; prologue `55 8b ec 83 ec
/// 18`, 6 detour-safe bytes). The search `G_UseTargets2` resolves a chain with:
/// objdump shows its two calls inside it pass `fieldofs` `0x31c`
/// (`FOFS(targetname)`, pinned `sdk::OFFSET_GENTITY_TARGETNAME`) at `0x16d68e`
/// (first match) and `0x16d6de` (resume). Hooked (index 14) so a `targetname`
/// lookup skips entities tagged to another instance while a link window is open.
pub const FN_G_FIND: usize = 0x0016_cec4;
/// `void G_RunThink(gentity_t *ent)` (nm `G_RunThink__FP9gentity_s`, file offset
/// `0x8a1b4`; prologue `55 8b ec 83 ec 10`, 6 detour-safe bytes). The single
/// dispatcher of `ent->think` (`g_main.c:3473-3487`: it reads `nextthink` at
/// `0x4a4` and `think` at `0x4a8`, both pinned `gentity_t` offsets, and calls
/// the function pointer): `G_RunFrame` reaches it directly for ordinary
/// entities, through `G_RunMover` for `ET_MOVER` and through `G_RunItem`/
/// `G_RunMissile` for those. Hooked (index 16) to record *which* entity's code
/// is running, so an interactive entity a think spawns inherits that entity's
/// instance instead of staying shared (S8).
pub const FN_G_RUN_THINK: usize = 0x0008_a1b4;
/// `void G_RunMissile(gentity_t *ent)` (nm `G_RunMissile__FP9gentity_s`, file
/// offset `0x14d274`; prologue `55 8b ec 81 ec d8 00 00 00`, 9 detour-safe
/// bytes). `G_RunFrame` routes every `ET_MISSILE` here (`g_main.c:3773`): it
/// advances the trajectory, resolves the impact (`G_MissileImpact` →
/// `G_RadiusDamage`, `G_RadiusList`) and ends with `G_RunThink` for the missile's
/// own think. Hooked (index 17) so the impact runs in the firing entity's acting
/// scope: a missile carries no instance of its own, but its owner does
/// (`r.ownerNum` → the firing player), so its radius damage and box queries only
/// reach that instance's entities (S9).
pub const FN_G_RUN_MISSILE: usize = 0x0014_d274;
/// `void G_TouchTriggers(gentity_t*)` (nm `G_TouchTriggers__FP9gentity_s`) —
/// the per-think touch dispatch (`g_active.c:720`). Hooked to clear
/// `CONTENTS_TRIGGER` on foreign-instance triggers for the duration.
#[allow(dead_code)]
pub const FN_G_TOUCH_TRIGGERS: usize = 0x0011_9bf4;
/// `void G_MoverTeam(gentity_t*)` (nm `G_MoverTeam__FP9gentity_s`).
#[allow(dead_code)]
pub const FN_G_MOVER_TEAM: usize = 0x0014_e6f4;
/// `void G_RunMover(gentity_t*)` (nm `G_RunMover__FP9gentity_s`).
#[allow(dead_code)]
pub const FN_G_RUN_MOVER: usize = 0x0014_e974;
/// `void G_SpawnEntitiesFromString(qboolean)` (nm
/// `G_SpawnEntitiesFromString__F8qboolean`) — map spawn, runs inside
/// `G_InitGame`; the clone pass runs after it returns.
#[allow(dead_code)]
pub const FN_G_SPAWN_ENTITIES_FROM_STRING: usize = 0x0015_d094;

/// Re-type a game-module function at `base + offset` as `$fn_ty`.
///
/// # Safety
///
/// `$fn_ty` must describe the actual cdecl signature of the function at
/// `base + offset`; the original module must be loaded.
macro_rules! game_fn {
    ($fn_ty:ty, $offset:expr) => {{
        let base = original::base();
        assert!(base != 0, "original module not loaded");
        // SAFETY: usize and fn pointers are both pointer-sized; the address is
        // the module base + a validated file offset of a function with the
        // given signature.
        unsafe { core::mem::transmute::<usize, $fn_ty>(base + $offset) }
    }};
}

// ---------------------------------------------------------------------------
// Calls
// ---------------------------------------------------------------------------

/// `int Q_stricmpn(const char*, const char*, int)` at game base+0x1a5184.
///
/// # Safety
///
/// `a`/`b` must be valid NUL-terminated strings readable through `n` bytes;
/// the original module must be loaded.
pub unsafe fn q_stricmpn(a: *const c_char, b: *const c_char, n: c_int) -> c_int {
    // SAFETY: pointer from base+offset of a loaded module.
    let f: unsafe extern "C" fn(*const c_char, *const c_char, c_int) -> c_int = game_fn!(
        unsafe extern "C" fn(*const c_char, *const c_char, c_int) -> c_int,
        FN_Q_STRICMPN
    );
    unsafe { f(a, b, n) }
}

/// `int Q_stricmp(const char*, const char*)` at game base+0x1a5304 (U-007).
///
/// # Safety
///
/// `a`/`b` must be valid NUL-terminated strings; the original module must be
/// loaded.
pub unsafe fn q_stricmp(a: *const c_char, b: *const c_char) -> c_int {
    // SAFETY: pointer from base+offset of a loaded module.
    let f: unsafe extern "C" fn(*const c_char, *const c_char) -> c_int = game_fn!(
        unsafe extern "C" fn(*const c_char, *const c_char) -> c_int,
        FN_Q_STRICMP
    );
    unsafe { f(a, b) }
}

/// `void Com_sprintf(char*, int, const char*, ...)` at game base+0x1a5524 —
/// called here with exactly one variadic argument, so the cdecl ABI is a plain
/// 4-arg function call.
///
/// # Safety
///
/// `dst` must be a writable buffer of `size` bytes; `fmt` a valid format
/// string whose single `%` argument is `arg1` (a `const char*`).
pub unsafe fn com_sprintf(dst: *mut c_char, size: c_int, fmt: *const c_char, arg1: *const c_char) {
    // SAFETY: pointer from base+offset of a loaded module.
    let f: unsafe extern "C" fn(*mut c_char, c_int, *const c_char, *const c_char) = game_fn!(
        unsafe extern "C" fn(*mut c_char, c_int, *const c_char, *const c_char),
        FN_COM_SPRINTF
    );
    unsafe { f(dst, size, fmt, arg1) };
}

/// `char* Info_ValueForKey(const char*, const char*)` at game base+0x1a5604.
///
/// # Safety
///
/// `info`/`key` must be valid NUL-terminated strings; the returned pointer is
/// valid until the game's static info buffer is overwritten.
pub unsafe fn info_value_for_key(info: *const c_char, key: *const c_char) -> *const c_char {
    // SAFETY: pointer from base+offset of a loaded module.
    let f: unsafe extern "C" fn(*const c_char, *const c_char) -> *const c_char = game_fn!(
        unsafe extern "C" fn(*const c_char, *const c_char) -> *const c_char,
        FN_INFO_VALUE_FOR_KEY
    );
    unsafe { f(info, key) }
}

/// `void Info_SetValueForKey(char*, const char*, const char*)` at game
/// base+0x1a5b54.
///
/// # Safety
///
/// `info` must point to a writable info string buffer; `key`/`value` must be
/// valid NUL-terminated strings.
pub unsafe fn info_set_value_for_key(info: *mut c_char, key: *const c_char, value: *const c_char) {
    // SAFETY: pointer from base+offset of a loaded module.
    let f: unsafe extern "C" fn(*mut c_char, *const c_char, *const c_char) = game_fn!(
        unsafe extern "C" fn(*mut c_char, *const c_char, *const c_char),
        FN_INFO_SET_VALUE_FOR_KEY
    );
    unsafe { f(info, key, value) };
}

/// `char* ConcatArgs(int)` at game base+0x129c74. Returns a pointer to the
/// concatenated remaining command args (game static buffer).
///
/// # Safety
///
/// `start` is the first arg index to include; the returned pointer is valid
/// until the next command is processed.
pub unsafe fn concat_args(start: c_int) -> *const c_char {
    // SAFETY: pointer from base+offset of a loaded module.
    let f: unsafe extern "C" fn(c_int) -> *const c_char =
        game_fn!(unsafe extern "C" fn(c_int) -> *const c_char, FN_CONCAT_ARGS);
    unsafe { f(start) }
}

/// `gentity_t* G_Spawn(void)` at game base+0x16db44.
///
/// # Safety
///
/// The original module must be loaded and `GAME_INIT` must have run (the game
/// spawns entities through this very function, so it is live from then on).
/// The returned pointer is valid until the entity is freed.
pub unsafe fn g_spawn() -> usize {
    // SAFETY: pointer from base+offset of a loaded module.
    let f: unsafe extern "C" fn() -> usize = game_fn!(unsafe extern "C" fn() -> usize, FN_G_SPAWN);
    unsafe { f() }
}

/// `void G_FreeEntity(gentity_t*)` at game base+0x16de44.
///
/// # Safety
///
/// `ent` must be a live gentity the caller owns (the clone pass passes a slot
/// it just spawned); the game must not be iterating `g_entities` at the time.
pub unsafe fn g_free_entity(ent: usize) {
    // SAFETY: pointer from base+offset of a loaded module.
    let f: unsafe extern "C" fn(usize) = game_fn!(unsafe extern "C" fn(usize), FN_G_FREE_ENTITY);
    unsafe { f(ent) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_stricmp_binding_uses_correct_address() {
        // U-007: the proxy's 0x1a5184 / 0x1a5184 double-binding was a bug; the
        // correct 2-arg Q_stricmp is 0x1a5304. Pin the corrected constant here.
        assert_eq!(FN_Q_STRICMP, 0x001a_5304);
        assert_ne!(FN_Q_STRICMPN, FN_Q_STRICMP);
    }

    #[test]
    fn offsets_are_in_game_text_range() {
        // Game `.text` spans file offsets 0x85564..0x1a66c4 (R-002).
        let text_lo = 0x0008_5564usize;
        let text_hi = 0x001a_66c4usize;
        for off in [
            FN_Q_STRICMPN,
            FN_Q_STRICMP,
            FN_COM_SPRINTF,
            FN_INFO_VALUE_FOR_KEY,
            FN_INFO_SET_VALUE_FOR_KEY,
            FN_CONCAT_ARGS,
            FN_G_DAMAGE,
            FN_PLAYER_DIE,
            FN_BEGIN_INTERMISSION,
            FN_G_ADD_EVENT,
            FN_CLIENT_THINK_REAL,
            FN_SET_TEAM,
            FN_ADD_SCORE,
            FN_LOG_EXIT,
            FN_G_SPAWN,
            FN_G_INIT_GENTITY,
            FN_G_FREE_ENTITY,
            FN_G_USE_TARGETS,
            FN_G_USE_TARGETS2,
            FN_G_FIND,
            FN_G_RUN_THINK,
            FN_G_RUN_MISSILE,
            FN_G_TOUCH_TRIGGERS,
            FN_G_MOVER_TEAM,
            FN_G_RUN_MOVER,
            FN_G_SPAWN_ENTITIES_FROM_STRING,
        ] {
            assert!(off >= text_lo && off < text_hi, "0x{off:x} out of .text");
        }
    }
}
