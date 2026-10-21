//! Engine hook wrappers (`Patches/server/Proxy_sv_*.cpp`) — the D-002
//! wanted/kept set as minimal-alteration detours: entry wrappers over pristine
//! bodies via the trampolines in `crate::patch`, plus the `ipAuthorize`/RMG/
//! `Com_Printf` intra injections (see `crate::hooks::attach_all`).

use core::ffi::{CStr, c_char, c_int};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::engine;
use crate::hooks::compat;
use crate::hooks::netstatus;
use crate::hooks::original_call;
use crate::jampgame;
use crate::sdk::{
    CLC_MOVE, CLC_MOVE_NO_DELTA, CS_ACTIVE, EF_PLAYER_EVENT, ENTITYNUM_NONE, ENTITYNUM_WORLD,
    ERR_DROP, MAX_CLIENTS, MAX_GENTITIES, MAX_QPATH, OFFSET_CLIENT_DOWNLOAD,
    OFFSET_CLIENT_DOWNLOAD_NAME, OFFSET_CLIENT_GAMESTATE_MESSAGE_NUM,
    OFFSET_CLIENT_LAST_CLIENT_COMMAND, OFFSET_CLIENT_NAME, OFFSET_CLIENT_PING, OFFSET_CLIENT_STATE,
    OFFSET_CLIENT_USERINFO, OFFSET_ENTITY_CLIENT_NUM, OFFSET_ENTITY_NUMBER, OFFSET_GENTITY_CLIENT,
    OFFSET_GENTITY_R_OWNER, OFFSET_GENTITY_S, OFFSET_GENTITY_S_EFLAGS, OFFSET_GENTITY_S_ETYPE,
    OFFSET_GENTITY_S_OTHER_ENTITY_NUM, OFFSET_GENTITY_S_OTHER_ENTITY_NUM2, OFFSET_GENTITY_S_OWNER,
    OFFSET_MSG_BIT, OFFSET_MSG_READCOUNT, OFFSET_SHARED_ENTITY_S, OFFSET_SNAPSHOT_ENUMS_COUNT,
    OFFSET_SNAPSHOT_ENUMS_LIST, OFFSET_SNAPSHOT_FRAME_PS_CLIENT_NUM, OFFSET_SV_GENTITIES,
    OFFSET_SV_GENTITY_SIZE, OFFSET_SV_SV_ENTITIES, SIZEOF_CLIENT_T, SIZEOF_GENTITY,
    SIZEOF_SV_ENTITY_T, SVC_DOWNLOAD,
};
use crate::state::{self, CVAR_ENABLE_RCON_CMD_COOLDOWN, CVAR_MODEL_PATH_LENGTH};
use crate::syscall;
use crate::utils;

/// `netadr_t` by value (SDK `qcommon.h`): `type(4) + ip[4](4) + ipx[10](10) +
/// port(2)` = 20 bytes on the cdecl stack.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Netadr {
    pub kind: i32,
    pub ip: [u8; 4],
    pub ipx: [u8; 10],
    pub port: u16,
}

/// The rcon cooldown timestamp (the original's `static unsigned int
/// commandCoolDown`, `Proxy_sv_main.cpp:126`). Single-threaded engine.
static RCON_COOLDOWN: AtomicU32 = AtomicU32::new(0);

/// True once `attach_all` installed the detours.
static HOOKS_ATTACHED: AtomicBool = AtomicBool::new(false);

pub fn set_attached() {
    HOOKS_ATTACHED.store(true, Ordering::Relaxed);
}

/// Clear the attached flag (the master disable switch / `detach_all`).
pub fn set_detached() {
    HOOKS_ATTACHED.store(false, Ordering::Relaxed);
}

pub fn attached() -> bool {
    HOOKS_ATTACHED.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Engine memory access helpers
// ---------------------------------------------------------------------------

/// `server.svs->time` (`serverStatic_t.time` @ 4, var 0x83121e0).
///
/// # Safety
///
/// Engine memory layer must be initialised.
pub unsafe fn svs_time() -> i32 {
    // SAFETY: validated engine RW address (R-018).
    unsafe { read_i32(engine::vars::SVS + 4) }
}

/// `svs->clients` array base (`serverStatic_t.clients` pointer @ 0x83121ec).
///
/// # Safety
///
/// Engine memory layer must be initialised.
pub unsafe fn clients_base() -> usize {
    // SAFETY: validated engine RW address (R-018).
    unsafe { core::ptr::read_unaligned(engine::vars::SVSCLIENTS as *const usize) }
}

/// The engine `server_t*` (`server.sv` @ 0x8273ec0). `sv` is a global *struct*
/// (`extern server_t sv`), so the "pointer" is the address of the global
/// itself, exactly as the original proxy binds `server.sv = (server_t*)0x8273ec0`.
pub unsafe fn sv_ptr() -> usize {
    engine::vars::SV
}

/// `(cl - svs->clients)` — the original's `getClientNumFromAddr`.
pub fn client_num_from_addr(cl: usize) -> usize {
    // SAFETY: engine memory layer initialised (hooks only run after GAME_INIT).
    let base = unsafe { clients_base() };
    let num = cl.wrapping_sub(base) / SIZEOF_CLIENT_T;
    if num >= crate::sdk::MAX_CLIENTS {
        eprintln!(
            "----- proxy-rs: netstatus getClientNumFromAddr cl={cl:#x} base={base:#x} -> {num}"
        );
    }
    num
}

/// Read an `i32` engine struct field.
///
/// # Safety
///
/// `addr` must be a readable engine/game struct field.
pub unsafe fn read_i32(addr: usize) -> i32 {
    // SAFETY: caller guarantees a readable int field.
    unsafe { core::ptr::read_unaligned(addr as *const i32) }
}

/// Read a `usize` (pointer) engine struct field.
///
/// # Safety
///
/// `addr` must be a readable engine/game struct field.
pub unsafe fn read_usize(addr: usize) -> usize {
    // SAFETY: caller guarantees a readable pointer field.
    unsafe { core::ptr::read_unaligned(addr as *const usize) }
}

/// Write an `i32` engine/game struct field.
///
/// # Safety
///
/// `addr` must be a writable field the caller owns (the clone pass writes
/// gentity slots it just spawned, never a slot another owner holds).
pub unsafe fn write_i32(addr: usize, value: i32) {
    // SAFETY: caller guarantees a writable int field.
    unsafe { core::ptr::write_unaligned(addr as *mut i32, value) }
}

/// Write a `usize` (pointer) engine/game struct field.
///
/// # Safety
///
/// See `write_i32`; `value` must be a valid pointer for the field's meaning.
pub unsafe fn write_usize(addr: usize, value: usize) {
    // SAFETY: caller guarantees a writable pointer field.
    unsafe { core::ptr::write_unaligned(addr as *mut usize, value) }
}

/// The client's `state` field.
///
/// # Safety
///
/// `cl` must be a valid `client_t*`.
pub unsafe fn client_state(cl: usize) -> i32 {
    // SAFETY: caller guarantees a valid client pointer.
    unsafe { read_i32(cl + OFFSET_CLIENT_STATE) }
}

/// The client's `ping` field.
///
/// # Safety
///
/// `cl` must be a valid `client_t*`.
#[allow(dead_code)] // mirrors the wrappers table; kept for future netstatus paths
pub unsafe fn client_ping(cl: usize) -> i32 {
    // SAFETY: caller guarantees a valid client pointer.
    unsafe { read_i32(cl + OFFSET_CLIENT_PING) }
}

/// NUL-terminated bytes of a C string, or the empty slice if null.
pub fn cstr_bytes<'a>(p: *const c_char) -> &'a [u8] {
    if p.is_null() {
        &b""[..]
    } else {
        // SAFETY: caller guarantees a NUL-terminated string pointer.
        unsafe { core::ffi::CStr::from_ptr(p) }.to_bytes()
    }
}

/// NUL-terminated string length (null pointer → 0).
pub fn cstr_len(p: *const c_char) -> usize {
    if p.is_null() {
        0
    } else {
        // SAFETY: caller guarantees NUL-termination.
        unsafe { core::ffi::CStr::from_ptr(p) }.to_bytes().len()
    }
}

// ---------------------------------------------------------------------------
// SVC_RemoteCommand (rcon) — entry wrapper (D-002 "rcon" row)
// ---------------------------------------------------------------------------

/// `Proxy_SVC_RemoteCommand` (`Proxy_sv_main.cpp:124-145`): cooldown + the
/// `writeconfig` `.cfg`-only check, then the original.
///
/// # Safety
///
/// Entered from the engine's connectionless dispatcher with `(netadr_t, msg_t*)`.
pub unsafe extern "C" fn svc_remote_command(from: Netadr, msg: usize) {
    let cooldown_enabled =
        state::with_state(|s| s.cvars[CVAR_ENABLE_RCON_CMD_COOLDOWN].integer != 0);
    // SAFETY: engine memory layer initialised.
    let now = unsafe { svs_time() } as u32;
    // wrapping_add: svs.time wraps past u32::MAX after ~49 days; a plain `+`
    // would panic (= abort) in debug builds at the wrap.
    if cooldown_enabled && now.wrapping_sub(RCON_COOLDOWN.load(Ordering::Relaxed)) < 500 {
        return;
    }
    RCON_COOLDOWN.store(now, Ordering::Relaxed);

    // Only allow writeconfig on cfg files.
    // SAFETY: engine Cmd_Argv is valid in the connectionless context.
    let argv2 = unsafe { engine::cmd_argv(2) };
    if !argv2.is_null() && unsafe { jampgame::q_stricmpn(argv2, c"writeconfig".as_ptr(), 11) } == 0
    {
        // SAFETY: Cmd_Argv returns NUL-terminated strings.
        let arg3 = unsafe { engine::cmd_argv(3) };
        let arg3_len = cstr_len(arg3);
        if arg3_len >= 5
            && unsafe { *arg3.add(arg3_len - 4) } == b'.' as i8
            && unsafe { jampgame::q_stricmpn(arg3.add(arg3_len - 4), c".cfg".as_ptr(), 4) } != 0
        {
            return;
        }
    }

    // SAFETY: trampoline attached at GAME_INIT; args forwarded unchanged.
    let original = original_call(crate::hooks::engine_hook_original_svc_remote_command);
    unsafe { original(from, msg) };
}

// ---------------------------------------------------------------------------
// SVC_Status / SVC_Info (q3infoboom) — entry wrappers
// ---------------------------------------------------------------------------

/// `Proxy_SVC_Status` (`Proxy_sv_main.cpp:88-99`): drop `getstatus` replies
/// whose argv[1] would overflow the info string (q3infoboom).
///
/// # Safety
///
/// Entered from `SV_ConnectionlessPacket` with a `netadr_t`.
pub unsafe extern "C" fn svc_status(from: Netadr) {
    if cmd_argv_len(1) > 128 {
        return;
    }
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::engine_hook_original_svc_status);
    unsafe { original(from) };
}

/// `Proxy_SVC_Info` (`Proxy_sv_main.cpp:101-113`) — same q3infoboom guard.
///
/// # Safety
///
/// Entered from `SV_ConnectionlessPacket` with a `netadr_t`.
pub unsafe extern "C" fn svc_info(from: Netadr) {
    if cmd_argv_len(1) > 128 {
        return;
    }
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::engine_hook_original_svc_info);
    unsafe { original(from) };
}

/// `strlen(Cmd_Argv(n))`, with a null-safe 0.
fn cmd_argv_len(n: c_int) -> usize {
    // SAFETY: engine Cmd_Argv is valid in the connectionless context.
    let p = unsafe { engine::cmd_argv(n) };
    cstr_len(p)
}

// ---------------------------------------------------------------------------
// SV_UserinfoChanged (model path length) — entry wrapper
// ---------------------------------------------------------------------------

/// `Proxy_SV_UserinfoChanged` (`Proxy_sv_client.cpp:404-424`): force a valid
/// model into `cl->userinfo` before the game's `ClientUserinfoChanged` runs
/// (connection-time model-length / non-ASCII crash fix).
///
/// # Safety
///
/// Entered from the engine with a valid `client_t*`.
pub unsafe extern "C" fn sv_userinfo_changed(cl: usize) {
    let userinfo = cl + OFFSET_CLIENT_USERINFO;
    // SAFETY: cl is a valid client_t; userinfo is NUL-terminated.
    let model =
        unsafe { jampgame::info_value_for_key(userinfo as *const c_char, c"model".as_ptr()) };
    if !model.is_null() {
        let model_bytes = cstr_bytes(model);
        let model_len = model_bytes.len();
        // Admin-controlled cvar is clamped >= 1 so a negative value cannot
        // disable the check via a huge `as usize` cast.
        let max_len =
            state::with_state(|s| s.cvars[CVAR_MODEL_PATH_LENGTH].integer.max(1)) as usize;
        if !utils::is_valid_ascii_str(model_bytes) || model_len >= max_len {
            // SAFETY: userinfo buffer writable; literals NUL-terminated.
            unsafe {
                jampgame::info_set_value_for_key(
                    userinfo as *mut c_char,
                    c"model".as_ptr(),
                    c"kyle".as_ptr(),
                )
            };
        }
    }
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::engine_hook_original_sv_userinfo_changed);
    unsafe { original(cl) };
}

// ---------------------------------------------------------------------------
// Download validation family — entry wrappers
// ---------------------------------------------------------------------------

/// `Proxy_SV_BeginDownload_f` (`Proxy_sv_client.cpp:439-453`): only allow
/// `.pk3` files without directory traversal, and never while the client is
/// active in game.
///
/// # Safety
///
/// Entered from the engine's command execution path with a valid `client_t*`.
pub unsafe extern "C" fn sv_begin_download_f(cl: usize) {
    if unsafe { client_state(cl) } == CS_ACTIVE {
        return;
    }
    // SAFETY: Cmd_Argv valid in the command context.
    let file_name = unsafe { engine::cmd_argv(1) };
    let len = cstr_len(file_name);
    let file_bytes = cstr_bytes(file_name);
    if len < 5
        || compat::check_dir_traversal(file_bytes)
        || unsafe { jampgame::q_stricmpn(file_name.add(len - 4), c".pk3".as_ptr(), 4) } != 0
    {
        return;
    }
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::engine_hook_original_sv_begin_download_f);
    unsafe { original(cl) };
}

/// `Proxy_SV_NextDownload_f` (`Proxy_sv_client.cpp:463-470`): block while the
/// client is active.
///
/// # Safety
///
/// Entered from the engine's command execution path with a valid `client_t*`.
pub unsafe extern "C" fn sv_next_download_f(cl: usize) {
    if unsafe { client_state(cl) } == CS_ACTIVE {
        return;
    }
    let original = original_call(crate::hooks::engine_hook_original_sv_next_download_f);
    unsafe { original(cl) };
}

/// `Proxy_SV_StopDownload_f` (`Proxy_sv_client.cpp:479-486`).
///
/// # Safety
///
/// Entered from the engine's command execution path with a valid `client_t*`.
pub unsafe extern "C" fn sv_stop_download_f(cl: usize) {
    if unsafe { client_state(cl) } == CS_ACTIVE {
        return;
    }
    let original = original_call(crate::hooks::engine_hook_original_sv_stop_download_f);
    unsafe { original(cl) };
}

/// `Proxy_SV_DoneDownload_f` (`Proxy_sv_client.cpp:495-502`).
///
/// # Safety
///
/// Entered from the engine's command execution path with a valid `client_t*`.
pub unsafe extern "C" fn sv_done_download_f(cl: usize) {
    if unsafe { client_state(cl) } == CS_ACTIVE {
        return;
    }
    let original = original_call(crate::hooks::engine_hook_original_sv_done_download_f);
    unsafe { original(cl) };
}

/// `Proxy_SV_UpdateUserinfo_f` (`Proxy_sv_client.cpp:509-518`): drop empty
/// `/userinfo` calls.
///
/// # Safety
///
/// Entered from the engine's command execution path with a valid `client_t*`.
pub unsafe extern "C" fn sv_update_userinfo_f(cl: usize) {
    // SAFETY: Cmd_Argv valid in the command context.
    let arg = unsafe { engine::cmd_argv(1) };
    if arg.is_null() || unsafe { *arg } == 0 {
        return;
    }
    let original = original_call(crate::hooks::engine_hook_original_sv_update_userinfo_f);
    unsafe { original(cl) };
}

/// `Proxy_SV_WriteDownloadToClient` (`Proxy_sv_client.cpp:557-623`): refuse to
/// serve pk3 files that are not referenced by the current pure-set; everything
/// else goes to the pristine writer.
///
/// # Safety
///
/// Entered from the engine's snapshot writer with a valid `(client_t*, msg_t*)`.
pub unsafe extern "C" fn sv_write_download_to_client(cl: usize, msg: usize) {
    // Nothing being downloaded → the original returns immediately too, so
    // mirror the original proxy exactly: return without calling it.
    let dlname = cl + OFFSET_CLIENT_DOWNLOAD_NAME;
    // SAFETY: cl is a valid client_t.
    if unsafe { *(dlname as *const u8) } == 0 {
        return;
    }

    // SAFETY: cl is a valid client_t.
    let download = unsafe { read_i32(cl + OFFSET_CLIENT_DOWNLOAD) };
    if download == 0 {
        // Chop off the filename extension and check for a pk3 reference.
        let mut pakbuf = [0u8; MAX_QPATH];
        // SAFETY: dlname is a NUL-terminated MAX_QPATH string.
        let dlname_bytes = cstr_bytes(dlname as *const c_char);
        let copy = pakbuf.len().min(dlname_bytes.len());
        pakbuf[..copy].copy_from_slice(&dlname_bytes[..copy]);
        if copy < pakbuf.len() {
            pakbuf[copy] = 0;
        }

        let mut unreferenced = true;
        if let Some(dot) = pakbuf.iter().rposition(|&b| b == b'.') {
            pakbuf[dot] = 0;
            // Check for pk3 filename extension.
            // SAFETY: pakbuf is NUL-terminated and points into a valid buffer.
            let ext = pakbuf[dot + 1..].as_ptr() as *const c_char;
            if unsafe { jampgame::q_stricmp(ext, c"pk3".as_ptr()) } == 0 {
                // SAFETY: engine filesystem initialised.
                let referenced_paks = unsafe { engine::fs_referenced_pak_names() };
                // SAFETY: tokenizer writes into the engine cmd globals.
                unsafe { compat::cmd_tokenize_string2(referenced_paks, true) };
                // SAFETY: engine cmd_argc var.
                let num_ref_paks = unsafe { engine::cmd_argc() };
                let pak_str = &pakbuf[..dot];
                for curindex in 0..num_ref_paks {
                    // SAFETY: Cmd_Argv points at the tokenizer output.
                    let pak = unsafe { engine::cmd_argv_ptr(curindex as usize) };
                    let pak_bytes = cstr_bytes(pak);
                    if !compat::filename_compare(pak_bytes, pak_str) {
                        unreferenced = false;
                        break;
                    }
                }
            }
        }

        // cl->download = 0 (the original always zeroes it in this branch).
        // SAFETY: cl is a valid client_t.
        unsafe { *((cl + OFFSET_CLIENT_DOWNLOAD) as *mut c_int) = 0 };

        if unreferenced {
            let client_num = client_num_from_addr(cl) as c_int;
            let dlname_cstr = dlname as *const c_char;
            // SAFETY: engine Com_Printf is valid; downloadName NUL-terminated.
            unsafe extern "C" {
                fn jampgame_proxy_com_printf(fmt: *const c_char, ...);
            }
            unsafe {
                jampgame_proxy_com_printf(
                    c"clientDownload: %d : \"%s\" is not referenced and cannot be downloaded.\n"
                        .as_ptr(),
                    client_num,
                    dlname_cstr,
                );
            }

            let mut error_message = [0u8; 1024];
            // SAFETY: game Com_sprintf is valid; buffers writable.
            unsafe {
                jampgame::com_sprintf(
                    error_message.as_mut_ptr() as *mut c_char,
                    error_message.len() as c_int,
                    c"File \"%s\" is not referenced and cannot be downloaded.".as_ptr(),
                    dlname_cstr,
                );
            }

            // SAFETY: msg is a valid msg_t being written.
            unsafe {
                engine::msg_write_byte(msg, SVC_DOWNLOAD);
                engine::msg_write_short(msg, 0); // client is expecting block zero
                engine::msg_write_long(msg, -1); // illegal file size
                engine::msg_write_string(msg, error_message.as_ptr() as *const c_char);
            }

            // *cl->downloadName = 0
            // SAFETY: cl is a valid client_t.
            unsafe { *(dlname as *mut u8) = 0 };

            // SAFETY: cl is a valid client_t (never non-zero here, mirrors the
            // original's ordering).
            let dl = unsafe { read_i32(cl + OFFSET_CLIENT_DOWNLOAD) };
            if dl != 0 {
                // SAFETY: engine FS_FCloseFile is valid.
                unsafe { engine::fs_fclose_file(dl) };
            }
            return;
        }
    }

    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::engine_hook_original_sv_write_download_to_client);
    unsafe { original(cl, msg) };
}

// ---------------------------------------------------------------------------
// SV_SvEntityForGentity (array-math hardening) — entry wrapper
// ---------------------------------------------------------------------------

/// `Proxy_SV_SvEntityForGentity` (`Proxy_sv_game.cpp:6-27`): bounds-check the
/// entity number and index the `svEntities` array by the real gentity index
/// (game gentities carry private data after the server-shared part), else
/// `Com_Error(ERR_DROP, ...)`.
///
/// # Safety
///
/// Entered from the engine with a `sharedEntity_t*`.
pub unsafe extern "C" fn sv_sventity_for_gentity(g_ent: usize) -> usize {
    // SAFETY: engine memory layer initialised.
    let sv = unsafe { sv_ptr() };
    let mut bad = g_ent == 0;
    if !bad {
        // SAFETY: g_ent is a sharedEntity_t; s.number is its first field.
        let number = unsafe { read_i32(g_ent + OFFSET_SHARED_ENTITY_S + OFFSET_ENTITY_NUMBER) };
        bad = number < 0 || number as usize >= MAX_GENTITIES;
    }
    if bad {
        // SAFETY: game Com_Error is valid; literal format string.
        unsafe {
            com_error(ERR_DROP, c"SV_SvEntityForGentity: bad gEnt\n".as_ptr());
        }
        return 0;
    }
    // SAFETY: sv is a valid server_t.
    let gentities = unsafe { read_i32(sv + OFFSET_SV_GENTITIES) } as usize;
    let gentity_size = unsafe { read_i32(sv + OFFSET_SV_GENTITY_SIZE) } as usize;
    // Harden the array math: a crafted pointer with a plausible s.number but
    // outside the array (or a zero/negative size) must not yield an OOB
    // svEntity pointer for the engine to write through.
    if gentity_size == 0 || g_ent < gentities || (g_ent - gentities) % gentity_size != 0 {
        // SAFETY: game Com_Error is valid; literal format string.
        unsafe {
            com_error(ERR_DROP, c"SV_SvEntityForGentity: bad gEnt\n".as_ptr());
        }
        return 0;
    }
    let num = (g_ent - gentities) / gentity_size;
    if num >= MAX_GENTITIES {
        // SAFETY: game Com_Error is valid; literal format string.
        unsafe {
            com_error(ERR_DROP, c"SV_SvEntityForGentity: bad gEnt\n".as_ptr());
        }
        return 0;
    }
    sv + OFFSET_SV_SV_ENTITIES + num * SIZEOF_SV_ENTITY_T
}

/// The game module's `Com_Error(int, const char*, ...)` (offset 0x868a4),
/// called with a fixed (no-varargs) argument list.
///
/// # Safety
///
/// The game module must be loaded; `fmt` NUL-terminated.
unsafe fn com_error(level: c_int, fmt: *const c_char) {
    // SAFETY: pointer from base+offset of a loaded module; calling a variadic
    // function with exactly two args is well-defined cdecl.
    let f: unsafe extern "C" fn(c_int, *const c_char) = unsafe {
        core::mem::transmute::<usize, unsafe extern "C" fn(c_int, *const c_char)>(
            crate::original::base() + 0x0008_68a4,
        )
    };
    unsafe { f(level, fmt) };
}

// ---------------------------------------------------------------------------
// CNavigator::Load (correct shape) — entry wrapper
// ---------------------------------------------------------------------------

/// `Proxy_CNavigator::Load` (`Proxy_navigator.cpp`): free any previous nav
/// data, then run the original. Correct shape `(this, filename, checksum)
/// -> bool` (U-006/D-002.2), unlike the original proxy's mis-shaped 2-arg
/// `void` hook.
///
/// # Safety
///
/// Entered from the engine's navigator loader with `this` as the first stack
/// word and the `bool` result consumed in `%al`.
pub unsafe extern "C" fn navigator_load(
    this_: usize,
    filename: *const c_char,
    checksum: c_int,
) -> bool {
    // SAFETY: engine syscall registered (dllEntry ran first); G_NAV_FREE
    // takes no arguments.
    unsafe { syscall::call_engine(crate::sdk::G_NAV_FREE, &[]) };
    // SAFETY: trampoline attached at GAME_INIT; args forwarded unchanged.
    let original = original_call(crate::hooks::engine_hook_original_navigator_load);
    unsafe { original(this_, filename, checksum) }
}

// ---------------------------------------------------------------------------
// SV_SendClientGameState — state-guard + RMG (redesigned per D-002)
// ---------------------------------------------------------------------------

/// `Proxy_SV_SendClientGameState` — the redesigned port of the original proxy's
/// full rewrite (`Proxy_sv_client.cpp:252-395`), reduced to its two real diffs
/// against the shipped binary:
///
/// 1. the pristine body sets `client->state = CS_PRIMED` unconditionally; the
///    proxy only promotes `CS_CONNECTED -> CS_PRIMED` (download-flow /
///    gamestate fix). Implemented here as an entry wrapper: run the pristine
///    body, then restore any non-`CS_CONNECTED` state. The pristine body only
///    *reads* `client->state` for its fragment-flush loop (verified by
///    objdump: `0x804cef6`/`0x804cf2e`), so restoring after the call is
///    behavior-equivalent to the proxy's mid-body guard.
///
/// 2. the RMG terrain block is forced off (`MSG_WriteShort(&msg, 0)` always) —
///    done as the intra `je`→`jmp` patch at `crate::patch::RMG_BRANCH` (with
///    the rel32 rewritten, `jcc_to_jmp`), so no wrapper code is needed for it.
///
/// The "[ISM]" fragment-flush fix is already in the shipped binary and is not
/// re-applied.
///
/// # Safety
///
/// Entered from the engine with a valid `client_t*`.
pub unsafe extern "C" fn sv_send_client_game_state(cl: usize) {
    // SAFETY: cl is a valid client_t.
    let orig_state = unsafe { read_i32(cl + OFFSET_CLIENT_STATE) };
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::engine_hook_original_sv_send_client_game_state);
    unsafe { original(cl) };
    if orig_state != crate::sdk::CS_CONNECTED {
        // SAFETY: cl is a valid client_t.
        unsafe { *((cl + OFFSET_CLIENT_STATE) as *mut c_int) = orig_state };
    }
}

// ---------------------------------------------------------------------------
// SV_ExecuteClientMessage — upstream gate + packet-start marking (entry wrapper)
// ---------------------------------------------------------------------------

/// The shipped binary's own `"%s : dropped gamestate, resending\n"` (passed as
/// the first argument of the resend `Com_DPrintf` at 0x804e94c, objdump-verified).
const DPRINTF_RESEND_FMT: usize = 0x0819_b584;

/// The upstream (newer ioquake3) ignore message — the shipped 1.011 binary
/// does not carry this string (its gate is the older form), so the proxy
/// embeds the original proxy's text.
const DPRINTF_IGNORE_FMT: &CStr = c"%s : ignoring pre map_restart / outdated client message\n";

/// `Proxy_SV_ExecuteClientMessage` (`Proxy_sv_client.cpp:152-238`), reduced
/// per D-002.1 to an entry wrapper: the pristine body keeps all its native
/// parsing and hack guards (both already in the shipped binary,
/// objdump-verified), while the wrapper adds the two real diffs the original
/// proxy's whole-function rewrite carried:
///
/// 1. the **newer upstream serverId gate** — `strstr(lastClientCommandString,
///    "nextdl")` tolerance, the map_restart *range* check
///    (`serverId >= restartedServerId && < serverId`) and the
///    `state != CS_ACTIVE` guard on the dropped-gamestate resend (icculus
///    bug 6324). The shipped binary has the older 1.01 form of all three
///    (verified at 0x804e920-0x804e96a); this wrapper early-returns/resends
///    itself so the pristine body is never entered when the upstream gate
///    would stop the message.
/// 2. the **packet-start marking** for the netStatus per-usercmd feed: the
///    original captured the pre-loop `cmdIndex` once per `SV_UserMove` call;
///    from this wrapper (one invocation per client message, before the body
///    runs) the same per-message identity is available — exact semantics.
///
/// The header (serverId / messageAcknowledge / reliableAcknowledge + the
/// command byte) is parsed with the engine's own MSG readers and the msg
/// read state is restored before the pristine body runs, so no message
/// parsing is duplicated.
///
/// # Safety
///
/// Entered from the engine with `(client_t*, msg_t*)`.
pub unsafe extern "C" fn sv_execute_client_message(cl: usize, msg: usize) {
    // Save the msg read state; MSG_Bitstream only clears `oob`, our reads
    // advance readcount/bit, and the pristine body re-reads from the start.
    // SAFETY: cl/msg are valid engine pointers.
    let saved_readcount = unsafe { read_i32(msg + OFFSET_MSG_READCOUNT) };
    let saved_bit = unsafe { read_i32(msg + OFFSET_MSG_BIT) };

    // SAFETY: msg is a valid msg_t.
    unsafe { engine::msg_bitstream(msg) };
    // SAFETY: msg positioned at the header.
    let server_id = unsafe { engine::msg_read_long(msg) };
    let message_acknowledge = unsafe { engine::msg_read_long(msg) };
    let _reliable_acknowledge = unsafe { engine::msg_read_long(msg) };

    // 1. The upstream gate (Proxy_sv_client.cpp:192-206).
    // SAFETY: engine memory layer initialised (hooks attached at GAME_INIT).
    let sv_server_id = unsafe { read_i32(engine::vars::SV_SERVER_ID) };
    let restarted_server_id = unsafe { read_i32(engine::vars::SV_RESTARTED_SERVER_ID) };
    if server_id != sv_server_id {
        // SAFETY: cl is a valid client_t; downloadName NUL-terminated.
        let downloading = unsafe { *((cl + OFFSET_CLIENT_DOWNLOAD_NAME) as *const u8) != 0 };
        let mut tolerated_nextdl = false;
        if !downloading {
            // SAFETY: lastClientCommandString is NUL-terminated.
            let lcc = cstr_bytes((cl + OFFSET_CLIENT_LAST_CLIENT_COMMAND) as *const c_char);
            tolerated_nextdl = lcc.windows(6).any(|w| w == b"nextdl");
        }
        if !tolerated_nextdl {
            if server_id >= restarted_server_id && server_id < sv_server_id {
                // SAFETY: cl->name is a NUL-terminated string.
                unsafe {
                    engine::com_dprintf(
                        DPRINTF_IGNORE_FMT.as_ptr() as usize,
                        cl + OFFSET_CLIENT_NAME,
                    )
                };
                return;
            }
            if unsafe { read_i32(cl + OFFSET_CLIENT_STATE) } != CS_ACTIVE
                && message_acknowledge
                    > unsafe { read_i32(cl + OFFSET_CLIENT_GAMESTATE_MESSAGE_NUM) }
            {
                // SAFETY: name NUL-terminated; format from the shipped binary.
                unsafe { engine::com_dprintf(DPRINTF_RESEND_FMT, cl + OFFSET_CLIENT_NAME) };
                // SAFETY: pristine SV_SendClientGameState is callable directly.
                unsafe { engine::sv_send_client_game_state(cl) };
            }
            return;
        }
    }

    // 2. Peek the command byte; mark the packet start for usercmd messages.
    let c = unsafe { engine::msg_read_byte(msg) };
    // SAFETY: restore exactly the state the body expects.
    unsafe {
        *((msg + OFFSET_MSG_READCOUNT) as *mut c_int) = saved_readcount;
        *((msg + OFFSET_MSG_BIT) as *mut c_int) = saved_bit;
    }
    if c == CLC_MOVE || c == CLC_MOVE_NO_DELTA {
        // SAFETY: cl is a valid client_t.
        unsafe { netstatus::mark_packet_start(cl) };
    }

    // SAFETY: trampoline attached at GAME_INIT; args forwarded unchanged.
    let original = original_call(crate::hooks::engine_hook_original_sv_execute_client_message);
    unsafe { original(cl, msg) };
}

// ---------------------------------------------------------------------------
// SV_AddEntitiesVisibleFromPoint — parallel-TFFA invisibility (entry wrapper)
// ---------------------------------------------------------------------------

/// Rust-only parallel-TFFA hook: run the pristine PVS/area/broadcast logic,
/// then drop cross-match entities from the snapshot list so players neither
/// see nor hear (events ride entities) other matches' fights. `EV_OBITUARY`
/// kill feeds are broadcast temp entities whose `s.otherEntityNum` is the
/// victim, so they are hidden the same way without trap parsing. Sound temp
/// entities carry no owner of their own, so they are attributed to the match
/// that created them (the `G_Sound`/`G_TempEntity` stamp in
/// `TffaState::sound_source`, or `s.clientNum` for `EV_ENTITY_SOUND`) and
/// dropped the same way; map/world sounds and `EV_GLOBAL_SOUND` broadcasts
/// stay visible/audible everywhere.
///
/// World/static entities are always kept. Player entities (`entNum < 32`
/// with a live client) and owned entities (missiles/sabers via
/// `r.ownerNum`/`s.owner`, obituaries via `s.otherEntityNum*`) are kept only
/// within the viewer's match (lobby `None` sees lobby).
///
/// # Safety
///
/// Entered from the engine with `(origin, frame, eNums, portal)`; all three
/// pointers are valid engine snapshot structures.
pub unsafe extern "C" fn sv_add_entities_visible_from_point(
    origin: usize,
    frame: usize,
    e_nums: usize,
    portal: c_int,
) {
    // SAFETY: trampoline attached at GAME_INIT; args forwarded unchanged.
    let original =
        original_call(crate::hooks::engine_hook_original_sv_add_entities_visible_from_point);
    unsafe { original(origin, frame, e_nums, portal) };

    if !crate::tffa::enabled() {
        return;
    }
    if frame == 0 || e_nums == 0 {
        return;
    }
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);
    if g_entities == 0 {
        return;
    }
    // SAFETY: frame is a valid clientSnapshot_t; clientNum is a plain int.
    let viewer = unsafe { read_i32(frame + OFFSET_SNAPSHOT_FRAME_PS_CLIENT_NUM) };
    if !(0..MAX_CLIENTS as c_int).contains(&viewer) {
        return;
    }
    // Snapshot the match table and the sound attribution once (no per-entity
    // locking); the viewer's own match is follow-aware (spectating someone
    // adopts their instance).
    let viewer_match = crate::tffa::effective_match(viewer);
    let (table, sound_source, entity_instance) = state::with_state(|s| {
        (
            s.tffa.client_match,
            s.tffa.sound_source,
            s.tffa.entity_instance,
        )
    });
    // SAFETY: eNums is a valid snapshotEntityNumbers_t.
    let count = unsafe { read_i32(e_nums + OFFSET_SNAPSHOT_ENUMS_COUNT) };
    if count <= 0 {
        return;
    }
    let count = (count as usize).min(crate::sdk::MAX_SNAPSHOT_ENTITIES);
    let mut kept = 0usize;
    for i in 0..count {
        // SAFETY: list holds `count` ints.
        let ent_num = unsafe { read_i32(e_nums + OFFSET_SNAPSHOT_ENUMS_LIST + i * 4) };
        let keep = snapshot_keep(
            viewer_match,
            &table,
            &sound_source,
            &entity_instance,
            g_entities,
            ent_num,
        );
        if keep {
            if kept != i {
                // SAFETY: both slots are inside the eNums list.
                unsafe {
                    let v = read_i32(e_nums + OFFSET_SNAPSHOT_ENUMS_LIST + i * 4);
                    *((e_nums + OFFSET_SNAPSHOT_ENUMS_LIST + kept * 4) as *mut c_int) = v;
                }
            }
            kept += 1;
        }
    }
    // SAFETY: count slot is a plain int we own post-body.
    unsafe {
        *((e_nums + OFFSET_SNAPSHOT_ENUMS_COUNT) as *mut c_int) = kept as c_int;
    }
}

/// Snapshot predicate for one entity number (pure given the match tables).
/// Match `0` is the main TFFA; every live player belongs to exactly one.
fn snapshot_keep(
    viewer_match: u8,
    table: &[u8; MAX_CLIENTS],
    sound_source: &[u8; MAX_GENTITIES],
    entity_instance: &[u8; MAX_GENTITIES],
    g_entities: usize,
    ent_num: c_int,
) -> bool {
    if ent_num < 0 || ent_num >= MAX_GENTITIES as c_int {
        return true;
    }
    if ent_num < MAX_CLIENTS as c_int {
        // Player entity: keep only same-match. Free slots (no client) are not
        // players — keep them (they should not be listed anyway).
        let ent = g_entities + ent_num as usize * SIZEOF_GENTITY;
        // SAFETY: ent is inside the g_entities array; client is a pointer field.
        let client = unsafe { read_usize(ent + OFFSET_GENTITY_CLIENT) };
        if client == 0 {
            return true;
        }
        return crate::tffa::entity_visible(viewer_match, table[ent_num as usize]);
    }
    let ent = g_entities + ent_num as usize * SIZEOF_GENTITY;
    // SAFETY: ent is inside the g_entities array; eType/owner fields are ints.
    let etype = unsafe { read_i32(ent + OFFSET_GENTITY_S_ETYPE) };
    // Temp event entities (`G_TempEntity`: `eType = ET_EVENTS + event`) carry
    // no `r.ownerNum`/`s.owner` attribution (`G_InitGentity` sets
    // `r.ownerNum = NONE`, `s.owner` stays 0 from the freelist `memset` — a
    // bare 0 that coincides with client 0 and used to hide every same-match
    // saber effect whenever client 0 was elsewhere). Sound and predicted
    // player events are attributed first (their own sources, below); the rest
    // only via `s.otherEntityNum*`, event-specifically:
    // - `EV_SABER_BLOCK` (saber-vs-saber clash): only `otherEntityNum2`
    //   (attacker) is set, `otherEntityNum` is the untouched 0 default.
    // - `EV_SABER_CLASHFLARE`: neither is set (both default 0) — broadcast,
    //   it marks the shared hit point and carries no victim/attacker.
    // - everything else (hits, obituaries, missile marks): both fields.
    if etype >= crate::sdk::ET_EVENTS {
        let event = etype - crate::sdk::ET_EVENTS;
        // Predicted player events (`SendPendingPredictableEvents`,
        // `g_active.c:1058`): the generating player rides `s.otherEntityNum`
        // and the client plays the event as that player (`CG_CheckEvents`
        // rewrites `number`), while `s.otherEntityNum2` is
        // `ps->emplacedIndex` (turret state, not an owner). Attribute to the
        // generator alone — this covers the predicted combat sounds
        // (`EV_FIRE_WEAPON`, `EV_SABER_ATTACK`, `EV_JUMP`, ...).
        // SAFETY: ent is inside the g_entities array; eFlags is a plain int.
        let eflags = unsafe { read_i32(ent + OFFSET_GENTITY_S_EFLAGS) };
        if eflags & EF_PLAYER_EVENT != 0 {
            // SAFETY: ent is inside the g_entities array; owner fields are ints.
            let generator = unsafe { read_i32(ent + OFFSET_GENTITY_S_OTHER_ENTITY_NUM) };
            return owners_in_viewer_match(viewer_match, table, g_entities, &[generator]);
        }
        // Sound events are attributed by the emitting entity's match — the
        // creation-time stamp for `G_Sound`, `s.clientNum` for
        // `G_EntitySound`/`G_SoundOnEnt` — while deliberate broadcasts stay
        // audible everywhere.
        match crate::tffa::sound_attribution(event) {
            crate::tffa::SoundAttribution::Broadcast => return true,
            crate::tffa::SoundAttribution::Stamped => {
                return crate::tffa::sound_visible(viewer_match, sound_source[ent_num as usize]);
            }
            crate::tffa::SoundAttribution::Emitter => {
                // SAFETY: ent is inside the g_entities array; s.clientNum is
                // a plain int.
                let emitter =
                    unsafe { read_i32(ent + OFFSET_GENTITY_S + OFFSET_ENTITY_CLIENT_NUM) };
                let source = crate::tffa::source_match_of_num(g_entities, emitter, table);
                return crate::tffa::sound_visible(viewer_match, source);
            }
            crate::tffa::SoundAttribution::None => {}
        }
        if event == crate::sdk::EV_SABER_CLASHFLARE {
            return true;
        }
        // SAFETY: ent is inside the g_entities array; owner fields are ints.
        if event == crate::sdk::EV_SABER_BLOCK {
            let o2 = unsafe { read_i32(ent + OFFSET_GENTITY_S_OTHER_ENTITY_NUM2) };
            return owners_in_viewer_match(viewer_match, table, g_entities, &[o2]);
        }
        let owned = unsafe {
            [
                read_i32(ent + OFFSET_GENTITY_S_OTHER_ENTITY_NUM),
                read_i32(ent + OFFSET_GENTITY_S_OTHER_ENTITY_NUM2),
            ]
        };
        return owners_in_viewer_match(viewer_match, table, g_entities, &owned);
    }
    // Per-instance world entity (D-006 extension): a tagged door/trigger/button
    // clone is visible only inside its own instance; untagged (`ENTITY_SHARED`)
    // entities fall through to the owner attribution below, unchanged.
    if !crate::tffa::world_entity_visible(viewer_match, entity_instance[ent_num as usize]) {
        return false;
    }
    // Non-player, non-temp entity: attribute via owner fields. Any player
    // owner in a different match hides the entity; world-owned stays visible.
    //
    // `r.ownerNum` is set to `ENTITYNUM_NONE` on every spawn (`G_InitGentity`,
    // `g_utils.c:694-700`), so a `0` there really is client 0. The
    // `s.owner`/`s.otherEntityNum*` fields are not initialised that way:
    // `G_InitGame` memsets the whole array (`g_main.c:963`) and nothing
    // re-zeroes a spawned entity's `s`, so an entity the game never gave an
    // owner keeps the bare `0` default. That `0` coincides with client 0, and
    // treating it as a real owner hid every world-owned entity from each
    // viewer whose match was not client 0's — movers (doors/escalators),
    // items, NPCs. It is the same coincidence the temp-entity round fixed for
    // `EF_PLAYER_EVENT`/saber effects (D-006), missed on this path: an
    // observed drop was a mover with `r.ownerNum=NONE`, `s.owner=0`,
    // `otherEntityNum=otherEntityNum2=0`. Skip `0` here; `r.ownerNum` still
    // carries the real owner of missiles/sabers/corpses/dropped items.
    let (r_owner, s_owner, oen, oen2) = unsafe {
        (
            read_i32(ent + OFFSET_GENTITY_R_OWNER),
            read_i32(ent + OFFSET_GENTITY_S_OWNER),
            read_i32(ent + OFFSET_GENTITY_S_OTHER_ENTITY_NUM),
            read_i32(ent + OFFSET_GENTITY_S_OTHER_ENTITY_NUM2),
        )
    };
    let mut owners = [r_owner, 0, 0, 0];
    let mut count = 1usize;
    for o in [s_owner, oen, oen2] {
        if o != 0 {
            owners[count] = o;
            count += 1;
        }
    }
    owners_in_viewer_match(viewer_match, table, g_entities, &owners[..count])
}

/// Hide `ent` when any live-player owner in `owned` is outside the viewer's
/// match; world-owned (no live-player owners) stays visible.
fn owners_in_viewer_match(
    viewer_match: u8,
    table: &[u8; MAX_CLIENTS],
    g_entities: usize,
    owned: &[i32],
) -> bool {
    for &o in owned {
        if o == ENTITYNUM_NONE || o == ENTITYNUM_WORLD {
            continue;
        }
        if !(0..MAX_CLIENTS as c_int).contains(&o) {
            continue;
        }
        // Only treat it as an owner if that slot actually holds a player;
        // otherwise e.g. a generic index coinciding with a client slot would
        // wrongly hide world entities.
        let owner_ent = g_entities + o as usize * SIZEOF_GENTITY;
        // SAFETY: owner slot is inside the g_entities array.
        let owner_client = unsafe { read_usize(owner_ent + OFFSET_GENTITY_CLIENT) };
        if owner_client == 0 {
            continue;
        }
        if !crate::tffa::entity_visible(viewer_match, table[o as usize]) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tffa::SOURCE_WORLD;

    /// `entityType_t` values used by the fixtures (`bg_public.h:1189-1206`).
    const ET_ITEM: i32 = 2;
    const ET_MISSILE: i32 = 3;
    const ET_MOVER: i32 = 6;

    /// A fabricated `g_entities` array: `MAX_GENTITIES` zeroed gentity slots,
    /// exactly the state `G_InitGame`'s `memset` leaves them in (`g_main.c:963`).
    struct Game {
        mem: Vec<u8>,
    }

    impl Game {
        fn new() -> Self {
            Self {
                mem: vec![0u8; MAX_GENTITIES * SIZEOF_GENTITY],
            }
        }

        fn base(&self) -> usize {
            self.mem.as_ptr() as usize
        }

        fn put_i32(&mut self, ent: usize, off: usize, v: i32) {
            let at = ent * SIZEOF_GENTITY + off;
            self.mem[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }

        fn put_ptr(&mut self, ent: usize, off: usize, v: usize) {
            let at = ent * SIZEOF_GENTITY + off;
            self.mem[at..at + 4].copy_from_slice(&(v as u32).to_le_bytes());
        }

        /// Mark a client slot as occupied: the game preassigns `client` for
        /// every slot below `maxclients` (`g_main.c:974`), so the pointer (not
        /// a connection) is what `owners_in_viewer_match` tests.
        fn with_player(&mut self, ent: usize) {
            self.put_ptr(ent, OFFSET_GENTITY_CLIENT, 0x1000_0000 + ent);
        }

        /// A non-player entity created by `G_Spawn`/`G_InitGentity`: the given
        /// `eType`, `r.ownerNum = ENTITYNUM_NONE`, and the `s.owner` /
        /// `s.otherEntityNum*` memset defaults (`0`) untouched — the state of
        /// a map mover, item or NPC.
        fn with_entity(&mut self, ent: usize, etype: i32) {
            self.put_i32(ent, OFFSET_GENTITY_S_ETYPE, etype);
            self.put_i32(ent, OFFSET_GENTITY_R_OWNER, ENTITYNUM_NONE);
        }
    }

    /// A player in slot 0 (main) and a second instance holding the viewer.
    fn main_player_and_viewer() -> (Game, [u8; MAX_CLIENTS]) {
        let mut game = Game::new();
        game.with_player(0);
        let table = [0u8; MAX_CLIENTS];
        (game, table)
    }

    fn sound_table() -> [u8; MAX_GENTITIES] {
        [SOURCE_WORLD; MAX_GENTITIES]
    }

    /// No entity is instance-tagged (the state before any clone pass runs).
    fn shared_table() -> [u8; MAX_GENTITIES] {
        [crate::tffa::ENTITY_SHARED; MAX_GENTITIES]
    }

    /// Doors/escalators (and items, NPCs) keep the `s.*` memset defaults; a
    /// viewer outside client 0's match must still see them (the reported bug:
    /// "people in TFFA instances cannot see escalators/doors").
    #[test]
    fn world_owned_entity_is_kept_for_another_instance() {
        for etype in [ET_MOVER, ET_ITEM] {
            let (game, table) = main_player_and_viewer();
            let mut game = game;
            game.with_entity(48, etype);
            assert!(
                snapshot_keep(1, &table, &sound_table(), &shared_table(), game.base(), 48),
                "eType {etype} with default owner fields must stay visible"
            );
            // ... and stays visible for a same-instance viewer too.
            assert!(snapshot_keep(
                0,
                &table,
                &sound_table(),
                &shared_table(),
                game.base(),
                48
            ));
        }
    }

    /// A bare `0` in `s.owner`/`s.otherEntityNum*` is the memset default, not a
    /// reference to client 0 (the regression this test pins).
    #[test]
    fn zero_s_owner_fields_are_not_client_zero() {
        let mut game = Game::new();
        game.with_player(0);
        game.with_entity(48, ET_MOVER);
        game.put_i32(48, OFFSET_GENTITY_S_OWNER, 0);
        game.put_i32(48, OFFSET_GENTITY_S_OTHER_ENTITY_NUM, 0);
        game.put_i32(48, OFFSET_GENTITY_S_OTHER_ENTITY_NUM2, 0);
        let table = [0u8; MAX_CLIENTS];
        assert!(snapshot_keep(
            1,
            &table,
            &sound_table(),
            &shared_table(),
            game.base(),
            48
        ));
    }

    /// `r.ownerNum` has a real sentinel, so owner `0` there is client 0 and a
    /// player-owned entity follows its owner's match.
    #[test]
    fn r_owner_num_follows_its_owner_match() {
        let (mut game, mut table) = main_player_and_viewer();
        game.with_entity(48, ET_MISSILE);
        game.put_i32(48, OFFSET_GENTITY_R_OWNER, 0);
        assert!(
            !snapshot_keep(1, &table, &sound_table(), &shared_table(), game.base(), 48),
            "client 0's missile must be hidden from instance 1"
        );
        assert!(snapshot_keep(
            0,
            &table,
            &sound_table(),
            &shared_table(),
            game.base(),
            48
        ));
        // Same entity owned by an instance-1 player instead.
        game.with_player(5);
        table[5] = 1;
        game.put_i32(48, OFFSET_GENTITY_R_OWNER, 5);
        assert!(snapshot_keep(
            1,
            &table,
            &sound_table(),
            &shared_table(),
            game.base(),
            48
        ));
        assert!(!snapshot_keep(
            0,
            &table,
            &sound_table(),
            &shared_table(),
            game.base(),
            48
        ));
    }

    /// `s.owner` still attributes player-owned entities the game does set it
    /// on (deployed sentries/shields/e-webs), as long as it is not the `0`
    /// default.
    #[test]
    fn non_zero_s_owner_follows_its_owner_match() {
        let (mut game, mut table) = main_player_and_viewer();
        game.with_player(5);
        table[5] = 1;
        game.with_entity(48, ET_ITEM);
        game.put_i32(48, OFFSET_GENTITY_S_OWNER, 5);
        assert!(!snapshot_keep(
            0,
            &table,
            &sound_table(),
            &shared_table(),
            game.base(),
            48
        ));
        assert!(snapshot_keep(
            1,
            &table,
            &sound_table(),
            &shared_table(),
            game.base(),
            48
        ));
    }

    /// Player entities follow their own match (unchanged behaviour).
    #[test]
    fn player_entity_follows_its_match() {
        let (mut game, mut table) = main_player_and_viewer();
        game.with_player(5);
        table[5] = 1;
        assert!(!snapshot_keep(
            1,
            &table,
            &sound_table(),
            &shared_table(),
            game.base(),
            0
        ));
        assert!(snapshot_keep(
            1,
            &table,
            &sound_table(),
            &shared_table(),
            game.base(),
            5
        ));
        assert!(!snapshot_keep(
            0,
            &table,
            &sound_table(),
            &shared_table(),
            game.base(),
            5
        ));
    }

    /// `EV_PREDEFSOUND` (`G_PreDefSound`: force jump/protect/absorb) carries no
    /// owner fields, so its only attribution is the construction-time stamp.
    /// A stamp naming another match must hide it; a world stamp stays audible
    /// everywhere.
    #[test]
    fn owner_less_predef_sound_follows_its_construction_stamp() {
        let (game, table) = main_player_and_viewer();
        let mut game = game;
        let etype = crate::sdk::ET_EVENTS + crate::sdk::EV_PREDEFSOUND;
        game.with_entity(48, etype);
        let mut sounds = sound_table();
        sounds[48] = 1;
        assert!(snapshot_keep(
            1,
            &table,
            &sounds,
            &shared_table(),
            game.base(),
            48
        ));
        assert!(
            !snapshot_keep(0, &table, &sounds, &shared_table(), game.base(), 48),
            "a force-jump sound stamped for instance 1 must be silent in main"
        );
        // World attribution (no scope was open when it was built): unchanged
        // map/world behaviour, audible in every instance.
        for viewer_match in [0, 1, 2] {
            assert!(snapshot_keep(
                viewer_match,
                &table,
                &sound_table(),
                &shared_table(),
                game.base(),
                48
            ));
        }
    }

    /// Per-instance world entities (D-006 extension): a tagged door/trigger is
    /// visible only inside its own instance, and tagging an entity is inert for
    /// every other instance (they keep the untagged behaviour).
    #[test]
    fn tagged_world_entity_visible_only_in_its_instance() {
        let (game, table) = main_player_and_viewer();
        let mut game = game;
        game.with_entity(48, ET_MOVER);
        let mut tags = shared_table();
        tags[48] = 1; // instance 1's door
        assert!(snapshot_keep(
            1,
            &table,
            &sound_table(),
            &tags,
            game.base(),
            48
        ));
        assert!(!snapshot_keep(
            0,
            &table,
            &sound_table(),
            &tags,
            game.base(),
            48
        ));
        assert!(!snapshot_keep(
            2,
            &table,
            &sound_table(),
            &tags,
            game.base(),
            48
        ));
        // Untagged (the state before any clone pass): visible everywhere.
        for viewer_match in [0, 1, 2] {
            assert!(snapshot_keep(
                viewer_match,
                &table,
                &sound_table(),
                &shared_table(),
                game.base(),
                48
            ));
        }
    }
}
