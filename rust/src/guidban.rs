//! File-backed `ja_guid` ban registry (`proxy_sv_enableGuidBan` /
//! `proxy_sv_guidBanFile`).
//!
//! Modern clients send a `ja_guid` userinfo key. It is **client-asserted**: a
//! player can change it at will, so a GUID ban is a deterrent, not a security
//! boundary (a cheater can pick a fresh value and rejoin). It is still useful
//! for keeping a persistent identity out of the server cheaply.
//!
//! The registry is a file of its own, separate from the IP ban list
//! (`crate::banfile`), and it never shares a file with it. Today the only
//! writers are this module's console commands, but the reason for the
//! separation is structural: a `ja_guid` is attacker-controlled userinfo, and
//! parsing it with the IP grammar (or appending it to the IP file) would let a
//! crafted value look like an address range. GUIDs are stored as strict,
//! validated ASCII tokens, so a value can only ever be a GUID line.
//!
//! Rules:
//!
//! - one token per line; 1..=64 printable ASCII characters, no whitespace, no
//!   `#`, no `\`, no `"`; compared and stored lower-case;
//! - `#` starts a comment; blank lines and invalid lines are skipped (with a
//!   line-numbered console warning for the invalid ones);
//! - file first, memory second: a mutation writes the file atomically and only
//!   then commits the in-memory copy (`crate::banfile::write_atomic`);
//! - `reloadbans` reloads this file too; `addguid`/`removeguid`/`listguid`
//!   manage it by hand; `rcon ban <player>` adds the target's value (D-019).
//! - the whole feature is behind `proxy_sv_enableGuidBan` (default `0`): when
//!   off, no connect is checked and the three commands answer that it is off.

use core::ffi::{CStr, c_int};
use std::ffi::CString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::banfile::{self, AddOutcome};
use crate::sdk::MAX_INFO_STRING;
use crate::syscall;
use crate::userinfo;
use crate::utils;

/// Longest stored token. A GUID (hex, UUID or base64-ish) is far below this.
pub(crate) const MAX_GUID: usize = 64;
/// How many entries `listguid` prints before summarising the rest.
const LIST_LIMIT: usize = 100;

const HEADER: &str = "\
# jampgame-proxy-rs GUID ban list (proxy_sv_guidBanFile)
# One ja_guid per line: printable ASCII, no whitespace/#/backslash, max 64.
# '#' starts a comment.
# Rewritten by `addguid` / `removeguid`; hand edits are picked up by `reloadbans`.
";

/// Why a value cannot be stored in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuidError {
    Empty,
    TooLong,
    Characters,
}

impl GuidError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            GuidError::Empty => "empty",
            GuidError::TooLong => "longer than 64 bytes",
            GuidError::Characters => "contains a character the registry cannot store",
        }
    }
}

/// Validate and normalise one `ja_guid` value (case-insensitive; stored
/// lower-case).
pub(crate) fn normalise(raw: &[u8]) -> Result<String, GuidError> {
    if raw.is_empty() {
        return Err(GuidError::Empty);
    }
    if raw.len() > MAX_GUID {
        return Err(GuidError::TooLong);
    }
    let mut out = String::with_capacity(raw.len());
    for &byte in raw {
        // Printable ASCII only. `#` would start a comment, `\` is the userinfo
        // separator, `"` is rejected by the userinfo validator anyway.
        if !(0x21..=0x7e).contains(&byte) || matches!(byte, b'#' | b'\\' | b'"') {
            return Err(GuidError::Characters);
        }
        out.push(byte.to_ascii_lowercase() as char);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// File parsing / writing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LineError {
    pub line: usize,
    pub text: String,
    pub error: GuidError,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub guids: Vec<String>,
    pub errors: Vec<LineError>,
}

fn normalise_list(guids: &mut Vec<String>) {
    guids.sort_unstable();
    guids.dedup();
}

/// One token per line, `#` comments, invalid lines collected and skipped.
pub(crate) fn parse_file(bytes: &[u8]) -> Parsed {
    let mut guids = Vec::new();
    let mut errors = Vec::new();
    for (index, raw) in bytes.split(|&byte| byte == b'\n').enumerate() {
        let raw = String::from_utf8_lossy(raw);
        let text = match raw.find('#') {
            Some(start) => &raw[..start],
            None => &raw[..],
        }
        .trim();
        if text.is_empty() {
            continue;
        }
        match normalise(text.as_bytes()) {
            Ok(guid) => guids.push(guid),
            Err(error) => errors.push(LineError {
                line: index + 1,
                text: text.to_string(),
                error,
            }),
        }
    }
    normalise_list(&mut guids);
    Parsed { guids, errors }
}

pub(crate) fn load(path: &Path) -> io::Result<Parsed> {
    Ok(parse_file(&banfile::read_file(path)?))
}

pub(crate) fn save(path: &Path, guids: &[String]) -> io::Result<()> {
    let mut guids = guids.to_vec();
    normalise_list(&mut guids);
    let mut body = String::from(HEADER);
    for guid in &guids {
        body.push_str(guid);
        body.push('\n');
    }
    banfile::write_atomic(path, &body)
}

// ---------------------------------------------------------------------------
// Runtime state
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Runtime {
    path: PathBuf,
    guids: Vec<String>,
}

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Runtime>> {
    RUNTIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether the registry is loaded and enabled right now.
pub(crate) fn enabled() -> bool {
    lock().as_ref().is_some()
}

/// Mirror `(proxy_sv_enableGuidBan, proxy_sv_guidBanFile)` from the cvar
/// refresh; loads only when the pair changes.
pub(crate) fn sync_config(enabled: bool, path: &str) {
    let path = PathBuf::from(path.trim());
    let enabled = enabled && !path.as_os_str().is_empty();
    {
        let runtime = lock();
        if let Some(current) = runtime.as_ref() {
            if enabled && current.path == path {
                return;
            }
        } else if !enabled {
            return;
        }
    }

    if !enabled {
        *lock() = None;
        return;
    }

    let (guids, errors, failure) = match load(&path) {
        Ok(parsed) => (parsed.guids, parsed.errors, None),
        Err(error) => (Vec::new(), Vec::new(), Some(error)),
    };
    let count = guids.len();
    *lock() = Some(Runtime {
        path: path.clone(),
        guids,
    });

    for error in &errors {
        print_line(&format!(
            "proxy: GUID ban file {}:{}: invalid entry \"{}\" ({}), skipped",
            path.display(),
            error.line,
            error.text,
            error.error.message()
        ));
    }
    match failure {
        Some(error) if error.kind() == io::ErrorKind::NotFound => print_line(&format!(
            "proxy: GUID ban file {} not found; starting with no GUID bans",
            path.display()
        )),
        Some(error) => print_line(&format!(
            "proxy: cannot read GUID ban file {}: {error}; starting with no GUID bans",
            path.display()
        )),
        None => print_line(&format!(
            "proxy: GUID ban file {} loaded ({count} entries)",
            path.display()
        )),
    }
}

/// Re-read the file. Returns `false` when the feature is disabled.
pub(crate) fn reload() -> bool {
    let path = {
        let runtime = lock();
        match runtime.as_ref() {
            Some(runtime) => runtime.path.clone(),
            None => return false,
        }
    };
    let parsed = match load(&path) {
        Ok(parsed) => parsed,
        Err(error) => {
            print_line(&format!(
                "proxy: cannot read GUID ban file {}: {error}",
                path.display()
            ));
            return true;
        }
    };
    for error in &parsed.errors {
        print_line(&format!(
            "proxy: GUID ban file {}:{}: invalid entry \"{}\" ({}), skipped",
            path.display(),
            error.line,
            error.text,
            error.error.message()
        ));
    }
    let count = parsed.guids.len();
    if let Some(runtime) = lock().as_mut() {
        runtime.guids = parsed.guids;
    }
    print_line(&format!(
        "proxy: GUID ban file {} reloaded ({count} entries)",
        path.display()
    ));
    true
}

/// Whether `guid` (already normalised) is listed.
fn is_banned(guid: &str) -> bool {
    let runtime = lock();
    runtime.as_ref().is_some_and(|runtime| {
        runtime
            .guids
            .binary_search_by(|entry| entry.as_str().cmp(guid))
            .is_ok()
    })
}

/// Add one normalised token to the loaded list and to its file (file first).
pub(crate) fn add_guid(guid: &str) -> Result<AddOutcome, String> {
    let Some((path, mut guids)) = snapshot() else {
        return Err("the GUID ban file is disabled (proxy_sv_enableGuidBan 0)".to_string());
    };
    match guids.binary_search_by(|entry| entry.as_str().cmp(guid)) {
        Ok(_) => return Ok(AddOutcome::AlreadyPresent),
        Err(index) => guids.insert(index, guid.to_string()),
    }
    save(&path, &guids)
        .map_err(|error| format!("cannot write GUID ban file {}: {error}", path.display()))?;
    committed(&path, guids);
    Ok(AddOutcome::Added)
}

/// Remove one normalised token. `Ok(false)` when it was not listed.
pub(crate) fn remove_guid(guid: &str) -> Result<bool, String> {
    let Some((path, mut guids)) = snapshot() else {
        return Err("the GUID ban file is disabled (proxy_sv_enableGuidBan 0)".to_string());
    };
    match guids.binary_search_by(|entry| entry.as_str().cmp(guid)) {
        Ok(index) => {
            guids.remove(index);
        }
        Err(_) => return Ok(false),
    }
    save(&path, &guids)
        .map_err(|error| format!("cannot write GUID ban file {}: {error}", path.display()))?;
    committed(&path, guids);
    Ok(true)
}

fn snapshot() -> Option<(PathBuf, Vec<String>)> {
    let runtime = lock();
    runtime
        .as_ref()
        .map(|runtime| (runtime.path.clone(), runtime.guids.clone()))
}

fn committed(path: &Path, guids: Vec<String>) {
    if let Some(runtime) = lock().as_mut() {
        debug_assert_eq!(runtime.path.as_path(), path);
        runtime.guids = guids;
    }
}

// ---------------------------------------------------------------------------
// Connect check
// ---------------------------------------------------------------------------

/// `GAME_CLIENT_CONNECT` gate: `Some("Banned.")` when the client's `ja_guid`
/// is listed. A missing or malformed value is not a ban.
pub(crate) fn refuse_connect(client_num: c_int, is_bot: bool) -> Option<&'static CStr> {
    if is_bot || !(0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num) {
        return None;
    }
    let mut buffer = [0u8; MAX_INFO_STRING];
    // SAFETY: engine syscall registered; buffer writable.
    unsafe { syscall::get_userinfo(client_num, &mut buffer) };
    let len = utils::c_strlen(&buffer);
    let raw = userinfo::value_for_key(&buffer[..len], b"ja_guid");
    let guid = match normalise(raw) {
        Ok(guid) => guid,
        Err(error) => {
            if !raw.is_empty() && crate::diagnostics::suspicious_enabled() {
                crate::diagnostics::suspicious(
                    "client.connect.guid_invalid",
                    format!(
                        "client={client_num} guid=\"{}\" ({})",
                        crate::diagnostics::escape_bytes(raw, MAX_GUID),
                        error.message()
                    ),
                );
            }
            return None;
        }
    };
    if !is_banned(&guid) {
        return None;
    }
    print_line(&format!(
        "proxy: connection from client {client_num} refused (GUID {guid})"
    ));
    crate::diagnostics::suspicious(
        "client.connect.banned",
        format!("client={client_num} guid={guid}"),
    );
    Some(c"Banned.")
}

// ---------------------------------------------------------------------------
// Console commands (`addguid`, `removeguid`, `listguid`)
// ---------------------------------------------------------------------------

pub(crate) fn handle_console_command() -> bool {
    let command = crate::player_target::arg(0).to_ascii_lowercase();
    let ours = matches!(command.as_slice(), b"addguid" | b"removeguid" | b"listguid");
    if !ours {
        return false;
    }
    if !enabled() {
        print_line("proxy: the GUID ban file is disabled (proxy_sv_enableGuidBan 0).");
        return true;
    }
    match command.as_slice() {
        b"addguid" => add_command(),
        b"removeguid" => remove_command(),
        b"listguid" => list_command(),
        _ => unreachable!(),
    }
    true
}

fn argument() -> Option<String> {
    let raw = crate::player_target::arg(1);
    if raw.is_empty() {
        print_line("usage: addguid <guid> | removeguid <guid>");
        return None;
    }
    match normalise(&raw) {
        Ok(guid) => Some(guid),
        Err(error) => {
            print_line(&format!(
                "proxy: invalid GUID \"{}\": {}",
                String::from_utf8_lossy(&raw),
                error.message()
            ));
            None
        }
    }
}

fn add_command() {
    let Some(guid) = argument() else {
        return;
    };
    match add_guid(&guid) {
        Ok(AddOutcome::AlreadyPresent) => {
            print_line(&format!("proxy: GUID {guid} is already banned"))
        }
        Ok(AddOutcome::Added) => {
            let count = lock().as_ref().map_or(0, |runtime| runtime.guids.len());
            print_line(&format!("proxy: banned GUID {guid} ({count} entries)"));
        }
        Err(error) => print_line(&format!("proxy: {error}")),
    }
}

fn remove_command() {
    let Some(guid) = argument() else {
        return;
    };
    match remove_guid(&guid) {
        Ok(true) => {
            let count = lock().as_ref().map_or(0, |runtime| runtime.guids.len());
            print_line(&format!("proxy: unbanned GUID {guid} ({count} entries)"));
        }
        Ok(false) => print_line(&format!("proxy: GUID {guid} is not banned")),
        Err(error) => print_line(&format!("proxy: {error}")),
    }
}

fn list_command() {
    let Some((path, guids)) = snapshot() else {
        return;
    };
    if guids.is_empty() {
        print_line(&format!("proxy: no GUID bans ({})", path.display()));
        return;
    }
    print_line(&format!(
        "proxy: {} GUID ban(s) from {}:",
        guids.len(),
        path.display()
    ));
    for guid in guids.iter().take(LIST_LIMIT) {
        print_line(&format!("  {guid}"));
    }
    if guids.len() > LIST_LIMIT {
        print_line(&format!("  ... {} more", guids.len() - LIST_LIMIT));
    }
}

/// `trap_Print`: one console line (redirected to the admin during rcon).
fn print_line(text: &str) {
    let Ok(line) = CString::new(format!("{text}\n")) else {
        return;
    };
    // SAFETY: engine syscall registered; `line` is NUL-terminated.
    unsafe { syscall::print(&line) };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guid_is_lower_cased_and_kept_whole() {
        assert_eq!(
            normalise(b"0123456789ABCDEF0123456789abcdef"),
            Ok("0123456789abcdef0123456789abcdef".to_string())
        );
        assert_eq!(
            normalise(b"3f2504e0-4f89-11d3-9a0c-0305e82c3301"),
            Ok("3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_string())
        );
        assert_eq!(normalise(b"A_b-1.2:3+4="), Ok("a_b-1.2:3+4=".to_string()));
    }

    #[test]
    fn unsafe_or_empty_values_are_refused() {
        assert_eq!(normalise(b""), Err(GuidError::Empty));
        assert_eq!(normalise(b"has space"), Err(GuidError::Characters));
        assert_eq!(normalise(b"line\nbreak"), Err(GuidError::Characters));
        assert_eq!(normalise(b"tab\there"), Err(GuidError::Characters));
        assert_eq!(normalise(b"a#b"), Err(GuidError::Characters));
        assert_eq!(normalise(b"a\\b"), Err(GuidError::Characters));
        assert_eq!(normalise(b"a\"b"), Err(GuidError::Characters));
        let long = [b'a'; MAX_GUID + 1];
        assert_eq!(normalise(&long), Err(GuidError::TooLong));
    }

    #[test]
    fn a_file_takes_comments_and_reports_bad_lines() {
        let parsed = parse_file(
            b"# header\n\
              0123456789ABCDEF\n\
              \n\
              3f2504e0-4f89-11d3-9a0c-0305e82c3301   # a UUID\n\
              bad value\n\
              0123456789abcdef\n",
        );
        assert_eq!(
            parsed.guids,
            vec![
                "0123456789abcdef".to_string(),
                "3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_string()
            ]
        );
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(parsed.errors[0].line, 5);
    }

    #[test]
    fn a_round_trip_is_canonical() {
        let dir = std::env::temp_dir().join(format!(
            "jampgame-proxy-guidban-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("guids.txt");

        let guids = vec![
            normalise(b"BBB").expect("valid"),
            normalise(b"aaa").expect("valid"),
            normalise(b"BBB").expect("valid"),
        ];
        save(&path, &guids).expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.starts_with("# jampgame-proxy-rs GUID ban list"));
        let entries: Vec<&str> = text
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .collect();
        assert_eq!(entries, vec!["aaa", "bbb"]);

        let loaded = load(&path).expect("load");
        assert_eq!(loaded.guids, vec!["aaa".to_string(), "bbb".to_string()]);
        assert!(loaded.errors.is_empty());

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
