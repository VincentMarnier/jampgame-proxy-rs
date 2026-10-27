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
mod mute;
mod netstatus_est;
mod original;
mod patch;
mod rules;
mod scoreboard;
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
    GAME_CLIENT_DISCONNECT, GAME_CLIENT_USERINFO_CHANGED, GAME_CONSOLE_COMMAND, GAME_INIT,
    GAME_RUN_FRAME, GAME_SHUTDOWN, ORIGINAL_ENGINE_VERSION, ORIGINAL_LIBRARY_NAME,
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
    // Keep this module mapped for the whole process: the engine dlclose()s us
    // mid-call on a map change (rcon commands run inside our SVC_RemoteCommand
    // wrapper), and the stale return must land on mapped code.
    original::pin_own_module();
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
            // Keep the serverinfo feature bit in step with the master switch
            // (a runtime disable makes existing clients fall back to the
            // pristine 14-int parse).
            scoreboard::sync_serverinfo();
            // SAFETY: original module loaded, memory layer initialised.
            unsafe { hooks::set_enabled(state::proxy_enabled()) };
            if state::proxy_enabled() {
                // The team lock opens each round's grace window and later caps
                // each TFFA instance against its own roster (main when the
                // feature is off), so it is compatible with parallel matches.
                teamlock::on_run_frame();
                rules::on_run_frame();
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
            // Per-client bookkeeping, not a feature: keep mirroring the client
            // lifecycle whatever the master switch says. A client that
            // connected while the switch was off would otherwise stay "not
            // connected" in the mirror after a runtime `proxy_sv_enable 1`,
            // and `shared_api::client_command` drops every command from an
            // unknown client — the TAB `score` request and `netStatus`
            // included, silently (empty scoreboard, no netStatus output).
            shared_api::client_disconnect(a0);
            if state::proxy_enabled() {
                tffa::on_client_disconnect(a0);
            }
            // A disconnecting client drops what they carry
            // (`ClientDisconnect` → `TossClientItems`, `g_client.c:3876`): the
            // dropped items belong to the match the player was in (S9).
            let scope = if tffa::enabled() {
                Some(tffa::open_client_scope(a0))
            } else {
                None
            };
            let response = forward(command, &args);
            if let Some(previous) = scope {
                tffa::close_acting_scope(previous);
            }
            response
        }
        GAME_CLIENT_BEGIN => {
            // See GAME_CLIENT_DISCONNECT: the connected mirror outlives the
            // master switch so a runtime enable finds the clients that began
            // while it was off.
            shared_api::client_begin(a0, a1 != 0);
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
                // A muted player's chat/voice never reaches the game's own
                // `ClientCommand`; everything else they send still does.
                if mute::handle_client_command(a0) {
                    return 0;
                }
                if !shared_api::client_command(a0) {
                    return 0;
                }
            }
            // The game's own `ClientCommand` runs here (a player's `+use` on a
            // named target looks it up with `G_Find`, `g_cmds.c:3251`): running
            // it in the issuing player's scope keeps that lookup inside their
            // instance (S9).
            let scope = if tffa::enabled() {
                Some(tffa::open_client_scope(a0))
            } else {
                None
            };
            let response = forward(command, &args);
            if let Some(previous) = scope {
                tffa::close_acting_scope(previous);
            }
            response
        }
        GAME_CLIENT_USERINFO_CHANGED => {
            if state::proxy_enabled() {
                shared_api::client_userinfo_changed(a0);
            }
            forward(command, &args)
        }
        // rcon (`SVC_RemoteCommand` → `Cmd_ExecuteString`) and the dedicated
        // console both hand a command the engine does not know to the game
        // module. The proxy claims `mute`/`unmute` here: returning qtrue keeps
        // the dedicated server from echoing the line to every client as
        // `server: …` (g_svcmds.c:ConsoleCommand).
        GAME_CONSOLE_COMMAND => {
            if state::proxy_enabled() && mute::handle_console_command() {
                return 1;
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

    // Fresh state for this session: the pinned module survives map changes, so
    // the fresh statics a real reload used to provide are reset here instead.
    // A `map_restart` (`restart != qfalse`) keeps the parallel-TFFA overlay —
    // which matches were open and which instance each client was in — so the
    // players land back in their matches; a full map change starts from main,
    // exactly like a fresh module load.
    let restart = args[2] != 0;
    state::reset_session(restart);

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
    // Advertise `taystJKinfo` bit 7 before the engine builds `CS_SERVERINFO`
    // for this spawn (`SV_SpawnServer` runs its settle frames, then sets the
    // configstring, both after `GAME_INIT`).
    scoreboard::sync_serverinfo();

    // Fresh round for the parallel-TFFA matches (scores reset, the main slot
    // re-opened; a `map_restart` keeps the matches and assignments carried
    // above, a full map change closes them). Per-instance worlds are re-measured
    // by `scan_world_entities` below and rebuilt by `restore_instances` once the
    // hook layer is attached.
    tffa::reset_round(restart);
    // Measure this map's interactive-entity set (per-instance world entities,
    // D-006 extension): the gentity cost of one further instance, and thus how
    // many matches the map can still take. Map entities now exist (the pristine
    // GAME_INIT spawned them).
    if tffa::enabled() {
        tffa::scan_world_entities();
    }
    // Fresh round for the team lock (caps and grace bookkeeping cleared, the
    // window re-armed for the reconnect burst that follows GAME_INIT).
    teamlock::reset();

    // Attach the engine/game hook layer (D-002 keep set) after the game is up
    // and the memory layer is populated. Skipped when `proxy_sv_enable` is off
    // (the master disable switch): the proxy then stays a pure passthrough.
    // SAFETY: original module loaded, memory layer initialised; hooks attach.
    unsafe { hooks::set_enabled(state::proxy_enabled()) };

    // Give the carried created matches their per-instance worlds back. This
    // needs the `G_Spawn` detour and the `G_LINKENTITY` trampoline, so it runs
    // after the hook layer is up; clients reconnect after the settle frames,
    // so they find their instance already rebuilt.
    tffa::restore_instances();

    response
}

/// `vmMain(GAME_SHUTDOWN, restart)`: detach the hooks, close any engine
/// redirect left open by an rcon map change, forward the shutdown, then unload
/// the original module (`Proxy_Main.cpp:128-160`).
fn game_shutdown(args: &[c_int; 12]) -> c_int {
    // Drop the serverinfo feature bit: the engine's cvar table outlives this
    // module, and a pristine module loaded next must not keep advertising the
    // 15-int scoreboard parse.
    scoreboard::clear_serverinfo();
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
        // The TaystJK feature key is not a proxy cvar: it lives in the
        // serverinfo (read by every cgame) and its value is owned by
        // `scoreboard::sync_serverinfo`.
        // SAFETY: the mirror is stable within the state.
        unsafe { scoreboard::register_cvar(&mut s.taystjk_info as *mut sdk::VmCvar) };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn client_event(command: c_int, client_num: c_int) {
        // SAFETY: `vmMain` is the engine's plain 13-word entry point; a
        // lifecycle command only touches proxy state (and forwards, a no-op
        // here because no original module is loaded in a unit test).
        unsafe {
            vmMain(command, client_num, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        }
    }

    fn is_connected(client_num: c_int) -> bool {
        state::with_state(|s| s.clients[client_num as usize].is_connected)
    }

    /// A client that connects and begins while `proxy_sv_enable` is `0` must
    /// still land in the proxy's per-client mirror.
    ///
    /// The mirror is what makes `shared_api::client_command` forward that
    /// client's commands once the master switch is turned back on; before this
    /// was fixed, a client that joined during a disabled window stayed "not
    /// connected" forever, so every command it issued was dropped silently —
    /// the TAB `score` request (empty scoreboard) and `netStatus`/`showNet`
    /// (no output) included.
    #[test]
    fn client_mirror_survives_a_disabled_master_switch() {
        state::set_proxy_enabled(false);
        assert!(!state::proxy_enabled());

        client_event(GAME_CLIENT_BEGIN, 5);
        assert!(
            is_connected(5),
            "a client that began while the proxy was off must be mirrored"
        );

        client_event(GAME_CLIENT_DISCONNECT, 5);
        assert!(
            !is_connected(5),
            "a disconnect while the proxy was off must clear the mirror"
        );

        // Restore the process-wide default for the other tests.
        state::set_proxy_enabled(true);
    }
}
