//! S22 (2026 `a0d322b`): reload the map before an engine counter wraps.
//!
//! `SV_Frame` stops the server before two counters wrap:
//! - `svs.time` past `0x70000000` (about 21.7 days): only `SV_Shutdown` resets
//!   it;
//! - `svs.nextSnapshotEntities` near `0x7FFFFFFE`: it grows by the entities of
//!   every snapshot built, and a map change resets it, so a server that stays
//!   on one map with many players reaches it in hours.
//!
//! It then calls `SV_Shutdown` and `map_restart 0`, but `map_restart` refuses
//! to run once the server is down ("Server is not running."): every player is
//! dropped and the server stays without a map until someone restarts it.
//!
//! The proxy acts a little before those limits. Its command is appended at the
//! next `Cbuf_Execute` of `Com_Frame`, before the next `SV_Frame` and its
//! checks:
//! - for `svs.nextSnapshotEntities`, `map <current map>` — a normal map change,
//!   the players stay connected;
//! - for `svs.time`, `killserver` then `map <current map>`, as ioq3 and OpenJK
//!   do after their shutdown. The players are dropped.

use core::ffi::c_int;
use core::sync::atomic::{AtomicBool, Ordering};
use std::ffi::CString;

use crate::engine;
use crate::sdk::{
    EXEC_APPEND, G_SEND_CONSOLE_COMMAND, MAX_QPATH, OFFSET_SVS_NEXT_SNAPSHOT_ENTITIES,
    OFFSET_SVS_NUM_SNAPSHOT_ENTITIES, OFFSET_SVS_TIME,
};
use crate::syscall;

/// `SV_Frame`'s `svs.time > 0x70000000` check.
pub const TIME_WRAP_LIMIT: i32 = 0x7000_0000;
/// Act one minute before `SV_Frame` would stop the server.
pub const TIME_WRAP_MARGIN_MS: i32 = 60_000;
/// `SV_Frame`'s `svs.nextSnapshotEntities >= 0x7FFFFFFE - svs.numSnapshotEntities`
/// check (the proxy uses a wider `2 * numSnapshotEntities` margin).
pub const ENTITY_WRAP_LIMIT: i32 = 0x7FFF_FFFE;

/// Which engine counter is about to wrap, in `SV_Frame`'s check order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapReason {
    Time,
    SnapshotEntities,
}

impl WrapReason {
    /// The counter's name in the engine (`SV_Frame` message and log line).
    pub fn name(self) -> &'static str {
        match self {
            WrapReason::Time => "svs.time",
            WrapReason::SnapshotEntities => "svs.nextSnapshotEntities",
        }
    }
}

/// `Proxy_Engine_Restart_BeforeWrap`'s decision, kept pure for testing.
///
/// Time wins when both counters are near their limit, as upstream
/// (`timeWraps ? "svs.time" : "svs.nextSnapshotEntities"`) because only the
/// time wrap needs `killserver` first.
pub fn wrap_reason(
    time: i32,
    next_snapshot_entities: i32,
    num_snapshot_entities: i32,
) -> Option<WrapReason> {
    // One minute before the limit of SV_Frame.
    if time > TIME_WRAP_LIMIT - TIME_WRAP_MARGIN_MS {
        return Some(WrapReason::Time);
    }
    // One numSnapshotEntities before the limit of SV_Frame. A server frame adds
    // at most sv_maxclients snapshots of MAX_SNAPSHOT_ENTITIES (256), and
    // numSnapshotEntities is at least sv_maxclients * 4 * 64.
    let margin = num_snapshot_entities.max(0).saturating_mul(2);
    if next_snapshot_entities >= ENTITY_WRAP_LIMIT - margin {
        return Some(WrapReason::SnapshotEntities);
    }
    None
}

/// The `EXEC_APPEND` text for a wrap: a plain map change for the snapshot
/// counter, `killserver` then the map for the time counter (upstream
/// `va("%smap %s\n", timeWraps ? "killserver\n" : "", mapname)`).
pub fn restart_command(reason: WrapReason, mapname: &str) -> String {
    let prefix = match reason {
        WrapReason::Time => "killserver\n",
        WrapReason::SnapshotEntities => "",
    };
    format!("{prefix}map {mapname}\n")
}

/// The console line upstream prints (`trap->Print`).
pub fn log_line(reason: WrapReason, mapname: &str) -> String {
    format!(
        "----- proxy-rs: {} is about to wrap, loading {} again\n",
        reason.name(),
        mapname
    )
}

/// Once per module load. Upstream's static lives in the game module, which the
/// engine reloads on a map change *and* on a `map_restart`; our module is
/// pinned across map changes (D-007), so `game_init` resets this instead.
static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Reset the once-per-load flag at `GAME_INIT` (both a full map change and a
/// `map_restart`, matching the upstream module reload).
pub fn reset_for_load() {
    REQUESTED.store(false, Ordering::Relaxed);
}

/// `Proxy_Engine_Restart_BeforeWrap`: called at each `GAME_RUN_FRAME`, before
/// the original module, so the appended command runs at the next
/// `Cbuf_Execute` before `SV_Frame`'s own checks.
///
/// # Safety
///
/// Engine memory layer initialised; `GAME_RUN_FRAME` only runs while the
/// server is up. The engine syscall pointer is registered.
pub unsafe fn before_wrap() {
    if REQUESTED.load(Ordering::Relaxed) {
        return;
    }

    // SAFETY: engine memory layer initialised (GAME_RUN_FRAME after GAME_INIT).
    let (time, next, num) = unsafe {
        (
            engine_sv_i32(OFFSET_SVS_TIME),
            engine_sv_i32(OFFSET_SVS_NEXT_SNAPSHOT_ENTITIES),
            engine_sv_i32(OFFSET_SVS_NUM_SNAPSHOT_ENTITIES),
        )
    };
    let Some(reason) = wrap_reason(time, next, num) else {
        return;
    };

    let mut mapname = [0u8; MAX_QPATH];
    // SAFETY: engine syscall registered; the buffer is writable and the name is
    // a literal.
    unsafe { syscall::cvar_variable_string_buffer(c"mapname", &mut mapname) };
    let len = mapname
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(mapname.len());
    let mapname = String::from_utf8_lossy(&mapname[..len]);

    let (Ok(line), Ok(command)) = (
        CString::new(log_line(reason, &mapname)),
        CString::new(restart_command(reason, &mapname)),
    ) else {
        return;
    };
    // SAFETY: both strings are NUL-terminated; the engine syscall is registered.
    // `EXEC_APPEND` runs the command at the next `Cbuf_Execute`.
    unsafe {
        syscall::print(&line);
        syscall::call_engine(
            G_SEND_CONSOLE_COMMAND,
            &[EXEC_APPEND, command.as_ptr() as c_int],
        );
    }

    REQUESTED.store(true, Ordering::Relaxed);
}

/// Read one `i32` field of `serverStatic_t` (`svs` at `engine::vars::SVS`).
///
/// # Safety
///
/// Engine memory layer initialised; `offset` is a validated field offset.
#[inline]
unsafe fn engine_sv_i32(offset: usize) -> i32 {
    // SAFETY: caller guarantees the address is a readable engine static.
    unsafe { core::ptr::read_unaligned((engine::vars::SVS + offset) as *const i32) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::{OFFSET_SVS_CHALLENGES, OFFSET_SVS_CLIENTS};

    #[test]
    fn wrap_reason_uses_the_upstream_thresholds() {
        // `time > 0x70000000 - 60000`, strictly.
        let below = TIME_WRAP_LIMIT - TIME_WRAP_MARGIN_MS;
        assert_eq!(wrap_reason(below, 0, 1000), None);
        assert_eq!(wrap_reason(below + 1, 0, 1000), Some(WrapReason::Time));

        // `next >= 0x7FFFFFFE - 2 * numSnapshotEntities`.
        let num = 1000;
        let threshold = ENTITY_WRAP_LIMIT - 2 * num;
        assert_eq!(wrap_reason(0, threshold - 1, num), None);
        assert_eq!(
            wrap_reason(0, threshold, num),
            Some(WrapReason::SnapshotEntities)
        );

        // Time wins when both are near their limit (upstream picks killserver).
        assert_eq!(
            wrap_reason(below + 1, threshold, num),
            Some(WrapReason::Time)
        );

        // An unset counter leaves only the entity check.
        assert_eq!(wrap_reason(0, 0, 0), None);
    }

    #[test]
    fn restart_commands_match_the_upstream_text() {
        assert_eq!(
            restart_command(WrapReason::SnapshotEntities, "mp/ffa3"),
            "map mp/ffa3\n"
        );
        assert_eq!(
            restart_command(WrapReason::Time, "mp/ffa3"),
            "killserver\nmap mp/ffa3\n"
        );
        assert_eq!(
            WrapReason::SnapshotEntities.name(),
            "svs.nextSnapshotEntities"
        );
        assert_eq!(WrapReason::Time.name(), "svs.time");
    }

    #[test]
    fn server_static_offsets_match_the_sdk_layout() {
        // serverStatic_t prefix: initialized (0), time (4), snapFlagServerBit
        // (8), clients (12), numSnapshotEntities (16), nextSnapshotEntities
        // (20), snapshotEntities (24), nextHeartbeatTime (28), challenges (32).
        assert_eq!(OFFSET_SVS_TIME, 4);
        assert_eq!(OFFSET_SVS_CLIENTS, 12);
        assert_eq!(OFFSET_SVS_NUM_SNAPSHOT_ENTITIES, 16);
        assert_eq!(OFFSET_SVS_NEXT_SNAPSHOT_ENTITIES, 20);
        assert_eq!(
            OFFSET_SVS_CHALLENGES,
            OFFSET_SVS_NEXT_SNAPSHOT_ENTITIES + 12
        );
    }
}
