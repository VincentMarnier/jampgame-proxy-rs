//! jampgame-proxy-rs — the Rust rewrite of `jampgame_proxy`.
//!
//! The module loads in the game-module slot, runs the version gate, loads and
//! wires the pristine game module, initialises the engine memory layer,
//! intercepts the `G_LOCATE_GAME_DATA`/`G_GET_USERCMD` traps, runs the
//! `vmMain` event handlers that feed the proxy's own state, and attaches the
//! engine/game hook layer (D-002 keep set: rcon + download + userinfo +
//! netStatus + game stats/anti-HP/min-jump, all as minimal-alteration
//! detours / intra injections).
//!
//! Loading model (see `docs/reverse-engineering.md` R-014):
//!
//! 1. The 2003 i386 engine (`linuxjampded`) `dlopen`s this module as
//!    `base/jampgamei386.so` and dlsym()s `dllEntry` + `vmMain`.
//! 2. `dllEntry(engineSyscall)` stores the engine syscall pointer.
//! 3. At `vmMain(GAME_INIT, ...)`: banner → version gate → `dlopen` the
//!    pristine module as `jampgame_original.so` → bind its symbols + base →
//!    hand it our syscall forwarder → initialise the engine memory layer →
//!    forward `GAME_INIT` → register the proxy's own cvars.
//! 4. Every later `vmMain` word is forwarded to the original module; every
//!    trap it issues goes through the C shim (`csrc/syscall_shim.c`) into
//!    `syscall::jampgame_syscall_forward`, where the two intercepted traps run
//!    before the passthrough.
//! 5. `GAME_SHUTDOWN` closes the engine redirect, forwards the shutdown, and
//!    unloads the original module.
//!
//! ABI: i386 System V cdecl, ILP32. `vmMain` is 13 `int`s (command + 12), the
//! syscall interface is variadic cdecl with up to 1 (command) + 16 words —
//! both mirroring the original proxy's forwarders. See `src/syscall.rs`,
//! `src/original.rs`, `src/sdk.rs`.

mod engine;
mod hooks;
mod jampgame;
mod original;
mod patch;
mod rules;
mod sdk;
mod shared_api;
mod state;
mod syscall;
mod teamlock;
mod tffa;
mod utils;

use core::ffi::c_int;

use sdk::{
    DEFAULT_BASE_GAME_FOLDER_NAME, GAME_CLIENT_BEGIN, GAME_CLIENT_COMMAND, GAME_CLIENT_CONNECT,
    GAME_CLIENT_DISCONNECT, GAME_CLIENT_USERINFO_CHANGED, GAME_INIT, GAME_RUN_FRAME, GAME_SHUTDOWN,
    ORIGINAL_ENGINE_VERSION, ORIGINAL_LIBRARY_NAME,
};
use state::PROXY_CVARS;

// ---------------------------------------------------------------------------
// Exported API (the only two symbols the engine dlsym()s)
//
// Alignment: the 2003 engine enters game modules with a 4-byte-aligned stack.
// Rust i686 codegen normally assumes 16-byte entry alignment and emits SSE
// moves that fault there; the i686 target therefore disables SSE
// (rust/.cargo/config.toml) so no 16-byte-aligned instruction can be emitted.
// The C syscall shim is compiled with -mstackrealign (build.rs) for the same
// reason on its entry from the game module.
// ---------------------------------------------------------------------------

/// The engine calls this once at load time, before any `vmMain`. It only
/// records the engine syscall pointer; heavy work is deferred to `GAME_INIT`
/// (same ordering as the original proxy, R-014).
///
/// # Safety
///
/// Called by the engine's `Sys_LoadDll` path with the engine syscall pointer;
/// `syscall` must be a valid cdecl variadic function pointer that stays valid
/// for the process lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dllEntry(syscall: syscall::EngineSyscall) {
    syscall::set_engine_syscall(syscall);
    eprintln!("----- proxy-rs {VERSION}: dllEntry, engine syscall registered");
}

/// 13-word `vmMain` (SDK signature `int vmMain(int command, int arg0 … int
/// arg11)`, `g_main.c:503`). All 13 words are forwarded to the original module
/// after the proxy's own event handling runs.
///
/// # Safety
///
/// Called by the engine exactly like any game module `vmMain`; the caller must
/// pass exactly 13 cdecl words.
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
        // The server is going to activate its new frame: track the time,
        // refresh our cvar mirrors, reconcile the master enable switch (attach
        // or detach the hook layer at runtime), then pass the frame on.
        GAME_RUN_FRAME => {
            state::set_svs_time(a0);
            update_proxy_cvars();
            // SAFETY: original module loaded, memory layer initialised.
            unsafe { hooks::set_enabled(state::proxy_enabled()) };
            if state::proxy_enabled() {
                // Team lock / size rules are global-roster features; they are
                // meaningless (and harmful) while parallel TFFA matches share
                // the RED/BLUE teams, so they stay off then.
                if !tffa::enabled() {
                    teamlock::on_run_frame();
                    rules::on_run_frame();
                }
                tffa::on_frame_start();
            }
            let response = forward(GAME_RUN_FRAME, &args);
            if state::proxy_enabled() {
                tffa::on_run_frame_post();
            }
            response
        }
        GAME_CLIENT_CONNECT => {
            if state::proxy_enabled() {
                shared_api::client_connect(a0, a1 != 0, a2 != 0);
                tffa::on_client_connect(a0, a1 != 0);
                teamlock::on_client_connect(a1 != 0);
            }
            forward(command, &args)
        }
        GAME_CLIENT_DISCONNECT => {
            if state::proxy_enabled() {
                shared_api::client_disconnect(a0);
                tffa::on_client_disconnect(a0);
            }
            forward(command, &args)
        }
        GAME_CLIENT_BEGIN => {
            if state::proxy_enabled() {
                shared_api::client_begin(a0, a1 != 0);
            }
            let response = forward(command, &args);
            if state::proxy_enabled() {
                tffa::on_client_begin(a0);
            }
            response
        }
        GAME_CLIENT_COMMAND => {
            if state::proxy_enabled() {
                if tffa::handle_client_command(a0) {
                    return 0;
                }
                if !shared_api::client_command(a0) {
                    return 0;
                }
            }
            forward(command, &args)
        }
        GAME_CLIENT_USERINFO_CHANGED => {
            if state::proxy_enabled() {
                shared_api::client_userinfo_changed(a0);
            }
            forward(command, &args)
        }
        _ => forward(command, &args),
    }
}

// ---------------------------------------------------------------------------
// vmMain handling
// ---------------------------------------------------------------------------

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `vmMain(GAME_INIT, levelTime, randomSeed, restart)` — mirrors
/// `Proxy_Main.cpp:88-126` (without the patch attach, which is a later
/// milestone).
fn game_init(args: &[c_int; 12]) -> c_int {
    eprintln!("========================================================================");
    eprintln!("----- proxy-rs {VERSION}: bootstrap core loaded");
    eprintln!("========================================================================");

    // Version gate: only run on the pristine engine (R-009).
    if !engine_version_matches() {
        eprintln!("========================================================================");
        eprintln!("----- Trying to run jampgame-proxy-rs on a modified engine, exiting");
        eprintln!("========================================================================");
        std::process::exit(1);
    }

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
        original::wire_original_dll_entry(jampgame_vm_dllsyscall as syscall::EngineSyscall);
    }
    eprintln!("----- proxy-rs: {ORIGINAL_LIBRARY_NAME} properly loaded");
    eprintln!("----- proxy-rs: Original engine detected");

    // Read the engine's variable/cvar slots (R-018 live-verified addresses).
    engine::init_memory_layer();

    // Forward GAME_INIT itself: the pristine game must initialise.
    let response = forward(GAME_INIT, args);

    // The original proxy registers its cvars inside the game's G_RegisterCvars
    // (a game detour, later milestone); we register them here, after the game
    // has run its own registration, which is idempotent on the engine side.
    register_proxy_cvars();
    update_proxy_cvars();

    // Fresh round for the parallel-TFFA matches (scores reset, created
    // matches closed, main slot re-opened). Client assignments survive a
    // map_restart; a full map change re-dlopens the module (fresh state).
    tffa::reset_round();

    // Attach the engine/game hook layer (D-002 keep set) after the game is up
    // and the memory layer is populated. Skipped when `proxy_sv_enable` is off
    // (the master disable switch): the proxy then stays a pure passthrough.
    // SAFETY: original module loaded, memory layer initialised; hooks attach.
    unsafe { hooks::set_enabled(state::proxy_enabled()) };

    response
}

/// `vmMain(GAME_SHUTDOWN, restart)`: detach the hooks, close any engine
/// redirect left open by an rcon map change, forward the shutdown, then unload
/// the original module (`Proxy_Main.cpp:128-160`).
fn game_shutdown(args: &[c_int; 12]) -> c_int {
    // Undo any team-size rule override before the module is unloaded: game cvars
    // persist across maps, so a rule value would otherwise leak into the next
    // round (no-op when no rule is applied).
    rules::restore_on_shutdown();
    // SAFETY: hooks were attached at GAME_INIT; must run before dlclose.
    unsafe { hooks::detach_all() };
    // SAFETY: engine redirect globals are valid; no-op when no redirect open.
    unsafe { engine::com_end_redirect() };
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
// Version gate
// ---------------------------------------------------------------------------

/// `strncmp(version, ORIGINAL_ENGINE_VERSION, len(ORIGINAL_ENGINE_VERSION))`
/// gate (`Proxy_Main.cpp:96-105`): pass only when the `version` cvar starts
/// with the pristine engine version string.
fn engine_version_matches() -> bool {
    let mut buffer = [0u8; sdk::MAX_STRING_CHARS];
    // SAFETY: engine syscall registered (dllEntry ran) and buffer is writable.
    unsafe { syscall::cvar_variable_string_buffer(c"version", &mut buffer) };
    let version = syscall::read_engine_string(&buffer);
    version.len() >= ORIGINAL_ENGINE_VERSION.len()
        && version[..ORIGINAL_ENGINE_VERSION.len()] == *ORIGINAL_ENGINE_VERSION.as_bytes()
}

// ---------------------------------------------------------------------------
// Proxy cvar mirror (register at GAME_INIT, refresh every GAME_RUN_FRAME)
// ---------------------------------------------------------------------------

fn register_proxy_cvars() {
    // The vmCvar_t mirrors live inside the mutex-protected proxy state; their
    // addresses are stable for the process lifetime, so the engine can write
    // through them across calls.
    state::with_state(|s| {
        for (i, (name, default)) in PROXY_CVARS.iter().enumerate() {
            let cvar = &mut s.cvars[i] as *mut sdk::VmCvar;
            // SAFETY: name/default are static literals; cvar is stable.
            unsafe {
                syscall::cvar_register(cvar, name, default, sdk::CVAR_ARCHIVE);
            }
        }
    });
}

fn update_proxy_cvars() {
    if !original::is_loaded() {
        return;
    }
    let enabled = state::with_state(|s| {
        for (i, _) in PROXY_CVARS.iter().enumerate() {
            let cvar = &mut s.cvars[i] as *mut sdk::VmCvar;
            // SAFETY: cvar is stable within the state.
            unsafe { syscall::cvar_update(cvar) };
        }
        s.cvars[state::CVAR_ENABLE].integer != 0
    });
    state::set_proxy_enabled(enabled);
}

// ---------------------------------------------------------------------------
// Original module location
// ---------------------------------------------------------------------------

/// CWD-relative path of the original module: `<fs_game>/jampgame_original.so`,
/// where an empty `fs_game` means `base` — identical to the original proxy's
/// path construction (`Proxy_Files.cpp`).
fn original_library_path() -> CString {
    let mut buffer = [0u8; sdk::MAX_OSPATH];
    // SAFETY: engine syscall pointer is set (dllEntry ran before any vmMain).
    unsafe {
        syscall::cvar_variable_string_buffer(c"fs_game", &mut buffer);
    }
    let fs_game = syscall::read_engine_string(&buffer);
    let fs_game = String::from_utf8_lossy(fs_game).trim().to_owned();
    let directory = if fs_game.is_empty() {
        DEFAULT_BASE_GAME_FOLDER_NAME
    } else {
        fs_game.as_str()
    };
    CString::new(format!("{directory}/{ORIGINAL_LIBRARY_NAME}"))
        .expect("derived library path has no interior NUL")
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

use std::ffi::CString;
