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

use core::ffi::c_int;
use core::sync::atomic::{AtomicUsize, Ordering};

/// The engine syscall pointer as handed to `dllEntry`.
pub type EngineSyscall = unsafe extern "C" fn(c_int, ...) -> c_int;

/// System trap ordinals the game module passes as `command` to the syscall
/// pointer. Values are from the SDK `gameImport_t` enum
/// (`original/jedi-academy-sdk/codemp/game/g_public.h`), verified consecutive
/// from `G_PRINT = 0` up to the `G_MEMSET = 100` re-anchor. Only the traps the
/// passthrough skeleton itself uses are listed.
/// `(const char *var_name, char *buffer, int bufsize)`
pub const G_CVAR_VARIABLE_STRING_BUFFER: c_int = 9;

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
/// call (command + up to 16 words) to the engine unchanged.
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
