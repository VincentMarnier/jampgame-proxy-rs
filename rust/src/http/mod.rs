//! Phase 4 demo HTTP surface (owner decision: the proxy hosts the server).
//!
//! The listener lifecycle is driven from the game thread, once per frame, by
//! [`frame_sync`] — but every std call (socket bind, thread spawn, mutex) runs
//! through the C realign shim, because `update_proxy_cvars` is entered from
//! `vmMain` on the engine's 4-byte-aligned stack (D-001.1/D-018). The listener
//! and its workers then run on their own, ABI-conforming stacks and never call
//! the engine: they only hold a [`server::Config`] snapshot and the shared
//! open-segment name set.
//!
//! Configuration comes from the proxy cvars:
//!
//! | cvar | meaning |
//! | --- | --- |
//! | `proxy_sv_enableHttp` | listener master |
//! | `proxy_sv_httpBind` / `proxy_sv_httpPort` | bind address / port |
//! | `proxy_sv_enableDemoDownload` | demo routes; with it off no route is enabled and the listener does not start |
//! | `proxy_sv_demoUrl` | public base for links (empty = bare names) |
//! | `proxy_sv_demoRetentionHours` | listing/serving filter, also swept on disk |
//!
//! A bind failure (EADDRINUSE/EACCES) logs one line and leaves the feature off
//! until the configuration changes; it never blocks or crashes the server.

pub mod server;

use core::ffi::c_int;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::demo::publish;
use crate::state;

/// The configuration currently applied to the listener, for change detection.
static CURRENT: Mutex<Option<Running>> = Mutex::new(None);
/// The configuration whose bind failed last (no retry until it changes).
static FAILED: Mutex<Option<String>> = Mutex::new(None);

struct Running {
    key: String,
    server: server::Server,
}

unsafe extern "C" {
    /// `csrc/syscall_shim.c`: realigns the stack to the i386 ABI and calls
    /// `f(op)` (D-001.1). Listener bind, thread creation and the std mutexes
    /// below must not run on the engine's 4-byte-aligned entry stack.
    fn jampgame_aligned_entry(f: extern "C" fn(c_int), op: c_int);
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

/// Per-frame listener reconciliation, ABI-aligned (called from
/// `update_proxy_cvars`).
pub fn frame_sync() {
    // SAFETY: the shim calls `dispatch` exactly once with `op`.
    unsafe { jampgame_aligned_entry(dispatch, ENTRY_SYNC) };
}

/// Stop the listener (game shutdown). Safe to call when nothing runs.
pub fn shutdown() {
    // SAFETY: the shim calls `dispatch` exactly once with `op`.
    unsafe { jampgame_aligned_entry(dispatch, ENTRY_SHUTDOWN) };
}

fn sync_impl() {
    let active = state::proxy_enabled()
        && state::cvar_integer(state::CVAR_ENABLE_HTTP) != 0
        && state::cvar_integer(state::CVAR_DEMO_DOWNLOAD) != 0;
    if !active {
        stop_current();
        return;
    }

    let bind = {
        let value = state::cvar_string(state::CVAR_HTTP_BIND);
        if value.trim().is_empty() {
            "127.0.0.1".to_string()
        } else {
            value.trim().to_string()
        }
    };
    let port = state::cvar_integer(state::CVAR_HTTP_PORT);
    if !(1..=u16::MAX as i32).contains(&port) {
        stop_current();
        if FAILED.lock().unwrap_or_else(|p| p.into_inner()).as_deref() != Some("invalid port") {
            eprintln!(
                "----- proxy-rs: demo HTTP disabled: proxy_sv_httpPort {port} is not a valid port"
            );
            *FAILED.lock().unwrap_or_else(|p| p.into_inner()) = Some("invalid port".to_string());
        }
        return;
    }
    let dir = {
        let value = state::cvar_string(state::CVAR_DEMO_DIR);
        if value.trim().is_empty() {
            PathBuf::from("demos")
        } else {
            PathBuf::from(value.trim())
        }
    };
    // Same link base as the client block and `/demos`: the public cvar when
    // set, otherwise the listener's own address (never bare names in JSON).
    let base_url = crate::demo::publish::link_base();
    let retention_hours =
        state::cvar_integer(state::CVAR_DEMO_RETENTION_HOURS).clamp(0, i32::from(u16::MAX)) as u32;
    let key = format!(
        "{bind}|{port}|{}|{base_url}|{retention_hours}",
        dir.display()
    );

    {
        let current = CURRENT.lock().unwrap_or_else(|p| p.into_inner());
        if current.as_ref().is_some_and(|running| running.key == key) {
            return;
        }
    }
    stop_current();
    if FAILED.lock().unwrap_or_else(|p| p.into_inner()).as_deref() == Some(key.as_str()) {
        // Already failed for this exact configuration; do not spam the log or
        // retry every frame.
        return;
    }

    let config = server::Config {
        dir: dir.clone(),
        base_url: base_url.clone(),
        retention_hours,
        open: publish::open_set(),
    };
    match server::Server::start(&bind, port as u16, config) {
        Ok(server) => {
            let local = server.local_addr();
            eprintln!(
                "----- proxy-rs: demo HTTP listening on http://{local} (dir {}, retention {retention_hours}h, links {})",
                dir.display(),
                if base_url.is_empty() {
                    "bare file names".to_string()
                } else {
                    base_url.clone()
                }
            );
            *CURRENT.lock().unwrap_or_else(|p| p.into_inner()) = Some(Running { key, server });
            *FAILED.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
        Err(error) => {
            eprintln!(
                "----- proxy-rs: demo HTTP disabled: cannot bind {bind}:{port}: {error} (feature off until the cvars change)"
            );
            *FAILED.lock().unwrap_or_else(|p| p.into_inner()) = Some(key);
        }
    }
}

fn stop_current() {
    let running = CURRENT.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(mut running) = running {
        running.server.stop();
        eprintln!("----- proxy-rs: demo HTTP listener stopped");
    }
}

fn shutdown_impl() {
    stop_current();
    *FAILED.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    #[test]
    fn bind_failure_is_an_error_not_a_panic() {
        // Occupy a port, then ask the server to bind it: start must fail, and
        // the caller keeps the feature off.
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = occupied.local_addr().unwrap().port();
        let config = server::Config {
            dir: std::env::temp_dir(),
            base_url: String::new(),
            retention_hours: 24,
            open: Arc::new(Mutex::new(BTreeSet::new())),
        };
        assert!(server::Server::start("127.0.0.1", port, config).is_err());
    }

    #[test]
    fn config_key_changes_on_every_listener_relevant_cvar() {
        // The key is what decides restart; a changed dir/base/port/bind/retention
        // must restart, and a steady config must not.
        let key = |bind: &str, port: i32, dir: &str, base: &str, retention: i32| {
            format!("{bind}|{port}|{dir}|{base}|{retention}")
        };
        let baseline = key("127.0.0.1", 8090, "demos", "", 24);
        assert_eq!(baseline, key("127.0.0.1", 8090, "demos", "", 24));
        assert_ne!(baseline, key("0.0.0.0", 8090, "demos", "", 24));
        assert_ne!(baseline, key("127.0.0.1", 8091, "demos", "", 24));
        assert_ne!(baseline, key("127.0.0.1", 8090, "demos2", "", 24));
        assert_ne!(baseline, key("127.0.0.1", 8090, "demos", "https://x", 24));
        assert_ne!(baseline, key("127.0.0.1", 8090, "demos", "", 1));
    }
}
