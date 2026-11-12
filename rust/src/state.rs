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

use crate::sdk::{MAX_CLIENTS, MAX_GENTITIES, MAX_NETNAME, VmCvar};

/// The `proxy.cvars` mirror table (D-002 keep list): each entry is
/// `(field-name, default string)`; `CVAR_ARCHIVE` is the flag for all of them.
/// The four dropped cvars (`pingFix`, `disableKillCmd`, `sabersFps`, and the
/// original `antiWallHack` implementation) are gone with their features.
/// `proxy_sv_antiWallHack` is re-added by D-023 as a new implementation that
/// reuses the D-022 hide path. `proxy_sv_enableRconCmdCooldown` was removed with
/// the 2026 rcon rate-limit policy (10-25 F6, S11).
pub const PROXY_CVARS: [(&CStr, &CStr); 60] = [
    (c"proxy_sv_enable", c"1"),
    (c"proxy_sv_enableNetStatus", c"0"),
    (c"proxy_sv_maxCallVoteMapRestartValue", c"60"),
    (c"proxy_sv_modelPathLength", c"64"),
    (c"proxy_sv_antiHpTeller", c"0"),
    (c"proxy_sv_minJumpTime", c"0"),
    (c"proxy_sv_enableEndGameStats", c"1"),
    (c"proxy_sv_lockTeamsAfterXSecs", c"0"),
    (c"proxy_sv_teamSizeRules", c""),
    (c"proxy_parallel_tffas_enable", c"0"),
    (c"proxy_parallel_tffas_maxMatches", c"8"),
    (c"proxy_parallel_tffas_maxGroupSize", c"12"),
    (c"proxy_sv_spectatorTeamOverlay", c"1"),
    (c"proxy_sv_muteMode", c"1"),
    (c"proxy_sv_debugLog", c"0"),
    (c"proxy_sv_enableClientPpsLimit", c"0"),
    (c"proxy_sv_globalMaxPps", c"100"),
    (c"proxy_sv_clientPpsLog", c"0"),
    (c"proxy_sv_voidTimeNudge", c"0"),
    (c"proxy_sv_voidTimeNudgeLog", c"0"),
    (c"proxy_sv_enableIgnore", c"0"),
    (c"proxy_sv_enableBanFile", c"0"),
    (c"proxy_sv_banFile", c"proxy_bans.txt"),
    (c"proxy_sv_enableGuidBan", c"0"),
    (c"proxy_sv_guidBanFile", c"proxy_guid_bans.txt"),
    (c"proxy_sv_antiMacro", c"0"),
    (c"proxy_sv_antiMacroLog", c"0"),
    (c"proxy_sv_antiMacroView", c"0"),
    (c"proxy_sv_maxFov", c"0"),
    (c"proxy_sv_maxFovAspect", c"1.7777778"),
    (c"proxy_sv_maxFovCameraRange", c"100"),
    (c"proxy_sv_maxFovMargin", c"0"),
    (c"proxy_sv_clientPpsFpsMargin", c"2"),
    (c"proxy_sv_timeNudgeMargin", c"2"),
    (c"proxy_sv_antiMacroMinReversals", c"4"),
    (c"proxy_sv_antiMacroAxisMaxMeanCmds", c"3"),
    (c"proxy_sv_antiMacroViewMaxMeanMs", c"65"),
    (c"proxy_sv_antiMacroAttackMaxMeanMs", c"60"),
    (c"proxy_sv_antiMacroMaxHoldSpreadMs", c"10"),
    (c"proxy_sv_antiMacroViewStepPercent", c"75"),
    (c"proxy_sv_antiMacroViewMinSwing", c"364"),
    (c"proxy_sv_clientPpsTolerance", c"1"),
    (c"proxy_sv_antiWallHack", c"0"),
    (c"proxy_sv_antiWallHackMargin", c"0"),
    (c"proxy_sv_antiWallHackCameraRange", c"100"),
    (c"proxy_parallel_duels_enable", c"0"),
    // Legacy spellings of the parallel-TFFA cvars, kept for retro-compatibility
    // after the D-026 rename. They are registered so an old config/server
    // script keeps working (and shows up in `cvarlist`), but the code only
    // reads them as a fallback: see [`cvar_integer`] and [`legacy_alias`].
    (c"proxy_tffa_enable", c"0"),
    (c"proxy_tffa_maxMatches", c"8"),
    (c"proxy_tffa_maxGroupSize", c"12"),
    // Phase 2 demo recording (docs/demo-format.md).
    (c"proxy_sv_enableDemoRecord", c"0"),
    (c"proxy_sv_demoDir", c"demos"),
    // Phase 4 publication (docs/demo-format.md §13): HTTP surface, demo
    // downloads, links, retention and the accepted disk guards.
    (c"proxy_sv_enableHttp", c"0"),
    (c"proxy_sv_httpBind", c"127.0.0.1"),
    (c"proxy_sv_httpPort", c"8090"),
    (c"proxy_sv_enableDemoDownload", c"0"),
    (c"proxy_sv_demoUrl", c""),
    (c"proxy_sv_demoRetentionHours", c"24"),
    (c"proxy_sv_demoMaxFileMB", c"128"),
    (c"proxy_sv_demoMinFreeMB", c"512"),
    (c"proxy_sv_demoIntermissionSecs", c"3"),
];

/// Indexes into `PROXY_CVARS` / `ProxyState::cvars` (mirror of
/// `proxy.cvars.*`, `Proxy_Header.hpp:108-120`, D-002 keep order).
pub const CVAR_ENABLE: usize = 0;
pub const CVAR_ENABLE_NET_STATUS: usize = 1;
pub const CVAR_MAX_CALLVOTE_MAPRESTART: usize = 2;
pub const CVAR_MODEL_PATH_LENGTH: usize = 3;
pub const CVAR_ANTI_HP_TELLER: usize = 4;
pub const CVAR_MIN_JUMP_TIME: usize = 5;
pub const CVAR_ENABLE_END_GAME_STATS: usize = 6;
/// Team-lock grace delay in seconds before a round's teams lock (`0` = off).
pub const CVAR_LOCK_TEAMS_AFTER_SECS: usize = 7;
/// Per-team-size `timelimit`/`fraglimit`/`capturelimit` rules (reconciled every
/// frame with the roster, independent of `proxy_sv_lockTeamsAfterXSecs`).
pub const CVAR_TEAM_SIZE_RULES: usize = 8;
/// Parallel TFFA matches (`GT_TEAM` only): `0` off, non-zero on
/// (`proxy_parallel_tffas_enable`; the pre-D-026 `proxy_tffa_enable` spelling
/// is still read as its alias, see [`cvar_integer`]).
pub const CVAR_TFFA_ENABLE: usize = 9;
/// Maximum number of parallel matches (`proxy_parallel_tffas_maxMatches`).
pub const CVAR_TFFA_MAX_MATCHES: usize = 10;
/// Maximum players per match, both teams combined
/// (`proxy_parallel_tffas_maxGroupSize`).
pub const CVAR_TFFA_MAX_GROUP_SIZE: usize = 11;
/// Spectator team overlay (`crate::teamoverlay`): while on, a spectator
/// following a player receives the stock `tinfo` roster (HP/armor/powerups) for
/// the followed player's team, which the pristine game only sends to teammates.
pub const CVAR_SPECTATOR_TEAM_OVERLAY: usize = 12;
/// `mute`/`unmute` behaviour (D-011, `crate::mute`): `0` off, `1` the stock
/// mute (chat dropped and the player told), `2` silent (the player's own chat
/// line is echoed back to them, nobody else receives it, no notice is sent).
pub const CVAR_MUTE_MODE: usize = 13;
/// Crash-diagnostics trace (`crate::diagnostics`): `0` off, `1` suspicious
/// events only, `2` every client command/userinfo/lifecycle event.
pub const CVAR_DEBUG_LOG: usize = 14;
/// Per-client incoming packet-rate limiter (`crate::pps`): `0` off, non-zero
/// on. When on, each client's limit is an integer downscale of its measured
/// FPS, capped by [`CVAR_GLOBAL_MAX_PPS`].
pub const CVAR_ENABLE_CLIENT_PPS_LIMIT: usize = 15;
/// Global hard ceiling on accepted packets/second per client (default `100`,
/// `0` = no ceiling). The per-client FPS-derived limit is capped to it.
pub const CVAR_GLOBAL_MAX_PPS: usize = 16;
/// `proxy_sv_clientPpsLog`: print one line for a client whose window is above
/// its limit (clients inside their limit stay silent).
pub const CVAR_CLIENT_PPS_LOG: usize = 17;
/// `proxy_sv_voidTimeNudge`: rewrite the usercmd `serverTime` of a client whose
/// estimated `cl_timeNudge` range excludes 0 (D-015, `crate::timenudge`).
pub const CVAR_VOID_TIME_NUDGE: usize = 18;
/// `proxy_sv_voidTimeNudgeLog`: log the non-zero/back-to-zero transitions;
/// works without `proxy_sv_voidTimeNudge` (the shift is reported, not applied).
pub const CVAR_VOID_TIME_NUDGE_LOG: usize = 19;
/// Player `/ignore` (D-016, `crate::ignore`): `0` off, non-zero on. When off,
/// `ignore`/`unignore` only answer that the feature is disabled and no chat is
/// filtered, even for a player still carrying a mask.
pub const CVAR_ENABLE_IGNORE: usize = 20;
/// File-backed IP bans (D-017, `crate::banfile`): `0` off (the pristine
/// `addip`/`removeip`/`listip` handlers run and no connect is checked), non-zero
/// on (`addip`/`removeip` edit [`CVAR_BAN_FILE`], `listip` prints it and every
/// connect is checked against it).
pub const CVAR_ENABLE_BAN_FILE: usize = 21;
/// Path of the ban file (relative to the server's working directory, or
/// absolute). Ignored when [`CVAR_ENABLE_BAN_FILE`] is `0`.
pub const CVAR_BAN_FILE: usize = 22;
/// File-backed `ja_guid` bans (D-019, `crate::guidban`): `0` off, non-zero on.
/// Separate registry and cvar from the IP list: `ja_guid` is client-asserted
/// userinfo, so it must not be able to add an IP entry.
pub const CVAR_ENABLE_GUID_BAN: usize = 23;
/// Path of the GUID ban file (relative to the server's working directory, or
/// absolute). Ignored when [`CVAR_ENABLE_GUID_BAN`] is `0`.
pub const CVAR_GUID_BAN_FILE: usize = 24;
/// Anti-macro input guard (`crate::macroguard`): `0` off, non-zero on (locked
/// usercmd channels are frozen). Detection always runs while this or
/// [`CVAR_ANTI_MACRO_LOG`] is on.
pub const CVAR_ANTI_MACRO: usize = 25;
/// `proxy_sv_antiMacroLog`: `0` off, `1` log the lock/release transitions,
/// `2` add the statistics of locked channels once per second, `3` add the
/// statistics of every channel with a reversal (diagnostic). Works without
/// `proxy_sv_antiMacro` (reports what would be frozen).
pub const CVAR_ANTI_MACRO_LOG: usize = 26;
/// `proxy_sv_antiMacroView`: `0` (default) observe and log the view channels but
/// never freeze them; non-zero freezes a locked `yaw`/`pitch`. Hand mouse play is
/// too close to a key-driven view wiggle at the wire level, so enforcement is
/// opt-in; the movement and attack channels are always covered by
/// [`CVAR_ANTI_MACRO`].
pub const CVAR_ANTI_MACRO_VIEW: usize = 27;
/// `proxy_sv_maxFov`: maximum `cg_fov` value whose rendered wedge is sent to a
/// viewer (`0` = off). Players outside the wedge are hidden for that viewer by
/// `crate::fov` (mind-trick bit + 1% model scale + a hidden blade), so a
/// client-side `cg_fov` wider than the cap shows no extra players.
pub const CVAR_MAX_FOV: usize = 28;
/// `proxy_sv_maxFovAspect`: reference aspect ratio used to turn the `cg_fov`
/// cap into a rendered horizontal wedge. Modern clients (`cg_fovAspectAdjust`
/// default 1 in OpenJK/TaystJK) widen the horizontal FOV on widescreen; the
/// default `1.7777778` is 16:9 (use `1.3333334` for 4:3/stock behaviour, more
/// for ultrawide).
pub const CVAR_MAX_FOV_ASPECT: usize = 29;
/// `proxy_sv_maxFovCameraRange`: third-person camera setback, in world units,
/// used when testing the wedge. Default `100` matches `cg_thirdPersonRange`;
/// `0` means test from the player's eye (first-person-strict).
pub const CVAR_MAX_FOV_CAMERA_RANGE: usize = 30;
/// `proxy_sv_maxFovMargin`: world-unit margin added to every side of the FOV
/// visibility volume. A player whose body or drawn saber comes within this many
/// world units of the frustum stays visible, so border contacts do not pop in
/// and out. `0` means the exact frustum (previous behaviour).
pub const CVAR_MAX_FOV_MARGIN: usize = 31;
/// `proxy_sv_clientPpsFpsMargin`: half-width, in FPS, of the uncertainty band
/// around the client's measured FPS used to derive the packet-rate limit
/// (`crate::pps`). The limit is the highest value the downscale formula
/// produces over `fps - margin ..= fps + margin`, so the band can only raise
/// the limit. Default `2` (the historical fixed value); `0` uses the measured
/// FPS alone.
pub const CVAR_CLIENT_PPS_FPS_MARGIN: usize = 32;
/// `proxy_sv_timeNudgeMargin`: bias floor, in milliseconds, added to the
/// `cl_timeNudge` estimator's confidence interval (`crate::netstatus_est`,
/// D-007/D-015). It also guards the verdict: the range must clear 0 by the
/// margin (the minimum for a positive reading, the maximum for a negative
/// one) before `proxy_sv_voidTimeNudge` acts and before the netStatus
/// `timeNudge` cell reports a confident non-zero setting.
/// Default `2.0` (the historical fixed value); `0` reports the statistical
/// interval alone.
pub const CVAR_TIME_NUDGE_MARGIN: usize = 33;
/// `proxy_sv_antiMacroMinReversals`: reversals in a run needed for the rate
/// test to fire (`crate::macroguard`). Raising it is the main false-positive
/// brake; the wary shortcut still needs [`WARY_MIN_REVERSALS`].
///
/// [`WARY_MIN_REVERSALS`]: crate::macroguard::WARY_MIN_REVERSALS
pub const CVAR_ANTI_MACRO_MIN_REVERSALS: usize = 34;
/// `proxy_sv_antiMacroAxisMaxMeanCmds`: movement channels fire when the run's
/// mean interval is at most this many commands (the acceleration payoff is per
/// command).
pub const CVAR_ANTI_MACRO_AXIS_MAX_MEAN_CMDS: usize = 35;
/// `proxy_sv_antiMacroViewMaxMeanMs`: `yaw`/`pitch` fire when the run's mean
/// interval is at most this many milliseconds (the wiggle payoff is per
/// second).
pub const CVAR_ANTI_MACRO_VIEW_MAX_MEAN_MS: usize = 36;
/// `proxy_sv_antiMacroAttackMaxMeanMs`: the attack channel fires when the run's
/// mean interval is at most this many milliseconds.
pub const CVAR_ANTI_MACRO_ATTACK_MAX_MEAN_MS: usize = 37;
/// `proxy_sv_antiMacroMaxHoldSpreadMs`: the `wait`-exactness test: a run fires
/// when its shortest and longest hold differ by at most this many
/// milliseconds. Lower is stricter.
pub const CVAR_ANTI_MACRO_MAX_HOLD_SPREAD_MS: usize = 38;
/// `proxy_sv_antiMacroViewStepPercent`: key-vs-mouse shape test for the view:
/// the mean per-command step must be at least this percentage of the peak
/// (a held turn key is near 100 %, a mouse wiggle accelerates and decelerates).
pub const CVAR_ANTI_MACRO_VIEW_STEP_PERCENT: usize = 39;
/// `proxy_sv_antiMacroViewMinSwing`: minimum view swing over a run, in
/// `ANGLE2SHORT` units (`364` ≈ 2° peak-to-peak), for a view verdict. `0`
/// removes the floor.
pub const CVAR_ANTI_MACRO_VIEW_MIN_SWING: usize = 40;
/// `proxy_sv_clientPpsTolerance`: packets per second of slack added to the
/// derived packet-rate limit **after** the FPS band and the global ceiling
/// (`crate::pps`). It absorbs the small difference between a client's real
/// send rate and the server's per-second measurement: with the default `1`, a
/// client sending `101` packets/s is accepted when the derived limit is `100`.
/// `0` restores the exact limit.
pub const CVAR_CLIENT_PPS_TOLERANCE: usize = 41;
/// `proxy_sv_antiWallHack`: hide a player from a viewer when no line of sight
/// from that viewer's camera reaches any part of the player's body or drawn
/// saber (`0` = off). Uses the same per-viewer hide as [`CVAR_MAX_FOV`]
/// (`EF_NODRAW` + mind-trick bit + 1% model scale + hidden blade), so the
/// player's footsteps, swings and saber hum keep playing. See `crate::fov`
/// and D-023.
pub const CVAR_ANTI_WALLHACK: usize = 42;
/// `proxy_sv_antiWallHackMargin`: world-unit margin added around the player's
/// sampled body box for the line-of-sight test. A player whose invisible
/// silhouette is within this many world units of a clear line stays visible,
/// so cover-edge contacts do not pop in and out. `0` is the exact body box.
pub const CVAR_ANTI_WALLHACK_MARGIN: usize = 43;
/// `proxy_sv_antiWallHackCameraRange`: third-person camera setback, in world
/// units, used as the start of the line-of-sight traces. Default `100` matches
/// the stock `cg_thirdPersonRange`; `0` traces from the player's eye
/// (first-person-strict).
pub const CVAR_ANTI_WALLHACK_CAMERA_RANGE: usize = 44;
/// `proxy_parallel_duels_enable`: run the parallel duel ladder on
/// `GT_DUEL` (`crate::duel`, D-025). `0` off (stock duel tournament), non-zero
/// on. One shared instance core serves both this and
/// `proxy_parallel_tffas_enable`.
pub const CVAR_PARALLEL_DUELS_ENABLE: usize = 45;

/// Legacy `proxy_tffa_enable` spelling (D-026), the pre-rename name of
/// [`CVAR_TFFA_ENABLE`]. Registered for retro-compatibility; read only as a
/// fallback when the canonical cvar still holds its default.
pub const CVAR_LEGACY_TFFA_ENABLE: usize = 46;
/// Legacy `proxy_tffa_maxMatches` spelling (D-026).
pub const CVAR_LEGACY_TFFA_MAX_MATCHES: usize = 47;
/// Legacy `proxy_tffa_maxGroupSize` spelling (D-026).
pub const CVAR_LEGACY_TFFA_MAX_GROUP_SIZE: usize = 48;
/// `proxy_sv_enableDemoRecord`: master switch for server-side demo recording.
pub const CVAR_DEMO_ENABLE: usize = 49;
/// `proxy_sv_demoDir`: output directory for `.dm_26` files.
pub const CVAR_DEMO_DIR: usize = 50;
/// `proxy_sv_enableHttp`: master switch for the proxy's own HTTP listener
/// (`crate::http`). The listener only starts when a route is enabled as well
/// (`proxy_sv_enableDemoDownload`), so turning downloads off really stops the
/// socket instead of leaving an empty server bound.
pub const CVAR_ENABLE_HTTP: usize = 51;
/// `proxy_sv_httpBind`: address the listener binds (`127.0.0.1` by default,
/// so nothing is exposed unless the owner opts in).
pub const CVAR_HTTP_BIND: usize = 52;
/// `proxy_sv_httpPort`: listener port (`8090` by default).
pub const CVAR_HTTP_PORT: usize = 53;
/// `proxy_sv_enableDemoDownload`: enables the `/demos` HTTP routes, the
/// `/demos` client command and the boundary announcements.
pub const CVAR_DEMO_DOWNLOAD: usize = 54;
/// `proxy_sv_demoUrl`: public base used to build links (e.g.
/// `https://example.com`). When empty the listener's own address is used
/// (`http://<proxy_sv_httpBind>:<proxy_sv_httpPort>`, a wildcard bind shown as
/// `127.0.0.1`). This is not an external-hosting switch: the proxy always
/// serves the files itself.
pub const CVAR_DEMO_URL: usize = 55;
/// `proxy_sv_demoRetentionHours`: delete matching demo files older than this
/// many hours at each game boundary (`0` = keep forever).
pub const CVAR_DEMO_RETENTION_HOURS: usize = 56;
/// `proxy_sv_demoMaxFileMB`: close a segment that reaches this size (`0` =
/// no cap) and continue with the next segment number.
pub const CVAR_DEMO_MAX_FILE_MB: usize = 57;
/// `proxy_sv_demoMinFreeMB`: stop recording (and close open segments) while
/// the demo directory's filesystem has less free space than this (`0` = off).
pub const CVAR_DEMO_MIN_FREE_MB: usize = 58;
/// `proxy_sv_demoIntermissionSecs`: seconds of the intermission to record
/// after `BeginIntermission` (so the demo shows the final scores) before the
/// segments are closed and the download links are printed to the clients.
/// `0` closes immediately at `BeginIntermission`. The engine's intermission is
/// at least 4 s (duel between rounds) / 5 s (other modes); a larger value may
/// fall back to the next game start.
pub const CVAR_DEMO_INTERMISSION_SECS: usize = 59;

/// The pre-rename spelling of a canonical cvar, if it has one (D-026).
///
/// Only the parallel-TFFA cvars were renamed so far; every other index maps to
/// `None`.
pub fn legacy_alias(index: usize) -> Option<usize> {
    match index {
        CVAR_TFFA_ENABLE => Some(CVAR_LEGACY_TFFA_ENABLE),
        CVAR_TFFA_MAX_MATCHES => Some(CVAR_LEGACY_TFFA_MAX_MATCHES),
        CVAR_TFFA_MAX_GROUP_SIZE => Some(CVAR_LEGACY_TFFA_MAX_GROUP_SIZE),
        _ => None,
    }
}

/// Which instancing feature owns the shared instance core right now.
///
/// Both features are overlays on the one pristine game module and reuse the
/// same per-instance machinery (assignment slate, isolation gates, world
/// clones, acting scope, per-viewer cs plumbing). They are mutually exclusive
/// by gametype: parallel TFFA only runs on `GT_TEAM`, parallel duels only on
/// `GT_DUEL`, so a gametype can never select both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstanceMode {
    /// No instancing: pristine game behaviour.
    #[default]
    Off,
    /// Parallel self-organized TFFA matches (`GT_TEAM`, D-006).
    Tffa,
    /// Parallel duels on a ladder (`GT_DUEL`, D-025).
    Duel,
}

impl InstanceMode {
    /// Whether either instancing feature is active.
    pub fn is_on(self) -> bool {
        self != InstanceMode::Off
    }

    /// Whether the active feature is the team-match policy.
    pub fn is_team(self) -> bool {
        self == InstanceMode::Tffa
    }
}

/// The active instance mode: master switch, the feature cvar and `g_gametype`.
///
/// Read on hot paths (every damage, trace, snapshot and client command), but
/// only two cvar reads and one engine cvar query after the state check; a
/// server with both features off pays the master-switch read and one state
/// lock.
pub fn instance_mode() -> InstanceMode {
    if !proxy_enabled() {
        return InstanceMode::Off;
    }
    let (tffa_on, duel_on) = with_state(|s| {
        (
            cvar_integer_in(s, CVAR_TFFA_ENABLE) != 0,
            s.cvars[CVAR_PARALLEL_DUELS_ENABLE].integer != 0,
        )
    });
    if !tffa_on && !duel_on {
        return InstanceMode::Off;
    }
    // SAFETY: engine syscall registered (GAME_INIT ran before any hook/frame).
    let gt = unsafe { crate::syscall::cvar_variable_integer_value(c"g_gametype") };
    split_mode(true, tffa_on, duel_on, gt)
}

/// Pure mode split: master switch, the two feature cvars and `g_gametype`.
/// Split out so the mutually-exclusive-gametype rule is unit-tested without an
/// engine (the caller reads the cvar through a trap).
pub fn split_mode(proxy: bool, tffa_on: bool, duel_on: bool, gametype: i32) -> InstanceMode {
    if !proxy {
        return InstanceMode::Off;
    }
    match gametype {
        crate::sdk::GT_TEAM if tffa_on => InstanceMode::Tffa,
        crate::sdk::GT_DUEL if duel_on => InstanceMode::Duel,
        _ => InstanceMode::Off,
    }
}

/// Whether the shared instance core is active (either feature on its
/// gametype). Gates the instance-wide machinery: isolation, per-viewer cs,
/// acting scopes, world clones.
pub fn instances_enabled() -> bool {
    instance_mode().is_on()
}

/// Whether parallel TFFA (team matches) is active: the TFFA-only policy
/// (teams, rules, passwords, promotion, team chat, `tffa_*` commands).
pub fn team_mode() -> bool {
    instance_mode().is_team()
}

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

/// One processed usercmd packet — a `clc_move`/`clc_moveNoDelta` message as
/// the netStatus estimator sees it (D-007). The per-cmd feed fills the *open*
/// record; `mark_packet_start` commits it and starts the next one, so the ring
/// holds one record per usercmd message that delivered at least one cmd.
#[derive(Debug, Default, Clone, Copy)]
pub struct PacketRecord {
    /// Sum of the processed cmds' `serverTime` (`Tbar_k = sum / count`).
    pub stamp_sum: i64,
    /// `svs.time` at the frame processing the packet (`A_k`) — the same sample
    /// the engine writes into `frames[].messageAcked` for the acked snapshot
    /// (SV_UserMove writes it before the `SV_ClientThink` loop).
    pub arrival: i32,
    /// `frames[messageAcknowledge & PACKET_MASK].messageSent` of the snapshot
    /// frame the packet acked — `0` when the slot carried no usable sample
    /// (`messageAcked <= 0` or a stale/wrapped slot).
    pub message_sent: i32,
    /// Newest `serverTime` in the packet (`s_max_k`).
    pub newest: i32,
    /// Oldest `serverTime` in the packet (the packet's stamp span measures the
    /// client's cmd interval exactly).
    pub oldest: i32,
    /// `messageAcknowledge` of the packet — the netchan sequence of the newest
    /// server message the client had seen when it sent the packet.
    pub msg_ack: i32,
    /// Processed usercmds in the packet (`n_k`): new cmds only — the engine
    /// drops `cl_packetdup` duplicates before the feed.
    pub count: u16,
}

/// Fixed per-packet estimation window (`netstatus`, D-007): 512 records cover
/// the ~5 s window up to ~100 packets/s (`cl_maxpackets` max) — a faster
/// client (LAN, one packet per frame) gets a proportionally shorter window.
pub const PACKET_RING_LEN: usize = 512;

/// Per-client netStatus estimator state (D-007). Replaces the original
/// proxy's `timenudgeData_t`/`ucmdStat_t` accumulators: everything is a
/// fixed-size ring, nothing allocates on the move-packet hot path.
#[derive(Debug, Clone, Copy)]
pub struct NetstatusState {
    /// Committed packet records, oldest at `head` when the ring has wrapped.
    pub packets: [PacketRecord; PACKET_RING_LEN],
    /// Next slot to write (the ring is full once `len == PACKET_RING_LEN`).
    pub head: usize,
    /// Committed records currently held (`0..=PACKET_RING_LEN`).
    pub len: usize,
    /// The packet the per-cmd feed is currently filling.
    pub open: PacketRecord,
    /// `cl->snapshotMsec` seen at the last check (a change resets the window).
    pub snapshot_msec: i32,
    /// `sv_fps` seen at the last check (a change resets the window).
    pub sv_fps: i32,
    /// Set when a window reset has been requested but not yet applied (the
    /// per-cmd feed may run mid-packet; the reset lands at the next packet).
    pub reset_pending: bool,
}

impl Default for NetstatusState {
    fn default() -> Self {
        NetstatusState {
            packets: core::array::from_fn(|_| PacketRecord::default()),
            head: 0,
            len: 0,
            open: PacketRecord::default(),
            snapshot_msec: 0,
            sv_fps: 0,
            reset_pending: false,
        }
    }
}

impl NetstatusState {
    /// Drop every window record and the open packet (map change, connect,
    /// disconnect, `sv_fps`/`snapshotMsec` change, netStatus off->on).
    pub fn reset(&mut self) {
        for record in self.packets.iter_mut() {
            *record = PacketRecord::default();
        }
        self.head = 0;
        self.len = 0;
        self.open = PacketRecord::default();
        self.reset_pending = false;
    }

    /// Commit the open packet (when it carried cmds) and start a new one
    /// stamped with this frame's `svs.time`, the acked snapshot's
    /// `messageSent`, and the acked message's netchan sequence.
    pub fn mark_packet_start(&mut self, arrival: i32, message_sent: i32, msg_ack: i32) {
        if self.reset_pending {
            self.reset();
        }
        if self.open.count > 0 {
            self.packets[self.head] = self.open;
            self.head = (self.head + 1) % PACKET_RING_LEN;
            self.len = (self.len + 1).min(PACKET_RING_LEN);
        }
        self.open = PacketRecord {
            stamp_sum: 0,
            arrival,
            message_sent,
            newest: i32::MIN,
            oldest: i32::MAX,
            msg_ack,
            count: 0,
        };
    }

    /// Accumulate one processed usercmd into the open packet.
    pub fn feed_cmd(&mut self, server_time: i32) {
        self.open.stamp_sum += server_time as i64;
        self.open.count = self.open.count.saturating_add(1);
        if server_time > self.open.newest {
            self.open.newest = server_time;
        }
        if server_time < self.open.oldest {
            self.open.oldest = server_time;
        }
    }

    /// The committed records, newest last (index 0 = oldest kept).
    pub fn records(&self) -> impl Iterator<Item = &PacketRecord> {
        let start = (self.head + PACKET_RING_LEN - self.len) % PACKET_RING_LEN;
        (0..self.len).map(move |i| &self.packets[(start + i) % PACKET_RING_LEN])
    }

    /// Drop everything but the `n` newest records (step-change reset: the
    /// pre-step half of the window is discarded so the new value shows within
    /// ~2 s). The ring is a FIFO — shrinking `len` drops from the front.
    pub fn keep_last(&mut self, n: usize) {
        self.len = n.min(self.len);
    }
}

/// `GameStats_t` (`Proxy_Header.hpp:79-85`) — per-(client, opponent) tallies.
#[derive(Debug, Default, Clone, Copy)]
pub struct GameStats {
    pub killed: i32,
    pub killed_by: i32,
    pub damages_given: i32,
    pub damages_taken: i32,
}

/// Team-lock caps of one instance (TFFA match id): how many players each team
/// may still field. `-1` means that team is not locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamCaps {
    pub red: i32,
    pub blue: i32,
}

impl Default for TeamCaps {
    fn default() -> Self {
        TeamCaps { red: -1, blue: -1 }
    }
}

/// Team-lock feature state (`proxy_sv_lockTeamsAfterXSecs`, the grace delay in
/// seconds before a round's teams lock; `0` disables the feature).
///
/// The round start opens a grace window: for its whole duration no team is
/// capped, so players can still join. A player who joins a team in an instance
/// cancels that instance's lock for the round. When the window expires, every
/// instance still armed and not cancelled is capped at its current even roster
/// (`caps[0]` = main). Each pair is `-1`/`-1` while that instance is unlocked.
///
/// `pending` waits for the reconnect burst that follows a map load/map_restart;
/// `saw_reconnect` flips on the first `firstTime == false`
/// `GAME_CLIENT_CONNECT` of that round (the engine reconnects every retained
/// client before the first frame that follows). `armed` records the instances
/// that existed when the grace window opened: a match created later in the round
/// is not locked until the next round, as before.
#[derive(Debug, Clone, Copy)]
pub struct TeamLock {
    pub caps: [TeamCaps; crate::tffa::TOTAL_MATCH_SLOTS],
    pub pending: bool,
    pub saw_reconnect: bool,
    /// Server time (ms) at which the grace window opened.
    pub grace_start: i32,
    /// Whether the grace window is counting down this round.
    pub grace_running: bool,
    /// Instances that existed at the round start and may still lock this round.
    pub armed: [bool; crate::tffa::TOTAL_MATCH_SLOTS],
    /// Instances in which a player joined during the grace window: they stay
    /// unlocked for the whole round.
    pub joined: [bool; crate::tffa::TOTAL_MATCH_SLOTS],
}

impl Default for TeamLock {
    fn default() -> Self {
        TeamLock {
            caps: [TeamCaps { red: -1, blue: -1 }; crate::tffa::TOTAL_MATCH_SLOTS],
            pending: true,
            saw_reconnect: false,
            grace_start: 0,
            grace_running: false,
            armed: [false; crate::tffa::TOTAL_MATCH_SLOTS],
            joined: [false; crate::tffa::TOTAL_MATCH_SLOTS],
        }
    }
}

/// One parallel TFFA match: proxy-side score copy. The game's global
/// `level.teamScores` is kept neutralised (see `tffa::on_add_score`) so the
/// pristine `CheckExitRules` never fires early; each match is compared against
/// the shared server `fraglimit` independently.
#[derive(Debug, Clone, Copy, Default)]
pub struct TffaMatch {
    pub active: bool,
    pub red: i32,
    pub blue: i32,
    /// `level.time` when the match's outcome was decided (fraglimit hit, or
    /// the shared timelimit expired untied), `0` while it is still being
    /// played. The match keeps running until `tffa::END_DELAY_MS` elapsed —
    /// the pristine `LogExit` → `BeginIntermission` delay — and its damage is
    /// void in the meantime, exactly like the pristine
    /// `level.intermissionQueued` window (`g_combat.c:4520-4524`).
    pub end_queued_at: i32,
    /// Roster of a finished game of **this slot's identity**: the team
    /// (`TEAM_RED`/`TEAM_BLUE`) each member played for when the game ended,
    /// `crate::tffa::REMEMBERED_NONE` (0) for every slot that is not
    /// remembered. The roster is stored under the game's TFFA id — not under
    /// the slot it was running in — so the group comes back to the same id at
    /// the next `map_restart`, with each member on their old team (D-006.10,
    /// D-006.11). A client is remembered by at most one id
    /// (`tffa::forget_return_rosters`).
    pub remembered: [u8; MAX_CLIENTS],
    /// Password protecting team joins to this match's **identity**
    /// (`crate::tffa::MAX_TFFA_PASSWORD` bytes; `0` = open). The password
    /// belongs to the id, not to the runtime slot, so it follows a game
    /// promoted into the main slot and survives a `map_restart` with the
    /// match. Anyone may spectate a protected match; only a client that gave
    /// the password (`TffaState::pass_ok`) may join RED/BLUE in it.
    pub password: [u8; crate::tffa::MAX_TFFA_PASSWORD],
    pub password_len: u8,
}

impl TffaMatch {
    /// Whether the match's outcome is decided but its end-of-match delay is
    /// still running: damage inside it is void and it has not been announced
    /// or moved to spectator yet.
    pub fn ending(&self) -> bool {
        self.active && self.end_queued_at != 0
    }

    /// Whether joining a team in this match needs the password.
    pub fn has_password(&self) -> bool {
        self.password_len != 0
    }

    /// The stored password bytes (`&[]` for an open match).
    pub fn password_bytes(&self) -> &[u8] {
        let len = (self.password_len as usize).min(self.password.len());
        &self.password[..len]
    }

    /// Whether `given` is this match's password. Exact and case-sensitive; an
    /// open match has no password and never matches.
    pub fn password_matches(&self, given: &[u8]) -> bool {
        self.has_password() && given == self.password_bytes()
    }

    /// Replace the password (`given` empty = open match). Returns `false`
    /// without changing the match when `given` is longer than the storage.
    pub fn set_password(&mut self, given: &[u8]) -> bool {
        if given.len() > self.password.len() {
            return false;
        }
        self.password = [0; crate::tffa::MAX_TFFA_PASSWORD];
        self.password[..given.len()].copy_from_slice(given);
        self.password_len = given.len() as u8;
        true
    }

    /// Make the match open again.
    pub fn clear_password(&mut self) {
        self.password = [0; crate::tffa::MAX_TFFA_PASSWORD];
        self.password_len = 0;
    }

    /// Whether this slot still remembers the roster of the game of its identity
    /// that finished: a `map_restart` re-creates the slot from it (or, for main,
    /// puts the remembered players back on their teams without deactivating
    /// it).
    pub fn has_remembered(&self) -> bool {
        self.remembered
            .iter()
            .any(|team| *team != crate::tffa::REMEMBERED_NONE)
    }
}

/// Parallel-TFFA state.
///
/// Slot `0` is the **main TFFA**: every connected client starts there, and it
/// is always `active`. Slots `1..=MAX_MATCHES` are player-created matches.
/// `client_match[n]` holds the match id (`0` = main) and is cleared for fresh
/// connects. `player_score[n]` is the per-match
/// per-player score copy the `scores` rewrite substitutes for the game's
/// global `PERS_SCORE` (which accumulates across the whole map).
///
/// `frame_stamp`/`rebuilt_stamp`/`owned` back the per-trace physics
/// isolation: `owned` caches the linked player-owned entities (sabers,
/// missiles, corpses) discovered on the last game frame.
///
/// `cs_players_base[n]` is the last `CS_PLAYERS + n` info string the game set
/// (via `G_SET_CONFIGSTRING`); per-viewer `cs` overrides (prefixed names,
/// outsiders forced to spectator) are derived from it so another match's
/// player never shows up in the viewer's team overlay.
#[derive(Debug, Clone)]
pub struct TffaState {
    pub matches: [TffaMatch; crate::tffa::TOTAL_MATCH_SLOTS],
    pub client_match: [u8; MAX_CLIENTS],
    /// The TFFA id each client belongs to (`0` = main), independent of the slot
    /// their game currently runs in. It changes only on an explicit instance
    /// move (`tffa_create`/`tffa_join`/`tffa_join 0`, or a closed match returning
    /// them to main), never when a match is promoted into the main slot or when
    /// the client picks a team. A `map_restart` returns every client to this id
    /// (`tffa::reset_round_state`), so TFFA 0 stays TFFA 0 and TFFA 1 stays
    /// TFFA 1 even after a promotion moved their games around (D-006.11).
    pub home: [u8; MAX_CLIENTS],
    /// The TFFA **identity** each client has given the password of, or
    /// `MAIN_MATCH` for none. Only a protected match consumes it: a client may
    /// join RED/BLUE in a match whose id it names, and anyone may spectate
    /// without it. It is dropped when the client disconnects or a fresh player
    /// takes the slot, when the match closes (its slot can be reused with a new
    /// password), and it is carried across a `map_restart` with the match.
    /// Identity-keyed rather than slot-keyed so it survives a promotion
    /// (D-006.11).
    pub pass_ok: [u8; MAX_CLIENTS],
    /// The TFFA id whose game currently runs in the **main slot** (D-006.11).
    /// A finished main game is replaced by the lowest-id still-played created
    /// match, whose members are runtime-assigned to main (`client_match = 0`)
    /// while their `home` keeps their own id. `0` when main hosts its own game.
    /// Used to store a finished game's roster under its identity and to keep
    /// the live promoted id's slot from being handed to a new match.
    pub main_origin: u8,
    pub player_score: [i32; MAX_CLIENTS],
    pub frame_stamp: u32,
    pub rebuilt_stamp: u32,
    pub owned: [(u16, u8); crate::tffa::OWNED_CAP],
    pub owned_count: usize,
    /// Sound attribution for the temp entity living in each gentity slot
    /// (parallel-TFFA sound isolation): the match whose action created it, or
    /// `crate::tffa::SOURCE_WORLD` for map/world sounds. Every temp-entity
    /// construction (`G_TempEntity`/`G_SoundTempEntity` detours) stamps its
    /// slot, so a reused slot never keeps a stale attribution.
    pub sound_source: [u8; MAX_GENTITIES],
    /// The `G_Sound` → `G_SoundTempEntity` handshake: source match of the
    /// temp entity currently being constructed (`SOURCE_WORLD` when none).
    pub pending_sound_source: u8,
    pub cs_players_base: [String; MAX_CLIENTS],
    /// Last `cs CS_PLAYERS + slot` value queued **to each viewer**
    /// (`[viewer][slot]`, empty = nothing sent yet): the per-viewer demotions
    /// are pushed again before every `scores` message so a lost override cannot
    /// group an outsider on the viewer's side, but a populated multi-match
    /// server has one demotion per playing outsider for every viewer — re-sending
    /// all of them (each a full `CS_PLAYERS` info string, ~150-250 bytes) on
    /// every scoreboard request queued several kilobytes of reliable commands to
    /// the one client that opened the board. The record is what the viewer is
    /// known to already hold, so the refresh only sends what changed. The
    /// engine's own `CS_PLAYERS` broadcasts and the proxy's sends keep it in
    /// step; a client that gets a fresh gamestate (connect, resend, restart)
    /// has its row dropped (`tffa::forget_view_overrides`).
    pub cs_players_sent: [[String; MAX_CLIENTS]; MAX_CLIENTS],
    /// Whether each viewer's `cs_players_sent` row must be rebuilt from the
    /// server's **base** strings because its client just received a gamestate
    /// (`SV_SendClientGameState` → `tffa::forget_view_overrides`). The gamestate
    /// delivers every `CS_PLAYERS` base string, so a fresh row may record them
    /// as held without re-sending (D-006.12). Any other full re-push (a respawn
    /// or team change begin, a deferred refresh) must **not** assume the base:
    /// the client still holds whatever the proxy last sent, and a base push that
    /// was deferred under reliable pressure is still owed. Consumed by
    /// `tffa::send_all_overrides_to_client`.
    pub view_bases_from_gamestate: [bool; MAX_CLIENTS],
    /// Last per-viewer `CS_SCORES1/2` sent, to skip redundant corrections
    /// (the 128-slot reliable buffer overflows otherwise: every frag would
    /// re-push unchanged scores to every client).
    pub cs_scores_sent: [(i32, i32); MAX_CLIENTS],
    /// The client asked for the scoreboard (`score`/`scoreboard` command, the
    /// TAB request). Only that request pays for the per-viewer `CS_PLAYERS`
    /// repair; a server-initiated final board (intermission) must not trigger
    /// a burst.
    pub score_requested: [bool; MAX_CLIENTS],
    /// Last `effective_match` per viewer, to re-push prefixed names + scores
    /// when spectating (game follow) changes your instance without any proxy
    /// command.
    pub viewer_match_cache: [u8; MAX_CLIENTS],
    /// Match whose final scoreboard each viewer must see, captured just before
    /// the pristine `BeginIntermission` moves clients and stops following
    /// (`tffa::snapshot_intermission_boards`). Without it a spectator watching
    /// another match would fall back to their own (often empty) match when the
    /// final `scores` message is built and read e.g. `TIED AT 0`.
    pub intermission_boards: [u8; MAX_CLIENTS],
    /// The team each client must auto-join when it begins the round a
    /// remembered instance was re-created for (`TEAM_RED`/`TEAM_BLUE`,
    /// `crate::tffa::REMEMBERED_NONE` = none). Filled by
    /// `tffa::reset_round_state` from a finished match's remembered roster and
    /// consumed by `tffa::on_client_begin` (D-006.10). Cleared for a client
    /// that disconnects or reconnects fresh, so a reused slot never inherits
    /// the previous occupant's team.
    pub pending_join: [u8; MAX_CLIENTS],
    /// Clients whose `GAME_CLIENT_BEGIN` handling was deferred because their
    /// reliable-command window was still full of the `map_restart` burst
    /// (`tffa::reliable_pressure`). The next frame under the limit runs
    /// `restore_pending_join` + `send_all_overrides_to_client` for them, so
    /// the auto-join and its game-side broadcasts do not add to the burst
    /// (D-006.12).
    pub deferred_begin: [bool; MAX_CLIENTS],
    /// Clients whose per-viewer `cs` correction had to be dropped because
    /// their reliable window was full; a later frame re-pushes their overrides
    /// once the client acknowledged the burst (D-006.12).
    pub pending_view_refresh: [bool; MAX_CLIENTS],
    /// Viewers that received a per-viewer `CS_PLAYERS` correction this frame
    /// and still owe cgame a `loaddefered` server command (D-006.14). A team
    /// change in a `CS_PLAYERS` string makes cgame rebuild the player's
    /// clientinfo; with `cg_deferPlayers` (the default) it then copies another
    /// client's model and marks the entry deferred, so the player renders with
    /// the wrong skin and in bind pose until the scoreboard triggers
    /// `CG_LoadClientInfo`. `loaddefered` is cgame's own server command for
    /// exactly that load; sending it after the `cs` corrections makes the real
    /// model load (and `CG_ResetPlayerEntity` run) immediately.
    pub anim_reload_pending: [bool; MAX_CLIENTS],
    /// Remaining game frames to flip the **spectator's own playerstate**
    /// animation-restart bits (`legsFlip`/`torsoFlip`) after a per-viewer
    /// `CS_PLAYERS` correction (D-006.14). A spectator's client renders the
    /// followed player from `cg.snap->ps`, not from the entity snapshot (the
    /// engine excludes the followed entity), so an entity-state flip cannot
    /// reach it; `SpectatorClientEndFrame` copies the followed player's ps into
    /// the spectator's ps every frame, and flipping it there makes
    /// `CG_RunLerpFrame` re-apply the followed player's current animation.
    pub anim_ps_flip_pending: [u8; MAX_CLIENTS],
    /// A `map_restart` reconnect burst is in progress (D-006.12). Set before
    /// the pristine `GAME_INIT` body runs, so even the configstrings it sets
    /// cannot make the proxy queue commands into the burst. Every proxy `cs`
    /// send and every `GAME_CLIENT_BEGIN` push is deferred until the
    /// `GAME_RUN_FRAME` after `SV_MapRestart_f`'s trailing one; the per-client
    /// reliable-window check then waits for the acknowledgements that the
    /// engine reads at the start of the next engine frame.
    pub restart_burst: bool,
    /// A `GAME_CLIENT_BEGIN` was seen while `restart_burst` was set: the
    /// reconnect loop ran, so the next `GAME_RUN_FRAME` is the engine's
    /// trailing settle frame (`SV_MapRestart_f`, `sv_ccmds.cpp:346-349`).
    pub restart_burst_connects: bool,
    /// That trailing frame has been observed; the following `GAME_RUN_FRAME`
    /// is the first real frame and the deferred work may run.
    pub restart_burst_trailing: bool,
    /// `GAME_RUN_FRAME` calls seen since the burst started. Bounded fallback
    /// so a modified engine that skips the trailing frame cannot freeze the
    /// proxy's per-viewer corrections forever.
    pub restart_burst_frames: u8,
    /// Set when any intermission begins (pristine timelimit exit or the
    /// proxy-triggered solo-main finish); cleared on `GAME_INIT`. While set,
    /// fraglimit handling stays off — the map is ending.
    pub intermission: bool,
    /// Instance tag of each world-entity slot (per-instance world entities,
    /// D-006 extension): the match id owning the entity, or
    /// `crate::tffa::ENTITY_SHARED` for an entity visible and solid in every
    /// instance. Untagged entities behave exactly as before.
    pub entity_instance: [u8; MAX_GENTITIES],
    /// Interactive map entities measured at the world scan
    /// (`crate::tffa::interactive_sources`) — the gentity cost of one further
    /// instance's clone set, and the number the capacity log line reports.
    /// `0` until the scan runs (`world_scanned`).
    pub world_clone_cost: usize,
    /// Instance whose clone set the map holds no longer (a freed clone slot
    /// must not be handed to a later match still tagged): instance `i` is
    /// marked for release when its match closes; the clones are freed at the
    /// next `GAME_RUN_FRAME` start, when no game code is mid-iteration.
    pub world_release: u32,
    /// Instances whose clone sets exist, as a bitmask (bit `i` = instance `i`).
    /// Clone creation is idempotent: re-opening a slot whose set survived
    /// reuses the world it left behind.
    pub world_cloned: u32,
    /// Whether the map holds an interactive entity that cannot be duplicated
    /// (an NPC/actor with a shared Ghoul2 model or a dynamic `client`): such a
    /// map takes no instance at all — all-or-nothing, D-006.6.
    pub world_clone_blocked: bool,
    /// Whether `scan_world_entities` has measured this map. A server that turns
    /// `proxy_parallel_tffas_enable` on mid-map has not scanned yet, and an
    /// unmeasured map must not take an instance (an empty clone set would silently leave
    /// every door shared).
    pub world_scanned: bool,
    /// Reusable save list for the world-entity gates: `(address of r.contents,
    /// saved value)` pairs, taken out of the state for the duration of one
    /// window (a synchronous trace, or a `G_TouchTriggers` call) and put back
    /// with its capacity. A list rather than a fixed array because the number of
    /// entities one window must ignore grows with the map and the number of
    /// instances — a cap that silently overflowed would leave a foreign door
    /// solid or triggerable (all-or-nothing) — and taking it out of the state
    /// keeps a nested window from sharing the outer one's entries.
    pub flip_buf: Vec<(usize, i32)>,
    /// The instance the game code currently running belongs to
    /// (`crate::tffa::SCOPE_NONE` outside such a call). Set and restored around
    /// each `G_RunThink` and `G_UseTargets2` call, and inherited by nested calls
    /// whose acting entity carries no tag, so a cascade a player action started
    /// never widens halfway through. Two gates read it:
    ///
    /// - `G_Find` skips `targetname` matches tagged to another instance (S7 link
    ///   gate), so a chain — a door trigger, ICARUS, a script runner — resolves
    ///   inside the instance whose code fired it;
    /// - `G_Spawn` remembers the slots it hands out, so an interactive entity
    ///   created mid-map joins the acting instance once its spawner returns
    ///   (S8 runtime-spawn gate).
    pub acting_scope: u8,
    /// The `(slot, instance)` pairs `G_Spawn` handed out inside the acting calls
    /// in progress, classified when the outermost one closes. Only the spawns
    /// that turn out *interactive* join an instance; the rest (temp event
    /// entities, missiles, debris) keep their tagless, shared life.
    pub spawn_pending: Vec<(u16, u8)>,
    /// Last `proxy_parallel_tffas_enable` value the per-frame cvar mirror
    /// showed
    /// (`crate::tffa::on_frame_start`): a non-zero → zero edge closes every
    /// created match and returns its members to spectator, because the feature
    /// is off from that frame on. Re-latched from the cvar at `GAME_INIT`.
    pub enable_latched: bool,
    /// Frames left in the post-`map_restart` settle window (`0` = none).
    /// Set by `GAME_INIT` for a `map_restart` whose open matches were carried
    /// over: the engine runs three settle frames and reconnects the clients
    /// *after* them (`SV_MapRestart_f`, `sv_ccmds.cpp:306-349`), so during
    /// those frames every game client slot is still empty and every preserved
    /// match would look member-less. While this is non-zero the per-frame
    /// empty-match sweep (and limit handling) stays off. The first
    /// `GAME_CLIENT_CONNECT` of the reconnect burst clears it; the countdown is
    /// the fallback for a restart nobody was retained for (an empty server
    /// restarted by rcon, then `addbot`), which would otherwise leave the
    /// feature disabled for the whole map.
    pub restart_grace_frames: u8,
}

impl Default for TffaState {
    fn default() -> Self {
        TffaState {
            matches: core::array::from_fn(|i| TffaMatch {
                active: i == 0,
                ..Default::default()
            }),
            client_match: [0; MAX_CLIENTS],
            home: [0; MAX_CLIENTS],
            pass_ok: [0; MAX_CLIENTS],
            main_origin: crate::tffa::MAIN_MATCH,
            player_score: [0; MAX_CLIENTS],
            frame_stamp: 0,
            rebuilt_stamp: 0,
            owned: [(0, 0); crate::tffa::OWNED_CAP],
            owned_count: 0,
            sound_source: [crate::tffa::SOURCE_WORLD; MAX_GENTITIES],
            pending_sound_source: crate::tffa::SOURCE_WORLD,
            cs_players_base: core::array::from_fn(|_| String::new()),
            cs_players_sent: core::array::from_fn(|_| core::array::from_fn(|_| String::new())),
            view_bases_from_gamestate: [false; MAX_CLIENTS],
            cs_scores_sent: [(i32::MIN, i32::MIN); MAX_CLIENTS],
            score_requested: [false; MAX_CLIENTS],
            viewer_match_cache: [0; MAX_CLIENTS],
            intermission_boards: [0; MAX_CLIENTS],
            pending_join: [crate::tffa::REMEMBERED_NONE; MAX_CLIENTS],
            deferred_begin: [false; MAX_CLIENTS],
            pending_view_refresh: [false; MAX_CLIENTS],
            anim_reload_pending: [false; MAX_CLIENTS],
            anim_ps_flip_pending: [0; MAX_CLIENTS],
            restart_burst: false,
            restart_burst_connects: false,
            restart_burst_trailing: false,
            restart_burst_frames: 0,
            intermission: false,
            entity_instance: [crate::tffa::ENTITY_SHARED; MAX_GENTITIES],
            world_clone_cost: 0,
            world_release: 0,
            world_cloned: 0,
            world_clone_blocked: false,
            world_scanned: false,
            flip_buf: Vec::new(),
            acting_scope: crate::tffa::SCOPE_NONE,
            spawn_pending: Vec::new(),
            enable_latched: false,
            restart_grace_frames: 0,
        }
    }
}

/// Phase of the parallel-duel ladder (D-025).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DuelPhase {
    /// No ladder running (off, or waiting for two willing players).
    #[default]
    Idle,
    /// A round is being played; courts resolve per frame.
    Playing,
    /// Every court resolved: the stock intermission shows the board until the
    /// proxy issues its own `map_restart`.
    Board,
}

/// Parallel-duel ladder state (D-025).
///
/// The instance assignment itself lives in [`TffaState::client_match`] (the
/// shared slate): court 1 runs in the main slot (instance 0), court `i` in
/// instance `i - 1`. `wins`/`losses` are the ladder-long standings the proxy
/// injects per viewer into `CS_PLAYERS`; the game's own `sess.wins/losses`
/// are never written, which keeps `DuelLimitHit` inert.
#[derive(Debug, Clone)]
pub struct DuelState {
    pub phase: DuelPhase,
    /// The cvar is on and a ladder may run (waiting for players included).
    pub ladder: bool,
    /// `proxy_parallel_duels_enable` edge latch.
    pub enable_latched: bool,
    /// Proxy-side willingness, seeded at connect from userinfo `team == "s"`
    /// and changed only by explicit client requests. Bots are always willing.
    pub willing: [bool; MAX_CLIENTS],
    /// Bench (FIFO), carried across the proxy's own round restart.
    pub queue: Vec<u8>,
    /// This round's courts, in ladder order (index 0 = court 1 = top).
    pub courts: [crate::duel::Court; crate::duel::MAX_COURTS],
    pub court_count: usize,
    /// `level.time` at the round start (per-round timelimit base).
    pub round_start: i32,
    /// `svs.time` at which the board window ends and the proxy restarts.
    pub board_until: i32,
    /// The one-shot "round restart" token armed immediately before the
    /// proxy's own `map_restart` (`crate::duel::classify_restart`).
    pub restart_armed: bool,
    /// Monotonic round counter mixed into the shuffle seed.
    pub round_serial: u32,
    /// Ladder-long standings (reset only on a new ladder).
    pub wins: [i32; MAX_CLIENTS],
    pub losses: [i32; MAX_CLIENTS],
    /// The ladder champion once someone reached `duel_fraglimit` wins; the
    /// round finishes first, then a fresh ladder starts.
    pub champion: Option<u8>,
    /// Shuffle seed for the next formation.
    pub seed: u32,
    /// Next round's courts and queue, captured at round end and carried
    /// through the proxy's own `map_restart` (`GAME_INIT(restart = true)`).
    pub pending_courts: Vec<crate::duel::Pair>,
    pub pending_queue: Vec<u8>,
    /// The pending carry belongs to a proxy round transition (round) or an
    /// admin/voted restart (fresh shuffle). Set by the restart classifier.
    pub carry_round: bool,
    /// Set when the ladder must start from a fresh shuffle after the next
    /// `GAME_INIT` (admin restart, champion, full map change).
    pub fresh_restart: bool,
    /// Per-court `level.time` at which the court's duel was decided (the 1 s
    /// void-damage window before both players move to spectator).
    pub resolved_at: [i32; crate::duel::MAX_COURTS],
    /// `svs.time` when the ladder last poked the roster (waiting message).
    pub last_roster_check: i32,
    /// Players waiting to be spawned into their court, drained a few per frame
    /// after the `map_restart` burst (D-006.12 deferral).
    pub spawn_pending: Vec<u8>,
    /// Whether a court player already has a body this round.
    pub spawned: [bool; MAX_CLIENTS],
    /// The proxy issued its own `map_restart` and is waiting for `GAME_INIT`;
    /// no formation may run in between.
    pub restart_pending: bool,
    /// The round clock starts on the first frame a court player is connected
    /// (clients reconnect after the settle frames).
    pub round_clock_started: bool,
    /// `svs.time` when a carried round was installed at `GAME_INIT`; the
    /// fallback for a carry nobody reconnects into.
    pub carried_at: i32,
    /// Per-viewer HUD dedup: the duelists configstring last queued, the
    /// healths string and the top-score pair (`CS_SCORES1/2`).
    pub hud_duelists: [String; MAX_CLIENTS],
    pub hud_healths: [String; MAX_CLIENTS],
    pub hud_scores: [(i32, i32); MAX_CLIENTS],
}

impl Default for DuelState {
    fn default() -> Self {
        DuelState {
            phase: DuelPhase::Idle,
            ladder: false,
            enable_latched: false,
            willing: [false; MAX_CLIENTS],
            queue: Vec::new(),
            courts: [crate::duel::Court::default(); crate::duel::MAX_COURTS],
            court_count: 0,
            round_start: 0,
            board_until: 0,
            restart_armed: false,
            round_serial: 0,
            wins: [0; MAX_CLIENTS],
            losses: [0; MAX_CLIENTS],
            champion: None,
            seed: 0,
            pending_courts: Vec::new(),
            pending_queue: Vec::new(),
            carry_round: false,
            fresh_restart: false,
            resolved_at: [0; crate::duel::MAX_COURTS],
            spawn_pending: Vec::new(),
            spawned: [false; MAX_CLIENTS],
            restart_pending: false,
            round_clock_started: false,
            carried_at: 0,
            last_roster_check: 0,
            hud_duelists: core::array::from_fn(|_| String::new()),
            hud_healths: core::array::from_fn(|_| String::new()),
            hud_scores: [(i32::MIN, i32::MIN); MAX_CLIENTS],
        }
    }
}

/// Per-client bookkeeping (`Proxy_Header.hpp:87-106`).
#[derive(Debug, Clone, Copy)]
pub struct ClientEntry {
    pub is_connected: bool,
    /// `isBot` of the last `GAME_CLIENT_CONNECT` (true on the reconnect after
    /// a map change too). The userinfo validation skips bots: their userinfo
    /// comes from the bot files and has none of the required keys.
    pub is_bot: bool,
    pub clean_name: [u8; MAX_NETNAME],
    /// Admin mute (D-011): the client's `say`/`say_team`/`tell`/`voice_cmd`
    /// commands are dropped (or, with `proxy_sv_muteMode 2`, shadowed) instead
    /// of reaching the other players. Set and cleared by the `mute`/`unmute`
    /// console commands, and dropped with the rest of the entry when the client
    /// disconnects (the slot belongs to the next player after that).
    pub muted: bool,
    /// Per-player `/ignore` mask (D-016, `crate::ignore`): bit `n` set means
    /// this client does not receive client `n`'s chat. Cleared when either
    /// client disconnects (slots are reused) and carried across a `map_restart`
    /// like the mute flag.
    pub ignored: u32,
    /// netStatus estimator state (D-007): per-packet window, fps/pps ring
    /// semantics and the reset rules.
    pub netstatus: NetstatusState,
    /// Incoming packet-rate accounting / smoothing (`crate::pps`).
    pub pps: crate::pps::PpsState,
    /// Anti-macro input guard (`crate::macroguard`): per-channel alternation
    /// runs and locks, fed from every processed usercmd.
    pub macro_guard: crate::macroguard::MacroState,
    /// `cl_timeNudge` neutralisation (`crate::timenudge`, D-015): the applied
    /// usercmd `serverTime` shift, its ramp target and the log edge.
    pub nudge_void: crate::timenudge::VoidState,
    pub last_time_net_status: i32,
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
            is_bot: false,
            clean_name: [0; MAX_NETNAME],
            muted: false,
            ignored: 0,
            netstatus: NetstatusState::default(),
            pps: crate::pps::PpsState::default(),
            macro_guard: crate::macroguard::MacroState::default(),
            nudge_void: crate::timenudge::VoidState::default(),
            last_time_net_status: 0,
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
    /// `taystJKinfo` serverinfo key mirror (`crate::scoreboard`): the proxy sets
    /// bit 7 (`TAYSTJK_INFO_SCORE_DEATHS`) while it is enabled, so TaystJK
    /// cgames select the 15-int `scores` parse.
    pub taystjk_info: VmCvar,
    pub jump_start_time: [i32; MAX_CLIENTS],
    pub team_lock: TeamLock,
    /// The `proxy_sv_teamSizeRules` rule most recently written, so the per-frame
    /// reconciler only re-applies the limits when the effective rule changes.
    pub last_team_rule: Option<crate::rules::Rule>,
    /// The `timelimit`/`fraglimit`/`capturelimit` captured before the proxy
    /// first overrode them, restored when the roster falls below every rule.
    pub base_limits: Option<crate::rules::BaseLimits>,
    /// `level.time` of the last periodic spectator team-overlay send
    /// (`crate::teamoverlay`).
    pub spec_overlay_last_time: i32,
    /// Per viewer, the `(team, match)` scope last announced to its cgame
    /// (`0` = none): a follow change resends the roster immediately instead of
    /// waiting for the next periodic tick.
    pub spec_overlay_scope: [i32; MAX_CLIENTS],
    pub tffa: TffaState,
    /// Parallel-duel ladder state (D-025). Mutually exclusive with the TFFA
    /// policy by gametype; both share `tffa.client_match` as the instance
    /// assignment slate.
    pub duel: DuelState,
    /// Per-viewer visibility-filter snapshot mutations (`crate::fov`): the FOV
    /// cap and the anti-wallhack. Holds the entity fields changed for the
    /// snapshot currently being built, restored as soon as the engine has
    /// copied them.
    pub fov: crate::fov::FovState,
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

/// Effective integer value of proxy cvar `index`, honoring its legacy alias
/// (D-026): the canonical spelling wins once an admin changed it away from its
/// default; while it still holds the default, a non-default legacy value
/// applies, so a config written before the rename keeps its effect. When both
/// were changed, the canonical one wins.
pub fn cvar_integer(index: usize) -> i32 {
    with_state(|s| cvar_integer_in(s, index))
}

/// The current string value of a proxy cvar (NUL-trimmed, lossy UTF-8).
pub fn cvar_string(index: usize) -> String {
    with_state(|s| {
        let field = &s.cvars[index].string;
        let len = field.iter().position(|&c| c == 0).unwrap_or(field.len());
        let bytes: Vec<u8> = field[..len].iter().map(|&c| c as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// [`cvar_integer`] for call sites that already hold the state lock.
pub fn cvar_integer_in(s: &ProxyState, index: usize) -> i32 {
    let canonical = &s.cvars[index];
    if !cvar_is_default(canonical, index) {
        return canonical.integer;
    }
    if let Some(legacy_index) = legacy_alias(index) {
        let legacy = &s.cvars[legacy_index];
        if !cvar_is_default(legacy, legacy_index) {
            return legacy.integer;
        }
    }
    canonical.integer
}

/// Whether the mirror still holds the default string it was registered with
/// (`PROXY_CVARS[index].1`). The engine copies the value verbatim into the
/// mirror, so a plain string comparison is enough — no parsing.
fn cvar_is_default(cvar: &VmCvar, index: usize) -> bool {
    let default = PROXY_CVARS[index].1.to_bytes();
    let raw = &cvar.string;
    let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    raw[..len]
        .iter()
        .map(|&c| c as u8)
        .eq(default.iter().copied())
}

/// Drop every per-session value so the next `GAME_INIT` starts from the state
/// a freshly `dlopen`ed module would have.
///
/// The proxy module is pinned for the process lifetime
/// (`crate::original::pin_own_module`) because the engine unmaps it mid-call on
/// a full map change; the engine's re-`dlopen` therefore reuses the same
/// instance instead of giving us fresh statics. This reset restores the
/// documented semantics ("a full map change re-dlopens the module (fresh
/// state)") on top of the pinned instance. Called at the top of `GAME_INIT`.
///
/// `restart` is the `GAME_INIT` restart flag: a `map_restart` (`qtrue`,
/// `sv_ccmds.cpp:306-336`) keeps the parallel-TFFA overlay — which matches were
/// open and which instance each client was in — while a full map change
/// (`SV_SpawnServer` → `SV_InitGameVM(qfalse)`) starts from main, exactly as a
/// fresh module load would. Scores, per-player copies, cs caches and the
/// per-instance worlds are per round / per map and are never carried.
pub fn reset_session(restart: bool) {
    let mut guard = lock();
    let (tffa, duel, muted, ignored) = {
        let previous = guard.as_ref();
        (
            tffa_after_reset(previous.map(|s| &s.tffa), restart),
            duel_after_reset(previous.map(|s| &s.duel), restart),
            mutes_after_reset(previous, restart),
            ignores_after_reset(previous, restart),
        )
    };
    let mut next = ProxyState::default();
    next.tffa = tffa;
    next.duel = duel;
    // A `map_restart` keeps every player in their slot, so an admin mute stays
    // on the player it was issued for and a player's ignore list keeps pointing
    // at the same neighbours. A full map change drops everyone (the slots are
    // handed to new players) and starts with nobody muted or ignored.
    for (entry, (muted, ignored)) in next.clients.iter_mut().zip(muted.into_iter().zip(ignored)) {
        entry.muted = muted;
        entry.ignored = ignored;
    }
    *guard = Some(next);
}

/// The per-slot mute flags a `GAME_INIT` starts from: carried for a
/// `map_restart` (client slots survive it), dropped for a full map change.
///
/// Kept separate from the lock so the carry is unit-tested directly.
fn mutes_after_reset(previous: Option<&ProxyState>, restart: bool) -> [bool; MAX_CLIENTS] {
    let mut muted = [false; MAX_CLIENTS];
    let Some(previous) = previous.filter(|_| restart) else {
        return muted;
    };
    for (carried, entry) in muted.iter_mut().zip(previous.clients.iter()) {
        *carried = entry.muted;
    }
    muted
}

/// The per-slot `/ignore` masks a `GAME_INIT` starts from: carried for a
/// `map_restart` (client slots survive it), dropped for a full map change. Same
/// rule as the mute flags.
fn ignores_after_reset(previous: Option<&ProxyState>, restart: bool) -> [u32; MAX_CLIENTS] {
    let mut ignored = [0u32; MAX_CLIENTS];
    let Some(previous) = previous.filter(|_| restart) else {
        return ignored;
    };
    for (carried, entry) in ignored.iter_mut().zip(previous.clients.iter()) {
        *carried = entry.ignored;
    }
    ignored
}

/// The [`DuelState`] a `GAME_INIT` starts from (D-025).
///
/// A full map change ends the ladder: the new map starts from a fresh shuffle
/// (and the players' willingness is seeded again at connect). A `map_restart`
/// carries the ladder only when the proxy's own round transition armed it
/// (`carry_round`): then the pending courts, queue and standings survive. Any
/// other `map_restart` — rcon, a voted `map_restart`, or a restart with no
/// armed token — is an admin restart: standings reset and the next formation
/// is a fresh shuffle. Per-round timers, resolved markers and the board window
/// never carry.
///
/// Kept separate from the lock so the carry/fresh-shuffle decision is
/// unit-tested directly.
fn duel_after_reset(previous: Option<&DuelState>, restart: bool) -> DuelState {
    let mut fresh = DuelState::default();
    let Some(previous) = previous.filter(|_| restart) else {
        return fresh;
    };
    fresh.willing = previous.willing;
    fresh.ladder = previous.ladder;
    fresh.enable_latched = previous.enable_latched;
    fresh.seed = previous.seed;
    fresh.round_serial = previous.round_serial;
    if previous.carry_round && !previous.fresh_restart {
        fresh.wins = previous.wins;
        fresh.losses = previous.losses;
        fresh.champion = previous.champion;
        fresh.queue = previous.queue.clone();
        fresh.pending_courts = previous.pending_courts.clone();
        fresh.pending_queue = previous.pending_queue.clone();
        fresh.carry_round = true;
        fresh.phase = DuelPhase::Playing;
    } else {
        // Admin restart, champion or no armed carry: fresh ladder next frame
        // (willingness survives, the standings do not).
        fresh.fresh_restart = true;
        fresh.phase = DuelPhase::Idle;
    }
    fresh
}

/// The fresh [`TffaState`] a `GAME_INIT` starts from, carrying the match overlay
/// of the round that just ended only for a `map_restart`: which created matches
/// were open, which id each client belongs to (its `home`, which survives a
/// promotion, D-006.11), the rosters a finished game remembers (D-006.10), and
/// each match's password with the clients that had given it (D-006.15).
/// The runtime-only `main_origin` is not carried: after the restart the main
/// slot hosts its own id (0) again.
///
/// Kept separate from the lock so the carry is unit-tested directly.
fn tffa_after_reset(previous: Option<&TffaState>, restart: bool) -> TffaState {
    // A `map_restart` reconnects every retained client in one engine frame
    // (`SV_MapRestart_f`); the proxy must not queue its own reliable commands
    // into that burst (D-006.12). Set before the pristine `GAME_INIT` body
    // runs, so even its `CS_PLAYERS` sets are handled as burst traffic.
    let mut fresh = TffaState {
        restart_burst: restart,
        ..TffaState::default()
    };
    let Some(previous) = previous.filter(|_| restart) else {
        return fresh;
    };
    // Slot 0 (main) is always active; only its roster is carried, for the
    // players a finished main game sent to spectator before a created match
    // was promoted over it (D-006.11) — a `map_restart` puts them back on their
    // teams without reactivating anything. A created slot carries whether it
    // was being played *and*, for a finished one, the roster it remembers: a
    // `map_restart` re-creates it so its members return to it (D-006.10).
    for (i, m) in fresh.matches.iter_mut().enumerate() {
        if i != crate::tffa::MAIN_MATCH as usize {
            m.active = previous.matches[i].active;
        }
        m.remembered = previous.matches[i].remembered;
        // The password belongs to the identity: a carried match keeps it (and
        // the clients that had given it), so the group does not have to pass it
        // around again after a `map_restart`. `reset_round_state` clears it for
        // a slot that does not survive.
        m.password = previous.matches[i].password;
        m.password_len = previous.matches[i].password_len;
    }
    fresh.client_match = previous.client_match;
    fresh.home = previous.home;
    fresh.pass_ok = previous.pass_ok;
    fresh
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
            taystjk_info: VmCvar::default(),
            jump_start_time: [0; MAX_CLIENTS],
            team_lock: TeamLock::default(),
            last_team_rule: None,
            base_limits: None,
            spec_overlay_last_time: 0,
            spec_overlay_scope: [0; MAX_CLIENTS],
            tffa: TffaState::default(),
            duel: DuelState::default(),
            fov: crate::fov::FovState::default(),
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
    // Keep the diagnostics timestamp lock-free: log call sites run from engine
    // hooks and must never take the proxy state lock (some are already nested
    // inside engine/game calls, and logging must stay usable if state is ever
    // held).
    crate::diagnostics::set_svs_time(time);
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

/// Serializes the tests that mutate the process-global [`STATE`] (the
/// `cargo test` harness runs tests in parallel; two tests initialising/clearing
/// the same global race).
#[cfg(test)]
pub(crate) static TEST_STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    fn no_empty_server_recording_switch_is_registered() {
        // Phase 2 had a temporary `proxy_sv_demoRecordEmpty` debug cvar. Phase 3
        // replaced it with the human-subject rule; Phase 6 removes the last
        // trace. Nothing may let production record an empty server.
        assert!(!PROXY_CVARS.iter().any(|(name, _)| {
            name.to_bytes()
                .windows(b"RecordEmpty".len())
                .any(|window| window.eq_ignore_ascii_case(b"RecordEmpty"))
        }));
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
        assert_eq!(PROXY_CVARS[CVAR_ENABLE_END_GAME_STATS].1.to_bytes(), b"1");
        assert_eq!(PROXY_CVARS[CVAR_LOCK_TEAMS_AFTER_SECS].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_TEAM_SIZE_RULES].1.to_bytes(), b"");
        assert_eq!(PROXY_CVARS[CVAR_TFFA_ENABLE].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_TFFA_MAX_MATCHES].1.to_bytes(), b"8");
        assert_eq!(PROXY_CVARS[CVAR_TFFA_MAX_GROUP_SIZE].1.to_bytes(), b"12");
        assert_eq!(PROXY_CVARS[CVAR_SPECTATOR_TEAM_OVERLAY].1.to_bytes(), b"1");
        assert_eq!(PROXY_CVARS[CVAR_MUTE_MODE].1.to_bytes(), b"1");
        assert_eq!(PROXY_CVARS[CVAR_DEBUG_LOG].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_ENABLE_CLIENT_PPS_LIMIT].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_GLOBAL_MAX_PPS].1.to_bytes(), b"100");
        assert_eq!(PROXY_CVARS[CVAR_CLIENT_PPS_LOG].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_VOID_TIME_NUDGE].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_VOID_TIME_NUDGE_LOG].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_ENABLE_IGNORE].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_ENABLE_BAN_FILE].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_BAN_FILE].1.to_bytes(), b"proxy_bans.txt");
        assert_eq!(PROXY_CVARS[CVAR_ENABLE_GUID_BAN].1.to_bytes(), b"0");
        assert_eq!(
            PROXY_CVARS[CVAR_GUID_BAN_FILE].1.to_bytes(),
            b"proxy_guid_bans.txt"
        );
        assert_eq!(PROXY_CVARS[CVAR_ANTI_MACRO].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_ANTI_MACRO_LOG].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_ANTI_MACRO_VIEW].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_MAX_FOV].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_MAX_FOV_ASPECT].1.to_bytes(), b"1.7777778");
        assert_eq!(PROXY_CVARS[CVAR_MAX_FOV_CAMERA_RANGE].1.to_bytes(), b"100");
        assert_eq!(PROXY_CVARS[CVAR_MAX_FOV_MARGIN].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_CLIENT_PPS_FPS_MARGIN].1.to_bytes(), b"2");
        assert_eq!(PROXY_CVARS[CVAR_TIME_NUDGE_MARGIN].1.to_bytes(), b"2");
        assert_eq!(
            PROXY_CVARS[CVAR_ANTI_MACRO_MIN_REVERSALS].1.to_bytes(),
            b"4"
        );
        assert_eq!(
            PROXY_CVARS[CVAR_ANTI_MACRO_AXIS_MAX_MEAN_CMDS].1.to_bytes(),
            b"3"
        );
        assert_eq!(
            PROXY_CVARS[CVAR_ANTI_MACRO_VIEW_MAX_MEAN_MS].1.to_bytes(),
            b"65"
        );
        assert_eq!(
            PROXY_CVARS[CVAR_ANTI_MACRO_ATTACK_MAX_MEAN_MS].1.to_bytes(),
            b"60"
        );
        assert_eq!(
            PROXY_CVARS[CVAR_ANTI_MACRO_MAX_HOLD_SPREAD_MS].1.to_bytes(),
            b"10"
        );
        assert_eq!(
            PROXY_CVARS[CVAR_ANTI_MACRO_VIEW_STEP_PERCENT].1.to_bytes(),
            b"75"
        );
        assert_eq!(
            PROXY_CVARS[CVAR_ANTI_MACRO_VIEW_MIN_SWING].1.to_bytes(),
            b"364"
        );
        assert_eq!(PROXY_CVARS[CVAR_CLIENT_PPS_TOLERANCE].1.to_bytes(), b"1");
        assert_eq!(PROXY_CVARS[CVAR_ANTI_WALLHACK].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_ANTI_WALLHACK_MARGIN].1.to_bytes(), b"0");
        assert_eq!(
            PROXY_CVARS[CVAR_ANTI_WALLHACK_CAMERA_RANGE].1.to_bytes(),
            b"100"
        );
        assert_eq!(PROXY_CVARS[CVAR_PARALLEL_DUELS_ENABLE].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_DEMO_ENABLE].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_DEMO_DIR].1.to_bytes(), b"demos");
        assert_eq!(PROXY_CVARS[CVAR_ENABLE_HTTP].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_HTTP_BIND].1.to_bytes(), b"127.0.0.1");
        assert_eq!(PROXY_CVARS[CVAR_HTTP_PORT].1.to_bytes(), b"8090");
        assert_eq!(PROXY_CVARS[CVAR_DEMO_DOWNLOAD].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_DEMO_URL].1.to_bytes(), b"");
        assert_eq!(PROXY_CVARS[CVAR_DEMO_RETENTION_HOURS].1.to_bytes(), b"24");
        assert_eq!(PROXY_CVARS[CVAR_DEMO_MAX_FILE_MB].1.to_bytes(), b"128");
        assert_eq!(PROXY_CVARS[CVAR_DEMO_MIN_FREE_MB].1.to_bytes(), b"512");
        assert_eq!(PROXY_CVARS[CVAR_DEMO_INTERMISSION_SECS].1.to_bytes(), b"3");
        assert_eq!(PROXY_CVARS[CVAR_ENABLE].1.to_bytes(), b"1");
    }

    #[test]
    fn dropped_cvars_are_gone() {
        // D-002: the not-wanted features are dropped entirely, cvars included.
        // `proxy_sv_antiWallHack` is no longer in this list: D-023 re-adds it
        // with a new implementation (the original one remains dropped).
        for (name, _) in PROXY_CVARS {
            let n = name.to_bytes();
            assert!(
                !n.starts_with(b"proxy_sv_pingFix")
                    && !n.starts_with(b"proxy_sv_disableKillCmd")
                    && !n.starts_with(b"proxy_sv_sabersFps"),
                "dropped cvar still registered: {}",
                String::from_utf8_lossy(n)
            );
        }
    }

    #[test]
    fn parallel_tffa_cvars_carry_their_legacy_alias() {
        // D-026: the canonical spellings replaced `proxy_tffa_*`, which stays
        // registered (same defaults) as the alias.
        for (canonical, legacy) in [
            (CVAR_TFFA_ENABLE, CVAR_LEGACY_TFFA_ENABLE),
            (CVAR_TFFA_MAX_MATCHES, CVAR_LEGACY_TFFA_MAX_MATCHES),
            (CVAR_TFFA_MAX_GROUP_SIZE, CVAR_LEGACY_TFFA_MAX_GROUP_SIZE),
        ] {
            assert_eq!(legacy_alias(canonical), Some(legacy));
            assert_eq!(
                PROXY_CVARS[canonical].1.to_bytes(),
                PROXY_CVARS[legacy].1.to_bytes()
            );
        }
        let names: Vec<_> = PROXY_CVARS.iter().map(|(n, _)| n.to_bytes()).collect();
        assert!(names.contains(&b"proxy_parallel_tffas_enable".as_slice()));
        assert!(names.contains(&b"proxy_parallel_tffas_maxMatches".as_slice()));
        assert!(names.contains(&b"proxy_parallel_tffas_maxGroupSize".as_slice()));
        assert!(names.contains(&b"proxy_tffa_enable".as_slice()));
        assert!(names.contains(&b"proxy_tffa_maxMatches".as_slice()));
        assert!(names.contains(&b"proxy_tffa_maxGroupSize".as_slice()));
        // Only the renamed cvars have an alias.
        assert_eq!(legacy_alias(CVAR_ENABLE), None);
        assert_eq!(legacy_alias(CVAR_PARALLEL_DUELS_ENABLE), None);
    }

    /// Copy `value` into the cvar mirror `index` the way the engine would
    /// (`string` verbatim, `integer` parsed from it).
    fn set_cvar(index: usize, value: &str) {
        with_state(|s| {
            let cvar = &mut s.cvars[index];
            *cvar = VmCvar::default();
            for (dst, src) in cvar.string.iter_mut().zip(value.bytes()) {
                *dst = src as _;
            }
            cvar.integer = value.parse().unwrap_or(0);
        });
    }

    #[test]
    fn legacy_tffa_alias_is_read_only_as_a_fallback() {
        let _guard = TEST_STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        *lock() = None;

        // Both at their defaults: canonical decides (off / 8 / 12).
        set_cvar(CVAR_TFFA_ENABLE, "0");
        set_cvar(CVAR_LEGACY_TFFA_ENABLE, "0");
        assert_eq!(cvar_integer(CVAR_TFFA_ENABLE), 0);
        set_cvar(CVAR_TFFA_MAX_GROUP_SIZE, "12");
        set_cvar(CVAR_LEGACY_TFFA_MAX_GROUP_SIZE, "12");
        assert_eq!(cvar_integer(CVAR_TFFA_MAX_GROUP_SIZE), 12);

        // A config written before the rename: only the legacy spelling moved.
        set_cvar(CVAR_LEGACY_TFFA_ENABLE, "1");
        set_cvar(CVAR_LEGACY_TFFA_MAX_GROUP_SIZE, "6");
        assert_eq!(cvar_integer(CVAR_TFFA_ENABLE), 1);
        assert_eq!(cvar_integer(CVAR_TFFA_MAX_GROUP_SIZE), 6);

        // The canonical spelling wins over a non-default legacy value.
        set_cvar(CVAR_TFFA_ENABLE, "2");
        set_cvar(CVAR_TFFA_MAX_GROUP_SIZE, "20");
        assert_eq!(cvar_integer(CVAR_TFFA_ENABLE), 2);
        assert_eq!(cvar_integer(CVAR_TFFA_MAX_GROUP_SIZE), 20);

        // A default canonical value falls back again (documented D-026 limit:
        // an explicit `0` cannot be told apart from the untouched default).
        set_cvar(CVAR_TFFA_ENABLE, "0");
        assert_eq!(cvar_integer(CVAR_TFFA_ENABLE), 1);

        // Reset for the next test that touches the global.
        *lock() = None;
    }

    #[test]
    fn state_initialises_once() {
        let _guard = TEST_STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // A test that ran before this one may have left the global initialised.
        *lock() = None;
        assert!(!is_initialised());
        with_state(|s| s.svs_time = 42);
        assert!(is_initialised());
        with_state(|s| assert_eq!(s.svs_time, 42));
        // Reset for the next test that touches the global.
        *lock() = None;
    }

    #[test]
    fn instance_slate_has_sixteen_created_slots_and_tffa_keeps_eight() {
        assert_eq!(crate::instances::MAX_MATCHES, 16);
        assert_eq!(crate::instances::TOTAL_MATCH_SLOTS, 17);
        // The shared slate grew for the duel ladder; the TFFA admin ceiling
        // did not (D-025).
        assert_eq!(crate::tffa::TFFA_MATCH_CAP, 8);
        // The world clone bitmask must cover every slot including 16.
        assert_eq!(1u32 << crate::instances::MAX_MATCHES, 0x1_0000);
    }

    #[test]
    fn instance_mode_split_respects_gametype_and_master_switch() {
        use crate::sdk::{GT_CTF, GT_DUEL, GT_FFA, GT_TEAM};
        // Both features on, but each only owns its own gametype.
        assert_eq!(split_mode(true, true, true, GT_TEAM), InstanceMode::Tffa);
        assert_eq!(split_mode(true, true, true, GT_DUEL), InstanceMode::Duel);
        // A feature whose cvar is off never takes over.
        assert_eq!(split_mode(true, false, true, GT_TEAM), InstanceMode::Off);
        assert_eq!(split_mode(true, true, false, GT_DUEL), InstanceMode::Off);
        // Other gametypes and the master switch keep instancing off.
        assert_eq!(split_mode(true, true, true, GT_FFA), InstanceMode::Off);
        assert_eq!(split_mode(true, true, true, GT_CTF), InstanceMode::Off);
        assert_eq!(split_mode(false, true, true, GT_TEAM), InstanceMode::Off);
        assert_eq!(split_mode(false, true, true, GT_DUEL), InstanceMode::Off);
        // Gating helpers agree with the split.
        assert!(!InstanceMode::Off.is_on());
        assert!(InstanceMode::Tffa.is_on() && InstanceMode::Tffa.is_team());
        assert!(InstanceMode::Duel.is_on() && !InstanceMode::Duel.is_team());
    }

    #[test]
    fn netstatus_ring_commits_one_record_per_packet() {
        let mut ns = NetstatusState::default();
        // two packets: 2 cmds then 1 cmd
        ns.mark_packet_start(1000, 900, 1);
        ns.feed_cmd(5000);
        ns.feed_cmd(5016);
        ns.mark_packet_start(1033, 950, 2);
        ns.feed_cmd(5033);
        ns.mark_packet_start(1066, 0, 3);
        let records: Vec<_> = ns.records().copied().collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].count, 2);
        assert_eq!(records[0].newest, 5016);
        assert_eq!(records[0].oldest, 5000);
        assert_eq!(records[0].stamp_sum, 5000 + 5016);
        assert_eq!(records[0].message_sent, 900);
        assert_eq!(records[0].arrival, 1000);
        assert_eq!(records[1].count, 1);
        assert_eq!(records[1].message_sent, 950);
        // an empty open packet (no cmds) is never committed
        assert_eq!(ns.len, 2);
    }

    #[test]
    fn netstatus_ring_wraps_and_keeps_the_newest() {
        let mut ns = NetstatusState::default();
        for i in 0..(PACKET_RING_LEN + 5) {
            ns.mark_packet_start(i as i32, i as i32, i as i32);
            ns.feed_cmd(i as i32);
        }
        // the open packet is only committed by the *next* mark_packet_start,
        // so 516 records were committed and the ring keeps the newest 512
        assert_eq!(ns.len, PACKET_RING_LEN);
        let records: Vec<_> = ns.records().copied().collect();
        assert_eq!(records.first().unwrap().arrival, 4);
        assert_eq!(
            records.last().unwrap().arrival,
            (PACKET_RING_LEN + 3) as i32
        );
    }

    #[test]
    fn netstatus_reset_and_keep_last_drop_the_window() {
        let mut ns = NetstatusState::default();
        for i in 0..10usize {
            ns.mark_packet_start(i as i32, i as i32, i as i32);
            ns.feed_cmd(i as i32);
        }
        ns.keep_last(4);
        let records: Vec<_> = ns.records().copied().collect();
        assert_eq!(records.len(), 4);
        // 9 committed records (the 10th is still open): 6,7,8 are kept plus
        // the one before them
        assert_eq!(records[0].arrival, 5);
        // keep_last on a shorter window is a no-op clamp
        ns.keep_last(99);
        assert_eq!(ns.len, 4);
        // a pending reset lands at the next packet
        ns.reset_pending = true;
        ns.mark_packet_start(100, 100, 4);
        assert_eq!(ns.len, 0);
        assert_eq!(ns.open.arrival, 100);
        // full reset also clears the open packet
        ns.feed_cmd(1);
        ns.reset();
        assert_eq!(ns.len, 0);
        assert_eq!(ns.open.count, 0);
    }

    #[test]
    fn map_restart_carries_only_the_match_overlay() {
        // A finished round: two created matches open, a member assigned to
        // one of them, tallies and per-map bookkeeping left behind.
        let mut previous = TffaState::default();
        previous.matches[2].active = true;
        previous.matches[2].red = 5;
        previous.matches[2].blue = 3;
        previous.matches[5].active = true;
        previous.client_match[4] = 2;
        previous.player_score[4] = 9;
        previous.restart_grace_frames = 3;
        previous.entity_instance[123] = 2;
        previous.cs_scores_sent[4] = (5, 3);
        assert!(previous.matches[2].set_password(b"hunter2"));
        previous.pass_ok[4] = 2;

        let carried = tffa_after_reset(Some(&previous), true);
        // Which matches were open and who was in them survives.
        assert!(carried.matches[2].active);
        assert!(carried.matches[5].active);
        assert_eq!(carried.client_match[4], 2);
        // A protected match keeps its password and the authorisations given to
        // it; a full map change drops both.
        assert!(carried.matches[2].password_matches(b"hunter2"));
        assert!(!carried.matches[5].has_password());
        assert_eq!(carried.pass_ok[4], 2);
        // Main is always open; scores, per-player copies, cs caches and the
        // per-map clone tags are per round and start clean.
        assert!(carried.matches[crate::tffa::MAIN_MATCH as usize].active);
        assert_eq!(carried.matches[2].red, 0);
        assert_eq!(carried.matches[2].blue, 0);
        assert_eq!(carried.player_score[4], 0);
        assert_eq!(carried.cs_scores_sent[4], (i32::MIN, i32::MIN));
        assert_eq!(carried.restart_grace_frames, 0);
        assert_eq!(carried.entity_instance[123], crate::tffa::ENTITY_SHARED);
    }

    /// D-006.10: a `map_restart` carries a finished match's remembered roster
    /// (so `tffa::reset_round_state` can re-create the instance), while a full
    /// map change drops it. The one-shot auto-join table never carries.
    #[test]
    fn map_restart_carries_a_finished_matches_remembered_roster() {
        let mut previous = TffaState::default();
        previous.matches[1].remembered[5] = crate::sdk::TEAM_RED as u8;
        previous.pending_join[5] = crate::sdk::TEAM_RED as u8;
        previous.home[5] = 1;
        // D-006.11: the roster of a finished *main* game is carried too, and a
        // promoted game's identity survives the restart.
        previous.matches[crate::tffa::MAIN_MATCH as usize].remembered[6] =
            crate::sdk::TEAM_BLUE as u8;
        previous.home[7] = 3;

        let carried = tffa_after_reset(Some(&previous), true);
        assert_eq!(carried.matches[1].remembered[5], crate::sdk::TEAM_RED as u8);
        assert!(carried.matches[1].has_remembered());
        assert_eq!(
            carried.matches[crate::tffa::MAIN_MATCH as usize].remembered[6],
            crate::sdk::TEAM_BLUE as u8
        );
        assert_eq!(carried.home[5], 1);
        assert_eq!(carried.home[7], 3);
        assert_eq!(carried.pending_join[5], crate::tffa::REMEMBERED_NONE);
        // The runtime slot assignment is not an identity: main always hosts its
        // own id again after the restart.
        assert_eq!(
            carried.main_origin,
            crate::tffa::MAIN_MATCH,
            "main_origin is runtime-only"
        );

        let fresh = tffa_after_reset(Some(&previous), false);
        assert_eq!(fresh.matches[1].remembered[5], crate::tffa::REMEMBERED_NONE);
        assert!(!fresh.matches[1].has_remembered());
        assert_eq!(
            fresh.matches[crate::tffa::MAIN_MATCH as usize].remembered[6],
            crate::tffa::REMEMBERED_NONE
        );
        assert_eq!(fresh.home[5], crate::tffa::MAIN_MATCH);
        assert_eq!(fresh.home[7], crate::tffa::MAIN_MATCH);
    }

    #[test]
    fn map_restart_keeps_admin_mutes_and_a_map_change_clears_them() {
        let mut previous = ProxyState::default();
        previous.clients[2].is_connected = true;
        previous.clients[2].muted = true;
        previous.clients[5].muted = true;

        // A `map_restart` keeps every player in their slot, so the mute stays
        // on the player it was issued for.
        let carried = mutes_after_reset(Some(&previous), true);
        assert!(carried[2] && carried[5]);
        assert_eq!(carried.iter().filter(|&&m| m).count(), 2);

        // A full map change drops everyone: the slots go to new players and
        // must start unmuted.
        let fresh = mutes_after_reset(Some(&previous), false);
        assert!(!fresh.iter().any(|&muted| muted));
        // The first `GAME_INIT` after a module load carries nothing either.
        assert!(!mutes_after_reset(None, true).iter().any(|&muted| muted));
    }

    #[test]
    fn map_restart_keeps_ignore_masks_and_a_map_change_clears_them() {
        let mut previous = ProxyState::default();
        previous.clients[2].ignored = (1 << 5) | (1 << 7);
        previous.clients[5].ignored = 1 << 2;

        // A `map_restart` keeps every player in their slot, so each mask still
        // names the same neighbours.
        let carried = ignores_after_reset(Some(&previous), true);
        assert_eq!(carried[2], (1 << 5) | (1 << 7));
        assert_eq!(carried[5], 1 << 2);
        assert_eq!(carried.iter().filter(|&&mask| mask != 0).count(), 2);

        // A full map change drops everyone: the slots go to new players and
        // must start with no ignores.
        let fresh = ignores_after_reset(Some(&previous), false);
        assert!(fresh.iter().all(|&mask| mask == 0));
        // The first `GAME_INIT` after a module load carries nothing either.
        assert!(
            ignores_after_reset(None, true)
                .iter()
                .all(|&mask| mask == 0)
        );
    }

    #[test]
    fn full_map_change_starts_from_a_fresh_overlay() {
        let mut previous = TffaState::default();
        previous.matches[1].active = true;
        previous.client_match[3] = 1;

        let fresh = tffa_after_reset(Some(&previous), false);
        assert!(!fresh.matches[1].active);
        assert_eq!(fresh.client_match[3], crate::tffa::MAIN_MATCH);

        // The first `GAME_INIT` after a module load has no previous round at
        // all: a restart flag there carries nothing either.
        let first = tffa_after_reset(None, true);
        assert!(!first.matches[1].active);
        assert_eq!(first.client_match[3], crate::tffa::MAIN_MATCH);
    }
}
