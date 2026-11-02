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
use std::sync::Mutex;

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
use crate::userinfo;
use crate::utils;

const MAX_CVAR_VALUE_STRING: usize = 256;

/// Longest command line (name, space and arguments) a client may send to the
/// game (upstream `d9daa68`, 11-01 F1). The game logs some command lines whole
/// in a 1024-byte buffer; no command of the game needs more than this.
const MAX_COMMAND_LENGTH: usize = 512;

/// Number of entries of `gc_orders` in the game (`g_cmds.c:1721-1729`,
/// upstream `85403cd`, 11-01 F3).
const GC_ORDERS_COUNT: i32 = 7;

/// Reason given to the engine (connect refusal) or to `DropClient` (userinfo
/// change) when the `darksidetools` anti-cheat fires (10-25 F3).
const ANTI_CHEAT_KICK_REASON: &CStr = c"(Anti-Cheat system) you got kicked cause of cheating";

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
    state::with_state(|s| {
        if (0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num) {
            // Remembered for every connect, including the reconnect after a
            // map change, so the userinfo validation can skip bots later.
            s.clients[client_num as usize].is_bot = is_bot;
        }
    });
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
        // A slot reused by a human must not keep the bot exemption.
        entry.is_bot = false;
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

/// Whether `GAME_CLIENT_CONNECT` runs the userinfo sanitiser (10-25 F2):
/// every non-bot connect, first or reconnect. Bots are skipped: their userinfo
/// comes from the bot files and has no `forcepowers`, so the sanitiser would
/// only add the default string (and `WP_InitForcePowers` ignores it for bots).
pub fn connect_userinfo_sanitized(is_bot: bool) -> bool {
    !is_bot
}

/// Why a userinfo was refused, as returned to the FFI callers.
pub struct SanitizeRejection {
    /// Static C string: the engine reads it after `GAME_CLIENT_CONNECT`
    /// returns.
    pub reason: &'static CStr,
    /// `true` for the `darksidetools` anti-cheat (public kick message);
    /// `false` for a validation or length failure (silent drop).
    pub anti_cheat: bool,
    /// Cleaned name when the name was processed (the anti-cheat chat uses it);
    /// zeroed otherwise.
    pub clean_name: [u8; MAX_NETNAME],
}

/// Owned rejection used by the pure core, so tests never touch the static
/// reason buffer.
struct SanitizeError {
    reason: String,
    anti_cheat: bool,
    clean_name: [u8; MAX_NETNAME],
}

/// A sanitised userinfo, ready for the caller to write back.
struct SanitizedUserinfo {
    info: Vec<u8>,
    clean_name: [u8; MAX_NETNAME],
}

/// Static storage for the last rejection reason. The engine reads the pointer
/// after `GAME_CLIENT_CONNECT` returns, so it cannot be a local (upstream uses
/// `static char reason[96]`). The server is single-threaded and consumes a
/// reason before the next sanitisation.
static REJECTION_REASON: Mutex<[u8; 128]> = Mutex::new([0; 128]);

/// Store `text` in [`REJECTION_REASON`] and return it as a C string.
fn rejection_reason(text: &str) -> &'static CStr {
    let mut buf = REJECTION_REASON
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let bytes = text.as_bytes();
    let n = bytes.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&bytes[..n]);
    buf[n] = 0;
    // SAFETY: the bytes live in the static array, are NUL-terminated just
    // above, and are read by the engine before the next sanitisation.
    unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) }
}

fn rejection(text: &str) -> SanitizeError {
    SanitizeError {
        reason: text.to_string(),
        anti_cheat: false,
        clean_name: [0; MAX_NETNAME],
    }
}

fn to_rejection(error: SanitizeError) -> SanitizeRejection {
    SanitizeRejection {
        reason: rejection_reason(&error.reason),
        anti_cheat: error.anti_cheat,
        clean_name: error.clean_name,
    }
}

/// Pure core of [`sanitize_userinfo`]: validate and clean the name, model and
/// `forcepowers` of `info` (the bytes before the NUL), returning the new
/// userinfo or the rejection.
///
/// `validate` is set for every non-bot client: OpenJK's
/// `G_ValidateUserinfo` runs first, so a duplicate or missing key, a bad slash
/// count or a control character refuses the client instead of reaching the
/// sanitising writes. Every write then goes through
/// [`crate::userinfo::set_value_for_key_vec`]; a write that does not fit (a
/// bad value near `MAX_INFO_STRING`) refuses the client instead of leaving the
/// old value in place.
fn sanitize_userinfo_bytes(
    client_num: c_int,
    info: &[u8],
    validate: bool,
    max_model_len: usize,
) -> Result<Option<SanitizedUserinfo>, SanitizeError> {
    if validate {
        if let Err(detail) = userinfo::validate_userinfo(info) {
            return Err(rejection(&format!("Failed userinfo validation: {detail}")));
        }
    }
    if info.is_empty() {
        return Ok(None);
    }
    // S9 (10-25 O1): the engine adds `ip` from the real address before
    // GAME_CLIENT_CONNECT; if it could not (a userinfo too long for the key),
    // refuse instead of letting the game check bans against no address. The
    // userinfo-change path re-adds `ip` in `SV_UserinfoChanged` first.
    if validate && userinfo::value_for_key(info, b"ip").is_empty() {
        return Err(rejection("Userinfo string length exceeded."));
    }
    let mut info = info.to_vec();

    let name_val = userinfo::value_for_key(&info, b"name").to_vec();
    let mut clean_name = [0u8; MAX_NETNAME];
    utils::client_clean_name(&name_val, &mut clean_name);
    if !userinfo::set_value_for_key_vec(&mut info, b"name", nul_terminated(&clean_name)) {
        return Err(rejection("Userinfo string length exceeded."));
    }

    // Copied out before the vector is written below. Missing and empty are the
    // same here (as the game's `Info_ValueForKey`).
    let model = userinfo::value_for_key(&info, b"model").to_vec();
    if !model.is_empty() {
        let is_darkside_tools = userinfo::starts_with_ignore_case(&model, b"darksidetools");
        let is_jedi_red = userinfo::starts_with_ignore_case(&model, b"jedi_/red");
        let is_jedi_blue = userinfo::starts_with_ignore_case(&model, b"jedi_/blue");
        let is_rancor = userinfo::starts_with_ignore_case(&model, b"rancor");
        let is_wampa = userinfo::starts_with_ignore_case(&model, b"wampa");

        // Anti-cheat: darksidetools model. The caller refuses the client: the
        // engine gets the reason on connect, and a userinfo change is dropped
        // after `DropClient`. Nothing is written back here (10-25 F3).
        if is_darkside_tools {
            if diagnostics::suspicious_enabled() {
                diagnostics::suspicious(
                    "userinfo.kick",
                    format!(
                        "{} reason=\"darksidetools model\" userinfo=\"{}\"",
                        crate::hooks::engine_sv::client_label_for_num(client_num),
                        diagnostics::escape_bytes(&info, 512)
                    ),
                );
            }
            return Err(SanitizeError {
                reason: ANTI_CHEAT_KICK_REASON.to_string_lossy().into_owned(),
                anti_cheat: true,
                clean_name,
            });
        }

        // Fix bugged / overlong models (also the model length crash fix; a
        // length check is duplicated in SV_UserinfoChanged).
        let bad_model = (userinfo::starts_with_ignore_case(&model, b"jedi_")
            && (is_jedi_red || is_jedi_blue))
            || is_rancor
            || is_wampa
            || !utils::is_valid_ascii_str(&model)
            || model.len() >= max_model_len;

        if bad_model {
            if diagnostics::suspicious_enabled() {
                diagnostics::suspicious(
                    "userinfo.model",
                    format!(
                        "{} len={} model=\"{}\" forced=\"kyle\"",
                        crate::hooks::engine_sv::client_label_for_num(client_num),
                        model.len(),
                        diagnostics::escape_bytes(&model, 256)
                    ),
                );
            }
            if !userinfo::set_value_for_key_vec(&mut info, b"model", b"kyle") {
                return Err(rejection("Userinfo string length exceeded."));
            }
        }
    }

    // F5 (10-25): `color1` and `color2` are copied whole into `CS_PLAYERS`,
    // which is part of every client's gamestate (16000 characters at most).
    // Cut each at 15, as OpenJK; TaystJK/EternalJK put a hat or cape name after
    // the colour (1 + 13 characters).
    if !truncate_colors(&mut info) {
        return Err(rejection("Userinfo string length exceeded."));
    }

    // Fix forcepowers crash (`Proxy_SharedAPI.cpp:270-324`).
    let mut force_powers = [0u8; 30];
    let fp_val = userinfo::value_for_key(&info, b"forcepowers").to_vec();
    utils::strncpyz(&mut force_powers, &fp_val);
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
    let fp_len = utils::c_strlen(&force_powers);

    if !userinfo::set_value_for_key_vec(&mut info, b"forcepowers", &force_powers[..fp_len]) {
        return Err(rejection("Userinfo string length exceeded."));
    }

    Ok(Some(SanitizedUserinfo { info, clean_name }))
}

/// `Proxy_SharedAPI_SanitizeUserinfo` (10-25 F2, O8): validate and clean the
/// name, model and `forcepowers` of a client's userinfo.
///
/// Returns `Some(reason)` when the client must be refused; nothing is written
/// back in that case. The function never drops the client itself: on a connect
/// the engine expects the reason as the `GAME_CLIENT_CONNECT` return value,
/// and `DropClient` during that event is unsafe (the slot is still
/// `CS_FREE`).
pub fn sanitize_userinfo(client_num: c_int, validate: bool) -> Option<SanitizeRejection> {
    if !(0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num) {
        return None;
    }
    let mut userinfo_buf = [0u8; MAX_INFO_STRING];
    // SAFETY: engine syscall registered; buffer writable.
    unsafe { syscall::get_userinfo(client_num, &mut userinfo_buf) };
    let info_len = utils::c_strlen(&userinfo_buf);
    // Admin-controlled cvar is clamped >= 1 so a negative value cannot disable
    // the check via a huge `as usize` cast.
    let max_model_len =
        state::with_state(|s| s.cvars[CVAR_MODEL_PATH_LENGTH].integer.max(1)) as usize;

    match sanitize_userinfo_bytes(
        client_num,
        &userinfo_buf[..info_len],
        validate,
        max_model_len,
    ) {
        Err(error) => {
            if error.anti_cheat {
                // The anti-cheat chat needs the cleaned name; other refusals
                // have none.
                state::with_state(|s| {
                    s.clients[client_num as usize].clean_name = error.clean_name;
                });
            }
            Some(to_rejection(error))
        }
        Ok(None) => None,
        Ok(Some(sanitized)) => {
            state::with_state(|s| {
                s.clients[client_num as usize].clean_name = sanitized.clean_name;
            });
            let len = sanitized.info.len();
            if len >= MAX_INFO_STRING {
                return Some(to_rejection(rejection("Userinfo string length exceeded.")));
            }
            userinfo_buf[..len].copy_from_slice(&sanitized.info);
            userinfo_buf[len] = 0;
            // SAFETY: the buffer is NUL-terminated just above.
            let cstr = unsafe { CStr::from_ptr(userinfo_buf.as_ptr() as *const c_char) };
            unsafe { syscall::set_userinfo(client_num, cstr) };
            None
        }
    }
}

/// `Proxy_SharedAPI_ClientUserinfoChanged` (`Proxy_SharedAPI.cpp:222-329`):
/// sanitise the userinfo on a `userinfo` change.
///
/// Returns `false` when the client was dropped for the anti-cheat, so `vmMain`
/// does not forward `GAME_CLIENT_USERINFO_CHANGED` to the game. The game's
/// `ClientUserinfoChanged` does not test that the client is connected and would
/// rewrite `CS_PLAYERS` for a slot that is being cleared (10-25 F3).
pub fn client_userinfo_changed(client_num: c_int) -> bool {
    // Bots keep the userinfo from their bot files (no required keys).
    let validate = state::with_state(|s| {
        (0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num)
            && !s.clients[client_num as usize].is_bot
    });
    let Some(rejection) = sanitize_userinfo(client_num, validate) else {
        return true;
    };

    if rejection.anti_cheat {
        let name = String::from_utf8_lossy(
            &rejection.clean_name[..utils::c_strlen(&rejection.clean_name)],
        );
        let chat =
            format!("chat \"^3(Anti-Cheat system) ^7{name}^3 got kicked cause of cheating^7\"");
        // SAFETY: formatted text has no interior NUL.
        let cchat = CString::new(chat).expect("chat text has no interior NUL");
        unsafe { syscall::send_server_command(-1, &cchat) };
    }

    // SAFETY: a static CStr (literal or the rejection buffer).
    unsafe { syscall::drop_client(client_num, rejection.reason) };

    false
}

/// Whether a `forcepowers` value passes the proxy's shape check
/// (`Proxy_SharedAPI.cpp:270-324`, as ported, plus 11-01 F4).
///
/// 22 to 24 characters, only digits and `-`, exactly two `-` at positions 1
/// to 5 and never adjacent, **at least 18 power digits after the second `-`**,
/// and every power digit in `0..=3`. The rank and side digits are not limited:
/// the game replaces the rank and checks the side itself.
///
/// `BG_LegalizedForcePowers` reads exactly `NUM_FORCE_POWERS` (18) digits after
/// the second `-`; with fewer, the missing `final_Powers[]` entries keep stack
/// garbage and are used as indices. A level above 3 is used as a
/// `bgForcePowerCost` column before being clamped.
///
/// The caller still writes the default string on failure.
fn forcepowers_is_valid(powers: &[u8]) -> bool {
    if !(22..=24).contains(&powers.len()) {
        return false;
    }
    let mut seps = 0u32;
    let mut second_sep = 0usize;
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
            if seps == 2 {
                second_sep = i;
            }
        }
    }
    if seps != 2 {
        return false;
    }

    // F4 (10-25): `BG_LegalizedForcePowers` reads NUM_FORCE_POWERS (18)
    // digits after the second `-`. With fewer, the missing `final_Powers[]`
    // entries keep stack garbage and are used as loop counters and as a
    // `bgForcePowerCost` column.
    if powers.len() - second_sep - 1 < 18 {
        return false;
    }

    // A level above 3 is used as a `bgForcePowerCost` column before being
    // clamped; refuse it, as OpenJK. The rank and side digits (before the
    // second `-`) are not limited: the game replaces/checks them itself.
    powers[second_sep + 1..]
        .iter()
        .all(|&b| (b'0'..=b'3').contains(&b))
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

/// Cut `color1` and `color2` at 15 characters (F5, upstream `01adba1`): the
/// values go as they are into the player's `CS_PLAYERS` configstring, which is
/// part of every client's gamestate. A colour is 1 digit plus up to 13
/// characters of a hat/cape name in TaystJK/EternalJK, so 15 keeps the
/// cosmetics. Returns `false` when the (shorter) write does not fit.
fn truncate_colors(info: &mut Vec<u8>) -> bool {
    for key in [b"color1".as_slice(), b"color2".as_slice()] {
        let color = userinfo::value_for_key(info, key).to_vec();
        if color.len() > 15 && !userinfo::set_value_for_key_vec(info, key, &color[..15]) {
            return false;
        }
    }
    true
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
        clientkick_max_slot, command_line_too_long, connect_userinfo_sanitized,
        forcepowers_is_valid, gc_command_denied, get_usercmd, vote_value_out_of_range,
    };
    use crate::sdk::{FP_LEVITATION, ROLL, Usercmd};

    /// 10-25 F2: the connect sanitiser runs for every non-bot first connection
    /// and reconnect; bots keep the userinfo from their bot file.
    #[test]
    fn connect_sanitiser_skips_bots_only() {
        assert!(connect_userinfo_sanitized(false));
        assert!(!connect_userinfo_sanitized(true));
    }

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

    /// F4 (10-25): `BG_LegalizedForcePowers` reads 18 power digits after the
    /// second `-`; the 18 shapes of the old window include 8 with only 16 or
    /// 17. After the fix only the 10 long shapes pass, and every accepted
    /// shape has at least 18 powers.
    #[test]
    fn f4_requires_18_power_digits() {
        let mut accepted = 0;

        for len in 22..=24 {
            for first in 1..=5 {
                for second in (first + 2)..=5 {
                    let mut value = vec![b'1'; len];
                    value[first] = b'-';
                    value[second] = b'-';

                    if forcepowers_is_valid(&value) {
                        accepted += 1;
                        assert!(
                            len - second > 18,
                            "accepted a shape with fewer than 18 powers: {len}/{first}/{second}"
                        );
                    }
                }
            }
        }

        assert_eq!(accepted, 10, "only the 10 long shapes of the window pass");
    }

    /// F4: a level above 3 is used as a `bgForcePowerCost` column before being
    /// clamped to 3. The 2026 fix refuses anything outside 0..=3, as OpenJK.
    #[test]
    fn f4_rejects_power_digits_above_three() {
        assert!(!forcepowers_is_valid(b"7-1-333333333333333399"));
        assert!(forcepowers_is_valid(b"7-1-030000000000003332"));
    }

    /// F4: the rank and side digits (before the second `-`) are not limited;
    /// the game replaces the rank and checks the side itself.
    #[test]
    fn f4_leaves_rank_and_side_digits_unrestricted() {
        assert!(forcepowers_is_valid(b"9-9-333333333333333333"));
    }

    // -----------------------------------------------------------------------
    // Full sanitiser core (S6 / O8)
    // -----------------------------------------------------------------------

    /// Outcome of a sanitisation, owned so parallel tests never share the
    /// static FFI reason buffer.
    struct Outcome {
        anti_cheat: bool,
        reason: Option<String>,
        info: Option<Vec<u8>>,
    }

    fn sanitize_with(input: &[u8], validate: bool) -> Outcome {
        match super::sanitize_userinfo_bytes(0, input, validate, 64) {
            Ok(Some(s)) => Outcome {
                anti_cheat: false,
                reason: None,
                info: Some(s.info),
            },
            Ok(None) => Outcome {
                anti_cheat: false,
                reason: None,
                info: None,
            },
            Err(e) => Outcome {
                anti_cheat: e.anti_cheat,
                reason: Some(e.reason),
                info: None,
            },
        }
    }

    fn valid_userinfo() -> Vec<u8> {
        b"\\ip\\127.0.0.1:29070\\name\\Padawan\\rate\\25000\\snaps\\20\
          \\model\\kyle/default\\forcepowers\\7-1-030000000000003332\
          \\color1\\4\\color2\\4\\handicap\\100\\cg_predictItems\\1\
          \\saber1\\single_1\\saber2\\none\\char_color_red\\255\
          \\char_color_green\\255\\char_color_blue\\255"
            .to_vec()
    }

    #[test]
    fn sanitizer_rewrites_a_bad_name_model_and_forcepowers() {
        let input = b"\\ip\\127.0.0.1:29070\\name\\***Bad\\rate\\25000\\snaps\\20\
            \\model\\rancor/default\\forcepowers\\7-1-\\color1\\4\\color2\\4\
            \\handicap\\100\\cg_predictItems\\1\\saber1\\single_1\\saber2\\none\
            \\char_color_red\\255\\char_color_green\\255\\char_color_blue\\255";
        let outcome = sanitize_with(input, true);
        assert!(outcome.reason.is_none());
        let out = outcome
            .info
            .expect("the sanitised userinfo must be written");
        assert_eq!(crate::userinfo::value_for_key(&out, b"name"), b"Bad");
        assert_eq!(crate::userinfo::value_for_key(&out, b"model"), b"kyle");
        assert_eq!(
            crate::userinfo::value_for_key(&out, b"forcepowers"),
            b"7-1-030000000000003332"
        );
    }

    #[test]
    fn sanitizer_refuses_a_duplicate_key() {
        let mut input = valid_userinfo();
        input.extend_from_slice(b"\\Forcepowers\\7-1-");
        let outcome = sanitize_with(&input, true);
        assert!(!outcome.anti_cheat);
        assert_eq!(
            outcome.reason.unwrap(),
            "Failed userinfo validation: Too many forcepowers fields (2/1)"
        );
        assert!(outcome.info.is_none(), "nothing is written on a refusal");
    }

    /// O8: a userinfo close to the limit with a capitalised `Forcepowers`
    /// passes the validation (the key is counted without case) and the write
    /// that would replace it cannot fit. Before S6 the old bad value stayed
    /// and the game read it; now the client is refused.
    #[test]
    fn sanitizer_refuses_the_o8_write_that_cannot_fit() {
        let mut input = b"\\ip\\127.0.0.1:29070\\name\\Padawan\\rate\\25000\\snaps\\20\
            \\model\\kyle\\Forcepowers\\7-1-\\color1\\4\\color2\\4\\handicap\\100\
            \\cg_predictItems\\1\\saber1\\single_1\\saber2\\none\
            \\char_color_red\\255\\char_color_green\\255\\char_color_blue\\255"
            .to_vec();
        // 990 bytes: the sanitising `\forcepowers\<default>` (35 bytes)
        // cannot fit, so the checked write reports the failure.
        let pad = 990 - input.len() - 5;
        input.extend_from_slice(b"\\pad\\");
        input.extend(std::iter::repeat_n(b'x', pad));
        assert_eq!(input.len(), 990);
        assert!(
            crate::userinfo::validate_userinfo(&input).is_ok(),
            "the capitalised key passes the validation"
        );
        assert_eq!(
            crate::userinfo::value_for_key(&input, b"forcepowers"),
            b"7-1-",
            "the game would read the bad value"
        );

        let outcome = sanitize_with(&input, true);
        assert!(!outcome.anti_cheat);
        assert_eq!(outcome.reason.unwrap(), "Userinfo string length exceeded.");
        assert!(outcome.info.is_none(), "nothing is written on a refusal");
    }

    #[test]
    fn sanitizer_refuses_darksidetools_without_writing() {
        let input = String::from_utf8(valid_userinfo())
            .unwrap()
            .replace("model\\kyle/default", "model\\darksidetools/default")
            .into_bytes();
        let outcome = sanitize_with(&input, true);
        assert!(outcome.anti_cheat);
        assert_eq!(
            outcome.reason.unwrap(),
            "(Anti-Cheat system) you got kicked cause of cheating"
        );
        assert!(outcome.info.is_none(), "nothing is written on a refusal");
    }

    #[test]
    fn sanitizer_skips_validation_for_bots() {
        // A bot userinfo has none of the required keys.
        let input = b"\\name\\Bot\\model\\kyle";
        let outcome = sanitize_with(input, false);
        assert!(outcome.reason.is_none());
        assert!(
            outcome.info.is_some(),
            "the bot userinfo is still sanitised, just not validated"
        );
    }

    /// S9 (10-25 O1): a non-bot connect whose userinfo has no `ip` was too long
    /// for the engine to add it, so the game's ban check would see no address.
    /// The connect is refused. A bot is not validated and keeps its userinfo.
    #[test]
    fn sanitizer_requires_ip_for_a_validated_connect() {
        let without_ip = String::from_utf8(valid_userinfo())
            .unwrap()
            .replace("\\ip\\127.0.0.1:29070", "")
            .into_bytes();
        assert!(!without_ip.is_empty());

        let outcome = sanitize_with(&without_ip, true);
        assert!(!outcome.anti_cheat);
        assert_eq!(outcome.reason.unwrap(), "Userinfo string length exceeded.");
        assert!(outcome.info.is_none(), "nothing is written on a refusal");

        let outcome = sanitize_with(&without_ip, false);
        assert!(outcome.reason.is_none());
        assert!(outcome.info.is_some(), "the bot path does not require `ip`");
    }

    /// F5 (10-25): `color1`/`color2` go into every client's gamestate; cut them
    /// at 15, keeping the `<colour><hat/cape>` cosmetics.
    #[test]
    fn sanitizer_cuts_color1_and_color2_to_15() {
        let input = String::from_utf8(valid_userinfo())
            .unwrap()
            .replace("\\color1\\4", "\\color1\\4abcdefghijklmnopqrstuvwxyz")
            .replace("\\color2\\4", "\\color2\\4ABCDEFGHIJKLMNOPQRSTUVWXYZ")
            .into_bytes();

        let outcome = sanitize_with(&input, true);
        assert!(outcome.reason.is_none());
        let out = outcome
            .info
            .expect("the sanitised userinfo must be written");
        assert_eq!(
            crate::userinfo::value_for_key(&out, b"color1"),
            b"4abcdefghijklmn"
        );
        assert_eq!(
            crate::userinfo::value_for_key(&out, b"color2"),
            b"4ABCDEFGHIJKLMN"
        );
        assert_eq!(
            crate::userinfo::value_for_key(&out, b"color1").len(),
            15,
            "1 colour digit + 14 characters"
        );
    }
}
