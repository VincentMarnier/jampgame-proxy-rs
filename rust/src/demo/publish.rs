//! Phase 4 publication: generation bookkeeping, `/demos`, boundary
//! announcements, retention and the disk guards.
//!
//! Two rules shape this module:
//!
//! - **The game thread never touches the filesystem.** Directory scans,
//!   retention deletion and `statvfs` run on a maintenance thread started
//!   through the C realign shim (`jampgame_aligned_entry`, D-001.1)); the game
//!   thread only reads an atomic free-space value and a published name set.
//! - **The HTTP thread never reads proxy or engine state.** It receives the
//!   demo directory, the link base, the retention window and the shared
//!   open-segment set as a plain configuration snapshot (see `crate::http`).
//!
//! The recorder calls [`opened`]/[`closed`] for every segment and
//! [`on_game_init`] at each game boundary; the boundary queues a `print` block
//! with the finished files and their download links for the clients (console
//! text like the end-game stats, never `svsay`), sent once a player is back in
//! the world.

use core::ffi::{CStr, c_int};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::file::{DemoName, Retention, Timestamp};

/// Maximum number of file lines in one boundary announcement block. The
/// trailer ("use /demos") is added on top when there are more.
pub const ANNOUNCEMENT_FILE_LINES: usize = 12;
/// Maximum number of files listed by the `/demos` client command.
pub const DEMOS_COMMAND_LINES: usize = 32;

/// One segment of the current game generation.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    closed: bool,
}

#[derive(Default)]
struct State {
    /// Segments started since the last `GAME_INIT`, in start order.
    files: Vec<Entry>,
    /// Matching demo files the maintenance thread last saw on disk (not
    /// expired), plus files this generation just closed. `/demos` lists the
    /// intersection with `files`.
    existing: BTreeSet<String>,
    /// Client block waiting for a player to be back in the world (a full map
    /// change runs `GAME_INIT` before anyone reconnects).
    pending: Vec<String>,
    /// The generation was already announced at intermission; `GAME_INIT` must
    /// not announce it a second time.
    announced: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    files: Vec::new(),
    existing: BTreeSet::new(),
    pending: Vec::new(),
    announced: false,
});

/// Names of the segments that are still open (the HTTP side never serves
/// them). Shared with the listener as plain proxy memory, never engine data.
fn open_arc() -> &'static std::sync::Arc<Mutex<BTreeSet<String>>> {
    static OPEN: OnceLock<std::sync::Arc<Mutex<BTreeSet<String>>>> = OnceLock::new();
    OPEN.get_or_init(|| std::sync::Arc::new(Mutex::new(BTreeSet::new())))
}

fn open_set_cell() -> &'static Mutex<BTreeSet<String>> {
    open_arc()
}

/// The shared open-segment set for the HTTP configuration snapshot.
pub fn open_set() -> std::sync::Arc<Mutex<BTreeSet<String>>> {
    open_arc().clone()
}

/// Free bytes on the demo directory's filesystem, `-1` while unknown.
static FREE_BYTES: AtomicI64 = AtomicI64::new(-1);
/// A retention sweep is due (set at each boundary and at thread start).
static SWEEP: AtomicBool = AtomicBool::new(true);
/// The maintenance thread should exit.
static STOP: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, PartialEq, Eq)]
struct MaintenanceConfig {
    dir: PathBuf,
    retention_hours: u32,
}

static CONFIG: Mutex<Option<MaintenanceConfig>> = Mutex::new(None);
static THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

/// The last free-space reading for the recorder's min-free guard.
pub fn free_bytes() -> i64 {
    FREE_BYTES.load(Ordering::Relaxed)
}

/// Whether the two download cvars are on. This is the configuration check the
/// boundary uses: `GAME_INIT` runs on the way from a shutdown to a fresh
/// session, and the master-switch atomic there is not what decides whether
/// downloads were configured.
fn downloads_configured() -> bool {
    crate::state::cvar_integer(crate::state::CVAR_ENABLE_HTTP) != 0
        && crate::state::cvar_integer(crate::state::CVAR_DEMO_DOWNLOAD) != 0
}

/// Whether downloads are enabled for a request: the proxy is on, the HTTP
/// master switch is on and the demo download switch is on. The listener,
/// `/demos` and the announcement delivery follow this rule.
pub fn downloads_enabled() -> bool {
    crate::state::proxy_enabled() && downloads_configured()
}

/// The public base for a link (`proxy_sv_demoUrl`), trailing slashes removed.
///
/// When the cvar is empty the listener's own address is used instead, so the
/// console and `/demos` print real URLs rather than bare file names:
/// `http://<httpBind>:<httpPort>`, with a wildcard bind (`0.0.0.0`, `::`)
/// shown as `127.0.0.1` for the local operator. A public host still belongs in
/// `proxy_sv_demoUrl` (NAT/reverse proxy).
fn local_base(bind: &str, port: i32) -> String {
    if port <= 0 || port > i32::from(u16::MAX) {
        return String::new();
    }
    let bind = bind.trim();
    let host = match bind {
        "" | "0.0.0.0" | "::" | "[::]" | "*" => "127.0.0.1".to_string(),
        other if other.contains(':') && !other.starts_with('[') => format!("[{other}]"),
        other => other.to_string(),
    };
    format!("http://{host}:{port}")
}

pub(crate) fn link_base() -> String {
    let configured = crate::state::cvar_string(crate::state::CVAR_DEMO_URL)
        .trim()
        .trim_end_matches('/')
        .to_string();
    if !configured.is_empty() {
        return configured;
    }
    local_base(
        &crate::state::cvar_string(crate::state::CVAR_HTTP_BIND),
        crate::state::cvar_integer(crate::state::CVAR_HTTP_PORT),
    )
}

/// The link shown for `file_name`: `<base>/demos/<name>`, or the bare name
/// when no public base is configured.
pub fn link_for(base: &str, file_name: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        file_name.to_string()
    } else {
        format!("{base}/demos/{file_name}")
    }
}

/// Drop anything that would break a `print`/`svsay` quoted argument.
fn clean_link(link: &str) -> String {
    link.chars()
        .filter(|c| *c != '"' && *c != '\r' && *c != '\n' && *c != '\\')
        .collect()
}

/// The `print` lines of one boundary block for the clients: a header, at most
/// [`ANNOUNCEMENT_FILE_LINES`] download links, then a `/demos` hint when
/// truncated. Pure so the cap and the link shape are unit-tested.
pub fn announcement_lines(names: &[String], base: &str) -> Vec<String> {
    let mut lines = vec!["^5Demo files from this game:".to_string()];
    for name in names.iter().take(ANNOUNCEMENT_FILE_LINES) {
        lines.push(format!("^7{}", clean_link(&link_for(base, name))));
    }
    if names.len() > ANNOUNCEMENT_FILE_LINES {
        lines.push(format!(
            "^7... {} more demo(s), type /demos",
            names.len() - ANNOUNCEMENT_FILE_LINES
        ));
    }
    lines
}

// ---------------------------------------------------------------------------
// Recorder hooks (game thread, already on an aligned stack)
// ---------------------------------------------------------------------------

/// A segment was created: it is not servable until it is closed.
pub fn opened(file_name: &str) {
    open_set_cell()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(file_name.to_string());
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    state.files.push(Entry {
        name: file_name.to_string(),
        closed: false,
    });
}

/// A segment is closed **and terminated**: it may now be served.
pub fn closed(file_name: &str) {
    open_set_cell()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(file_name);
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(entry) = state.files.iter_mut().find(|e| e.name == file_name) {
        entry.closed = true;
    } else {
        state.files.push(Entry {
            name: file_name.to_string(),
            closed: true,
        });
    }
    // We just wrote it; it exists even if the next scan has not run yet.
    state.existing.insert(file_name.to_string());
}

/// `GAME_INIT`: announce the generation that just ended, start a new one and
/// schedule the retention sweep.
pub fn on_game_init() {
    let base = link_base();
    let print_block = downloads_configured();
    let (names, lines) = {
        let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
        let closed: Vec<String> = state
            .files
            .iter()
            .filter(|e| e.closed)
            .map(|e| e.name.clone())
            .collect();
        // Already printed at intermission: clear the generation without a
        // second block at the next game start.
        let lines = if print_block && !state.announced {
            announcement_lines(&closed, &base)
        } else {
            Vec::new()
        };
        state.files.clear();
        state.announced = false;
        state.pending = lines.clone();
        (closed, lines)
    };
    // The directory scan may not have seen them yet; closed segments are
    // already in `existing` (see `closed`).
    SWEEP.store(true, Ordering::Relaxed);
    if !names.is_empty() {
        // Console output stays out of the way: the file block goes to the
        // clients as `print` commands (like the end-game stats); this detail
        // line is only for the crash-diagnostics log.
        crate::diagnostics::verbose(
            "demo.boundary",
            format!(
                "{} file(s) closed, {} client line(s) queued",
                names.len(),
                lines.len()
            ),
        );
    }
}

/// Send the queued boundary block to the clients once someone is back in the
/// world. It is a `print` broadcast (console text, exactly like the end-game
/// stats tables), never `svsay`: the demo list does not go to the server
/// console and is not chat.
pub fn flush_announcement(any_in_world: bool) {
    if !any_in_world || !downloads_enabled() {
        return;
    }
    let lines = {
        let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
        core::mem::take(&mut state.pending)
    };
    for line in lines {
        let text = line.replace('"', "'");
        let Ok(command) = std::ffi::CString::new(format!("print \"{text}\n\"")) else {
            continue;
        };
        // SAFETY: engine syscall registered; `command` is NUL-terminated and
        // the engine copies it during the call. `-1` broadcasts to every
        // connected client, like the stats' global table.
        unsafe { crate::syscall::send_server_command(-1, &command) };
    }
}

/// `BeginIntermission` + `proxy_sv_demoIntermissionSecs`: the intermission
/// tail has been recorded, the recorder closed its segments, and the clients
/// are still in the intermission screen. Print the block to them now (console
/// text, like the stats) and mark the generation announced so `GAME_INIT` does
/// not repeat it. `/demos` keeps listing the files until the next game.
pub fn announce_intermission() {
    let base = link_base();
    let print_block = downloads_configured();
    let (lines, names) = {
        let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
        let names: Vec<String> = state
            .files
            .iter()
            .filter(|e| e.closed)
            .map(|e| e.name.clone())
            .collect();
        state.announced = true;
        let lines = if print_block {
            announcement_lines(&names, &base)
        } else {
            Vec::new()
        };
        (lines, names)
    };
    if names.is_empty() {
        return;
    }
    crate::diagnostics::verbose(
        "demo.intermission",
        format!(
            "{} file(s) announced at intermission, {} client line(s)",
            names.len(),
            lines.len()
        ),
    );
    for line in &lines {
        crate::diagnostics::verbose("demo.intermission.line", line.clone());
    }
    for line in lines {
        let text = line.replace('"', "'");
        let Ok(command) = std::ffi::CString::new(format!("print \"{text}\n\"")) else {
            continue;
        };
        // SAFETY: engine syscall registered; the command is NUL-terminated and
        // the engine copies it. `-1` is every connected client; at
        // intermission they are all still connected.
        unsafe { crate::syscall::send_server_command(-1, &command) };
    }
}

/// `/demos`: list the current generation's closed, still existing files to
/// the requester. Returns `false` when the command is not ours.
pub fn handle_client_command(client_num: c_int, command: &[u8]) -> bool {
    if !command.eq_ignore_ascii_case(b"demos") {
        return false;
    }
    if !downloads_enabled() {
        print_line(
            client_num,
            "^5Demos: ^7demo downloads are disabled on this server.\n",
        );
        return true;
    }
    let base = link_base();
    let names = {
        let state = STATE.lock().unwrap_or_else(|p| p.into_inner());
        let mut names: Vec<String> = state
            .files
            .iter()
            .filter(|e| e.closed && state.existing.contains(&e.name))
            .map(|e| e.name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    };
    if names.is_empty() {
        print_line(client_num, "^5Demos: ^7none recorded for this game yet.\n");
        return true;
    }
    print_line(client_num, "^5Demos for this game:\n");
    for name in names.iter().take(DEMOS_COMMAND_LINES) {
        let link = clean_link(&link_for(&base, name));
        // Operational trace: the game side cannot be inspected from a client
        // capture reliably (the tool's netchan state after a client command),
        // so the served links are logged here.
        eprintln!(
            "----- proxy-rs: /demos from client {client_num}: {link} ({} available)",
            names.len()
        );
        print_line(client_num, &format!("^7{link}\n"));
    }
    if names.len() > DEMOS_COMMAND_LINES {
        print_line(
            client_num,
            &format!(
                "^7… and {} more (see /demos on the web).\n",
                names.len() - DEMOS_COMMAND_LINES
            ),
        );
    }
    true
}

fn print_line(client_num: c_int, text: &str) {
    let cleaned = text.replace('"', "'");
    let Ok(command) = std::ffi::CString::new(format!("print \"{cleaned}\"")) else {
        return;
    };
    // SAFETY: engine syscall registered; the command outlives the call.
    unsafe { crate::syscall::send_server_command(client_num, &command) };
}

// ---------------------------------------------------------------------------
// Maintenance thread (filesystem side)
// ---------------------------------------------------------------------------

unsafe extern "C" {
    /// `csrc/syscall_shim.c`: realigns the stack to the i386 ABI and calls
    /// `f(op)`. Thread creation and the std setup below must not run on the
    /// engine's 4-byte-aligned entry stack (D-001.1/D-018).
    fn jampgame_aligned_entry(f: extern "C" fn(c_int), op: c_int);
    /// `csrc/syscall_shim.c`: `statvfs(dir).f_bavail * f_frsize`, `-1` on
    /// error. Written in C because the `struct statvfs` layout is the C ABI's.
    fn jampgame_free_bytes(path: *const std::ffi::c_char) -> i64;
}

const ENTRY_SYNC: c_int = 0;
const ENTRY_SHUTDOWN: c_int = 1;

extern "C" fn dispatch(op: c_int) {
    match op {
        ENTRY_SYNC => sync_impl(),
        ENTRY_SHUTDOWN => shutdown_impl(),
        _ => {}
    }
}

/// Per-frame configuration sync, ABI-aligned (called from
/// `update_proxy_cvars`).
pub fn frame_sync() {
    // SAFETY: the shim calls `dispatch` exactly once with `op`.
    unsafe { jampgame_aligned_entry(dispatch, ENTRY_SYNC) };
}

/// Stop the maintenance thread (module shutdown / master switch off).
pub fn shutdown() {
    // SAFETY: the shim calls `dispatch` exactly once with `op`.
    unsafe { jampgame_aligned_entry(dispatch, ENTRY_SHUTDOWN) };
}

/// The maintenance thread exists only to serve a demo feature: sampling free
/// space and retention/scan for the recorder, or the directory scan behind
/// `/demos`/HTTP. With every demo cvar at its default (off) nothing runs, no
/// directory is scanned, no sweep can delete files, and no thread is started.
fn demo_maintenance_needed(record: bool, download: bool) -> bool {
    record || download
}

fn sync_impl() {
    if !crate::state::proxy_enabled() {
        shutdown_impl();
        return;
    }
    let record = crate::state::cvar_integer(crate::state::CVAR_DEMO_ENABLE) != 0;
    let download = crate::state::cvar_integer(crate::state::CVAR_DEMO_DOWNLOAD) != 0;
    if !demo_maintenance_needed(record, download) {
        shutdown_impl();
        return;
    }
    let dir = {
        let value = crate::state::cvar_string(crate::state::CVAR_DEMO_DIR);
        if value.trim().is_empty() {
            PathBuf::from("demos")
        } else {
            PathBuf::from(value.trim())
        }
    };
    let retention_hours = crate::state::cvar_integer(crate::state::CVAR_DEMO_RETENTION_HOURS)
        .clamp(0, i32::from(u16::MAX)) as u32;
    let desired = MaintenanceConfig {
        dir,
        retention_hours,
    };
    let changed = {
        let mut config = CONFIG.lock().unwrap_or_else(|p| p.into_inner());
        let changed = config.as_ref() != Some(&desired);
        if changed {
            *config = Some(desired);
        }
        changed
    };
    if changed {
        // A new directory/retention pair: sweep it once.
        SWEEP.store(true, Ordering::Relaxed);
    }
    ensure_thread();
}

fn ensure_thread() {
    let mut handle = THREAD.lock().unwrap_or_else(|p| p.into_inner());
    if handle.is_some() {
        return;
    }
    STOP.store(false, Ordering::Relaxed);
    match std::thread::Builder::new()
        .name("proxy-demo-maintenance".to_string())
        .spawn(maintenance_loop)
    {
        Ok(joined) => {
            eprintln!("----- proxy-rs: demo maintenance thread started");
            *handle = Some(joined);
        }
        Err(error) => {
            eprintln!("----- proxy-rs: demo maintenance thread: {error}");
            // Leave `handle` empty; the next frame tries again.
        }
    }
}

fn shutdown_impl() {
    STOP.store(true, Ordering::Relaxed);
    let handle = THREAD.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(handle) = handle {
        let _ = handle.join();
        eprintln!("----- proxy-rs: demo maintenance thread stopped");
    }
    *CONFIG.lock().unwrap_or_else(|p| p.into_inner()) = None;
    FREE_BYTES.store(-1, Ordering::Relaxed);
}

fn maintenance_loop() {
    let mut last_scan = Instant::now() - Duration::from_secs(10);
    loop {
        if STOP.load(Ordering::Relaxed) {
            return;
        }
        let config = CONFIG.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(config) = config {
            if SWEEP.swap(false, Ordering::Relaxed) {
                sweep(&config);
            }
            if last_scan.elapsed() >= Duration::from_secs(1) {
                last_scan = Instant::now();
                scan(&config);
                FREE_BYTES.store(free_bytes_of(&config.dir), Ordering::Relaxed);
            }
        }
        // Wake up often enough that shutdown does not wait a full second.
        for _ in 0..10 {
            if STOP.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Recount the matching, not-yet-expired demo files (the set `/demos` and the
/// HTTP listing intersect with).
fn scan(config: &MaintenanceConfig) {
    let now = Timestamp::now().ok();
    let retention = Retention::new(config.retention_hours);
    let mut names = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(&config.dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if DemoName::parse(&name).is_none() {
            continue;
        }
        let expired = match now {
            Some(now) => retention.is_expired(&name, now),
            None => false,
        };
        if expired {
            continue;
        }
        names.insert(name);
    }
    STATE.lock().unwrap_or_else(|p| p.into_inner()).existing = names;
}

/// Delete matching, expired, **closed** files. Foreign names are never
/// touched, and an open segment is never deleted even if its timestamp is
/// old (a very long game with a short retention).
fn sweep(config: &MaintenanceConfig) {
    let Some(now) = Timestamp::now().ok() else {
        return;
    };
    let retention = Retention::new(config.retention_hours);
    if retention.hours == 0 {
        eprintln!(
            "----- proxy-rs: demo retention (keep forever): nothing deleted in {}",
            config.dir.display()
        );
        return;
    }
    let mut deleted = 0usize;
    let mut kept = 0usize;
    let mut failed = 0usize;
    let Ok(entries) = std::fs::read_dir(&config.dir) else {
        eprintln!(
            "----- proxy-rs: demo retention ({}h): directory {} is not readable",
            retention.hours,
            config.dir.display()
        );
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if DemoName::parse(&name).is_none() {
            // Foreign files (and directories) are never deleted.
            continue;
        }
        if open_set_cell()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&name)
        {
            kept += 1;
            continue;
        }
        if !retention.is_expired(&name, now) {
            kept += 1;
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => deleted += 1,
            Err(_) => failed += 1,
        }
    }
    let suffix = if failed > 0 {
        format!(", {failed} failed")
    } else {
        String::new()
    };
    eprintln!(
        "----- proxy-rs: demo retention ({}h): deleted {deleted}, kept {kept}{suffix} in {}",
        retention.hours,
        config.dir.display()
    );
}

fn free_bytes_of(dir: &Path) -> i64 {
    let Ok(path) = std::ffi::CString::new(dir.to_string_lossy().as_bytes()) else {
        return -1;
    };
    // SAFETY: `path` is NUL-terminated; the C helper only reads it.
    unsafe { jampgame_free_bytes(path.as_ptr()) }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_use_the_public_base_or_the_bare_name() {
        let name = "20260101-000000_mp_ffa3_main_s1.dm_26";
        assert_eq!(link_for("", name), name);
        assert_eq!(
            link_for("https://example.com", name),
            format!("https://example.com/demos/{name}")
        );
        assert_eq!(
            link_for("https://example.com/", name),
            format!("https://example.com/demos/{name}")
        );
    }

    #[test]
    fn maintenance_runs_only_while_a_demo_feature_is_on() {
        assert!(!demo_maintenance_needed(false, false));
        assert!(demo_maintenance_needed(true, false));
        assert!(demo_maintenance_needed(false, true));
        assert!(demo_maintenance_needed(true, true));
    }

    #[test]
    fn the_empty_base_falls_back_to_the_local_listener_url() {
        assert_eq!(local_base("0.0.0.0", 8090), "http://127.0.0.1:8090");
        assert_eq!(local_base("::", 8090), "http://127.0.0.1:8090");
        assert_eq!(local_base("127.0.0.1", 30000), "http://127.0.0.1:30000");
        assert_eq!(local_base("10.0.0.5", 8080), "http://10.0.0.5:8080");
        assert_eq!(local_base("::1", 8080), "http://[::1]:8080");
        assert_eq!(local_base("0.0.0.0", 0), "");
        let name = "20260101-000000_mp_ffa3_main_s1.dm_26";
        assert_eq!(
            link_for(&local_base("0.0.0.0", 8090), name),
            format!("http://127.0.0.1:8090/demos/{name}")
        );
    }

    #[test]
    fn announcement_caps_at_twelve_links_plus_header_and_trailer() {
        let names: Vec<String> = (1..=14)
            .map(|n| format!("20260101-000000_mp_ffa3_main_s{n}.dm_26"))
            .collect();
        let lines = announcement_lines(&names, "https://example.com");
        assert_eq!(lines.len(), 1 + ANNOUNCEMENT_FILE_LINES + 1);
        assert!(lines[0].starts_with("^5Demo files"));
        assert!(lines[1].starts_with("^7https://example.com/demos/"));
        assert!(lines[13].contains("2 more demo(s)"));

        let lines = announcement_lines(&names[..3], "http://127.0.0.1:8090");
        assert_eq!(lines.len(), 4);
        assert!(lines[1].starts_with("^7http://127.0.0.1:8090/demos/"));
        assert!(!lines.iter().any(|l| l.contains("more demo")));
    }

    #[test]
    fn links_cannot_break_out_of_a_quoted_command() {
        let lines = announcement_lines(
            &["20260101-000000_mp_ffa3_main_s1.dm_26".to_string()],
            "https://example.com/\"evil\\",
        );
        assert!(!lines[1].contains('"'));
        assert!(!lines[1].contains('\\'));
    }
}
