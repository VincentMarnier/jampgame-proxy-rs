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
    CLC_MOVE, CLC_MOVE_NO_DELTA, CS_ACTIVE, ERR_DROP, MAX_GENTITIES, MAX_QPATH,
    OFFSET_CLIENT_DOWNLOAD, OFFSET_CLIENT_DOWNLOAD_NAME, OFFSET_CLIENT_GAMESTATE_MESSAGE_NUM,
    OFFSET_CLIENT_LAST_CLIENT_COMMAND, OFFSET_CLIENT_NAME, OFFSET_CLIENT_PING, OFFSET_CLIENT_STATE,
    OFFSET_CLIENT_USERINFO, OFFSET_ENTITY_NUMBER, OFFSET_MSG_BIT, OFFSET_MSG_READCOUNT,
    OFFSET_SHARED_ENTITY_S, OFFSET_SV_GENTITIES, OFFSET_SV_GENTITY_SIZE, OFFSET_SV_SV_ENTITIES,
    SIZEOF_CLIENT_T, SIZEOF_SV_ENTITY_T, SVC_DOWNLOAD,
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

#[allow(dead_code)] // guard for the wrappers; consumed by future debugging paths
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
    if cooldown_enabled && now < RCON_COOLDOWN.load(Ordering::Relaxed) + 500 {
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
        let max_len = state::with_state(|s| s.cvars[CVAR_MODEL_PATH_LENGTH].integer) as usize;
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
    let num = g_ent.wrapping_sub(gentities) / gentity_size;
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
