//! SDK ABI surface the proxy must reproduce exactly.
//!
//! Every number and layout here is either
//!   - an SDK enum ordinal (verified by compiling `g_public.h` with the i386
//!     toolchain, `tools/` oracle), or
//!   - a layout offset/size produced by the same compiler from the SDK headers
//!     (`original/jampgame-proxy/src/sdk/`, which is byte-identical to the
//!     pristine `original/jedi-academy-sdk/` for these types — see
//!     `docs/architecture.md` and U-003).
//!
//! Do not change a value without re-running the layout oracle and updating the
//! corresponding unit test.

use core::ffi::c_char;

// ---------------------------------------------------------------------------
// Constants (SDK `q_shared.h` / `g_local.h` / `qcommon.h`)
// ---------------------------------------------------------------------------

pub const MAX_CLIENTS: usize = 32;
pub const MAX_NETNAME: usize = 36;
pub const MAX_INFO_STRING: usize = 1024;
pub const MAX_STRING_CHARS: usize = 1024;
pub const MAX_TOKEN_CHARS: usize = 1024;
pub const MAX_CVAR_VALUE_STRING: usize = 256;
pub const MAX_OSPATH: usize = 4096; // PATH_MAX on Linux
pub const CVAR_ARCHIVE: u32 = 0x0000_0001;
pub const Q_COLOR_ESCAPE: u8 = b'^';

// angles indices (SDK `q_shared.h:346-348`)
pub const ROLL: usize = 2;

// force powers (SDK `q_shared.h:590-612`)
pub const FP_LEVITATION: u8 = 1;
pub const NUM_FORCE_POWERS: u8 = 18;

/// Version string the engine must report in its `version` cvar
/// (`Proxy_Header.hpp:31`, matches `linuxjampded`'s `JAmp: v1.0.1.1 …`).
pub const ORIGINAL_ENGINE_VERSION: &str = "JAmp: v1.0.1.1 linux-i386 Nov 10 2003";

/// Name the pristine game module is installed under next to the proxy.
pub const ORIGINAL_LIBRARY_NAME: &str = "jampgame_original.so";
/// Subdirectory the engine loads game modules from when `fs_game` is unset.
pub const DEFAULT_BASE_GAME_FOLDER_NAME: &str = "base";

// ---------------------------------------------------------------------------
// System trap ordinals (SDK `gameImport_t`, `g_public.h:102-276`)
//
// Consecutive from `G_PRINT = 0` up to the `G_MEMSET = 100` re-anchor; the
// values below were produced by compiling the SDK header (oracle), not by hand.
// Only the traps the proxy itself uses are listed.
// ---------------------------------------------------------------------------

pub const G_CVAR_REGISTER: i32 = 5;
pub const G_CVAR_UPDATE: i32 = 6;
pub const G_CVAR_VARIABLE_INTEGER_VALUE: i32 = 8;
pub const G_CVAR_VARIABLE_STRING_BUFFER: i32 = 9;
pub const G_ARGV: i32 = 11;
pub const G_LOCATE_GAME_DATA: i32 = 17;
pub const G_DROP_CLIENT: i32 = 18;
pub const G_SEND_SERVER_COMMAND: i32 = 19;
pub const G_GET_USERINFO: i32 = 22;
pub const G_SET_USERINFO: i32 = 23;
pub const G_GET_USERCMD: i32 = 40;

// ---------------------------------------------------------------------------
// vmMain commands (SDK `gameExport_t`, `g_public.h:735+`)
// ---------------------------------------------------------------------------

pub const GAME_INIT: i32 = 0;
pub const GAME_SHUTDOWN: i32 = 1;
pub const GAME_CLIENT_CONNECT: i32 = 2;
pub const GAME_CLIENT_BEGIN: i32 = 3;
pub const GAME_CLIENT_USERINFO_CHANGED: i32 = 4;
pub const GAME_CLIENT_DISCONNECT: i32 = 5;
pub const GAME_CLIENT_COMMAND: i32 = 6;
pub const GAME_RUN_FRAME: i32 = 8;

// ---------------------------------------------------------------------------
// Layout oracle: sizes/offsets produced by compiling the SDK headers with the
// i386 toolchain (g++ 12, Debian bookworm i386). The `cfg(test)` constants are
// the regression-gate pins consumed by the unit tests at the bottom of this
// file; the ones used by production code sit next to the fields they describe.
// Do not "optimise" the structures below away from these.
// ---------------------------------------------------------------------------

/// `sizeof(gentity_t) == 1516`, `offsetof(gentity_t, client) == 864`
/// (SDK `g_local.h:133`).
pub const SIZEOF_GENTITY: usize = 1516;
pub const OFFSET_GENTITY_CLIENT: usize = 864;

/// `sizeof(gclient_t) == 7232`, `offsetof(gclient_t, pers) == 1552`,
/// `offsetof(clientPersistant_t, netname) == 48` (SDK `g_local.h:440,535`).
#[allow(dead_code)] // sizeof pin; offsets below are what production code reads
pub const SIZEOF_GCLIENT: usize = 7232;
pub const OFFSET_GCLIENT_PERS: usize = 1552;
pub const OFFSET_PERS_NETNAME: usize = 48;

#[cfg(test)]
mod layout_pins {
    /// `sizeof(usercmd_t) == 28` (SDK `q_shared.h:2524`).
    pub const SIZEOF_USERCMD: usize = 28;
    /// `offsetof(usercmd_t, angles)`.
    pub const OFFSET_USERCMD_ANGLES: usize = 4;
    /// `offsetof(usercmd_t, forcesel)`.
    pub const OFFSET_USERCMD_FORCESEL: usize = 21;

    /// `sizeof(vmCvar_t) == 272` (SDK `q_shared.h:1412-1418`).
    pub const SIZEOF_VMCVAR: usize = 272;
    pub const OFFSET_VMCVAR_HANDLE: usize = 0;
    pub const OFFSET_VMCVAR_MODIFICATION_COUNT: usize = 4;
    pub const OFFSET_VMCVAR_VALUE: usize = 8;
    pub const OFFSET_VMCVAR_INTEGER: usize = 12;
    pub const OFFSET_VMCVAR_STRING: usize = 16;
}

// ---------------------------------------------------------------------------
// #[repr(C)] mirror of the SDK types the proxy passes to the engine
// ---------------------------------------------------------------------------

/// `usercmd_t` — the command the engine stores for a client
/// (SDK `q_shared.h:2524-2533`).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct Usercmd {
    pub server_time: i32,
    pub angles: [i32; 3],
    pub buttons: i32,
    pub weapon: u8,
    pub forcesel: u8,
    pub invensel: u8,
    pub generic_cmd: u8,
    pub forwardmove: i8,
    pub rightmove: i8,
    pub upmove: i8,
}

/// `vmCvar_t` — engine↔game cvar mirror (SDK `q_shared.h:1412-1418`).
/// The engine writes every field through `G_CVAR_REGISTER`/`G_CVAR_UPDATE`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VmCvar {
    pub handle: i32,
    pub modification_count: i32,
    pub value: f32,
    pub integer: i32,
    pub string: [c_char; MAX_CVAR_VALUE_STRING],
}

impl Default for VmCvar {
    fn default() -> Self {
        VmCvar {
            handle: 0,
            modification_count: 0,
            value: 0.0,
            integer: 0,
            string: [0; MAX_CVAR_VALUE_STRING],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::layout_pins::*;
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn usercmd_layout_matches_sdk() {
        assert_eq!(size_of::<Usercmd>(), SIZEOF_USERCMD);
        assert_eq!(align_of::<Usercmd>(), 4);
        assert_eq!(offset_of!(Usercmd, server_time), 0);
        assert_eq!(offset_of!(Usercmd, angles), OFFSET_USERCMD_ANGLES);
        assert_eq!(offset_of!(Usercmd, forcesel), OFFSET_USERCMD_FORCESEL);
    }

    #[test]
    fn vm_cvar_layout_matches_sdk() {
        assert_eq!(size_of::<VmCvar>(), SIZEOF_VMCVAR);
        assert_eq!(offset_of!(VmCvar, handle), OFFSET_VMCVAR_HANDLE);
        assert_eq!(
            offset_of!(VmCvar, modification_count),
            OFFSET_VMCVAR_MODIFICATION_COUNT
        );
        assert_eq!(offset_of!(VmCvar, value), OFFSET_VMCVAR_VALUE);
        assert_eq!(offset_of!(VmCvar, integer), OFFSET_VMCVAR_INTEGER);
        assert_eq!(offset_of!(VmCvar, string), OFFSET_VMCVAR_STRING);
    }

    #[test]
    fn trap_ordinals_match_sdk() {
        // Values verified by compiling g_public.h (oracle output committed in
        // the module docs); pin them so a header re-read cannot silently drift.
        assert_eq!(G_CVAR_REGISTER, 5);
        assert_eq!(G_CVAR_UPDATE, 6);
        assert_eq!(G_CVAR_VARIABLE_INTEGER_VALUE, 8);
        assert_eq!(G_CVAR_VARIABLE_STRING_BUFFER, 9);
        assert_eq!(G_ARGV, 11);
        assert_eq!(G_LOCATE_GAME_DATA, 17);
        assert_eq!(G_DROP_CLIENT, 18);
        assert_eq!(G_SEND_SERVER_COMMAND, 19);
        assert_eq!(G_GET_USERINFO, 22);
        assert_eq!(G_SET_USERINFO, 23);
        assert_eq!(G_GET_USERCMD, 40);
    }

    #[test]
    fn vm_main_commands_match_sdk() {
        assert_eq!(GAME_INIT, 0);
        assert_eq!(GAME_SHUTDOWN, 1);
        assert_eq!(GAME_CLIENT_CONNECT, 2);
        assert_eq!(GAME_CLIENT_BEGIN, 3);
        assert_eq!(GAME_CLIENT_USERINFO_CHANGED, 4);
        assert_eq!(GAME_CLIENT_DISCONNECT, 5);
        assert_eq!(GAME_CLIENT_COMMAND, 6);
        assert_eq!(GAME_RUN_FRAME, 8);
    }

    #[test]
    fn engine_version_gate_string_is_ascii() {
        assert!(ORIGINAL_ENGINE_VERSION.is_ascii());
        assert_eq!(
            ORIGINAL_ENGINE_VERSION.as_bytes(),
            b"JAmp: v1.0.1.1 linux-i386 Nov 10 2003"
        );
    }
}
