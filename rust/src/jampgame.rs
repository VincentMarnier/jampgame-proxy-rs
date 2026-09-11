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
        ] {
            assert!(off >= text_lo && off < text_hi, "0x{off:x} out of .text");
        }
    }
}
