//! Engine syscall ("trap") interface.
//!
//! The engine hands the proxy a variadic cdecl syscall pointer through
//! `dllEntry`. That pointer is what both the proxy itself (at bootstrap, e.g.
//! to read cvars) and — via the C shim in `csrc/syscall_shim.c` — the original
//! game module ultimately reach the engine through.
//!
//! ABI notes:
//! - i386 System V cdecl, ILP32: every argument travels as a 32-bit word, so a
//!   `c_int` (i32) faithfully carries pointers too (the callee reinterprets).
//! - The original proxy's forwarder harvests up to 1 (command) + 16 args and
//!   always re-passes 16 (`Proxy_OriginalAPI_Wrappers.cpp:9-51`). We mirror
//!   that exactly so observable behaviour is identical, including traps that
//!   declare fewer arguments (the engine ignores the surplus words).
//! - Stable Rust cannot *define* a C-variadic function body, so receiving the
//!   variadic trap call happens in C (`jampgame_vm_dllsyscall`); it calls back
//!   into the fixed-arity `jampgame_syscall_forward` below.
//! - The proxy's *own* trap helpers (`trap_Cvar_*`, `trap_Argv`, …) call the
//!   engine with the exact arity each trap needs, exactly like
//!   `Proxy_Translate_SystemCalls.cpp`.

use core::ffi::{CStr, c_int};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sdk::{
    CS_PLAYERS, CS_SCORES1, CS_SCORES2, G_ARGV, G_CVAR_REGISTER, G_CVAR_SET, G_CVAR_UPDATE,
    G_CVAR_VARIABLE_INTEGER_VALUE, G_CVAR_VARIABLE_STRING_BUFFER, G_DROP_CLIENT,
    G_GET_CONFIGSTRING, G_GET_USERCMD, G_GET_USERINFO, G_LOCATE_GAME_DATA, G_NAV_FREE, G_NAV_LOAD,
    G_PRINT, G_SEND_SERVER_COMMAND, G_SET_CONFIGSTRING, G_SET_USERINFO, MAX_CLIENTS, VmCvar,
};

/// The engine syscall pointer as handed to `dllEntry`.
pub type EngineSyscall = unsafe extern "C" fn(c_int, ...) -> c_int;

/// Stored engine syscall pointer, set once by `dllEntry` before any `vmMain`
/// call. Read-only afterwards; only the single engine thread reaches it.
static ENGINE_SYSCALL: AtomicUsize = AtomicUsize::new(0);

/// Serializes unit tests that install a fake process-global engine syscall
/// (`ENGINE_SYSCALL`) and drive traps through the forwarder.
#[cfg(test)]
pub(crate) static TEST_ENGINE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Clear the installed engine syscall after a test (test-only).
#[cfg(test)]
pub(crate) fn clear_test_engine_syscall() {
    ENGINE_SYSCALL.store(0, Ordering::Relaxed);
}

/// Record the syscall pointer delivered by the engine in `dllEntry`.
pub fn set_engine_syscall(syscall: EngineSyscall) {
    ENGINE_SYSCALL.store(syscall as usize, Ordering::Relaxed);
}

/// Retrieve the stored engine syscall pointer.
pub fn engine_syscall() -> Option<EngineSyscall> {
    let raw = ENGINE_SYSCALL.load(Ordering::Relaxed);
    if raw == 0 {
        None
    } else {
        // usize and fn pointers are both pointer-sized; this only reinterprets
        // the bits stored by `set_engine_syscall`.
        Some(unsafe { core::mem::transmute::<usize, EngineSyscall>(raw) })
    }
}

/// Call the engine syscall pointer with a fixed set of arguments. Used by the
/// proxy itself during bootstrap (no game module loaded yet, so the C shim is
/// not involved).
///
/// # Safety
///
/// `args` must describe the arguments the given trap `command` expects, and
/// the engine syscall pointer must have been delivered via `dllEntry`.
pub unsafe fn call_engine(command: c_int, args: &[c_int]) -> c_int {
    let Some(syscall) = engine_syscall() else {
        eprintln!("----- proxy-rs: engine syscall called before dllEntry");
        return -1;
    };
    unsafe {
        match args {
            [] => syscall(command),
            [a0] => syscall(command, *a0),
            [a0, a1] => syscall(command, *a0, *a1),
            [a0, a1, a2] => syscall(command, *a0, *a1, *a2),
            [a0, a1, a2, a3] => syscall(command, *a0, *a1, *a2, *a3),
            [a0, a1, a2, a3, a4] => syscall(command, *a0, *a1, *a2, *a3, *a4),
            _ => {
                eprintln!("----- proxy-rs: unsupported syscall arity for {command}");
                -1
            }
        }
    }
}

/// Whether a `G_SEND_SERVER_COMMAND` team chat must be dropped for
/// `recipient`: only clients of the sender's own instance may receive it.
///
/// `command` is screened on its **bytes**: the message embeds the client's raw
/// chat text, which is a single-byte encoding (a latin-1 `é` is `0xE9`, not
/// UTF-8). Gating this on `CStr::to_str()` skipped the whole interception for
/// every non-UTF-8 message, so accented team chat reached every instance.
///
/// `tffa_on` is the feature read the caller already made for the kill-limit
/// prune and the score rewrite (one read per command).
fn team_chat_dropped(command: &CStr, tffa_on: bool, recipient: c_int) -> bool {
    tffa_on
        && crate::tffa::is_team_chat(command.to_bytes())
        && !crate::tffa::team_chat_visible(crate::tffa::acting_scope(), recipient)
}

/// Fixed-arity continuation of the C variadic shim: forward one harvested trap
/// call (command + up to 16 words) to the engine unchanged — except for the
/// traps the proxy intercepts (`Proxy_OriginalAPI_Wrappers.cpp:24-45`):
/// `G_LOCATE_GAME_DATA` is recorded, `G_GET_USERCMD` is mutated after the
/// engine fills the cmd, `G_NAV_LOAD` frees the previous nav graph before it
/// loads the next one (map-change memory leak), `G_SEND_SERVER_COMMAND`
/// `scores ...` messages are rewritten per viewer while parallel TFFA matches
/// run and gain the TaystJK 15th `deaths` int (`crate::scoreboard`), and
/// `G_SET_CONFIGSTRING` for `CS_SCORES1/2` + `CS_PLAYERS` fans
/// out per-viewer `cs` corrections (own-match mini HUD and own-side client
/// names; another match's players are kept out of the viewer's team overlay).
/// All are then forwarded normally.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jampgame_syscall_forward(
    command: c_int,
    a0: c_int,
    a1: c_int,
    a2: c_int,
    a3: c_int,
    a4: c_int,
    a5: c_int,
    a6: c_int,
    a7: c_int,
    a8: c_int,
    a9: c_int,
    a10: c_int,
    a11: c_int,
    a12: c_int,
    a13: c_int,
    a14: c_int,
    a15: c_int,
) -> c_int {
    match command {
        // `G_LOCATE_GAME_DATA(sharedEntity_t*, int, int, playerState_t*, int)`:
        // record where the game put its data (later hooks read it), then pass
        // the trap through to the engine.
        G_LOCATE_GAME_DATA => {
            crate::shared_api::locate_game_data(a0 as usize, a1, a2, a3 as usize, a4);
        }
        // `G_GET_USERCMD(int clientNum, usercmd_t* cmd)`: forward first (the
        // engine writes the usercmd into *cmd), then sanitise it before the
        // game reads it.
        G_GET_USERCMD => {
            // SAFETY: engine syscall registered (dllEntry ran first); the game
            // passes a valid usercmd pointer it wants filled.
            let response = unsafe {
                engine_syscall_call(
                    command,
                    [
                        a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15,
                    ],
                )
            };
            // SAFETY: a1 is the game's usercmd buffer, just filled by the engine.
            // Only mutate it while the proxy is enabled (master switch); the
            // recording trap above stays active so a runtime re-enable works.
            if crate::state::proxy_enabled() {
                unsafe { crate::shared_api::get_usercmd(a0, a1 as *mut crate::sdk::Usercmd) };
            }
            return response;
        }
        // `G_NAV_LOAD(const char*, int)` (`g_main.c:1034`, the game's only nav
        // load): the engine's `CNavigator::Load` appends every node of the
        // parsed file to `m_nodes` without clearing the previous graph
        // (`navigator.cpp:602-655`), so every map load, map change and
        // `map_restart` would leak the previous map's whole nav graph. The
        // upstream proxy freed it in a `CNavigator::Load` hook; this port has
        // that hook too, but its hooks are attached only *after* `GAME_INIT`
        // is forwarded, i.e. after the game's own `trap_Nav_Load` has already
        // run — so the leak survived every map. Free here, on the game's
        // syscall, which is active from `dllEntry` on: every load (including
        // the first one of a map) starts from an empty graph. `Free` is
        // idempotent, so the engine hook's own free (when it runs) is a no-op.
        G_NAV_LOAD => {
            if crate::state::proxy_enabled() {
                // SAFETY: engine syscall registered; `G_NAV_FREE` takes no
                // arguments (the surplus words are ignored by the engine).
                unsafe {
                    engine_syscall_call(
                        G_NAV_FREE,
                        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                    );
                }
            }
        }
        // `G_SEND_SERVER_COMMAND(int clientNum, const char*)`: rewrite the
        // per-viewer `scores ...` message while parallel TFFA matches run so
        // each match sees only its own roster and scores, append the TaystJK
        // deaths field to every `scores` message, and drop a team
        // chat (`tchat`/`ltchat`) aimed at a viewer of another match. Global
        // chat stays server-wide.
        // Pristine fraglimit notices (`Red|Blue|<name> …HIT_THE_KILL_LIMIT…`)
        // are swallowed under TFFA (companion to the `LogExit` hook — the
        // proxy's own `on_run_frame_post` owns ALL fraglimit endings).
        G_SEND_SERVER_COMMAND => {
            if crate::state::proxy_enabled() && !(a1 as usize as *const core::ffi::c_char).is_null()
            {
                // SAFETY: the game passes a NUL-terminated command string.
                let text = unsafe { CStr::from_ptr(a1 as usize as *const core::ffi::c_char) };
                // One feature read per command: the same gate covers the
                // kill-limit prune, the team-chat prune and the score rewrite
                // below.
                let tffa_on = crate::tffa::enabled();
                // Team chat (`say_team` → `tchat`/`ltchat`) stays inside the
                // sender's instance: G_SayTo already limited it to the sender's
                // team, and this drops the recipients of another match (blue in
                // main no longer reaches blue in tffa1). Global `chat` stays
                // server-wide. The sender is the acting scope the
                // `GAME_CLIENT_COMMAND` forward opened around the game's own
                // `ClientCommand` (`Cmd_Say_f` → `G_Say`), which covers real
                // clients and bots alike; with no scope open the message is
                // left untouched. Screened before — and independently of — the
                // UTF-8 gate below (`team_chat_dropped`).
                if team_chat_dropped(text, tffa_on, a0) {
                    return 0;
                }
                if let Ok(text_str) = text.to_str() {
                    if tffa_on && crate::tffa::is_kill_limit_print(text_str) {
                        return 0;
                    }
                    // The scoreboard rewrite runs on the message the viewer
                    // will actually receive: the parallel-TFFA rewrite first
                    // (it parses the stock 14-int entry layout and substitutes
                    // the viewer's match), then the TaystJK deaths field on
                    // the result, but only for viewers whose cgame advertised
                    // the 15-int parse in its userinfo (`taystJKcgflags`).
                    // Broadcast scores (`-1`) carry no single viewer: they are
                    // fanned out per client with each client's own layout.
                    if a0 < 0 && crate::scoreboard::broadcast_scores(text_str) {
                        return 0;
                    }
                    let mut rewritten: Option<String> = None;
                    if a0 >= 0 && (a0 as usize) < crate::sdk::MAX_CLIENTS {
                        if let Some(scores) = crate::tffa::rewrite_scores_for_viewer(text_str, a0) {
                            // The client groups the roster by its local
                            // CS_PLAYERS team data; re-push the demotions so a
                            // lost/overwritten override cannot show another
                            // match's player on this viewer's RED/BLUE side.
                            crate::tffa::refresh_cs_players_for_scores(a0);
                            rewritten = Some(scores);
                        }
                    }
                    let source = rewritten.as_deref().unwrap_or(text_str);
                    if let Some(with_deaths) = crate::scoreboard::rewrite_command(source, a0) {
                        rewritten = Some(with_deaths);
                    }
                    if let Some(out) = rewritten {
                        if let Ok(cmsg) = std::ffi::CString::new(out) {
                            // SAFETY: engine syscall registered; cmsg outlives
                            // the call (the engine copies into
                            // reliableCommands synchronously).
                            let response = unsafe {
                                engine_syscall_call(
                                    command,
                                    [
                                        a0,
                                        cmsg.as_ptr() as c_int,
                                        a2,
                                        a3,
                                        a4,
                                        a5,
                                        a6,
                                        a7,
                                        a8,
                                        a9,
                                        a10,
                                        a11,
                                        a12,
                                        a13,
                                        a14,
                                        a15,
                                    ],
                                )
                            };
                            return response;
                        }
                    }
                }
            }
        }
        // Trace family (`trap_Trace`/`trap_G2Trace`/`trap_TraceCapsule`, all
        // `SV_Trace(results, start, mins, maxs, end, passEntityNum=a5,
        // contentmask=a6)`): while parallel TFFA matches run, zero `r.contents`
        // of cross-match entities for the duration of this single synchronous
        // trap so they are intangible game-side (no body blocking, no saber
        // clashes, no other instance's door in the way), then restore.
        //
        // `a6` (the trace's contentmask) lets the selection skip entities the
        // engine would skip anyway (`sv_world.cpp:637`), which is what keeps
        // the list short on a map with hundreds of tagged entities.
        //
        // The save list is a reusable `Vec` held in the state: it can grow to
        // whatever the map needs (no fixed cap that could silently leave a door
        // solid), steady play never allocates, and taking it *out* means a
        // nested trace simply finds it empty instead of clobbering this one.
        //
        // The `enabled()` gate is here rather than in `trace_flips` so a server
        // that never turns the feature on pays one cvar read per trace and no
        // state buffer traffic at all.
        crate::sdk::G_TRACE | crate::sdk::G_G2TRACE | crate::sdk::G_TRACECAPSULE => {
            if crate::state::proxy_enabled() && crate::tffa::enabled() {
                let mut flips = crate::state::with_state(|s| core::mem::take(&mut s.tffa.flip_buf));
                let n = crate::tffa::trace_flips(a5, a6, &mut flips);
                if n > 0 {
                    crate::tffa::flip_apply(&flips[..n]);
                }
                // SAFETY: engine syscall registered; words forwarded.
                let response = unsafe {
                    engine_syscall_call(
                        command,
                        [
                            a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15,
                        ],
                    )
                };
                if n > 0 {
                    crate::tffa::flip_restore(&flips[..n]);
                }
                flips.clear();
                crate::state::with_state(|s| s.tffa.flip_buf = flips);
                return response;
            }
        }
        // World-query family (`trap_EntitiesInBox` / `trap_EntityContact` /
        // `trap_EntityContactCapsule`, D-006 extension S9): game code that asks
        // the world what is around it only sees its own instance's tagged
        // entities plus everything shared. The engine still does the whole query
        // — the box test, the brush-model contact test — and the filter is the
        // same rule the `G_Find` link gate applies (a foreign tag is hidden), so
        // a mover, an explosion or a script running in one instance no longer
        // reaches another instance's doors, triggers or items.
        //
        // The scope comes from the acting entity (`G_RunThink`, `G_RunMissile`,
        // `G_UseTargets2`, `player_die`); with no scope open the query is
        // pristine, exactly as before this gate.
        crate::sdk::G_ENTITIES_IN_BOX => {
            if crate::state::proxy_enabled() && crate::tffa::enabled() {
                let scope = crate::tffa::acting_scope();
                if scope != crate::tffa::SCOPE_NONE {
                    // SAFETY: engine syscall registered; words forwarded.
                    let response = unsafe {
                        engine_syscall_call(
                            command,
                            [
                                a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14,
                                a15,
                            ],
                        )
                    };
                    // `a2` is the game's `int *list`, `a3` its `maxcount`.
                    let count = response.min(a3.max(0));
                    // SAFETY: the engine wrote `count` numbers into the game's
                    // own array, which is what the filter compacts in place.
                    return unsafe {
                        crate::tffa::filter_box_list(a2 as usize as *mut c_int, count, scope)
                    };
                }
            }
        }
        crate::sdk::G_ENTITY_CONTACT | crate::sdk::G_ENTITY_CONTACTCAPSULE => {
            if crate::state::proxy_enabled() && crate::tffa::enabled() {
                let scope = crate::tffa::acting_scope();
                if scope == crate::tffa::SCOPE_NONE {
                    // fall through to the pristine call below
                } else {
                    // `a2` is the `const gentity_t *` the contact is tested
                    // against. A foreign instance's entity does not exist for
                    // this query, and the engine test is a pure geometric
                    // predicate, so answering `false` needs no engine call.
                    if a2 != 0 && crate::tffa::link_hides(a2 as usize, scope) {
                        return 0;
                    }
                    // SAFETY: engine syscall registered; words forwarded.
                    return unsafe {
                        engine_syscall_call(
                            command,
                            [
                                a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14,
                                a15,
                            ],
                        )
                    };
                }
            }
        }
        // `G_SET_CONFIGSTRING(int num, const char*)`: forward first (engine
        // updates `sv.configstrings` + broadcasts `cs` to all), then fan out
        // per-viewer `cs` corrections while TFFA runs so each match gets its
        // own mini-HUD scores and its own team-overlay client names. The
        // corrections arrive after the broadcast and win per client.
        G_SET_CONFIGSTRING => {
            if crate::state::proxy_enabled()
                && (a0 == CS_SCORES1
                    || a0 == CS_SCORES2
                    || (CS_PLAYERS..CS_PLAYERS + MAX_CLIENTS as c_int).contains(&a0))
            {
                let ptr = a1 as usize as *const core::ffi::c_char;
                let value: Option<String> = if ptr.is_null() {
                    Some(String::new())
                } else {
                    // SAFETY: game passes a NUL-terminated configstring.
                    let cstr = unsafe { CStr::from_ptr(ptr) };
                    Some(cstr.to_string_lossy().into_owned())
                };
                // SAFETY: engine syscall registered; words forwarded.
                let response = unsafe {
                    engine_syscall_call(
                        command,
                        [
                            a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15,
                        ],
                    )
                };
                if let Some(value) = value {
                    if a0 == CS_SCORES1 || a0 == CS_SCORES2 {
                        // The engine just broadcast the global tally to every
                        // HUD: correct per viewer against the broadcast value
                        // (plain `on_set_cs_scores()` would dedup-skip the
                        // bystanders whose own tally did not move, leaving
                        // them stuck on the killer's bump).
                        crate::tffa::on_set_cs_scores_broadcast(a0, &value);
                    } else {
                        crate::tffa::on_set_cs_players(a0 - CS_PLAYERS, &value);
                    }
                }
                return response;
            }
        }
        _ => {}
    }

    match engine_syscall() {
        Some(syscall) => unsafe {
            syscall(
                command, a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15,
            )
        },
        None => {
            eprintln!("----- proxy-rs: syscall {command} before dllEntry");
            -1
        }
    }
}

/// Call the engine syscall pointer with exactly the given words (used by the
/// `G_GET_USERCMD` forwarding path and by nothing else directly).
///
/// # Safety
///
/// The engine syscall pointer must have been delivered via `dllEntry`.
#[inline]
unsafe fn engine_syscall_call(command: c_int, words: [c_int; 16]) -> c_int {
    let Some(syscall) = engine_syscall() else {
        eprintln!("----- proxy-rs: syscall {command} before dllEntry");
        return -1;
    };
    unsafe {
        syscall(
            command, words[0], words[1], words[2], words[3], words[4], words[5], words[6],
            words[7], words[8], words[9], words[10], words[11], words[12], words[13], words[14],
            words[15],
        )
    }
}

// ---------------------------------------------------------------------------
// Proxy-side trap helpers (Proxy_Translate_SystemCalls.cpp subset)
// ---------------------------------------------------------------------------

/// Fill `buffer` with the value of cvar `name`
/// (`G_CVAR_VARIABLE_STRING_BUFFER`, `trap_Cvar_VariableStringBuffer`).
///
/// # Safety
///
/// The engine syscall pointer must be registered; `buffer` is written by the
/// engine and NUL-terminated.
pub unsafe fn cvar_variable_string_buffer(name: &CStr, buffer: &mut [u8]) {
    unsafe {
        call_engine(
            G_CVAR_VARIABLE_STRING_BUFFER,
            &[
                name.as_ptr() as c_int,
                buffer.as_mut_ptr() as c_int,
                buffer.len() as c_int,
            ],
        );
    }
}

/// Integer value of cvar `name` (`trap_Cvar_VariableIntegerValue`).
///
/// # Safety
///
/// The engine syscall pointer must be registered.
pub unsafe fn cvar_variable_integer_value(name: &CStr) -> c_int {
    unsafe { call_engine(G_CVAR_VARIABLE_INTEGER_VALUE, &[name.as_ptr() as c_int]) }
}

/// `trap_Cvar_Register` — register/re-sync a `vmCvar_t` mirror.
///
/// # Safety
///
/// `cvar` must point to a live `VmCvar` the engine writes through; `name` and
/// `default_value` must be NUL-terminated.
pub unsafe fn cvar_register(cvar: *mut VmCvar, name: &CStr, default_value: &CStr, flags: u32) {
    unsafe {
        call_engine(
            G_CVAR_REGISTER,
            &[
                cvar as usize as c_int,
                name.as_ptr() as c_int,
                default_value.as_ptr() as c_int,
                flags as c_int,
            ],
        );
    }
}

/// `trap_Cvar_Update` — refresh a `vmCvar_t` mirror from the engine.
///
/// # Safety
///
/// `cvar` must point to a live `VmCvar` the engine writes through.
pub unsafe fn cvar_update(cvar: *mut VmCvar) {
    unsafe {
        call_engine(G_CVAR_UPDATE, &[cvar as usize as c_int]);
    }
}

/// `trap_Cvar_Set` — set cvar `name` to `value` engine-side
/// (`G_CVAR_SET`, `trap_Cvar_Set`).
///
/// # Safety
///
/// The engine syscall pointer must be registered; `name` and `value` must be
/// NUL-terminated.
pub unsafe fn cvar_set(name: &CStr, value: &CStr) {
    unsafe {
        call_engine(
            G_CVAR_SET,
            &[name.as_ptr() as c_int, value.as_ptr() as c_int],
        );
    }
}

/// `trap_GetConfigstring` — copy server configstring `index` into `buffer`
/// (`G_GET_CONFIGSTRING`).
///
/// # Safety
///
/// The engine syscall pointer must be registered; `buffer` is written by the
/// engine and NUL-terminated.
pub unsafe fn get_configstring(index: c_int, buffer: &mut [u8]) {
    unsafe {
        call_engine(
            G_GET_CONFIGSTRING,
            &[index, buffer.as_mut_ptr() as c_int, buffer.len() as c_int],
        );
    }
}

/// `trap_Print` — write `text` to the server console.
///
/// Inside an rcon packet the engine is redirecting `Com_Printf` into its reply
/// buffer (`SVC_RemoteCommand` → `Com_BeginRedirect`), so this is how an
/// `rcon` command answers the admin. Outside one it lands on the dedicated
/// console.
///
/// # Safety
///
/// `text` must be NUL-terminated; the engine syscall pointer must be registered.
pub unsafe fn print(text: &CStr) {
    unsafe {
        call_engine(G_PRINT, &[text.as_ptr() as c_int]);
    }
}

/// `trap_Argv` — copy command argument `n` into `buffer`.
///
/// # Safety
///
/// The engine syscall pointer must be registered; `buffer` is written by the
/// engine and NUL-terminated.
pub unsafe fn argv(n: c_int, buffer: &mut [u8]) {
    unsafe {
        call_engine(
            G_ARGV,
            &[n, buffer.as_mut_ptr() as c_int, buffer.len() as c_int],
        );
    }
}

/// `trap_DropClient` — kick `client_num` with `reason`.
///
/// # Safety
///
/// `reason` must be NUL-terminated; the engine syscall pointer must be
/// registered.
pub unsafe fn drop_client(client_num: c_int, reason: &CStr) {
    unsafe {
        call_engine(G_DROP_CLIENT, &[client_num, reason.as_ptr() as c_int]);
    }
}

/// `trap_SendServerCommand` — reliably send `text` to `client_num` (`-1` = all),
/// skipping messages longer than 1022 chars (guard from
/// `Proxy_Translate_SystemCalls.cpp:102-107`).
///
/// # Safety
///
/// `text` must be NUL-terminated; the engine syscall pointer must be
/// registered.
pub unsafe fn send_server_command(client_num: c_int, text: &CStr) {
    let bytes = text.to_bytes();
    if bytes.len() > 1022 {
        return;
    }
    unsafe {
        call_engine(G_SEND_SERVER_COMMAND, &[client_num, text.as_ptr() as c_int]);
    }
}

/// `trap_GetUserinfo` — copy client `num`'s userinfo into `buffer`.
///
/// # Safety
///
/// The engine syscall pointer must be registered; `buffer` is written by the
/// engine and NUL-terminated.
pub unsafe fn get_userinfo(num: c_int, buffer: &mut [u8]) {
    unsafe {
        call_engine(
            G_GET_USERINFO,
            &[num, buffer.as_mut_ptr() as c_int, buffer.len() as c_int],
        );
    }
}

/// `trap_SetUserinfo` — replace client `num`'s userinfo.
///
/// # Safety
///
/// `buffer` must be NUL-terminated; the engine syscall pointer must be
/// registered.
pub unsafe fn set_userinfo(num: c_int, buffer: &CStr) {
    unsafe {
        call_engine(G_SET_USERINFO, &[num, buffer.as_ptr() as c_int]);
    }
}

/// Read a NUL-terminated string the engine wrote into `buffer`, returning the
/// bytes before the terminator.
pub fn read_engine_string(buffer: &[u8]) -> &[u8] {
    let n = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    &buffer[..n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Fixed-arity shape of the variadic engine syscall (command + the 16
    /// words `jampgame_syscall_forward` always re-passes).
    type FixedSyscall = unsafe extern "C" fn(
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
    ) -> c_int;

    #[test]
    fn engine_syscall_round_trips() {
        let _guard = TEST_ENGINE_LOCK.lock().unwrap();
        // A real code pointer with a compatible cdecl word layout: transmuting
        // between fn-pointer types keeps a valid pointer, so no invalid-value
        // lint fires (a bare `0usize as fn` would be UB).
        unsafe extern "C" fn marker(
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
            _: c_int,
        ) -> c_int {
            0
        }
        // Fixed-arity cdecl fn (a valid code pointer), re-typed as the
        // variadic syscall pointer: fn-pointer-to-fn-pointer transmute keeps a
        // valid pointer, so no invalid-value lint fires.
        let fixed: FixedSyscall = marker;
        let marker: EngineSyscall = unsafe { core::mem::transmute(fixed) };
        set_engine_syscall(marker);
        assert!(matches!(engine_syscall(), Some(f) if f as usize == marker as usize));
        clear_test_engine_syscall();
        assert!(engine_syscall().is_none());
    }

    /// Commands the recorder below received, in order.
    static RECORDED: Mutex<Vec<c_int>> = Mutex::new(Vec::new());

    /// Stand-in engine syscall: records every trap and answers `G_NAV_LOAD`
    /// with a recognizable non-zero value so the forwarding path is visible.
    unsafe extern "C" fn recording_syscall(
        command: c_int,
        _a0: c_int,
        _a1: c_int,
        _a2: c_int,
        _a3: c_int,
        _a4: c_int,
        _a5: c_int,
        _a6: c_int,
        _a7: c_int,
        _a8: c_int,
        _a9: c_int,
        _a10: c_int,
        _a11: c_int,
        _a12: c_int,
        _a13: c_int,
        _a14: c_int,
        _a15: c_int,
    ) -> c_int {
        RECORDED.lock().unwrap().push(command);
        if command == G_NAV_LOAD { 7 } else { 0 }
    }

    #[test]
    fn nav_load_frees_the_previous_nav_graph_first() {
        let _guard = TEST_ENGINE_LOCK.lock().unwrap();
        RECORDED.lock().unwrap().clear();
        let fixed: FixedSyscall = recording_syscall;
        let recorder: EngineSyscall = unsafe { core::mem::transmute(fixed) };
        set_engine_syscall(recorder);
        // The leak fix must be on by default (same state as a running proxy
        // before the first cvar refresh).
        assert!(crate::state::proxy_enabled());
        // SAFETY: the recorder stands in for the engine syscall; the arguments
        // are the ones the game passes for `trap_Nav_Load`.
        let response = unsafe {
            jampgame_syscall_forward(G_NAV_LOAD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
        };
        assert_eq!(response, 7, "G_NAV_LOAD's result must be forwarded");
        assert_eq!(
            *RECORDED.lock().unwrap(),
            vec![G_NAV_FREE, G_NAV_LOAD],
            "the old nav graph is freed before the new one loads"
        );
        clear_test_engine_syscall();
        RECORDED.lock().unwrap().clear();
    }

    /// A team chat whose text is not valid UTF-8 (any latin-1 accent, e.g. a
    /// French `é` = 0xE9) must be gated like every other team chat. The
    /// `G_SEND_SERVER_COMMAND` arm used to screen inside `CStr::to_str()`, so a
    /// message that was not UTF-8 skipped the interception entirely and reached
    /// every instance.
    #[test]
    fn non_utf8_team_chat_stays_inside_the_senders_instance() {
        use std::ffi::CString;

        // The same shape `G_SayTo` builds (`g_cmds.c:1511-1522`), latin-1 text.
        let chat = CString::new(b"tchat \"\x19(alpha^7\x19)\x19: ^5CAF\xe9\"".to_vec()).unwrap();
        let text = chat.as_c_str();
        assert!(
            text.to_str().is_err(),
            "the payload must not be valid UTF-8 for this test to mean anything"
        );
        crate::state::with_state(|s| {
            s.tffa.client_match[0] = 1;
            s.tffa.client_match[1] = crate::tffa::MAIN_MATCH;
        });
        let previous = crate::tffa::open_client_scope(0);
        assert_eq!(previous, crate::tffa::SCOPE_NONE);
        // Recipient 1 is in main: dropped.
        assert!(team_chat_dropped(text, true, 1));
        // Recipient 0 is in the sender's instance: delivered.
        assert!(!team_chat_dropped(text, true, 0));
        // Broadcast or no scope: the fail-open rule of every TFFA gate.
        assert!(!team_chat_dropped(text, true, -1));
        crate::tffa::close_acting_scope(previous);
        assert!(!team_chat_dropped(text, true, 1));
        // Feature off: untouched, no state read.
        assert!(!team_chat_dropped(text, false, 1));
        crate::state::with_state(|s| {
            s.tffa.client_match[0] = crate::tffa::MAIN_MATCH;
            s.tffa.client_match[1] = crate::tffa::MAIN_MATCH;
        });
    }
}
