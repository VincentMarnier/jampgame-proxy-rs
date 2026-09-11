//! Engine syscall ("trap") interface.
//!
//! The engine hands the proxy a variadic cdecl syscall pointer through
//! `dllEntry`. That pointer is what both the proxy itself (at bootstrap, e.g.
//! to read cvars) and — via the C shim in `csrc/syscall_shim.c` — the original
//! game module ultimately reach the engine through.
//!
//! ABI notes:
//! - i386 System V cdecl, ILP32: every argument travels as a 32-bit word, so a
//!   `c_int` (i32) faithfully carries pointers too (the callee reinterprets).
//! - The original proxy's forwarder harvests up to 1 (command) + 16 args and
//!   always re-passes 16 (`Proxy_OriginalAPI_Wrappers.cpp:9-51`). We mirror
//!   that exactly so observable behaviour is identical, including traps that
//!   declare fewer arguments (the engine ignores the surplus words).
//! - Stable Rust cannot *define* a C-variadic function body, so receiving the
//!   variadic trap call happens in C (`jampgame_vm_dllsyscall`); it calls back
//!   into the fixed-arity `jampgame_syscall_forward` below.
//! - The proxy's *own* trap helpers (`trap_Cvar_*`, `trap_Argv`, …) call the
//!   engine with the exact arity each trap needs, exactly like
//!   `Proxy_Translate_SystemCalls.cpp`.

use core::ffi::{CStr, c_int};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sdk::{
    G_ARGV, G_CVAR_REGISTER, G_CVAR_SET, G_CVAR_UPDATE, G_CVAR_VARIABLE_INTEGER_VALUE,
    G_CVAR_VARIABLE_STRING_BUFFER, G_DROP_CLIENT, G_GET_USERCMD, G_GET_USERINFO,
    G_LOCATE_GAME_DATA, G_SEND_SERVER_COMMAND, G_SET_USERINFO, VmCvar,
};

/// The engine syscall pointer as handed to `dllEntry`.
pub type EngineSyscall = unsafe extern "C" fn(c_int, ...) -> c_int;

/// Stored engine syscall pointer, set once by `dllEntry` before any `vmMain`
/// call. Read-only afterwards; only the single engine thread reaches it.
static ENGINE_SYSCALL: AtomicUsize = AtomicUsize::new(0);

/// Record the syscall pointer delivered by the engine in `dllEntry`.
pub fn set_engine_syscall(syscall: EngineSyscall) {
    ENGINE_SYSCALL.store(syscall as usize, Ordering::Relaxed);
}

/// Retrieve the stored engine syscall pointer.
pub fn engine_syscall() -> Option<EngineSyscall> {
    let raw = ENGINE_SYSCALL.load(Ordering::Relaxed);
    if raw == 0 {
        None
    } else {
        // usize and fn pointers are both pointer-sized; this only reinterprets
        // the bits stored by `set_engine_syscall`.
        Some(unsafe { core::mem::transmute::<usize, EngineSyscall>(raw) })
    }
}

/// Call the engine syscall pointer with a fixed set of arguments. Used by the
/// proxy itself during bootstrap (no game module loaded yet, so the C shim is
/// not involved).
///
/// # Safety
///
/// `args` must describe the arguments the given trap `command` expects, and
/// the engine syscall pointer must have been delivered via `dllEntry`.
pub unsafe fn call_engine(command: c_int, args: &[c_int]) -> c_int {
    let Some(syscall) = engine_syscall() else {
        eprintln!("----- proxy-rs: engine syscall called before dllEntry");
        return -1;
    };
    unsafe {
        match args {
            [] => syscall(command),
            [a0] => syscall(command, *a0),
            [a0, a1] => syscall(command, *a0, *a1),
            [a0, a1, a2] => syscall(command, *a0, *a1, *a2),
            [a0, a1, a2, a3] => syscall(command, *a0, *a1, *a2, *a3),
            [a0, a1, a2, a3, a4] => syscall(command, *a0, *a1, *a2, *a3, *a4),
            _ => {
                eprintln!("----- proxy-rs: unsupported syscall arity for {command}");
                -1
            }
        }
    }
}

/// Fixed-arity continuation of the C variadic shim: forward one harvested trap
/// call (command + up to 16 words) to the engine unchanged — except for the
/// two traps the proxy intercepts (`Proxy_OriginalAPI_Wrappers.cpp:24-45`):
/// `G_LOCATE_GAME_DATA` is recorded, `G_GET_USERCMD` is mutated after the
/// engine fills the cmd. Both are then forwarded normally.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jampgame_syscall_forward(
    command: c_int,
    a0: c_int,
    a1: c_int,
    a2: c_int,
    a3: c_int,
    a4: c_int,
    a5: c_int,
    a6: c_int,
    a7: c_int,
    a8: c_int,
    a9: c_int,
    a10: c_int,
    a11: c_int,
    a12: c_int,
    a13: c_int,
    a14: c_int,
    a15: c_int,
) -> c_int {
    match command {
        // `G_LOCATE_GAME_DATA(sharedEntity_t*, int, int, playerState_t*, int)`:
        // record where the game put its data (later hooks read it), then pass
        // the trap through to the engine.
        G_LOCATE_GAME_DATA => {
            crate::shared_api::locate_game_data(a0 as usize, a1, a2, a3 as usize, a4);
        }
        // `G_GET_USERCMD(int clientNum, usercmd_t* cmd)`: forward first (the
        // engine writes the usercmd into *cmd), then sanitise it before the
        // game reads it.
        G_GET_USERCMD => {
            // SAFETY: engine syscall registered (dllEntry ran first); the game
            // passes a valid usercmd pointer it wants filled.
            let response = unsafe {
                engine_syscall_call(
                    command,
                    [
                        a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15,
                    ],
                )
            };
            // SAFETY: a1 is the game's usercmd buffer, just filled by the engine.
            // Only mutate it while the proxy is enabled (master switch); the
            // recording trap above stays active so a runtime re-enable works.
            if crate::state::proxy_enabled() {
                unsafe { crate::shared_api::get_usercmd(a0, a1 as *mut crate::sdk::Usercmd) };
            }
            return response;
        }
        _ => {}
    }

    match engine_syscall() {
        Some(syscall) => unsafe {
            syscall(
                command, a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15,
            )
        },
        None => {
            eprintln!("----- proxy-rs: syscall {command} before dllEntry");
            -1
        }
    }
}

/// Call the engine syscall pointer with exactly the given words (used by the
/// `G_GET_USERCMD` forwarding path and by nothing else directly).
///
/// # Safety
///
/// The engine syscall pointer must have been delivered via `dllEntry`.
#[inline]
unsafe fn engine_syscall_call(command: c_int, words: [c_int; 16]) -> c_int {
    let syscall = engine_syscall().expect("engine syscall not registered");
    unsafe {
        syscall(
            command, words[0], words[1], words[2], words[3], words[4], words[5], words[6],
            words[7], words[8], words[9], words[10], words[11], words[12], words[13], words[14],
            words[15],
        )
    }
}

// ---------------------------------------------------------------------------
// Proxy-side trap helpers (Proxy_Translate_SystemCalls.cpp subset)
// ---------------------------------------------------------------------------

/// Fill `buffer` with the value of cvar `name`
/// (`G_CVAR_VARIABLE_STRING_BUFFER`, `trap_Cvar_VariableStringBuffer`).
///
/// # Safety
///
/// The engine syscall pointer must be registered; `buffer` is written by the
/// engine and NUL-terminated.
pub unsafe fn cvar_variable_string_buffer(name: &CStr, buffer: &mut [u8]) {
    unsafe {
        call_engine(
            G_CVAR_VARIABLE_STRING_BUFFER,
            &[
                name.as_ptr() as c_int,
                buffer.as_mut_ptr() as c_int,
                buffer.len() as c_int,
            ],
        );
    }
}

/// Integer value of cvar `name` (`trap_Cvar_VariableIntegerValue`).
///
/// # Safety
///
/// The engine syscall pointer must be registered.
pub unsafe fn cvar_variable_integer_value(name: &CStr) -> c_int {
    unsafe { call_engine(G_CVAR_VARIABLE_INTEGER_VALUE, &[name.as_ptr() as c_int]) }
}

/// `trap_Cvar_Register` — register/re-sync a `vmCvar_t` mirror.
///
/// # Safety
///
/// `cvar` must point to a live `VmCvar` the engine writes through; `name` and
/// `default_value` must be NUL-terminated.
pub unsafe fn cvar_register(cvar: *mut VmCvar, name: &CStr, default_value: &CStr, flags: u32) {
    unsafe {
        call_engine(
            G_CVAR_REGISTER,
            &[
                cvar as usize as c_int,
                name.as_ptr() as c_int,
                default_value.as_ptr() as c_int,
                flags as c_int,
            ],
        );
    }
}

/// `trap_Cvar_Update` — refresh a `vmCvar_t` mirror from the engine.
///
/// # Safety
///
/// `cvar` must point to a live `VmCvar` the engine writes through.
pub unsafe fn cvar_update(cvar: *mut VmCvar) {
    unsafe {
        call_engine(G_CVAR_UPDATE, &[cvar as usize as c_int]);
    }
}

/// `trap_Cvar_Set` — set cvar `name` to `value` engine-side
/// (`G_CVAR_SET`, `trap_Cvar_Set`).
///
/// # Safety
///
/// The engine syscall pointer must be registered; `name` and `value` must be
/// NUL-terminated.
pub unsafe fn cvar_set(name: &CStr, value: &CStr) {
    unsafe {
        call_engine(
            G_CVAR_SET,
            &[name.as_ptr() as c_int, value.as_ptr() as c_int],
        );
    }
}

/// `trap_Argv` — copy command argument `n` into `buffer`.
///
/// # Safety
///
/// The engine syscall pointer must be registered; `buffer` is written by the
/// engine and NUL-terminated.
pub unsafe fn argv(n: c_int, buffer: &mut [u8]) {
    unsafe {
        call_engine(
            G_ARGV,
            &[n, buffer.as_mut_ptr() as c_int, buffer.len() as c_int],
        );
    }
}

/// `trap_DropClient` — kick `client_num` with `reason`.
///
/// # Safety
///
/// `reason` must be NUL-terminated; the engine syscall pointer must be
/// registered.
pub unsafe fn drop_client(client_num: c_int, reason: &CStr) {
    unsafe {
        call_engine(G_DROP_CLIENT, &[client_num, reason.as_ptr() as c_int]);
    }
}

/// `trap_SendServerCommand` — reliably send `text` to `client_num` (`-1` = all),
/// skipping messages longer than 1022 chars (guard from
/// `Proxy_Translate_SystemCalls.cpp:102-107`).
///
/// # Safety
///
/// `text` must be NUL-terminated; the engine syscall pointer must be
/// registered.
pub unsafe fn send_server_command(client_num: c_int, text: &CStr) {
    let bytes = text.to_bytes();
    if bytes.len() > 1022 {
        return;
    }
    unsafe {
        call_engine(G_SEND_SERVER_COMMAND, &[client_num, text.as_ptr() as c_int]);
    }
}

/// `trap_GetUserinfo` — copy client `num`'s userinfo into `buffer`.
///
/// # Safety
///
/// The engine syscall pointer must be registered; `buffer` is written by the
/// engine and NUL-terminated.
pub unsafe fn get_userinfo(num: c_int, buffer: &mut [u8]) {
    unsafe {
        call_engine(
            G_GET_USERINFO,
            &[num, buffer.as_mut_ptr() as c_int, buffer.len() as c_int],
        );
    }
}

/// `trap_SetUserinfo` — replace client `num`'s userinfo.
///
/// # Safety
///
/// `buffer` must be NUL-terminated; the engine syscall pointer must be
/// registered.
pub unsafe fn set_userinfo(num: c_int, buffer: &CStr) {
    unsafe {
        call_engine(G_SET_USERINFO, &[num, buffer.as_ptr() as c_int]);
    }
}

/// Read a NUL-terminated string the engine wrote into `buffer`, returning the
/// bytes before the terminator.
pub fn read_engine_string(buffer: &[u8]) -> &[u8] {
    let n = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    &buffer[..n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_syscall_round_trips() {
        // A real code pointer with a compatible cdecl word layout: transmuting
        // between fn-pointer types keeps a valid pointer, so no invalid-value
        // lint fires (a bare `0usize as fn` would be UB).
        unsafe extern "C" fn marker(
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
        ) -> c_int {
            0
        }
        // Fixed-arity cdecl fn (a valid code pointer), re-typed as the
        // variadic syscall pointer: fn-pointer-to-fn-pointer transmute keeps a
        // valid pointer, so no invalid-value lint fires.
        let fixed: unsafe extern "C" fn(
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
        ) -> c_int = marker;
        let marker: EngineSyscall = unsafe { core::mem::transmute(fixed) };
        set_engine_syscall(marker);
        assert!(matches!(engine_syscall(), Some(f) if f as usize == marker as usize));
        ENGINE_SYSCALL.store(0, Ordering::Relaxed);
        assert!(engine_syscall().is_none());
    }
}
