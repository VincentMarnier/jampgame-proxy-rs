//! Proxy_SharedAPI — the traps and vmMain events the proxy intercepts
//! (`Proxy_SharedAPI.cpp`, `Proxy_OriginalAPI_Wrappers.cpp`).
//!
//! Fidelity notes:
//! - String helpers (`Q_stricmpn`, `Info_*`, `ConcatArgs`) come from the
//!   *original game module* (`jampgame.functions.*`), exactly as the original
//!   proxy does, so edge-case semantics are the game's, not ours.
//! - The `netStatus`/`showNet` command handler (`Proxy_Engine_ClientCommand_NetStatus`)
//!   is not ported yet (it needs the `client_t`/`playerState_t` struct surface
//!   of a later milestone); while `proxy_sv_enableNetStatus` is non-zero the
//!   command is forwarded to the game instead of being intercepted — see
//!   `docs/`.

use core::ffi::{CStr, c_char, c_int};

use std::ffi::CString;

use crate::jampgame;
use crate::original;
use crate::sdk::{
    FP_LEVITATION, MAX_INFO_STRING, MAX_NETNAME, MAX_TOKEN_CHARS, NUM_FORCE_POWERS, ROLL, Usercmd,
};
use crate::state::{
    self, CVAR_DISABLE_KILL_CMD, CVAR_ENABLE_NET_STATUS, CVAR_MAX_CALLVOTE_MAPRESTART,
    CVAR_MODEL_PATH_LENGTH,
};
use crate::syscall;
use crate::utils;

const MAX_CVAR_VALUE_STRING: usize = 256;

// ---------------------------------------------------------------------------
// Trap interception (import table — traps the game issues to the engine)
// ---------------------------------------------------------------------------

/// `Proxy_SharedAPI_LocateGameData`: remember where the game placed its data
/// (`Proxy_SharedAPI.cpp:14-22`).
pub fn locate_game_data(
    g_entities: usize,
    num_entities: c_int,
    g_entity_size: c_int,
    g_clients: usize,
    g_client_size: c_int,
) {
    state::with_state(|s| {
        s.located_game_data.g_entities = g_entities;
        s.located_game_data.num_entities = num_entities;
        s.located_game_data.g_entity_size = g_entity_size;
        s.located_game_data.g_clients = g_clients;
        s.located_game_data.g_client_size = g_client_size;
    });
}

/// `Proxy_SharedAPI_GetUsercmd` (`Proxy_SharedAPI.cpp:24-32`): sanitise the
/// usercmd the engine just wrote.
///
/// # Safety
///
/// `ucmd` must be a valid `usercmd_t*` the engine just filled (from a
/// `G_GET_USERCMD` trap).
pub unsafe fn get_usercmd(_client_num: c_int, ucmd: *mut Usercmd) {
    // SAFETY: caller guarantees a valid usercmd pointer.
    let ucmd = unsafe { &mut *ucmd };
    if ucmd.forcesel == FP_LEVITATION || ucmd.forcesel >= NUM_FORCE_POWERS {
        ucmd.forcesel = 0xFF;
    }
    ucmd.angles[ROLL] = 0;
}

// ---------------------------------------------------------------------------
// vmMain event dispatch (export table — events the engine sends the proxy)
// ---------------------------------------------------------------------------

/// `Proxy_SharedAPI_ClientConnect` (`Proxy_SharedAPI.cpp:38-45`): announce the
/// proxy to first-time non-bot connects.
pub fn client_connect(client_num: c_int, first_time: bool, is_bot: bool) {
    if first_time && !is_bot {
        // SAFETY: static welcome text (same bytes as the original proxy's
        // `va("print \"^5%s (^7%s^5)^7\n\"", JAMPGAMEPROXY_NAME, VERSION)`).
        unsafe {
            syscall::send_server_command(client_num, c"print \"^5jampgame_proxy (^70.0.7^5)^7\n\"");
        }
    }
}

/// `Proxy_SharedAPI_ClientDisconnect` (`Proxy_SharedAPI.cpp:47-53`): reset the
/// client's proxy-side bookkeeping.
pub fn client_disconnect(client_num: c_int) {
    state::with_state(|s| {
        if (0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num)
            && s.clients[client_num as usize].is_connected
        {
            s.clients[client_num as usize] = Default::default();
        }
    });
}

/// `Proxy_SharedAPI_ClientBegin` (`Proxy_SharedAPI.cpp:55-61`): mark the client
/// connected.
pub fn client_begin(client_num: c_int, _allow_team_reset: bool) {
    state::with_state(|s| {
        if (0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num) {
            s.clients[client_num as usize].is_connected = true;
        }
    });
}

/// `Proxy_SharedAPI_ClientCommand` (`Proxy_SharedAPI.cpp:63-214`): filter and
/// sanitise client commands. Returns `true` to let the command through to the
/// game, `false` to block it.
pub fn client_command(client_num: c_int) -> bool {
    if !(0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num) {
        return true;
    }
    let is_connected = state::with_state(|s| s.clients[client_num as usize].is_connected);
    if !is_connected {
        return false;
    }

    let mut cmd = [0u8; MAX_TOKEN_CHARS];
    let mut cmd_arg1 = [0u8; MAX_TOKEN_CHARS];
    let mut cmd_arg2 = [0u8; MAX_TOKEN_CHARS];
    let mut say_cmd = false;

    // SAFETY: engine syscall registered (dllEntry ran) and buffers writable.
    unsafe { syscall::argv(0, &mut cmd) };
    let cmd = nul_terminated(&cmd);

    // Anti-cheat: kick jkaDST_ tool commands.
    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"jkaDST_".as_ptr(), 7) } == 0 {
        let netname = client_netname(client_num);
        let chat =
            format!("chat \"^3(Anti-Cheat system) ^7{netname}^3 got kicked cause of cheating^7\"");
        // SAFETY: formatted text has no interior NUL.
        let cchat = CString::new(chat).expect("chat text has no interior NUL");
        unsafe { syscall::send_server_command(-1, &cchat) };
        // SAFETY: literal reason string.
        unsafe {
            syscall::drop_client(
                client_num,
                c"(Anti-Cheat system) you got kicked cause of cheating",
            )
        };
        return false;
    }

    // SAFETY: game module loaded (GAME_INIT ran before any client command).
    let args_concat = unsafe { jampgame::concat_args(1) };
    let args_concat = cstr_bytes_or_empty(args_concat);

    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"say".as_ptr(), 3) } == 0
        || unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"say_team".as_ptr(), 8) } == 0
        || unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"tell".as_ptr(), 4) } == 0
    {
        say_cmd = true;
        if args_concat.len() > 256 {
            return false;
        }
    }

    // SAFETY: engine syscall registered; buffers writable.
    unsafe { syscall::argv(1, &mut cmd_arg1) };
    unsafe { syscall::argv(2, &mut cmd_arg2) };
    let cmd_arg1 = nul_terminated(&cmd_arg1);
    let cmd_arg2 = nul_terminated(&cmd_arg2);

    // Fix: gc crash — refuse clientNums at/above sv_maxclients.
    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"gc".as_ptr(), 2) } == 0 {
        // SAFETY: engine syscall registered.
        let sv_maxclients = unsafe { syscall::cvar_variable_integer_value(c"sv_maxclients") };
        if utils::atoi(cmd_arg1) >= sv_maxclients {
            return false;
        }
    }

    // Fix: crash npc spawn.
    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"npc".as_ptr(), 3) } == 0
        && unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"spawn".as_ptr(), 5) } == 0
        && (unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg2), c"ragnos".as_ptr(), 6) } == 0
            || unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg2), c"saber_droid".as_ptr(), 6) } == 0)
    {
        return false;
    }

    // Fix: team crash.
    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"team".as_ptr(), 4) } == 0
        && (unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"follow1".as_ptr(), 7) } == 0
            || unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"follow2".as_ptr(), 7) } == 0)
    {
        return false;
    }

    // Disable callteamvote (useless in basejka, can bug the custom-client UI).
    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"callteamvote".as_ptr(), 12) } == 0 {
        return false;
    }

    if state::with_state(|s| s.cvars[CVAR_DISABLE_KILL_CMD].integer != 0)
        && unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"kill".as_ptr(), 4) } == 0
    {
        return false;
    }

    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"callvote".as_ptr(), 8) } == 0 {
        let cmd_arg2_number_value = utils::atoi(cmd_arg2);
        let mut min_arg2 = 0i32;
        let mut max_arg2 = 1i32;
        let mut check_needed = false;

        // Fix: callvote long string length crash.
        if cmd_arg2.len() >= MAX_CVAR_VALUE_STRING {
            return false;
        }

        if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"capturelimit".as_ptr(), 12) } == 0 {
            min_arg2 = 0;
            max_arg2 = 0x7FFF_FFFF;
            check_needed = true;
        }
        if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"fraglimit".as_ptr(), 9) } == 0 {
            min_arg2 = 0;
            max_arg2 = 0x7FFF_FFFF;
            check_needed = true;
        }
        if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"g_doWarmup".as_ptr(), 10) } == 0 {
            min_arg2 = 0;
            max_arg2 = 1;
            check_needed = true;
        }
        if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"map_restart".as_ptr(), 11) } == 0 {
            min_arg2 = 0;
            max_arg2 = state::with_state(|s| s.cvars[CVAR_MAX_CALLVOTE_MAPRESTART].integer);
            check_needed = true;
        }
        if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"timelimit".as_ptr(), 9) } == 0 {
            min_arg2 = 0;
            max_arg2 = 35790;
            check_needed = true;
        }

        if check_needed && (cmd_arg2_number_value < min_arg2 || cmd_arg2_number_value > max_arg2) {
            return false;
        }
    }

    if utils::strchrs(args_concat, b"\r\n").is_some() {
        return false;
    }
    if !say_cmd && utils::strchrs(args_concat, b";").is_some() {
        return false;
    }

    let net_status_enabled = state::with_state(|s| s.cvars[CVAR_ENABLE_NET_STATUS].integer != 0);
    if net_status_enabled
        && (unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"netStatus".as_ptr(), 9) } == 0
            || unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"showNet".as_ptr(), 7) } == 0)
    {
        // Not yet ported (needs client_t/playerState_t layout from a later
        // milestone); forward the command to the game instead of intercepting.
        eprintln!("----- proxy-rs: netStatus/showNet not yet ported; forwarding");
        return true;
    }

    true
}

/// `Proxy_SharedAPI_ClientUserinfoChanged` (`Proxy_SharedAPI.cpp:222-329`):
/// sanitise the client's name/model/forcepowers in their userinfo.
pub fn client_userinfo_changed(client_num: c_int) {
    let mut userinfo_buf = [0u8; MAX_INFO_STRING];
    // SAFETY: engine syscall registered; buffer writable.
    unsafe { syscall::get_userinfo(client_num, &mut userinfo_buf) };
    let info_len = utils::c_strlen(&userinfo_buf);
    if info_len == 0 {
        return;
    }
    // userinfo_buf is a NUL-terminated info string the whole time (the engine
    // writes it, Info_SetValueForKey keeps it terminated); hand out the base
    // pointer for the game helpers and keep the buffer for the writes.
    let info_ptr = userinfo_buf.as_ptr() as *const c_char;

    // SAFETY: game module loaded; userinfo is NUL-terminated.
    let name_val = unsafe { jampgame::info_value_for_key(info_ptr, c"name".as_ptr()) };
    let name_val = cstr_bytes_or_empty(name_val);

    // Sanitise the name into the client's persisted clean-name slot (mirrors
    // `currentClientData->cleanName`), then write it back.
    let clean_name = {
        let mut name = [0u8; MAX_NETNAME];
        utils::client_clean_name(name_val, &mut name);
        state::with_state(|s| {
            s.clients[client_num as usize].clean_name = name;
        });
        name
    };
    // SAFETY: all strings NUL-terminated; userinfo buffer writable.
    unsafe {
        jampgame::info_set_value_for_key(
            userinfo_buf.as_mut_ptr() as *mut c_char,
            c"name".as_ptr(),
            clean_name.as_ptr() as *const c_char,
        )
    };

    // SAFETY: game module loaded.
    let model_ptr = unsafe { jampgame::info_value_for_key(info_ptr, c"model".as_ptr()) };
    if !model_ptr.is_null() {
        let model = cstr_bytes_or_empty(model_ptr);
        let len = model.len() as c_int;

        // Anti-cheat: darksidetools model.
        if unsafe { jampgame::q_stricmpn(model_ptr, c"darksidetools".as_ptr(), len) } == 0 {
            let name = String::from_utf8_lossy(&clean_name[..utils::c_strlen(&clean_name)]);
            let chat =
                format!("chat \"^3(Anti-Cheat system) ^7{name}^3 got kicked cause of cheating^7\"");
            // SAFETY: formatted text has no interior NUL.
            let cchat = CString::new(chat).expect("chat text has no interior NUL");
            unsafe { syscall::send_server_command(-1, &cchat) };
            // SAFETY: literal reason string.
            unsafe {
                syscall::drop_client(
                    client_num,
                    c"(Anti-Cheat system) you got kicked cause of cheating",
                )
            };
        }

        // Fix bugged / overlong models (also the model length crash fix; a
        // length check is duplicated in SV_UserinfoChanged).
        let bad_model = (unsafe { jampgame::q_stricmpn(model_ptr, c"jedi_".as_ptr(), 5) } == 0
            && (unsafe { jampgame::q_stricmpn(model_ptr, c"jedi_/red".as_ptr(), len) } == 0
                || unsafe { jampgame::q_stricmpn(model_ptr, c"jedi_/blue".as_ptr(), len) } == 0))
            || unsafe { jampgame::q_stricmpn(model_ptr, c"rancor".as_ptr(), len) } == 0
            || unsafe { jampgame::q_stricmpn(model_ptr, c"wampa".as_ptr(), len) } == 0
            || !utils::is_valid_ascii_str(model)
            || model.len()
                >= state::with_state(|s| s.cvars[CVAR_MODEL_PATH_LENGTH].integer) as usize;

        if bad_model {
            // SAFETY: userinfo buffer writable; literals NUL-terminated.
            unsafe {
                jampgame::info_set_value_for_key(
                    userinfo_buf.as_mut_ptr() as *mut c_char,
                    c"model".as_ptr(),
                    c"kyle".as_ptr(),
                )
            };
        }
    }

    // Fix forcepowers crash (`Proxy_SharedAPI.cpp:270-324`).
    let mut force_powers = [0u8; 30];
    // SAFETY: game module loaded.
    let fp_ptr = unsafe { jampgame::info_value_for_key(info_ptr, c"forcepowers".as_ptr()) };
    utils::strncpyz(&mut force_powers, cstr_bytes_or_empty(fp_ptr));
    let fp_len = utils::c_strlen(&force_powers);

    let mut bad_force = false;
    if (22..=24).contains(&fp_len) {
        let mut seps = 0u32;
        for (i, &b) in force_powers[..fp_len].iter().enumerate() {
            if b != b'-' && !b.is_ascii_digit() {
                bad_force = true;
                break;
            }
            if !(1..=5).contains(&i) && b == b'-' {
                bad_force = true;
                break;
            }
            if i > 0 && force_powers[i - 1] == b'-' && b == b'-' {
                bad_force = true;
                break;
            }
            if b == b'-' {
                seps += 1;
            }
        }
        if seps != 2 {
            bad_force = true;
        }
    } else {
        bad_force = true;
    }

    if bad_force {
        utils::strncpyz(&mut force_powers, b"7-1-030000000000003332");
    }

    // SAFETY: strncpyz NUL-terminates force_powers.
    let fp_cstr = unsafe { CStr::from_bytes_with_nul_unchecked(&force_powers) };
    unsafe {
        jampgame::info_set_value_for_key(
            userinfo_buf.as_mut_ptr() as *mut c_char,
            c"forcepowers".as_ptr(),
            fp_cstr.as_ptr(),
        )
    };

    // SAFETY: Info_SetValueForKey keeps userinfo_buf NUL-terminated.
    let userinfo_cstr = unsafe { CStr::from_bytes_with_nul_unchecked(&userinfo_buf) };
    unsafe { syscall::set_userinfo(client_num, userinfo_cstr) };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Bytes before the NUL terminator of `buf`.
fn nul_terminated(buf: &[u8]) -> &[u8] {
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    &buf[..n]
}

/// Bytes of a NUL-terminated C string, or the empty slice if null.
fn cstr_bytes_or_empty<'a>(p: *const c_char) -> &'a [u8] {
    if p.is_null() {
        &b""[..]
    } else {
        // SAFETY: caller guarantees a NUL-terminated string pointer.
        unsafe { CStr::from_ptr(p) }.to_bytes()
    }
}

fn cstr_ptr(bytes: &[u8]) -> *const c_char {
    bytes.as_ptr() as *const c_char
}

/// `(proxy.g_entities + clientNum)->client->pers.netname` via the SDK layout
/// offsets (`gentity_t.client` @ 864, `gclient_t.pers` @ 1552,
/// `clientPersistant_t.netname` @ 48).
fn client_netname(client_num: c_int) -> String {
    let base = original::g_entities_address();
    if base == 0 {
        return String::new();
    }
    let entity = base + client_num as usize * crate::sdk::SIZEOF_GENTITY;
    // SAFETY: g_entities is a valid gentity_t array of the loaded game; the
    // client pointer is set for connected players.
    let client_ptr = unsafe { *((entity + crate::sdk::OFFSET_GENTITY_CLIENT) as *const usize) };
    if client_ptr == 0 {
        return String::new();
    }
    let netname_ptr =
        client_ptr + crate::sdk::OFFSET_GCLIENT_PERS + crate::sdk::OFFSET_PERS_NETNAME;
    // SAFETY: netname is a NUL-terminated MAX_NETNAME string inside gclient_t.
    let bytes = unsafe { CStr::from_ptr(netname_ptr as *const c_char) }.to_bytes();
    String::from_utf8_lossy(bytes).into_owned()
}
