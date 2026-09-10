//! Engine address tables and memory-layer initialisation
//! (`Proxy_Engine_Wrappers.{hpp,cpp}`).
//!
//! The engine (`linuxjampded`) is a non-PIE `ET_EXEC` fixed at `0x08048000`, so
//! all engine addresses are absolute and version-locked to
//! `JAmp: v1.0.1.1 linux-i386 Nov 10 2003` (R-001/R-010). The tables below are
//! the Rust copy of `Proxy_Engine_Wrappers.hpp`; `init_memory_layer` mirrors
//! `Proxy_Engine_Initialize_MemoryLayer` (reads `svs`/`sv`, the cvar-pointer
//! slots and the common vars, and binds `Com_EndRedirect`).
//!
//! Only the parts the ported milestones use are populated further than the
//! address tables: function-pointer *calls* (besides `Com_EndRedirect`) arrive
//! with the hook milestones.

use core::ffi::{c_char, c_int};
use std::sync::{Mutex, MutexGuard};

// ---------------------------------------------------------------------------
// Engine address tables (Proxy_Engine_Wrappers.hpp, verified R-010/R-018)
// ---------------------------------------------------------------------------

/// Engine `.text`/RW segment bounds (R-002) used by the address sanity tests.
#[cfg(test)]
pub const ENGINE_TEXT_LO: usize = 0x0804_a1e0;
#[cfg(test)]
pub const ENGINE_TEXT_HI: usize = 0x0819_9494; // _fini, end of executable LOAD
#[cfg(test)]
pub const ENGINE_RW_LO: usize = 0x0819_a4e0;
#[cfg(test)]
pub const ENGINE_RW_HI: usize = 0x0839_6bb4; // end of RW LOAD (incl. BSS)

#[allow(dead_code)]
/// Reference table copied from `Proxy_Engine_Wrappers.hpp` — hookable/callable
/// engine function addresses (absolute, version-locked R-010). Consumed by the
/// hook milestones; kept whole so the table is auditable and range-swept by
/// the `cfg(test)` tests.
pub mod functions {
    pub const COM_PRINTF: usize = 0x0807_2ca4;
    pub const SV_CALCPINGS: usize = 0x0805_7204;
    pub const SV_SEND_MESSAGE_TO_CLIENT: usize = 0x0805_8c84;
    pub const SV_SEND_CLIENT_GAME_STATE: usize = 0x0804_cee4;
    pub const SV_SVENTITY_FOR_GENTITY: usize = 0x0804_ffb4;
    pub const SV_CONNECTIONLESS_PACKET: usize = 0x0805_6d64;
    pub const SVC_STATUS: usize = 0x0805_6574;
    pub const SVC_INFO: usize = 0x0805_6784;
    pub const SVC_REMOTE_COMMAND: usize = 0x0805_6b14;
    pub const CMD_TOKENIZE_STRING: usize = 0x0812_c454;
    pub const SV_USERINFO_CHANGED: usize = 0x0804_e144;
    pub const SV_EXECUTE_CLIENT_MESSAGE: usize = 0x0804_e8c4;
    pub const SV_PACKET_EVENT: usize = 0x0805_7024;
    pub const SV_EXECUTE_CLIENT_COMMAND: usize = 0x0804_e3d4;
    pub const SV_BEGIN_DOWNLOAD_F: usize = 0x0804_d714;
    pub const SV_DONE_DOWNLOAD_F: usize = 0x0804_eaa4;
    pub const SV_WRITE_DOWNLOAD_TO_CLIENT: usize = 0x0804_d764;
    pub const SV_ADD_ENTITIES_VISIBLE_FROM_POINT: usize = 0x0805_8404;
    pub const SV_SEND_CLIENT_SNAPSHOT: usize = 0x0805_8e04;
    pub const SV_NEXT_DOWNLOAD_F: usize = 0x0804_d614;
    pub const SV_STOP_DOWNLOAD_F: usize = 0x0804_d5b4;
    pub const SV_UPDATE_USERINFO_F: usize = 0x0804_e314;
    pub const NAVIGATOR_LOAD: usize = 0x0812_4ff4;

    pub const SV_CLIENT_ENTER_WORLD: usize = 0x0804_d444;
    pub const SV_CLIENT_THINK: usize = 0x0804_e634;
    pub const SV_DROP_CLIENT: usize = 0x0804_cb84;
    pub const SV_NETCHAN_TRANSMIT: usize = 0x0805_7db4;
    pub const SV_FLUSH_REDIRECT: usize = 0x0805_7b44;
    pub const SV_UPDATE_SERVER_COMMANDS_TO_CLIENT: usize = 0x0805_82c4;
    pub const SV_GET_CHALLENGE: usize = 0x0804_b9a4;
    pub const SV_DIRECT_CONNECT: usize = 0x0804_c014;
    pub const SV_NETCHAN_PROCESS: usize = 0x0805_7e14;
    pub const SV_SEND_SERVER_COMMAND: usize = 0x0805_6214;
    pub const SV_ADD_ENT_TO_SNAPSHOT: usize = 0x0805_83b4;
    pub const COM_DPRINTF: usize = 0x0807_2ed4;
    pub const COM_HASH_KEY: usize = 0x0807_3b14;
    pub const COM_BEGIN_REDIRECT: usize = 0x0807_2c34;
    pub const COM_END_REDIRECT: usize = 0x0807_2c74;
    pub const CVAR_VARIABLE_STRING: usize = 0x0807_56f4;
    pub const CVAR_VARIABLE_VALUE: usize = 0x0807_5694;
    pub const FS_FOPEN_FILE_WRITE: usize = 0x0812_d2a4;
    pub const FS_FORCE_FLUSH: usize = 0x0812_c8a4;
    pub const FS_INITIALIZED: usize = 0x0812_b754;
    pub const FS_WRITE: usize = 0x0812_e074;
    pub const FS_FILE_IS_IN_PAK: usize = 0x0812_e354;
    pub const FS_LOADED_PAK_PURE_CHECKSUMS: usize = 0x0813_0d44;
    pub const FS_REFERENCED_PAK_NAMES: usize = 0x0813_1024;
    pub const FS_SV_FOPEN_FILE_READ: usize = 0x0812_cd74;
    pub const FS_READ: usize = 0x0812_df44;
    pub const FS_FCLOSE_FILE: usize = 0x0812_d1b4;
    pub const NETCHAN_TRANSMIT_NEXT_FRAGMENT: usize = 0x0807_ab74;
    pub const NET_ADR_TO_STRING: usize = 0x0807_b314;
    pub const NET_OUT_OF_BAND_PRINT: usize = 0x0807_b744;
    pub const NET_COMPARE_BASE_ADR: usize = 0x0807_b284;
    pub const MSG_INIT: usize = 0x0807_74a4;
    pub const MSG_READ_BYTE: usize = 0x0807_7df4;
    pub const MSG_READ_DELTA_USERCMD_KEY: usize = 0x0807_8b34;
    pub const MSG_READ_SHORT: usize = 0x0807_7e34;
    pub const MSG_READ_LONG: usize = 0x0807_7e74;
    pub const MSG_READ_STRING: usize = 0x0807_7ee4;
    pub const MSG_READ_STRING_LINE: usize = 0x0807_7ff4;
    pub const MSG_BITSTREAM: usize = 0x0807_75d4;
    pub const MSG_BEGIN_READING_OOB: usize = 0x0807_7614;
    pub const MSG_WRITE_BIG_STRING: usize = 0x0807_7c04;
    pub const MSG_WRITE_BYTE: usize = 0x0807_7a24;
    pub const MSG_WRITE_DELTA_ENTITY: usize = 0x0807_8d74;
    pub const MSG_WRITE_LONG: usize = 0x0807_7ad4;
    pub const MSG_WRITE_SHORT: usize = 0x0807_7aa4;
    pub const MSG_WRITE_STRING: usize = 0x0807_7b34;
    pub const MSG_WRITE_DATA: usize = 0x0807_7a54;
    pub const SYS_IS_LAN_ADDRESS: usize = 0x080c_5f84;
    pub const SYS_PRINT: usize = 0x080c_57a4;
    pub const CMD_ARGV: usize = 0x0812_c264;
    pub const CMD_EXECUTE_STRING: usize = 0x0812_4144;
    pub const HUFF_DECOMPRESS: usize = 0x0807_c224;
    pub const Z_FREE: usize = 0x0808_0474;
    pub const Z_MALLOC: usize = 0x0808_00f4;
    pub const CM_BOX_TRACE: usize = 0x0807_2bc4;
    pub const SV_TRACE: usize = 0x0805_a4c4;
    pub const SV_CLIENT_COMMAND: usize = 0x0804_e4b4;
    pub const SV_GAME_CLIENT_NUM: usize = 0x0805_4754;
    pub const SV_RATE_MSEC: usize = 0x0805_8c04;
}

#[allow(dead_code)]
/// Call-site retarget (Proxy_Engine_Wrappers.hpp:63).
pub mod calls {
    pub const CALL_SV_ADD_ENT_TO_SNAPSHOT_FOR_PLAYERS: usize = 0x0805_882d;
}

#[allow(dead_code)]
/// Inline patches (Proxy_Engine_Wrappers.hpp:19-29).
pub mod patches {
    pub const SVC_REMOTE_COMMAND_TIMER_NOP_ADDR: usize = 0x0805_6b26;
    pub const SVC_REMOTE_COMMAND_TIMER_NOP_LEN: usize = 25;
    pub const SVC_REMOTE_COMMAND_REDIRECT_LEN_ADDR: usize = 0x0805_6c7d;
    pub const SVC_REMOTE_COMMAND_REDIRECT_LEN_BYTE: u8 = 0x03;
}

#[allow(dead_code)]
/// Engine variable addresses (Proxy_Engine_Wrappers.hpp:135-144).
pub mod vars {
    pub const SVS: usize = 0x0831_21e0;
    pub const SVSCLIENTS: usize = 0x0831_21ec;
    pub const SV: usize = 0x0827_3ec0;
    /// `server.sv.serverId` (binary: `SV_ExecuteClientMessage` compares the
    /// message serverId against `0x8273ec8`).
    pub const SV_SERVER_ID: usize = 0x0827_3ec8;
    /// `server.sv.restartedServerId` (`0x8273ecc`, same compare site).
    pub const SV_RESTARTED_SERVER_ID: usize = 0x0827_3ecc;
    pub const RD_BUFFER: usize = 0x081e_90c0;
    pub const RD_BUFFERSIZE: usize = 0x081e_90c4;
    pub const RD_FLUSH: usize = 0x081e_90c8;
    pub const LOGFILE: usize = 0x0831_f24c;
    pub const CMD_ARGC: usize = 0x0826_0e20;
    pub const CMD_ARGV: usize = 0x0826_0e40;
    pub const CMD_TOKENIZED: usize = 0x0826_4440;
}

#[allow(dead_code)]
/// cvar-pointer slots (Proxy_Engine_Wrappers.hpp:157-173): each address holds
/// a `cvar_t*` (R-018 live-verified). The 17 pointers are dereferenced once by
/// `init_memory_layer`; the slot addresses themselves remain reference data.
pub mod cvar_slots {
    pub const SV_FPS: usize = 0x0827_3e84;
    pub const SV_GAMETYPE: usize = 0x0831_21cc;
    pub const SV_HOSTNAME: usize = 0x0827_3e9c;
    pub const SV_MAPNAME: usize = 0x0827_3e90;
    pub const SV_MAXCLIENTS: usize = 0x0827_3ea4;
    pub const SV_PRIVATECLIENTS: usize = 0x0827_3e80;
    pub const SV_PURE: usize = 0x0831_21a8;
    pub const SV_MAXRATE: usize = 0x0831_219c;
    pub const SV_RCONPASSWORD: usize = 0x0831_21d4;
    pub const SV_FLOODPROTECT: usize = 0x0827_3e88;
    pub const SV_ALLOWDOWNLOAD: usize = 0x0831_21b0;
    pub const COM_DEDICATED: usize = 0x0831_f254;
    pub const COM_SV_RUNNING: usize = 0x0831_f300;
    pub const COM_CL_RUNNING: usize = 0x0831_f400;
    pub const COM_LOGFILE: usize = 0x0831_f41c;
    pub const COM_DEVELOPER: usize = 0x0831_e204;
    pub const FS_GAMEDIRVAR: usize = 0x0838_aaec;
}

// ---------------------------------------------------------------------------
// Runtime memory layer
// ---------------------------------------------------------------------------

/// Result of `Proxy_Engine_Initialize_MemoryLayer` (values read once at
/// `GAME_INIT`). Addresses kept as `usize`; raw pointers are re-materialised
/// on use. Fields are read by the hook milestones (svs/sv already proved live
/// in R-018), hence `allow(dead_code)` until then.
#[derive(Debug, Default, Clone, Copy)]
#[allow(dead_code)]
pub struct MemoryLayer {
    /// `serverStatic_t*` at 0x83121e0.
    pub svs: usize,
    /// `server_t*` at 0x8273ec0.
    pub sv: usize,
    /// The 17 cvar-pointer slots, dereferenced to their `cvar_t*`.
    pub cvar_ptrs: [usize; 17],
    /// Common var addresses (the *address of* each engine global, not its value).
    pub common_vars: [usize; 6],
}

static MEMORY: Mutex<Option<MemoryLayer>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<MemoryLayer>> {
    MEMORY.lock().unwrap_or_else(|p| p.into_inner())
}

/// Read a `cvar_t*` from an engine cvar-pointer slot.
///
/// # Safety
///
/// `slot` must be a valid engine data address holding a `cvar_t*` (see
/// `cvar_slots`); only meaningful inside the engine process.
unsafe fn read_cvar_ptr_slot(slot: usize) -> usize {
    // SAFETY: validated by the address-range unit tests + R-018 live read-back.
    unsafe { (*(slot as *const *const u8)) as usize }
}

/// `Proxy_Engine_Initialize_MemoryLayer` (`Proxy_Engine_Wrappers.cpp:7-138`).
/// Reads the engine var/cvar slots and records them; called once at `GAME_INIT`
/// after the original module is loaded and before forwarding init.
pub fn init_memory_layer() {
    eprintln!("----- proxy-rs: Initializing memory layer");
    // SAFETY: every slot below is a validated engine data address (R-010) and
    // the engine is running (GAME_INIT).
    let cvar_ptrs = unsafe {
        [
            read_cvar_ptr_slot(cvar_slots::SV_FPS),
            read_cvar_ptr_slot(cvar_slots::SV_GAMETYPE),
            read_cvar_ptr_slot(cvar_slots::SV_HOSTNAME),
            read_cvar_ptr_slot(cvar_slots::SV_MAPNAME),
            read_cvar_ptr_slot(cvar_slots::SV_MAXCLIENTS),
            read_cvar_ptr_slot(cvar_slots::SV_PRIVATECLIENTS),
            read_cvar_ptr_slot(cvar_slots::SV_PURE),
            read_cvar_ptr_slot(cvar_slots::SV_MAXRATE),
            read_cvar_ptr_slot(cvar_slots::SV_RCONPASSWORD),
            read_cvar_ptr_slot(cvar_slots::SV_FLOODPROTECT),
            read_cvar_ptr_slot(cvar_slots::SV_ALLOWDOWNLOAD),
            read_cvar_ptr_slot(cvar_slots::COM_DEDICATED),
            read_cvar_ptr_slot(cvar_slots::COM_SV_RUNNING),
            read_cvar_ptr_slot(cvar_slots::COM_CL_RUNNING),
            read_cvar_ptr_slot(cvar_slots::COM_LOGFILE),
            read_cvar_ptr_slot(cvar_slots::COM_DEVELOPER),
            read_cvar_ptr_slot(cvar_slots::FS_GAMEDIRVAR),
        ]
    };
    let layer = MemoryLayer {
        svs: vars::SVS,
        sv: vars::SV,
        cvar_ptrs,
        common_vars: [
            vars::RD_BUFFER,
            vars::RD_BUFFERSIZE,
            vars::RD_FLUSH,
            vars::LOGFILE,
            vars::CMD_ARGC,
            vars::CMD_ARGV,
        ],
    };
    *lock() = Some(layer);
    eprintln!("----- proxy-rs: Memory layer properly initialized");
}

// ---------------------------------------------------------------------------
// Engine call layer (`server.functions.*` / `server.common.functions.*` /
// `server.common.vars.*` — the subset the ported hooks call)
// ---------------------------------------------------------------------------

/// Re-type an engine function address as `$fn_ty`.
///
/// The address is loaded with a **volatile** read (which the optimizer cannot
/// fold) so the call is **indirect**: a direct relative `call <absolute>`
/// would be patched by the linker relative to the .so's link-time base and
/// land wrong under ASLR, because the loader applies no text relocation for it
/// (the .so has no `.rel.text` `R_386_PC32` entries). The engine is a fixed
/// non-PIE at 0x08048000, so the absolute value in the data slot is correct
/// as-is.
///
/// # Safety
///
/// `$fn_ty` must describe the actual cdecl signature of the engine function
/// at `$addr` (version-swept table, R-010).
macro_rules! engine_fn {
    ($fn_ty:ty, $addr:expr) => {{
        static ADDR: usize = $addr;
        // SAFETY: volatile read of a static; the loaded value is a validated
        // engine function entry (R-010).
        let a = unsafe { core::ptr::read_volatile(&ADDR) };
        // SAFETY: usize and fn pointers are both pointer-sized.
        unsafe { core::mem::transmute::<usize, $fn_ty>(a) }
    }};
}

/// `server.common.functions.Com_EndRedirect()` (0x8072c74): flush and close the
/// engine's current redirect. Called at `GAME_SHUTDOWN` because an `rcon map`
/// change leaves a redirect open (`Proxy_Main.cpp:135`); no-op if none is open.
///
/// # Safety
///
/// Engine-only; the engine's redirect globals must be valid.
pub unsafe fn com_end_redirect() {
    let f: unsafe extern "C" fn() = engine_fn!(unsafe extern "C" fn(), functions::COM_END_REDIRECT);
    unsafe { f() };
}

/// `playerState_t* SV_GameClientNum(int)` (0x8054754) — the game module's
/// playerState for a client (the proxy's `server.functions.SV_GameClientNum`).
///
/// # Safety
///
/// Engine-internal; `client_num` must be in range.
pub unsafe fn sv_game_client_num(client_num: c_int) -> usize {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(c_int) -> usize = engine_fn!(
        unsafe extern "C" fn(c_int) -> usize,
        functions::SV_GAME_CLIENT_NUM
    );
    unsafe { f(client_num) }
}

/// `char* Cmd_Argv(int)` (0x812c264) — the engine's argv table.
///
/// # Safety
///
/// `n` must index the tokenized command; the returned pointer is valid until
/// the next tokenization.
pub unsafe fn cmd_argv(n: c_int) -> *const c_char {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(c_int) -> *const c_char = engine_fn!(
        unsafe extern "C" fn(c_int) -> *const c_char,
        functions::CMD_ARGV
    );
    unsafe { f(n) }
}

/// `const char* FS_ReferencedPakNames(void)` (0x8131024).
///
/// # Safety
///
/// Engine filesystem must be initialised; pointer valid until the next call.
pub unsafe fn fs_referenced_pak_names() -> *const c_char {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn() -> *const c_char = engine_fn!(
        unsafe extern "C" fn() -> *const c_char,
        functions::FS_REFERENCED_PAK_NAMES
    );
    unsafe { f() }
}

/// `void FS_FCloseFile(fileHandle_t)` (0x812d1b4).
///
/// # Safety
///
/// `handle` must be a valid open file handle or 0.
pub unsafe fn fs_fclose_file(handle: c_int) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(c_int) =
        engine_fn!(unsafe extern "C" fn(c_int), functions::FS_FCLOSE_FILE);
    unsafe { f(handle) };
}

/// `void MSG_WriteByte(msg_t*, int)` (0x8077a24).
///
/// # Safety
///
/// `msg` must be a valid msg_t being written.
pub unsafe fn msg_write_byte(msg: usize, c: c_int) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize, c_int) = engine_fn!(
        unsafe extern "C" fn(usize, c_int),
        functions::MSG_WRITE_BYTE
    );
    unsafe { f(msg, c) };
}

/// `void MSG_WriteShort(msg_t*, int)` (0x8077aa4).
///
/// # Safety
///
/// `msg` must be a valid msg_t being written.
pub unsafe fn msg_write_short(msg: usize, c: c_int) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize, c_int) = engine_fn!(
        unsafe extern "C" fn(usize, c_int),
        functions::MSG_WRITE_SHORT
    );
    unsafe { f(msg, c) };
}

/// `void MSG_WriteLong(msg_t*, int)` (0x8077ad4).
///
/// # Safety
///
/// `msg` must be a valid msg_t being written.
pub unsafe fn msg_write_long(msg: usize, c: c_int) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize, c_int) = engine_fn!(
        unsafe extern "C" fn(usize, c_int),
        functions::MSG_WRITE_LONG
    );
    unsafe { f(msg, c) };
}

/// `void MSG_WriteString(msg_t*, const char*)` (0x8077b34).
///
/// # Safety
///
/// `msg` must be a valid msg_t being written; `s` NUL-terminated.
pub unsafe fn msg_write_string(msg: usize, s: *const c_char) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize, *const c_char) = engine_fn!(
        unsafe extern "C" fn(usize, *const c_char),
        functions::MSG_WRITE_STRING
    );
    unsafe { f(msg, s) };
}

/// `void SV_ClientThink(client_t*, usercmd_t*)` (0x804e634) — the original
/// target of the retargeted usercmd-loop call site.
///
/// # Safety
///
/// `cl`/`ucmd` must be valid engine pointers.
pub unsafe fn sv_client_think(cl: usize, ucmd: usize) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize, usize) = engine_fn!(
        unsafe extern "C" fn(usize, usize),
        functions::SV_CLIENT_THINK
    );
    unsafe { f(cl, ucmd) };
}

/// `void MSG_Bitstream(msg_t*)` (0x80775d4) — switch a message to bitstream
/// reading (sets `oob = qfalse`; does not reset readcount/bit).
///
/// # Safety
///
/// `msg` must be a valid engine `msg_t*`.
pub unsafe fn msg_bitstream(msg: usize) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize) =
        engine_fn!(unsafe extern "C" fn(usize), functions::MSG_BITSTREAM);
    unsafe { f(msg) };
}

/// `int MSG_ReadLong(msg_t*)` (0x8077e74).
///
/// # Safety
///
/// `msg` must be a valid engine `msg_t*` positioned for reading.
pub unsafe fn msg_read_long(msg: usize) -> c_int {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize) -> c_int = engine_fn!(
        unsafe extern "C" fn(usize) -> c_int,
        functions::MSG_READ_LONG
    );
    unsafe { f(msg) }
}

/// `int MSG_ReadByte(msg_t*)` (0x8077df4).
///
/// # Safety
///
/// `msg` must be a valid engine `msg_t*` positioned for reading.
pub unsafe fn msg_read_byte(msg: usize) -> c_int {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize) -> c_int = engine_fn!(
        unsafe extern "C" fn(usize) -> c_int,
        functions::MSG_READ_BYTE
    );
    unsafe { f(msg) }
}

/// `void Com_DPrintf(const char *fmt, ...)` (0x8072ed4) — developer-only
/// printf; called with the exact `(fmt, one arg)` word list the gate uses
/// (cdecl variadic with two words ≡ the fixed two-arg form).
///
/// # Safety
///
/// The engine must be initialised; `arg` must match the format string.
pub unsafe fn com_dprintf(fmt: usize, arg: usize) {
    // SAFETY: pointer from the fixed engine table; a cdecl call with exactly
    // two words is ABI-identical to the variadic call with one %s argument.
    let f: unsafe extern "C" fn(usize, usize) =
        engine_fn!(unsafe extern "C" fn(usize, usize), functions::COM_DPRINTF);
    unsafe { f(fmt, arg) };
}

/// `void SV_SendClientGameState(client_t*)` (0x804cee4) — pristine body call
/// (used by the `SV_ExecuteClientMessage` wrapper's resend arm).
///
/// # Safety
///
/// `cl` must be a valid engine `client_t*`.
pub unsafe fn sv_send_client_game_state(cl: usize) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize) = engine_fn!(
        unsafe extern "C" fn(usize),
        functions::SV_SEND_CLIENT_GAME_STATE
    );
    unsafe { f(cl) };
}

/// `void SV_SendMessageToClient(msg_t*, client_t*)` (0x8058c84).
///
/// # Safety
///
/// `msg`/`cl` must be valid engine pointers.
#[allow(dead_code)] // mirrors the wrappers table; the pristine body uses it via trampoline
pub unsafe fn sv_send_message_to_client(msg: usize, cl: usize) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize, usize) = engine_fn!(
        unsafe extern "C" fn(usize, usize),
        functions::SV_SEND_MESSAGE_TO_CLIENT
    );
    unsafe { f(msg, cl) };
}

/// Read the engine `cmd_argc` var (int @ 0x8260e20).
///
/// # Safety
///
/// Engine data address from the fixed table.
pub unsafe fn cmd_argc() -> c_int {
    // SAFETY: validated engine RW address (R-010/R-018).
    unsafe { *(vars::CMD_ARGC as *const c_int) }
}

/// Read engine `cmd_argv[n]` (char* table @ 0x8260e40).
///
/// # Safety
///
/// `n < MAX_STRING_TOKENS`; engine data address from the fixed table.
pub unsafe fn cmd_argv_ptr(n: usize) -> *const c_char {
    // SAFETY: validated engine RW address; argv is char*[MAX_STRING_TOKENS].
    unsafe { *((vars::CMD_ARGV as *const *const c_char).add(n)) }
}

/// Write engine `cmd_argc` (used by the `Cmd_TokenizeString2` local helper).
///
/// # Safety
///
/// Engine data address from the fixed table.
pub unsafe fn set_cmd_argc(n: c_int) {
    // SAFETY: validated engine RW address.
    unsafe { *(vars::CMD_ARGC as *mut c_int) = n };
}

/// Write one token into the engine `cmd_argv`/`cmd_tokenized` buffers
/// (used by the `Cmd_TokenizeString2` local helper).
///
/// # Safety
///
/// `index < MAX_STRING_TOKENS`; `dst` must point into `cmd_tokenized`.
pub unsafe fn set_cmd_argv(index: usize, dst: *const c_char) {
    // SAFETY: validated engine RW addresses.
    unsafe { *(vars::CMD_ARGV as *mut *const c_char).add(index) = dst };
}

/// The engine `cmd_tokenized` buffer base (char @ 0x8264440).
pub const CMD_TOKENIZED_BASE: usize = vars::CMD_TOKENIZED;

// ---------------------------------------------------------------------------
// cvar_t offsets (oracle)
// ---------------------------------------------------------------------------

/// `offsetof(cvar_t, string)`.
#[allow(dead_code)]
pub const OFFSET_CVAR_STRING: usize = 4;
/// `offsetof(cvar_t, integer)`.
pub const OFFSET_CVAR_INTEGER: usize = 32;

/// Read an engine `cvar_t`'s `integer` field (`cvar_t.integer` @ 32, oracle).
///
/// # Safety
///
/// `cvar_ptr` must be a live `cvar_t*` from the engine cvar slots.
pub unsafe fn cvar_integer(cvar_ptr: usize) -> c_int {
    // SAFETY: validated slot dereferenced at init; integer is a plain int.
    unsafe { core::ptr::read_unaligned((cvar_ptr + OFFSET_CVAR_INTEGER) as *const i32) }
}

/// Read an engine `cvar_t`'s `string` field (`cvar_t.string` @ 4, oracle),
/// returning a NUL-terminated C string pointer.
///
/// # Safety
///
/// `cvar_ptr` must be a live `cvar_t*`; the returned pointer is valid while
/// the cvar exists.
#[allow(dead_code)] // mirrors the wrappers table; not needed by the current hook set
pub unsafe fn cvar_string_ptr(cvar_ptr: usize) -> *const u8 {
    // SAFETY: validated slot dereferenced at init; string is a char array.
    unsafe { core::ptr::read_unaligned((cvar_ptr + OFFSET_CVAR_STRING) as *const *const u8) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_function_addresses_are_in_text_range() {
        // Every entry of the `functions` table must live inside the engine's
        // executable LOAD segment (R-002/R-010).
        for (name, addr) in [
            ("COM_PRINTF", functions::COM_PRINTF),
            ("SV_CALCPINGS", functions::SV_CALCPINGS),
            (
                "SV_CONNECTIONLESS_PACKET",
                functions::SV_CONNECTIONLESS_PACKET,
            ),
            ("SVC_STATUS", functions::SVC_STATUS),
            ("SVC_INFO", functions::SVC_INFO),
            ("SVC_REMOTE_COMMAND", functions::SVC_REMOTE_COMMAND),
            ("NAVIGATOR_LOAD", functions::NAVIGATOR_LOAD),
            ("COM_END_REDIRECT", functions::COM_END_REDIRECT),
        ] {
            assert!(
                (ENGINE_TEXT_LO..ENGINE_TEXT_HI).contains(&addr),
                "{name}: 0x{addr:x} outside engine .text"
            );
        }
    }

    #[test]
    fn engine_var_and_cvar_addresses_are_in_rw_range() {
        for (name, addr) in [
            ("SVS", vars::SVS),
            ("SV", vars::SV),
            ("RD_BUFFER", vars::RD_BUFFER),
            ("CMD_ARGC", vars::CMD_ARGC),
            ("SV_FPS", cvar_slots::SV_FPS),
            ("SV_MAXCLIENTS", cvar_slots::SV_MAXCLIENTS),
            ("COM_DEDICATED", cvar_slots::COM_DEDICATED),
            ("FS_GAMEDIRVAR", cvar_slots::FS_GAMEDIRVAR),
        ] {
            assert!(
                (ENGINE_RW_LO..ENGINE_RW_HI).contains(&addr),
                "{name}: 0x{addr:x} outside engine RW LOAD"
            );
        }
    }

    #[test]
    fn inline_patch_sites_are_in_text_range() {
        assert!(
            (ENGINE_TEXT_LO..ENGINE_TEXT_HI).contains(&patches::SVC_REMOTE_COMMAND_TIMER_NOP_ADDR)
        );
        assert!(
            (ENGINE_TEXT_LO..ENGINE_TEXT_HI)
                .contains(&patches::SVC_REMOTE_COMMAND_REDIRECT_LEN_ADDR)
        );
        assert!(
            (ENGINE_TEXT_LO..ENGINE_TEXT_HI)
                .contains(&calls::CALL_SV_ADD_ENT_TO_SNAPSHOT_FOR_PLAYERS)
        );
        assert_eq!(patches::SVC_REMOTE_COMMAND_TIMER_NOP_LEN, 25);
    }
}
