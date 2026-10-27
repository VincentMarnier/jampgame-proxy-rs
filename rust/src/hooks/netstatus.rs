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
//! With `proxy_sv_enableNetStatus 0` the feature is **completely inert**
//! (user directive, 2026-10-26): the per-cmd feed, the per-packet record
//! work, the table and every estimator computation are gated on the cvar, so
//! the only cost left is one cvar mirror read per usercmd message.

use core::ffi::{c_char, c_int};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::engine;
use crate::hooks::engine_sv::{
    client_num_from_addr, clients_base, cstr_bytes, read_i32, read_usize, svs_time,
};
use crate::netstatus_est::{self, Estimate};
use crate::sdk::{
    CS_CONNECTED, CS_ZOMBIE, OFFSET_CLIENT_FRAMES, OFFSET_CLIENT_NAME, OFFSET_CLIENT_PING,
    OFFSET_CLIENT_RATE, OFFSET_CLIENT_SNAPSHOT_MESSAGE_SENT, OFFSET_CLIENT_SNAPSHOT_MSEC,
    OFFSET_CLIENT_STATE, PACKET_MASK, SIZEOF_CLIENT_SNAPSHOT_T, SIZEOF_CLIENT_T, Usercmd,
};
use crate::state::{self, CVAR_ENABLE_NET_STATUS};
use crate::syscall;

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
/// With `proxy_sv_enableNetStatus 0` the whole feature is inert: this returns
/// before touching the engine or the window (the per-cmd feed and the table
/// are gated too), and the off→on edge resets every window through
/// [`NETSTATUS_WAS_OFF`].
///
/// # Safety
///
/// `cl` must be a valid `client_t*`.
pub unsafe fn mark_packet_start(cl: usize, message_acknowledge: i32) {
    let client_num = client_num_from_addr(cl);
    if client_num >= crate::sdk::MAX_CLIENTS {
        return;
    }
    if !state::with_state(|s| s.cvars[CVAR_ENABLE_NET_STATUS].integer != 0) {
        NETSTATUS_WAS_OFF.store(true, Ordering::Relaxed);
        return;
    }
    // SAFETY: engine memory layer initialised.
    let now = unsafe { svs_time() };
    // SAFETY: cl is a valid client_t.
    let message_sent = unsafe { acked_message_sent(cl, message_acknowledge, now) };
    // SAFETY: cl is a valid client_t.
    let snapshot_msec = unsafe { read_i32(cl + OFFSET_CLIENT_SNAPSHOT_MSEC) };
    let fps = sv_fps();
    state::with_state(|s| {
        let c = &mut s.clients[client_num];
        c.packet_counter = c.packet_counter.wrapping_add(1);
        let ns = &mut c.netstatus;
        // reset rules (D-007 step 4): sv_fps or snapshotMsec change
        // invalidates the window
        if ns.sv_fps != fps || ns.snapshot_msec != snapshot_msec {
            if ns.sv_fps != 0 || ns.snapshot_msec != 0 {
                ns.reset();
            }
            ns.sv_fps = fps;
            ns.snapshot_msec = snapshot_msec;
        }
        ns.mark_packet_start(now, message_sent, message_acknowledge);
    });
}

/// Per-usercmd feed: accumulate the cmd into the open packet record.
pub fn feed_cmd(client_num: usize, cmd: &Usercmd) {
    state::with_state(|s| s.clients[client_num].netstatus.feed_cmd(cmd.server_time));
}

/// Drop one client's estimator window (connect/disconnect, D-007 step 4).
pub fn reset_client(client_num: usize) {
    state::with_state(|s| {
        if let Some(client) = s.clients.get_mut(client_num) {
            client.netstatus.reset();
        }
    });
}

/// Drop every estimator window (`proxy_sv_enableNetStatus` off -> on). The
/// reset is deferred to each client's next usercmd packet (`reset_pending`) so
/// the hot path never clears the 512-record ring mid-packet.
pub fn reset_all() {
    state::with_state(|s| {
        for client in s.clients.iter_mut() {
            client.netstatus.reset_pending = true;
        }
    });
}

/// Set when `proxy_sv_enableNetStatus` was seen off, so the next enabled feed
/// invalidates every window (D-007 step 4).
static NETSTATUS_WAS_OFF: AtomicBool = AtomicBool::new(false);

/// `sv_client_think_stub` — the retarget of `call SV_ClientThink` inside the
/// pristine `SV_UserMove` (0x804e891). Runs the pristine `SV_ClientThink`,
/// then the per-usercmd netStatus feed (D-002.1 technique 3).
///
/// # Safety
///
/// Entered from the engine's usercmd loop with `(client_t*, usercmd_t*)`.
pub unsafe extern "C" fn sv_client_think_stub(cl: usize, cmd: *mut Usercmd) {
    // SAFETY: engine function at the fixed table; args from the engine loop.
    unsafe { engine::sv_client_think(cl, cmd as usize) };

    let net_status_enabled = state::with_state(|s| s.cvars[CVAR_ENABLE_NET_STATUS].integer != 0);
    if !net_status_enabled {
        NETSTATUS_WAS_OFF.store(true, Ordering::Relaxed);
        return;
    }
    if NETSTATUS_WAS_OFF.swap(false, Ordering::Relaxed) {
        reset_all();
    }
    // SAFETY: cmd is the engine's usercmd buffer.
    let ucmd = unsafe { &*cmd };
    let client_num = client_num_from_addr(cl);
    // A stale `client_t*` (e.g. after disconnect) can yield an out-of-range
    // slot: fail closed instead of panicking (= abort) in the move-packet hot
    // path.
    if client_num >= crate::sdk::MAX_CLIENTS {
        return;
    }
    feed_cmd(client_num, ucmd);
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

/// `Proxy_Engine_ClientCommand_NetStatus` (`Proxy_Engine_ClientCommand.cpp:
/// 13-96`): print the per-player net status table to `client_num` (rate-limited
/// to once per 500 ms of game time). D-007 columns:
/// `ping rtt jit fps pps timeNudge snaps rate name`.
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
    // SAFETY: engine memory layer initialised.
    let clients = unsafe { clients_base() };

    let mut status: Vec<u8> = Vec::new();
    // header + rule, same widths as the rows below (4/6/5/4/4/13/4/6)
    status.extend_from_slice(
        format!(
            "{:>4} {:>6} {:>5} {:>4} {:>4} {:>13} {:>4} {:>6} {}\n",
            "ping", "rtt", "jit", "fps", "pps", "timeNudge", "snaps", "rate", "name"
        )
        .as_bytes(),
    );
    status.extend_from_slice(
        b"---- ------ ----- ---- ---- ------------- ----- ------ ---------------\n",
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
        let (tn_cell, rtt, jit) = match est {
            Some(est) => (
                format!("[{:.1}..{:.1}]", est.lo, est.hi),
                format!("{:.1}", est.rtt_mean),
                format!("{:.1}", est.rtt_dev),
            ),
            None => ("-".to_string(), "-".to_string(), "-".to_string()),
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
            format!(
                "{ping_str} {rtt:>6} {jit:>5} {fps:4} {pps:4} {tn_cell:>13} {snaps:5} {rate:6} "
            )
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
