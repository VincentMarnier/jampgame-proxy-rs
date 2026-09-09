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

/// `server.common.functions.Com_EndRedirect()` (0x8072c74): flush and close the
/// engine's current redirect. Called at `GAME_SHUTDOWN` because an `rcon map`
/// change leaves a redirect open (`Proxy_Main.cpp:135`); no-op if none is open.
///
/// # Safety
///
/// Engine-only; the engine's redirect globals must be valid.
pub unsafe fn com_end_redirect() {
    let f: unsafe extern "C" fn() = unsafe { core::mem::transmute(functions::COM_END_REDIRECT) };
    unsafe { f() };
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
