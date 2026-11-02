//! Shared client-number / player-name target resolution.
//!
//! The rcon `mute`/`unmute` commands (D-011) and the player `/ignore`/
//! `/unignore` commands (D-016) accept the same target shape: a whole decimal
//! word is a client slot, anything else a player name matched against the live
//! roster in three tiers. Keeping one resolver means both commands answer a bad
//! or ambiguous target the same way, and the rules cannot drift apart.
//!
//! The roster is read from the live game state (`g_entities`), with the proxy's
//! connect mirror as the fallback before the world is located. A slot is a
//! player when `pers.connected != CON_DISCONNECTED`; bots are included because
//! the game module adds them without raising `GAME_CLIENT_BEGIN`.

use core::ffi::c_int;

use crate::hooks::engine_sv::{read_i32, read_usize};
use crate::sdk::{
    CON_DISCONNECTED, MAX_CLIENTS, MAX_NETNAME, MAX_TOKEN_CHARS, OFFSET_GCLIENT_PERS,
    OFFSET_GENTITY_CLIENT, OFFSET_PERS_CONNECTED, SIZEOF_GENTITY,
};
use crate::state::{self, ClientEntry};
use crate::syscall;
use crate::utils;

/// How many candidates an ambiguity reply lists. Each printed rcon line is its
/// own packet and the engine's redirect buffer is 1008 bytes
/// (`SVC_RemoteCommand`'s `sv_outputbuf`), so a full 32-slot list would be
/// truncated mid-name. A client `print` has the same practical limit.
const MAX_LISTED_CANDIDATES: usize = 8;

/// What a `mute`/`unmute`/`ignore`/`unignore` argument selected.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// A connected client slot.
    Slot(usize),
    /// A decimal argument naming a slot that holds no player (or no slot at
    /// all).
    EmptySlot(usize),
    /// No connected player has a name matching the argument.
    UnknownName,
    /// The argument is empty once colour codes are removed (nothing to match).
    EmptyName,
    /// Several connected players match; the slots, in slot order.
    Ambiguous(Vec<usize>),
}

/// Per-slot connection state, mute flag and normalised (colour-stripped) name,
/// taken once so resolution, reporting and the following write see the same
/// roster.
pub(crate) struct Census {
    pub connected: [bool; MAX_CLIENTS],
    /// Admin-mute flags carried along for the `mute` usage/list reply (D-011);
    /// the target resolver itself does not use them.
    pub muted: [bool; MAX_CLIENTS],
    pub names: [[u8; MAX_NETNAME]; MAX_CLIENTS],
}

/// Read the live roster.
pub(crate) fn census() -> Census {
    let g_entities = state::with_state(|s| s.located_game_data.g_entities);

    // Before the world is located (bootstrap window) the connect mirror is all
    // there is.
    if g_entities == 0 {
        return state::with_state(|s| census_from(&s.clients));
    }

    let muted: [bool; MAX_CLIENTS] =
        state::with_state(|s| core::array::from_fn(|i| s.clients[i].muted));
    let mut census = Census {
        connected: [false; MAX_CLIENTS],
        muted,
        names: [[0u8; MAX_NETNAME]; MAX_CLIENTS],
    };

    // Live game state, like `tffa::connected_clients`: bots are added by the
    // game module itself and never raise `GAME_CLIENT_BEGIN`, so the proxy's
    // own mirror does not see them.
    for slot in 0..MAX_CLIENTS {
        let entity = g_entities + slot * SIZEOF_GENTITY;
        // SAFETY: `entity` is inside the located g_entities array.
        let client = unsafe { read_usize(entity + OFFSET_GENTITY_CLIENT) };
        // SAFETY: a client slot's `pers.connected` is a plain int.
        census.connected[slot] = client != 0
            && unsafe { read_i32(client + OFFSET_GCLIENT_PERS + OFFSET_PERS_CONNECTED) }
                != CON_DISCONNECTED;
        if census.connected[slot] {
            crate::hooks::game::client_clean_netname(slot as c_int, &mut census.names[slot]);
        }
    }
    census
}

/// The connect-mirror census (`Proxy_SharedAPI_ClientBegin`'s `is_connected` and
/// the mirrored, sanitised userinfo name). Used before the world is located and
/// by the unit tests; [`census`] prefers the live game memory.
pub(crate) fn census_from(clients: &[ClientEntry]) -> Census {
    let mut census = Census {
        connected: [false; MAX_CLIENTS],
        muted: [false; MAX_CLIENTS],
        names: [[0u8; MAX_NETNAME]; MAX_CLIENTS],
    };
    for (slot, entry) in clients.iter().take(MAX_CLIENTS).enumerate() {
        census.connected[slot] = entry.is_connected;
        census.muted[slot] = entry.muted;
        if entry.is_connected {
            utils::strncpyz(
                &mut census.names[slot],
                syscall::read_engine_string(&entry.clean_name),
            );
            utils::strip_color(&mut census.names[slot]);
        }
    }
    census
}

/// Resolve a target argument against the census.
///
/// A decimal word is a client slot (`mute 0` is the engine's own clientNum, the
/// number `status` and the scoreboard use). Anything else is a player name,
/// matched against the colour-stripped `pers.netname` in three tiers:
/// exact, then prefix, then substring — case-insensitive, with colour codes
/// ignored on both sides. The first tier that matches decides, so `darth` picks
/// "Darth" over "Darth Vader", and `ade` picks "Darth Vader". Several players in
/// the winning tier are reported rather than guessed.
pub(crate) fn resolve(target: &[u8], census: &Census) -> Target {
    if let Some(slot) = parse_slot(target) {
        return if slot < MAX_CLIENTS && census.connected[slot] {
            Target::Slot(slot)
        } else {
            Target::EmptySlot(slot)
        };
    }
    match_name(target, census)
}

/// A whole decimal word as a client slot number (`"0"`), or `None` when the
/// argument is not a plain non-negative number and must be a name.
fn parse_slot(target: &[u8]) -> Option<usize> {
    let text = core::str::from_utf8(target).ok()?;
    // `u32` covers every real slot; a longer number is out of range anyway and
    // is reported as an empty slot.
    text.parse::<u32>().ok().map(|slot| slot as usize)
}

fn match_name(target: &[u8], census: &Census) -> Target {
    let mut wanted = [0u8; MAX_NETNAME];
    utils::strncpyz(&mut wanted, target);
    utils::strip_color(&mut wanted);
    let wanted = syscall::read_engine_string(&wanted);
    if wanted.is_empty() {
        return Target::EmptyName;
    }

    let mut exact = Vec::new();
    let mut prefix = Vec::new();
    let mut substring = Vec::new();
    for slot in 0..MAX_CLIENTS {
        if !census.connected[slot] {
            continue;
        }
        let name = syscall::read_engine_string(&census.names[slot]);
        if name.eq_ignore_ascii_case(wanted) {
            exact.push(slot);
        } else if name.len() > wanted.len() && name[..wanted.len()].eq_ignore_ascii_case(wanted) {
            prefix.push(slot);
        } else if name.len() > wanted.len() && contains_ignore_ascii_case(name, wanted) {
            substring.push(slot);
        }
    }

    match_tier(exact)
        .or_else(|| match_tier(prefix))
        .or_else(|| match_tier(substring))
        .unwrap_or(Target::UnknownName)
}

/// Turn one tier's candidate list into a target: the only candidate, an
/// ambiguity, or nothing so the next tier can be tried.
fn match_tier(mut candidates: Vec<usize>) -> Option<Target> {
    match candidates.len() {
        0 => None,
        1 => Some(Target::Slot(candidates[0])),
        _ => {
            candidates.sort_unstable();
            Some(Target::Ambiguous(candidates))
        }
    }
}

/// ASCII-case-insensitive `strstr` on a name and a wanted fragment. Both come
/// from colour-stripped buffers, so a `^` in a name is literal there.
fn contains_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// `#0 "Darth", #1 "Darker", … (+N more)` for an ambiguity reply.
pub(crate) fn candidate_list(slots: &[usize], census: &Census) -> String {
    let shown = slots
        .iter()
        .take(MAX_LISTED_CANDIDATES)
        .map(|&slot| format!("#{slot} \"{}\"", display_name(census, slot)))
        .collect::<Vec<_>>()
        .join(", ");
    match slots.len().checked_sub(MAX_LISTED_CANDIDATES) {
        Some(more) if more > 0 => format!("{shown}, +{more} more (see status)"),
        _ => shown,
    }
}

/// The census's name for a slot, for console output.
pub(crate) fn display_name(census: &Census, slot: usize) -> String {
    String::from_utf8_lossy(syscall::read_engine_string(&census.names[slot])).into_owned()
}

/// Command argument `n` of the command being executed (`trap_Argv`), copied out
/// of the engine's token buffer.
pub(crate) fn arg(n: c_int) -> Vec<u8> {
    let mut buffer = [0u8; MAX_TOKEN_CHARS];
    // SAFETY: engine syscall registered; buffer writable and NUL-terminated by
    // the engine.
    unsafe { syscall::argv(n, &mut buffer) };
    syscall::read_engine_string(&buffer).to_vec()
}

/// The whole target: every argument from 1 on, joined with single spaces by the
/// game's own `ConcatArgs` (the same helper the proxy uses for chat text).
///
/// Quoting cannot be relied on here: for rcon, `SVC_RemoteCommand` re-tokenises
/// the payload and rebuilds the line from `Cmd_Argv(2..)` with single spaces
/// (`original/jedi-academy/codemp/server/sv_main.cpp:504-510`), so
/// `rcon mute "Darth Vader"` reaches the module as `mute Darth Vader`. A client
/// command goes through the same tokeniser (`Cmd_TokenizeString`), so
/// `/ignore Darth Vader` arrives the same way. Concatenating the remainder is
/// what lets a name with spaces still be named, and it is a no-op for `mute 0`.
pub(crate) fn target_argument() -> Vec<u8> {
    // SAFETY: `GAME_CONSOLE_COMMAND`/`GAME_CLIENT_COMMAND` are called through our
    // vmMain, so the original module is loaded and `GAME_INIT` has run; the
    // returned buffer is valid until the next command is processed.
    let ptr = unsafe { crate::jampgame::concat_args(1) };
    if ptr.is_null() {
        return Vec::new();
    }
    // SAFETY: `ConcatArgs` returns a NUL-terminated game static buffer.
    unsafe { core::ffi::CStr::from_ptr(ptr) }
        .to_bytes()
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a census with `(connected, name)` per slot, the rest empty.
    fn census_of(entries: &[(bool, &str)]) -> Census {
        let mut clients: Vec<ClientEntry> =
            (0..MAX_CLIENTS).map(|_| ClientEntry::default()).collect();
        for (slot, (connected, name)) in entries.iter().enumerate() {
            clients[slot].is_connected = *connected;
            utils::strncpyz(&mut clients[slot].clean_name, name.as_bytes());
        }
        census_from(&clients)
    }

    #[test]
    fn numeric_target_is_a_client_slot() {
        let census = census_of(&[(true, "Darth"), (true, "Luke")]);
        assert_eq!(resolve(b"0", &census), Target::Slot(0));
        assert_eq!(resolve(b"1", &census), Target::Slot(1));
        // An empty slot, and a number beyond the table, are not players.
        assert_eq!(resolve(b"2", &census), Target::EmptySlot(2));
        assert_eq!(resolve(b"99", &census), Target::EmptySlot(99));
        // A negative number is not a slot; it is looked up as a name.
        assert_eq!(resolve(b"-1", &census), Target::UnknownName);
    }

    #[test]
    fn name_target_ignores_case_and_colour_codes() {
        let census = census_of(&[(true, "^1Darth"), (true, "Luke")]);
        for target in [&b"darth"[..], b"DARTH", b"^1Darth", b"^1darth"] {
            assert_eq!(
                resolve(target, &census),
                Target::Slot(0),
                "{:?}",
                String::from_utf8_lossy(target)
            );
        }
        assert_eq!(resolve(b"luke", &census), Target::Slot(1));
        assert_eq!(resolve(b"yoda", &census), Target::UnknownName);
        // Only colour codes: nothing to match.
        assert_eq!(resolve(b"^1", &census), Target::EmptyName);
    }

    #[test]
    fn disconnected_slots_are_not_players() {
        let census = census_of(&[(false, "Darth"), (true, "Luke")]);
        assert_eq!(resolve(b"darth", &census), Target::UnknownName);
        assert_eq!(resolve(b"0", &census), Target::EmptySlot(0));
        // Colour codes on a disconnected name give an empty query.
        assert_eq!(resolve(b"^1", &census), Target::EmptyName);
    }

    #[test]
    fn any_part_of_a_name_matches() {
        let census = census_of(&[(true, "Darth Vader"), (true, "Luke")]);
        // Substring anywhere in the name, in any case.
        for target in [&b"ade"[..], b"ADE", b"th va", b"vad"] {
            assert_eq!(
                resolve(target, &census),
                Target::Slot(0),
                "{:?}",
                String::from_utf8_lossy(target)
            );
        }
        assert_eq!(resolve(b"uke", &census), Target::Slot(1));
        // Not a part of any name.
        assert_eq!(resolve(b"yoda", &census), Target::UnknownName);
    }

    #[test]
    fn exact_beats_prefix_beats_substring() {
        let census = census_of(&[(true, "Darth"), (true, "Darth Vader"), (true, "Lewdarth")]);
        // Exact name: "Darth".
        assert_eq!(resolve(b"darth", &census), Target::Slot(0));
        // Prefix of "Darth Vader" only.
        assert_eq!(resolve(b"darth v", &census), Target::Slot(1));
        // Only a substring of "Lewdarth".
        assert_eq!(resolve(b"lewd", &census), Target::Slot(2));
        // "arth" is a substring of all three: refused, never guessed.
        assert_eq!(resolve(b"arth", &census), Target::Ambiguous(vec![0, 1, 2]));
    }

    #[test]
    fn ambiguous_tier_lists_every_candidate() {
        let census = census_of(&[(true, "Darth"), (true, "Darker"), (true, "Luke")]);
        assert_eq!(resolve(b"dar", &census), Target::Ambiguous(vec![0, 1]));
        // Duplicate names are ambiguous too, never guessed.
        let duplicates = census_of(&[(true, "Darth"), (true, "darth")]);
        assert_eq!(
            resolve(b"Darth", &duplicates),
            Target::Ambiguous(vec![0, 1])
        );
    }

    #[test]
    fn ambiguity_reply_caps_the_list_to_fit_one_line() {
        let entries: Vec<(bool, String)> = (0..MAX_CLIENTS)
            .map(|slot| (true, format!("Raider{slot}")))
            .collect();
        let mut clients: Vec<ClientEntry> =
            (0..MAX_CLIENTS).map(|_| ClientEntry::default()).collect();
        for (slot, (connected, name)) in entries.iter().enumerate() {
            clients[slot].is_connected = *connected;
            utils::strncpyz(&mut clients[slot].clean_name, name.as_bytes());
        }
        let census = census_from(&clients);
        let slots: Vec<usize> = (0..MAX_CLIENTS).collect();
        let list = candidate_list(&slots, &census);
        assert!(list.starts_with("#0 \"Raider0\", #1 \"Raider1\""));
        assert!(list.ends_with(", +24 more (see status)"));
        // Eight names and their slots stay well inside the 1008-byte redirect
        // buffer, which is what makes the reply one readable line.
        assert!(list.len() < 400, "candidate list too long: {}", list.len());
    }
}
