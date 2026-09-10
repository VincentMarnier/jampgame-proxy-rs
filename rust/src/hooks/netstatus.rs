//! Players net status — `Proxy_Engine_Client.{hpp,cpp}` +
//! `Proxy_Engine_ClientCommand.cpp` (the D-002 "Players net status" row).
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
//! `cmdIndex`), so `CalcPacketsAndFPS` counts packets, not cmds, exactly.
//!
//! Deliberate deviation (documented, R-022): the original gated the feed and
//! the table's stats on `cl->ping >= 1` — a bot discriminator (`SV_CalcPings`
//! writes ping 0 for `svFlags & SVF_BOT`) that also excluded real players
//! whose computed ping is 0, i.e. every localhost/LAN player. Bots can never
//! reach this feed anyway (they send no `clc_move`; their cmds arrive via the
//! `BOTLIB_USER_COMMAND` syscall, sv_game.cpp:990, a call site that is not
//! retargeted), so the feed runs for every client that reaches it, and the
//! printer discriminates bots with the engine's own
//! `gentity->r.svFlags & SVF_BOT` check instead of ping.

use core::ffi::{c_char, c_int};

use crate::engine;
use crate::hooks::engine_sv::{
    client_num_from_addr, clients_base, cstr_bytes, read_i32, read_usize, svs_time,
};
use crate::sdk::{
    CMD_MASK, CS_CONNECTED, CS_ZOMBIE, OFFSET_CLIENT_NAME, OFFSET_CLIENT_RATE,
    OFFSET_CLIENT_SNAPSHOT_MSEC, OFFSET_CLIENT_STATE, OFFSET_PS_PERSISTANT, PERS_SCORE,
    SIZEOF_CLIENT_T, Usercmd,
};
use crate::state::{self, CVAR_ENABLE_NET_STATUS};
use crate::syscall;

/// `server.cvars.sv_maxclients->integer` (cvar slot 0x8273ea4).
fn sv_maxclients() -> c_int {
    // SAFETY: the slot holds a cvar_t* (R-018 verified).
    let ptr = unsafe { *(crate::engine::cvar_slots::SV_MAXCLIENTS as *const usize) };
    // SAFETY: ptr is a live cvar_t*.
    unsafe { engine::cvar_integer(ptr) }
}

/// `server.cvars.sv_fps->integer` (cvar slot 0x8273e84).
fn sv_fps() -> c_int {
    // SAFETY: cvar slot validated and dereferenced at GAME_INIT (R-018).
    let ptr = unsafe { *(crate::engine::cvar_slots::SV_FPS as *const usize) };
    // SAFETY: the slot holds a cvar_t* (R-018 verified).
    unsafe { engine::cvar_integer(ptr) }
}

/// Whether the client is a bot — the same discriminator `SV_CalcPings` uses
/// (`cl->gentity != 0 && gentity->r.svFlags & SVF_BOT`, binary-verified at
/// `0x8057264-0x805727d`). Replaces the original's `ping >= 1` check, which
/// conflated bots with real players whose computed ping is 0 (localhost/LAN).
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

/// `Proxy_Engine_Client_UpdateUcmdStats` (`Proxy_Engine_Client.cpp:5-13`).
fn update_ucmd_stats(client_num: usize, cmd: &Usercmd, packet_index: usize) {
    state::with_state(|s| {
        let cl = &mut s.clients[client_num];
        cl.cmd_index = cl.cmd_index.wrapping_add(1);
        let idx = cl.cmd_index & (CMD_MASK - 1);
        cl.cmd_stats[idx].server_time = cmd.server_time;
        cl.cmd_stats[idx].packet_index = packet_index;
    });
}

/// `Proxy_Engine_Client_UpdateTimenudge` (`Proxy_Engine_Client.cpp:76-107`).
///
/// # Safety
///
/// `cl` must be a valid `client_t*`; `cmd` a valid `usercmd_t*`.
unsafe fn update_timenudge(cl: usize, cmd: &Usercmd, milliseconds: c_int) {
    let client_num = client_num_from_addr(cl);
    // SAFETY: engine memory layer initialised.
    let now = unsafe { svs_time() };
    // SAFETY: cl is a valid client_t.
    let ping = unsafe { read_i32(cl + crate::sdk::OFFSET_CLIENT_PING) };
    state::with_state(|s| {
        let c = &mut s.clients[client_num];
        c.timenudge_data.delay_count += 1;
        c.timenudge_data.delay_sum += cmd.server_time - now;
        c.timenudge_data.ping_sum += ping;

        // Wait 1500 ms so we have enough data for an approximation.
        if milliseconds < c.timenudge_data.last_time_nudge_calculation + 1500 {
            return;
        }
        let fps = sv_fps();
        // Guard the division the original left unguarded (SIGFPE for
        // sv_fps 0); an invalid sv_fps simply contributes no offset term.
        let ms_per_frame = if fps > 0 { 1000 / fps } else { 0 };
        let magic_offset = get_timenudge_magic_offset(fps);
        // ((serverTime - sv.time) + ping - magicOffset + (1000/sv_fps)) * -1
        c.timenudge = ((((c.timenudge_data.delay_sum as f32)
            / (c.timenudge_data.delay_count as f32))
            + ((c.timenudge_data.ping_sum as f32) / (c.timenudge_data.delay_count as f32))
            - magic_offset as f32
            + ms_per_frame as f32)
            * -1.0) as i32;
        c.timenudge_data.delay_count = 0;
        c.timenudge_data.delay_sum = 0;
        c.timenudge_data.ping_sum = 0;
        c.timenudge_data.last_time_nudge_calculation = milliseconds;
    });
}

/// `Proxy_Engine_Client_CalcPacketsAndFPS` (`Proxy_Engine_Client.cpp:15-42`).
fn calc_packets_and_fps(client_num: usize, packets: &mut c_int, fps: &mut c_int) {
    state::with_state(|s| {
        let cl = &s.clients[client_num];
        let last = cl.cmd_stats[cl.cmd_index & (CMD_MASK - 1)].server_time;
        let mut last_packet_index = 0usize;
        for stat in &cl.cmd_stats {
            if stat.server_time + 1000 >= last {
                *fps += 1;
                if last_packet_index != stat.packet_index {
                    last_packet_index = stat.packet_index;
                    *packets += 1;
                }
            }
        }
    });
}

/// `Proxy_GetTimenudgeMagicOffset` (`Proxy_Engine_Client.cpp:45-74`).
fn get_timenudge_magic_offset(sv_fps: c_int) -> c_int {
    const MAGIC_OFFSETS: [i32; 61] = [
        18, 15, 15, 13, 14, 14, 11, 10, 9, 10, // [20, 29]
        10, 0, 0, 0, 0, -21, -21, -21, -21, -19, // [30, 39] DO NOT USE 31..34
        -19, 0, 0, 0, 0, -19, -17, -17, -16, -17, // [40, 49] DO NOT USE 41..44
        -17, -17, -17, -17, -17, -17, -16, -15, -15, 0, // [50, 59] DO NOT USE 59
        0, 0, 0, -30, -30, -30, -30, -28, -28, -28, // [60, 69] DO NOT USE 60..62
        -28, -28, -28, -28, -28, -28, -27, -26, -26, -27, // [70, 79]
        -27, // [80[
    ];
    let table_size = MAGIC_OFFSETS.len() as i32;
    let mut magic_offset = 0;
    if sv_fps >= 20 {
        let mut index = sv_fps - 20;
        if sv_fps >= table_size + 20 {
            index = table_size - 1;
        }
        magic_offset = MAGIC_OFFSETS[index as usize];
    }
    magic_offset
}

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
        return;
    }
    // SAFETY: cmd is the engine's usercmd buffer.
    let ucmd = unsafe { &*cmd };
    let client_num = client_num_from_addr(cl);
    let packet_index = state::with_state(|s| s.clients[client_num].packet_counter) as usize;
    update_ucmd_stats(client_num, ucmd, packet_index);
    // SAFETY: engine syscall registered; G_MILLISECONDS takes no arguments.
    let now = unsafe { syscall::call_engine(crate::sdk::G_MILLISECONDS, &[]) };
    // SAFETY: cl is a valid client_t; cmd a valid usercmd.
    unsafe { update_timenudge(cl, ucmd, now) };
}

/// `mark_packet_start` — called once per usercmd client message by the
/// `SV_ExecuteClientMessage` entry wrapper, reproducing the original's
/// pre-loop `cmdIndex` packet-identity capture: the counter value handed to
/// the per-cmd `UpdateUcmdStats` is constant within one client message and
/// strictly different between messages.
///
/// # Safety
///
/// `cl` must be a valid `client_t*`.
pub unsafe fn mark_packet_start(cl: usize) {
    let client_num = client_num_from_addr(cl);
    if client_num < crate::sdk::MAX_CLIENTS {
        state::with_state(|s| {
            s.clients[client_num].packet_counter =
                s.clients[client_num].packet_counter.wrapping_add(1);
        });
    }
}

// ---------------------------------------------------------------------------
// Proxy_Engine_ClientCommand_NetStatus
// ---------------------------------------------------------------------------

/// `Proxy_Engine_ClientCommand_NetStatus` (`Proxy_Engine_ClientCommand.cpp:
/// 13-96`): print the per-player net status table to `client_num` (rate-limited
/// to once per 500 ms of game time).
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

    let maxclients = sv_maxclients();
    let fps_cvar = sv_fps();
    // SAFETY: engine memory layer initialised.
    let clients = unsafe { clients_base() };

    let mut status: Vec<u8> = Vec::new();
    status.extend_from_slice(b"score ping rate   fps  packets timeNudge snaps id name \n");
    status
        .extend_from_slice(b"----- ---- ------ ---- ------- --------- ----- -- ---------------\n");

    for i in 0..maxclients {
        let cl = clients + i as usize * SIZEOF_CLIENT_T;
        // SAFETY: cl is a valid client_t.
        let state = unsafe { read_i32(cl + OFFSET_CLIENT_STATE) };
        if state == 0 {
            continue;
        }
        // SAFETY: engine SV_GameClientNum is valid.
        let ps = unsafe { engine::sv_game_client_num(i) };
        let score = if ps != 0 {
            // SAFETY: ps is a valid playerState_t.
            unsafe { read_i32(ps + OFFSET_PS_PERSISTANT + PERS_SCORE * 4) }
        } else {
            0
        };

        let state_str = if state == CS_CONNECTED {
            "CON ".to_string()
        } else if state == CS_ZOMBIE {
            "ZMB ".to_string()
        } else {
            // SAFETY: cl is a valid client_t.
            let ping = unsafe { read_i32(cl + crate::sdk::OFFSET_CLIENT_PING) };
            let ping = if ping < 9999 { ping } else { 9999 };
            format!("{ping:4}")
        };

        let mut fps = 0i32;
        let mut packets = 0i32;
        // SAFETY: cl is a valid client_t.
        if !unsafe { is_bot(cl) } {
            calc_packets_and_fps(i as usize, &mut packets, &mut fps);
        } else {
            // If a bot then snapshotMsec will = 0, which is bad for
            // "1000 / cl->snapshotMsec".
            // SAFETY: cl is a valid client_t.
            unsafe { *((cl + OFFSET_CLIENT_SNAPSHOT_MSEC) as *mut c_int) = 1 };
        }

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
        let timenudge = state::with_state(|s| s.clients[i as usize].timenudge);
        // SAFETY: cl is a valid client_t; name is NUL-terminated.
        let name = cstr_bytes((cl + OFFSET_CLIENT_NAME) as *const c_char);

        status.extend_from_slice(
            format!(
                "{score:5} {state_str} {rate:6} {fps:4} {packets:7} {timenudge:9} {snaps:5} {i:2} "
            )
            .as_bytes(),
        );
        status.extend_from_slice(name);
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
