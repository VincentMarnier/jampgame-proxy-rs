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
use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::sdk::{MAX_CLIENTS, MAX_NETNAME, VmCvar};

/// The `proxy.cvars` mirror table (D-002 keep list): each entry is
/// `(field-name, default string)`; `CVAR_ARCHIVE` is the flag for all of them.
/// The four dropped cvars (`pingFix`, `antiWallHack`, `disableKillCmd`,
/// `sabersFps`) are gone with their features.
pub const PROXY_CVARS: [(&CStr, &CStr); 8] = [
    (c"proxy_sv_enable", c"1"),
    (c"proxy_sv_enableRconCmdCooldown", c"0"),
    (c"proxy_sv_enableNetStatus", c"0"),
    (c"proxy_sv_maxCallVoteMapRestartValue", c"60"),
    (c"proxy_sv_modelPathLength", c"64"),
    (c"proxy_sv_antiHpTeller", c"0"),
    (c"proxy_sv_minJumpTime", c"0"),
    (c"proxy_sv_enableEndGameStats", c"1"),
];

/// Indexes into `PROXY_CVARS` / `ProxyState::cvars` (mirror of
/// `proxy.cvars.*`, `Proxy_Header.hpp:108-120`, D-002 keep order).
pub const CVAR_ENABLE: usize = 0;
pub const CVAR_ENABLE_RCON_CMD_COOLDOWN: usize = 1;
pub const CVAR_ENABLE_NET_STATUS: usize = 2;
pub const CVAR_MAX_CALLVOTE_MAPRESTART: usize = 3;
pub const CVAR_MODEL_PATH_LENGTH: usize = 4;
pub const CVAR_ANTI_HP_TELLER: usize = 5;
pub const CVAR_MIN_JUMP_TIME: usize = 6;
pub const CVAR_ENABLE_END_GAME_STATS: usize = 7;

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

/// `timenudgeData_t` (`Proxy_Header.hpp:48-54`) — netStatus delay/ping samples.
#[derive(Debug, Default, Clone, Copy)]
pub struct TimenudgeData {
    pub delay_count: i32,
    pub delay_sum: i32,
    pub ping_sum: i32,
    pub last_time_nudge_calculation: i32,
}

/// `ucmdStat_t` (`Proxy_Header.hpp:56-60`) — one per-usercmd stats ring slot.
#[derive(Debug, Default, Clone, Copy)]
pub struct UcmdStat {
    pub server_time: i32,
    pub packet_index: usize,
}

/// `GameStats_t` (`Proxy_Header.hpp:79-85`) — per-(client, opponent) tallies.
#[derive(Debug, Default, Clone, Copy)]
pub struct GameStats {
    pub killed: i32,
    pub killed_by: i32,
    pub damages_given: i32,
    pub damages_taken: i32,
}

/// Per-client bookkeeping (`Proxy_Header.hpp:87-106`).
#[derive(Debug, Clone, Copy)]
pub struct ClientEntry {
    pub is_connected: bool,
    pub clean_name: [u8; MAX_NETNAME],
    pub timenudge_data: TimenudgeData,
    pub timenudge: i32,
    pub last_time_net_status: i32,
    pub cmd_stats: [UcmdStat; crate::sdk::CMD_MASK],
    pub cmd_index: usize,
    /// Monotonic packet-identity counter for the cmdStats feed: bumped once
    /// per usercmd client message by the `SV_ExecuteClientMessage` entry
    /// wrapper (see `netstatus::mark_packet_start`), reproducing the
    /// original's pre-loop `cmdIndex` capture.
    pub packet_counter: u32,
    pub game_stats: [GameStats; MAX_CLIENTS],
    pub team_kills: i32,
    pub team_killed: i32,
    pub team_damages_given: i32,
    pub team_damages_taken: i32,
}

impl Default for ClientEntry {
    fn default() -> Self {
        ClientEntry {
            is_connected: false,
            clean_name: [0; MAX_NETNAME],
            timenudge_data: TimenudgeData::default(),
            timenudge: 0,
            last_time_net_status: 0,
            cmd_stats: [UcmdStat::default(); crate::sdk::CMD_MASK],
            cmd_index: 0,
            packet_counter: 0,
            game_stats: [GameStats::default(); MAX_CLIENTS],
            team_kills: 0,
            team_killed: 0,
            team_damages_given: 0,
            team_damages_taken: 0,
        }
    }
}

/// The full proxy state. The per-client table is built element-wise on the
/// heap: a `Box<[ClientEntry; MAX_CLIENTS]>: Default` would materialise the
/// whole ~280 KiB array on the *stack* inside whatever `with_state` call site
/// the compiler inlines the lazy-init into (observed live: a ~278 KB
/// stack-probing prologue in the netStatus wrappers — fatal on deep engine
/// stacks), so the default constructs `ClientEntry`s one at a time.
#[derive(Debug)]
pub struct ProxyState {
    pub located_game_data: LocatedGameData,
    pub svs_time: i32,
    pub clients: Box<[ClientEntry]>,
    pub cvars: [VmCvar; PROXY_CVARS.len()],
    pub jump_start_time: [i32; MAX_CLIENTS],
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

/// Heap-built per-client table: element-wise construction keeps the default
/// off the stack (see `ProxyState`).
fn default_clients() -> Box<[ClientEntry]> {
    (0..MAX_CLIENTS)
        .map(|_| ClientEntry::default())
        .collect::<Vec<ClientEntry>>()
        .into_boxed_slice()
}

impl ProxyState {
    fn default() -> Self {
        ProxyState {
            located_game_data: LocatedGameData::default(),
            svs_time: 0,
            clients: default_clients(),
            cvars: core::array::from_fn(|_| VmCvar::default()),
            jump_start_time: [0; MAX_CLIENTS],
        }
    }
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

// ---------------------------------------------------------------------------
// Master enable switch (mirror of `proxy_sv_enable`, refreshed each frame)
// ---------------------------------------------------------------------------

/// Whether the proxy's hooks/interceptions are active. Defaults to `true` so
/// the narrow window between `dllEntry` and the first `GAME_RUN_FRAME` (before
/// the cvar mirror is refreshed) keeps the proxy on, matching the cvar default.
static PROXY_ENABLED: AtomicBool = AtomicBool::new(true);

/// Set the master switch (called once the `proxy_sv_enable` cvar is refreshed).
pub fn set_proxy_enabled(enabled: bool) {
    PROXY_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether the proxy is currently enabled. Lock-free: read from hot paths
/// (`G_GET_USERCMD` trap, every `vmMain`).
pub fn proxy_enabled() -> bool {
    PROXY_ENABLED.load(Ordering::Relaxed)
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
        assert_eq!(PROXY_CVARS[CVAR_ENABLE_NET_STATUS].1.to_bytes(), b"0");
        assert_eq!(
            PROXY_CVARS[CVAR_ENABLE_RCON_CMD_COOLDOWN].1.to_bytes(),
            b"0"
        );
        assert_eq!(PROXY_CVARS[CVAR_ENABLE_END_GAME_STATS].1.to_bytes(), b"1");
        assert_eq!(PROXY_CVARS[CVAR_ENABLE].1.to_bytes(), b"1");
    }

    #[test]
    fn dropped_cvars_are_gone() {
        // D-002: the not-wanted features are dropped entirely, cvars included.
        for (name, _) in PROXY_CVARS {
            let n = name.to_bytes();
            assert!(
                !n.starts_with(b"proxy_sv_pingFix")
                    && !n.starts_with(b"proxy_sv_antiWallHack")
                    && !n.starts_with(b"proxy_sv_disableKillCmd")
                    && !n.starts_with(b"proxy_sv_sabersFps"),
                "dropped cvar still registered: {}",
                String::from_utf8_lossy(n)
            );
        }
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
