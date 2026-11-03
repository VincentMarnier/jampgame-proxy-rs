//! Players net status — `Proxy_Engine_Client.{hpp,cpp}` +
//! `Proxy_Engine_ClientCommand.cpp` (the D-002 "Players net status" row),
//! reworked per D-007: a model-based `cl_timeNudge` estimator reporting a
//! range, and a richer table.
//!
//! The per-usercmd feed runs from the retargeted `call SV_ClientThink`
//! (`crate::patch::CALL_SV_CLIENT_THINK` → `sv_client_think_stub`), which is
//! the D-002.1 call-site injection replacing the original proxy's
//! `SV_ExecuteClientMessage`/`SV_UserMove` whole-function rewrites.
//!
//! `sv_client_think_stub` derives the packet identity from
//! `packet_counter`, which the `SV_ExecuteClientMessage` entry wrapper bumps
//! once per usercmd message (`clc_move`/`clc_moveNoDelta`) — the same
//! per-`SV_UserMove` capture point the original proxy used (its pre-loop
//! `cmdIndex`), so one packet record is filled per usercmd message.
//!
//! Deliberate deviations (documented, R-022 / D-007):
//!
//! * the original gated the feed and the table's stats on `cl->ping >= 1` — a
//!   bot discriminator (`SV_CalcPings` writes ping 0 for `svFlags & SVF_BOT`)
//!   that also excluded real players whose computed ping is 0, i.e. every
//!   localhost/LAN player. Bots can never reach this feed anyway (they send no
//!   `clc_move`; their cmds arrive via the `BOTLIB_USER_COMMAND` syscall), so
//!   the feed runs for every client that reaches it. The printer identifies
//!   bots with the engine's own `gentity->r.svFlags & SVF_BOT` check and skips
//!   their rows (zero-ping localhost/LAN players included);
//! * **the magic `timeNudge` table is deleted** (D-007 decision 4): the old
//!   `Proxy_GetTimenudgeMagicOffset` 61-entry `sv_fps` table cannot capture
//!   per-client quantities (cmd rate, packet pacing) and is replaced by the
//!   model-based estimator in [`crate::netstatus_est`];
//! * `cl->ping` is no longer used for anything (decision 9): the table's `rtt`
//!   comes from `cl->frames[]` per-packet samples instead.
//!
//! The `proxy_sv_netStatusDebug` cvar of the planning session was dropped at
//! the user's request (2026-10-26): no estimator internals are exposed at
//! runtime. The estimator itself keeps its per-term structure (and the
//! plan-literal variant) because the unit tests validate them; the printed
//! table is the only surface.
//!
//! With `proxy_sv_enableNetStatus 0` the table and every estimator computation
//! are **completely inert** (user directive, 2026-10-26). The raw per-cmd
//! record feed is also what the per-client packet-rate limiter uses to derive
//! each client's FPS (`crate::pps`), so it stays active while
//! `proxy_sv_enableClientPpsLimit` or `proxy_sv_clientPpsLog` is on. The same
//! feed backs the optional `cl_timeNudge` neutralisation (D-015): while
//! `proxy_sv_voidTimeNudge` or `proxy_sv_voidTimeNudgeLog` is on, the packet
//! start refreshes the estimator verdict (every 500 ms) and the stub rewrites
//! the cmd's `serverTime` (see `crate::timenudge`). With every one of those
//! cvars off the feed does nothing beyond one cvar mirror read per usercmd
//! message.
//!
//! The estimator is always fed the client's **raw** stamp, even while the
//! cmd rewrite is active: feeding the adjusted value would make the estimator
//! converge to "no nudge" and switch the correction off.
//!
//! The table's `packets/s` cell is rendered from the limiter's own accounting
//! (`crate::pps::status`), not from the record ring: the ring only counts
//! packets that carried new usercmds, so it cannot see a client flooding
//! empty/duplicate packets. The same completed window and the same
//! `over_limit` predicate drive the `proxy_sv_clientPpsLog` line, so the table
//! and the log always agree. An over-limit cell is red: with
//! `proxy_sv_enableClientPpsLimit` on it shows the effective rate (the observed
//! rate capped at the limit, i.e. what the limiter lets through); with the
//! limiter off it appends the limit it would enforce
//! (`^1<actual>^7/<limit>`, actual red, limit default colour).

use core::ffi::{c_char, c_int};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::engine;
use crate::hooks::engine_sv::{
    client_num_from_addr, clients_base, cstr_bytes, read_i32, read_usize, svs_time, write_i32,
};
use crate::netstatus_est::{self, Estimate};
use crate::sdk::{
    CS_CONNECTED, CS_ZOMBIE, OFFSET_CLIENT_FRAMES, OFFSET_CLIENT_LAST_USERCMD, OFFSET_CLIENT_NAME,
    OFFSET_CLIENT_PING, OFFSET_CLIENT_RATE, OFFSET_CLIENT_SNAPSHOT_MESSAGE_SENT,
    OFFSET_CLIENT_SNAPSHOT_MSEC, OFFSET_CLIENT_STATE, PACKET_MASK, SIZEOF_CLIENT_SNAPSHOT_T,
    SIZEOF_CLIENT_T, Usercmd,
};
use crate::state::{
    self, CVAR_ANTI_MACRO, CVAR_ANTI_MACRO_LOG, CVAR_CLIENT_PPS_LOG, CVAR_ENABLE_CLIENT_PPS_LIMIT,
    CVAR_ENABLE_NET_STATUS, CVAR_VOID_TIME_NUDGE, CVAR_VOID_TIME_NUDGE_LOG,
};
use crate::syscall;
use crate::timenudge;

/// Whether the shared per-usercmd record feed must run for `s`: the netStatus
/// table needs it, the packet-rate limiter derives each client's FPS from it,
/// `proxy_sv_clientPpsLog` uses that same FPS-derived limit, and
/// `proxy_sv_voidTimeNudge`/`proxy_sv_voidTimeNudgeLog` (D-015) need the
/// estimator's `cl_timeNudge` range.
fn feed_enabled_in(s: &state::ProxyState) -> bool {
    s.cvars[CVAR_ENABLE_NET_STATUS].integer != 0
        || s.cvars[CVAR_ENABLE_CLIENT_PPS_LIMIT].integer != 0
        || s.cvars[CVAR_CLIENT_PPS_LOG].integer != 0
        || s.cvars[CVAR_VOID_TIME_NUDGE].integer != 0
        || s.cvars[CVAR_VOID_TIME_NUDGE_LOG].integer != 0
}

fn feed_enabled() -> bool {
    state::with_state(|s| feed_enabled_in(s))
}

/// `server.cvars.sv_maxclients->integer` (cvar slot 0x8273ea4). Returns 0 when
/// the memory layer is not initialised yet (null slot) instead of faulting.
fn sv_maxclients() -> c_int {
    // SAFETY: the slot holds a cvar_t* (R-018 verified).
    let ptr = unsafe { *(crate::engine::cvar_slots::SV_MAXCLIENTS as *const usize) };
    if ptr == 0 {
        return 0;
    }
    // SAFETY: ptr is a live cvar_t*.
    unsafe { engine::cvar_integer(ptr) }
}

/// `server.cvars.sv_fps->integer` (cvar slot 0x8273e84). Returns 0 when the
/// memory layer is not initialised yet (null slot) instead of faulting.
fn sv_fps() -> c_int {
    // SAFETY: cvar slot validated and dereferenced at GAME_INIT (R-018).
    let ptr = unsafe { *(crate::engine::cvar_slots::SV_FPS as *const usize) };
    if ptr == 0 {
        return 0;
    }
    // SAFETY: the slot holds a cvar_t* (R-018 verified).
    unsafe { engine::cvar_integer(ptr) }
}

/// Whether the client is a bot — the same discriminator `SV_CalcPings` uses
/// (`cl->gentity != 0 && gentity->r.svFlags & SVF_BOT`, binary-verified at
/// `0x8057264-0x805727d`). Replaces the original's `ping >= 1` check, which
/// conflated bots with real players whose computed ping is 0 (localhost/LAN),
/// and excludes bots from the printed table.
///
/// # Safety
///
/// `cl` must be a valid `client_t*`.
unsafe fn is_bot(cl: usize) -> bool {
    // SAFETY: cl is a valid client_t; gentity is a sharedEntity_t*.
    let gentity = unsafe { read_usize(cl + crate::sdk::OFFSET_CLIENT_GENTITY) };
    if gentity == 0 {
        // No gentity (connecting/zombie) — not a bot in the SVF sense.
        return false;
    }
    // SAFETY: gentity is a valid sharedEntity_t.
    unsafe {
        read_i32(gentity + crate::sdk::OFFSET_SHARED_SVFLAGS) & crate::sdk::SVF_BOT as i32 != 0
    }
}

// ---------------------------------------------------------------------------
// Measurement plumbing (D-007 step 2)
// ---------------------------------------------------------------------------

/// `cl->frames[message_acknowledge & PACKET_MASK].messageSent` of the snapshot
/// message the packet acks — the RTT's send endpoint (D-007).
///
/// Returns 0 when the slot carries no usable sample (`messageSent` empty, a
/// wrapped/stale slot, or an out-of-window message).
///
/// Validity: `SV_UserMove` writes `frames[ack & PACKET_MASK].messageAcked =
/// svs.time` *before* the `SV_ClientThink` loop (sv_client.cpp:1757;
/// binary-verified at 0x804e7c8-0x804e7dd: `messageAcknowledge & 0x1f`,
/// `imul $0xc58`, store at `+0x21574`), so this packet's ack sample is already
/// in place when the feed runs. `messageSent` still belongs to the acked
/// message only while the slot has not been reused by a newer one, hence the
/// freshness bound.
///
/// # Safety
///
/// `cl` must be a valid `client_t*`.
unsafe fn acked_message_sent(cl: usize, message_acknowledge: i32, now: i32) -> i32 {
    if message_acknowledge < 0 {
        return 0;
    }
    let frame = cl
        + OFFSET_CLIENT_FRAMES
        + (message_acknowledge as usize & PACKET_MASK) * SIZEOF_CLIENT_SNAPSHOT_T;
    // SAFETY: frame points into cl->frames[PACKET_BACKUP].
    let sent = unsafe { read_i32(frame + OFFSET_CLIENT_SNAPSHOT_MESSAGE_SENT) };
    if sent <= 0 {
        return 0;
    }
    let age = now.wrapping_sub(sent);
    if !(0..=2000).contains(&age) {
        // stale/wrapped slot or an svs.time discontinuity
        return 0;
    }
    sent
}

/// `cl->frames[]` message spacing (`I_msg`) over the last `PACKET_BACKUP`
/// messages — the server's own send pacing, as observed traffic (decision 1's
/// "never from `cl->snapshotMsec`" rule). Returns 0 when fewer than two
/// in-window samples exist.
///
/// # Safety
///
/// `cl` must be a valid `client_t*`.
unsafe fn frames_msg_interval(cl: usize, now: i32) -> f32 {
    let mut sends = [0i32; crate::sdk::PACKET_BACKUP];
    let mut n = 0usize;
    for slot in 0..crate::sdk::PACKET_BACKUP {
        // SAFETY: slot indexes cl->frames[PACKET_BACKUP].
        let sent = unsafe {
            read_i32(
                cl + OFFSET_CLIENT_FRAMES
                    + slot * SIZEOF_CLIENT_SNAPSHOT_T
                    + OFFSET_CLIENT_SNAPSHOT_MESSAGE_SENT,
            )
        };
        let age = now.wrapping_sub(sent);
        if sent > 0 && (0..=netstatus_est::WINDOW_MS).contains(&age) {
            sends[n] = sent;
            n += 1;
        }
    }
    if n < 2 {
        return 0.0;
    }
    let slice = &mut sends[..n];
    slice.sort_unstable();
    // consecutive duplicates are the same message acked by several packets
    let mut first = 0i32;
    let mut last = 0i32;
    let mut unique = 0usize;
    for &sent in slice.iter() {
        if unique == 0 {
            first = sent;
            last = sent;
            unique = 1;
        } else if sent != last {
            last = sent;
            unique += 1;
        }
    }
    if unique < 2 {
        return 0.0;
    }
    (last - first) as f32 / (unique - 1) as f32
}

/// `mark_packet_start` — called once per usercmd client message by the
/// `SV_ExecuteClientMessage` entry wrapper, reproducing the original's
/// pre-loop `cmdIndex` packet-identity capture. Commits the previous packet
/// record and opens the next one with this frame's `svs.time` (`A_k`) and the
/// acked snapshot's `messageSent`.
///
/// `message_acknowledge` is the value parsed from *this* message's header by
/// the wrapper — `cl->messageAcknowledge` in memory is still the previous
/// message's ack at this point (`SV_ExecuteClientMessage` writes it before
/// calling `SV_UserMove`), so the pairing must use the parsed one.
///
/// With both `proxy_sv_enableNetStatus` and
/// `proxy_sv_enableClientPpsLimit` at `0` the feed is inert: this returns
/// before touching the engine or the window, and the off→on edge resets every
/// window through [`FEED_WAS_OFF`].
///
/// # Safety
///
/// `cl` must be a valid `client_t*`.
pub unsafe fn mark_packet_start(cl: usize, message_acknowledge: i32) {
    let client_num = client_num_from_addr(cl);
    if client_num >= crate::sdk::MAX_CLIENTS {
        return;
    }
    if !feed_enabled() {
        FEED_WAS_OFF.store(true, Ordering::Relaxed);
        return;
    }
    // SAFETY: engine memory layer initialised.
    let now = unsafe { svs_time() };
    // SAFETY: cl is a valid client_t.
    let message_sent = unsafe { acked_message_sent(cl, message_acknowledge, now) };
    // SAFETY: cl is a valid client_t.
    let snapshot_msec = unsafe { read_i32(cl + OFFSET_CLIENT_SNAPSHOT_MSEC) };
    let fps = sv_fps();
    let void_enabled = timenudge::enabled();
    let void_log = timenudge::logging();
    let due = state::with_state(|s| {
        let c = &mut s.clients[client_num];
        c.packet_counter = c.packet_counter.wrapping_add(1);
        // reset rules (D-007 step 4): sv_fps or snapshotMsec change
        // invalidates the window
        if c.netstatus.sv_fps != fps || c.netstatus.snapshot_msec != snapshot_msec {
            if c.netstatus.sv_fps != 0 || c.netstatus.snapshot_msec != 0 {
                c.netstatus.reset();
                // The new window does not describe the current clock yet:
                // ramp the correction away instead of keeping a stale shift.
                c.nudge_void.stop();
            }
            c.netstatus.sv_fps = fps;
            c.netstatus.snapshot_msec = snapshot_msec;
        }
        c.netstatus
            .mark_packet_start(now, message_sent, message_acknowledge);
        (void_enabled || void_log) && c.nudge_void.due(now)
    });

    // D-015: refresh the timeNudge-neutralisation verdict at most every
    // `timenudge::REFRESH_MS`. The estimator itself must not run per command:
    // it copies and sorts the window.
    if due {
        // SAFETY: cl is a valid client_t.
        let msg_interval = unsafe { frames_msg_interval(cl, now) };
        let est = compute(client_num, now, msg_interval);
        if let Some(transition) =
            timenudge::update(client_num, now, est.as_ref(), void_enabled, void_log)
        {
            // SAFETY: cl is a valid client_t.
            unsafe { timenudge::print_transition(cl, transition) };
        }
    }
}

/// Per-usercmd feed: accumulate the cmd into the open packet record.
pub fn feed_cmd(client_num: usize, server_time: i32) {
    state::with_state(|s| s.clients[client_num].netstatus.feed_cmd(server_time));
}

/// Drop one client's estimator window (connect/disconnect, D-007 step 4).
pub fn reset_client(client_num: usize) {
    state::with_state(|s| {
        if let Some(client) = s.clients.get_mut(client_num) {
            client.netstatus.reset();
            client.nudge_void.clear();
        }
    });
}

/// Drop every estimator window (the shared feed off -> on). The reset is
/// deferred to each client's next usercmd packet (`reset_pending`) so the hot
/// path never clears the 512-record ring mid-packet; the neutralisation state
/// is cleared right away because no shift was applied while the feed was off.
pub fn reset_all() {
    state::with_state(|s| {
        for client in s.clients.iter_mut() {
            client.netstatus.reset_pending = true;
            client.nudge_void.clear();
        }
    });
}

/// Set when the shared feed was seen off, so the next enabled feed
/// invalidates every window (D-007 step 4).
static FEED_WAS_OFF: AtomicBool = AtomicBool::new(false);

/// `sv_client_think_stub` — the retarget of `call SV_ClientThink` inside the
/// pristine `SV_UserMove` (0x804e891). Runs the pristine `SV_ClientThink`,
/// then the per-usercmd netStatus feed (D-002.1 technique 3). While
/// `proxy_sv_voidTimeNudge` is on, the cmd's `serverTime` is shifted by the
/// client's estimated `cl_timeNudge` first (D-015, `crate::timenudge`), so the
/// game processes the move on the neutral clock.
///
/// The estimator is fed the client's **raw** stamp: correcting the value the
/// estimator sees would make it converge to "no nudge" and switch itself off.
///
/// # Safety
///
/// Entered from the engine's usercmd loop with `(client_t*, usercmd_t*)`.
pub unsafe extern "C" fn sv_client_think_stub(cl: usize, cmd: *mut Usercmd) {
    let client_num = client_num_from_addr(cl);
    // A stale `client_t*` (e.g. after disconnect) can yield an out-of-range
    // slot: fail closed instead of panicking (= abort) in the move-packet hot
    // path.
    if client_num >= crate::sdk::MAX_CLIENTS {
        // Still run the pristine call: this stub replaces it.
        // SAFETY: engine function at the fixed table; args from the engine loop.
        unsafe { engine::sv_client_think(cl, cmd as usize) };
        return;
    }

    let voiding = timenudge::enabled();
    let (feed, offset, macro_events, macro_log) = state::with_state(|s| {
        let feed = feed_enabled_in(s);
        // One ramp step per usercmd; the target is refreshed at packet start.
        let offset = if feed && voiding {
            s.clients[client_num].nudge_void.step()
        } else {
            0
        };
        // Anti-macro guard: observes the raw command every time it is on, so
        // `proxy_sv_antiMacroLog` reports what the guard would freeze; only
        // `proxy_sv_antiMacro` rewrites the command. The engine's duplicate
        // filter above this call site means each command is seen once.
        let anti_macro = s.cvars[CVAR_ANTI_MACRO].integer != 0;
        let log_level = s.cvars[CVAR_ANTI_MACRO_LOG].integer;
        let events = if anti_macro || log_level != 0 {
            // SAFETY: cmd is the engine's usercmd buffer for this call.
            crate::macroguard::process(
                &mut s.clients[client_num].macro_guard,
                unsafe { &mut *cmd },
                anti_macro,
                log_level,
            )
        } else {
            // Inert while off; drop any state a previous enable left behind.
            let guard = &mut s.clients[client_num].macro_guard;
            if guard.is_active() {
                *guard = crate::macroguard::MacroState::default();
            }
            crate::macroguard::Events::default()
        };
        (feed, offset, events, log_level)
    });
    let offset = if !feed {
        FEED_WAS_OFF.store(true, Ordering::Relaxed);
        0
    } else if FEED_WAS_OFF.swap(false, Ordering::Relaxed) {
        reset_all();
        0
    } else {
        offset
    };

    // SAFETY: cmd is the engine's usercmd buffer.
    let original = unsafe { (*cmd).server_time };
    if offset != 0 {
        // SAFETY: cmd is the engine's usercmd buffer, valid for this call.
        unsafe { (*cmd).server_time = original.wrapping_add(offset) };
    }

    // SAFETY: engine function at the fixed table; args from the engine loop.
    unsafe { engine::sv_client_think(cl, cmd as usize) };

    if offset != 0 {
        // Restore the engine's buffer and, more importantly, the stamp the
        // pristine `SV_UserMove` compares the next packet's `cl_packetdup`
        // copies against. The game already read the adjusted value through
        // `trap_GetUsercmd` inside `SV_ClientThink`.
        // SAFETY: cmd and cl are valid as above.
        unsafe {
            (*cmd).server_time = original;
            write_i32(cl + OFFSET_CLIENT_LAST_USERCMD, original);
        }
    }

    if feed {
        feed_cmd(client_num, original);
    }

    if macro_events.any() {
        log_macro_events(cl, &macro_events, macro_log);
    }
}

/// Print anti-macro transitions (level ≥ 1) and the once-per-second statistics
/// (level ≥ 2) after the state lock is released — the engine print trap must
/// not run under the proxy mutex. Server console only: the player is never
/// told, so a script cannot learn the thresholds.
fn log_macro_events(cl: usize, events: &crate::macroguard::Events, level: i32) {
    if level <= 0 {
        return;
    }
    // SAFETY: cl is the client_t* whose command was just processed.
    let label = unsafe { crate::hooks::engine_sv::client_label(cl) };
    for (ch, started) in events.started.iter().enumerate() {
        if *started {
            print_macro(&label, crate::macroguard::CHANNEL_NAMES[ch], "locked");
        }
    }
    for (ch, released) in events.released.iter().enumerate() {
        if *released {
            print_macro(&label, crate::macroguard::CHANNEL_NAMES[ch], "released");
        }
    }
    if let Some(debug) = events.debug {
        for (ch, stats) in debug.channels.iter().enumerate() {
            let Some(stats) = stats else { continue };
            print_macro_debug(
                &label,
                debug.cmds,
                crate::macroguard::CHANNEL_NAMES[ch],
                stats,
            );
        }
    }
}

fn print_macro(label: &str, channel: &str, action: &str) {
    print_macro_line(format!(
        "jampgame-proxy-rs: antiMacro {label} {channel} {action}\n"
    ));
}

/// `swing` is in `ANGLE2SHORT` units (360°/65536); `holds` are command counts.
fn print_macro_debug(label: &str, cmds: u32, channel: &str, stats: &crate::macroguard::Stats) {
    let swing_deg = stats.swing * 360 / 65536;
    print_macro_line(format!(
        "jampgame-proxy-rs: antiMacro debug {label} cmds={cmds} {channel} \
         rev={} mean={}ms/{}cmd holds={}..{} swing={}deg locked={}\n",
        stats.reversals,
        stats.mean_ms,
        stats.mean_cmds,
        stats.min_hold_cmds,
        stats.max_hold_cmds,
        swing_deg,
        u8::from(stats.locked),
    ));
}

fn print_macro_line(line: String) {
    match std::ffi::CString::new(line) {
        // SAFETY: engine syscall pointer registered by dllEntry; text is
        // NUL-terminated and outlives the call.
        Ok(text) => unsafe { crate::syscall::print(&text) },
        Err(err) => eprintln!(
            "----- proxy-rs: antiMacro log line contained a NUL byte: {}",
            String::from_utf8_lossy(&err.into_vec())
        ),
    }
}

// ---------------------------------------------------------------------------
// Estimator computation (D-007 step 3)
// ---------------------------------------------------------------------------

/// Compute the estimate for one client over its window, applying a detected
/// step reset. `msg_interval` is the `cl->frames[]`-derived message spacing
/// (0 = derive it from the window instead).
fn compute(client_num: usize, now: i32, msg_interval: f32) -> Option<Estimate> {
    let frame_msec = {
        let fps = sv_fps();
        if fps > 0 { 1000.0 / fps as f32 } else { 0.0 }
    };
    state::with_state(|s| {
        let ns = &mut s.clients[client_num].netstatus;
        let records: Vec<_> = ns.records().copied().collect();
        let est = netstatus_est::estimate(
            &records,
            now,
            frame_msec,
            Some(msg_interval).filter(|v| *v > 0.0),
        )?;
        if est.step {
            // keep the post-step half so the new value shows within ~2 s
            ns.keep_last(est.samples / 2);
        }
        Some(est)
    })
}

/// fps/pps over the 1 s window (decision 13).
fn fps_pps_of(client_num: usize) -> (i32, i32) {
    state::with_state(|s| {
        let records: Vec<_> = s.clients[client_num].netstatus.records().copied().collect();
        netstatus_est::fps_pps(&records)
    })
}

// ---------------------------------------------------------------------------
// Proxy_Engine_ClientCommand_NetStatus
// ---------------------------------------------------------------------------

/// Width of the `packets/s` column in the printed table (`packets/s` is
/// exactly nine characters). An over-limit client shows the limit after a `/`
/// only while the limiter is off; colour codes are not counted, so the column
/// stays aligned.
const PPS_COL_WIDTH: usize = 9;

/// The `packets/s` cell for one client: the shared limiter counter
/// (`crate::pps::`) as the rate actually in effect. With the limiter on, an
/// over-limit client shows the rate the limiter lets through (the observed
/// rate capped at the limit — e.g. 200 pps observed, limit 100 → `100`) and is
/// red (`^1`, the next cell restores the colour). With the limiter off there is
/// no effective rate to show, so the red actual rate is followed by the limit
/// it would enforce, in the default colour (`^7` before it).
fn pps_cell(status: Option<crate::pps::PpsSample>, fallback_pps: i32, enforcing: bool) -> String {
    let Some(sample) = status else {
        // Before the first completed window the record ring's cmd-carrying
        // packet count is the best available value (never red).
        return format!("{fallback_pps:>PPS_COL_WIDTH$}");
    };
    if !sample.over_limit() {
        return format!("{:>PPS_COL_WIDTH$}", sample.rate);
    }
    if enforcing {
        // `limit > 0` here: `over_limit` is false for a fail-open limit.
        let effective = sample.rate.min(sample.limit as u32);
        format!("^1{effective:>PPS_COL_WIDTH$}^7")
    } else {
        let actual = sample.rate.to_string();
        let limit = sample.limit.to_string();
        // `actual/limit` is right-aligned in the column (the digits of
        // `actual` carry the padding, not the colour codes).
        let pad = PPS_COL_WIDTH
            .saturating_sub(limit.len() + 1)
            .max(actual.len());
        format!("^1{actual:>pad$}^7/{limit}")
    }
}

/// Width of the `timeNudge` column in the printed table (the same 13 as the
/// header/rule; the `[lo..hi]` cell can be one character wider, as before).
const TN_COL_WIDTH: usize = 13;

/// The `timeNudge` cell for one client (D-007 diagnostic, D-015 colour rules).
///
/// Without `proxy_sv_voidTimeNudge` the cell reports the estimated range:
///
/// * red (`^1`) when the whole range is outside the release client's legal
///   `[-30, 30]` — `hi < -30` or `lo > 30`: the player is outside the range the
///   engine's own clamp can produce;
/// * yellow (`^3`) when 0 is not in the range but the range is not entirely
///   outside the legal clamp: the player is confidently using a non-zero
///   setting;
/// * white when 0 is in the range (consistent with the default setting).
///
/// With the feature active the column reports the value the server enforces:
/// `0` for every client, red on the clients whose range excludes 0 (the
/// timenudges being voided). A stale/filling window has no estimate, so it
/// keeps the `-` placeholder (white).
///
/// Colour codes are not counted in [`TN_COL_WIDTH`], so the coloured cells stay
/// aligned; `^7` restores the default colour for the following columns.
fn tn_cell(est: Option<Estimate>, voiding: bool) -> String {
    let Some(est) = est else {
        return format!("{:>TN_COL_WIDTH$}", "-");
    };
    if voiding {
        let zero = "0";
        if est.lo > 0.0 || est.hi < 0.0 {
            return format!("^1{zero:>TN_COL_WIDTH$}^7");
        }
        return format!("{zero:>TN_COL_WIDTH$}");
    }
    let range = format!("[{:.1}..{:.1}]", est.lo, est.hi);
    let legal = timenudge::MAX_SHIFT_MS as f32;
    if est.hi < -legal || est.lo > legal {
        format!("^1{range:>TN_COL_WIDTH$}^7")
    } else if est.lo > 0.0 || est.hi < 0.0 {
        format!("^3{range:>TN_COL_WIDTH$}^7")
    } else {
        format!("{range:>TN_COL_WIDTH$}")
    }
}

/// `Proxy_Engine_ClientCommand_NetStatus` (`Proxy_Engine_ClientCommand.cpp:
/// 13-96`): print the per-player net status table to `client_num` (rate-limited
/// to once per 500 ms of game time). D-007 columns:
/// `ping rtt jit fps packets/s timeNudge snaps rate name`.
///
/// The `packets/s` cell renders the shared per-client packet accounting in
/// `crate::pps` (the limiter's own counter, which sees every sequenced packet
/// including ones with no new usercmd). An over-limit client is red; with the
/// limiter on the value is the effective rate (observed capped at the limit),
/// with the limiter off the limit follows as `^1<actual>^7/<limit>`.
///
/// # Safety
///
/// Called from the proxy's own `vmMain` command handler (no engine frame on
/// the stack); `client_num` must be a connected client.
pub fn client_command_net_status(client_num: c_int) {
    // SAFETY: engine memory layer initialised.
    let svs_time = unsafe { svs_time() };
    let last = state::with_state(|s| s.clients[client_num as usize].last_time_net_status);
    if last + 500 > svs_time {
        return;
    }

    let maxclients = sv_maxclients().clamp(0, crate::sdk::MAX_CLIENTS as c_int);
    let fps_cvar = sv_fps();
    // The `timeNudge` column reports the enforced value while D-015 is active;
    // latch it once so one table cannot mix both renditions.
    let voiding = timenudge::enabled();
    // SAFETY: engine memory layer initialised.
    let clients = unsafe { clients_base() };

    let mut status: Vec<u8> = Vec::new();
    // header + rule, same widths as the rows below (4/6/5/4/9/13/4/6)
    status.extend_from_slice(
        format!(
            "{:>4} {:>6} {:>5} {:>4} {:>9} {:>13} {:>4} {:>6} {}\n",
            "ping", "rtt", "jit", "fps", "packets/s", "timeNudge", "snaps", "rate", "name"
        )
        .as_bytes(),
    );
    status.extend_from_slice(
        b"---- ------ ----- ---- --------- ------------- ----- ------ ---------------\n",
    );

    for i in 0..maxclients {
        let cl = clients + i as usize * SIZEOF_CLIENT_T;
        // SAFETY: cl is a valid client_t.
        let state = unsafe { read_i32(cl + OFFSET_CLIENT_STATE) };
        if state == 0 {
            continue;
        }
        // Exclude bots from the table (engine's own SVF_BOT discriminator).
        // SAFETY: cl is a valid client_t.
        if unsafe { is_bot(cl) } {
            continue;
        }
        let ping_str = if state == CS_CONNECTED {
            "CON ".to_string()
        } else if state == CS_ZOMBIE {
            "ZMB ".to_string()
        } else {
            // SAFETY: cl is a valid client_t.
            let ping = unsafe { read_i32(cl + OFFSET_CLIENT_PING) };
            let ping = if ping < 9999 { ping } else { 9999 };
            format!("{ping:4}")
        };

        // SAFETY: cl is a valid client_t.
        let msg_interval = unsafe { frames_msg_interval(cl, svs_time) };
        let est = compute(i as usize, svs_time, msg_interval);
        let (fps, pps) = fps_pps_of(i as usize);
        // The limiter's own accounting, shared with the `clientPpsLog` line:
        // it sees every sequenced packet (empty/duplicate ones included), unlike
        // the record ring above, which only counts packets carrying new cmds.
        let pps_cell = pps_cell(
            crate::pps::status(i as usize, svs_time),
            pps,
            crate::pps::enforcing(),
        );
        let (tn, rtt, jit) = match est {
            Some(est) => (
                tn_cell(Some(est), voiding),
                format!("{:.1}", est.rtt_mean),
                format!("{:.1}", est.rtt_dev),
            ),
            None => (tn_cell(None, voiding), "-".to_string(), "-".to_string()),
        };

        // SAFETY: cl is a valid client_t.
        let snapshot_msec = unsafe { read_i32(cl + OFFSET_CLIENT_SNAPSHOT_MSEC) };
        let snaps = if snapshot_msec > 0 && 1000 / snapshot_msec > fps_cvar {
            fps_cvar
        } else if snapshot_msec > 0 {
            1000 / snapshot_msec
        } else {
            0
        };

        // SAFETY: cl is a valid client_t.
        let rate = unsafe { read_i32(cl + OFFSET_CLIENT_RATE) };
        // SAFETY: cl is a valid client_t; name is NUL-terminated.
        // Escape `"`, `\` and newlines so a hostile name cannot break the
        // `print "..."` reliable-command framing.
        let name = cstr_bytes((cl + OFFSET_CLIENT_NAME) as *const c_char);
        let mut clean_name: Vec<u8> = Vec::with_capacity(name.len());
        for &b in name {
            match b {
                b'"' => clean_name.push(b'\''),
                b'\n' | b'\r' => clean_name.push(b' '),
                b'\\' => {
                    clean_name.push(b'\\');
                    clean_name.push(b'\\');
                }
                _ => clean_name.push(b),
            }
        }

        status.extend_from_slice(
            format!("{ping_str} {rtt:>6} {jit:>5} {fps:4} {pps_cell} {tn} {snaps:5} {rate:6} ")
                .as_bytes(),
        );
        status.extend_from_slice(&clean_name);
        status.extend_from_slice(b"^7\n");
    }

    state::with_state(|s| s.clients[client_num as usize].last_time_net_status = svs_time);

    // Send in ≤1012-byte chunks (the proxy's `va("print \"%s", buffer)` loop).
    let status_len = status.len();
    let mut progress = 0usize;
    while progress < status_len {
        let chunk_len = (status_len - progress).min(1011);
        let mut chunk = status[progress..progress + chunk_len].to_vec();
        if chunk.len() < 1011 {
            chunk.push(b'\n');
        }
        progress += chunk.len();
        let mut cmd = Vec::with_capacity(chunk.len() + 9);
        cmd.extend_from_slice(b"print \"");
        cmd.extend_from_slice(&chunk);
        // SAFETY: no interior NUL (names are filtered at connect time).
        let mut full = cmd;
        full.push(0);
        let cstr = unsafe { core::ffi::CStr::from_bytes_with_nul_unchecked(&full) };
        // SAFETY: engine syscall registered.
        unsafe { syscall::send_server_command(client_num, cstr) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pps::PpsSample;

    fn sample(rate: u32, limit: i32, dropped: u32) -> PpsSample {
        PpsSample {
            rate,
            window_ms: 1000,
            observed: rate,
            dropped,
            fps: 60,
            limit,
        }
    }

    #[test]
    fn pps_cell_is_nine_wide_and_plain_inside_the_limit() {
        assert_eq!(pps_cell(Some(sample(60, 62, 0)), 60, false), "       60");
        assert_eq!(pps_cell(Some(sample(60, 62, 0)), 60, true), "       60");
        assert_eq!(pps_cell(None, 30, false), "       30");
    }

    #[test]
    fn pps_cell_is_red_only_and_shows_the_effective_rate_while_enforcing() {
        // Example: 200 pps observed, the limit is 100 -> the limiter lets 100
        // through, and that is what the table shows (red).
        assert_eq!(
            pps_cell(Some(sample(200, 100, 100)), 0, true),
            "^1      100^7"
        );
        // Over the limit: the capped rate, not the raw one.
        assert_eq!(
            pps_cell(Some(sample(100, 73, 27)), 0, true),
            "^1       73^7"
        );
        // Limiter off: red actual, default-colour limit.
        assert_eq!(
            pps_cell(Some(sample(100, 73, 27)), 0, false),
            "^1   100^7/73"
        );
        // The red flag also comes from drops on a just-completed window; the
        // effective rate is still the observed one (nothing above the limit).
        assert_eq!(pps_cell(Some(sample(73, 73, 1)), 0, true), "^1       73^7");
        assert_eq!(pps_cell(Some(sample(73, 73, 1)), 0, false), "^1    73^7/73");
    }

    #[test]
    fn pps_cell_never_truncates_a_wide_limit() {
        assert_eq!(
            pps_cell(Some(sample(200_000, 100_000, 0)), 0, false),
            "^1200000^7/100000"
        );
    }

    fn estimate(lo: f32, hi: f32) -> Estimate {
        Estimate {
            samples: 10,
            messages: 10,
            cmd_interval: 16.0,
            lag: 0.0,
            ackdelay: 0.0,
            clock_gap: 0.0,
            rtt_mean: 0.0,
            rtt_dev: 0.0,
            tn: (lo + hi) / 2.0,
            tn_plan: 0.0,
            raw_mean: 0.0,
            se: 0.0,
            lo,
            hi,
            step: false,
        }
    }

    #[test]
    fn tn_cell_is_white_when_zero_is_in_the_range() {
        assert_eq!(tn_cell(Some(estimate(-2.0, 3.0)), false), "  [-2.0..3.0]");
        // A range that exceeds the legal clamp on both sides still contains 0:
        // the client could be at the default setting, so it stays white.
        assert_eq!(tn_cell(Some(estimate(-40.0, 40.0)), false), "[-40.0..40.0]");
    }

    #[test]
    fn tn_cell_is_yellow_when_zero_is_outside_a_legal_range() {
        assert_eq!(
            tn_cell(Some(estimate(-17.9, -10.5)), false),
            "^3[-17.9..-10.5]^7"
        );
        // On the clamp boundary the reading is still legal (`hi == -30`).
        assert_eq!(
            tn_cell(Some(estimate(-30.0, -25.0)), false),
            "^3[-30.0..-25.0]^7"
        );
    }

    #[test]
    fn tn_cell_is_red_when_the_whole_range_is_outside_the_clamp() {
        assert_eq!(
            tn_cell(Some(estimate(-40.0, -33.0)), false),
            "^1[-40.0..-33.0]^7"
        );
        assert_eq!(
            tn_cell(Some(estimate(33.0, 40.0)), false),
            "^1 [33.0..40.0]^7"
        );
        // `hi < -30` / `lo > 30` are strict: -30 itself is not red.
        assert_eq!(
            tn_cell(Some(estimate(-40.0, -30.0)), false),
            "^3[-40.0..-30.0]^7"
        );
    }

    #[test]
    fn tn_cell_reports_zero_while_the_feature_voids() {
        // 0 is in the range: the neutral setting, nothing to void.
        assert_eq!(tn_cell(Some(estimate(-2.0, 3.0)), true), "            0");
        // The range excludes 0: this timenudge is being voided (red).
        assert_eq!(
            tn_cell(Some(estimate(-17.9, -10.5)), true),
            "^1            0^7"
        );
        // Even a range entirely outside the clamp is reported as a red 0.
        assert_eq!(
            tn_cell(Some(estimate(-40.0, -33.0)), true),
            "^1            0^7"
        );
    }

    #[test]
    fn tn_cell_keeps_the_placeholder_without_an_estimate() {
        assert_eq!(tn_cell(None, false), "            -");
        // The feature cannot void without a reading; it stays unknown.
        assert_eq!(tn_cell(None, true), "            -");
    }
}
