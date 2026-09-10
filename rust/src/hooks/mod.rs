//! The engine/game hook patch layer — `Proxy_Engine_Patch.cpp` equivalent
//! (attach/detach) plus the D-002 wanted/kept features as minimal-alteration
//! wrappers (`docs/inventory.md` is the per-feature map).
//!
//! Techniques used (D-002.1): entry wrappers over pristine bodies (the
//! trampoline detours in `crate::patch`), intra byte/NOP injections (rcon
//! patches, `ipAuthorize` call removal, the RMG else-path force) and call-site
//! retargets (`Com_Printf` vsnprintf shim, the `SV_ClientThink` netStatus
//! feed).

pub mod compat;
pub mod engine_sv;
pub mod game;
pub mod netstatus;

use crate::patch;

// ---------------------------------------------------------------------------
// Trampoline getters (the "Original_*" function pointers)
// ---------------------------------------------------------------------------

macro_rules! engine_trampoline {
    ($name:ident, $index:literal, $ty:ty) => {
        pub fn $name() -> $ty {
            // SAFETY: the trampoline is a re-typed block of machine code with
            // the exact signature of the original function (patch::attach set
            // it up; the wrapper signatures mirror the engine prototypes).
            unsafe { core::mem::transmute::<usize, $ty>(patch::ENGINE_HOOKS[$index].original()) }
        }
    };
}

macro_rules! game_trampoline {
    ($name:ident, $index:literal, $ty:ty) => {
        pub fn $name() -> $ty {
            // SAFETY: see engine_trampoline; game wrappers mirror the game
            // function prototypes (R-013).
            unsafe { core::mem::transmute::<usize, $ty>(patch::GAME_HOOKS[$index].original()) }
        }
    };
}

engine_trampoline!(
    engine_hook_original_svc_status,
    0,
    unsafe extern "C" fn(engine_sv::Netadr)
);
engine_trampoline!(
    engine_hook_original_svc_info,
    1,
    unsafe extern "C" fn(engine_sv::Netadr)
);
engine_trampoline!(
    engine_hook_original_svc_remote_command,
    2,
    unsafe extern "C" fn(engine_sv::Netadr, usize)
);
engine_trampoline!(
    engine_hook_original_sv_userinfo_changed,
    3,
    unsafe extern "C" fn(usize)
);
engine_trampoline!(
    engine_hook_original_sv_begin_download_f,
    4,
    unsafe extern "C" fn(usize)
);
engine_trampoline!(
    engine_hook_original_sv_next_download_f,
    5,
    unsafe extern "C" fn(usize)
);
engine_trampoline!(
    engine_hook_original_sv_stop_download_f,
    6,
    unsafe extern "C" fn(usize)
);
engine_trampoline!(
    engine_hook_original_sv_done_download_f,
    7,
    unsafe extern "C" fn(usize)
);
engine_trampoline!(
    engine_hook_original_sv_update_userinfo_f,
    8,
    unsafe extern "C" fn(usize)
);
engine_trampoline!(
    engine_hook_original_sv_write_download_to_client,
    9,
    unsafe extern "C" fn(usize, usize)
);
engine_trampoline!(
    engine_hook_original_navigator_load,
    11,
    unsafe extern "C" fn(usize, *const core::ffi::c_char, core::ffi::c_int) -> bool
);
engine_trampoline!(
    engine_hook_original_sv_send_client_game_state,
    12,
    unsafe extern "C" fn(usize)
);
engine_trampoline!(
    engine_hook_original_sv_execute_client_message,
    13,
    unsafe extern "C" fn(usize, usize)
);

game_trampoline!(
    game_hook_original_g_damage,
    0,
    unsafe extern "C" fn(
        usize,
        usize,
        usize,
        *const f32,
        *const f32,
        core::ffi::c_int,
        core::ffi::c_int,
        core::ffi::c_int,
    )
);
game_trampoline!(
    game_hook_original_player_die,
    1,
    unsafe extern "C" fn(usize, usize, usize, core::ffi::c_int, core::ffi::c_int)
);
game_trampoline!(
    game_hook_original_begin_intermission,
    2,
    unsafe extern "C" fn()
);
game_trampoline!(
    game_hook_original_g_add_event,
    3,
    unsafe extern "C" fn(usize, core::ffi::c_int, core::ffi::c_int)
);
game_trampoline!(
    game_hook_original_client_think_real,
    4,
    unsafe extern "C" fn(usize)
);

/// Thin alias kept for call-site readability (`original_call(f) == f()`).
#[inline]
pub fn original_call<F>(getter: fn() -> F) -> F {
    getter()
}

// ---------------------------------------------------------------------------
// Attach / detach
// ---------------------------------------------------------------------------

// The `vsnprintf` shim (defined in `csrc/syscall_shim.c`): the retarget
// target of the engine's `call vsprintf` inside `Com_Printf`.
unsafe extern "C" {
    fn jampgame_proxy_vsnprintf_shim(
        buf: *mut core::ffi::c_char,
        fmt: *const core::ffi::c_char,
        ap: *mut core::ffi::c_void,
    ) -> core::ffi::c_int;
}

/// Attach every hook — `Proxy_Engine_Attach_Patches` (D-002 keep set).
///
/// Order mirrors the original: inline/intra patches first, then the engine
/// detours, then the game detours (rebased), then the call-site retargets.
///
/// # Safety
///
/// Must run after the memory layer is initialised and the original module is
/// loaded; every patched address is version-swept (R-010, U-005) or
/// objdump-verified.
pub unsafe fn attach_all() {
    let base = crate::original::base();
    if base == 0 {
        eprintln!("----- proxy-rs: cannot attach hooks without the original module");
        return;
    }

    // Intra injections first (Proxy_Engine_Inline_Patches + D-002 additions).
    // SAFETY: verified whole-instruction tile / byte sites (R-010).
    unsafe {
        // rcon: NOP the timer block ("fix rcon disabler").
        patch::nop_bytes(
            crate::engine::patches::SVC_REMOTE_COMMAND_TIMER_NOP_ADDR,
            crate::engine::patches::SVC_REMOTE_COMMAND_TIMER_NOP_LEN,
        );
        // rcon: Com_BeginRedirect buffer 49136 -> 1008.
        patch::patch_byte(
            crate::engine::patches::SVC_REMOTE_COMMAND_REDIRECT_LEN_ADDR,
            crate::engine::patches::SVC_REMOTE_COMMAND_REDIRECT_LEN_BYTE,
        );
        // anti-DDoS: drop the SV_AuthorizeIpPacket dispatch (NOP the call).
        patch::nop_bytes(patch::CALL_IP_AUTHORIZE, 5);
        // SV_SendClientGameState: force the old-RMG else path
        // (je 0x804d3a1 -> jmp 0x804d3a1; the rel32 is rewritten by
        // jcc_to_jmp since the E9 operand shifts one byte earlier).
        patch::jcc_to_jmp(patch::RMG_BRANCH);
        // Com_Printf hardening: vsprintf -> vsnprintf shim (call-site).
        patch::retarget_call_tracked(patch::CALL_VSPRINTF, jampgame_proxy_vsnprintf_shim as usize);
        // netStatus feed: retarget `call SV_ClientThink` (call-site).
        patch::retarget_call_tracked(
            patch::CALL_SV_CLIENT_THINK,
            netstatus::sv_client_think_stub as usize,
        );
    }

    // Engine detours.
    let engine_wrappers: [usize; patch::ENGINE_HOOKS.len()] = [
        engine_sv::svc_status as usize,
        engine_sv::svc_info as usize,
        engine_sv::svc_remote_command as usize,
        engine_sv::sv_userinfo_changed as usize,
        engine_sv::sv_begin_download_f as usize,
        engine_sv::sv_next_download_f as usize,
        engine_sv::sv_stop_download_f as usize,
        engine_sv::sv_done_download_f as usize,
        engine_sv::sv_update_userinfo_f as usize,
        engine_sv::sv_write_download_to_client as usize,
        engine_sv::sv_sventity_for_gentity as usize,
        engine_sv::navigator_load as usize,
        engine_sv::sv_send_client_game_state as usize,
        engine_sv::sv_execute_client_message as usize,
    ];
    for (hook, wrapper) in patch::ENGINE_HOOKS.iter().zip(&engine_wrappers) {
        // SAFETY: hook addresses are version-swept; wrappers match the engine
        // prototypes (R-013).
        unsafe { patch::attach(hook, *wrapper) };
    }

    // Game detours (rebased by the module base).
    let game_wrappers: [usize; patch::GAME_HOOKS.len()] = [
        game::g_damage as usize,
        game::player_die as usize,
        game::begin_intermission as usize,
        game::g_add_event as usize,
        game::client_think_real as usize,
    ];
    for (hook, wrapper) in patch::GAME_HOOKS.iter().zip(&game_wrappers) {
        // SAFETY: game offsets are nm-verified (R-011); wrappers match the
        // game prototypes.
        unsafe { patch::attach(hook, *wrapper) };
    }

    engine_sv::set_attached();
    eprintln!("----- proxy-rs: Engine properly patched");
}

/// Detach every hook and restore the call-site retargets
/// (`Proxy_Engine_Detach_Patches`). The inline/intra patches are left in place
/// (the original proxy does not revert them either).
///
/// # Safety
///
/// Must run before the original module is unloaded.
pub unsafe fn detach_all() {
    for hook in &patch::ENGINE_HOOKS {
        // SAFETY: hook was attached by attach_all.
        unsafe { patch::detach(hook) };
    }
    for hook in &patch::GAME_HOOKS {
        // SAFETY: hook was attached by attach_all.
        unsafe { patch::detach(hook) };
    }
    patch::restore_calls();
}
