//! Game-module hook wrappers (`Patches/game/Proxy_g_*.cpp`) — all D-002
//! wanted game-side features as entry wrappers over pristine game functions
//! (`crate::patch::GAME_HOOKS`): stats accounting (`G_Damage`, `player_die`,
//! `BeginIntermission`), anti-HP teller (`G_AddEvent`) and minimum jump time
//! (`ClientThink_real`).

use core::ffi::{c_char, c_int};

use crate::hooks::engine_sv::read_i32;
use crate::hooks::original_call;
use crate::original;
use crate::sdk::{
    MAX_CLIENTS, MAX_NETNAME, OFFSET_ENTITY_CLIENT_NUM, OFFSET_GCLIENT_PERS, OFFSET_GCLIENT_PS,
    OFFSET_GCLIENT_SESS, OFFSET_GENTITY_CLIENT, OFFSET_GENTITY_DAMAGE_REDIRECT,
    OFFSET_GENTITY_PLAYER_STATE, OFFSET_GENTITY_S, OFFSET_LEVEL_TIME, OFFSET_PERS_CMD,
    OFFSET_PERS_NETNAME, OFFSET_PS_CLIENT_NUM, OFFSET_PS_PERSISTANT, OFFSET_PS_STATS,
    OFFSET_SESS_SESSION_TEAM, PERS_KILLED, PERS_SCORE, STAT_ARMOR, STAT_HEALTH, TEAM_BLUE,
    TEAM_RED, TEAM_SPECTATOR,
};
use crate::state;
use crate::syscall;
use crate::utils;

/// `EV_PAIN` event number (SDK `bg_public.h:851`, oracle-verified).
const EV_PAIN: c_int = 89;

/// `proxy.g_entities` (recorded by `G_LOCATE_GAME_DATA`).
fn g_entities() -> usize {
    state::with_state(|s| s.located_game_data.g_entities)
}

/// `proxy.level` (exported by the original module, R-004).
fn level() -> usize {
    original::level_address()
}

/// `ent->s.clientNum`.
///
/// # Safety
///
/// `ent` must be a valid `gentity_t*`.
unsafe fn ent_client_num(ent: usize) -> c_int {
    // SAFETY: caller guarantees a valid gentity.
    unsafe { read_i32(ent + OFFSET_GENTITY_S + OFFSET_ENTITY_CLIENT_NUM) }
}

/// `ent->client` (the `gclient_t*`), or 0.
///
/// # Safety
///
/// `ent` must be a valid `gentity_t*`.
unsafe fn ent_client(ent: usize) -> usize {
    // SAFETY: caller guarantees a valid gentity.
    unsafe { core::ptr::read_unaligned((ent + OFFSET_GENTITY_CLIENT) as *const usize) }
}

/// `ent->playerState` (the `playerState_t*`), or 0.
///
/// # Safety
///
/// `ent` must be a valid `gentity_t*`.
unsafe fn ent_player_state(ent: usize) -> usize {
    // SAFETY: caller guarantees a valid gentity.
    unsafe { core::ptr::read_unaligned((ent + OFFSET_GENTITY_PLAYER_STATE) as *const usize) }
}

/// `client->ps.stats[index]`.
///
/// # Safety
///
/// `client` must be a valid `gclient_t*`.
unsafe fn ps_stat(client: usize, index: usize) -> c_int {
    // SAFETY: caller guarantees a valid gclient; stats is an int array.
    unsafe { read_i32(client + OFFSET_GCLIENT_PS + OFFSET_PS_STATS + index * 4) }
}

/// `client->ps.persistant[index]`.
///
/// # Safety
///
/// `client` must be a valid `gclient_t*`.
unsafe fn ps_persistant(client: usize, index: usize) -> c_int {
    // SAFETY: caller guarantees a valid gclient; persistant is an int array.
    unsafe { read_i32(client + OFFSET_GCLIENT_PS + OFFSET_PS_PERSISTANT + index * 4) }
}

/// `client->sess.sessionTeam`.
///
/// # Safety
///
/// `client` must be a valid `gclient_t*`.
unsafe fn session_team(client: usize) -> c_int {
    // SAFETY: caller guarantees a valid gclient.
    unsafe { read_i32(client + OFFSET_GCLIENT_SESS + OFFSET_SESS_SESSION_TEAM) }
}

/// Whether the team pair counts as a teamkill/damage (RED or BLUE + same team).
fn is_same_team(a: c_int, b: c_int) -> bool {
    (a == TEAM_RED || a == TEAM_BLUE) && a == b
}

// ---------------------------------------------------------------------------
// G_Damage — stats damage accounting (entry wrapper)
// ---------------------------------------------------------------------------

/// `Proxy_G_Damage` (`Proxy_g_combat.cpp:6-33`): run the pristine damage, then
/// account the delta on the proxy's per-client `gameStats`.
///
/// `vec3_t` parameters decay to `float*` in C (the pristine prototype), so the
/// wrapper receives pointers for `dir`/`point`.
///
/// # Safety
///
/// Entered from the game module with the pristine `G_Damage` argument list.
pub unsafe extern "C" fn g_damage(
    targ: usize,
    inflictor: usize,
    attacker: usize,
    dir: *const f32,
    point: *const f32,
    damage: c_int,
    dflags: c_int,
    mod_: c_int,
) {
    let targ_client = unsafe { ent_client(targ) };
    let attacker_client = unsafe { ent_client(attacker) };
    let targ_redirect = unsafe { read_i32(targ + OFFSET_GENTITY_DAMAGE_REDIRECT) };

    if targ != 0 && targ_client != 0 && attacker != 0 && attacker_client != 0 && targ_redirect == 0
    {
        // SAFETY: both clients are valid gclient_t.
        let total_health = unsafe {
            ps_stat(targ_client, STAT_ARMOR).max(0) + ps_stat(targ_client, STAT_HEALTH).max(0)
        };
        // SAFETY: trampoline attached at GAME_INIT.
        let original = original_call(crate::hooks::game_hook_original_g_damage);
        unsafe { original(targ, inflictor, attacker, dir, point, damage, dflags, mod_) };
        // SAFETY: both clients are valid gclient_t.
        let damages = unsafe {
            total_health
                - ps_stat(targ_client, STAT_ARMOR).max(0)
                - ps_stat(targ_client, STAT_HEALTH).max(0)
        };
        let targ_num = unsafe { ent_client_num(targ) };
        let attacker_num = unsafe { ent_client_num(attacker) };
        if (0..MAX_CLIENTS as c_int).contains(&attacker_num)
            && (0..MAX_CLIENTS as c_int).contains(&targ_num)
        {
            // SAFETY: both clients are valid gclient_t.
            let a_team = unsafe { session_team(attacker_client) };
            let t_team = unsafe { session_team(targ_client) };
            let is_team = is_same_team(t_team, a_team);
            state::with_state(|s| {
                s.clients[attacker_num as usize].game_stats[targ_num as usize].damages_given +=
                    damages;
            });
            state::with_state(|s| {
                s.clients[targ_num as usize].game_stats[attacker_num as usize].damages_taken +=
                    damages;
            });
            if is_team {
                state::with_state(|s| {
                    s.clients[targ_num as usize].team_damages_taken += damages;
                });
                state::with_state(|s| {
                    s.clients[attacker_num as usize].team_damages_given += damages;
                });
            }
        }
    } else {
        // SAFETY: trampoline attached at GAME_INIT.
        let original = original_call(crate::hooks::game_hook_original_g_damage);
        unsafe { original(targ, inflictor, attacker, dir, point, damage, dflags, mod_) };
    }
}

// ---------------------------------------------------------------------------
// player_die — stats kill accounting (entry wrapper)
// ---------------------------------------------------------------------------

/// `Proxy_player_die` (`Proxy_g_combat.cpp:35-55`): pristine death, then
/// account `killed`/`killedBy` (+ team tallies).
///
/// # Safety
///
/// Entered from the game module with the pristine `player_die` argument list.
pub unsafe extern "C" fn player_die(
    self_: usize,
    inflictor: usize,
    attacker: usize,
    damage: c_int,
    means_of_death: c_int,
) {
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::game_hook_original_player_die);
    unsafe { original(self_, inflictor, attacker, damage, means_of_death) };

    let self_client = unsafe { ent_client(self_) };
    let attacker_client = unsafe { ent_client(attacker) };
    if self_ != 0 && self_client != 0 && attacker != 0 && attacker_client != 0 {
        let self_num = unsafe { ent_client_num(self_) };
        let attacker_num = unsafe { ent_client_num(attacker) };
        if (0..MAX_CLIENTS as c_int).contains(&self_num)
            && (0..MAX_CLIENTS as c_int).contains(&attacker_num)
        {
            // SAFETY: both clients are valid gclient_t.
            let a_team = unsafe { session_team(attacker_client) };
            let s_team = unsafe { session_team(self_client) };
            let is_team = is_same_team(s_team, a_team);
            state::with_state(|s| {
                s.clients[attacker_num as usize].game_stats[self_num as usize].killed += 1;
            });
            state::with_state(|s| {
                s.clients[self_num as usize].game_stats[attacker_num as usize].killed_by += 1;
            });
            if is_team {
                state::with_state(|s| {
                    s.clients[self_num as usize].team_killed += 1;
                });
                state::with_state(|s| {
                    s.clients[attacker_num as usize].team_kills += 1;
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// G_AddEvent — anti HP teller (entry wrapper)
// ---------------------------------------------------------------------------

/// `Proxy_G_AddEvent` (`Proxy_g_local.cpp:5-29`): quantise `EV_PAIN`
/// `eventParm` to 24/49/74/100 while `proxy_sv_antiHpTeller` is on.
///
/// # Safety
///
/// Entered from the game module with the pristine `G_AddEvent` argument list.
pub unsafe extern "C" fn g_add_event(ent: usize, event: c_int, mut event_parm: c_int) {
    if event == EV_PAIN {
        let anti_hp =
            state::with_state(|s| s.cvars[crate::state::CVAR_ANTI_HP_TELLER].integer != 0);
        if anti_hp {
            event_parm = if event_parm < 25 {
                24
            } else if event_parm < 50 {
                49
            } else if event_parm < 75 {
                74
            } else {
                100
            };
        }
    }
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::game_hook_original_g_add_event);
    unsafe { original(ent, event, event_parm) };
}

// ---------------------------------------------------------------------------
// ClientThink_real — minimum jump time (entry wrapper)
// ---------------------------------------------------------------------------

/// `Proxy_ClientThink_real` (`Proxy_g_local.cpp:31-67`): hold `upmove` for a
/// minimum duration when `proxy_sv_minJumpTime` is set.
///
/// # Safety
///
/// Entered from the game module with the pristine `ClientThink_real` argument.
pub unsafe extern "C" fn client_think_real(ent: usize) {
    let client_num = {
        let ps = unsafe { ent_player_state(ent) };
        if ps == 0 {
            -1
        } else {
            // SAFETY: ps is a valid playerState_t.
            unsafe { read_i32(ps + OFFSET_PS_CLIENT_NUM) }
        }
    };

    let min_jump_time = state::with_state(|s| s.cvars[crate::state::CVAR_MIN_JUMP_TIME].integer);
    if min_jump_time > 0 && (0..MAX_CLIENTS as c_int).contains(&client_num) {
        let ent_cl = unsafe { ent_client(ent) };
        let gents = g_entities();
        let ent_self_client = if gents != 0 {
            // SAFETY: g_entities is a valid gentity array of the loaded game.
            unsafe { ent_client(gents + client_num as usize * crate::sdk::SIZEOF_GENTITY) }
        } else {
            0
        };
        let is_spectator =
            ent_self_client != 0 && unsafe { session_team(ent_self_client) } == TEAM_SPECTATOR;
        // ent->client->pers.cmd.upmove
        let upmove = if ent_cl != 0 {
            // SAFETY: ent_cl is a valid gclient_t; pers.cmd.upmove is an i8.
            unsafe {
                core::ptr::read_unaligned(
                    (ent_cl + OFFSET_GCLIENT_PERS + OFFSET_PERS_CMD + 26) as *const i8,
                ) as i32
            }
        } else {
            0
        };
        let level_time = if level() != 0 {
            // SAFETY: level is a valid level_locals_t.
            unsafe { read_i32(level() + OFFSET_LEVEL_TIME) }
        } else {
            0
        };

        let set_upmove = |v: i8| {
            if ent_cl != 0 {
                // SAFETY: ent_cl is a valid gclient_t; pers.cmd.upmove is an i8.
                unsafe { *((ent_cl + OFFSET_GCLIENT_PERS + OFFSET_PERS_CMD + 26) as *mut i8) = v };
            }
        };

        if !is_spectator && upmove > 0 {
            state::with_state(|s| {
                if s.jump_start_time[client_num as usize] == 0 {
                    // Player started to jump.
                    s.jump_start_time[client_num as usize] = level_time;
                    set_upmove(127);
                }
            });
        } else {
            let jump_start = state::with_state(|s| s.jump_start_time[client_num as usize]);
            if jump_start != 0 {
                if (level_time - jump_start).abs() < min_jump_time {
                    // Player is scripting, force him jumping a little longer.
                    set_upmove(127);
                } else {
                    // Restart jump timer.
                    state::with_state(|s| s.jump_start_time[client_num as usize] = 0);
                }
            }
        }
    }

    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::game_hook_original_client_think_real);
    unsafe { original(ent) };
}

// ---------------------------------------------------------------------------
// BeginIntermission — stats tables (entry wrapper)
// ---------------------------------------------------------------------------

/// `formatScore(w, l)` (`Proxy_g_cmds.cpp:5-8`): `w/l(+diff) [ratio]` with
/// colors.
fn format_score(w: c_int, l: c_int) -> String {
    format!(
        "{w}/{l}(^3{:+}^7) [^5{:.2}^7]",
        w - l,
        utils::calc_ratio(w, l)
    )
}

/// The `(proxy.g_entities + n)->client` pointer (the `gclient_s*`).
fn client_of(client_num: c_int) -> usize {
    let base = g_entities();
    if base == 0 || !(0..MAX_CLIENTS as c_int).contains(&client_num) {
        return 0;
    }
    // SAFETY: g_entities is a valid gentity array of the loaded game.
    unsafe { ent_client(base + client_num as usize * crate::sdk::SIZEOF_GENTITY) }
}

/// `(proxy.g_entities + n)->client->pers.netname`, colour-stripped into `out`.
fn client_clean_netname(client_num: c_int, out: &mut [u8]) {
    let client = client_of(client_num);
    if client == 0 {
        out[0] = 0;
        return;
    }
    // SAFETY: client is a valid gclient_t; netname is a NUL-terminated string.
    let netname_ptr = client + OFFSET_GCLIENT_PERS + OFFSET_PERS_NETNAME;
    let bytes = unsafe { core::ffi::CStr::from_ptr(netname_ptr as *const c_char) }.to_bytes();
    let copy = out.len().saturating_sub(1).min(bytes.len());
    out[..copy].copy_from_slice(&bytes[..copy]);
    out[copy] = 0;
    utils::strip_color(out);
}

/// The `Q_strncpyz(dst, src, size)` + `%<width>.<width>s` combination: copy at
/// most `size-1` bytes of `src`, then print right-justified in a `width`-wide
/// field (truncating to `width`). Mirrors the original's
/// `Q_strncpyz(kd, formatScore(...), N)` + `va("...%N.Ns...")` chains.
fn printf_str(src: &str, size: usize, width: usize) -> String {
    let bytes = src.as_bytes();
    let cap = size.saturating_sub(1).min(width);
    let len = cap.min(bytes.len());
    let mut out = vec![b' '; width];
    out[width - len..].copy_from_slice(&bytes[..len]);
    // SAFETY: out only contains spaces and the copied bytes (no NULs).
    String::from_utf8(out).expect("stats table row is ASCII")
}

/// `trap_SendServerCommand(clientNum, text)`.
fn send_server_command(client_num: c_int, text: &[u8]) {
    let mut buf = Vec::with_capacity(text.len() + 1);
    buf.extend_from_slice(text);
    buf.push(0);
    // SAFETY: buf has no interior NUL (callers build plain messages).
    let text = unsafe { core::ffi::CStr::from_bytes_with_nul_unchecked(&buf) };
    // SAFETY: engine syscall registered.
    unsafe { syscall::send_server_command(client_num, text) };
}

/// `Proxy_BeginIntermission` (`Proxy_g_cmds.cpp:9-144`): run the pristine
/// intermission, then print the personal/global/best-player stats tables.
///
/// # Safety
///
/// Entered from the game module with no arguments.
pub unsafe extern "C" fn begin_intermission() {
    // SAFETY: trampoline attached at GAME_INIT.
    let original = original_call(crate::hooks::game_hook_original_begin_intermission);
    unsafe { original() };

    // Print per-client stats.
    for viewer in 0..MAX_CLIENTS as c_int {
        let connected = state::with_state(|s| s.clients[viewer as usize].is_connected);
        if !connected {
            continue;
        }
        send_server_command(
            viewer,
            b"print \"========================== PERSONAL STATS ==========================\n           Name   K/D(^3diff.^7) [^5ratio^7]          Damages(^3diff.^7) [^5ratio^7]\n--------------- -------------------- -------------------------------\n\"",
        );
        for opponent in 0..MAX_CLIENTS as c_int {
            let (given, taken) = state::with_state(|s| {
                let gs = &s.clients[viewer as usize].game_stats[opponent as usize];
                (gs.damages_given, gs.damages_taken)
            });
            if given > 0 || taken > 0 {
                let mut client_name = [0u8; MAX_NETNAME];
                client_clean_netname(opponent, &mut client_name);
                let name = String::from_utf8_lossy(&client_name[..utils::c_strlen(&client_name)]);
                let (killed, killed_by) = state::with_state(|s| {
                    let gs = &s.clients[viewer as usize].game_stats[opponent as usize];
                    (gs.killed, gs.killed_by)
                });
                let kd = printf_str(&format_score(killed, killed_by), 28, 28);
                let dmg = printf_str(&format_score(given, taken), 39, 39);
                let row = format!("print \"{}{}{}\n\"", printf_str(&name, 36, 15), kd, dmg);
                send_server_command(viewer, row.as_bytes());
            }
        }
        send_server_command(viewer, b"print \"\n\"");
    }

    // Print global stats.
    let (mut highest_dmg_dealt, mut highest_dmg_dealer_client_num) = (i32::MIN, -1);
    let (mut best_dmg_dealer_diff, mut best_dmg_dealer_client_num) = (i32::MIN, -1);
    let (mut best_fragger_diff, mut best_fragger_client_num) = (i32::MIN, -1);

    send_server_command(
        -1,
        b"print \"======================================= GLOBAL STATS =======================================\n           Name   K/D(^3diff.^7) [^5ratio^7]          Damages(^3diff.^7) [^5ratio^7] Teamkills  Team damages\n--------------- -------------------- ------------------------------- --------- -------------\n\"",
    );

    for client_num in 0..MAX_CLIENTS as c_int {
        if !state::with_state(|s| s.clients[client_num as usize].is_connected) {
            continue;
        }
        let (mut damages_given, damages_taken) = state::with_state(|s| {
            let cl = &s.clients[client_num as usize];
            let mut given = 0i32;
            let mut taken = 0i32;
            for gs in &cl.game_stats {
                given += gs.damages_given;
                taken += gs.damages_taken;
            }
            (given, taken)
        });

        if damages_given > 0 || damages_taken > 0 {
            let client = client_of(client_num);
            let (score, killed) = if client != 0 {
                // SAFETY: client is a valid gclient_t.
                (unsafe { ps_persistant(client, PERS_SCORE) }, unsafe {
                    ps_persistant(client, PERS_KILLED)
                })
            } else {
                (0, 0)
            };
            let kills_diff = score - killed;

            let mut client_name = [0u8; MAX_NETNAME];
            client_clean_netname(client_num, &mut client_name);
            let name = String::from_utf8_lossy(&client_name[..utils::c_strlen(&client_name)]);
            let kd = printf_str(&format_score(score, killed), 28, 28);
            damages_given -=
                state::with_state(|s| s.clients[client_num as usize].team_damages_given);
            let dmg = printf_str(&format_score(damages_given, damages_taken), 39, 39);
            let (team_kills, team_killed, team_dmg_given, team_dmg_taken) =
                state::with_state(|s| {
                    let cl = &s.clients[client_num as usize];
                    (
                        cl.team_kills,
                        cl.team_killed,
                        cl.team_damages_given,
                        cl.team_damages_taken,
                    )
                });
            let tk = printf_str(&format!("{team_kills}/{team_killed}"), 9, 9);
            let tdmg = printf_str(&format!("{team_dmg_given}/{team_dmg_taken}"), 13, 13);
            let row = format!(
                "print \"{}{}{}{}{}\n\"",
                printf_str(&name, 36, 15),
                kd,
                dmg,
                tk,
                tdmg
            );
            send_server_command(-1, row.as_bytes());

            // Update best players.
            let damages_diff = damages_given - damages_taken;
            if damages_diff > best_dmg_dealer_diff {
                best_dmg_dealer_diff = damages_diff;
                best_dmg_dealer_client_num = client_num;
            }
            if damages_given > highest_dmg_dealt {
                highest_dmg_dealt = damages_given;
                highest_dmg_dealer_client_num = client_num;
            }
            if kills_diff > best_fragger_diff {
                best_fragger_diff = kills_diff;
                best_fragger_client_num = client_num;
            }
        }
    }

    // Print best players.
    if best_fragger_client_num >= 0
        || best_dmg_dealer_client_num >= 0
        || highest_dmg_dealer_client_num >= 0
    {
        send_server_command(-1, b"print \"\n\"");
        if best_dmg_dealer_client_num >= 0 {
            let mut name = [0u8; MAX_NETNAME];
            client_clean_netname(best_dmg_dealer_client_num, &mut name);
            let name = String::from_utf8_lossy(&name[..utils::c_strlen(&name)]);
            send_server_command(
                -1,
                format!("print \"MVP: {name} (^3{:+}^7)\n\"", best_dmg_dealer_diff).as_bytes(),
            );
        }
        if highest_dmg_dealer_client_num >= 0 {
            let mut name = [0u8; MAX_NETNAME];
            client_clean_netname(highest_dmg_dealer_client_num, &mut name);
            let name = String::from_utf8_lossy(&name[..utils::c_strlen(&name)]);
            send_server_command(
                -1,
                format!(
                    "print \"Highest damage dealer: {name} (^3{:+}^7)\n\"",
                    highest_dmg_dealt
                )
                .as_bytes(),
            );
        }
        if best_fragger_client_num >= 0 {
            let mut name = [0u8; MAX_NETNAME];
            client_clean_netname(best_fragger_client_num, &mut name);
            let name = String::from_utf8_lossy(&name[..utils::c_strlen(&name)]);
            send_server_command(
                -1,
                format!(
                    "print \"Best fragger: {name} (^3{:+}^7)\n\"",
                    best_fragger_diff
                )
                .as_bytes(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_score_matches_proxy() {
        // `%d/%d(^3%+d^7) [^5%.2f^7]` with colors from q_shared.h.
        assert_eq!(format_score(3, 1), "3/1(^3+2^7) [^53.00^7]");
        assert_eq!(format_score(0, 1), "0/1(^3-1^7) [^50.00^7]");
        assert_eq!(format_score(2, 2), "2/2(^3+0^7) [^51.00^7]");
        assert_eq!(format_score(5, 2), "5/2(^3+3^7) [^52.50^7]");
    }

    #[test]
    fn printf_str_reproduces_strncpyz_then_field() {
        // Q_strncpyz(dst, src, size) + `%<width>.<width>s`.
        assert_eq!(printf_str("abc", 36, 15), "            abc");
        assert_eq!(
            printf_str("abcdefghijklmnopqrstuvwxyz0123456789ABCDEF", 36, 15),
            "abcdefghijklmno"
        );
        // kd: Q_strncpyz size 28 (max 27) then %28.28s.
        assert_eq!(printf_str("12/3(^3+9^7) [^54.00^7]", 28, 28).len(), 28);
        // tk: Com_sprintf size 9 (max 8) then %9.9s → 6 spaces + "2/1".
        assert_eq!(printf_str("2/1", 9, 9), "      2/1");
    }

    #[test]
    fn strip_color_removes_color_escapes() {
        let mut s = *b"^3red^7";
        utils::strip_color(&mut s);
        assert_eq!(&s[..4], b"red\0");
    }
}
