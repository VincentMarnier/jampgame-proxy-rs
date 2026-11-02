//! Proxy_SharedAPI — the traps and vmMain events the proxy intercepts
//! (`Proxy_SharedAPI.cpp`, `Proxy_OriginalAPI_Wrappers.cpp`).
//!
//! Fidelity notes:
//! - String helpers (`Q_stricmpn`, `Info_*`, `ConcatArgs`) come from the
//!   *original game module* (`jampgame.functions.*`), exactly as the original
//!   proxy does, so edge-case semantics are the game's, not ours.
//! - `netStatus`/`showNet` are intercepted here and handled by
//!   `crate::hooks::netstatus` (D-001.2 lifted).

use core::ffi::{CStr, c_char, c_int};

use std::ffi::CString;

use crate::diagnostics;
use crate::jampgame;
use crate::original;
use crate::sdk::{
    FP_LEVITATION, MAX_INFO_STRING, MAX_NETNAME, MAX_TOKEN_CHARS, NUM_FORCE_POWERS, ROLL, Usercmd,
};
use crate::state::{
    self, CVAR_ENABLE_NET_STATUS, CVAR_MAX_CALLVOTE_MAPRESTART, CVAR_MODEL_PATH_LENGTH,
};
use crate::syscall;
use crate::utils;

const MAX_CVAR_VALUE_STRING: usize = 256;

/// Longest command line (name, space and arguments) a client may send to the
/// game (upstream `d9daa68`, 11-01 F1). The game logs some command lines whole
/// in a 1024-byte buffer; no command of the game needs more than this.
const MAX_COMMAND_LENGTH: usize = 512;

/// Number of entries of `gc_orders` in the game (`g_cmds.c:1721-1729`,
/// upstream `85403cd`, 11-01 F3).
const GC_ORDERS_COUNT: i32 = 7;

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
    if first_time {
        // D-007 reset rule: a connect starts from an empty estimator window.
        // The packet-rate limiter starts from a fresh measurement window too:
        // a slot reused by the next player must not inherit a filtered rate.
        crate::hooks::netstatus::reset_client(client_num as usize);
        crate::pps::reset_client(client_num as usize);
    }
    if first_time && !is_bot {
        // Same shape as the original proxy's
        // `va("print \"^5%s (^7%s^5)^7\n\"", JAMPGAMEPROXY_NAME, VERSION)`, with
        // the name renamed to `jampgame-proxy-rs` and the version taken from the
        // crate (`Cargo.toml`) so releases only bump it in one place.
        let banner = CString::new(format!(
            "print \"^5jampgame-proxy-rs (^7{}^5)^7\n\"",
            crate::VERSION
        ))
        .expect("static banner contains no interior NUL");
        // SAFETY: `banner` is a NUL-terminated C string that outlives the call;
        // the engine syscall pointer is registered.
        unsafe {
            syscall::send_server_command(client_num, &banner);
        }
    }
}

/// `Proxy_SharedAPI_ClientDisconnect` (`Proxy_SharedAPI.cpp:47-53`): reset the
/// client's proxy-side bookkeeping.
pub fn client_disconnect(client_num: c_int) {
    // The leaving player's ignore list goes, and so does their slot from every
    // other player's list: a new player inheriting the slot must not be
    // silently ignored. Runs before the entry reset below (and outside its lock
    // section).
    crate::ignore::on_client_disconnect(client_num);
    state::with_state(|s| {
        if !(0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num) {
            return;
        }
        let entry = &mut s.clients[client_num as usize];
        // An admin mute never outlives the player who earned it. This runs even
        // when the connect mirror never saw the slot begin: bots are added by
        // the game module itself and do not raise `GAME_CLIENT_BEGIN`.
        entry.muted = false;
        if entry.is_connected {
            *entry = Default::default();
            // Drop any in-progress jump-hold so a new client reusing the slot
            // is not forced to jump by a stale timer.
            s.jump_start_time[client_num as usize] = 0;
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
        log_blocked(client_num, "not connected", b"", b"");
        return false;
    }

    let mut cmd = [0u8; MAX_TOKEN_CHARS];
    let mut cmd_arg1 = [0u8; MAX_TOKEN_CHARS];
    let mut cmd_arg2 = [0u8; MAX_TOKEN_CHARS];
    let mut say_cmd = false;

    // SAFETY: engine syscall registered (dllEntry ran) and buffers writable.
    unsafe { syscall::argv(0, &mut cmd) };
    cmd[MAX_TOKEN_CHARS - 1] = 0;
    let cmd = nul_terminated(&cmd);

    // Anti-cheat: kick jkaDST_ tool commands.
    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"jkaDST_".as_ptr(), 7) } == 0 {
        log_blocked(client_num, "anti-cheat jkaDST_ command", cmd, &[]);
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

    // Fix: stack overflow in the game's G_LogPrintf (`vsprintf` in a buffer of
    // 1024 bytes). At intermission the game turns every command it does not
    // know into a public chat message of the whole command line, and logs it
    // before cutting it. The limits on "say", "say_team" and "tell" below do
    // not cover that. 512 leaves a wide margin: the overflow starts at about
    // 970 characters.
    if command_line_too_long(cmd, args_concat) {
        log_blocked(
            client_num,
            "command line longer than 512 bytes",
            cmd,
            args_concat,
        );
        return false;
    }

    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"say".as_ptr(), 3) } == 0
        || unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"say_team".as_ptr(), 8) } == 0
        || unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"tell".as_ptr(), 4) } == 0
    {
        say_cmd = true;
        if args_concat.len() > 256 {
            log_blocked(client_num, "chat longer than 256 bytes", cmd, args_concat);
            return false;
        }
    }

    // SAFETY: engine syscall registered; buffers writable.
    unsafe { syscall::argv(1, &mut cmd_arg1) };
    unsafe { syscall::argv(2, &mut cmd_arg2) };
    cmd_arg1[MAX_TOKEN_CHARS - 1] = 0;
    cmd_arg2[MAX_TOKEN_CHARS - 1] = 0;
    let cmd_arg1 = nul_terminated(&cmd_arg1);
    let cmd_arg2 = nul_terminated(&cmd_arg2);

    // Fix: gc crash. The player number must be a client slot (`client` is NULL
    // from sv_maxclients up to MAX_CLIENTS - 1) and the order an index of the
    // game's `gc_orders`: the game also accepts the array length, one past the
    // end, and sends what it reads there as a text (crash on Linux). The game
    // already refuses a negative player or order, so those are left to it.
    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"gc".as_ptr(), 2) } == 0 {
        // SAFETY: engine syscall registered.
        let sv_maxclients = unsafe { syscall::cvar_variable_integer_value(c"sv_maxclients") };
        if gc_command_denied(cmd_arg1, cmd_arg2, sv_maxclients) {
            log_blocked(
                client_num,
                "gc player >= sv_maxclients or order past gc_orders",
                cmd,
                args_concat,
            );
            return false;
        }
    }

    // Fix: crash npc spawn.
    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"npc".as_ptr(), 3) } == 0
        && unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"spawn".as_ptr(), 5) } == 0
        && (unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg2), c"ragnos".as_ptr(), 6) } == 0
            || unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg2), c"saber_droid".as_ptr(), 6) } == 0)
    {
        log_blocked(client_num, "npc spawn crash guard", cmd, args_concat);
        return false;
    }

    // Fix: team crash.
    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"team".as_ptr(), 4) } == 0
        && (unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"follow1".as_ptr(), 7) } == 0
            || unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"follow2".as_ptr(), 7) } == 0)
    {
        log_blocked(client_num, "team follow crash guard", cmd, args_concat);
        return false;
    }

    // Disable callteamvote (useless in basejka, can bug the custom-client UI).
    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"callteamvote".as_ptr(), 12) } == 0 {
        log_blocked(client_num, "callteamvote disabled", cmd, args_concat);
        return false;
    }

    if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"callvote".as_ptr(), 8) } == 0 {
        let cmd_arg2_number_value = utils::atoi(cmd_arg2);
        let mut min_arg2 = 0i32;
        let mut max_arg2 = 1i32;
        let mut check_needed = false;

        // Fix: callvote long string length crash.
        if cmd_arg2.len() >= MAX_CVAR_VALUE_STRING {
            log_blocked(client_num, "callvote value too long", cmd, args_concat);
            return false;
        }

        // Fix: callvote clientkick crash. The game only compares the number
        // with MAX_CLIENTS, then reads g_entities[n].client, which is NULL from
        // sv_maxclients up to MAX_CLIENTS - 1.
        if unsafe { jampgame::q_stricmpn(cstr_ptr(cmd_arg1), c"clientkick".as_ptr(), 10) } == 0 {
            // SAFETY: engine syscall registered.
            let sv_maxclients = unsafe { syscall::cvar_variable_integer_value(c"sv_maxclients") };
            min_arg2 = 0;
            max_arg2 = clientkick_max_slot(sv_maxclients);
            check_needed = true;
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

        if check_needed && vote_value_out_of_range(cmd_arg2_number_value, min_arg2, max_arg2) {
            log_blocked(client_num, "callvote value out of range", cmd, args_concat);
            return false;
        }
    }

    if utils::strchrs(args_concat, b"\r\n").is_some() {
        log_blocked(client_num, "CR/LF command injection", cmd, args_concat);
        return false;
    }
    if !say_cmd && utils::strchrs(args_concat, b";").is_some() {
        log_blocked(client_num, "semicolon command injection", cmd, args_concat);
        return false;
    }

    let net_status_enabled = state::with_state(|s| s.cvars[CVAR_ENABLE_NET_STATUS].integer != 0);
    if net_status_enabled
        && (unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"netStatus".as_ptr(), 9) } == 0
            || unsafe { jampgame::q_stricmpn(cstr_ptr(cmd), c"showNet".as_ptr(), 7) } == 0)
    {
        crate::hooks::netstatus::client_command_net_status(client_num);
        return false;
    }

    true
}

/// `Proxy_SharedAPI_ClientUserinfoChanged` (`Proxy_SharedAPI.cpp:222-329`):
/// sanitise the client's name/model/forcepowers in their userinfo.
pub fn client_userinfo_changed(client_num: c_int) {
    if !(0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num) {
        return;
    }
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

        // Fixed literal lengths, so the game helper never reads past the
        // literal's NUL (prefix semantics: catches suffixed variants).
        // "darksidetools" = 13, "jedi_/red" = 9, "jedi_/blue" = 10,
        // "rancor" = 6, "wampa" = 5.
        let is_darkside_tools = model.len() >= 13
            && unsafe { jampgame::q_stricmpn(model_ptr, c"darksidetools".as_ptr(), 13) } == 0;
        let is_jedi_red = model.len() >= 9
            && unsafe { jampgame::q_stricmpn(model_ptr, c"jedi_/red".as_ptr(), 9) } == 0;
        let is_jedi_blue = model.len() >= 10
            && unsafe { jampgame::q_stricmpn(model_ptr, c"jedi_/blue".as_ptr(), 10) } == 0;
        let is_rancor = model.len() >= 6
            && unsafe { jampgame::q_stricmpn(model_ptr, c"rancor".as_ptr(), 6) } == 0;
        let is_wampa = model.len() >= 5
            && unsafe { jampgame::q_stricmpn(model_ptr, c"wampa".as_ptr(), 5) } == 0;

        // Anti-cheat: darksidetools model.
        if is_darkside_tools {
            if diagnostics::suspicious_enabled() {
                diagnostics::suspicious(
                    "userinfo.kick",
                    format!(
                        "{} reason=\"darksidetools model\" userinfo=\"{}\"",
                        crate::hooks::engine_sv::client_label_for_num(client_num),
                        diagnostics::escape_bytes(&userinfo_buf, 512)
                    ),
                );
            }
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
        // Admin-controlled cvar is clamped >= 1 so a negative value cannot
        // disable the check via a huge `as usize` cast.
        let max_model_len =
            state::with_state(|s| s.cvars[CVAR_MODEL_PATH_LENGTH].integer.max(1)) as usize;
        let bad_model = (unsafe { jampgame::q_stricmpn(model_ptr, c"jedi_".as_ptr(), 5) } == 0
            && (is_jedi_red || is_jedi_blue))
            || is_rancor
            || is_wampa
            || !utils::is_valid_ascii_str(model)
            || model.len() >= max_model_len;

        if bad_model {
            if diagnostics::suspicious_enabled() {
                diagnostics::suspicious(
                    "userinfo.model",
                    format!(
                        "{} len={} model=\"{}\" forced=\"kyle\"",
                        crate::hooks::engine_sv::client_label_for_num(client_num),
                        model.len(),
                        diagnostics::escape_bytes(model, 256)
                    ),
                );
            }
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

    if !forcepowers_is_valid(&force_powers[..fp_len]) {
        if diagnostics::suspicious_enabled() {
            diagnostics::suspicious(
                "userinfo.forcepowers",
                format!(
                    "{} len={} forcepowers=\"{}\" forced=\"7-1-030000000000003332\"",
                    crate::hooks::engine_sv::client_label_for_num(client_num),
                    fp_len,
                    diagnostics::escape_bytes(&force_powers[..fp_len], 64)
                ),
            );
        }
        utils::strncpyz(&mut force_powers, b"7-1-030000000000003332");
    }

    // SAFETY: strncpyz NUL-terminates force_powers; from_ptr stops at the
    // first NUL (the buffer has trailing zeros, so the unchecked slice form
    // would violate the single-NUL CStr contract).
    let fp_cstr = unsafe { CStr::from_ptr(force_powers.as_ptr() as *const c_char) };
    unsafe {
        jampgame::info_set_value_for_key(
            userinfo_buf.as_mut_ptr() as *mut c_char,
            c"forcepowers".as_ptr(),
            fp_cstr.as_ptr(),
        )
    };

    // SAFETY: Info_SetValueForKey keeps userinfo_buf NUL-terminated; the
    // engine wrote it and every mutation above preserves termination.
    // from_ptr stops at the first NUL instead of claiming trailing zeros.
    userinfo_buf[MAX_INFO_STRING - 1] = 0;
    let userinfo_cstr = unsafe { CStr::from_ptr(userinfo_buf.as_ptr() as *const c_char) };
    unsafe { syscall::set_userinfo(client_num, userinfo_cstr) };
}

/// Whether a `forcepowers` value passes the proxy's shape check
/// (`Proxy_SharedAPI.cpp:270-324`, as ported).
///
/// The rule, verbatim from the inline loop it replaces: 22 to 24 characters,
/// only digits and `-`, exactly two `-` at positions 1 to 5, never adjacent.
/// It is a pure function so the audit vectors of
/// `docs/ybeproxy-2026-delta.md` (F4) can be pinned by unit tests; the caller
/// still writes the default string on failure.
///
/// Known gap (F4, 2026-11-01 audit): the rule does not require the 18 power
/// digits that `BG_LegalizedForcePowers` reads after the second `-`, and it
/// accepts a power digit above 3. See the tests below.
fn forcepowers_is_valid(powers: &[u8]) -> bool {
    if !(22..=24).contains(&powers.len()) {
        return false;
    }
    let mut seps = 0u32;
    for (i, &b) in powers.iter().enumerate() {
        if b != b'-' && !b.is_ascii_digit() {
            return false;
        }
        if !(1..=5).contains(&i) && b == b'-' {
            return false;
        }
        if i > 0 && powers[i - 1] == b'-' && b == b'-' {
            return false;
        }
        if b == b'-' {
            seps += 1;
        }
    }
    seps == 2
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Whether the command line `cmd` + `" "` + `args` exceeds
/// [`MAX_COMMAND_LENGTH`]. Mirrors upstream's
/// `strlen(cmd) + 1 + strlen(argsConcat) > 512`: the separating space is
/// counted even when there are no arguments (11-01 F1).
fn command_line_too_long(cmd: &[u8], args: &[u8]) -> bool {
    cmd.len() + 1 + args.len() > MAX_COMMAND_LENGTH
}

/// Whether a `gc <player> <order>` command must be dropped by the proxy
/// (upstream `85403cd`, 11-01 F3).
///
/// The retail game only refuses `player < 0 || player >= MAX_CLIENTS` and
/// `order < 0 || order > sizeof(gc_orders)/sizeof(char *)`. The second test is
/// off by one: `order == GC_ORDERS_COUNT` passes and `gc_orders[7]` is read
/// past the array. It also never checks the player against `sv_maxclients`;
/// from there to `MAX_CLIENTS - 1` the slot's `client` pointer is `NULL` and
/// reading it crashes the server. Both out-of-range directions are left to the
/// game on purpose: they already return there.
fn gc_command_denied(player: &[u8], order: &[u8], sv_maxclients: i32) -> bool {
    utils::atoi(player) >= sv_maxclients || utils::atoi(order) >= GC_ORDERS_COUNT
}

/// Last client slot `callvote clientkick <n>` may target (upstream `94c89b0`,
/// 11-01 F2). The game only tests `n < MAX_CLIENTS`, then reads
/// `g_entities[n].client`, which is `NULL` from `sv_maxclients` up to
/// `MAX_CLIENTS - 1`.
fn clientkick_max_slot(sv_maxclients: i32) -> i32 {
    sv_maxclients - 1
}

/// Whether a numeric `callvote` argument is outside `min..=max`. Used by the
/// per-vote range table in `client_command`; `clientkick` fills the bounds
/// with `0..=clientkick_max_slot(sv_maxclients)`.
fn vote_value_out_of_range(value: i32, min: i32, max: i32) -> bool {
    value < min || value > max
}

/// Log a command the proxy refused to forward (or a client that tried to send
/// one before it was fully in the game).
fn log_blocked(client_num: c_int, reason: &str, cmd: &[u8], args: &[u8]) {
    if !diagnostics::suspicious_enabled() {
        return;
    }
    diagnostics::suspicious(
        "clientcmd.block",
        format!(
            "{} reason=\"{}\" cmd=\"{}\" args=\"{}\"",
            crate::hooks::engine_sv::client_label_for_num(client_num),
            reason,
            diagnostics::escape_bytes(cmd, 128),
            diagnostics::escape_bytes(args, 512)
        ),
    );
}

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
    if !(0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num) {
        return String::new();
    }
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

#[cfg(test)]
mod tests {
    use super::{
        clientkick_max_slot, command_line_too_long, forcepowers_is_valid, gc_command_denied,
        get_usercmd, vote_value_out_of_range,
    };
    use crate::sdk::{FP_LEVITATION, ROLL, Usercmd};

    /// 11-01 F1: the whole command line (name, one separating space, then the
    /// arguments) is capped at 512 bytes, and exactly 512 is still allowed.
    /// This is the upstream `strlen(cmd) + 1 + strlen(argsConcat) > 512`.
    #[test]
    fn command_line_is_capped_at_512_bytes() {
        // 3 + 1 + 508 = 512: allowed.
        assert!(!command_line_too_long(b"say", &[b'a'; 508]));
        // 3 + 1 + 509 = 513: dropped.
        assert!(command_line_too_long(b"say", &[b'a'; 509]));

        // The separating space is counted even without arguments.
        assert!(!command_line_too_long(b"x", b""));
        assert!(!command_line_too_long(b"", &[b'a'; 511]));
        assert!(command_line_too_long(b"", &[b'a'; 512]));

        // The attack shape: a full `ConcatArgs(0)` line of 1022 characters.
        assert!(command_line_too_long(b"x", &[b'a'; 1022]));
    }

    /// 11-01 F3: a player in a real client slot and an order in `gc_orders`
    /// pass. `g_entities[player].client` is set below `sv_maxclients`;
    /// `gc_orders` has 7 entries.
    #[test]
    fn gc_accepts_a_real_slot_and_a_listed_order() {
        assert!(!gc_command_denied(b"0", b"0", 8));
        assert!(!gc_command_denied(b"7", b"6", 8));
        // `atoi` semantics: a non-numeric value is 0, a valid slot/order.
        assert!(!gc_command_denied(b"abc", b"abc", 8));
    }

    /// The off-by-one upstream fix: order 7 is `gc_orders[7]`, one past the
    /// array. On the Linux retail module that entry is the text `"prin"` and
    /// the server crashes when `G_Say` reads it.
    #[test]
    fn gc_drops_the_order_past_the_end() {
        assert!(gc_command_denied(b"0", b"7", 8));
        assert!(gc_command_denied(b"0", b"8", 8));
        assert!(gc_command_denied(b"0", b"999", 8));
    }

    /// A player at or above `sv_maxclients` has a `NULL` `client` pointer;
    /// the game's own `MAX_CLIENTS` test does not catch it.
    #[test]
    fn gc_drops_a_player_outside_the_configured_slots() {
        assert!(gc_command_denied(b"8", b"0", 8));
        assert!(gc_command_denied(b"31", b"0", 8));
        // The same player is valid on a full 32-slot server.
        assert!(!gc_command_denied(b"31", b"0", 32));
    }

    /// Negative players and orders are already refused by the game
    /// (`order < 0 || order > 7`, `player < 0`), so the proxy leaves them
    /// alone; upstream made the same choice.
    #[test]
    fn gc_leaves_negative_player_and_order_to_the_game() {
        assert!(!gc_command_denied(b"-1", b"0", 8));
        assert!(!gc_command_denied(b"0", b"-1", 8));
    }

    /// 11-01 F2: `callvote clientkick <n>` accepts every slot below
    /// `sv_maxclients`, including the last one. The table sets
    /// `min = 0`, `max = clientkick_max_slot(sv_maxclients)`.
    #[test]
    fn clientkick_accepts_slots_below_sv_maxclients() {
        for sv_maxclients in [1, 8, 32] {
            let max = clientkick_max_slot(sv_maxclients);
            assert!(!vote_value_out_of_range(0, 0, max));
            assert!(!vote_value_out_of_range(sv_maxclients - 1, 0, max));
        }
    }

    /// The crash: from `sv_maxclients` to `MAX_CLIENTS - 1`,
    /// `g_entities[n].client` is NULL. The game's own `MAX_CLIENTS` test does
    /// not catch it.
    #[test]
    fn clientkick_drops_the_slot_at_or_above_sv_maxclients() {
        assert!(vote_value_out_of_range(8, 0, clientkick_max_slot(8)));
        assert!(vote_value_out_of_range(31, 0, clientkick_max_slot(8)));
        assert!(vote_value_out_of_range(32, 0, clientkick_max_slot(32)));
    }

    /// A negative number is already refused by the game (`n < 0`), but the
    /// table's `min = 0` rejects it too, as upstream.
    #[test]
    fn clientkick_drops_a_negative_slot() {
        assert!(vote_value_out_of_range(-1, 0, clientkick_max_slot(8)));
    }

    /// `get_usercmd` still sanitises `forcesel` and `ROLL`. The engine writes
    /// the struct first; the proxy only mutates it in place.
    #[test]
    fn get_usercmd_sanitises_forcesel_and_roll() {
        let mut cmd = Usercmd {
            forcesel: FP_LEVITATION,
            angles: [1, 2, 3],
            ..Default::default()
        };
        // SAFETY: a local `Usercmd` is a valid pointer; the function only
        // mutates the struct, no engine or game call is made.
        unsafe { get_usercmd(0, &mut cmd) };
        assert_eq!(cmd.forcesel, 0xFF);
        assert_eq!(cmd.angles[ROLL], 0);
    }

    /// O10 known gap (2026-10-25 audit): ioq3/OpenJK clamp a movement axis of
    /// -128 to -127 so a negative move is never one step longer than a
    /// positive one. Our `get_usercmd` leaves -128 untouched.
    ///
    /// When O10 is fixed, this test must be inverted.
    #[test]
    fn o10_known_gap_get_usercmd_leaves_minus_128() {
        let mut cmd = Usercmd {
            forwardmove: -128,
            rightmove: -128,
            upmove: -128,
            ..Default::default()
        };
        // SAFETY: see the test above.
        unsafe { get_usercmd(0, &mut cmd) };
        assert_eq!(
            (cmd.forwardmove, cmd.rightmove, cmd.upmove),
            (-128, -128, -128)
        );
    }

    /// The value the retail client sends at full rank with 18 power levels.
    /// `BG_LegalizedForcePowers` reads exactly `NUM_FORCE_POWERS` (18) digits
    /// after the second `-`.
    #[test]
    fn retail_forcepowers_value_is_accepted() {
        assert!(forcepowers_is_valid(b"7-1-030000000000003332"));
    }

    /// Every power at 0..=3 is a legal level. A string of the wrong length or
    /// shape is left to the game's default.
    #[test]
    fn length_and_separator_shape_is_enforced() {
        assert!(!forcepowers_is_valid(b"7-1-"));
        // 21 and 25 characters are outside the accepted window.
        assert!(!forcepowers_is_valid(b"7-1-00000000000000000"));
        assert!(!forcepowers_is_valid(b"7-1-000000000000000000000"));
        // Only two `-`, never adjacent, and only at positions 1..=5.
        assert!(!forcepowers_is_valid(b"7--0300000000000003332"));
        assert!(!forcepowers_is_valid(b"7-1-00000000000000333-"));
        assert!(!forcepowers_is_valid(b"7-1-a00000000000003332"));
    }

    /// F4 known gap (2026-11-01 audit): our accepted window is exactly the 18
    /// shapes the audit enumerated, and 8 of them carry fewer than the 18
    /// digits `BG_LegalizedForcePowers` reads after the second `-`. The
    /// missing entries stay uninitialised on the stack and are used as indices
    /// (`bgForcePowerCost[i][countDown]` in the retail game), which can crash
    /// the server.
    ///
    /// When F4 is fixed, this test must be inverted: `short` must be 0 and
    /// every short shape must be refused.
    #[test]
    fn f4_known_gap_accepts_shapes_with_16_or_17_powers() {
        let mut accepted = 0;
        let mut short = 0;

        for len in 22..=24 {
            for first in 1..=5 {
                for second in (first + 2)..=5 {
                    let mut value = vec![b'1'; len];
                    value[first] = b'-';
                    value[second] = b'-';

                    if forcepowers_is_valid(&value) {
                        accepted += 1;
                        // The game reads a digit for every index from the byte
                        // after the second `-` to the end of the string.
                        if len - second - 1 < 18 {
                            short += 1;
                        }
                    }
                }
            }
        }

        assert_eq!(accepted, 18, "the accepted window has 18 shapes");
        assert_eq!(
            short, 8,
            "8 of them give BG_LegalizedForcePowers 16 or 17 digits"
        );
    }

    /// F4 known gap, second half: `BG_LegalizedForcePowers` uses the digit as
    /// a column of `bgForcePowerCost` before clamping it to 3, so 4..=9 read
    /// past the table. The 2026 fix refuses anything outside 0..=3, as OpenJK.
    #[test]
    fn f4_known_gap_accepts_power_digits_above_three() {
        assert!(forcepowers_is_valid(b"7-1-333333333333333399"));
    }
}
