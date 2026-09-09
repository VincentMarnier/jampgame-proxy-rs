//! Proxy state — the Rust mirror of `Proxy_t` (`Proxy_Header.hpp:122-141`).
//!
//! Everything is plain integer/array data (`Send`) so the global can live in a
//! `Mutex` without any `unsafe impl`; raw pointers are re-materialised from the
//! integer addresses at the call sites (same convention as `original.rs`).
//!
//! The engine is single-threaded, but the game module's traps are forwarded
//! *inside* a `vmMain` while our own handlers may also be running, so all
//! state access goes through short-lived lock sections that never span an
//! engine or game call.

use core::ffi::CStr;
use std::sync::{Mutex, MutexGuard};

use crate::sdk::{MAX_CLIENTS, MAX_NETNAME, VmCvar};

/// The `proxy.cvars` mirror table (`Proxy_g_main.cpp:14-26`): each entry is
/// `(field-name, default string)`; `CVAR_ARCHIVE` is the flag for all of them.
pub const PROXY_CVARS: [(&CStr, &CStr); 10] = [
    (c"proxy_sv_pingFix", c"0"),
    (c"proxy_sv_enableRconCmdCooldown", c"0"),
    (c"proxy_sv_enableNetStatus", c"0"),
    (c"proxy_sv_antiWallHack", c"0"),
    (c"proxy_sv_maxCallVoteMapRestartValue", c"60"),
    (c"proxy_sv_modelPathLength", c"64"),
    (c"proxy_sv_disableKillCmd", c"0"),
    (c"proxy_sv_antiHpTeller", c"0"),
    (c"proxy_sv_minJumpTime", c"0"),
    (c"proxy_sv_sabersFps", c"0"),
];

/// Indexes into `PROXY_CVARS` / `ProxyState::cvars` (mirror of
/// `proxy.cvars.*`, `Proxy_Header.hpp:108-120`). Entries not yet consulted by
/// a ported handler are marked for the hook milestones.
#[allow(dead_code)] // consulted when the hook milestones land
pub const CVAR_PING_FIX: usize = 0;
#[allow(dead_code)]
pub const CVAR_ENABLE_RCON_CMD_COOLDOWN: usize = 1;
pub const CVAR_ENABLE_NET_STATUS: usize = 2;
#[allow(dead_code)]
pub const CVAR_ANTI_WALL_HACK: usize = 3;
pub const CVAR_MAX_CALLVOTE_MAPRESTART: usize = 4;
pub const CVAR_MODEL_PATH_LENGTH: usize = 5;
pub const CVAR_DISABLE_KILL_CMD: usize = 6;
#[allow(dead_code)]
pub const CVAR_ANTI_HP_TELLER: usize = 7;
#[allow(dead_code)]
pub const CVAR_MIN_JUMP_TIME: usize = 8;
#[allow(dead_code)]
pub const CVAR_SABERS_FPS: usize = 9;

/// `LocatedGameData_t` (`Proxy_Header.hpp:69-77`) — recorded by the
/// `G_LOCATE_GAME_DATA` trap interception. Addresses kept as `usize`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocatedGameData {
    pub g_entities: usize,
    pub num_entities: i32,
    pub g_entity_size: i32,
    pub g_clients: usize,
    pub g_client_size: i32,
}

/// Per-client bookkeeping (`Proxy_Header.hpp:87-106`). Only the fields the
/// currently-ported handlers touch are mirrored; the netStatus/timenudge
/// fields arrive with the hook milestones.
#[derive(Debug, Clone, Copy)]
pub struct ClientEntry {
    pub is_connected: bool,
    pub clean_name: [u8; MAX_NETNAME],
}

impl Default for ClientEntry {
    fn default() -> Self {
        ClientEntry {
            is_connected: false,
            clean_name: [0; MAX_NETNAME],
        }
    }
}

/// The full proxy state.
#[derive(Debug, Default)]
pub struct ProxyState {
    pub located_game_data: LocatedGameData,
    pub svs_time: i32,
    pub clients: [ClientEntry; MAX_CLIENTS],
    pub cvars: [VmCvar; PROXY_CVARS.len()],
}

static STATE: Mutex<Option<ProxyState>> = Mutex::new(None);

/// The engine's single thread owns all state; `Mutex::lock` poisoning is
/// impossible in practice (panic = abort), but handle it without unwinding.
fn lock() -> MutexGuard<'static, Option<ProxyState>> {
    STATE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Run `f` with exclusive access to the proxy state, initialising it on first
/// use.
pub fn with_state<R>(f: impl FnOnce(&mut ProxyState) -> R) -> R {
    let mut guard = lock();
    let state = guard.get_or_insert_with(ProxyState::default);
    f(state)
}

/// Whether proxy state has been initialised yet (i.e. `dllEntry` ran).
#[cfg(test)]
pub fn is_initialised() -> bool {
    lock().is_some()
}

/// `GAME_RUN_FRAME` bookkeeping (`Proxy_Main.cpp:169`).
pub fn set_svs_time(time: i32) {
    with_state(|s| s.svs_time = time);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cvar_table_has_distinct_names() {
        let mut names: Vec<_> = PROXY_CVARS.iter().map(|(n, _)| n.to_bytes()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), PROXY_CVARS.len());
    }

    #[test]
    fn default_cvar_values_match_proxy() {
        // The two non-zero defaults the SharedAPI logic depends on
        // (`Proxy_g_main.cpp:20-21`).
        assert_eq!(
            PROXY_CVARS[CVAR_MAX_CALLVOTE_MAPRESTART].1.to_bytes(),
            b"60"
        );
        assert_eq!(PROXY_CVARS[CVAR_MODEL_PATH_LENGTH].1.to_bytes(), b"64");
        assert_eq!(PROXY_CVARS[CVAR_PING_FIX].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_ENABLE_NET_STATUS].1.to_bytes(), b"0");
    }

    #[test]
    fn state_initialises_once() {
        assert!(!is_initialised());
        with_state(|s| s.svs_time = 42);
        assert!(is_initialised());
        with_state(|s| assert_eq!(s.svs_time, 42));
        // Reset for the next test that touches the global.
        *lock() = None;
    }
}
