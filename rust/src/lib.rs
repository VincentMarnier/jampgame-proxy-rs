//! jampgame-proxy-rs — passthrough skeleton.
//!
//! A Rust re-implementation of the `jampgame_proxy` game-module proxy, in its
//! initial "forward everything" stage.
//!
//! Loading model (see `docs/reverse-engineering.md` R-014):
//!
//! 1. The 2003 i386 engine (`linuxjampded`) `dlopen`s this module as
//!    `base/jampgamei386.so` and dlsym()s `dllEntry` + `vmMain`.
//! 2. The engine calls `dllEntry(engineSyscall)`: we store the engine syscall
//!    pointer (nothing heavier — syscalls that need game state are not usable
//!    yet).
//! 3. At `vmMain(GAME_INIT, ...)` we print the banner, `dlopen` the pristine
//!    module as `jampgame_original.so` (CWD-relative, `fs_game`/`base` path),
//!    resolve its `dllEntry`/`vmMain`, and hand it our own syscall forwarder
//!    (`csrc/syscall_shim.c`) as its syscall pointer.
//! 4. Every `vmMain` word is then forwarded unchanged to the original module;
//!    every trap the original module issues goes through our shim and is
//!    forwarded unchanged to the engine.
//! 5. `GAME_SHUTDOWN` forwards the shutdown, closes the original module and
//!    resets.
//!
//! ABI: i386 System V cdecl, ILP32. `vmMain` is 13 `int`s (command + 12), the
//! syscall interface is variadic cdecl with up to 1 (command) + 16 words —
//! both mirroring the original proxy's fixed-arity forwarders. See
//! `src/syscall.rs` and `src/original.rs`.

mod original;
mod syscall;

use core::ffi::c_int;
use std::ffi::CString;

use syscall::{EngineSyscall, G_CVAR_VARIABLE_STRING_BUFFER};

/// Name the pristine game module is installed under next to this proxy.
const ORIGINAL_LIBRARY_NAME: &str = "jampgame_original.so";
/// Subdirectory the engine loads game modules from when `fs_game` is unset.
const DEFAULT_BASE_GAME_FOLDER_NAME: &str = "base";

// vmMain export enum (SDK `gameExport_t`): commands this skeleton handles
// specially. All other commands are forwarded verbatim.
const GAME_INIT: c_int = 0;
const GAME_SHUTDOWN: c_int = 1;

// ---------------------------------------------------------------------------
// Exported API (the only two symbols the engine dlsym()s)
// ---------------------------------------------------------------------------

/// The engine calls this once at load time, before any `vmMain`.
///
/// It only records the engine syscall pointer. Heavy work is deferred to
/// `GAME_INIT`, after the engine's subsystems (cvars, etc.) exist — same
/// ordering as the original proxy (R-014).
///
/// # Safety
///
/// Called by the engine's `Sys_LoadDll` path with the engine syscall pointer;
/// `syscall` must be a valid cdecl variadic function pointer that stays valid
/// for the process lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dllEntry(syscall: EngineSyscall) {
    syscall::set_engine_syscall(syscall);
    eprintln!("----- proxy-rs {VERSION}: dllEntry, engine syscall registered");
}

/// 13-word `vmMain`, matching the SDK signature `int vmMain(int command,
/// int arg0 … int arg11)` (SDK `g_main.c:503`). All 13 words are forwarded to
/// the original module.
///
/// # Safety
///
/// Called by the engine exactly like any game module `vmMain`. The caller must
/// pass exactly 13 cdecl words; the function itself only forwards them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vmMain(
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
) -> c_int {
    let args = [a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11];
    match command {
        GAME_INIT => game_init(&args),
        GAME_SHUTDOWN => game_shutdown(&args),
        _ => forward(command, &args),
    }
}

// ---------------------------------------------------------------------------
// vmMain handling
// ---------------------------------------------------------------------------

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `vmMain(GAME_INIT, levelTime, randomSeed, restart)`: load and wire the
/// original module, then forward the init so the pristine game initialises.
fn game_init(args: &[c_int; 12]) -> c_int {
    eprintln!("========================================================================");
    eprintln!("----- proxy-rs {VERSION}: passthrough skeleton loaded");
    eprintln!("========================================================================");

    let path = original_library_path();
    eprintln!("----- proxy-rs: loading original game library {ORIGINAL_LIBRARY_NAME} ({path:?})");
    if let Err(err) = original::load(&path) {
        eprintln!("----- proxy-rs: {err}");
        eprintln!("----- proxy-rs: cannot continue without the original game module, exiting");
        std::process::exit(1);
    }

    // Give the original module *our* syscall forwarder so every trap the game
    // issues passes through the proxy (mirrors Proxy_GetOriginalGameAPI).
    // SAFETY: original::load just succeeded.
    unsafe {
        original::wire_original_dll_entry(jampgame_vm_dllsyscall as EngineSyscall);
    }
    eprintln!("----- proxy-rs: {ORIGINAL_LIBRARY_NAME} properly loaded");

    // Forward GAME_INIT itself: the pristine game must initialise.
    forward(GAME_INIT, args)
}

/// `vmMain(GAME_SHUTDOWN, restart)`: forward the shutdown, then unload the
/// original module (reverse of `game_init`, same as the original proxy).
fn game_shutdown(args: &[c_int; 12]) -> c_int {
    let response = forward(GAME_SHUTDOWN, args);
    if original::is_loaded() {
        eprintln!("----- proxy-rs: unloading original game library {ORIGINAL_LIBRARY_NAME}");
        original::unload();
        eprintln!("----- proxy-rs: {ORIGINAL_LIBRARY_NAME} properly unloaded");
    }
    response
}

/// Forward one `vmMain` call (command + 12 words) to the original module.
fn forward(command: c_int, args: &[c_int; 12]) -> c_int {
    match original::forward_vm_main(command, args) {
        Some(response) => response,
        None => {
            eprintln!("----- proxy-rs: vmMain({command}) with no original module loaded");
            0
        }
    }
}

// ---------------------------------------------------------------------------
// Original module location
// ---------------------------------------------------------------------------

/// CWD-relative path of the original module: `<fs_game>/jampgame_original.so`,
/// where an empty `fs_game` means `base` — identical to the original proxy's
/// path construction (`Proxy_Files.cpp`).
fn original_library_path() -> CString {
    let mut buffer = [0u8; 1024];
    // SAFETY: G_CVAR_VARIABLE_STRING_BUFFER expects
    // (const char *name, char *buf, int bufsize); buffer is 1024 bytes and
    // engine syscall pointer is set (dllEntry ran before any vmMain).
    unsafe {
        syscall::call_engine(
            G_CVAR_VARIABLE_STRING_BUFFER,
            &[
                c"fs_game".as_ptr() as c_int,
                buffer.as_mut_ptr() as c_int,
                buffer.len() as c_int,
            ],
        );
    }
    let len = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    let fs_game = String::from_utf8_lossy(&buffer[..len]);
    let fs_game = fs_game.trim();
    let directory = if fs_game.is_empty() {
        DEFAULT_BASE_GAME_FOLDER_NAME
    } else {
        fs_game
    };
    match CString::new(format!("{directory}/{ORIGINAL_LIBRARY_NAME}")) {
        Ok(path) => path,
        Err(_) => CString::new(format!(
            "{DEFAULT_BASE_GAME_FOLDER_NAME}/{ORIGINAL_LIBRARY_NAME}"
        ))
        .expect("static path has no interior NUL"),
    }
}

// ---------------------------------------------------------------------------
// The variadic trap shim (defined in C, csrc/syscall_shim.c)
// ---------------------------------------------------------------------------

unsafe extern "C" {
    /// Harvests up to command + 16 words and calls back into
    /// `syscall::jampgame_syscall_forward`. Handed to the original module's
    /// `dllEntry` as its syscall pointer.
    fn jampgame_vm_dllsyscall(command: c_int, ...) -> c_int;
}

// The C shim is only referenced through its function pointer (never called
// directly from Rust), so keep the linker from dropping it.
#[used]
static KEEP_SHIM: unsafe extern "C" fn(c_int, ...) -> c_int = jampgame_vm_dllsyscall;
