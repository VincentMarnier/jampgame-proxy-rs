//! Opt-in runtime diagnostics (`proxy_sv_debugLog`).
//!
//! The proxy is a network-facing component: when a player crashes the server,
//! the useful evidence is usually the last few things that player sent. This
//! module provides a deliberately narrow, cvar-gated trace:
//!
//! - `0` (default) — completely inert: no formatting, no file, no console
//!   output. Every call site checks the level before building a message.
//! - `1` — suspicious events only: commands the proxy blocked, userinfo/model
//!   fields it had to sanitise, rcon attempts, rejected downloads, malformed
//!   client messages and oversized connectionless queries.
//! - `2` — every client command, every userinfo update and every client
//!   lifecycle event. This is the mode to run while reproducing a crash.
//!
//! Lines go to `proxy_debug.log` in the server's working directory *and* to the
//! engine console (`G_PRINT`, which also feeds `qconsole.log` when the server
//! has `logfile` on). The file line is written and flushed before the console
//! call, so a crash inside the engine/game code cannot lose the command that
//! caused it.
//!
//! Deliberately not logged: the rcon password (rcon lines redact it and only
//! report whether it matched), and unescaped client bytes (control characters
//! are rendered as `\xNN` so a malicious command cannot forge log lines).

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const LEVEL_OFF: i32 = 0;
pub const LEVEL_SUSPICIOUS: i32 = 1;
pub const LEVEL_VERBOSE: i32 = 2;

const LOG_FILE_NAME: &str = "proxy_debug.log";
// Per-field escape budget (the call sites pass 512). Commands/userinfo are at
// most `MAX_TOKEN_CHARS` / `MAX_INFO_STRING`; the original length is still
// reported, so truncating the printable preview keeps lines well below the
// engine's 4096-byte print buffer while never losing the size that matters for
// overflow crashes.

static LEVEL: AtomicI32 = AtomicI32::new(LEVEL_OFF);
static SVS_TIME: AtomicI32 = AtomicI32::new(0);
static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);
static FILE_FAILED: AtomicBool = AtomicBool::new(false);
static FILE_ANNOUNCED: AtomicBool = AtomicBool::new(false);

/// Mirror the `proxy_sv_debugLog` cvar. Called from the per-frame cvar refresh;
/// lock-free so log call sites never contend with proxy state.
pub fn set_level(level: i32) {
    LEVEL.store(level.clamp(LEVEL_OFF, LEVEL_VERBOSE), Ordering::Relaxed);
}

pub fn level() -> i32 {
    LEVEL.load(Ordering::Relaxed)
}

pub fn suspicious_enabled() -> bool {
    level() >= LEVEL_SUSPICIOUS
}

pub fn verbose_enabled() -> bool {
    level() >= LEVEL_VERBOSE
}

/// Track `svs.time` without taking the proxy state lock from a log call site.
pub fn set_svs_time(time: i32) {
    SVS_TIME.store(time, Ordering::Relaxed);
}

/// Log an event that is security-relevant or indicates a malformed client.
pub fn suspicious(category: &str, detail: impl AsRef<str>) {
    if suspicious_enabled() {
        emit("SUSPICIOUS", category, detail.as_ref());
    }
}

/// Log normal client activity; only emitted at `proxy_sv_debugLog 2`.
pub fn verbose(category: &str, detail: impl AsRef<str>) {
    if verbose_enabled() {
        emit("INFO", category, detail.as_ref());
    }
}

/// Escape arbitrary client bytes for a single log line: printable ASCII is
/// kept, `\` and `"` are escaped, control/high bytes become `\xNN`.
pub fn escape_bytes(bytes: &[u8], max: usize) -> String {
    let mut out = String::with_capacity(bytes.len().min(max) + 16);
    let limit = max.min(bytes.len());
    for &b in &bytes[..limit] {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7E => out.push(b as char),
            _ => {
                use std::fmt::Write as _;
                // Writing to a String cannot fail.
                let _ = write!(out, "\\x{b:02x}");
            }
        }
    }
    if bytes.len() > limit {
        use std::fmt::Write as _;
        let _ = write!(out, "...<{} bytes total>", bytes.len());
    }
    out
}

fn emit(kind: &str, category: &str, detail: &str) {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let svs_time = SVS_TIME.load(Ordering::Relaxed);
    let line = format!("t={svs_time} unix_ms={unix_ms} level={kind} event={category} {detail}");

    write_file(&line);

    // The engine's `G_PRINT` trap (Com_Printf "%s") reaches the dedicated
    // console and qconsole.log. Keep the line bounded: escape_bytes caps
    // every client controlled field well below MAXPRINTMSG.
    let mut console = String::with_capacity(line.len() + 1);
    console.push_str(&line);
    console.push('\n');
    if let Ok(cstring) = CString::new(console) {
        // SAFETY: the syscall pointer is registered by dllEntry before any
        // hook can run, and cstring is NUL-terminated.
        unsafe { crate::syscall::print(&cstring) };
    } else {
        eprintln!("{line}");
    }
}

fn write_file(line: &str) {
    let mut guard = LOG_FILE.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        if FILE_FAILED.load(Ordering::Relaxed) {
            return;
        }
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOG_FILE_NAME)
        {
            Ok(mut file) => {
                if !FILE_ANNOUNCED.swap(true, Ordering::Relaxed) {
                    let _ = writeln!(
                        file,
                        "# jampgame-proxy-rs {} diagnostics log (proxy_sv_debugLog {})",
                        crate::VERSION,
                        level()
                    );
                    eprintln!("----- proxy-rs: diagnostics logging to {LOG_FILE_NAME}");
                }
                *guard = Some(file);
            }
            Err(err) => {
                FILE_FAILED.store(true, Ordering::Relaxed);
                eprintln!("----- proxy-rs: cannot open {LOG_FILE_NAME}: {err}");
                return;
            }
        }
    }
    if let Some(file) = guard.as_mut() {
        // Flush per line: the point of this log is surviving a SIGSEGV.
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_is_single_line_and_reports_truncation() {
        assert_eq!(escape_bytes(b"say hello", 64), "say hello");
        assert_eq!(escape_bytes(b"a\nb\rc\td", 64), "a\\nb\\rc\\td");
        assert_eq!(escape_bytes(b"quote\"slash\\", 64), "quote\\\"slash\\\\");
        assert_eq!(
            escape_bytes(&[0x00, 0x1f, 0x7f, 0xff], 64),
            "\\x00\\x1f\\x7f\\xff"
        );
        assert_eq!(escape_bytes(b"abcdef", 3), "abc...<6 bytes total>");
    }

    #[test]
    fn levels_are_clamped_and_off_by_default() {
        set_level(-1);
        assert_eq!(level(), LEVEL_OFF);
        assert!(!suspicious_enabled());
        assert!(!verbose_enabled());
        set_level(1);
        assert!(suspicious_enabled());
        assert!(!verbose_enabled());
        set_level(2);
        assert!(verbose_enabled());
        set_level(99);
        assert_eq!(level(), LEVEL_VERBOSE);
        set_level(LEVEL_OFF);
    }
}
