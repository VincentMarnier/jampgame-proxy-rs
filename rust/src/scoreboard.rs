//! TaystJK scoreboard deaths (server-authoritative K/D on the tab scoreboard).
//!
//! TaystJK's cgame appends a 15th per-client int to the server's `scores`
//! command when the serverinfo key `taystJKinfo` carries bit 7
//! (`TAYSTJK_INFO_SCORE_DEATHS`, `bg_public.h:557`) and displays it as the
//! player's deaths (`cg_servercmds.c:58-67`, `cg_scoreboard.c:186-195`). The
//! stock game module writes 14 ints per entry
//! (`g_cmds.c:DeathmatchScoreboardMessage`) and never advertises the key.
//!
//! The proxy supplies both halves for any loaded game module:
//!
//! - it registers a `taystJKinfo` cvar flagged `CVAR_SERVERINFO` (so the
//!   engine includes it in `CS_SERVERINFO`) and keeps bit 7 set while the
//!   master switch is on, leaving any other module's bits untouched;
//! - it appends `cl->ps.persistant[PERS_KILLED]` — the game's own death count,
//!   read at send time — as the 15th int of every `scores` command.
//!
//! The key is server-wide, so every client parses the 15-int layout while the
//! proxy is enabled. A cgame that ignores the key (stock JKA, JA+, jaPRO)
//! would misparse it, and no userinfo field reliably identifies a TaystJK
//! cgame: TaystJK sends `cjp_client=1.4JAPRO`, which JA+/jaPRO clients send
//! too. Running a mixture of cgames is therefore out of scope for this
//! feature; a server that needs stock clients turns `proxy_sv_enable` off.

use core::ffi::{CStr, c_int};

use crate::sdk::{MAX_STRING_CHARS, VmCvar};
use crate::state;

/// `TAYSTJK_INFO_SCORE_DEATHS` (`bg_public.h:557`): the server sends a 15th
/// per-client scoreboard int (deaths). The only bit this module advertises.
pub const TAYSTJK_INFO_SCORE_DEATHS: i32 = 1 << 7;

/// Serverinfo key the TaystJK cgame reads (`cg_servercmds.c:250`).
pub const SERVERINFO_CVAR: &CStr = c"taystJKinfo";

/// Per-entry int count of a pristine `scores` payload.
const BASE_ENTRY_INTS: usize = 14;
/// Per-entry int count once the deaths field is present.
const WITH_DEATHS_ENTRY_INTS: usize = 15;
/// `client->reliableCommands` slots hold `MAX_STRING_CHARS` bytes and the
/// engine copies with `Q_strncpyz`, so a longer command is truncated
/// (`sv_main.cpp:SV_AddServerCommand`). Leave room for the NUL.
const MAX_COMMAND_LEN: usize = MAX_STRING_CHARS - 1;
/// Upper bound of `"scores <count> <red> <blue>"` (three i32 fields plus the
/// keyword and separators) used to keep appended entries inside the budget.
const MAX_HEADER_LEN: usize = 64;

/// Refresh the `taystJKinfo` mirror and advertise bit 7 while the proxy is
/// enabled. Called once per `GAME_RUN_FRAME` (after the proxy cvars were
/// refreshed) and at `GAME_INIT`.
pub fn sync_serverinfo() {
    set_advertised(state::proxy_enabled());
}

/// Clear bit 7 at `GAME_SHUTDOWN`: the cvar table outlives a game-module
/// switch, and a server that leaves the proxy must not keep advertising the
/// 15-int layout to clients of a pristine module.
pub fn clear_serverinfo() {
    set_advertised(false);
}

/// Keep bit 7 in sync without disturbing the bits another game module may
/// advertise in the same key.
fn set_advertised(wanted: bool) {
    let (current, target) = state::with_state(|s| {
        // SAFETY: `taystjk_info` is a live VmCvar the engine writes through.
        unsafe { crate::syscall::cvar_update(&mut s.taystjk_info as *mut _) };
        let current = s.taystjk_info.integer;
        let target = if wanted {
            current | TAYSTJK_INFO_SCORE_DEATHS
        } else {
            current & !TAYSTJK_INFO_SCORE_DEATHS
        };
        (current, target)
    });
    if target == current {
        return;
    }
    let value = std::ffi::CString::new(target.to_string()).expect("integer has no NUL");
    // SAFETY: engine syscall registered; `value` is NUL-terminated.
    unsafe { crate::syscall::cvar_set(SERVERINFO_CVAR, &value) };
}

/// Register the `taystJKinfo` mirror with the engine.
///
/// # Safety
///
/// The engine syscall pointer must be registered (`dllEntry` ran) and `cvar`
/// must point to a live `VmCvar` that outlives the game module.
pub unsafe fn register_cvar(cvar: *mut VmCvar) {
    unsafe {
        crate::syscall::cvar_register(
            cvar,
            SERVERINFO_CVAR,
            c"0",
            crate::sdk::CVAR_SERVERINFO | crate::sdk::CVAR_ROM,
        );
    }
}

/// Rewrite a `scores ...` server command with the deaths field, or `None` when
/// the command is not a scores message, the feature is off, or the layout is
/// not recognisable (the caller then forwards the original untouched).
pub fn rewrite_command(text: &str) -> Option<String> {
    if !state::proxy_enabled() {
        return None;
    }
    append_deaths(text, crate::hooks::game::pers_killed)
}

/// Pure `scores` rewrite: turn 14-int entries into 15-int entries whose last
/// field is `deaths_of(client_number)`. The header's count is set to the
/// number of entries actually emitted, and entries are dropped from the tail
/// when the 15-int layout would overflow the engine's reliable-command budget
/// (a truncated deaths field would shift the next entry's fields).
fn append_deaths(text: &str, deaths_of: impl Fn(c_int) -> c_int) -> Option<String> {
    let body = text.strip_prefix("scores ")?;
    let mut nums: Vec<i32> = Vec::new();
    for token in body.split_whitespace() {
        nums.push(token.parse::<i32>().ok()?);
    }
    if nums.len() < 3 {
        return None;
    }
    let entries = &nums[3..];
    if entries.is_empty() {
        return None;
    }
    // Already carries deaths (a jaPRO/JA+ game module appended them), or an
    // unknown layout: leave the message alone.
    if entry_width(nums[0], entries.len())? != BASE_ENTRY_INTS {
        return None;
    }

    let mut payload = String::with_capacity(entries.len() * 4);
    let mut written = 0i32;
    for chunk in entries.chunks(BASE_ENTRY_INTS) {
        let mut piece = String::with_capacity(1 + BASE_ENTRY_INTS * 4);
        for value in chunk {
            piece.push(' ');
            piece.push_str(&value.to_string());
        }
        piece.push(' ');
        piece.push_str(&deaths_of(chunk[0]).to_string());
        if payload.len() + piece.len() + MAX_HEADER_LEN > MAX_COMMAND_LEN {
            break;
        }
        payload.push_str(&piece);
        written += 1;
    }
    if written == 0 {
        return None;
    }
    Some(format!("scores {written} {} {}{payload}", nums[1], nums[2]))
}

/// Per-entry int count of a `scores` payload, from the header count when it is
/// consistent, otherwise from the only layout that divides the entry list.
///
/// The header count is `level.numConnectedClients`, which can exceed the
/// entries actually sent: the game stops appending once its 1022-byte entry
/// buffer would overflow. Ambiguous sizes (a multiple of 14*15) with an
/// inconsistent count are left alone rather than risk treating a jaPRO/JA+
/// deaths field as a stock entry.
fn entry_width(count: i32, len: usize) -> Option<usize> {
    if (1..=crate::sdk::MAX_CLIENTS as i32).contains(&count) {
        let count = count as usize;
        if count * WITH_DEATHS_ENTRY_INTS == len {
            return Some(WITH_DEATHS_ENTRY_INTS);
        }
        if count * BASE_ENTRY_INTS == len {
            return Some(BASE_ENTRY_INTS);
        }
    }
    match (
        len % BASE_ENTRY_INTS == 0,
        len % WITH_DEATHS_ENTRY_INTS == 0,
    ) {
        (true, false) => Some(BASE_ENTRY_INTS),
        (false, true) => Some(WITH_DEATHS_ENTRY_INTS),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One pristine 14-int entry (the game's own field order).
    fn entry(client: i32, score: i32) -> String {
        format!("{client} {score} 30 1 0 0 50 0 0 0 0 0 0 0")
    }

    fn nums(text: &str) -> Vec<i32> {
        text.strip_prefix("scores ")
            .unwrap()
            .split_whitespace()
            .map(|token| token.parse().unwrap())
            .collect()
    }

    #[test]
    fn appends_deaths_as_the_fifteenth_int() {
        let text = format!("scores 2 5 3 {} {}", entry(0, 10), entry(1, 7));
        let out = append_deaths(&text, |client| if client == 0 { 4 } else { 9 }).unwrap();
        let nums = nums(&out);

        assert_eq!(&nums[..3], &[2, 5, 3]);
        assert_eq!(nums.len(), 3 + 2 * WITH_DEATHS_ENTRY_INTS);
        assert_eq!(nums[3 + 1], 10); // score untouched
        assert_eq!(nums[3 + 14], 4); // first entry's deaths
        assert_eq!(nums[3 + 15 + 1], 7); // second entry's score
        assert_eq!(nums[3 + 15 + 14], 9); // second entry's deaths
    }

    #[test]
    fn deaths_are_read_per_entry_client() {
        let text = format!(
            "scores 3 0 0 {} {} {}",
            entry(5, 1),
            entry(2, 3),
            entry(9, 2)
        );
        let out = append_deaths(&text, |client| client * 10).unwrap();
        let nums = nums(&out);
        assert_eq!(nums[3 + 14], 50);
        assert_eq!(nums[3 + 15 + 14], 20);
        assert_eq!(nums[3 + 30 + 14], 90);
    }

    #[test]
    fn an_already_fifteen_int_entry_is_left_alone() {
        let text = format!("scores 1 0 0 {} 4", entry(0, 10));
        assert!(append_deaths(&text, |_| 4).is_none());
    }

    #[test]
    fn non_scores_and_bad_layouts_are_left_alone() {
        assert!(append_deaths("print \"hi\"", |_| 0).is_none());
        assert!(append_deaths("scores", |_| 0).is_none());
        assert!(append_deaths("scores 1 0 0 1 2 3", |_| 0).is_none());
        // An empty roster has nothing to append.
        assert!(append_deaths("scores 0 0 0", |_| 0).is_none());
        // 16 ints: neither 14 nor 15 per entry.
        assert!(
            append_deaths("scores 2 0 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16", |_| 0).is_none()
        );
    }

    #[test]
    fn a_count_above_the_entry_list_still_resolves_the_layout() {
        // The game sends `level.numConnectedClients` as the count but stops
        // adding entries at its 1022-byte budget.
        let text = format!("scores 3 1 2 {}", entry(0, 10));
        let out = append_deaths(&text, |_| 7).unwrap();
        let nums = nums(&out);
        assert_eq!(nums[0], 1); // count now matches the single emitted entry
        assert_eq!(nums[1], 1);
        assert_eq!(nums[2], 2);
        assert_eq!(nums[3 + 14], 7);
    }

    #[test]
    fn the_command_stays_inside_the_engine_reliable_buffer() {
        // 32 entries would need over 1023 bytes with the extra int: the tail
        // is dropped whole so every emitted entry stays parseable, and the
        // header count follows what was emitted.
        let entries: Vec<String> = (0..32).map(|n| entry(n, n)).collect();
        let text = format!("scores 32 0 0 {}", entries.join(" "));
        let out = append_deaths(&text, |_| 123).unwrap();
        assert!(out.len() <= MAX_COMMAND_LEN, "{} bytes", out.len());
        let nums = nums(&out);
        let emitted = nums[0] as usize;
        assert!(emitted > 0 && emitted < 32);
        assert_eq!(nums.len(), 3 + emitted * WITH_DEATHS_ENTRY_INTS);
        // Every emitted entry keeps its full 15-int shape.
        for i in 0..emitted {
            assert_eq!(nums[3 + i * 15], i as i32);
            assert_eq!(nums[3 + i * 15 + 14], 123);
        }
    }

    #[test]
    fn ambiguous_entry_counts_are_left_alone() {
        // 210 ints = 14*15: with a count that matches neither, the layout is
        // unknown, so the message is untouched (never double-append).
        let text = format!(
            "scores 99 0 0 {}",
            (0..15).map(|_| entry(0, 1)).collect::<Vec<_>>().join(" ")
        );
        assert!(append_deaths(&text, |_| 0).is_none());
    }
}
