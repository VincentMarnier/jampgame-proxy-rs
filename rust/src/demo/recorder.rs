//! Server-side demo recorder: one stream per instance (Phase 3).
//!
//! A non-instanced gametype records the one server game (`main`); parallel
//! TFFA matches and duel courts each get their own recorder and file
//! (`tffa<N>` / `duel<N>`, 1-based). Membership comes from the existing
//! `tffa::client_match` table (written for both modes) and the duel court
//! list; no instance bookkeeping is duplicated here.
//!
//! Per recorder (proxy memory only) this module owns:
//!
//! - a 32-slot `clientSnapshot_t`-compatible ring used as the playerstate /
//!   entity delta base;
//! - two `entityState_t` scratch slots (current + previous; 532 bytes each)
//!   and its own `num_entities`/`first_entity` indices;
//! - a reliable-command queue with its own sequence counter;
//! - a `msg_t`-compatible buffer (16 KiB, the engine's `MAX_MSGLEN`);
//! - the Phase 1 [`super::file::DemoWriter`] and gamestate encoder.
//!
//! It never appends to `svs.snapshotEntities`, never touches
//! `svs.nextSnapshotEntities` and never mutates `svs.clients`. The engine's
//! `SV_AddEntitiesVisibleFromPoint` is called with its own `frame`/`eNums`
//! pointers; the `recorder_build` flag makes the FOV/anti-wallhack post-filter
//! skip the build while the existing instance filter still runs.
//!
//! Configstrings are mirrored as the *instance's* view: `CS_PLAYERS` through
//! the existing per-viewer rewrite, `CS_SCORES1/2`/duel HUD from the match or
//! court tables, and the `scores` probe text through the same per-viewer
//! rewrite. Chat follows one named rule (`chat_command_for`): server-wide
//! broadcast `chat` only; never team chat, a private `tell` or a per-client
//! `print`.
//!
//! Vehicle playerstates are not emitted yet: a frame whose subject has
//! `ps.m_iVehicleNum != 0` is discarded (the engine would write a second
//! `MSG_WriteDeltaPlayerstate(..., qtrue)` block that Phase 2 does not
//! replicate). RMG maps are refused by the Phase 1 gamestate encoder.

use core::ffi::{CStr, c_char, c_int};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use crate::engine::{self, cvar_slots, vars};
use crate::sdk::{
    CS_CLIENT_DUELHEALTHS, CS_CLIENT_DUELISTS, CS_PLAYERS, CS_PRIMED, CS_SCORES1, CS_SCORES2,
    CS_SERVERINFO, CS_SYSTEMINFO, MAX_CLIENTS, MAX_CONFIGSTRINGS, MAX_GENTITIES,
    MAX_MAP_AREA_BYTES, MAX_MSGLEN, MAX_SNAPSHOT_ENTITIES, OFFSET_CLIENT_GENTITY,
    OFFSET_CLIENT_STATE, OFFSET_GCLIENT_SESS, OFFSET_GENTITY_S, OFFSET_PS_CLIENT_NUM,
    OFFSET_PS_COMMAND_TIME, OFFSET_PS_M_I_VEHICLE_NUM, OFFSET_PS_ORIGIN, OFFSET_PS_PM_FLAGS,
    OFFSET_PS_VIEWHEIGHT, OFFSET_SESS_SESSION_TEAM, OFFSET_SNAPSHOT_ENUMS_COUNT,
    OFFSET_SNAPSHOT_ENUMS_LIST, OFFSET_SV_CHECKSUM_FEED, OFFSET_SV_CONFIGSTRINGS,
    OFFSET_SV_ENTITY_BASELINE, OFFSET_SV_ENTITY_SNAPSHOT_COUNTER, OFFSET_SV_SNAPSHOT_COUNTER,
    OFFSET_SV_STATE, OFFSET_SV_SV_ENTITIES, OFFSET_SVS_CLIENTS, OFFSET_SVS_SNAPFLAG_SERVER_BIT,
    OFFSET_SVS_TIME, PACKET_BACKUP, PACKET_MASK, PMF_FOLLOW, SIZEOF_CLIENT_SNAPSHOT_T,
    SIZEOF_CLIENT_T, SIZEOF_PLAYER_STATE, SIZEOF_SV_ENTITY_T, SS_GAME, TEAM_SPECTATOR,
};
use crate::state::{self, CVAR_DEMO_DIR, CVAR_DEMO_ENABLE, InstanceMode};

use super::file::{DEFAULT_BUFFER_BYTES, DemoName, DemoWriter, FileSink, InstanceLabel, Timestamp};
use super::gamestate::{
    Baseline, ConfigString, ENTITY_STATE_SIZE, GamestateHeader, RmgTail, encode_gamestate,
};
use super::msg::MsgWriter;

const SVC_SERVERCOMMAND: u8 = 5;
const SVC_SNAPSHOT: u8 = 7;
const SVC_SETGAME: u8 = 8;
const SVC_EOF: u8 = 10;

const MAX_RELIABLE_COMMANDS: usize = 64;
const MAX_STRING_CHARS: usize = 1024;
/// `SV_SetConfigstring`'s `maxChunkSize = MAX_STRING_CHARS - 24`.
const CONFIGSTRING_CHUNK: usize = MAX_STRING_CHARS - 24;
const SCRATCH_SLOTS: usize = 2;
/// Concurrent recorders (TFFA allows 9 instances including the main slot but
/// caps created matches at 8; the duel ladder allows 16 courts). Each recorder
/// costs its writer buffer plus the two entity scratch slots (~1.2 MiB).
const MAX_DEMO_RECORDERS: usize = 16;
const MSG_BUFFER_LEN: usize = MAX_MSGLEN;
const GENTITYNUM_BITS: c_int = 10;

/// `msg_t` (i386, `qcommon.h:17-26`): 32 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Msg {
    allowoverflow: i32,
    overflowed: i32,
    oob: i32,
    /// Engine pointer, 32-bit (i386 `msg_t`); fixed width so the host test
    /// checks the same layout the engine sees.
    data: u32,
    maxsize: i32,
    cursize: i32,
    readcount: i32,
    bit: i32,
}

/// `clientSnapshot_t` (i386, `server.h:96-114`): 3160 bytes, no
/// `_ONEBIT_COMBO` pointers in the shipped build.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ClientSnapshot {
    areabytes: i32,
    areabits: [u8; MAX_MAP_AREA_BYTES],
    ps: [u8; SIZEOF_PLAYER_STATE],
    vps: [u8; SIZEOF_PLAYER_STATE],
    num_entities: i32,
    first_entity: i32,
    message_sent: i32,
    message_acked: i32,
    message_size: i32,
}

impl Default for ClientSnapshot {
    fn default() -> Self {
        Self {
            areabytes: 0,
            areabits: [0; MAX_MAP_AREA_BYTES],
            ps: [0; SIZEOF_PLAYER_STATE],
            vps: [0; SIZEOF_PLAYER_STATE],
            num_entities: 0,
            first_entity: 0,
            message_sent: 0,
            message_acked: 0,
            message_size: 0,
        }
    }
}

/// `snapshotEntityNumbers_t` (`sv_snapshot.cpp:264-268`): 4 + 4096 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SnapshotEntityNumbers {
    count: i32,
    list: [i32; MAX_SNAPSHOT_ENTITIES],
}

impl Default for SnapshotEntityNumbers {
    fn default() -> Self {
        Self {
            count: 0,
            list: [0; MAX_SNAPSHOT_ENTITIES],
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DeltaBase {
    sequence: i32,
    scratch_slot: usize,
}

/// The instance one recorder belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DemoInstance {
    /// Non-instanced gametype: the one server game.
    Main,
    /// Parallel TFFA match id (`0` = the main slot's match).
    Tffa(u8),
    /// Parallel duel court index (`0` = the first court).
    Duel(u8),
}

impl DemoInstance {
    /// Demo file label. Phase 1 naming is 1-based (`main`, `tffa<N>`,
    /// `duel<N>`), so TFFA match `0` and duel court `0` become `<kind>1`.
    fn label(self) -> InstanceLabel {
        match self {
            DemoInstance::Main => InstanceLabel::Main,
            DemoInstance::Tffa(id) => InstanceLabel::Tffa(u32::from(id) + 1),
            DemoInstance::Duel(court) => InstanceLabel::Duel(u32::from(court) + 1),
        }
    }

    /// The match/court id used by the instance tables and the per-viewer
    /// rewrites.
    fn viewer_match(self) -> u8 {
        match self {
            DemoInstance::Main => crate::instances::MAIN_MATCH,
            DemoInstance::Tffa(id) => id,
            DemoInstance::Duel(court) => court,
        }
    }
}

/// One open segment.
struct Active {
    writer: DemoWriter<FileSink>,
    path: PathBuf,
    /// Next frame sequence to write. `1` = gamestate, `2` = svc_setgame; the
    /// first snapshot is `3`.
    sequence: i32,
    base: Option<DeltaBase>,
    frames: Vec<ClientSnapshot>,
    /// Two `MAX_SNAPSHOT_ENTITIES`-sized scratch slots of 532-byte states.
    entities: Vec<u8>,
    entity_numbers: Box<SnapshotEntityNumbers>,
    msg_buf: Vec<u8>,
    commands: VecDeque<(i32, Vec<u8>)>,
    next_command_sequence: i32,
    /// Last value observed for every configstring (for `cs` command dedup).
    configstrings: Vec<Option<Vec<u8>>>,
    /// Next `svs.time` at which the game is asked to build a `scores` command.
    next_scoreboard: i32,
    /// Last `scores` command queued (identical boards are not repeated).
    last_scoreboard: Option<Vec<u8>>,
    /// One-shot log when the loaded module has no scoreboard generator.
    scoreboard_unavailable: bool,
    /// Followed player slot (a human client of this segment's instance).
    subject: i32,
    /// Slot the gamestate names as "me"; see `pick_viewer_slot`.
    viewer_slot: i32,
    /// Instance this segment records.
    instance: DemoInstance,
    /// `CS_PLAYERS` round-trip state: the match table and
    /// `instances::cs_generation()` at the last per-viewer resync.
    membership: [u8; MAX_CLIENTS],
    cs_generation: u32,
    last_overflow_log: i32,
}

/// One instance's recorder and its open segment.
struct InstanceRecorder {
    instance: DemoInstance,
    active: Option<Box<Active>>,
}

/// Process-global recorder state. Kept out of `ProxyState` so the engine calls
/// in a build never happen under the proxy-state mutex.
#[derive(Default)]
pub struct Recorder {
    instances: Vec<InstanceRecorder>,
    /// Next segment number per instance, reset at `GAME_INIT`.
    segments: Vec<(DemoInstance, u32)>,
    next_tick: i32,
    /// One-shot log while `MAX_DEMO_RECORDERS` is reached.
    limit_logged: bool,
    /// `proxy_sv_demoMinFreeMB` is currently exceeded: segments were closed
    /// and no new one starts until the maintenance thread reports recovery.
    min_free_blocked: bool,
    /// One-shot log for the max-file-size guard in this generation.
    size_warned: bool,
    /// Per-instance `svs.time` before which no new segment starts (the
    /// max-file-size guard continues on the *next* tick).
    size_resume: Vec<(DemoInstance, i32)>,
    /// `BeginIntermission`: when the intermission tail has been recorded long
    /// enough to close the segments and print the links.
    intermission_deadline: Option<i32>,
    /// Intermission closed the segments: no new segment starts until the next
    /// `GAME_INIT`.
    intermission_closed: bool,
}

static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Recorder>> {
    RECORDER.lock().unwrap_or_else(|p| p.into_inner())
}

fn with_recorder<R>(f: impl FnOnce(&mut Recorder) -> R) -> R {
    let mut guard = lock();
    let recorder = guard.get_or_insert_with(Recorder::default);
    f(recorder)
}

// ---------------------------------------------------------------------------
// Engine memory helpers
// ---------------------------------------------------------------------------

unsafe fn read_i32(addr: usize) -> i32 {
    // SAFETY: caller supplies a valid engine address.
    unsafe { core::ptr::read_unaligned(addr as *const i32) }
}

unsafe fn write_i32(addr: usize, value: i32) {
    // SAFETY: caller supplies a valid writable engine address.
    unsafe { core::ptr::write_unaligned(addr as *mut i32, value) };
}

unsafe fn read_usize(addr: usize) -> usize {
    // SAFETY: caller supplies a valid engine address.
    unsafe { core::ptr::read_unaligned(addr as *const usize) }
}

fn read_i32_bytes(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn write_i32_bytes(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn read_f32_bytes(bytes: &[u8], offset: usize) -> f32 {
    f32::from_bits(u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

unsafe fn cvar_pointer(slot: usize) -> usize {
    // SAFETY: validated cvar slot (R-018); null before GAME_INIT.
    unsafe { read_usize(slot) }
}

unsafe fn cvar_int(slot: usize) -> c_int {
    let ptr = unsafe { cvar_pointer(slot) };
    if ptr == 0 {
        return 0;
    }
    // SAFETY: live cvar_t*.
    unsafe { engine::cvar_integer(ptr) }
}

unsafe fn cvar_bytes(slot: usize) -> Option<Vec<u8>> {
    let ptr = unsafe { cvar_pointer(slot) };
    if ptr == 0 {
        return None;
    }
    // SAFETY: live cvar_t*; string field is a NUL-terminated char*.
    let text = unsafe { engine::cvar_string_ptr(ptr) };
    if text.is_null() {
        return None;
    }
    // SAFETY: NUL-terminated engine cvar string.
    Some(
        unsafe { CStr::from_ptr(text as *const c_char) }
            .to_bytes()
            .to_vec(),
    )
}

fn gamedir() -> Vec<u8> {
    // SAFETY: cvar slot validated; fs_gamedirvar is set before the game runs.
    let value = unsafe { cvar_bytes(cvar_slots::FS_GAMEDIRVAR) };
    value
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| b"base".to_vec())
}

fn map_name() -> Vec<u8> {
    // SAFETY: cvar slot validated; sv_mapname is set by the map load.
    let value = unsafe { cvar_bytes(cvar_slots::SV_MAPNAME) };
    value
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| b"map".to_vec())
}

fn demo_dir() -> PathBuf {
    let raw = state::with_state(|s| {
        let field = &s.cvars[CVAR_DEMO_DIR].string;
        let len = field.iter().position(|&c| c == 0).unwrap_or(field.len());
        field[..len].iter().map(|&c| c as u8).collect::<Vec<u8>>()
    });
    if raw.is_empty() {
        PathBuf::from("demos")
    } else {
        PathBuf::from(String::from_utf8_lossy(&raw).into_owned())
    }
}

fn server_maxclients() -> usize {
    // SAFETY: cvar slot validated; returns 0 before the memory layer exists.
    let max = unsafe { cvar_int(cvar_slots::SV_MAXCLIENTS) };
    max.clamp(1, MAX_CLIENTS as c_int) as usize
}

fn sv_fps() -> c_int {
    // SAFETY: cvar slot validated.
    unsafe { cvar_int(cvar_slots::SV_FPS) }.max(1)
}

fn read_configstring(index: usize) -> Vec<u8> {
    if index >= MAX_CONFIGSTRINGS {
        return Vec::new();
    }
    // SAFETY: server.configstrings is a char*[MAX_CONFIGSTRINGS] at a fixed
    // offset; entries are engine-owned NUL-terminated strings.
    let ptr = unsafe { read_usize(vars::SV + OFFSET_SV_CONFIGSTRINGS + index * 4) };
    if ptr == 0 {
        return Vec::new();
    }
    // SAFETY: NUL-terminated configstring copied out before we release it.
    unsafe { CStr::from_ptr(ptr as *const c_char) }
        .to_bytes()
        .to_vec()
}

/// One engine client slot as the recorder's subject rule sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClientSlot {
    slot: i32,
    connected: bool,
    /// `state >= CS_ACTIVE`: the client has entered the world. A connecting
    /// client already has a `gentity` (the engine assigns it at direct
    /// connect), so it must not be treated as playing.
    in_world: bool,
    has_gentity: bool,
    spectator: bool,
    /// `GAME_CLIENT_CONNECT` reported a bot (`ClientState::is_bot`). Phase 3
    /// records human POVs only.
    bot: bool,
    /// Any engine client state but `CS_FREE`: the engine will not hand this
    /// slot to a new connection, and a demo viewer identity naming it can
    /// collide with whoever sits there.
    occupied: bool,
}

impl ClientSlot {
    /// In the world and playing, not spectating, not a bot. A spectating
    /// client's `ps` is `SpectatorClientEndFrame`'s copy of the player they
    /// follow (with `PMF_FOLLOW` and that player's `clientNum`), so recording
    /// them would silently follow someone else's POV instead of the
    /// spectator's slot.
    fn eligible(self) -> bool {
        self.connected && self.in_world && self.has_gentity && !self.spectator && !self.bot
    }
}

/// Why the current subject can no longer be recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubjectLoss {
    Left,
    Spectator,
    /// Still connected and playing, but no longer in this instance.
    Moved,
}

/// What the tick does with the subject this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubjectDecision {
    Keep,
    Switch { to: i32, reason: SubjectLoss },
    Stop,
}

/// `level.clients[slot].sess.sessionTeam == TEAM_SPECTATOR`.
///
/// Returns `false` when the game client array is not located or too small:
/// without the team the slot stays eligible, so a missing layout never blanks
/// the recording.
fn session_is_spectator(slot: i32, g_clients: usize, g_client_size: i32) -> bool {
    const NEED: usize = OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM + 4;
    if g_clients == 0 || g_client_size < NEED as i32 || slot < 0 {
        return false;
    }
    let address = g_clients
        + slot as usize * g_client_size as usize
        + OFFSET_GCLIENT_SESS
        + OFFSET_SESS_SESSION_TEAM;
    // SAFETY: slot is inside level.clients; sessionTeam is a plain int.
    unsafe { read_i32(address) == TEAM_SPECTATOR }
}

impl ClientSlot {
    const EMPTY: Self = Self {
        slot: 0,
        connected: false,
        in_world: false,
        has_gentity: false,
        spectator: false,
        bot: false,
        occupied: false,
    };
}

/// Snapshot every engine slot once per tick into `slots`; returns the number
/// of entries written. Preallocated by the caller so the recorded hot path
/// stays allocation-free.
fn read_slots(
    g_clients: usize,
    g_client_size: i32,
    bots: &[bool; MAX_CLIENTS],
    slots: &mut [ClientSlot; MAX_CLIENTS],
) -> usize {
    let max = server_maxclients().min(MAX_CLIENTS);
    // SAFETY: svs.clients is the engine's client_t* array.
    let clients = unsafe { read_usize(vars::SVS + OFFSET_SVS_CLIENTS) };
    for (index, out) in slots.iter_mut().enumerate().take(max) {
        let mut state = 0;
        let mut gentity = 0;
        if clients != 0 {
            let client = clients + index * SIZEOF_CLIENT_T;
            // SAFETY: index is inside the engine's client array.
            state = unsafe { read_i32(client + OFFSET_CLIENT_STATE) };
            // SAFETY: client_t field.
            gentity = unsafe { read_usize(client + OFFSET_CLIENT_GENTITY) };
        }
        let connected = state >= CS_PRIMED;
        let has_gentity = gentity != 0;
        let spectator = connected
            && has_gentity
            && session_is_spectator(index as i32, g_clients, g_client_size);
        // The engine's own discriminator (`SV_CalcPings`/`G_AddBot`):
        // `gentity->r.svFlags & SVF_BOT`. Game-created bots (`addbot`) get
        // their gentity flag but never pass through the proxy's
        // `GAME_CLIENT_CONNECT` mirror, so the mirror alone is not enough.
        let engine_bot = has_gentity
            && unsafe { read_i32(gentity + crate::sdk::OFFSET_SHARED_SVFLAGS) }
                & crate::sdk::SVF_BOT as i32
                != 0;
        *out = ClientSlot {
            slot: index as i32,
            connected,
            in_world: state >= crate::sdk::CS_ACTIVE,
            has_gentity,
            spectator,
            bot: bots[index] || engine_bot,
            occupied: state != 0,
        };
    }
    max
}

/// Whether `slot` may be the recorded POV of `instance`: Phase 2 eligibility
/// (in the world, playing, not spectating) plus human-only and this
/// instance's membership.
fn eligible_in(
    slot: &ClientSlot,
    instance: DemoInstance,
    membership: &[Option<DemoInstance>; MAX_CLIENTS],
) -> bool {
    slot.eligible() && membership.get(slot.slot as usize) == Some(&Some(instance))
}

/// Lowest eligible subject of `instance` (the recorder's rule).
fn lowest_subject(
    instance: DemoInstance,
    slots: &[ClientSlot],
    membership: &[Option<DemoInstance>; MAX_CLIENTS],
) -> Option<i32> {
    slots
        .iter()
        .find(|slot| eligible_in(slot, instance, membership))
        .map(|slot| slot.slot)
}

/// Keep the current POV, switch it to the lowest eligible player of the same
/// instance, or stop when nobody is left there.
///
/// A spectator demo switches follow targets by changing `ps.clientNum` in the
/// next snapshot (the engine does the same when a spectator cycles targets);
/// the file, sequence numbers and delta base continue unchanged.
fn choose_subject(
    current: i32,
    slots: &[ClientSlot],
    instance: DemoInstance,
    membership: &[Option<DemoInstance>; MAX_CLIENTS],
) -> SubjectDecision {
    let current_slot = slots.iter().find(|slot| slot.slot == current).copied();
    if current_slot.is_some_and(|slot| eligible_in(&slot, instance, membership)) {
        return SubjectDecision::Keep;
    }
    let reason = match current_slot {
        Some(slot) if !slot.connected || !slot.in_world || !slot.has_gentity => SubjectLoss::Left,
        Some(slot) if slot.spectator => SubjectLoss::Spectator,
        Some(_) => SubjectLoss::Moved,
        None => SubjectLoss::Left,
    };
    match lowest_subject(instance, slots, membership) {
        Some(next) => SubjectDecision::Switch { to: next, reason },
        None => SubjectDecision::Stop,
    }
}

/// The demo's viewer identity (`GamestateHeader::client_num`) — the slot a
/// client treats as "me" (cgame's `cg.clientNum`).
///
/// A client-recorded spectator demo uses the spectator's own client slot,
/// which never holds a player. A server-side recorder has no such slot, and
/// using the initial subject's slot is only safe until that slot is reused:
/// a player who reconnects there is then the demo's "me", and a client that
/// renders that slot from the followed player's state puts the newcomer at the
/// followed player's position (observed: player 0 reconnects into slot 0 after
/// the POV moved to player 1, and appears fused with player 1).
///
/// `sv_maxclients < MAX_CLIENTS` leaves slots the engine never assigns; take
/// the **highest** (`MAX_CLIENTS - 1`). With every slot assignable
/// (`sv_maxclients == MAX_CLIENTS`), take the **highest free** slot at segment
/// start; the per-tick `identity_lost` check ends the segment if a client ever
/// takes it. With no free slot the viewer is the subject's own slot, i.e. the
/// demo is that player's POV (the shape of a client-recorded demo:
/// `ps.clientNum == clientNum`, no `PMF_FOLLOW`).
fn pick_viewer_slot(subject_slot: i32, server_maxclients: usize, slots: &[ClientSlot]) -> i32 {
    if server_maxclients < MAX_CLIENTS {
        // The server never assigns any slot past `sv_maxclients`; take the
        // highest of them.
        return MAX_CLIENTS as i32 - 1;
    }
    slots
        .iter()
        .rev()
        .find(|slot| !slot.occupied)
        .map(|slot| slot.slot)
        .unwrap_or(subject_slot)
}

/// True when the demo's viewer identity has been compromised: the slot the
/// gamestate names as "me" is held by a client that is not the followed
/// player. The tick checks this **before** building a snapshot, so a demo
/// never contains a frame in which a stranger's entity carries the demo's
/// `clientNum`; the segment closes instead.
fn identity_lost(viewer_slot: i32, subject: i32, slots: &[ClientSlot]) -> Option<i32> {
    slots
        .iter()
        .find(|slot| slot.slot == viewer_slot && slot.occupied && slot.slot != subject)
        .map(|slot| slot.slot)
}

/// Spectator-follow vs. self view: the engine's `PMF_FOLLOW` is only for a
/// viewer that follows someone else. When the demo's `clientNum` is the
/// subject itself the stream must look like a client-recorded demo.
fn follow_mode(subject: i32, viewer_slot: i32) -> bool {
    subject != viewer_slot
}

/// Instance membership of one client slot.
///
/// `client_match` is written for both instance modes (TFFA match ids, duel
/// court indices). In duel mode a client that is not in a court is benched or
/// spectating and has no instance of its own.
fn membership_of(
    slot: usize,
    mode: InstanceMode,
    matches: &[u8; MAX_CLIENTS],
    court_of: &[Option<u8>; MAX_CLIENTS],
) -> Option<DemoInstance> {
    match mode {
        InstanceMode::Off => Some(DemoInstance::Main),
        InstanceMode::Tffa => Some(DemoInstance::Tffa(matches[slot])),
        InstanceMode::Duel => court_of[slot].map(DemoInstance::Duel),
    }
}

// ---------------------------------------------------------------------------
// Configstring mirror (`cs` reliable commands)
// ---------------------------------------------------------------------------

/// Build the engine's own `SV_SetConfigstring` command text for a runtime
/// configstring change. The normal path is exactly `cs %i "%s"\n`; values of
/// `maxChunkSize` (1000) or more use the engine's `bcs0`/`bcs1`/`bcs2`
/// reassembly protocol. There is deliberately no escaping: the shipped demos
/// contain raw info-string backslashes and quote-delimited names, matching the
/// engine's `vsprintf` output.
pub fn configstring_command(index: i32, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() + 32);
    if value.len() >= CONFIGSTRING_CHUNK {
        let mut sent = 0usize;
        while sent < value.len() {
            let tag: &[u8] = if sent == 0 {
                b"bcs0"
            } else if value.len() - sent < CONFIGSTRING_CHUNK {
                b"bcs2"
            } else {
                b"bcs1"
            };
            let end = (sent + CONFIGSTRING_CHUNK - 1).min(value.len());
            out.extend_from_slice(tag);
            out.push(b' ');
            out.extend_from_slice(index.to_string().as_bytes());
            out.extend_from_slice(b" \"");
            out.extend_from_slice(&value[sent..end]);
            out.extend_from_slice(b"\"\n");
            sent = end;
        }
    } else {
        out.extend_from_slice(b"cs ");
        out.extend_from_slice(index.to_string().as_bytes());
        out.extend_from_slice(b" \"");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\"\n");
    }
    out
}

/// Queue one reliable command for the next written snapshot, with its own
/// sequence number.
fn queue_command(active: &mut Active, command: Vec<u8>) {
    active.next_command_sequence = active.next_command_sequence.wrapping_add(1);
    let sequence = active.next_command_sequence;
    if active.commands.len() >= MAX_RELIABLE_COMMANDS {
        // The demo client's reliable window is the same 64 entries; dropping
        // the oldest keeps the newest state reachable. Changes are infrequent,
        // so this is a safety valve, not the norm.
        active.commands.pop_front();
    }
    active.commands.push_back((sequence, command));
}

/// Queue one `cs <index> "<value>"` if it differs from what this demo already
/// carries.
fn queue_configstring(active: &mut Active, index: usize, value: Vec<u8>) {
    if index >= MAX_CONFIGSTRINGS {
        return;
    }
    let changed = match &active.configstrings[index] {
        Some(previous) => previous != &value,
        None => !value.is_empty(),
    };
    if !changed {
        return;
    }
    let command = configstring_command(index as i32, &value);
    queue_command(active, command);
    active.configstrings[index] = if value.is_empty() { None } else { Some(value) };
}

/// `CS_PLAYERS + slot` as this instance's viewer holds it: the existing
/// per-viewer rewrite (TFFA demotes and prefixes outsiders, the duel ladder
/// labels courts and injects W/L).
fn cs_players_value(
    base: &[u8],
    slot: usize,
    matches: &[u8; MAX_CLIENTS],
    instance: DemoInstance,
) -> Vec<u8> {
    let Ok(text) = core::str::from_utf8(base) else {
        return base.to_vec();
    };
    match crate::instances::rewrite_cs_players_for_slot(
        text,
        slot,
        matches[slot],
        instance.viewer_match(),
    ) {
        Some(value) => value.into_bytes(),
        None => base.to_vec(),
    }
}

/// A configstring as this instance's demo carries it.
///
/// The proxy advertises `taystJKinfo` bit 7 (15-int `scores` layout) in the
/// live serverinfo so capable cgames get deaths. A demo has one stream for
/// every future cgame, and a stock cgame always parses the 14-int layout, so
/// the recording carries the stock layout: the key is removed from the
/// recorded `CS_SERVERINFO` and the `scores` commands are the game's stock
/// text. Live clients keep their advertisement untouched.
///
/// Instance values come from the same per-viewer functions the live path uses:
/// `CS_PLAYERS` through the TFFA/duel rewrite, `CS_SCORES1/2` from the match or
/// court table, and the duel HUD from the court. `base` is the engine's global
/// value (which the per-viewer code overrides).
fn instance_configstring(
    instance: DemoInstance,
    matches: &[u8; MAX_CLIENTS],
    index: usize,
    base: &[u8],
) -> Vec<u8> {
    if index as i32 == CS_SERVERINFO {
        return crate::userinfo::without_key(base, b"taystJKinfo");
    }
    // Non-instanced: the engine's own values (the match table may be stale
    // after instancing was switched off).
    if instance == DemoInstance::Main {
        return base.to_vec();
    }
    let index32 = index as i32;
    if (CS_PLAYERS..CS_PLAYERS + MAX_CLIENTS as i32).contains(&index32) {
        return cs_players_value(base, (index32 - CS_PLAYERS) as usize, matches, instance);
    }
    match instance {
        DemoInstance::Main => base.to_vec(),
        DemoInstance::Tffa(_) | DemoInstance::Duel(_) => {
            if let Some((red, blue)) = instance_scores(instance) {
                if index32 == CS_SCORES1 {
                    return red.to_string().into_bytes();
                }
                if index32 == CS_SCORES2 {
                    return blue.to_string().into_bytes();
                }
            }
            if let DemoInstance::Duel(court) = instance {
                if let Some((_, duelists, healths)) = crate::duel::court_hud(court as usize) {
                    if index32 == CS_CLIENT_DUELISTS {
                        return duelists.into_bytes();
                    }
                    if index32 == CS_CLIENT_DUELHEALTHS {
                        if let Some(healths) = healths {
                            return healths.into_bytes();
                        }
                    }
                }
            }
            base.to_vec()
        }
    }
}

/// Per-instance `CS_SCORES1/2` pair (TFFA match tally or duel court frags).
fn instance_scores(instance: DemoInstance) -> Option<(i32, i32)> {
    match instance {
        DemoInstance::Main => None,
        DemoInstance::Tffa(id) => Some(crate::instances::view_scores(id)),
        DemoInstance::Duel(court) => {
            crate::duel::court_hud(court as usize).map(|(scores, _, _)| scores)
        }
    }
}

/// Re-emit the instance's volatile configstrings when their source changed:
/// `CS_PLAYERS` when the match table or the proxy's per-viewer refresh
/// generation moved, and the per-match/court scores and duel HUD every tick
/// (cheap integer/string comparisons, deduped by `queue_configstring`).
fn sync_instance_configstrings(active: &mut Active, matches: &[u8; MAX_CLIENTS]) {
    let generation = crate::instances::cs_generation();
    if active.cs_generation != generation || active.membership != *matches {
        active.cs_generation = generation;
        active.membership = *matches;
        for slot in 0..MAX_CLIENTS {
            let index = CS_PLAYERS as usize + slot;
            let base = read_configstring(index);
            if base.is_empty() {
                continue;
            }
            let value = instance_configstring(active.instance, matches, index, &base);
            queue_configstring(active, index, value);
        }
    }
    if let Some((red, blue)) = instance_scores(active.instance) {
        queue_configstring(active, CS_SCORES1 as usize, red.to_string().into_bytes());
        queue_configstring(active, CS_SCORES2 as usize, blue.to_string().into_bytes());
    }
    if let DemoInstance::Duel(court) = active.instance {
        if let Some((_, duelists, healths)) = crate::duel::court_hud(court as usize) {
            queue_configstring(active, CS_CLIENT_DUELISTS as usize, duelists.into_bytes());
            if let Some(healths) = healths {
                queue_configstring(active, CS_CLIENT_DUELHEALTHS as usize, healths.into_bytes());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Scoreboard (`scores` reliable command)
// ---------------------------------------------------------------------------

/// How often the game is asked to rebuild the board. A demo viewer cannot ask
/// for one (cgame skips the request in demo playback), so the stream carries
/// the game's own text at this cadence.
const SCOREBOARD_INTERVAL_MS: i32 = 1000;
/// `SV_AddServerCommand` drops a command longer than `MAX_STRING_CHARS - 2`.
const MAX_COMMAND_BYTES: usize = MAX_STRING_CHARS - 2;

/// One in-flight `DeathmatchScoreboardMessage` call. The game answers it with
/// a single `trap_SendServerCommand`, which the syscall forwarder captures
/// here instead of delivering it to a live client.
struct ScoreboardProbe {
    armed: bool,
    client: i32,
    text: Option<Vec<u8>>,
}

impl ScoreboardProbe {
    const EMPTY: Self = Self {
        armed: false,
        client: -1,
        text: None,
    };

    fn arm(&mut self, client: i32) {
        self.armed = true;
        self.client = client;
        self.text = None;
    }

    /// Intercept the probe's own `scores` trap. Returns true when the command
    /// was captured and must not be forwarded to the engine.
    fn capture(&mut self, client: i32, text: &[u8]) -> bool {
        if !self.armed || client != self.client || !text.starts_with(b"scores ") {
            return false;
        }
        let mut command = text.to_vec();
        if command.last() != Some(&b'\n') {
            command.push(b'\n');
        }
        self.text = Some(command);
        self.armed = false;
        true
    }

    /// Finish one probe: nothing stays armed after the call, so a real
    /// client's own `scores` request in the following second is never mistaken
    /// for ours.
    fn take(&mut self) -> Option<Vec<u8>> {
        self.armed = false;
        self.text.take()
    }
}

/// Separate from `RECORDER`: the probe runs while the recorder mutex is held,
/// and the trap handler must not re-enter it (a second lock would deadlock).
static SCOREBOARD_PROBE: Mutex<ScoreboardProbe> = Mutex::new(ScoreboardProbe::EMPTY);

fn probe_lock() -> MutexGuard<'static, ScoreboardProbe> {
    SCOREBOARD_PROBE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Called from `G_SEND_SERVER_COMMAND`. True means "this is the recorder's own
/// probe; keep it out of the live client's reliable stream".
pub fn capture_scoreboard_command(client: c_int, text: &[u8]) -> bool {
    probe_lock().capture(client, text)
}

/// Ask the game for its authoritative scoreboard text and queue it, rewritten
/// to this instance's board with the same per-viewer function the live path
/// uses.
fn probe_scoreboard(active: &mut Active, now: i32, g_entities: usize, g_entity_size: i32) {
    if now < active.next_scoreboard {
        return;
    }
    active.next_scoreboard = now.saturating_add(SCOREBOARD_INTERVAL_MS);

    let subject = active.subject;
    let Some(build) = crate::original::deathmatch_scoreboard_message() else {
        if !active.scoreboard_unavailable {
            active.scoreboard_unavailable = true;
            eprintln!(
                "----- proxy-rs: demo scoreboard disabled: module does not export DeathmatchScoreboardMessage"
            );
        }
        return;
    };
    if g_entities == 0 || g_entity_size <= 0 || subject < 0 {
        return;
    }
    let ent = g_entities + subject as usize * g_entity_size as usize;
    probe_lock().arm(subject);
    // SAFETY: `ent` is the subject's gentity_t in the game's array; the
    // function only reads level data and sends one `scores` trap, which
    // `capture_scoreboard_command` intercepts.
    unsafe { build(ent) };
    let Some(command) = probe_lock().take() else {
        return;
    };
    let command = instance_scores_command(active.instance, subject, &command);
    if command.len() > MAX_COMMAND_BYTES {
        // The engine would refuse it; do not put an undeliverable command in
        // the demo stream.
        return;
    }
    if active.last_scoreboard.as_deref() == Some(command.as_slice()) {
        return;
    }
    active.last_scoreboard = Some(command.clone());
    queue_command(active, command);
}

/// Rewrite the game's global `scores ...` text for one instance's viewer.
/// `None` (or an unparseable text) passes the original through, exactly like
/// the live `G_SEND_SERVER_COMMAND` path.
fn instance_scores_command(instance: DemoInstance, viewer: i32, command: &[u8]) -> Vec<u8> {
    if instance == DemoInstance::Main {
        return command.to_vec();
    }
    let trimmed = command.strip_suffix(b"\n").unwrap_or(command);
    let Ok(text) = core::str::from_utf8(trimmed) else {
        return command.to_vec();
    };
    let rewritten = match instance {
        DemoInstance::Main => None,
        DemoInstance::Tffa(_) => crate::tffa::rewrite_scores_for_viewer(text, viewer),
        DemoInstance::Duel(_) => crate::duel::rewrite_scores_for_viewer(text, viewer),
    };
    match rewritten {
        Some(text) => {
            let mut out = text.into_bytes();
            out.push(b'\n');
            out
        }
        None => command.to_vec(),
    }
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

/// Bounded buffer of chat commands the proxy forwarded since the last tick.
/// The recorder tick runs while the recorder mutex is held, and the trap
/// handler must not re-enter it (a second lock would deadlock), so chat uses
/// its own small queue.
const MAX_PENDING_CHAT: usize = 256;
static PENDING_CHAT: Mutex<VecDeque<(i32, Vec<u8>)>> = Mutex::new(VecDeque::new());

/// The demo chat rule, deliberately in one place.
///
/// **Owner decision (2026-11-12): broadcasts only.** A demo carries the
/// server-wide `chat`/`lchat` line; team chat (`tchat`/`ltchat`), a private
/// `tell` and a per-client `print` are never recorded.
///
/// The game's `G_Say` does not broadcast with `-1`: it calls `G_SayTo` for
/// every client and sends that client its own `chat` command
/// (`g_cmds.c:1550-1554`). So the demo takes the copy **addressed to the
/// subject** (or a `-1` command, for a sender that still uses it); that copy
/// is by construction the broadcast that player received. Team chat is a
/// different command name (`tchat`/`ltchat`), so it never enters the stream
/// even though the subject also receives it; a `tell` is a `chat` line whose
/// name carries the ``ESC '['`` marker and is excluded; a `print` is not a
/// chat candidate at all.
pub fn chat_command_for(recipient: c_int, text: &[u8], subject: c_int) -> bool {
    chat_candidate(text) && !is_tell(text) && (recipient == subject || recipient < 0)
}

/// Server-wide chat commands only (the owner's "broadcasts only" rule):
/// `tchat`/`ltchat` are the per-team variants and are deliberately absent.
fn chat_candidate(text: &[u8]) -> bool {
    text.starts_with(b"chat ") || text.starts_with(b"lchat ")
}

/// A private `tell` reaches the game as the same `chat` command, addressed to
/// its target: `G_Say`'s `SAY_TELL` case wraps the sender's name in
/// `ESC '[' … ESC ']'` (`g_cmds.c:1601-1613`, `Q_COLOR_ESCAPE` is `0x19`), and
/// `Cmd_Tell_f` only sends it to that client and to the speaker
/// (`g_cmds.c:1689-1695`). The bracket marker is therefore the observable that
/// separates a tell from a broadcast `chat`/`lchat` line, and a demo must not
/// carry it.
fn is_tell(text: &[u8]) -> bool {
    [b"chat \"".as_slice(), b"lchat \"".as_slice()]
        .iter()
        .any(|prefix| {
            text.strip_prefix(*prefix)
                .is_some_and(|rest| rest.starts_with(b"\x19["))
        })
}

/// Called from `G_SEND_SERVER_COMMAND` after the engine accepted the command
/// (so a message the proxy dropped for this recipient is never queued).
pub fn on_server_command(recipient: c_int, text: &[u8]) {
    if !chat_candidate(text) {
        return;
    }
    if !state::proxy_enabled() || state::cvar_integer(CVAR_DEMO_ENABLE) == 0 {
        return;
    }
    if crate::diagnostics::verbose_enabled() {
        crate::diagnostics::verbose(
            "demo.chat",
            format!(
                "recipient={recipient} len={} text=\"{}\"",
                text.len(),
                crate::diagnostics::escape_bytes(text, 256)
            ),
        );
    }
    let mut pending = PENDING_CHAT.lock().unwrap_or_else(|p| p.into_inner());
    if pending.len() >= MAX_PENDING_CHAT {
        pending.pop_front();
    }
    pending.push_back((recipient, text.to_vec()));
}

/// Take the chat commands buffered since the last tick.
fn drain_chat() -> Vec<(i32, Vec<u8>)> {
    let mut pending = PENDING_CHAT.lock().unwrap_or_else(|p| p.into_inner());
    pending.drain(..).collect()
}

// ---------------------------------------------------------------------------
// ABI-aligned entry
// ---------------------------------------------------------------------------

/// Recorder entry points (values travel through the C realign shim).
const ENTRY_TICK: c_int = 0;
const ENTRY_GAME_INIT: c_int = 1;
const ENTRY_GAME_SHUTDOWN: c_int = 2;
const ENTRY_INTERMISSION: c_int = 3;

unsafe extern "C" {
    /// `csrc/syscall_shim.c`: realigns the stack to the i386 ABI, then calls
    /// `f`. The engine enters our detours and `vmMain` with a 4-byte-aligned
    /// stack while the precompiled std assumes 16 bytes and zeroes stack
    /// locals with `movaps` (D-001.1); every recorder entry below reaches
    /// std file-system code (create/open/write/stat), so it must run on an
    /// aligned stack.
    fn jampgame_aligned_entry(f: extern "C" fn(c_int), op: c_int);
}

extern "C" fn dispatch(op: c_int) {
    match op {
        ENTRY_TICK => tick_impl(),
        ENTRY_GAME_INIT => game_init_impl(),
        ENTRY_GAME_SHUTDOWN => game_shutdown_impl(),
        ENTRY_INTERMISSION => begin_intermission_impl(),
        _ => {}
    }
}

fn aligned(op: c_int) {
    // SAFETY: the shim calls `dispatch` exactly once with `op`.
    unsafe { jampgame_aligned_entry(dispatch, op) };
}

/// `SV_SendClientMessages` tick (after the pristine body), ABI-aligned.
pub fn on_sv_send_client_messages() {
    aligned(ENTRY_TICK);
}

/// `GAME_INIT`, ABI-aligned.
pub fn on_game_init() {
    aligned(ENTRY_GAME_INIT);
}

/// `GAME_SHUTDOWN` (and master-switch disable), ABI-aligned.
pub fn on_game_shutdown() {
    aligned(ENTRY_GAME_SHUTDOWN);
}

/// `BeginIntermission`, ABI-aligned: start the intermission tail timer.
pub fn on_begin_intermission() {
    aligned(ENTRY_INTERMISSION);
}

fn begin_intermission_impl() {
    with_recorder(|recorder| {
        if recorder.instances.is_empty() {
            return;
        }
        let seconds = state::cvar_integer(state::CVAR_DEMO_INTERMISSION_SECS).clamp(0, 30);
        // SAFETY: svs.time is the server snapshot clock.
        let now = unsafe { read_i32(vars::SVS + OFFSET_SVS_TIME) };
        recorder.intermission_deadline = Some(now.saturating_add(seconds.saturating_mul(1000)));
        recorder.intermission_closed = false;
    });
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Close every open segment, keeping the per-instance segment counters (used
/// by the disk guards, which must not restart numbering at `s1`).
fn close_instances(recorder: &mut Recorder) {
    for entry in recorder.instances.drain(..) {
        if let Some(active) = entry.active {
            stop_active(*active);
        }
    }
}

/// Close every open segment (map boundary, shutdown, master switch off). Each
/// file gets its `-1/-1` terminator; nothing crosses a map change.
fn close_all(recorder: &mut Recorder) {
    close_instances(recorder);
    recorder.segments.clear();
    recorder.limit_logged = false;
    recorder.intermission_deadline = None;
    recorder.intermission_closed = false;
}

/// `GAME_INIT`: close a previous map's files, reset timing and announce the
/// generation that just ended.
fn game_init_impl() {
    with_recorder(|recorder| {
        recorder.next_tick = 0;
        close_all(recorder);
        recorder.min_free_blocked = false;
        recorder.size_warned = false;
        recorder.size_resume.clear();
        recorder.intermission_deadline = None;
        recorder.intermission_closed = false;
    });
    crate::demo::publish::on_game_init();
}

/// `GAME_SHUTDOWN`: close every open file with the `-1/-1` terminator.
fn game_shutdown_impl() {
    with_recorder(|recorder| {
        close_all(recorder);
        recorder.next_tick = 0;
    });
}

/// `GAME_SET_CONFIGSTRING` has been forwarded to the engine: mirror the new
/// `sv.configstrings[index]` as the reliable `cs` command this instance's demo
/// needs (the per-viewer value for `CS_PLAYERS`, `CS_SCORES1/2` and the duel
/// HUD; the global value otherwise).
pub fn on_set_configstring(index: i32) {
    if !state::proxy_enabled() || state::cvar_integer(CVAR_DEMO_ENABLE) == 0 {
        return;
    }
    if index < 0 || index as usize >= MAX_CONFIGSTRINGS {
        return;
    }
    let base = read_configstring(index as usize);
    let matches = state::with_state(|s| s.tffa.client_match);
    with_recorder(|recorder| {
        for entry in &mut recorder.instances {
            let Some(active) = entry.active.as_mut() else {
                continue;
            };
            let value = instance_configstring(entry.instance, &matches, index as usize, &base);
            queue_configstring(active, index as usize, value);
        }
    });
}

// ---------------------------------------------------------------------------
// Tick
// ---------------------------------------------------------------------------

/// Runs after the pristine `SV_SendClientMessages` returns (once per SV_Frame).
fn tick_impl() {
    let enabled = state::proxy_enabled() && state::cvar_integer(CVAR_DEMO_ENABLE) != 0;
    with_recorder(|recorder| {
        if !enabled {
            close_all(recorder);
            return;
        }
        // SAFETY: sv is the fixed engine server_t.
        if unsafe { read_i32(vars::SV + OFFSET_SV_STATE) } != SS_GAME {
            return;
        }
        // SAFETY: svs.time is the server snapshot clock.
        let now = unsafe { read_i32(vars::SVS + OFFSET_SVS_TIME) };
        if now < recorder.next_tick {
            return;
        }
        recorder.next_tick = now.saturating_add(1000 / sv_fps());

        let mode = state::instance_mode();
        let (g_clients, g_client_size, g_entities, g_entity_size) = state::with_state(|s| {
            (
                s.located_game_data.g_clients,
                s.located_game_data.g_client_size,
                s.located_game_data.g_entities,
                s.located_game_data.g_entity_size,
            )
        });
        let (bots, matches, court_of) = state::with_state(|s| {
            let mut bots = [false; MAX_CLIENTS];
            for (index, client) in s.clients.iter().enumerate() {
                bots[index] = client.is_bot;
            }
            let mut court_of = [None; MAX_CLIENTS];
            for (court, entry) in s.duel.courts[..s.duel.court_count].iter().enumerate() {
                for player in entry.players() {
                    court_of[player as usize] = Some(court as u8);
                }
            }
            (bots, s.tffa.client_match, court_of)
        });
        let mut slots = [ClientSlot::EMPTY; MAX_CLIENTS];
        let len = read_slots(g_clients, g_client_size, &bots, &mut slots);
        let slots = &slots[..len];
        let mut membership = [None; MAX_CLIENTS];
        for (slot, entry) in membership.iter_mut().enumerate().take(len) {
            *entry = membership_of(slot, mode, &matches, &court_of);
        }

        // Phase 4 disk guards. The free-space reading comes from the
        // maintenance thread (statvfs on the game thread is D-001.1 class);
        // it is at most one second old and every value below is a plain
        // atomic read.
        let min_free_mb = i64::from(state::cvar_integer(state::CVAR_DEMO_MIN_FREE_MB).max(0));
        let free_bytes = crate::demo::publish::free_bytes();
        let below_minimum =
            min_free_mb > 0 && free_bytes >= 0 && free_bytes < min_free_mb * 1024 * 1024;
        if below_minimum && !recorder.min_free_blocked {
            recorder.min_free_blocked = true;
            eprintln!(
                "----- proxy-rs: demo recording paused: {free_bytes} bytes free is below proxy_sv_demoMinFreeMB {min_free_mb} MB"
            );
            // Keep the per-instance segment counters: resuming continues the
            // numbering instead of colliding with the files just closed.
            close_instances(recorder);
        } else if !below_minimum && recorder.min_free_blocked {
            recorder.min_free_blocked = false;
            eprintln!(
                "----- proxy-rs: demo recording resumed: {free_bytes} bytes free (minimum {min_free_mb} MB)"
            );
        }

        // The boundary demo block is a `print` broadcast sent once a player is
        // back in the world (a full map change runs `GAME_INIT` before anyone
        // reconnects).
        crate::demo::publish::flush_announcement(
            slots
                .iter()
                .any(|slot| slot.in_world && !slot.bot && slot.connected),
        );

        // `BeginIntermission`: keep recording the intermission tail for a few
        // seconds (final scores visible in the demo), then close and print the
        // links to the clients who are still in intermission.
        if let Some(deadline) = recorder.intermission_deadline {
            if now >= deadline {
                recorder.intermission_deadline = None;
                recorder.intermission_closed = true;
                close_instances(recorder);
                crate::demo::publish::announce_intermission();
            }
        }

        // Chat commands forwarded by the proxy since the last tick, screened
        // per recorder by the one named rule.
        let chats = drain_chat();

        let max_file_mb = i64::from(state::cvar_integer(state::CVAR_DEMO_MAX_FILE_MB).max(0));
        let tick_ms = 1000 / sv_fps().max(1);

        // 1. Advance every open segment.
        for entry in &mut recorder.instances {
            let Some(active) = entry.active.as_mut() else {
                continue;
            };
            let decision = choose_subject(active.subject, slots, entry.instance, &membership);
            match decision {
                SubjectDecision::Keep => {}
                SubjectDecision::Stop => {
                    if let Some(active) = entry.active.take() {
                        stop_active(*active);
                    }
                    continue;
                }
                SubjectDecision::Switch { to, reason } => {
                    let from = active.subject;
                    let why = match reason {
                        SubjectLoss::Left => "left",
                        SubjectLoss::Spectator => "spectator",
                        SubjectLoss::Moved => "moved instance",
                    };
                    eprintln!(
                        "----- proxy-rs: demo subject switched: {from} -> {to} ({why}), continuing {}",
                        active.path.display()
                    );
                    active.subject = to;
                }
            }

            // The viewer identity must never name a player other than the one
            // being followed. Checked before a snapshot is built, so no frame
            // with the collision can reach the file.
            if let Some(by) = identity_lost(active.viewer_slot, active.subject, slots) {
                eprintln!(
                    "----- proxy-rs: demo viewer slot {} taken by client {by}; closing {}",
                    active.viewer_slot,
                    active.path.display()
                );
                if let Some(active) = entry.active.take() {
                    stop_active(*active);
                }
                continue;
            }

            sync_instance_configstrings(active, &matches);
            for (recipient, text) in &chats {
                if chat_command_for(*recipient, text, active.subject) {
                    let mut command = text.clone();
                    if command.last() != Some(&b'\n') {
                        command.push(b'\n');
                    }
                    if command.len() <= MAX_COMMAND_BYTES {
                        queue_command(active, command);
                    }
                }
            }
            probe_scoreboard(active, now, g_entities, g_entity_size);
            let written = build_snapshot(active, now);
            if !written {
                if let Some(active) = entry.active.take() {
                    stop_active(*active);
                }
                continue;
            }
            // Max-file-size guard: close a segment that reached the cap and
            // continue with the next segment number on the next tick.
            if size_cap_reached(active.writer.bytes_written(), max_file_mb) {
                let instance = entry.instance;
                let bytes = active.writer.bytes_written();
                let path = active.path.clone();
                if let Some(active) = entry.active.take() {
                    stop_active(*active);
                }
                if !recorder.size_warned {
                    recorder.size_warned = true;
                    eprintln!(
                        "----- proxy-rs: demo segment reached proxy_sv_demoMaxFileMB {max_file_mb} MB ({bytes} bytes) — closed {}",
                        path.display()
                    );
                }
                recorder
                    .size_resume
                    .push((instance, now.saturating_add(tick_ms)));
            }
        }
        recorder.instances.retain(|entry| entry.active.is_some());

        // 2. Start a recorder for every instance that now has an eligible
        //    human and no open segment. The min-free and size guards keep
        //    this loop from restarting immediately.
        if !recorder.min_free_blocked {
            recorder.size_resume.retain(|(_, at)| *at > now);
        }
        for slot in 0..len {
            if recorder.min_free_blocked || recorder.intermission_closed {
                break;
            }
            if !slots[slot].eligible() {
                continue;
            }
            let Some(instance) = membership[slot] else {
                continue;
            };
            if recorder
                .instances
                .iter()
                .any(|entry| entry.instance == instance)
            {
                continue;
            }
            if recorder.instances.len() >= MAX_DEMO_RECORDERS {
                if !recorder.limit_logged {
                    recorder.limit_logged = true;
                    eprintln!(
                        "----- proxy-rs: demo recorder limit ({MAX_DEMO_RECORDERS}) reached; {} not recorded",
                        instance.label().as_label()
                    );
                }
                continue;
            }
            if recorder
                .size_resume
                .iter()
                .any(|(blocked, _)| *blocked == instance)
            {
                continue;
            }
            let Some(subject) = lowest_subject(instance, slots, &membership) else {
                continue;
            };
            let segment = next_segment(&mut recorder.segments, instance);
            if let Some(active) = try_start(now, instance, subject, slots, &matches, segment) {
                recorder.instances.push(InstanceRecorder {
                    instance,
                    active: Some(Box::new(active)),
                });
            }
        }
        if recorder.instances.len() < MAX_DEMO_RECORDERS {
            recorder.limit_logged = false;
        }
    });
}

/// Next file segment number for an instance (1-based, reset at `GAME_INIT`).
/// `proxy_sv_demoMaxFileMB` reached? `0` (or less) disables the guard.
fn size_cap_reached(bytes: u64, max_file_mb: i64) -> bool {
    max_file_mb > 0 && bytes >= (max_file_mb as u64) * 1024 * 1024
}

fn next_segment(segments: &mut Vec<(DemoInstance, u32)>, instance: DemoInstance) -> u32 {
    if let Some(entry) = segments.iter_mut().find(|(known, _)| *known == instance) {
        let segment = entry.1;
        entry.1 = entry.1.saturating_add(1);
        segment
    } else {
        segments.push((instance, 2));
        1
    }
}

fn try_start(
    now: i32,
    instance: DemoInstance,
    subject: i32,
    slots: &[ClientSlot],
    matches: &[u8; MAX_CLIENTS],
    segment: u32,
) -> Option<Active> {
    let viewer_slot = pick_viewer_slot(subject, server_maxclients(), slots);

    // RMG maps have a variable gamestate tail; the Phase 1 encoder refuses
    // them (U-010) rather than writing a wrong `int16 0`.
    // SAFETY: fixed engine global; null on a normal map.
    let rmg = unsafe { read_usize(vars::THE_RANDOM_MISSION_MANAGER) } != 0;
    if rmg {
        eprintln!(
            "----- proxy-rs: demo recording skipped ({}): RMG gamestate tail unsupported",
            instance.label().as_label()
        );
        return None;
    }

    let configs: Vec<(u16, Vec<u8>)> = (0..MAX_CONFIGSTRINGS)
        .filter_map(|index| {
            let base = read_configstring(index);
            let value = instance_configstring(instance, matches, index, &base);
            (!value.is_empty()).then_some((index as u16, value))
        })
        .collect();
    let configstrings: Vec<ConfigString<'_>> = configs
        .iter()
        .map(|(index, value)| ConfigString::new(*index, value))
        .collect();

    let mut baselines = Vec::new();
    for slot in 0..MAX_GENTITIES {
        let baseline = vars::SV
            + OFFSET_SV_SV_ENTITIES
            + slot * SIZEOF_SV_ENTITY_T
            + OFFSET_SV_ENTITY_BASELINE;
        // SAFETY: slot is inside server.svEntities; baseline.number at +0.
        if unsafe { read_i32(baseline) } == 0 {
            continue;
        }
        let mut state = [0u8; ENTITY_STATE_SIZE];
        // SAFETY: baseline is a full entityState_t in engine memory.
        unsafe {
            core::ptr::copy_nonoverlapping(
                baseline as *const u8,
                state.as_mut_ptr(),
                ENTITY_STATE_SIZE,
            )
        };
        baselines.push(Baseline::from_state(state));
    }

    // SAFETY: server.checksumFeed is a plain int.
    let checksum_feed = unsafe { read_i32(vars::SV + OFFSET_SV_CHECKSUM_FEED) };
    let header = GamestateHeader {
        last_client_command: 0,
        reliable_sequence: 0,
        client_num: viewer_slot,
        checksum_feed,
    };
    let gamestate = match encode_gamestate(header, &configstrings, &baselines, RmgTail::NoManager) {
        Ok(payload) => payload,
        Err(error) => {
            eprintln!(
                "----- proxy-rs: demo gamestate refused ({}): {error}",
                instance.label().as_label()
            );
            return None;
        }
    };

    let timestamp = match Timestamp::now() {
        Ok(timestamp) => timestamp,
        Err(error) => {
            eprintln!("----- proxy-rs: demo recording skipped: clock: {error}");
            return None;
        }
    };
    // SAFETY: sv_mapname cvar; the value is a map path such as mp/ffa3.
    let map = map_name();
    let map = String::from_utf8_lossy(&map);
    let name = match DemoName::new(timestamp, &map, instance.label(), segment) {
        Ok(name) => name,
        Err(error) => {
            eprintln!("----- proxy-rs: demo name refused: {error}");
            return None;
        }
    };

    let directory = demo_dir();
    if let Err(error) = std::fs::create_dir_all(&directory) {
        eprintln!(
            "----- proxy-rs: demo directory {}: {error}",
            directory.display()
        );
        return None;
    }
    let path = directory.join(name.file_name());
    let sink = match FileSink::create(&path) {
        Ok(sink) => sink,
        Err(error) => {
            eprintln!("----- proxy-rs: demo file {}: {error}", path.display());
            return None;
        }
    };
    let mut writer = DemoWriter::new(sink, DEFAULT_BUFFER_BYTES);

    // Frame 1: the gamestate. Frame 2: the engine's one-shot svc_setgame.
    if let Err(error) = writer.write_frame(1, &gamestate) {
        eprintln!("----- proxy-rs: demo {}: {error}", path.display());
        let _ = std::fs::remove_file(&path);
        return None;
    }
    let setgame = match setgame_payload() {
        Ok(payload) => payload,
        Err(error) => {
            eprintln!("----- proxy-rs: demo setgame: {error}");
            let _ = std::fs::remove_file(&path);
            return None;
        }
    };
    if let Err(error) = writer.write_frame(2, &setgame) {
        eprintln!("----- proxy-rs: demo {}: {error}", path.display());
        let _ = std::fs::remove_file(&path);
        return None;
    }

    let mut configstring_mirror = vec![None; MAX_CONFIGSTRINGS];
    for (index, value) in &configs {
        if !value.is_empty() {
            configstring_mirror[*index as usize] = Some(value.clone());
        }
    }

    eprintln!(
        "----- proxy-rs: demo recording started: {} (instance {}, subject {}, viewer {}, time {now})",
        path.display(),
        instance.label().as_label(),
        subject,
        viewer_slot
    );
    // Phase 4: publish the name so the HTTP side keeps returning 404 until the
    // file is terminated (see `stop_active`).
    if let Some(file_name) = path.file_name() {
        crate::demo::publish::opened(&file_name.to_string_lossy());
    }

    Some(Active {
        writer,
        path,
        sequence: 3,
        base: None,
        frames: vec![ClientSnapshot::default(); PACKET_BACKUP],
        entities: vec![0u8; SCRATCH_SLOTS * MAX_SNAPSHOT_ENTITIES * ENTITY_STATE_SIZE],
        entity_numbers: Box::new(SnapshotEntityNumbers::default()),
        msg_buf: vec![0u8; MSG_BUFFER_LEN],
        commands: VecDeque::new(),
        next_command_sequence: 0,
        configstrings: configstring_mirror,
        subject,
        viewer_slot,
        instance,
        membership: *matches,
        cs_generation: crate::instances::cs_generation(),
        last_overflow_log: -1000,
        next_scoreboard: 0,
        last_scoreboard: None,
        scoreboard_unavailable: false,
    })
}

fn setgame_payload() -> Result<Vec<u8>, super::msg::MsgError> {
    let mut msg = MsgWriter::with_capacity(64);
    msg.write_long(0)?; // lastClientCommand
    msg.write_byte(SVC_SETGAME)?;
    msg.write_data(&gamedir())?;
    msg.write_byte(0)?;
    msg.write_byte(SVC_EOF)?;
    Ok(msg.into_payload())
}

fn stop_active(mut active: Active) {
    let frames = active.writer.frames_written();
    let bytes = active.writer.bytes_written();
    let name = active
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let result = active.writer.finish();
    match result {
        Ok(()) => {
            // Only a terminated file may be served (Phase 4).
            if let Some(name) = &name {
                crate::demo::publish::closed(name);
            }
            eprintln!(
                "----- proxy-rs: demo recording stopped: {} ({frames} frames, {bytes} bytes)",
                active.path.display()
            )
        }
        Err(error) => eprintln!(
            "----- proxy-rs: demo recording stop failed: {}: {error}",
            active.path.display()
        ),
    }
}

// ---------------------------------------------------------------------------
// One snapshot
// ---------------------------------------------------------------------------

fn copy_subject_ps(subject: i32, out: &mut [u8; SIZEOF_PLAYER_STATE]) -> bool {
    let (g_clients, g_client_size) = state::with_state(|s| {
        (
            s.located_game_data.g_clients,
            s.located_game_data.g_client_size,
        )
    });
    if g_clients == 0 || g_client_size < SIZEOF_PLAYER_STATE as i32 || subject < 0 {
        return false;
    }
    let source = g_clients + subject as usize * g_client_size as usize;
    // SAFETY: subject is a live client slot; gclient_t.ps is at offset 0.
    unsafe {
        core::ptr::copy_nonoverlapping(source as *const u8, out.as_mut_ptr(), SIZEOF_PLAYER_STATE)
    };
    true
}

fn build_snapshot(active: &mut Active, now: i32) -> bool {
    let sequence = active.sequence;
    let frame_index = (sequence as usize) & PACKET_MASK;
    let base = active.base;
    let base_index = base.map(|base| (base.sequence as usize) & PACKET_MASK);

    // Capture the base pointers/indices before touching the candidate frame.
    let from_ps = base_index
        .map(|index| active.frames[index].ps.as_ptr() as usize)
        .unwrap_or(0);
    let old_count = base_index
        .map(|index| active.frames[index].num_entities.max(0) as usize)
        .unwrap_or(0);
    let old_first = base_index
        .map(|index| active.frames[index].first_entity.max(0) as usize)
        .unwrap_or(0);

    let mut ps = [0u8; SIZEOF_PLAYER_STATE];
    if !copy_subject_ps(active.subject, &mut ps) {
        return false;
    }
    if follow_mode(active.subject, active.viewer_slot) {
        // SpectatorClientEndFrame: the watched player's ps with PMF_FOLLOW.
        // A self-view segment (viewer slot == subject) must stay a plain
        // client demo, so the flag is not added there.
        let flags = read_i32_bytes(&ps, OFFSET_PS_PM_FLAGS);
        write_i32_bytes(&mut ps, OFFSET_PS_PM_FLAGS, flags | PMF_FOLLOW);
    }
    if read_i32_bytes(&ps, OFFSET_PS_M_I_VEHICLE_NUM) != 0 {
        // The engine would append a second (vehicle) playerstate block; Phase 2
        // does not implement it yet (U-010). Drop this frame rather than
        // producing a stream the client misparses.
        if now.saturating_sub(active.last_overflow_log) >= 5000 {
            active.last_overflow_log = now;
            crate::diagnostics::verbose("demo", "vehicle playerstate unsupported; frame discarded");
        }
        return true;
    }
    let client_num = read_i32_bytes(&ps, OFFSET_PS_CLIENT_NUM);
    if !(0..MAX_CLIENTS as i32).contains(&client_num) {
        return false;
    }

    let (g_entities, g_entity_size) = state::with_state(|s| {
        (
            s.located_game_data.g_entities,
            s.located_game_data.g_entity_size,
        )
    });
    if g_entities == 0 || g_entity_size < ENTITY_STATE_SIZE as i32 {
        return false;
    }

    // SAFETY: sv.snapshotCounter is the engine's dedupe token; bumping it
    // before our build matches SV_BuildClientSnapshot. Real snapshots are
    // built earlier in this SV_Frame and use their own counter value.
    let counter = unsafe { read_i32(vars::SV + OFFSET_SV_SNAPSHOT_COUNTER) }.wrapping_add(1);
    unsafe { write_i32(vars::SV + OFFSET_SV_SNAPSHOT_COUNTER, counter) };
    // "never send client's own entity, because it can be regenerated from the
    // playerstate": pre-mark the subject's svEntity exactly like
    // SV_BuildClientSnapshot.
    let subject_sv_entity = vars::SV
        + OFFSET_SV_SV_ENTITIES
        + client_num as usize * SIZEOF_SV_ENTITY_T
        + OFFSET_SV_ENTITY_SNAPSHOT_COUNTER;
    // SAFETY: clientNum is a player slot (< MAX_CLIENTS), inside svEntities.
    unsafe { write_i32(subject_sv_entity, counter) };

    let origin = [
        read_f32_bytes(&ps, OFFSET_PS_ORIGIN),
        read_f32_bytes(&ps, OFFSET_PS_ORIGIN + 4),
        read_f32_bytes(&ps, OFFSET_PS_ORIGIN + 8)
            + read_i32_bytes(&ps, OFFSET_PS_VIEWHEIGHT) as f32,
    ];

    // Raw pointers keep the candidate frame and the base frame independent for
    // the borrow checker; frame_index != base_index because the sequence only
    // advances on a successfully written frame.
    // SAFETY: frame_index is inside the preallocated ring.
    let frame_ptr = unsafe { active.frames.as_mut_ptr().add(frame_index) };
    // SAFETY: frame_index is inside the preallocated ring.
    unsafe {
        (*frame_ptr).ps = ps;
        (*frame_ptr).areabytes = 0;
        (*frame_ptr).areabits = [0; MAX_MAP_AREA_BYTES];
        (*frame_ptr).num_entities = 0;
        (*frame_ptr).first_entity = 0;
        (*frame_ptr).message_sent = 0;
        (*frame_ptr).message_acked = 0;
        (*frame_ptr).message_size = 0;
    }
    let enums_ptr = active.entity_numbers.as_mut() as *mut SnapshotEntityNumbers;
    // SAFETY: our own eNums buffer.
    unsafe { (*enums_ptr).count = 0 };

    // The recorder build is exempt from the FOV cap and anti-wallhack (D-022 /
    // D-023); the wrapper still runs the instance filter. The guard clears the
    // flag on every path, including early returns from the engine wrapper.
    let build_guard = RecorderBuildGuard::new();
    // SAFETY: frame is our layout-compatible clientSnapshot_t; enums is our
    // snapshotEntityNumbers_t; origin is a stack vec3_t. The engine fills them.
    unsafe {
        crate::hooks::engine_sv::sv_add_entities_visible_from_point(
            origin.as_ptr() as usize,
            frame_ptr as usize,
            enums_ptr as usize,
            0,
        );
    }
    drop(build_guard);

    // SV_BuildClientSnapshot inverts the areamask after the visibility pass.
    // SAFETY: the engine wrote areabytes/areabits in our frame.
    let areabytes = unsafe { (*frame_ptr).areabytes.clamp(0, MAX_MAP_AREA_BYTES as i32) as usize };
    let mut areabits = unsafe { (*frame_ptr).areabits };
    for word in areabits.chunks_exact_mut(4) {
        let value = i32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        word.copy_from_slice(&(!value).to_le_bytes());
    }

    // SAFETY: the engine wrote count/list in our eNums buffer.
    let mut count = unsafe { (*enums_ptr).count.clamp(0, MAX_SNAPSHOT_ENTITIES as i32) as usize };
    // SAFETY: list has MAX_SNAPSHOT_ENTITIES entries; count is clamped.
    let list = unsafe { core::slice::from_raw_parts_mut((*enums_ptr).list.as_mut_ptr(), count) };
    list.sort_unstable();

    let scratch_slot = match base {
        Some(base) => 1 - base.scratch_slot,
        None => 0,
    };
    let first_entity = scratch_slot * MAX_SNAPSHOT_ENTITIES;
    let entities_ptr = active.entities.as_mut_ptr();
    let mut kept = 0usize;
    for &entity_number in list.iter() {
        if !(0..MAX_GENTITIES as i32).contains(&entity_number) {
            continue;
        }
        let source =
            g_entities + entity_number as usize * g_entity_size as usize + OFFSET_GENTITY_S;
        // SAFETY: entity_number comes from the engine's visible list; its
        // sharedEntity_t.s is a full entityState_t.
        unsafe {
            core::ptr::copy_nonoverlapping(
                source as *const u8,
                entities_ptr.add((first_entity + kept) * ENTITY_STATE_SIZE),
                ENTITY_STATE_SIZE,
            )
        };
        kept += 1;
    }
    count = kept;
    // SAFETY: frame_ptr is our candidate frame.
    unsafe {
        (*frame_ptr).num_entities = count as i32;
        (*frame_ptr).first_entity = first_entity as i32;
    }

    // Build the demo payload with the engine's own MSG writer.
    let mut msg = Msg::default();
    let msg_ptr = &mut msg as *mut Msg as usize;
    // SAFETY: msg is a valid msg_t; msg_buf is MAX_MSGLEN bytes.
    unsafe {
        engine::msg_init(
            msg_ptr,
            active.msg_buf.as_mut_ptr(),
            MSG_BUFFER_LEN as c_int,
        );
    }
    msg.allowoverflow = 1;

    let commands_written = active.commands.len();
    let lastframe = base
        .map(|base| sequence.wrapping_sub(base.sequence))
        .unwrap_or(0)
        .clamp(0, PACKET_MASK as i32);
    let entities_base = active.entities.as_ptr();
    let to_ps = unsafe { (*frame_ptr).ps.as_ptr() as usize };
    // SAFETY: msg is initialised; the pointers below are valid for the call.
    unsafe {
        engine::msg_write_long(msg_ptr, 0); // lastClientCommand
        for (command_sequence, command) in active.commands.iter().take(commands_written) {
            engine::msg_write_byte(msg_ptr, SVC_SERVERCOMMAND as c_int);
            engine::msg_write_long(msg_ptr, *command_sequence);
            engine::msg_write_data(msg_ptr, command.as_ptr(), command.len() as c_int);
            engine::msg_write_byte(msg_ptr, 0);
        }
        engine::msg_write_byte(msg_ptr, SVC_SNAPSHOT as c_int);
        engine::msg_write_long(msg_ptr, now);
        engine::msg_write_byte(msg_ptr, lastframe as c_int);
        // SAFETY: svs.snapFlagServerBit is a plain int.
        engine::msg_write_byte(
            msg_ptr,
            read_i32(vars::SVS + OFFSET_SVS_SNAPFLAG_SERVER_BIT),
        );
        engine::msg_write_byte(msg_ptr, (*frame_ptr).areabytes);
        engine::msg_write_data(msg_ptr, areabits.as_ptr(), areabytes as c_int);

        engine::msg_write_delta_playerstate(msg_ptr, from_ps, to_ps, 0);

        let mut old_index = 0usize;
        let mut new_index = 0usize;
        while new_index < count || old_index < old_count {
            let new_number = if new_index < count {
                read_i32(entities_base.add((first_entity + new_index) * ENTITY_STATE_SIZE) as usize)
            } else {
                9999
            };
            let old_number = if old_index < old_count {
                read_i32(entities_base.add((old_first + old_index) * ENTITY_STATE_SIZE) as usize)
            } else {
                9999
            };

            match new_number.cmp(&old_number) {
                core::cmp::Ordering::Equal => {
                    let old_ptr =
                        entities_base.add((old_first + old_index) * ENTITY_STATE_SIZE) as usize;
                    let new_ptr =
                        entities_base.add((first_entity + new_index) * ENTITY_STATE_SIZE) as usize;
                    engine::msg_write_delta_entity(msg_ptr, old_ptr, new_ptr, 0);
                    old_index += 1;
                    new_index += 1;
                }
                core::cmp::Ordering::Less => {
                    let baseline = vars::SV
                        + OFFSET_SV_SV_ENTITIES
                        + new_number as usize * SIZEOF_SV_ENTITY_T
                        + OFFSET_SV_ENTITY_BASELINE;
                    let new_ptr =
                        entities_base.add((first_entity + new_index) * ENTITY_STATE_SIZE) as usize;
                    engine::msg_write_delta_entity(msg_ptr, baseline, new_ptr, 1);
                    new_index += 1;
                }
                core::cmp::Ordering::Greater => {
                    let old_ptr =
                        entities_base.add((old_first + old_index) * ENTITY_STATE_SIZE) as usize;
                    engine::msg_write_delta_entity(msg_ptr, old_ptr, 0, 1);
                    old_index += 1;
                }
            }
        }
        engine::msg_write_bits(msg_ptr, MAX_GENTITIES as c_int - 1, GENTITYNUM_BITS);
        engine::msg_write_byte(msg_ptr, SVC_EOF as c_int);
    }

    if msg.overflowed != 0 {
        if now.saturating_sub(active.last_overflow_log) >= 5000 {
            active.last_overflow_log = now;
            crate::diagnostics::verbose("demo", "snapshot overflow; frame discarded");
        }
        return true;
    }
    let length = (msg.cursize.max(0) as usize).min(active.msg_buf.len());
    if length == 0 {
        return true;
    }
    // Distinct fields: the writer borrows the sink, the payload borrows the
    // fixed message buffer. No allocation per frame.
    let result = {
        let (writer, msg_buf) = (&mut active.writer, &active.msg_buf);
        writer.write_frame(sequence, &msg_buf[..length])
    };
    if let Err(error) = result {
        eprintln!(
            "----- proxy-rs: demo write {}: {error}",
            active.path.display()
        );
        return false;
    }
    for _ in 0..commands_written {
        active.commands.pop_front();
    }
    active.base = Some(DeltaBase {
        sequence,
        scratch_slot,
    });
    active.sequence = active.sequence.wrapping_add(1);
    true
}

/// RAII clear for `state.fov.recorder_build`.
struct RecorderBuildGuard;

impl RecorderBuildGuard {
    fn new() -> Self {
        state::with_state(|s| s.fov.recorder_build = true);
        Self
    }
}

impl Drop for RecorderBuildGuard {
    fn drop(&mut self) {
        state::with_state(|s| s.fov.recorder_build = false);
    }
}

// ---------------------------------------------------------------------------
// Layout tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    fn slot(index: i32, connected: bool, has_gentity: bool, spectator: bool) -> ClientSlot {
        ClientSlot {
            slot: index,
            connected,
            in_world: connected,
            has_gentity,
            spectator,
            bot: false,
            occupied: connected || has_gentity,
        }
    }

    fn human(index: i32) -> ClientSlot {
        slot(index, true, true, false)
    }

    fn occupied(index: i32) -> ClientSlot {
        human(index)
    }

    fn free(index: i32) -> ClientSlot {
        ClientSlot {
            slot: index,
            connected: false,
            in_world: false,
            has_gentity: false,
            spectator: false,
            bot: false,
            occupied: false,
        }
    }

    fn bot(index: i32) -> ClientSlot {
        ClientSlot {
            bot: true,
            ..human(index)
        }
    }

    fn membership(pairs: &[(i32, DemoInstance)]) -> [Option<DemoInstance>; MAX_CLIENTS] {
        let mut out = [None; MAX_CLIENTS];
        for (slot, instance) in pairs {
            out[*slot as usize] = Some(*instance);
        }
        out
    }

    #[test]
    fn msg_t_layout_matches_the_engine() {
        assert_eq!(size_of::<Msg>(), 32);
        assert_eq!(offset_of!(Msg, allowoverflow), 0);
        assert_eq!(offset_of!(Msg, overflowed), 4);
        assert_eq!(offset_of!(Msg, oob), 8);
        assert_eq!(offset_of!(Msg, data), 12);
        assert_eq!(offset_of!(Msg, maxsize), 16);
        assert_eq!(offset_of!(Msg, cursize), 20);
        assert_eq!(offset_of!(Msg, readcount), 24);
        assert_eq!(offset_of!(Msg, bit), 28);
    }

    #[test]
    fn client_snapshot_layout_matches_the_engine() {
        assert_eq!(size_of::<ClientSnapshot>(), SIZEOF_CLIENT_SNAPSHOT_T);
        assert_eq!(offset_of!(ClientSnapshot, areabytes), 0);
        assert_eq!(offset_of!(ClientSnapshot, areabits), 4);
        assert_eq!(offset_of!(ClientSnapshot, ps), 36);
        assert_eq!(offset_of!(ClientSnapshot, vps), 1588);
        assert_eq!(offset_of!(ClientSnapshot, num_entities), 3140);
        assert_eq!(offset_of!(ClientSnapshot, first_entity), 3144);
        assert_eq!(offset_of!(ClientSnapshot, message_sent), 3148);
        assert_eq!(offset_of!(ClientSnapshot, message_acked), 3152);
        assert_eq!(offset_of!(ClientSnapshot, message_size), 3156);
    }

    #[test]
    fn entity_number_buffer_layout_matches_the_engine() {
        assert_eq!(size_of::<SnapshotEntityNumbers>(), 4 + 4 * 1024);
        assert_eq!(offset_of!(SnapshotEntityNumbers, count), 0);
        assert_eq!(offset_of!(SnapshotEntityNumbers, list), 4);
    }

    #[test]
    fn demo_instance_labels_are_one_based() {
        assert_eq!(DemoInstance::Main.label().as_label(), "main");
        assert_eq!(DemoInstance::Tffa(0).label().as_label(), "tffa1");
        assert_eq!(DemoInstance::Tffa(7).label().as_label(), "tffa8");
        assert_eq!(DemoInstance::Duel(0).label().as_label(), "duel1");
        assert_eq!(DemoInstance::Duel(15).label().as_label(), "duel16");
        assert_eq!(DemoInstance::Tffa(3).viewer_match(), 3);
        assert_eq!(DemoInstance::Duel(2).viewer_match(), 2);
    }

    #[test]
    fn membership_comes_from_the_existing_tables() {
        let mut matches = [0u8; MAX_CLIENTS];
        matches[2] = 3;
        let mut court_of = [None; MAX_CLIENTS];
        court_of[4] = Some(2);
        assert_eq!(
            membership_of(5, InstanceMode::Off, &matches, &court_of),
            Some(DemoInstance::Main)
        );
        assert_eq!(
            membership_of(2, InstanceMode::Tffa, &matches, &court_of),
            Some(DemoInstance::Tffa(3))
        );
        assert_eq!(
            membership_of(0, InstanceMode::Tffa, &matches, &court_of),
            Some(DemoInstance::Tffa(0))
        );
        assert_eq!(
            membership_of(4, InstanceMode::Duel, &matches, &court_of),
            Some(DemoInstance::Duel(2))
        );
        // Benched/spectating duel clients have no court.
        assert_eq!(
            membership_of(5, InstanceMode::Duel, &matches, &court_of),
            None
        );
    }

    #[test]
    fn viewer_slot_is_the_highest_unassigned_slot_when_the_server_leaves_room() {
        // 8 configured slots (0..7): 31 is the highest the server never
        // assigns.
        assert_eq!(pick_viewer_slot(0, 8, &[]), MAX_CLIENTS as i32 - 1);
        assert_eq!(
            pick_viewer_slot(5, 8, &[occupied(0)]),
            MAX_CLIENTS as i32 - 1
        );
    }

    #[test]
    fn viewer_slot_is_the_highest_free_slot_when_all_are_assignable() {
        let slots = [occupied(0), free(1), occupied(2), free(3), free(4)];
        // Highest free slot wins (31 would be the top of a real 32-slot list).
        assert_eq!(pick_viewer_slot(0, MAX_CLIENTS, &slots), 4);
        let slots = [occupied(0), free(1)];
        assert_eq!(pick_viewer_slot(0, MAX_CLIENTS, &slots), 1);
    }

    #[test]
    fn viewer_slot_falls_back_to_the_subject_on_a_full_server() {
        let slots: Vec<ClientSlot> = (0..MAX_CLIENTS as i32).map(occupied).collect();
        assert_eq!(pick_viewer_slot(7, MAX_CLIENTS, &slots), 7);
    }

    #[test]
    fn identity_is_lost_only_when_a_stranger_holds_the_viewer_slot() {
        // Unassigned viewer slot: fine even when every real slot is busy.
        assert_eq!(identity_lost(8, 1, &[occupied(0), occupied(1)]), None);
        // Someone takes the spare viewer slot: the identity is compromised.
        assert_eq!(
            identity_lost(8, 1, &[occupied(0), occupied(1), occupied(8)]),
            Some(8)
        );
        // Self view (viewer == subject): the occupant is the followed player.
        assert_eq!(identity_lost(3, 3, &[occupied(3)]), None);
        // The subject moved away and a stranger now sits in the old slot.
        assert_eq!(identity_lost(3, 4, &[occupied(3)]), Some(3));
    }

    #[test]
    fn follow_flag_only_when_the_viewer_follows_someone_else() {
        assert!(follow_mode(1, 8));
        assert!(!follow_mode(3, 3));
    }

    #[test]
    fn scoreboard_probe_captures_only_its_own_scores_trap() {
        let mut probe = ScoreboardProbe::EMPTY;
        assert!(!probe.capture(3, b"scores 1 0 0 3 5 0 0 0 0 0 0 0 0 0 0 0 0\n"));
        probe.arm(3);
        // Another client's command is not ours.
        assert!(!probe.capture(4, b"scores 0 0 0\n"));
        // Another command to the same client is not ours either.
        assert!(!probe.capture(3, b"print hello\n"));
        // Our own trap is captured with the newline the engine would add.
        assert!(probe.capture(3, b"scores 1 0 0 3 5 0 0 0 0 0 0 0 0 0 0 0 0"));
        let mut probe = ScoreboardProbe::EMPTY;
        probe.arm(3);
        assert!(probe.capture(3, b"scores 0 0 0"));
        assert_eq!(
            probe.take().as_deref(),
            Some(&b"scores 0 0 0\n"[..]),
            "the demo command text ends with a newline"
        );
        assert!(probe.take().is_none(), "capture is taken once");
        assert!(!probe.capture(3, b"scores 0 0 0"), "disarmed after capture");
    }

    #[test]
    fn recorded_serverinfo_drops_the_taystjk_layout_key() {
        let info = b"\\sv_hostname\\srv\\taystJKinfo\\128\\g_gametype\\0";
        let stripped = crate::userinfo::without_key(info, b"taystJKinfo");
        assert_eq!(stripped, b"\\sv_hostname\\srv\\g_gametype\\0");
        assert_eq!(
            crate::userinfo::without_key(b"\\sv_hostname\\srv", b"taystJKinfo"),
            b"\\sv_hostname\\srv",
            "absent key leaves the string untouched"
        );
    }

    #[test]
    fn subject_decision_keeps_an_eligible_subject() {
        let m = membership(&[(0, DemoInstance::Tffa(0)), (1, DemoInstance::Tffa(0))]);
        let slots = [human(0), human(1)];
        assert_eq!(
            choose_subject(0, &slots, DemoInstance::Tffa(0), &m),
            SubjectDecision::Keep
        );
    }

    #[test]
    fn subject_decision_switches_when_the_subject_leaves() {
        // Slot 0 is gone (kicked/disconnected); slot 1 plays the same match.
        let m = membership(&[(1, DemoInstance::Tffa(2))]);
        let slots = [slot(0, false, false, false), human(1)];
        assert_eq!(
            choose_subject(0, &slots, DemoInstance::Tffa(2), &m),
            SubjectDecision::Switch {
                to: 1,
                reason: SubjectLoss::Left
            }
        );
    }

    #[test]
    fn subject_decision_switches_when_the_subject_spectates() {
        // Slot 0 joined spectators; slot 1 plays the same match.
        let m = membership(&[(0, DemoInstance::Duel(0)), (1, DemoInstance::Duel(0))]);
        let slots = [slot(0, true, true, true), human(1)];
        assert_eq!(
            choose_subject(0, &slots, DemoInstance::Duel(0), &m),
            SubjectDecision::Switch {
                to: 1,
                reason: SubjectLoss::Spectator
            }
        );
    }

    #[test]
    fn subject_decision_stays_inside_the_instance() {
        // Slot 1 plays another match; slot 2 is the lowest of this one.
        let m = membership(&[
            (1, DemoInstance::Tffa(1)),
            (2, DemoInstance::Tffa(0)),
            (3, DemoInstance::Tffa(0)),
        ]);
        let slots = [slot(0, false, false, false), human(1), human(2), human(3)];
        assert_eq!(
            choose_subject(0, &slots, DemoInstance::Tffa(0), &m),
            SubjectDecision::Switch {
                to: 2,
                reason: SubjectLoss::Left
            }
        );
    }

    #[test]
    fn subject_decision_switches_when_the_subject_moves_instance() {
        // Slot 0 is still playing but moved to match 1.
        let m = membership(&[(0, DemoInstance::Tffa(1)), (1, DemoInstance::Tffa(0))]);
        let slots = [human(0), human(1)];
        assert_eq!(
            choose_subject(0, &slots, DemoInstance::Tffa(0), &m),
            SubjectDecision::Switch {
                to: 1,
                reason: SubjectLoss::Moved
            }
        );
    }

    #[test]
    fn subject_decision_skips_bots() {
        // Phase 3 records humans only.
        let m = membership(&[(0, DemoInstance::Main), (1, DemoInstance::Main)]);
        let slots = [bot(0), human(1)];
        assert_eq!(
            choose_subject(0, &slots, DemoInstance::Main, &m),
            SubjectDecision::Switch {
                to: 1,
                reason: SubjectLoss::Moved
            }
        );
        assert_eq!(
            choose_subject(1, &slots, DemoInstance::Main, &m),
            SubjectDecision::Keep
        );
    }

    #[test]
    fn subject_decision_skips_a_connecting_client() {
        // CS_PRIMED has a gentity but is not in the world yet.
        let m = membership(&[(0, DemoInstance::Main), (1, DemoInstance::Main)]);
        let mut primed = slot(0, true, true, false);
        primed.in_world = false;
        let slots = [primed, human(1)];
        assert_eq!(
            choose_subject(0, &slots, DemoInstance::Main, &m),
            SubjectDecision::Switch {
                to: 1,
                reason: SubjectLoss::Left
            }
        );
    }

    #[test]
    fn subject_decision_skips_slots_without_a_gentity() {
        // A client still connecting has no gentity and no recordable ps yet.
        let m = membership(&[(2, DemoInstance::Main)]);
        let slots = [
            slot(0, false, false, false),
            slot(1, true, false, false),
            human(2),
        ];
        assert_eq!(
            choose_subject(0, &slots, DemoInstance::Main, &m),
            SubjectDecision::Switch {
                to: 2,
                reason: SubjectLoss::Left
            }
        );
        assert_eq!(
            choose_subject(0, &slots[..2], DemoInstance::Main, &m),
            SubjectDecision::Stop
        );
    }

    #[test]
    fn subject_decision_stops_only_when_the_instance_is_empty() {
        let m = membership(&[(1, DemoInstance::Duel(3))]);
        let slots = [slot(0, false, false, false), human(1)];
        assert_eq!(
            choose_subject(0, &slots, DemoInstance::Duel(2), &m),
            SubjectDecision::Stop
        );
    }

    #[test]
    fn chat_rule_is_the_copy_addressed_to_the_subject() {
        let subject = 2;
        // `G_Say` sends a per-client copy of every chat line: the subject's
        // copy of a server broadcast is the one the demo carries, exactly once.
        assert!(chat_command_for(subject, b"chat \"hello\"", subject));
        assert!(chat_command_for(
            subject,
            b"lchat \"hello\" \"loc\"",
            subject
        ));
        // A `-1` broadcast is still accepted.
        assert!(chat_command_for(-1, b"chat \"hello\"", subject));
        // Another client's copy is that client's, not this demo's.
        assert!(!chat_command_for(3, b"chat \"hello\"", subject));
        // A `tell` arrives as a `chat` addressed to its target, with the
        // sender's name in the `ESC [` marker: never recorded.
        assert!(!chat_command_for(
            subject,
            b"chat \"\x19[ChatTwo\x19]\x19: ^6psst\"",
            subject
        ));
        assert!(!chat_command_for(
            subject,
            b"lchat \"\x19[ChatTwo\x19]\x19: ^6psst\" \"loc\" \"6\" \"psst\"",
            subject
        ));
        // Never private tells or per-client prints.
        assert!(!chat_command_for(subject, b"tell \"psst\"", subject));
        assert!(!chat_command_for(-1, b"print hello", subject));
        assert!(!chat_command_for(subject, b"print hello", subject));
        assert!(!chat_command_for(subject, b"scores 1 0 0", subject));
    }

    #[test]
    fn chat_rule_records_broadcasts_but_never_team_chat() {
        // Owner decision (2026-11-12): broadcast only. The subject receives
        // both commands, but `tchat`/`ltchat` must not reach the demo.
        let subject = 2;
        assert!(chat_command_for(subject, b"chat \"all\"", subject));
        assert!(chat_command_for(subject, b"lchat \"all\" \"loc\"", subject));
        assert!(!chat_command_for(subject, b"tchat \"team\"", subject));
        assert!(!chat_command_for(subject, b"ltchat \"team\"", subject));
        assert!(!chat_command_for(-1, b"tchat \"team\"", subject));
    }

    #[test]
    fn max_file_size_guard_threshold_is_inclusive_and_optional() {
        // `0` (and a negative admin value) means no cap.
        assert!(!size_cap_reached(0, 0));
        assert!(!size_cap_reached(u64::MAX, -1));
        // The cap is inclusive and counted in MiB.
        assert!(!size_cap_reached(1024 * 1024 - 1, 1));
        assert!(size_cap_reached(1024 * 1024, 1));
        assert!(size_cap_reached(1024 * 1024 + 1, 1));
        assert!(!size_cap_reached(2 * 1024 * 1024 - 1, 2));
        assert!(size_cap_reached(128 * 1024 * 1024, 128));
    }

    #[test]
    fn segment_numbers_are_per_instance_and_reset_with_the_map() {
        let mut segments = Vec::new();
        assert_eq!(next_segment(&mut segments, DemoInstance::Tffa(0)), 1);
        assert_eq!(next_segment(&mut segments, DemoInstance::Tffa(0)), 2);
        assert_eq!(next_segment(&mut segments, DemoInstance::Tffa(1)), 1);
        assert_eq!(next_segment(&mut segments, DemoInstance::Tffa(0)), 3);
    }

    #[test]
    fn main_scores_command_passes_through_unchanged() {
        let text = b"scores 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0\n";
        assert_eq!(
            instance_scores_command(DemoInstance::Main, 0, text),
            text.to_vec()
        );
    }

    #[test]
    fn configstring_command_matches_the_engine_wording() {
        assert_eq!(configstring_command(10, b"2"), b"cs 10 \"2\"\n");
        assert_eq!(configstring_command(0, b""), b"cs 0 \"\"\n");
        // Raw info-string backslashes; the demos show no escaping.
        assert_eq!(
            configstring_command(1132, b"n\\viper\\model\\luke/default"),
            b"cs 1132 \"n\\viper\\model\\luke/default\"\n"
        );
        assert_eq!(configstring_command(7, b"a\"b"), b"cs 7 \"a\"b\"\n");
    }

    #[test]
    fn long_configstrings_use_the_engine_chunk_protocol() {
        let value = vec![b'x'; CONFIGSTRING_CHUNK];
        let text = configstring_command(42, &value);
        assert!(text.starts_with(b"bcs0 42 \""));
        assert!(text.ends_with(b"\"\n"));
        assert_eq!(text.iter().filter(|&&b| b == b'"').count(), 4);
        assert!(text.windows(4).any(|w| w == b"bcs2"));

        let value = vec![b'x'; CONFIGSTRING_CHUNK * 2 + 5];
        let text = configstring_command(1, &value);
        assert!(text.starts_with(b"bcs0 1 \""));
        assert!(text.windows(4).any(|w| w == b"bcs1"));
        assert!(text.windows(4).any(|w| w == b"bcs2"));
    }
}
