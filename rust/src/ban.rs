//! `rcon ban <clientNumber|playerName>` (D-019).
//!
//! One admin command for the two registries:
//!
//! 1. ban the target's IP through the IP ban file (`crate::banfile`) —
//!    when `proxy_sv_enableBanFile` is off, fall back to the pristine
//!    `addip` the same way an admin would type it;
//! 2. ban the target's `ja_guid` through the separate GUID registry
//!    (`crate::guidban`) when `proxy_sv_enableGuidBan` is on (a stock client has
//!    no `ja_guid`, which is reported, not treated as an error);
//! 3. kick the target with the stock `Banned.` reason.
//!
//! The target shape is the shared `player_target` resolver (slot number, exact
//! name, unique prefix, or a name fragment), so `ban` answers an ambiguous or
//! missing target exactly like `mute`.
//!
//! `ja_guid` is client-asserted userinfo: it can be changed by the player, so a
//! GUID ban is a deterrent and not a security boundary. The value is validated
//! (`crate::guidban::normalise`) before it can reach the registry, and the two
//! registries never share a file, so a crafted userinfo value cannot add an IP
//! range.

use std::ffi::CString;
use std::net::Ipv4Addr;

use crate::banfile::{self, AddOutcome};
use crate::guidban;
use crate::player_target::{self, Census, Target};
use crate::sdk::{EXEC_APPEND, G_SEND_CONSOLE_COMMAND, MAX_INFO_STRING};
use crate::syscall;
use crate::userinfo;
use crate::utils;

const USAGE: &str = "usage: ban <clientNumber|playerName>";

/// Handle `ban` from `GAME_CONSOLE_COMMAND`. Returns `true` when the line was
/// ours (the pristine module does not know the command and would echo it to
/// every client as `server: ...`).
pub(crate) fn handle_console_command() -> bool {
    if !matches!(
        player_target::arg(0).to_ascii_lowercase().as_slice(),
        b"ban"
    ) {
        return false;
    }
    let target = player_target::target_argument();
    if target.is_empty() {
        print_line(USAGE);
        return true;
    }
    let census = player_target::census();
    match player_target::resolve(&target, &census) {
        Target::Slot(slot) => ban_slot(slot, &census),
        Target::EmptySlot(slot) => print_line(&format!("proxy: client slot {slot} has no player.")),
        Target::EmptyName => print_line("proxy: give a client number or a player name."),
        Target::UnknownName => print_line(&format!(
            "proxy: no player named \"{}\".",
            String::from_utf8_lossy(&target)
        )),
        Target::Ambiguous(slots) => print_line(&format!(
            "proxy: \"{}\" matches {}; use the client number.",
            String::from_utf8_lossy(&target),
            player_target::candidate_list(&slots, &census)
        )),
    }
    true
}

/// Ban one resolved client: registry writes first, then the kick.
fn ban_slot(slot: usize, census: &Census) {
    let name = player_target::display_name(census, slot);
    let mut buffer = [0u8; MAX_INFO_STRING];
    // SAFETY: engine syscall registered; buffer writable.
    unsafe { syscall::get_userinfo(slot as core::ffi::c_int, &mut buffer) };
    let len = utils::c_strlen(&buffer);
    let info = &buffer[..len];
    let address = banfile::parse_userinfo_ip(userinfo::value_for_key(info, b"ip"));
    let raw_guid = userinfo::value_for_key(info, b"ja_guid");

    let mut notes = Vec::new();

    match address {
        Some(address) if banfile::enabled() => match banfile::ban_address(address) {
            Ok(AddOutcome::Added) => notes.push(format!("ip {address} banned")),
            Ok(AddOutcome::AlreadyPresent) => notes.push(format!("ip {address} already banned")),
            Err(error) => notes.push(format!("ip not banned: {error}")),
        },
        Some(address) => {
            queue_legacy_addip(address);
            notes.push(format!(
                "ip {address} banned through the pristine addip (proxy_sv_enableBanFile 0)"
            ));
        }
        None => notes.push("no bannable IP (LAN/local client)".to_string()),
    }

    if guidban::enabled() {
        match guidban::normalise(raw_guid) {
            Ok(guid) => match guidban::add_guid(&guid) {
                Ok(AddOutcome::Added) => notes.push(format!("GUID {guid} banned")),
                Ok(AddOutcome::AlreadyPresent) => notes.push(format!("GUID {guid} already banned")),
                Err(error) => notes.push(format!("GUID not banned: {error}")),
            },
            Err(_) if raw_guid.is_empty() => {
                notes.push("no ja_guid (GUID list unchanged)".to_string())
            }
            Err(error) => notes.push(format!(
                "invalid ja_guid \"{}\" ({}), GUID list unchanged",
                crate::diagnostics::escape_bytes(raw_guid, 64),
                error.message()
            )),
        }
    } else {
        notes.push("GUID not banned (proxy_sv_enableGuidBan 0)".to_string());
    }

    // SAFETY: engine syscall registered; `slot` is a live client and the
    // literal reason is NUL-terminated.
    unsafe { syscall::drop_client(slot as core::ffi::c_int, c"Banned.") };
    print_line(&format!(
        "proxy: banned #{slot} \"{name}\": {}; kicked.",
        notes.join("; ")
    ));
}

/// The pristine `addip` path, queued on the engine command buffer (the
/// `EXEC_APPEND` form `crate::restart` uses). Only used when the IP ban file is
/// off; the game's own 256-byte `g_banIPs` limits apply.
fn queue_legacy_addip(address: Ipv4Addr) {
    let Ok(command) = CString::new(legacy_addip_command(address)) else {
        return;
    };
    // SAFETY: engine syscall registered; the command is NUL-terminated and the
    // engine copies it into its command buffer.
    unsafe {
        syscall::call_engine(
            G_SEND_CONSOLE_COMMAND,
            &[EXEC_APPEND, command.as_ptr() as core::ffi::c_int],
        );
    }
}

fn legacy_addip_command(address: Ipv4Addr) -> String {
    format!("addip {address}\n")
}

/// `trap_Print`: one console line (redirected to the admin during rcon).
fn print_line(text: &str) {
    let Ok(line) = CString::new(format!("{text}\n")) else {
        return;
    };
    // SAFETY: engine syscall registered; `line` is NUL-terminated.
    unsafe { syscall::print(&line) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pristine_fallback_is_the_command_an_admin_would_type() {
        assert_eq!(
            legacy_addip_command(Ipv4Addr::new(84, 17, 32, 5)),
            "addip 84.17.32.5\n"
        );
    }
}
