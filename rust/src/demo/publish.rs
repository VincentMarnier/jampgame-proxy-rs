//! Phase 4 publication: the join-time download hint, retention and the disk
//! guards.
//!
//! Two rules shape this module:
//!
//! - **The game thread never touches the filesystem.** Retention deletion and
//!   `statvfs` run on a maintenance thread started through the C realign shim
//!   (`jampgame_aligned_entry`, D-001.1)); the game thread only reads an atomic
//!   free-space value and publishes the open name set.
//! - **The HTTP thread never reads proxy or engine state.** It receives the
//!   demo directory, the link base, the retention window and the shared
//!   open-segment set as a plain configuration snapshot (see `crate::http`).
//!
//! The recorder calls [`opened`]/[`closed`] for every segment and
//! [`on_game_init`] at each game boundary. Finished files are never printed to
//! the clients: the game only points players at the `/demos` page (see
//! [`downloads_page`] and the join notice in `crate::shared_api`), and the
//! per-file links are served by the HTTP listing only.

use core::ffi::c_int;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::file::{DemoName, Retention, Timestamp};

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
/// join hint uses: it must reflect what the operator configured, not whether a
/// socket happens to be up at that instant.
fn downloads_configured() -> bool {
    crate::state::cvar_integer(crate::state::CVAR_ENABLE_HTTP) != 0
        && crate::state::cvar_integer(crate::state::CVAR_DEMO_DOWNLOAD) != 0
}

/// Whether downloads are enabled for a request: the proxy is on, the HTTP
/// master switch is on and the demo download switch is on. The listener and
/// the join-time hint follow this rule.
pub fn downloads_enabled() -> bool {
    crate::state::proxy_enabled() && downloads_configured()
}

/// The public base for a link (`proxy_sv_demoUrl`), trailing slashes removed.
///
/// When the cvar is empty the listener's own address is used instead, so the
/// join hint shows a real URL rather than a bare path:
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

/// The demo page URL (`<base>/demos`) the join hint points at. Empty when no
/// public base is configured (an invalid listener port has no reachable page).
pub fn page_url(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        String::new()
    } else {
        format!("{base}/demos")
    }
}

/// The demo page URL for the join hint, or `None` while downloads are off or
/// the listener has no usable base. Pure enough to unit-test: it reads only the
/// proxy cvars, never engine state.
pub fn downloads_page() -> Option<String> {
    if !downloads_enabled() {
        return None;
    }
    let page = clean_link(&page_url(&link_base()));
    (!page.is_empty()).then_some(page)
}

/// Drop anything that would break a `print`/`svsay` quoted argument.
fn clean_link(link: &str) -> String {
    link.chars()
        .filter(|c| *c != '"' && *c != '\r' && *c != '\n' && *c != '\\')
        .collect()
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
}

/// A segment is closed **and terminated**: it may now be served.
pub fn closed(file_name: &str) {
    open_set_cell()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(file_name);
}

/// `GAME_INIT`: schedule the retention sweep. Finished files are not
/// announced; their links live in the HTTP listing.
pub fn on_game_init() {
    SWEEP.store(true, Ordering::Relaxed);
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
/// space and sweeping retention for the recorder, or sampling free space for
/// the HTTP listener. With every demo cvar at its default (off) nothing runs,
/// no sweep can delete files, and no thread is started.
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
    let mut last_sample = Instant::now() - Duration::from_secs(10);
    loop {
        if STOP.load(Ordering::Relaxed) {
            return;
        }
        let config = CONFIG.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(config) = config {
            if SWEEP.swap(false, Ordering::Relaxed) {
                sweep(&config);
            }
            if last_sample.elapsed() >= Duration::from_secs(1) {
                last_sample = Instant::now();
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
    fn the_demo_page_url_is_built_from_the_public_base() {
        assert_eq!(page_url("https://example.com"), "https://example.com/demos");
        assert_eq!(
            page_url("https://example.com/"),
            "https://example.com/demos"
        );
        assert_eq!(page_url("  "), "");
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
        assert_eq!(
            page_url(&local_base("0.0.0.0", 8090)),
            "http://127.0.0.1:8090/demos"
        );
    }

    #[test]
    fn the_page_link_cannot_break_out_of_a_quoted_command() {
        let page = clean_link(&page_url("https://example.com/\"evil\\"));
        assert!(!page.contains('"'));
        assert!(!page.contains('\\'));
    }
}
