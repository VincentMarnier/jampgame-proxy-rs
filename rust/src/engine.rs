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
    /// `Info_Print` (`0x8073504`): retail `char key[512]` / `char value[512]`
    /// stack buffers; rewritten with growable buffers (S7/10-25 F11).
    pub const INFO_PRINT: usize = 0x0807_3504;
    /// `Com_WriteConfig_f` (`0x8074d64`): `.cfg`-only / no `..` / reserved
    /// names check (S12/10-25 F7).
    pub const COM_WRITE_CONFIG_F: usize = 0x0807_4d64;
    /// `Com_WriteConfigToFile(const char*)` (`0x8074cb4`), called by the
    /// rewritten command.
    pub const COM_WRITE_CONFIG_TO_FILE: usize = 0x0807_4cb4;
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
    /// `void SV_SendClientMessages(void)` (`0x8059034`): `SV_Frame` calls it
    /// once per server frame, after the `GAME_RUN_FRAME` chunk loop and
    /// `SV_CheckTimeouts` (call site `0x8057a36`). This is the demo recorder's
    /// hook point (run after the original returns).
    pub const SV_SEND_CLIENT_MESSAGES: usize = 0x0805_9034;
    /// `void SV_Frame(int msec)` (`0x8057844`), reference only.
    pub const SV_FRAME: usize = 0x0805_7844;
    /// `static void SV_WriteSnapshotToClient(client_t*, msg_t*)`
    /// (`0x8058034`), reference only (the recorder replicates its layout).
    pub const SV_WRITE_SNAPSHOT_TO_CLIENT: usize = 0x0805_8034;
    /// `static void SV_EmitPacketEntities(clientSnapshot_t *from,
    /// clientSnapshot_t *to, msg_t *msg)` (`0x8057e64`), reference only.
    pub const SV_EMIT_PACKET_ENTITIES: usize = 0x0805_7e64;
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
    /// `MSG_ReadBits` (`0x8077834`): no bound in the retail code
    /// (CVE-2017-11721); wrapped with the ioq3/OpenJK bounds (S13/O11).
    pub const MSG_READ_BITS: usize = 0x0807_7834;
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
    /// `void MSG_WriteBits(msg_t*, int value, int bits)` (`0x8077634`):
    /// whole bytes are encoded as symbols of the static `msg_hData` Huffman
    /// tree (`MSG_initHuffman` preloads it; the writer does not adapt per
    /// message).
    pub const MSG_WRITE_BITS: usize = 0x0807_7634;
    /// `void MSG_WriteDeltaPlayerstate(msg_t*, playerState_t *from,
    /// playerState_t *to, qboolean isVehiclePS)` (`0x80799c4`): the compiled
    /// function used by `SV_WriteSnapshotToClient` (five call sites at
    /// `0x8058132/0x805816f/0x8058198/0x8058203/0x8058236`). The shipped
    /// binary has `_OPTIMIZED_VEHICLE_NETWORKING`: the non-vehicle path writes
    /// a leading 1-bit "pilot playerstate" flag.
    pub const MSG_WRITE_DELTA_PLAYERSTATE: usize = 0x0807_99c4;
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
    /// `Z_TagFree(memtag_t)` (`0x80804e4`): frees every allocation of a tag
    /// (S20 frees `TAG_BSP_DISKIMAGE` after `GAME_INIT`).
    pub const Z_TAG_FREE: usize = 0x0808_04e4;
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

    /// `Info_SetValueForKey` length test: `cmp $0x400, %ebx; jbe` at
    /// `0x807fe24`. `jbe` lets a pair that brings the string to exactly
    /// `MAX_INFO_STRING` through, and the `strcat`/`strcpy` then write the
    /// terminator one byte past the caller's 1024-byte buffer (O12/S8).
    /// Changing `0x76` to `0x72` (`jb`) refuses that boundary.
    pub const INFO_SET_VALUE_FOR_KEY_LENGTH_JBE_ADDR: usize = 0x0807_fe24;
    /// Same site in `Info_SetValueForKey_Big` (`cmp $0x2000`), at
    /// `0x8080024`.
    pub const INFO_SET_VALUE_FOR_KEY_BIG_LENGTH_JBE_ADDR: usize = 0x0808_0024;
    /// The retail `jbe` (`0x76`) and the patched `jb` (`0x72`).
    pub const INFO_SET_VALUE_FOR_KEY_JBE_PRISTINE: u8 = 0x76;
    pub const INFO_SET_VALUE_FOR_KEY_JBE_PATCHED: u8 = 0x72;

    /// `SV_MapRestart_f`: `client->state = CS_ACTIVE` at `0x804f23f`
    /// (`c7 04 0e 04 00 00 00`, 7 bytes), followed by the argument setup and
    /// the `SV_ClientEnterWorld` call at `0x804f253`. The patched sequence
    /// compares the state and skips the call for a client that is still
    /// `CS_CONNECTED`/`CS_PRIMED` (S21 / upstream `48d4da5`).
    pub const SV_MAP_RESTART_SET_ACTIVE_ADDR: usize = 0x0804_f23f;
    pub const SV_MAP_RESTART_SET_ACTIVE_PRISTINE: [u8; 7] =
        [0xc7, 0x04, 0x0e, 0x04, 0x00, 0x00, 0x00];
    /// `cmp $4,(%esi,%ecx,1); jne +0x13; nop` — the jump lands at
    /// `0x804f258`, right after the `call` (which ends there).
    pub const SV_MAP_RESTART_SET_ACTIVE_PATCHED: [u8; 7] =
        [0x83, 0x3c, 0x0e, 0x04, 0x75, 0x13, 0x90];
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
    /// `TheRandomMissionManager` pointer (`SV_SendClientGameState` checks it
    /// at `0x804d12a`); null on a normal map.
    pub const THE_RANDOM_MISSION_MANAGER: usize = 0x0839_4b50;
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

// The macro is used by the hook modules too: a direct `call <absolute>` to an
// engine address would be encoded relative to the .so's link-time base and
// land at `base + address` under ASLR (observed: `Z_TagFree`, and every other
// constant-transmute call site listed in D-018). Routing every such call
// through this macro keeps it indirect.
pub(crate) use engine_fn;

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

/// `void Z_TagFree(memtag_t)` (0x80804e4): frees every allocation of a tag.
///
/// # Safety
///
/// Engine-only; the tag must be a valid `memtag_t` and the engine initialised.
pub unsafe fn z_tag_free(tag: c_int) {
    // Indirect through the wrapper macro: a direct `call <absolute>` would be
    // encoded relative to the .so's link-time base and land anywhere under
    // ASLR (see the `engine_fn!` note above).
    let f: unsafe extern "C" fn(c_int) =
        engine_fn!(unsafe extern "C" fn(c_int), functions::Z_TAG_FREE);
    unsafe { f(tag) };
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

/// `void MSG_WriteBits(msg_t*, int value, int bits)` (0x8077634).
///
/// # Safety
///
/// `msg` must be a valid msg_t being written. A negative `bits` means a signed
/// value (the engine writes the same bit count; the sign only affects reads).
#[allow(dead_code)] // used by the demo recorder (Phase 2)
pub unsafe fn msg_write_bits(msg: usize, value: c_int, bits: c_int) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize, c_int, c_int) = engine_fn!(
        unsafe extern "C" fn(usize, c_int, c_int),
        functions::MSG_WRITE_BITS
    );
    unsafe { f(msg, value, bits) };
}

/// `void MSG_WriteDeltaPlayerstate(msg_t*, playerState_t *from,
/// playerState_t *to, qboolean isVehiclePS)` (0x80799c4).
///
/// `from` may be 0 for a non-delta playerstate. The shipped binary has the
/// `_OPTIMIZED_VEHICLE_NETWORKING` layout: the non-vehicle path starts with a
/// one-bit pilot flag, so callers must not pre-write anything.
///
/// # Safety
///
/// `msg` must be a valid msg_t; `from` must be 0 or point at a `playerState_t`;
/// `to` must point at a `playerState_t`; `is_vehicle` must be 0 or 1.
#[allow(dead_code)] // used by the demo recorder (Phase 2)
pub unsafe fn msg_write_delta_playerstate(msg: usize, from: usize, to: usize, is_vehicle: c_int) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize, usize, usize, c_int) = engine_fn!(
        unsafe extern "C" fn(usize, usize, usize, c_int),
        functions::MSG_WRITE_DELTA_PLAYERSTATE
    );
    unsafe { f(msg, from, to, is_vehicle) };
}

/// `void MSG_WriteData(msg_t*, const void*, int)` (0x8077a54).
///
/// # Safety
///
/// `msg` must be a valid msg_t being written; `data` must be readable for
/// `len` bytes.
pub unsafe fn msg_write_data(msg: usize, data: *const u8, len: c_int) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize, *const u8, c_int) = engine_fn!(
        unsafe extern "C" fn(usize, *const u8, c_int),
        functions::MSG_WRITE_DATA
    );
    unsafe { f(msg, data, len) };
}

/// `void MSG_WriteDeltaEntity(msg_t*, entityState_t *from,
/// entityState_t *to, qboolean force)` (0x8078d74).
///
/// # Safety
///
/// `msg` must be a valid msg_t; `from`/`to` must be null or point at a live
/// `entityState_t` (532 bytes).
pub unsafe fn msg_write_delta_entity(msg: usize, from: usize, to: usize, force: c_int) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize, usize, usize, c_int) = engine_fn!(
        unsafe extern "C" fn(usize, usize, usize, c_int),
        functions::MSG_WRITE_DELTA_ENTITY
    );
    unsafe { f(msg, from, to, force) };
}

/// `void MSG_Init(msg_t*, byte *data, int length)` (0x80774a4).
///
/// # Safety
///
/// `msg` must point at a writable `msg_t`; `data` must be writable for
/// `length` bytes and stay alive while the message is used.
pub unsafe fn msg_init(msg: usize, data: *mut u8, length: c_int) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(usize, *mut u8, c_int) = engine_fn!(
        unsafe extern "C" fn(usize, *mut u8, c_int),
        functions::MSG_INIT
    );
    unsafe { f(msg, data, length) };
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

/// `void SV_Trace(trace_t *results, const vec3_t start, const vec3_t mins,
/// const vec3_t maxs, const vec3_t end, int passEntityNum, int contentmask,
/// int capsule, int traceFlags, int useLod)` (0x805a4c4) — the server's
/// world+entity trace (`sv_world.cpp:792`). The anti-wallhack uses a point box
/// for line of sight and the client's ±`CAMERA_SIZE` box for the camera
/// collision estimate.
///
/// # Safety
///
/// `results` must point at a writable `trace_t`; engine memory layer and game
/// module must be live.
pub unsafe fn sv_trace(
    results: &mut crate::sdk::Trace,
    start: [f32; 3],
    mins: [f32; 3],
    maxs: [f32; 3],
    end: [f32; 3],
    pass_entity_num: c_int,
    contentmask: c_int,
) {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn(
        *mut crate::sdk::Trace,
        *const f32,
        *const f32,
        *const f32,
        *const f32,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
    ) = engine_fn!(
        unsafe extern "C" fn(
            *mut crate::sdk::Trace,
            *const f32,
            *const f32,
            *const f32,
            *const f32,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
        ),
        functions::SV_TRACE
    );
    unsafe {
        f(
            results,
            start.as_ptr(),
            mins.as_ptr(),
            maxs.as_ptr(),
            end.as_ptr(),
            pass_entity_num,
            contentmask,
            0,
            0,
            0,
        )
    };
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

/// `void SV_SendClientMessages(void)` (0x8059034): the engine's per-frame
/// snapshot loop. The demo recorder runs its own build after this returns
/// (see `functions::SV_SEND_CLIENT_MESSAGES`).
///
/// # Safety
///
/// Engine-only; the server must be running (`sv.state`, `svs.clients` live).
#[allow(dead_code)] // used by the demo recorder hook (Phase 2)
pub unsafe fn sv_send_client_messages() {
    // SAFETY: pointer from the fixed engine table.
    let f: unsafe extern "C" fn() =
        engine_fn!(unsafe extern "C" fn(), functions::SV_SEND_CLIENT_MESSAGES);
    unsafe { f() };
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
    fn demo_recording_addresses_are_in_text_range() {
        // Phase 0 pins for server-side demo recording (docs/demo-format.md):
        // MSG_WriteBits / MSG_WriteDeltaPlayerstate, the SV_Frame hook point
        // and the reference snapshot functions.
        let pins = [
            ("MSG_WRITE_BITS", functions::MSG_WRITE_BITS),
            (
                "MSG_WRITE_DELTA_PLAYERSTATE",
                functions::MSG_WRITE_DELTA_PLAYERSTATE,
            ),
            ("MSG_WRITE_DELTA_ENTITY", functions::MSG_WRITE_DELTA_ENTITY),
            ("MSG_WRITE_DATA", functions::MSG_WRITE_DATA),
            ("MSG_INIT", functions::MSG_INIT),
            (
                "SV_SEND_CLIENT_MESSAGES",
                functions::SV_SEND_CLIENT_MESSAGES,
            ),
            ("SV_FRAME", functions::SV_FRAME),
            (
                "SV_WRITE_SNAPSHOT_TO_CLIENT",
                functions::SV_WRITE_SNAPSHOT_TO_CLIENT,
            ),
            (
                "SV_EMIT_PACKET_ENTITIES",
                functions::SV_EMIT_PACKET_ENTITIES,
            ),
        ];
        for (name, addr) in pins {
            assert!(
                (ENGINE_TEXT_LO..ENGINE_TEXT_HI).contains(&addr),
                "{name}: 0x{addr:x} outside engine .text"
            );
        }
        // Distinct function entries (a copy/paste must not collapse two pins).
        for i in 0..pins.len() {
            for j in (i + 1)..pins.len() {
                assert_ne!(pins[i].1, pins[j].1, "{} == {}", pins[i].0, pins[j].0);
            }
        }
    }

    #[test]
    fn engine_var_and_cvar_addresses_are_in_rw_range() {
        for (name, addr) in [
            ("SVS", vars::SVS),
            ("SV", vars::SV),
            (
                "THE_RANDOM_MISSION_MANAGER",
                vars::THE_RANDOM_MISSION_MANAGER,
            ),
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
