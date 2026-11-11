//! Engine hook wrappers (`Patches/server/Proxy_sv_*.cpp`) — the D-002
//! wanted/kept set as minimal-alteration detours: entry wrappers over pristine
//! bodies via the trampolines in `crate::patch`, plus the `ipAuthorize`/RMG/
//! `Com_Printf` intra injections (see `crate::hooks::attach_all`).

use core::ffi::{CStr, c_char, c_int};
use core::sync::atomic::{AtomicBool, Ordering};

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
    OFFSET_MSG_BIT, OFFSET_MSG_CURSIZE, OFFSET_MSG_OOB, OFFSET_MSG_READCOUNT,
    OFFSET_NETCHAN_REMOTE_ADDRESS, OFFSET_SHARED_ENTITY_S, OFFSET_SNAPSHOT_ENUMS_COUNT,
    OFFSET_SNAPSHOT_ENUMS_LIST, OFFSET_SNAPSHOT_FRAME_PS_CLIENT_NUM, OFFSET_SV_GENTITIES,
    OFFSET_SV_GENTITY_SIZE, OFFSET_SV_SV_ENTITIES, OFFSET_SVS_TIME, SIZEOF_CLIENT_T,
    SIZEOF_GENTITY, SIZEOF_SV_ENTITY_T, SVC_DOWNLOAD,
};
use crate::state::{self, CVAR_MODEL_PATH_LENGTH};
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

/// `netadr_t.type` for a loopback address (`NA_LOOPBACK`).
const NA_LOOPBACK: i32 = 2;

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
    unsafe { read_i32(engine::vars::SVS + OFFSET_SVS_TIME) }
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

/// Raw 20-byte `netadr_t` of `svs.clients[slot].netchan.remoteAddress`, used by
/// the vote-slot check (S14).
///
/// # Safety
///
/// Engine memory layer initialised and `slot < sv_maxclients`: the engine
/// allocates `svs.clients` for `sv_maxclients` entries only.
pub unsafe fn client_address_bytes(slot: usize) -> [u8; 20] {
    // SAFETY: engine memory layer initialised (hooks only run after GAME_INIT).
    let base = unsafe { clients_base() };
    if base == 0 || slot >= MAX_CLIENTS {
        return [0u8; 20];
    }
    let addr = base + slot * SIZEOF_CLIENT_T + OFFSET_NETCHAN_REMOTE_ADDRESS;
    // SAFETY: the caller guarantees `slot < sv_maxclients`, so the 20 bytes
    // of `netchan.remoteAddress` are inside the clients array.
    unsafe { core::ptr::read_unaligned(addr as *const [u8; 20]) }
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

/// The engine's client slot index for a `client_t*`, or `None` when the
/// pointer is not one of `svs->clients` (never prints: this runs from hot
/// diagnostic paths).
pub fn client_number(cl: usize) -> Option<usize> {
    // SAFETY: engine memory layer initialised (hooks only run after GAME_INIT).
    let base = unsafe { clients_base() };
    if base == 0 {
        return None;
    }
    let num = cl.wrapping_sub(base) / SIZEOF_CLIENT_T;
    (num < crate::sdk::MAX_CLIENTS).then_some(num)
}

/// `NET_AdrToString`-compatible rendering of a `netadr_t`, without the
/// engine's shared static buffer.
pub fn netadr_string(adr: &Netadr) -> String {
    match adr.kind {
        0 => "bot".to_owned(),       // NA_BOT
        1 => "BAD".to_owned(),       // NA_BAD
        2 => "loopback".to_owned(),  // NA_LOOPBACK
        3 => "broadcast".to_owned(), // NA_BROADCAST
        4 => format!(
            "{}.{}.{}.{}:{}",
            adr.ip[0],
            adr.ip[1],
            adr.ip[2],
            adr.ip[3],
            u16::from_be(adr.port)
        ),
        other => format!(
            "type{}:{:02x}{:02x}{:02x}{:02x}.{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}:{}",
            other,
            adr.ipx[0],
            adr.ipx[1],
            adr.ipx[2],
            adr.ipx[3],
            adr.ipx[4],
            adr.ipx[5],
            adr.ipx[6],
            adr.ipx[7],
            adr.ipx[8],
            adr.ipx[9],
            u16::from_be(adr.port)
        ),
    }
}

/// One-line client identity for diagnostic logs: slot, engine name and remote
/// address. Reads engine memory only (safe from any hook), never proxy state.
///
/// # Safety
///
/// `cl` must be a valid `client_t*` while the engine is running.
pub unsafe fn client_label(cl: usize) -> String {
    let number = client_number(cl)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".to_owned());
    // SAFETY: cl is a valid client_t; name/userinfo are NUL-terminated.
    let name = cstr_bytes((cl + OFFSET_CLIENT_NAME) as *const c_char);
    // SAFETY: cl is a valid client_t; remoteAddress is a plain netadr_t.
    let adr =
        unsafe { core::ptr::read_unaligned((cl + OFFSET_NETCHAN_REMOTE_ADDRESS) as *const Netadr) };
    format!(
        "client={number} name=\"{}\" adr=\"{}\"",
        crate::diagnostics::escape_bytes(name, 128),
        netadr_string(&adr)
    )
}

/// [`client_label`] for a client slot index (used by the game-module vmMain
/// event handlers, which only receive the number).
pub fn client_label_for_num(client_num: c_int) -> String {
    if !(0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num) {
        return format!("client={client_num} (out of range)");
    }
    // SAFETY: engine memory layer initialised; the slot index is in range.
    let base = unsafe { clients_base() };
    if base == 0 {
        return format!("client={client_num}");
    }
    // SAFETY: base points at svs->clients, slot index in range.
    unsafe { client_label(base + client_num as usize * SIZEOF_CLIENT_T) }
}

/// Join `Cmd_Argv(start..argc)` for a log line, escaping each token.
pub fn join_cmd_args(start: c_int) -> String {
    let argc = unsafe { engine::cmd_argc() }.clamp(0, 64);
    let mut out = String::new();
    for i in start..argc {
        // SAFETY: Cmd_Argv is valid in the command context; NUL-terminated.
        let arg = unsafe { engine::cmd_argv(i) };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&crate::diagnostics::escape_bytes(cstr_bytes(arg), 256));
    }
    out
}

// ---------------------------------------------------------------------------
// SVC_RemoteCommand (rcon) — entry wrapper (D-002 "rcon" row)
// ---------------------------------------------------------------------------

/// `Proxy_SVC_RemoteCommand` (10-25 F6): the rcon policy and the `writeconfig`
/// `.cfg`-only check, then the original.
///
/// A valid password is never limited, so an admin keeps rcon during a flood
/// that spoofs their own address. A bad password shares one global bucket
/// `(10, 1000)`, about one guess per second whatever the source addresses; a
/// dropped request gets no reply.
///
/// # Safety
///
/// Entered from the engine's connectionless dispatcher with `(netadr_t, msg_t*)`.
pub unsafe extern "C" fn svc_remote_command(from: Netadr, msg: usize) {
    // SAFETY: Cmd_Argv is valid in the connectionless command context and
    // NUL-terminated; the cvar slot is a live `cvar_t*`.
    let valid = unsafe { rcon_password_matches(engine::cmd_argv(1)) };

    // Log every rcon attempt with the password redacted: only the match
    // result is recorded.
    if crate::diagnostics::suspicious_enabled() {
        crate::diagnostics::suspicious(
            "rcon",
            format!(
                "from=\"{}\" valid={} command=\"{}\"",
                netadr_string(&from),
                valid,
                join_cmd_args(2)
            ),
        );
    }

    if !crate::ratelimit::rcon_allowed(valid) {
        return; // dropped silently
    }

    // The `writeconfig` policy now lives in the command itself
    // (`com_write_config_f`), so `vstr`/`exec` and the console are covered
    // too.

    // SAFETY: trampoline attached at GAME_INIT; args forwarded unchanged.
    let original = original_call(crate::hooks::engine_hook_original_svc_remote_command);
    unsafe { original(from, msg) };
}

/// Whether `supplied` matches the engine's current `sv_rconpassword`.
///
/// # Safety
///
/// Called in the connectionless command context with engine memory
/// initialised; `supplied` is NUL-terminated.
unsafe fn rcon_password_matches(supplied: *const c_char) -> bool {
    // SAFETY: validated cvar-pointer slot (R-018), dereferenced by the memory
    // layer at GAME_INIT.
    let cvar_ptr = unsafe { *(engine::cvar_slots::SV_RCONPASSWORD as *const usize) };
    if cvar_ptr == 0 {
        return false;
    }
    // SAFETY: cvar_ptr is a live cvar_t*; string is a NUL-terminated char*.
    let expected = unsafe { engine::cvar_string_ptr(cvar_ptr) };
    if expected.is_null() {
        return false;
    }
    // SAFETY: engine cvar strings are NUL-terminated for the cvar's lifetime.
    let expected = unsafe { core::ffi::CStr::from_ptr(expected as *const c_char) }.to_bytes();
    !expected.is_empty() && expected == cstr_bytes(supplied)
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
    let len = cmd_argv_len(1);
    if len > 128 {
        if crate::diagnostics::suspicious_enabled() {
            crate::diagnostics::suspicious(
                "query.oversize",
                format!(
                    "kind=getstatus from=\"{}\" len={len} arg=\"{}\"",
                    netadr_string(&from),
                    // SAFETY: Cmd_Argv is NUL-terminated in this context.
                    crate::diagnostics::escape_bytes(
                        cstr_bytes(unsafe { engine::cmd_argv(1) }),
                        256
                    )
                ),
            );
        }
        return;
    }
    // Per address (10, 1000), then the global outbound (10, 100): a reflection
    // attack is aimed at one victim address, and the total is capped.
    if !crate::ratelimit::status_allowed(&from, "getstatus") {
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
    let len = cmd_argv_len(1);
    if len > 128 {
        if crate::diagnostics::suspicious_enabled() {
            crate::diagnostics::suspicious(
                "query.oversize",
                format!(
                    "kind=getinfo from=\"{}\" len={len} arg=\"{}\"",
                    netadr_string(&from),
                    // SAFETY: Cmd_Argv is NUL-terminated in this context.
                    crate::diagnostics::escape_bytes(
                        cstr_bytes(unsafe { engine::cmd_argv(1) }),
                        256
                    )
                ),
            );
        }
        return;
    }
    // Same policy as getstatus.
    if !crate::ratelimit::status_allowed(&from, "getinfo") {
        return;
    }
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::engine_hook_original_svc_info);
    unsafe { original(from) };
}

// ---------------------------------------------------------------------------
// SV_GetChallenge / SV_DirectConnect — per-address rate limit (10-25 F6/O2)
// ---------------------------------------------------------------------------

/// `SV_GetChallenge(netadr_t)`: per-address limit, then a **stateless**
/// challenge answer (S18). The original engine handler is never called, so
/// nothing is stored in `svs.challenges` at this point.
///
/// # Safety
///
/// Entered from `SV_ConnectionlessPacket` with a `netadr_t` by value.
pub unsafe extern "C" fn sv_get_challenge(from: Netadr) {
    if !crate::ratelimit::address_allowed(&from, "getchallenge") {
        return;
    }
    crate::challenge::answer(&from);
}

/// `SV_DirectConnect(netadr_t)`: per-address limit, then validate the stateless
/// challenge and write its entry into `svs.challenges` for the original, which
/// then receives the call unchanged.
///
/// # Safety
///
/// Entered from `SV_ConnectionlessPacket` with a `netadr_t` by value.
pub unsafe extern "C" fn sv_direct_connect(from: Netadr) {
    if !crate::ratelimit::address_allowed(&from, "connect") {
        return;
    }
    crate::challenge::admit(&from);
    // SAFETY: trampoline attached at GAME_INIT; the argument is unchanged.
    let original = original_call(crate::hooks::engine_hook_original_sv_direct_connect);
    unsafe { original(from) };
}

// ---------------------------------------------------------------------------
// Info_Print (dumpuser / serverinfo / systeminfo) — full rewrite (S7/F11)
// ---------------------------------------------------------------------------

/// Format an info string the way the retail `Info_Print` does: one key per
/// entry, padded with spaces to 20 characters when shorter (no newline), then
/// the value (or `None` for a missing value). Growable buffers instead of the
/// retail `char key[512]` / `char value[512]` (S7/10-25 F11).
fn info_print_lines(bytes: &[u8]) -> Vec<(Vec<u8>, Option<Vec<u8>>)> {
    let mut out = Vec::new();
    let mut i = 0;
    if bytes.first() == Some(&b'\\') {
        i += 1;
    }
    while i < bytes.len() {
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'\\' {
            i += 1;
        }
        let mut key = bytes[key_start..i].to_vec();
        if key.len() < 20 {
            key.resize(20, b' ');
        }

        if i >= bytes.len() {
            out.push((key, None));
            break;
        }

        i += 1;
        let value_start = i;
        while i < bytes.len() && bytes[i] != b'\\' {
            i += 1;
        }
        out.push((key, Some(bytes[value_start..i].to_vec())));

        if i < bytes.len() {
            i += 1;
        }
    }
    out
}

/// `void Com_Printf(const char *fmt, ...)` with one `%s` argument.
///
/// # Safety
///
/// Engine memory layer initialised; both strings NUL-terminated.
unsafe fn com_printf(format: *const c_char, text: *const c_char) {
    // SAFETY: fixed engine address; a cdecl call with `(const char*, const
    // char*)` is a valid use of the variadic function for a `%s` format.
    let f: unsafe extern "C" fn(*const c_char, *const c_char) = engine::engine_fn!(
        unsafe extern "C" fn(*const c_char, *const c_char),
        engine::functions::COM_PRINTF
    );
    unsafe { f(format, text) };
}

/// Print a proxy-formatted line through the engine's `Com_Printf`.
fn print_text(text: &str) {
    let Ok(text) = std::ffi::CString::new(text) else {
        return;
    };
    // SAFETY: NUL-terminated string; engine printf valid.
    unsafe { com_printf(c"%s".as_ptr(), text.as_ptr()) };
}

/// `Com_WriteConfig_f` (S12/10-25 F7): only a `.cfg` name, no directory
/// traversal, and not the reserved default configs, then
/// `Com_WriteConfigToFile`. The original is never called. The check lives in
/// the command itself, so `vstr`, `exec` and the dedicated console cannot
/// bypass it.
///
/// # Safety
///
/// Entered from `Cmd_ExecuteString` with the command arguments in the engine
/// `Cmd_Argc`/`Cmd_Argv` globals.
pub unsafe extern "C" fn com_write_config_f() {
    if unsafe { engine::cmd_argc() } != 2 {
        print_text("Usage: writeconfig <filename>\n");
        return;
    }

    let mut filename = [0u8; MAX_QPATH];
    // SAFETY: Cmd_Argv is valid in the command context and NUL-terminated.
    let arg = unsafe { engine::cmd_argv(1) };
    let bytes = if arg.is_null() {
        &b""[..]
    } else {
        // SAFETY: Cmd_Argv returns a NUL-terminated string.
        unsafe { CStr::from_ptr(arg) }.to_bytes()
    };
    utils::strncpyz(&mut filename, bytes);
    crate::hooks::compat::default_extension(&mut filename, b".cfg");

    let len = utils::c_strlen(&filename);
    let name = &filename[..len];

    if !crate::hooks::compat::compare_extension(name, b".cfg") {
        print_text(
            "Com_WriteConfig_f: Only the \".cfg\" extension is supported by this command!\n",
        );
        return;
    }
    if crate::hooks::compat::check_dir_traversal(name) {
        print_text(&format!(
            "Com_WriteConfig_f: The filename \"{}\" must not contain \"..\"!\n",
            String::from_utf8_lossy(name)
        ));
        return;
    }
    if !crate::hooks::compat::filename_compare(name, b"mpdefault.cfg")
        || !crate::hooks::compat::filename_compare(name, b"default.cfg")
    {
        print_text(&format!(
            "^3Com_WriteConfig_f: The filename \"{}\" is reserved! Please choose another name.\n",
            String::from_utf8_lossy(name)
        ));
        return;
    }

    print_text(&format!("Writing {}.\n", String::from_utf8_lossy(name)));
    // SAFETY: fixed engine address, cdecl `(const char*)`.
    let f: unsafe extern "C" fn(*const c_char) = engine::engine_fn!(
        unsafe extern "C" fn(*const c_char),
        engine::functions::COM_WRITE_CONFIG_TO_FILE
    );
    unsafe { f(filename.as_ptr() as *const c_char) };
}

/// `Info_Print` (S7/10-25 F11): the retail function copies each key and value
/// into a 512-byte stack buffer, so a userinfo value of 512 or more characters
/// overflows it — a client plants the value and `rcon dumpuser` kills the
/// server. This rewrite uses growable buffers; the original body is never
/// called. The output format is the retail one (`"%s"` for the padded key,
/// then the value and a newline, or `MISSING VALUE`).
///
/// # Safety
///
/// Entered from the engine's `serverinfo`, `systeminfo` and `dumpuser`
/// commands with a NUL-terminated info string.
pub unsafe extern "C" fn info_print(s: *const c_char) {
    if s.is_null() {
        return;
    }
    // SAFETY: the engine passes a NUL-terminated info string.
    let bytes = unsafe { CStr::from_ptr(s) }.to_bytes();

    for (mut key, value) in info_print_lines(bytes) {
        key.push(0);
        // SAFETY: `key` is NUL-terminated; engine printf valid.
        unsafe { com_printf(c"%s".as_ptr(), key.as_ptr() as *const c_char) };

        match value {
            None => {
                // SAFETY: static NUL-terminated string.
                unsafe { com_printf(c"%s".as_ptr(), c"MISSING VALUE\n".as_ptr()) };
            }
            Some(mut value) => {
                value.push(b'\n');
                value.push(0);
                // SAFETY: `value` is NUL-terminated; engine printf valid.
                unsafe { com_printf(c"%s".as_ptr(), value.as_ptr() as *const c_char) };
            }
        }
    }
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

/// S9 (10-25 O1): the game's IP-ban check reads the `ip` userinfo key, which a
/// client can replace with any `userinfo` command. Re-derive it from the real
/// address before the game sees the userinfo. Returns `false` when the client
/// was dropped because the key does not fit.
///
/// # Safety
///
/// `cl` must be a valid `client_t*`.
unsafe fn refresh_ip(cl: usize) -> bool {
    let userinfo = cl + OFFSET_CLIENT_USERINFO;
    // SAFETY: cl is a valid client_t; netchan.remoteAddress is a netadr_t.
    let from =
        unsafe { core::ptr::read_unaligned((cl + OFFSET_NETCHAN_REMOTE_ADDRESS) as *const Netadr) };

    let ip = if from.kind == NA_LOOPBACK {
        // What the engines use for a local client (the engine's own
        // SV_DirectConnect uses "localhost" too).
        b"localhost".to_vec()
    } else {
        // `const char *NET_AdrToString(netadr_t)` (`0x807b314`) returns a
        // static buffer; copy it before any other engine call.
        // SAFETY: fixed engine address, cdecl `(netadr_t)`.
        let f: unsafe extern "C" fn(Netadr) -> *const c_char = engine::engine_fn!(
            unsafe extern "C" fn(Netadr) -> *const c_char,
            engine::functions::NET_ADR_TO_STRING
        );
        let text = unsafe { f(from) };
        if text.is_null() {
            return true;
        }
        // SAFETY: the engine's static string is NUL-terminated.
        unsafe { CStr::from_ptr(text) }.to_bytes().to_vec()
    };

    // SAFETY: cl->userinfo is a char[MAX_INFO_STRING] inside client_t.
    let buffer = unsafe {
        core::slice::from_raw_parts_mut(userinfo as *mut u8, crate::sdk::MAX_INFO_STRING)
    };
    if crate::userinfo::set_value_for_key(buffer, b"ip", &ip) {
        return true;
    }

    // No room left: the fake or absent value would stay. Drop the client, as
    // ioq3/OpenJK do (10-25 O1).
    // SAFETY: cl is a valid client_t; literal reason.
    unsafe { sv_drop_client(cl, c"userinfo string length exceeded") };
    false
}

/// `Proxy_SV_UserinfoChanged` (`Proxy_sv_client.cpp:404-424`): force a valid
/// model into `cl->userinfo` before the game's `ClientUserinfoChanged` runs
/// (connection-time model-length / non-ASCII crash fix). Also re-derives `ip`
/// (S9) before anything else.
///
/// # Safety
///
/// Entered from the engine with a valid `client_t*`.
pub unsafe extern "C" fn sv_userinfo_changed(cl: usize) {
    let userinfo = cl + OFFSET_CLIENT_USERINFO;
    if crate::diagnostics::verbose_enabled() {
        // SAFETY: cl is a valid client_t; userinfo is NUL-terminated.
        let info = cstr_bytes(userinfo as *const c_char);
        crate::diagnostics::verbose(
            "userinfo",
            format!(
                "{} len={} userinfo=\"{}\"",
                // SAFETY: cl is a valid client_t.
                unsafe { client_label(cl) },
                info.len(),
                crate::diagnostics::escape_bytes(info, 512)
            ),
        );
    }
    // S9: before the model fix and the original, so the game never reads a
    // client-supplied `ip`.
    // SAFETY: cl is a valid client_t.
    if !unsafe { refresh_ip(cl) } {
        return;
    }
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
            if crate::diagnostics::suspicious_enabled() {
                crate::diagnostics::suspicious(
                    "userinfo.model",
                    format!(
                        "{} len={} model=\"{}\" forced=\"kyle\"",
                        // SAFETY: cl is a valid client_t.
                        unsafe { client_label(cl) },
                        model_len,
                        crate::diagnostics::escape_bytes(model_bytes, 256)
                    ),
                );
            }
            // `Info_SetValueForKey` adds nothing when the userinfo has no
            // room left, so the long model would stay. The checked Rust copy
            // reports the failure; drop the client, as JK2MV and the 2026
            // proxy do (O8).
            // SAFETY: cl->userinfo is a char[MAX_INFO_STRING] inside client_t.
            let buffer = unsafe {
                core::slice::from_raw_parts_mut(userinfo as *mut u8, crate::sdk::MAX_INFO_STRING)
            };
            if !crate::userinfo::set_value_for_key(buffer, b"model", b"kyle") {
                // SAFETY: cl is a valid client_t; literal reason.
                unsafe { sv_drop_client(cl, c"userinfo string length exceeded") };
                return;
            }
        }
    }
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::engine_hook_original_sv_userinfo_changed);
    unsafe { original(cl) };
}

/// `SV_DropClient(client_t*, const char*)` (`sv_client.cpp:686`). The engine
/// function is not hooked, so it is called by address.
///
/// # Safety
///
/// `cl` must be a valid `client_t*` and `reason` a NUL-terminated string.
unsafe fn sv_drop_client(cl: usize, reason: &CStr) {
    // SAFETY: fixed engine function address, cdecl `(client_t*, const char*)`.
    let f: unsafe extern "C" fn(usize, *const c_char) = crate::engine::engine_fn!(
        unsafe extern "C" fn(usize, *const c_char),
        crate::engine::functions::SV_DROP_CLIENT
    );
    unsafe { f(cl, reason.as_ptr()) };
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
        if crate::diagnostics::suspicious_enabled() {
            crate::diagnostics::suspicious(
                "download.block",
                format!(
                    "{} reason=\"player active\" file=\"{}\"",
                    // SAFETY: cl is a valid client_t.
                    unsafe { client_label(cl) },
                    // SAFETY: Cmd_Argv valid in the command context.
                    crate::diagnostics::escape_bytes(
                        cstr_bytes(unsafe { engine::cmd_argv(1) }),
                        256
                    )
                ),
            );
        }
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
        if crate::diagnostics::suspicious_enabled() {
            let reason = if compat::check_dir_traversal(file_bytes) {
                "directory traversal"
            } else if len < 5 {
                "short name"
            } else {
                "not .pk3"
            };
            crate::diagnostics::suspicious(
                "download.block",
                format!(
                    "{} reason=\"{}\" file=\"{}\"",
                    // SAFETY: cl is a valid client_t.
                    unsafe { client_label(cl) },
                    reason,
                    crate::diagnostics::escape_bytes(file_bytes, 256)
                ),
            );
        }
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
        if crate::diagnostics::suspicious_enabled() {
            // SAFETY: cl is a valid client_t.
            crate::diagnostics::suspicious(
                "download.block",
                format!("{} reason=\"player active\" command=nextdl", unsafe {
                    client_label(cl)
                }),
            );
        }
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
        if crate::diagnostics::suspicious_enabled() {
            // SAFETY: cl is a valid client_t.
            crate::diagnostics::suspicious(
                "download.block",
                format!("{} reason=\"player active\" command=stopdl", unsafe {
                    client_label(cl)
                }),
            );
        }
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
        if crate::diagnostics::suspicious_enabled() {
            // SAFETY: cl is a valid client_t.
            crate::diagnostics::suspicious(
                "download.block",
                format!("{} reason=\"player active\" command=donedl", unsafe {
                    client_label(cl)
                }),
            );
        }
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
        if crate::diagnostics::suspicious_enabled() {
            // SAFETY: cl is a valid client_t.
            crate::diagnostics::suspicious(
                "userinfo.empty",
                format!("{} command=userinfo", unsafe { client_label(cl) }),
            );
        }
        return;
    }
    if crate::diagnostics::verbose_enabled() {
        let bytes = cstr_bytes(arg);
        crate::diagnostics::verbose(
            "userinfo.command",
            format!(
                "{} len={} userinfo=\"{}\"",
                // SAFETY: cl is a valid client_t.
                unsafe { client_label(cl) },
                bytes.len(),
                crate::diagnostics::escape_bytes(bytes, 512)
            ),
        );
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
            if crate::diagnostics::suspicious_enabled() {
                crate::diagnostics::suspicious(
                    "download.block",
                    format!(
                        "{} reason=\"not in pure set\" file=\"{}\"",
                        // SAFETY: cl is a valid client_t.
                        unsafe { client_label(cl) },
                        crate::diagnostics::escape_bytes(cstr_bytes(dlname as *const c_char), 256)
                    ),
                );
            }
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
        if crate::diagnostics::suspicious_enabled() {
            crate::diagnostics::suspicious(
                "sventity.bad",
                format!("gentity={g_ent:#x} reason=\"bad entity number\""),
            );
        }
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
        if crate::diagnostics::suspicious_enabled() {
            crate::diagnostics::suspicious(
                "sventity.bad",
                format!(
                    "gentity={g_ent:#x} base={gentities:#x} size={gentity_size} reason=\"array math\""
                ),
            );
        }
        // SAFETY: game Com_Error is valid; literal format string.
        unsafe {
            com_error(ERR_DROP, c"SV_SvEntityForGentity: bad gEnt\n".as_ptr());
        }
        return 0;
    }
    let num = (g_ent - gentities) / gentity_size;
    if num >= MAX_GENTITIES {
        if crate::diagnostics::suspicious_enabled() {
            crate::diagnostics::suspicious(
                "sventity.bad",
                format!(
                    "gentity={g_ent:#x} index={num} max={MAX_GENTITIES} reason=\"index out of range\""
                ),
            );
        }
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
    // A (re)sent gamestate carries the server's **base** configstrings, not the
    // proxy's per-viewer `CS_PLAYERS` overrides: whatever this client held is
    // gone from its `cgs`. Drop the proxy's record of it and re-push on the next
    // frame (a mid-game resend of an outdated client message), so the next
    // scoreboard request does not rely on values the client no longer has.
    if crate::state::instances_enabled() {
        let client_num = client_num_from_addr(cl);
        if client_num < crate::sdk::MAX_CLIENTS {
            crate::tffa::forget_view_overrides(client_num as c_int);
        }
    }
    if orig_state != crate::sdk::CS_CONNECTED {
        // SAFETY: cl is a valid client_t.
        unsafe { *((cl + OFFSET_CLIENT_STATE) as *mut c_int) = orig_state };
    }
}

// ---------------------------------------------------------------------------
// SV_ExecuteClientCommand — diagnostic trace of every client command
// ---------------------------------------------------------------------------

/// `SV_ExecuteClientCommand(client_t*, const char*, qboolean)` — logs the raw
/// command before the engine dispatches it to its own `ucmds` table or to the
/// game module's `ClientCommand`.
///
/// This is the single point every client command passes through, including the
/// server-level ones (`userinfo`, `download`, `nextdl`) that never reach the
/// game's `GAME_CLIENT_COMMAND` vmMain event. Logging before the original body
/// means a command that crashes the engine/game still has its last trace
/// flushed to `proxy_debug.log`.
///
/// # Safety
///
/// Entered from the engine with a valid `client_t*` and a NUL-terminated
/// command string.
pub unsafe extern "C" fn sv_execute_client_command(
    cl: usize,
    command: *const c_char,
    client_ok: c_int,
) {
    if crate::diagnostics::verbose_enabled() {
        let bytes = cstr_bytes(command);
        // A `tffa_*` password argument never reaches the log: the command
        // shape is kept, the secret token is replaced.
        let shown = crate::tffa::redact_client_command(bytes);
        crate::diagnostics::verbose(
            "clientcmd",
            format!(
                "{} client_ok={} len={} cmd=\"{}\"",
                // SAFETY: cl is a valid client_t.
                unsafe { client_label(cl) },
                client_ok,
                bytes.len(),
                crate::diagnostics::escape_bytes(&shown, 512)
            ),
        );
    }
    // SAFETY: trampoline attached at GAME_INIT; args forwarded unchanged.
    let original = original_call(crate::hooks::engine_hook_original_sv_execute_client_command);
    unsafe { original(cl, command, client_ok) };
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
            if crate::diagnostics::verbose_enabled() {
                let cursize = unsafe { read_i32(msg + OFFSET_MSG_CURSIZE) };
                crate::diagnostics::verbose(
                    "clientmsg.gate",
                    format!(
                        "{} server_id={} expected={} restarted={} bytes={cursize} reason=\"stale serverId\"",
                        // SAFETY: cl is a valid client_t.
                        unsafe { client_label(cl) },
                        server_id,
                        sv_server_id,
                        restarted_server_id
                    ),
                );
            }
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
    if !(0..=5).contains(&c) && crate::diagnostics::suspicious_enabled() {
        // clc_bad..clc_EOF; the pristine body only dispatches 2/3 and treats
        // the rest as end-of-message. An out-of-range byte is a malformed
        // client and can be the fingerprint of a crashing exploit.
        let cursize = unsafe { read_i32(msg + OFFSET_MSG_CURSIZE) };
        crate::diagnostics::suspicious(
            "clientmsg.badcmd",
            format!(
                "{} command_byte={c} bytes={cursize}",
                // SAFETY: cl is a valid client_t.
                unsafe { client_label(cl) }
            ),
        );
    }
    // SAFETY: restore exactly the state the body expects.
    unsafe {
        *((msg + OFFSET_MSG_READCOUNT) as *mut c_int) = saved_readcount;
        *((msg + OFFSET_MSG_BIT) as *mut c_int) = saved_bit;
    }
    if c == CLC_MOVE || c == CLC_MOVE_NO_DELTA {
        // SAFETY: cl is a valid client_t; `message_acknowledge` is this
        // message's own ack (cl->messageAcknowledge is only updated when the
        // pristine body runs).
        unsafe { netstatus::mark_packet_start(cl, message_acknowledge) };
    }

    // SAFETY: trampoline attached at GAME_INIT; args forwarded unchanged.
    let original = original_call(crate::hooks::engine_hook_original_sv_execute_client_message);
    unsafe { original(cl, msg) };
}

// ---------------------------------------------------------------------------
// SV_Netchan_Process — incoming packet-rate limiter (entry wrapper)
// ---------------------------------------------------------------------------

/// `qboolean SV_Netchan_Process(client_t *client, msg_t *msg)` (0x8057e14):
/// account the packet for `proxy_sv_enableClientPpsLimit` and, when the client
/// is over its FPS-derived limit, return `qfalse` so the engine drops it
/// exactly as it drops an out-of-order packet (`sv_main.cpp:623`).
///
/// This is the first sequenced-packet point that already knows the
/// `client_t*` (address and qport matched), so the limiter is per player and
/// connectionless/unknown-address traffic keeps its own engine handling. The
/// pristine body (netchan sequencing/decoding) is still called for accepted
/// packets; a dropped packet never reaches `SV_ExecuteClientMessage`, does not
/// refresh `lastPacketTime` and is retransmitted by the client's reliable path
/// or covered by its usercmd redundancy like any other lost UDP packet.
///
/// With `proxy_sv_enableClientPpsLimit 0` the wrapper is a plain passthrough
/// (one atomic check).
///
/// # Safety
///
/// Entered from the engine with `(client_t*, msg_t*)`.
pub unsafe extern "C" fn sv_netchan_process(cl: usize, msg: usize) -> c_int {
    // SAFETY: cl is the client_t* the engine matched for this packet.
    if !unsafe { crate::pps::on_incoming_packet(cl) } {
        return 0;
    }
    // SAFETY: trampoline attached at GAME_INIT; args forwarded unchanged.
    let original = original_call(crate::hooks::engine_hook_original_sv_netchan_process);
    unsafe { original(cl, msg) }
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
/// `TffaState::sound_source`, or `s.clientNum` for `EV_ENTITY_SOUND` and the
/// player respawn/teleport events `EV_PLAYER_TELEPORT_IN`/`OUT`) and dropped
/// the same way; map/world sounds and `EV_GLOBAL_SOUND` broadcasts stay
/// visible/audible everywhere.
///
/// World/static entities are always kept. Player entities (`entNum < 32`
/// with a live client) and owned entities (missiles/sabers via
/// `r.ownerNum`/`s.owner`, obituaries via `s.otherEntityNum*`) are kept only
/// within the viewer's match (lobby `None` sees lobby).
///
/// After the pristine and TFFA passes, the main-view call also applies the
/// per-viewer visibility filter (`crate::fov`): the `proxy_sv_maxFov` cap and
/// the `proxy_sv_antiWallHack` line-of-sight test. A player failing either is
/// hidden for this snapshot (mind-trick bit + 1 % model scale + NODRAW), and
/// `SV_SendClientSnapshot` restores the shared entity state once it has been
/// copied.
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
    if crate::state::instances_enabled() {
        // Snapshot the match table and the sound attribution once (no
        // per-entity locking); the viewer's own match is follow-aware
        // (spectating someone adopts their instance).
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
    // FOV cap (`proxy_sv_maxFov`, D-022): run after the TFFA compaction so an
    // entity the viewer will not receive is not touched, and for the main view
    // only (portal viewpoints render elsewhere on screen).
    //
    // SAFETY: origin/frame/eNums are the engine's arguments; g_entities is the
    // located game array.
    unsafe { crate::fov::apply(viewer, origin, frame, e_nums, g_entities, portal) };
}

// ---------------------------------------------------------------------------
// SV_SendClientSnapshot — restore the per-viewer FOV mutations (D-022)
// ---------------------------------------------------------------------------

/// `void SV_SendClientSnapshot(client_t *client)` (`sv_snapshot.cpp:740`):
/// build the client's snapshot (which runs the `SV_AddEntitiesVisibleFromPoint`
/// wrapper and copies the entity states), send it, then put back the entity
/// fields the FOV cap changed. The restore must happen after the copy, not at
/// the end of `SV_AddEntitiesVisibleFromPoint`, or the snapshot would carry the
/// original state.
///
/// # Safety
///
/// Entered from the engine with a valid `client_t*`.
pub unsafe extern "C" fn sv_send_client_snapshot(client: usize) {
    // Record the *actual* engine client slot before the build: for a
    // following spectator `frame->ps.clientNum` is the followed player, and
    // the FOV cap must know that the viewer is a spectator.
    if client != 0 {
        let slot = client_num_from_addr(client);
        if slot < crate::sdk::MAX_CLIENTS {
            state::with_state(|s| s.fov.viewer_client = slot as i32);
        }
    }
    // SAFETY: trampoline attached at GAME_INIT; `client` forwarded unchanged.
    let original = original_call(crate::hooks::engine_hook_original_sv_send_client_snapshot);
    unsafe { original(client) };
    crate::fov::restore();
    state::with_state(|s| s.fov.viewer_client = -1);
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
    // no `r.ownerNum` (`G_InitGentity` sets it to `NONE`), and only the
    // `s.*` fields their creator assigns: the rest keep the freed slot's
    // memset zero (`g_utils.c:1100`), a bare `0` that coincides with client 0.
    // Which fields are meaningful is therefore event-specific
    // (`tffa::event_owners`, source-derived from every `G_TempEntity` call
    // site). Reading a field the event never sets as "client 0" dropped every
    // such effect outside client 0's match — the reported "shield is not
    // displayed when receiving damage, when not on main TFFA"
    // (`EV_SHIELD_HIT` sets `s.otherEntityNum` only) and the same class for
    // impact sparks, glass shards and projectile-deflect flashes.
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
        // `G_EntitySound`/`G_SoundOnEnt` and the respawn/teleport pair — while
        // deliberate broadcasts stay audible everywhere.
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
        // Clash effects carry no usable owner on most creator paths (the
        // saber-lock tick, a thrown-saber block, a deflect and the wall-hit
        // flare set none at all, and `WP_SaberDoClash`'s `s.otherEntityNum2` is
        // ambiguous with the memset zero for client 0). The construction stamp —
        // recorded while the acting player's scope was open (`WP_SaberPositionUpdate`
        // / `ClientThink_real` / `G_RunMissile`) — names the instance, so it is
        // authoritative whenever it is not the world stamp. A world stamp keeps
        // the old field-based behaviour below (`EV_SABER_BLOCK`'s
        // `OptionalAttacker` rule).
        if crate::tffa::event_uses_construction_source(event) {
            let source = sound_source[ent_num as usize];
            if source != crate::tffa::SOURCE_WORLD {
                return crate::tffa::sound_visible(viewer_match, source);
            }
        }
        // SAFETY: ent is inside the g_entities array; `s.*` fields are ints.
        let field = |off: usize| unsafe { read_i32(ent + off) };
        return match crate::tffa::event_owners(event) {
            crate::tffa::EventOwners::World => true,
            crate::tffa::EventOwners::Victim => {
                let owned = [field(OFFSET_GENTITY_S_OTHER_ENTITY_NUM)];
                owners_in_viewer_match(viewer_match, table, g_entities, &owned)
            }
            crate::tffa::EventOwners::OptionalVictim => {
                let victim = field(OFFSET_GENTITY_S_OTHER_ENTITY_NUM);
                // The creator leaves the field at 0 when it names no player
                // (`EV_DISRUPTOR_SNIPER_SHOT` on a wall): a world effect.
                victim == 0 || owners_in_viewer_match(viewer_match, table, g_entities, &[victim])
            }
            crate::tffa::EventOwners::OptionalAttacker => {
                let attacker = field(OFFSET_GENTITY_S_OTHER_ENTITY_NUM2);
                // Projectile-deflect `EV_SABER_BLOCK`s set no attacker; the
                // untouched 0 is not client 0.
                attacker == 0
                    || owners_in_viewer_match(viewer_match, table, g_entities, &[attacker])
            }
            crate::tffa::EventOwners::VictimAndAttacker => {
                let owned = [
                    field(OFFSET_GENTITY_S_OTHER_ENTITY_NUM),
                    field(OFFSET_GENTITY_S_OTHER_ENTITY_NUM2),
                ];
                owners_in_viewer_match(viewer_match, table, g_entities, &owned)
            }
            crate::tffa::EventOwners::SOwner => {
                let owned = [field(OFFSET_GENTITY_S_OWNER)];
                owners_in_viewer_match(viewer_match, table, g_entities, &owned)
            }
        };
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

// ---------------------------------------------------------------------------
// MSG_ReadBits (CVE-2017-11721) — entry wrapper (S13/O11)
// ---------------------------------------------------------------------------

/// `MSG_ReadBits` (S13/O11, CVE-2017-11721): the retail function has no bound
/// and reads whatever follows the message in the receive buffer. These are the
/// ioq3/OpenJK bounds: once the end is passed, the read returns 0 and
/// `readcount` stays above `cursize`, which the callers test. The original is
/// still called for the read itself, so one Huffman symbol can go a few bytes
/// past the end before the post-test; that stays inside the receive buffer and
/// the value is dropped.
///
/// # Safety
///
/// Entered from the engine's message readers with a valid `msg_t*`.
pub unsafe extern "C" fn msg_read_bits(msg: usize, bits: c_int) -> c_int {
    // SAFETY: msg is a valid msg_t from the engine's receive path.
    let readcount = unsafe { read_i32(msg + OFFSET_MSG_READCOUNT) };
    let cursize = unsafe { read_i32(msg + OFFSET_MSG_CURSIZE) };

    if readcount > cursize {
        return 0;
    }

    // SAFETY: msg is a valid msg_t; `oob` is the third boolean, `bit` the
    // last int.
    let (oob, bit) = unsafe {
        (
            read_i32(msg + OFFSET_MSG_OOB) != 0,
            read_i32(msg + OFFSET_MSG_BIT),
        )
    };

    if readbits_over_bound(readcount, cursize, bit, bits, oob) {
        // SAFETY: msg is writable; this is the engine's own "past end"
        // marker.
        unsafe { write_i32(msg + OFFSET_MSG_READCOUNT, cursize + 1) };
        return 0;
    }

    // SAFETY: trampoline attached at GAME_INIT; arguments unchanged.
    let original = original_call(crate::hooks::engine_hook_original_msg_read_bits);
    let value = unsafe { original(msg, bits) };

    // SAFETY: msg is a valid msg_t.
    let bit = unsafe { read_i32(msg + OFFSET_MSG_BIT) };
    if readbits_post_over(bit, cursize, oob) {
        // SAFETY: msg is writable.
        unsafe { write_i32(msg + OFFSET_MSG_READCOUNT, cursize + 1) };
        return 0;
    }

    value
}

/// Whether a `MSG_ReadBits(msg, bits)` request starts past the end of the
/// message (ioq3/OpenJK pre-check).
fn readbits_over_bound(readcount: i32, cursize: i32, bit: i32, bits: i32, oob: bool) -> bool {
    let bit_count = if bits < 0 { -bits } else { bits };
    if oob {
        readcount + (bit_count >> 3) > cursize
    } else {
        bit + (bit_count & 7) > (cursize << 3)
    }
}

/// Whether the read left the bit cursor past the end (ioq3/OpenJK post-check).
fn readbits_post_over(bit: i32, cursize: i32, oob: bool) -> bool {
    !oob && bit > (cursize << 3)
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

    /// Saber clash effects from another instance: `EV_SABER_BLOCK` (block
    /// sound + effect + screen flash) and `EV_SABER_CLASHFLARE`
    /// (`saberhitwall` sound + flare) are built by the saber update outside
    /// every owner field. The construction stamp recorded while
    /// `WP_SaberPositionUpdate` (or the blocking think / missile think) had the
    /// player's acting scope open is authoritative, including when
    /// `WP_SaberDoClash`'s `s.otherEntityNum2` is `0` for client 0.
    #[test]
    fn clash_effects_follow_their_construction_stamp() {
        let (mut game, mut table) = main_player_and_viewer();
        game.with_player(5);
        table[5] = 1;
        for event in [crate::sdk::EV_SABER_BLOCK, crate::sdk::EV_SABER_CLASHFLARE] {
            game.with_entity(48, crate::sdk::ET_EVENTS + event);
            // The saber-lock / flare shapes: no owner field at all.
            let mut sounds = sound_table();
            sounds[48] = 1;
            assert!(
                snapshot_keep(1, &table, &sounds, &shared_table(), game.base(), 48),
                "event {event} stamped for instance 1 must be kept there"
            );
            for foreign in [0, 2] {
                assert!(
                    !snapshot_keep(foreign, &table, &sounds, &shared_table(), game.base(), 48),
                    "event {event} stamped for instance 1 must be hidden in match {foreign}"
                );
            }
            // `WP_SaberDoClash` names the attacker, but client 0's `0` is the
            // same value as the untouched default: the stamp still wins.
            game.put_i32(48, OFFSET_GENTITY_S_OTHER_ENTITY_NUM2, 0);
            assert!(snapshot_keep(
                1,
                &table,
                &sounds,
                &shared_table(),
                game.base(),
                48
            ));
            assert!(!snapshot_keep(
                0,
                &table,
                &sounds,
                &shared_table(),
                game.base(),
                48
            ));
            // A world stamp (no scope was open) keeps the old behaviour: see
            // `ownerless_temp_effect_stays_visible_in_every_instance`.
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

    /// Player respawn/teleport temp entities (`EV_PLAYER_TELEPORT_IN`/`OUT`)
    /// carry their player in `s.clientNum` and nothing else. Before this
    /// classification they fell through to the generic owner-field rule, whose
    /// zero defaults resolved to client 0, so the main instance heard every
    /// other instance's respawn `telein` sound (and instance players heard
    /// main's when they were not in client 0's match).
    #[test]
    fn respawn_teleport_sound_follows_its_player_match() {
        let (mut game, mut table) = main_player_and_viewer();
        game.with_player(5);
        table[5] = 1;
        for event in [
            crate::sdk::EV_PLAYER_TELEPORT_IN,
            crate::sdk::EV_PLAYER_TELEPORT_OUT,
        ] {
            game.with_entity(48, crate::sdk::ET_EVENTS + event);
            game.put_i32(48, OFFSET_ENTITY_CLIENT_NUM, 5);
            assert!(
                snapshot_keep(1, &table, &sound_table(), &shared_table(), game.base(), 48),
                "event {event} must stay audible in its own instance"
            );
            assert!(
                !snapshot_keep(0, &table, &sound_table(), &shared_table(), game.base(), 48),
                "event {event} of an instance-1 player must be silent in main"
            );
            // The same event for a main player stays in main only.
            game.put_i32(48, OFFSET_ENTITY_CLIENT_NUM, 0);
            assert!(snapshot_keep(
                0,
                &table,
                &sound_table(),
                &shared_table(),
                game.base(),
                48
            ));
            assert!(!snapshot_keep(
                1,
                &table,
                &sound_table(),
                &shared_table(),
                game.base(),
                48
            ));
        }
    }

    /// `EV_SHIELD_HIT` (`g_combat.c:5257-5259`) names its victim in
    /// `s.otherEntityNum` only; `s.otherEntityNum2` keeps the untouched `0`
    /// default. Reading that default as client 0 hid the shield shell from
    /// every instance but main — the reported "shield is not displayed when
    /// receiving damage, when not on main TFFA".
    #[test]
    fn shield_hit_follows_its_victim_match() {
        let (mut game, mut table) = main_player_and_viewer();
        game.with_player(5);
        table[5] = 1;
        game.with_entity(48, crate::sdk::ET_EVENTS + crate::sdk::EV_SHIELD_HIT);
        // Victim in instance 1; the untouched oEN2 stays 0.
        game.put_i32(48, OFFSET_GENTITY_S_OTHER_ENTITY_NUM, 5);
        assert!(
            snapshot_keep(1, &table, &sound_table(), &shared_table(), game.base(), 48),
            "the shield shell must show in the victim's instance"
        );
        assert!(
            !snapshot_keep(0, &table, &sound_table(), &shared_table(), game.base(), 48),
            "the shield shell must stay hidden from main"
        );
        // Victim is client 0: 0 in the used field really is client 0.
        game.put_i32(48, OFFSET_GENTITY_S_OTHER_ENTITY_NUM, 0);
        assert!(snapshot_keep(
            0,
            &table,
            &sound_table(),
            &shared_table(),
            game.base(),
            48
        ));
        assert!(!snapshot_keep(
            1,
            &table,
            &sound_table(),
            &shared_table(),
            game.base(),
            48
        ));
    }

    /// Owner-attributed events (`EV_OBITUARY`, `EV_SABER_HIT`) name both the
    /// victim and the attacker/killer; the other instance must not see them.
    #[test]
    fn two_owner_temp_event_follows_both_matches() {
        let (mut game, mut table) = main_player_and_viewer();
        game.with_player(5);
        table[5] = 1;
        for event in [crate::sdk::EV_OBITUARY, crate::sdk::EV_SABER_HIT] {
            game.with_entity(48, crate::sdk::ET_EVENTS + event);
            game.put_i32(48, OFFSET_GENTITY_S_OTHER_ENTITY_NUM, 5);
            game.put_i32(48, OFFSET_GENTITY_S_OTHER_ENTITY_NUM2, 5);
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
            // An attacker from main (mixed assignments are not expected, but
            // the hide direction must be symmetric).
            game.put_i32(48, OFFSET_GENTITY_S_OTHER_ENTITY_NUM, 5);
            game.put_i32(48, OFFSET_GENTITY_S_OTHER_ENTITY_NUM2, 0);
            assert!(!snapshot_keep(
                1,
                &table,
                &sound_table(),
                &shared_table(),
                game.base(),
                48
            ));
        }
    }

    /// `s.owner`-tagged temp events (`EV_FORCE_DRAINED`, `EV_LOCALTIMER`, ...)
    /// follow the player in that field.
    #[test]
    fn s_owner_temp_event_follows_its_player_match() {
        let (mut game, mut table) = main_player_and_viewer();
        game.with_player(5);
        table[5] = 1;
        for event in [
            crate::sdk::EV_FORCE_DRAINED,
            crate::sdk::EV_LOCALTIMER,
            crate::sdk::EV_CONC_ALT_IMPACT,
        ] {
            game.with_entity(48, crate::sdk::ET_EVENTS + event);
            game.put_i32(48, OFFSET_GENTITY_S_OWNER, 5);
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
    }

    /// Events whose owner field is set only on some paths must not read the
    /// untouched `0` as client 0: a projectile-deflect `EV_SABER_BLOCK`
    /// (`g_missile.c:338,507,582` — no attacker) and a sniper beam that hit a
    /// wall (`g_weapon.c:776-786` — no victim) are world effects.
    #[test]
    fn optional_owner_temp_event_is_world_when_unset() {
        let (game, table) = main_player_and_viewer();
        let mut game = game;
        for event in [
            crate::sdk::EV_SABER_BLOCK,
            crate::sdk::EV_DISRUPTOR_SNIPER_SHOT,
        ] {
            game.with_entity(48, crate::sdk::ET_EVENTS + event);
            for viewer_match in [0, 1, 2] {
                assert!(
                    snapshot_keep(
                        viewer_match,
                        &table,
                        &sound_table(),
                        &shared_table(),
                        game.base(),
                        48
                    ),
                    "event {event} with no owner must stay visible everywhere"
                );
            }
        }
        // When the optional field does name a player, it is attributed again.
        let mut table = table;
        game.with_player(5);
        table[5] = 1;
        game.with_entity(48, crate::sdk::ET_EVENTS + crate::sdk::EV_SABER_BLOCK);
        game.put_i32(48, OFFSET_GENTITY_S_OTHER_ENTITY_NUM2, 5);
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

    /// Owner-less positional effects stay in every instance. Before the
    /// event-specific classification the filter read both `s.otherEntityNum*`
    /// for them, and their bare `0` defaults dropped them outside client 0's
    /// match (weapon impact sparks, glass shards, the saber clash flare).
    #[test]
    fn ownerless_temp_effect_stays_visible_in_every_instance() {
        let (game, table) = main_player_and_viewer();
        let mut game = game;
        for event in [
            crate::sdk::EV_SABER_CLASHFLARE,
            crate::sdk::EV_GLASS_SHATTER,
            crate::sdk::EV_MISSILE_MISS,
            crate::sdk::EV_DISRUPTOR_HIT,
        ] {
            game.with_entity(48, crate::sdk::ET_EVENTS + event);
            for viewer_match in [0, 1, 2] {
                assert!(
                    snapshot_keep(
                        viewer_match,
                        &table,
                        &sound_table(),
                        &shared_table(),
                        game.base(),
                        48
                    ),
                    "event {event} carries no owner and must stay visible"
                );
            }
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

    #[test]
    fn info_print_pads_short_keys_and_keeps_long_values() {
        let long_value = vec![b'x'; 600];
        let mut info = b"\\a\\1\\b\\".to_vec();
        info.extend_from_slice(&long_value);

        let lines = info_print_lines(&info);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].0.len(), 20, "short key padded to 20");
        assert_eq!(&lines[0].0[..5], b"a    ");
        assert_eq!(lines[0].1.as_deref(), Some(&b"1"[..]));
        assert_eq!(lines[1].0.len(), 20);
        assert_eq!(
            lines[1].1.as_ref().map(Vec::len),
            Some(600),
            "the retail 512-byte buffer would have overflowed"
        );
    }

    #[test]
    fn info_print_reports_a_missing_value() {
        let lines = info_print_lines(b"\\key\\value\\last");
        assert_eq!(lines.len(), 2);
        assert_eq!(&lines[0].0[..3], b"key");
        assert_eq!(lines[0].1.as_deref(), Some(&b"value"[..]));
        assert_eq!(&lines[1].0[..4], b"last");
        assert!(lines[1].1.is_none());
    }

    #[test]
    fn info_print_leaves_a_long_key_unpadded() {
        let key = vec![b'k'; 600];
        let mut info = b"\\".to_vec();
        info.extend_from_slice(&key);
        info.extend_from_slice(b"\\v");

        let lines = info_print_lines(&info);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, key);
        assert_eq!(lines[0].1.as_deref(), Some(&b"v"[..]));
    }

    #[test]
    fn msg_read_bits_bounds_match_ioq3() {
        // Out-of-band: whole bytes.
        assert!(!readbits_over_bound(0, 2, 0, 16, true));
        assert!(readbits_over_bound(0, 2, 0, 24, true));
        assert!(!readbits_over_bound(2, 2, 0, 0, true));

        // Huffman: bit cursor, the exact end is still allowed.
        assert!(!readbits_over_bound(0, 2, 8, 8, false));
        assert!(!readbits_over_bound(0, 2, 15, 1, false));
        assert!(readbits_over_bound(0, 2, 16, 1, false));

        // Negative bit counts use their magnitude.
        assert!(readbits_over_bound(0, 2, 0, -24, true));

        // After the read: one Huffman symbol may overshoot, then the value is
        // dropped and the cursor stays past the end.
        assert!(!readbits_post_over(16, 2, false));
        assert!(readbits_post_over(17, 2, false));
        assert!(!readbits_post_over(17, 2, true));
    }
}
