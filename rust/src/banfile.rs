//! File-backed IP ban list (`proxy_sv_enableBanFile` / `proxy_sv_banFile`).
//!
//! The pristine game keeps its bans in a per-session `ipFilters[1024]` array and
//! mirrors the list into the archived cvar `g_banIPs`. That mirror is a
//! `vmCvar_t` with `MAX_CVAR_VALUE_STRING` (256) bytes, and the engine's
//! `Cvar_Update` turns a longer value into `Com_Error(ERR_DROP)` (source
//! `qcommon/cvar.cpp:939-949`; measured on the retail engine: 25 entries /
//! 267 bytes → `Cvar_Update: src g_banIPs length 267 exceeds
//! MAX_CVAR_VALUE_STRING`, `ShutdownGame`, exit 139 — and the over-long value
//! had already been written to `jampserver.cfg`, so every later start failed the
//! same way). The pristine mechanism is therefore unusable for an IP-range
//! list: it holds a couple of dozen addresses and overrunning it bricks the
//! server.
//!
//! This module makes a file the only source of truth:
//!
//! - `addip` / `removeip` (claimed from `GAME_CONSOLE_COMMAND`) edit the file
//!   and the in-memory mirror; the game's `g_banIPs` is never written.
//! - `listip` prints the file's entries instead of the cvar.
//! - `reloadbans` re-reads the file (hand edits during a running session).
//! - Every connect is checked at `GAME_CLIENT_CONNECT` against the loaded list;
//!   a match returns the game's own `"Banned."`, so the engine refuses the
//!   client before it enters the game.
//!
//! The in-memory vector is a cache of the file, not a second source of truth:
//! every mutation is written to disk first and only then committed to memory,
//! so a failed write leaves the file and the cache in agreement.
//!
//! The whole feature is behind `proxy_sv_enableBanFile` (default `0`): when it
//! is off, `addip`/`removeip`/`listip` are forwarded to the pristine game and
//! no connect is checked. `proxy_sv_banFile` names the file (default
//! `proxy_bans.txt`, relative to the server's working directory like
//! `proxy_debug.log`, or an absolute path).

use core::ffi::{CStr, c_int, c_void};
use std::ffi::CString;
use std::io;
use std::net::Ipv4Addr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};

use crate::sdk::MAX_INFO_STRING;
use crate::syscall;
use crate::userinfo;
use crate::utils;

/// How many entries `listip` prints before summarising the rest (a VPN list can
/// hold tens of thousands of ranges; the file is the place to read it).
const LIST_LIMIT: usize = 100;

const HEADER: &str = "\
# jampgame-proxy-rs ban list (proxy_sv_banFile)
# One IPv4 address or CIDR range per line. '#' starts a comment.
# Rewritten by `addip` / `removeip`; other changes are picked up by `reloadbans`.
";

// ---------------------------------------------------------------------------
// One entry
// ---------------------------------------------------------------------------

/// A denied IPv4 address or CIDR range, normalised to its network address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ban {
    network: Ipv4Addr,
    prefix: u8,
}

/// Why an entry could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    Address,
    Prefix,
    /// `/0` would deny every address (the LAN exemption aside), which is never
    /// what an `addip` typo means; `g_filterBan 0` is the allow-only mode.
    PrefixZero,
}

impl ParseError {
    fn message(self) -> &'static str {
        match self {
            ParseError::Empty => "empty address",
            ParseError::Address => "not an IPv4 address",
            ParseError::Prefix => "prefix length must be 1-32",
            ParseError::PrefixZero => "prefix 0 would ban every address",
        }
    }
}

impl Ban {
    /// Parse `<ipv4>` or `<ipv4>/<prefix>`; the host bits are masked off
    /// (`84.17.32.5/19` and `84.17.32.0/19` are the same entry).
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(ParseError::Empty);
        }
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (address, prefix),
            None => (text, "32"),
        };
        let address = Ipv4Addr::from_str(address).map_err(|_| ParseError::Address)?;
        let prefix: u8 = prefix.parse().map_err(|_| ParseError::Prefix)?;
        if prefix == 0 {
            return Err(ParseError::PrefixZero);
        }
        if prefix > 32 {
            return Err(ParseError::Prefix);
        }
        Ok(Self::new(address, prefix))
    }

    fn new(address: Ipv4Addr, prefix: u8) -> Self {
        debug_assert!((1..=32).contains(&prefix));
        // `prefix` is 1-32, so the shift is 0-31.
        let mask = u32::MAX << (32 - prefix);
        Ban {
            network: Ipv4Addr::from(u32::from(address) & mask),
            prefix,
        }
    }

    /// The first address of the range.
    pub fn network(&self) -> Ipv4Addr {
        self.network
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    pub fn contains(&self, address: Ipv4Addr) -> bool {
        let mask = u32::MAX << (32 - self.prefix);
        u32::from(address) & mask == u32::from(self.network)
    }

    /// Canonical text: `a.b.c.d` for `/32`, `a.b.c.d/n` otherwise.
    pub fn canonical(&self) -> String {
        if self.prefix == 32 {
            self.network.to_string()
        } else {
            format!("{}/{}", self.network, self.prefix)
        }
    }
}

impl core::fmt::Display for Ban {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.canonical())
    }
}

// ---------------------------------------------------------------------------
// File parsing / writing
// ---------------------------------------------------------------------------

/// A rejected line (reported at load, then skipped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineError {
    pub line: usize,
    pub text: String,
    pub error: ParseError,
}

/// A parsed ban file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub bans: Vec<Ban>,
    pub errors: Vec<LineError>,
}

fn normalise(bans: &mut Vec<Ban>) {
    bans.sort_unstable();
    bans.dedup();
}

/// Parse a file body: one entry per line, blank lines and `#` comments
/// ignored, duplicates and host bits normalised.
pub fn parse_file(bytes: &[u8]) -> Parsed {
    let mut bans = Vec::new();
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
        match Ban::parse(text) {
            Ok(ban) => bans.push(ban),
            Err(error) => errors.push(LineError {
                line: index + 1,
                text: text.to_string(),
                error,
            }),
        }
    }
    normalise(&mut bans);
    Parsed { bans, errors }
}

/// Read and parse `path`.
///
/// The file I/O goes through libc, not `std::fs`: `std::fs::read` calls
/// `File::metadata` → `try_statx` → `movaps`, which faults on the engine's
/// 4-byte-aligned entry stack (D-001/D-018 — observed on the first
/// `reloadbans`).
pub fn load(path: &Path) -> io::Result<Parsed> {
    Ok(parse_file(&read_file(path)?))
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

/// Read a whole file through libc `open`/`read`/`close`.
fn read_file(path: &Path) -> io::Result<Vec<u8>> {
    const O_RDONLY: c_int = 0;
    const CHUNK: usize = 8192;
    let path = c_path(path)?;
    // SAFETY: `path` is NUL-terminated; the descriptor is closed below.
    unsafe {
        let fd = sys::open(path.as_ptr(), O_RDONLY);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut out = Vec::new();
        let mut buffer = [0u8; CHUNK];
        loop {
            let n = sys::read(fd, buffer.as_mut_ptr() as *mut c_void, buffer.len());
            if n < 0 {
                let error = io::Error::last_os_error();
                sys::close(fd);
                return Err(error);
            }
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buffer[..n as usize]);
        }
        sys::close(fd);
        Ok(out)
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Write `bans` as a canonical list, atomically (temporary file + rename), so a
/// crash mid-write cannot truncate the list. The entries are sorted and
/// deduplicated first, so the file is deterministic whatever the caller passes.
///
/// libc `open`/`write`/`rename` for the same reason as [`load`]:
/// `std::fs::File::metadata`/`statx` uses SSE, which cannot run on the engine's
/// 4-byte-aligned stack.
pub fn save(path: &Path, bans: &[Ban]) -> io::Result<()> {
    const O_WRONLY: c_int = 1;
    const O_CREAT: c_int = 0o100;
    const O_TRUNC: c_int = 0o1000;
    const MODE: c_int = 0o644;

    let mut bans = bans.to_vec();
    normalise(&mut bans);
    let mut body = String::from(HEADER);
    for ban in &bans {
        body.push_str(&ban.canonical());
        body.push('\n');
    }

    let target = c_path(path)?;
    let temporary_path = temporary_path(path);
    let temporary = c_path(&temporary_path)?;

    // SAFETY: both paths are NUL-terminated; the descriptor is closed below.
    unsafe {
        let fd = sys::open(temporary.as_ptr(), O_WRONLY | O_CREAT | O_TRUNC, MODE);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut written = 0;
        let bytes = body.as_bytes();
        while written < bytes.len() {
            let n = sys::write(
                fd,
                bytes[written..].as_ptr() as *const c_void,
                bytes.len() - written,
            );
            if n < 0 {
                let error = io::Error::last_os_error();
                sys::close(fd);
                sys::unlink(temporary.as_ptr());
                return Err(error);
            }
            written += n as usize;
        }
        if sys::fsync(fd) != 0 {
            let error = io::Error::last_os_error();
            sys::close(fd);
            sys::unlink(temporary.as_ptr());
            return Err(error);
        }
        sys::close(fd);
        if sys::rename(temporary.as_ptr(), target.as_ptr()) != 0 {
            let error = io::Error::last_os_error();
            sys::unlink(temporary.as_ptr());
            return Err(error);
        }
    }
    Ok(())
}

mod sys {
    use core::ffi::{c_char, c_int, c_void};

    unsafe extern "C" {
        pub fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        pub fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize;
        pub fn write(fd: c_int, buffer: *const c_void, count: usize) -> isize;
        pub fn close(fd: c_int) -> c_int;
        pub fn fsync(fd: c_int) -> c_int;
        pub fn rename(old: *const c_char, new: *const c_char) -> c_int;
        pub fn unlink(path: *const c_char) -> c_int;
    }
}

// ---------------------------------------------------------------------------
// Runtime state
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Runtime {
    path: PathBuf,
    bans: Vec<Ban>,
}

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Runtime>> {
    RUNTIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether the feature is loaded and enabled right now.
pub fn enabled() -> bool {
    let runtime = lock();
    runtime.as_ref().is_some()
}

/// Mirror `(proxy_sv_enableBanFile, proxy_sv_banFile)` from the cvar refresh.
/// Loads the file only when the pair changes, so the per-frame call is free.
pub fn sync_config(enabled: bool, path: &str) {
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

    let (bans, errors, failure) = match load(&path) {
        Ok(parsed) => (parsed.bans, parsed.errors, None),
        Err(error) => (Vec::new(), Vec::new(), Some(error)),
    };
    let count = bans.len();
    *lock() = Some(Runtime {
        path: path.clone(),
        bans,
    });

    for error in &errors {
        print_line(&format!(
            "proxy: ban file {}:{}: invalid entry \"{}\" ({}), skipped",
            path.display(),
            error.line,
            error.text,
            error.error.message()
        ));
    }
    match failure {
        Some(error) if error.kind() == io::ErrorKind::NotFound => print_line(&format!(
            "proxy: ban file {} not found; starting with no bans",
            path.display()
        )),
        Some(error) => print_line(&format!(
            "proxy: cannot read ban file {}: {error}; starting with no bans",
            path.display()
        )),
        None => print_line(&format!(
            "proxy: ban file {} loaded ({count} entries)",
            path.display()
        )),
    }
    warn_legacy_bans(&path);
}

/// The pristine `addip` list is still enforced by the game module's own
/// `ClientConnect`. Report it once when the file list is enabled: an admin
/// migrating to the file must clear `g_banIPs`, or the old bans keep applying.
fn warn_legacy_bans(path: &Path) {
    let mut buffer = [0u8; 256];
    // SAFETY: engine syscall registered (this runs from cvar refresh, after
    // GAME_INIT); the buffer is writable and NUL-terminated by the engine.
    unsafe { syscall::cvar_variable_string_buffer(c"g_banIPs", &mut buffer) };
    let legacy = syscall::read_engine_string(&buffer);
    let count = legacy
        .split(|&byte| byte == b' ')
        .filter(|entry| !entry.is_empty())
        .count();
    if count > 0 {
        print_line(&format!(
            "proxy: note: the legacy g_banIPs cvar still holds {count} entr{} and is still \
             enforced; move them to {} and run `set g_banIPs \"\"`",
            if count == 1 { "y" } else { "ies" },
            path.display()
        ));
    }
}

/// Re-read the file. Returns `false` when the feature is disabled.
pub fn reload() -> bool {
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
                "proxy: cannot read ban file {}: {error}",
                path.display()
            ));
            return true;
        }
    };
    for error in &parsed.errors {
        print_line(&format!(
            "proxy: ban file {}:{}: invalid entry \"{}\" ({}), skipped",
            path.display(),
            error.line,
            error.text,
            error.error.message()
        ));
    }
    let count = parsed.bans.len();
    if let Some(runtime) = lock().as_mut() {
        runtime.bans = parsed.bans;
    }
    print_line(&format!(
        "proxy: ban file {} reloaded ({count} entries)",
        path.display()
    ));
    true
}

/// The first loaded entry that contains `address`.
fn find(address: Ipv4Addr) -> Option<Ban> {
    let runtime = lock();
    runtime.as_ref().and_then(|runtime| {
        runtime
            .bans
            .iter()
            .find(|ban| ban.contains(address))
            .copied()
    })
}

// ---------------------------------------------------------------------------
// Connect check
// ---------------------------------------------------------------------------

/// Parse the engine's `ip` userinfo value: `a.b.c.d:port`, or `"localhost"`
/// for a LAN client (`server/sv_client.cpp:327-329`). Anything else (IPv6,
/// garbage) yields `None` and the connect is not checked.
fn parse_userinfo_ip(value: &[u8]) -> Option<Ipv4Addr> {
    // The engine writes `a.b.c.d:port`; drop the port.
    let value = value.split(|&byte| byte == b':').next()?;
    let text = core::str::from_utf8(value).ok()?;
    if text.eq_ignore_ascii_case("localhost") {
        return None;
    }
    Ipv4Addr::from_str(text).ok()
}

/// The client's address from the engine-forced `ip` userinfo key, or `None`
/// for bots, LAN/loopback clients and anything unparseable (the engine writes
/// `"localhost"` for a local address, `server/sv_client.cpp:327-329`).
fn client_address(client_num: c_int) -> Option<Ipv4Addr> {
    if !(0..crate::sdk::MAX_CLIENTS as c_int).contains(&client_num) {
        return None;
    }
    let mut buffer = [0u8; MAX_INFO_STRING];
    // SAFETY: engine syscall registered; buffer writable.
    unsafe { syscall::get_userinfo(client_num, &mut buffer) };
    let len = utils::c_strlen(&buffer);
    parse_userinfo_ip(userinfo::value_for_key(&buffer[..len], b"ip"))
}

/// `GAME_CLIENT_CONNECT` gate: `Some("Banned.")` when the address is listed.
pub fn refuse_connect(client_num: c_int, is_bot: bool) -> Option<&'static CStr> {
    if is_bot {
        return None;
    }
    let address = client_address(client_num)?;
    let ban = find(address)?;
    print_line(&format!(
        "proxy: connection from {address} refused (ban {ban})"
    ));
    crate::diagnostics::suspicious(
        "client.connect.banned",
        format!("client={client_num} ip={address} ban={ban}"),
    );
    Some(c"Banned.")
}

// ---------------------------------------------------------------------------
// Console commands (`addip`, `removeip`, `listip`, `reloadbans`)
// ---------------------------------------------------------------------------

/// Handle the ban commands from `GAME_CONSOLE_COMMAND`. Returns `true` when the
/// line was ours, so the caller answers qtrue instead of letting the pristine
/// `ConsoleCommand` treat it as a server chat line.
///
/// With `proxy_sv_enableBanFile 0` the three pristine commands are forwarded
/// untouched; `reloadbans` is proxy-only, so it is claimed and reports that the
/// feature is off.
pub fn handle_console_command() -> bool {
    let command = crate::player_target::arg(0).to_ascii_lowercase();
    let ours = matches!(
        command.as_slice(),
        b"addip" | b"removeip" | b"listip" | b"reloadbans"
    );
    if !ours {
        return false;
    }
    if !enabled() {
        if matches!(command.as_slice(), b"reloadbans") {
            print_line("proxy: the ban file is disabled (proxy_sv_enableBanFile 0).");
            return true;
        }
        return false;
    }
    match command.as_slice() {
        b"addip" => add_command(),
        b"removeip" => remove_command(),
        b"listip" => list_command(),
        b"reloadbans" => {
            reload();
        }
        _ => unreachable!(),
    }
    true
}

/// The current file path and a copy of the loaded entries.
fn snapshot() -> Option<(PathBuf, Vec<Ban>)> {
    let runtime = lock();
    runtime
        .as_ref()
        .map(|runtime| (runtime.path.clone(), runtime.bans.clone()))
}

fn committed(path: &Path, bans: Vec<Ban>) {
    if let Some(runtime) = lock().as_mut() {
        debug_assert_eq!(runtime.path.as_path(), path);
        runtime.bans = bans;
    }
}

fn argument() -> Option<String> {
    let raw = crate::player_target::arg(1);
    let text = String::from_utf8_lossy(&raw).trim().to_string();
    if text.is_empty() {
        print_line("usage: addip <ip[/prefix]> | removeip <ip[/prefix]>");
        return None;
    }
    Some(text)
}

fn parse_argument(text: &str) -> Option<Ban> {
    match Ban::parse(text) {
        Ok(ban) => Some(ban),
        Err(error) => {
            print_line(&format!(
                "proxy: invalid ban \"{text}\": {}",
                error.message()
            ));
            None
        }
    }
}

fn add_command() {
    let Some((path, mut bans)) = snapshot() else {
        return;
    };
    let Some(ban) = argument().as_deref().and_then(parse_argument) else {
        return;
    };
    if bans.contains(&ban) {
        print_line(&format!("proxy: {ban} is already banned"));
        return;
    }
    bans.push(ban);
    normalise(&mut bans);
    match save(&path, &bans) {
        Ok(()) => {
            let count = bans.len();
            committed(&path, bans);
            print_line(&format!("proxy: banned {ban} ({count} entries)"));
        }
        Err(error) => print_line(&format!(
            "proxy: cannot write ban file {}: {error}",
            path.display()
        )),
    }
}

fn remove_command() {
    let Some((path, bans)) = snapshot() else {
        return;
    };
    let Some(ban) = argument().as_deref().and_then(parse_argument) else {
        return;
    };
    if !bans.contains(&ban) {
        // A single address inside a wider range cannot be removed without
        // naming the range; say which one covers it.
        match bans.iter().find(|candidate| {
            candidate.prefix() < ban.prefix() && candidate.contains(ban.network())
        }) {
            Some(covering) => print_line(&format!(
                "proxy: {ban} is covered by the wider ban {covering}; remove that range instead"
            )),
            None => print_line(&format!("proxy: {ban} is not banned")),
        }
        return;
    }
    let mut bans = bans;
    bans.retain(|entry| *entry != ban);
    match save(&path, &bans) {
        Ok(()) => {
            let count = bans.len();
            committed(&path, bans);
            print_line(&format!("proxy: unbanned {ban} ({count} entries)"));
        }
        Err(error) => print_line(&format!(
            "proxy: cannot write ban file {}: {error}",
            path.display()
        )),
    }
}

fn list_command() {
    let Some((path, bans)) = snapshot() else {
        return;
    };
    if bans.is_empty() {
        print_line(&format!("proxy: no bans ({})", path.display()));
        return;
    }
    print_line(&format!(
        "proxy: {} ban(s) from {}:",
        bans.len(),
        path.display()
    ));
    for ban in bans.iter().take(LIST_LIMIT) {
        print_line(&format!("  {ban}"));
    }
    if bans.len() > LIST_LIMIT {
        print_line(&format!("  ... {} more", bans.len() - LIST_LIMIT));
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

    fn ban(text: &str) -> Ban {
        Ban::parse(text).expect("valid ban")
    }

    fn ip(text: &str) -> Ipv4Addr {
        Ipv4Addr::from_str(text).expect("valid address")
    }

    #[test]
    fn a_bare_address_is_a_single_host() {
        let entry = ban("84.17.32.5");
        assert_eq!(entry.prefix(), 32);
        assert_eq!(entry.canonical(), "84.17.32.5");
        assert!(entry.contains(ip("84.17.32.5")));
        assert!(!entry.contains(ip("84.17.32.6")));
    }

    #[test]
    fn a_range_matches_every_address_inside_it() {
        let entry = ban("84.17.32.0/19");
        assert!(entry.contains(ip("84.17.32.0")));
        assert!(entry.contains(ip("84.17.32.255")));
        assert!(entry.contains(ip("84.17.63.255")));
        assert!(!entry.contains(ip("84.17.31.255")));
        assert!(!entry.contains(ip("84.17.64.0")));
    }

    #[test]
    fn host_bits_are_masked_off() {
        assert_eq!(ban("84.17.32.5/19"), ban("84.17.32.0/19"));
        assert_eq!(ban("84.17.32.5/19").canonical(), "84.17.32.0/19");
        assert_eq!(ban("10.1.2.3/8").canonical(), "10.0.0.0/8");
        assert_eq!(ban("10.1.2.3/24").canonical(), "10.1.2.0/24");
    }

    #[test]
    fn invalid_entries_are_refused() {
        assert_eq!(Ban::parse(""), Err(ParseError::Empty));
        assert_eq!(Ban::parse("   "), Err(ParseError::Empty));
        assert_eq!(Ban::parse("nope"), Err(ParseError::Address));
        assert_eq!(Ban::parse("1.2.3"), Err(ParseError::Address));
        assert_eq!(Ban::parse("1.2.3.256"), Err(ParseError::Address));
        assert_eq!(Ban::parse("010.0.0.1"), Err(ParseError::Address));
        assert_eq!(Ban::parse("1.2.3.4/"), Err(ParseError::Prefix));
        assert_eq!(Ban::parse("1.2.3.4/33"), Err(ParseError::Prefix));
        assert_eq!(Ban::parse("1.2.3.4/x"), Err(ParseError::Prefix));
        assert_eq!(Ban::parse("0.0.0.0/0"), Err(ParseError::PrefixZero));
    }

    #[test]
    fn a_file_skips_comments_blanks_and_reports_bad_lines() {
        let parsed = parse_file(
            b"# header\n\
              10.0.0.0/8\n\
              \n\
              192.168.1.5   # one host\n\
              not-an-ip\n\
              10.0.0.0/8\n\
              172.16.0.0/12\r\n",
        );
        assert_eq!(
            parsed.bans,
            vec![ban("10.0.0.0/8"), ban("172.16.0.0/12"), ban("192.168.1.5")]
        );
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(parsed.errors[0].line, 5);
        assert_eq!(parsed.errors[0].text, "not-an-ip");
    }

    #[test]
    fn a_round_trip_preserves_the_entries() {
        let dir = std::env::temp_dir().join(format!(
            "jampgame-proxy-banlist-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("bans.txt");

        let bans = vec![ban("10.0.0.0/8"), ban("84.17.32.5")];
        save(&path, &bans).expect("save");
        let loaded = load(&path).expect("load");
        assert_eq!(loaded.bans, vec![ban("10.0.0.0/8"), ban("84.17.32.5")]);
        assert!(loaded.errors.is_empty());
        assert!(!temporary_path(&path).exists(), "temporary file is renamed");

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn the_file_is_canonical_and_sorted() {
        let dir = std::env::temp_dir().join(format!(
            "jampgame-proxy-banlist-sort-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("bans.txt");

        save(&path, &[ban("192.168.1.5"), ban("10.0.0.0/8")]).expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.starts_with("# jampgame-proxy-rs"));
        let entries: Vec<&str> = text
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .collect();
        assert_eq!(entries, vec!["10.0.0.0/8", "192.168.1.5"]);

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn removing_a_covered_address_names_the_wider_range() {
        let bans = [ban("10.0.0.0/8")];
        let covering = bans
            .iter()
            .find(|candidate| candidate.prefix < 32 && candidate.contains(ip("10.1.2.3")));
        assert_eq!(covering, Some(&ban("10.0.0.0/8")));
    }

    #[test]
    fn the_engines_ip_value_is_parsed_without_its_port() {
        assert_eq!(
            parse_userinfo_ip(b"172.30.0.1:29070"),
            Some(ip("172.30.0.1"))
        );
        assert_eq!(parse_userinfo_ip(b"1.2.3.4"), Some(ip("1.2.3.4")));
        assert_eq!(parse_userinfo_ip(b"localhost"), None);
        assert_eq!(parse_userinfo_ip(b"LocalHost"), None);
        assert_eq!(parse_userinfo_ip(b""), None);
        assert_eq!(parse_userinfo_ip(b"1.2.3"), None);
        assert_eq!(parse_userinfo_ip(b"::1:29070"), None);
    }
}
