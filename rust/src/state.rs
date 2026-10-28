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
/// The four dropped cvars (`pingFix`, `antiWallHack`, `disableKillCmd`,
/// `sabersFps`) are gone with their features.
pub const PROXY_CVARS: [(&CStr, &CStr); 13] = [
    (c"proxy_sv_enable", c"1"),
    (c"proxy_sv_enableRconCmdCooldown", c"0"),
    (c"proxy_sv_enableNetStatus", c"0"),
    (c"proxy_sv_maxCallVoteMapRestartValue", c"60"),
    (c"proxy_sv_modelPathLength", c"64"),
    (c"proxy_sv_antiHpTeller", c"0"),
    (c"proxy_sv_minJumpTime", c"0"),
    (c"proxy_sv_enableEndGameStats", c"1"),
    (c"proxy_sv_lockTeamsAfterXSecs", c"0"),
    (c"proxy_sv_teamSizeRules", c""),
    (c"proxy_tffa_enable", c"0"),
    (c"proxy_tffa_maxMatches", c"8"),
    (c"proxy_tffa_maxGroupSize", c"12"),
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
/// Team-lock grace delay in seconds before a round's teams lock (`0` = off).
pub const CVAR_LOCK_TEAMS_AFTER_SECS: usize = 8;
/// Per-team-size `timelimit`/`fraglimit`/`capturelimit` rules (reconciled every
/// frame with the roster, independent of `proxy_sv_lockTeamsAfterXSecs`).
pub const CVAR_TEAM_SIZE_RULES: usize = 9;
/// Parallel TFFA matches (`GT_TEAM` only): `0` off, non-zero on.
pub const CVAR_TFFA_ENABLE: usize = 10;
/// Maximum number of parallel matches.
pub const CVAR_TFFA_MAX_MATCHES: usize = 11;
/// Maximum players per match (both teams combined).
pub const CVAR_TFFA_MAX_GROUP_SIZE: usize = 12;

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
}

impl TffaMatch {
    /// Whether the match's outcome is decided but its end-of-match delay is
    /// still running: damage inside it is void and it has not been announced
    /// or moved to spectator yet.
    pub fn ending(&self) -> bool {
        self.active && self.end_queued_at != 0
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
    /// move (`tffa_create`/`tffa_join`/`tffa_leave`, or a closed match returning
    /// them to main), never when a match is promoted into the main slot or when
    /// the client picks a team. A `map_restart` returns every client to this id
    /// (`tffa::reset_round_state`), so TFFA 0 stays TFFA 0 and TFFA 1 stays
    /// TFFA 1 even after a promotion moved their games around (D-006.11).
    pub home: [u8; MAX_CLIENTS],
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
    /// Last per-viewer `CS_SCORES1/2` sent, to skip redundant corrections
    /// (the 128-slot reliable buffer overflows otherwise: every frag would
    /// re-push unchanged scores to every client).
    pub cs_scores_sent: [(i32, i32); MAX_CLIENTS],
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
    pub world_release: u16,
    /// Instances whose clone sets exist, as a bitmask (bit `i` = instance `i`).
    /// Clone creation is idempotent: re-opening a slot whose set survived
    /// reuses the world it left behind.
    pub world_cloned: u16,
    /// Whether the map holds an interactive entity that cannot be duplicated
    /// (an NPC/actor with a shared Ghoul2 model or a dynamic `client`): such a
    /// map takes no instance at all — all-or-nothing, D-006.6.
    pub world_clone_blocked: bool,
    /// Whether `scan_world_entities` has measured this map. A server that turns
    /// `proxy_tffa_enable` on mid-map has not scanned yet, and an unmeasured
    /// map must not take an instance (an empty clone set would silently leave
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
    /// Last `proxy_tffa_enable` value the per-frame cvar mirror showed
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
            main_origin: crate::tffa::MAIN_MATCH,
            player_score: [0; MAX_CLIENTS],
            frame_stamp: 0,
            rebuilt_stamp: 0,
            owned: [(0, 0); crate::tffa::OWNED_CAP],
            owned_count: 0,
            sound_source: [crate::tffa::SOURCE_WORLD; MAX_GENTITIES],
            pending_sound_source: crate::tffa::SOURCE_WORLD,
            cs_players_base: core::array::from_fn(|_| String::new()),
            cs_scores_sent: [(i32::MIN, i32::MIN); MAX_CLIENTS],
            viewer_match_cache: [0; MAX_CLIENTS],
            intermission_boards: [0; MAX_CLIENTS],
            pending_join: [crate::tffa::REMEMBERED_NONE; MAX_CLIENTS],
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

/// Per-client bookkeeping (`Proxy_Header.hpp:87-106`).
#[derive(Debug, Clone, Copy)]
pub struct ClientEntry {
    pub is_connected: bool,
    pub clean_name: [u8; MAX_NETNAME],
    /// Admin mute (D-011): the client's `say`/`say_team`/`tell`/`voice_cmd`
    /// commands are dropped instead of being forwarded to the game. Set and
    /// cleared by the `mute`/`unmute` console commands, and dropped with the
    /// rest of the entry when the client disconnects (the slot belongs to the
    /// next player after that).
    pub muted: bool,
    /// netStatus estimator state (D-007): per-packet window, fps/pps ring
    /// semantics and the reset rules.
    pub netstatus: NetstatusState,
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
            clean_name: [0; MAX_NETNAME],
            muted: false,
            netstatus: NetstatusState::default(),
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
    pub tffa: TffaState,
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
    let (tffa, muted) = {
        let previous = guard.as_ref();
        (
            tffa_after_reset(previous.map(|s| &s.tffa), restart),
            mutes_after_reset(previous, restart),
        )
    };
    let mut next = ProxyState::default();
    next.tffa = tffa;
    // A `map_restart` keeps every player in their slot, so an admin mute stays
    // on the player it was issued for. A full map change drops everyone (the
    // slots are handed to new players) and starts unmuted.
    for (entry, muted) in next.clients.iter_mut().zip(muted) {
        entry.muted = muted;
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

/// The fresh [`TffaState`] a `GAME_INIT` starts from, carrying the match overlay
/// of the round that just ended only for a `map_restart`: which created matches
/// were open, which id each client belongs to (its `home`, which survives a
/// promotion, D-006.11), and the rosters a finished game remembers (D-006.10).
/// The runtime-only `main_origin` is not carried: after the restart the main
/// slot hosts its own id (0) again.
///
/// Kept separate from the lock so the carry is unit-tested directly.
fn tffa_after_reset(previous: Option<&TffaState>, restart: bool) -> TffaState {
    let mut fresh = TffaState::default();
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
    }
    fresh.client_match = previous.client_match;
    fresh.home = previous.home;
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
            tffa: TffaState::default(),
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
        assert_eq!(PROXY_CVARS[CVAR_LOCK_TEAMS_AFTER_SECS].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_TEAM_SIZE_RULES].1.to_bytes(), b"");
        assert_eq!(PROXY_CVARS[CVAR_TFFA_ENABLE].1.to_bytes(), b"0");
        assert_eq!(PROXY_CVARS[CVAR_TFFA_MAX_MATCHES].1.to_bytes(), b"8");
        assert_eq!(PROXY_CVARS[CVAR_TFFA_MAX_GROUP_SIZE].1.to_bytes(), b"12");
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

        let carried = tffa_after_reset(Some(&previous), true);
        // Which matches were open and who was in them survives.
        assert!(carried.matches[2].active);
        assert!(carried.matches[5].active);
        assert_eq!(carried.client_match[4], 2);
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
