//! The demo HTTP server: request parsing, routes and static file serving.
//!
//! This module runs entirely on its own threads (`proxy-demo-http`, plus four
//! workers). It receives a [`Config`] snapshot and the shared open-segment set
//! at start; it never calls an engine function, never reads engine memory and
//! never touches `crate::state`. Everything it needs from the game side is
//! copied into the snapshot or into the open set, which holds plain file names.
//!
//! Routes (GET/HEAD only):
//!
//! - `/demos` — the demo names that are closed, still present and not expired;
//! - `/demos/<file>` — the file itself (`application/octet-stream`);
//! - `/api/...` — reserved (501);
//! - everything else — 404.
//!
//! Safety properties: request headers are capped at 8 KiB, one request per
//! connection (`Connection: close`), a fixed worker pool with a bounded queue,
//! sockets carry read/write timeouts, and only names that satisfy
//! [`DemoName::parse`] are resolved inside the demo directory (canonicalised,
//! so a symlink escape is rejected). The TOCTOU window between canonicalising
//! and opening is accepted and documented in `docs/demo-format.md` §13.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::demo::file::{DemoName, Retention, Timestamp};

/// Concurrent response workers.
pub const WORKERS: usize = 4;
/// Connections that may wait for a worker before new ones are refused.
pub const QUEUE: usize = 8;
/// Maximum accepted request-header block (request line + headers).
pub const MAX_HEADER_BYTES: usize = 8 * 1024;
/// Read timeout for one request.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Write timeout for one response chunk.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(15);
/// Copy buffer for a file body.
const SEND_CHUNK: usize = 64 * 1024;
/// Upper bound on names returned by one listing.
pub const MAX_LISTED_FILES: usize = 500;

/// Closed demo files carry the writer's `-1/-1` terminator as their last
/// eight bytes; the HTTP side refuses anything else.
const TERMINATOR: [u8; 8] = [0xFF; 8];

/// Everything the HTTP threads may know about the game side.
#[derive(Clone)]
pub struct Config {
    /// Directory holding the demo files.
    pub dir: PathBuf,
    /// Public base used to build the `url` field of the JSON listing.
    pub base_url: String,
    /// `proxy_sv_demoRetentionHours`, for listing/serving expired names.
    pub retention_hours: u32,
    /// Segments still being written; never served.
    pub open: Arc<Mutex<BTreeSet<String>>>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("dir", &self.dir)
            .field("base_url", &self.base_url)
            .field("retention_hours", &self.retention_hours)
            .finish_non_exhaustive()
    }
}

/// One running listener. Dropping it without [`Server::stop`] detaches the
/// listener thread (the proxy always stops explicitly).
pub struct Server {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    local: SocketAddr,
}

impl Server {
    /// Bind and start the listener and its workers.
    pub fn start(bind: &str, port: u16, config: Config) -> io::Result<Self> {
        let listener = TcpListener::bind(socket_addr(bind, port)?)?;
        listener.set_nonblocking(true)?;
        let local = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let join = thread::Builder::new()
            .name("proxy-demo-http".to_string())
            .spawn(move || accept_loop(listener, config, thread_stop))?;
        Ok(Self {
            stop,
            join: Some(join),
            local,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Stop accepting and release the socket. Workers are detached: one may
    /// finish an in-flight body while the game thread continues.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn socket_addr(bind: &str, port: u16) -> io::Result<SocketAddr> {
    let bind = if bind.trim().is_empty() {
        "127.0.0.1"
    } else {
        bind.trim()
    };
    let text = if bind.contains(':') {
        format!("[{bind}]:{port}")
    } else {
        format!("{bind}:{port}")
    };
    text.parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid bind address"))
}

fn accept_loop(listener: TcpListener, config: Config, stop: Arc<AtomicBool>) {
    // One small queue per worker: the accept loop round-robins connections,
    // which caps concurrency at `WORKERS` without an unbounded thread per
    // client. When a worker's queue is full the connection gets a 503.
    let queue_per_worker = (QUEUE / WORKERS).max(1);
    let mut senders: Vec<SyncSender<TcpStream>> = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let (sender, receiver) = mpsc::sync_channel::<TcpStream>(queue_per_worker);
        let worker_config = config.clone();
        let _ = thread::Builder::new()
            .name("proxy-demo-http-worker".to_string())
            .spawn(move || worker_loop(receiver, worker_config));
        senders.push(sender);
    }
    let mut next_worker = 0usize;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                if stream.set_nonblocking(false).is_ok() {
                    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
                    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
                    let sender = &senders[next_worker];
                    next_worker = (next_worker + 1) % WORKERS;
                    match sender.try_send(stream) {
                        Ok(()) => {}
                        Err(mpsc::TrySendError::Full(mut stream)) => {
                            // Bounded worker set: say so instead of spawning an
                            // unbounded thread.
                            let _ = write_error(&mut stream, 503, "Service Unavailable", true);
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => break,
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
    // Dropping the senders ends the workers after their current response.
    drop(senders);
}

fn worker_loop(receiver: Receiver<TcpStream>, config: Config) {
    while let Ok(mut stream) = receiver.recv() {
        handle_connection(&mut stream, &config);
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

fn handle_connection(stream: &mut TcpStream, config: &Config) {
    let header = match read_header(stream) {
        Ok(header) => header,
        Err(status) => {
            let _ = write_error(stream, status, reason(status), true);
            return;
        }
    };
    let request = match parse_request(&header) {
        Ok(request) => request,
        Err(status) => {
            let _ = write_error(stream, status, reason(status), false);
            return;
        }
    };
    match request.route {
        Route::Listing => {
            let body = listing_body(config);
            let _ = write_small(
                stream,
                200,
                "OK",
                "application/json; charset=utf-8",
                &body,
                request.head,
                false,
            );
        }
        Route::Api => {
            let _ = write_small(
                stream,
                501,
                "Not Implemented",
                "text/plain; charset=utf-8",
                b"reserved\n",
                request.head,
                false,
            );
        }
        Route::NotFound => {
            let _ = write_error(stream, 404, "Not Found", request.head);
        }
        Route::File(name) => serve_file(stream, config, &name, request.head),
    }
}

/// Read one request-header block. The returned slice contains the header
/// lines without the terminating blank line.
fn read_header(stream: &mut TcpStream) -> Result<Vec<u8>, u16> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return Err(400),
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(end) = find_header_end(&buffer) {
                    if end > MAX_HEADER_BYTES {
                        return Err(431);
                    }
                    buffer.truncate(end);
                    return Ok(buffer);
                }
                if buffer.len() > MAX_HEADER_BYTES {
                    return Err(431);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(408);
            }
            Err(_) => return Err(400),
        }
    }
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|w| w == b"\r\n\r\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    Listing,
    File(String),
    Api,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    head: bool,
    route: Route,
}

/// Parse the request line and route it. Pure, so traversal and method
/// handling are unit-tested without sockets.
fn parse_request(header: &[u8]) -> Result<Request, u16> {
    let text = std::str::from_utf8(header).map_err(|_| 400u16)?;
    let request_line = text.split("\r\n").next().ok_or(400u16)?;
    let mut words = request_line.split(' ');
    let method = words.next().ok_or(400u16)?;
    let target = words.next().ok_or(400u16)?;
    let version = words.next().ok_or(400u16)?;
    if words.next().is_some() {
        return Err(400);
    }
    if !version.starts_with("HTTP/1.") {
        return Err(400);
    }
    let head = match method {
        "GET" => false,
        "HEAD" => true,
        _ => return Err(405),
    };
    let path = target.split('?').next().unwrap_or(target);
    if !path.starts_with('/') {
        return Err(404);
    }
    let decoded = percent_decode(path).ok_or(400u16)?;
    let path = String::from_utf8(decoded).map_err(|_| 404u16)?;
    let route = if path == "/demos" || path == "/demos/" {
        Route::Listing
    } else if let Some(name) = path.strip_prefix("/demos/") {
        if safe_file_name(name) {
            Route::File(name.to_string())
        } else {
            Route::NotFound
        }
    } else if path == "/api" || path.starts_with("/api/") {
        Route::Api
    } else {
        Route::NotFound
    };
    Ok(Request { head, route })
}

/// Only the Phase 1 pattern is servable. `DemoName::parse` rejects slashes,
/// backslashes, `..`, NUL bytes, unknown instances and a non-sanitised map, so
/// a traversal attempt can never become a path component.
fn safe_file_name(name: &str) -> bool {
    !name.is_empty() && DemoName::parse(name).is_some()
}

/// Percent-decode a path. `%2e%2e%2f` and friends are decoded **before** the
/// route/name validation, so an encoded traversal reaches `safe_file_name`
/// as `../…` and is rejected there.
fn percent_decode(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    return None;
                }
                let high = hex_value(bytes[index + 1])?;
                let low = hex_value(bytes[index + 2])?;
                out.push((high << 4) | low);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The names `/demos` returns and `/demos/<file>` may serve: pattern-matching,
/// closed (terminator present), not open, not expired, with their sizes.
fn servable_names(config: &Config) -> Vec<(String, u64)> {
    let now = Timestamp::now().ok();
    let retention = Retention::new(config.retention_hours);
    let mut names = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(&config.dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !safe_file_name(&name) {
            continue;
        }
        if let Some(now) = now {
            if retention.is_expired(&name, now) {
                continue;
            }
        }
        let Some(path) = resolve(config, &name) else {
            continue;
        };
        if !has_terminator(&path) {
            continue;
        }
        if config
            .open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&name)
        {
            continue;
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        names.insert((name, size));
    }
    names.into_iter().take(MAX_LISTED_FILES).collect()
}

fn listing_body(config: &Config) -> Vec<u8> {
    let entries = servable_names(config);
    json_listing(&entries, &config.base_url).into_bytes()
}

/// `GET /demos` payload: a JSON array of `{name, url, size}`.
fn json_listing(entries: &[(String, u64)], base: &str) -> String {
    let mut out = String::from("[");
    for (index, (name, size)) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let url = if base.is_empty() {
            format!("/demos/{name}")
        } else {
            format!("{base}/demos/{name}")
        };
        out.push_str(&format!(
            "{{\"name\":\"{}\",\"url\":\"{}\",\"size\":{}}}",
            json_escape(name),
            json_escape(&url),
            size
        ));
    }
    out.push_str("]\n");
    out
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn serve_file(stream: &mut TcpStream, config: &Config, name: &str, head: bool) {
    // A segment still being written is never served, whatever its bytes say.
    if config
        .open
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(name)
    {
        let _ = write_error(stream, 404, "Not Found", head);
        return;
    }
    if let Ok(now) = Timestamp::now() {
        if Retention::new(config.retention_hours).is_expired(name, now) {
            let _ = write_error(stream, 404, "Not Found", head);
            return;
        }
    }
    let Some(path) = resolve(config, name) else {
        let _ = write_error(stream, 404, "Not Found", head);
        return;
    };
    let Ok(mut file) = File::open(&path) else {
        let _ = write_error(stream, 404, "Not Found", head);
        return;
    };
    let Ok(metadata) = file.metadata() else {
        let _ = write_error(stream, 404, "Not Found", head);
        return;
    };
    if !metadata.is_file() || !has_terminator_handle(&mut file, metadata.len()) {
        let _ = write_error(stream, 404, "Not Found", head);
        return;
    }
    let length = metadata.len();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(header.as_bytes()).is_err() {
        return;
    }
    if head {
        return;
    }
    let _ = file.seek(SeekFrom::Start(0));
    let mut chunk = vec![0u8; SEND_CHUNK];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                if stream.write_all(&chunk[..read]).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

/// Resolve a validated name inside the demo directory, rejecting symlink
/// escapes. The TOCTOU window between canonicalisation and `File::open` is
/// accepted (an attacker who can already write the demo directory can also
/// replace a file there).
fn resolve(config: &Config, name: &str) -> Option<PathBuf> {
    if !safe_file_name(name) {
        return None;
    }
    let directory = std::fs::canonicalize(&config.dir).ok()?;
    let candidate = directory.join(name);
    let resolved = std::fs::canonicalize(&candidate).ok()?;
    if !resolved.starts_with(&directory) {
        return None;
    }
    Some(resolved)
}

fn has_terminator(path: &std::path::Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    has_terminator_handle(&mut file, metadata.len())
}

fn has_terminator_handle(file: &mut File, length: u64) -> bool {
    if length < TERMINATOR.len() as u64 {
        return false;
    }
    if file
        .seek(SeekFrom::End(-(TERMINATOR.len() as i64)))
        .is_err()
    {
        return false;
    }
    let mut tail = [0u8; 8];
    file.read_exact(&mut tail).is_ok() && tail == TERMINATOR
}

fn write_small(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    head: bool,
    allow: bool,
) -> io::Result<()> {
    let allow_header = if allow { "Allow: GET, HEAD\r\n" } else { "" };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{allow_header}\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    if !head {
        stream.write_all(body)?;
    }
    Ok(())
}

fn write_error(stream: &mut TcpStream, status: u16, reason: &str, head: bool) -> io::Result<()> {
    write_small(
        stream,
        status,
        reason,
        "text/plain; charset=utf-8",
        reason.as_bytes(),
        head,
        status == 405,
    )
}

fn reason(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpStream as Client;
    use std::path::Path;

    /// A name whose embedded timestamp is now, so retention does not hide it.
    fn fresh_name(segment: u32) -> String {
        let timestamp = Timestamp::now().unwrap();
        DemoName::new(
            timestamp,
            "mp/ffa3",
            crate::demo::file::InstanceLabel::Main,
            segment,
        )
        .unwrap()
        .file_name()
    }

    fn request(text: &str) -> Vec<u8> {
        text.as_bytes().to_vec()
    }

    fn parse(text: &str) -> Result<Request, u16> {
        parse_request(&request(text))
    }

    #[test]
    fn routes_are_the_explicit_ones() {
        assert_eq!(
            parse("GET /demos HTTP/1.1\r\nHost: x"),
            Ok(Request {
                head: false,
                route: Route::Listing
            })
        );
        assert_eq!(
            parse("HEAD /demos/20260101-000000_mp_ffa3_main_s1.dm_26 HTTP/1.1"),
            Ok(Request {
                head: true,
                route: Route::File("20260101-000000_mp_ffa3_main_s1.dm_26".to_string())
            })
        );
        assert_eq!(
            parse("GET /api/anything HTTP/1.1"),
            Ok(Request {
                head: false,
                route: Route::Api
            })
        );
        assert_eq!(
            parse("GET / HTTP/1.1"),
            Ok(Request {
                head: false,
                route: Route::NotFound
            })
        );
        assert_eq!(parse("POST /demos HTTP/1.1"), Err(405));
        assert_eq!(parse("GET /demos HTTP/2.0"), Err(400));
        assert_eq!(parse("GET demos HTTP/1.1"), Err(404));
    }

    #[test]
    fn traversal_and_encoded_traversal_never_reach_a_file_route() {
        for target in [
            "/demos/../x",
            "/demos/..%2fx",
            "/demos/%2e%2e/x",
            "/demos/%2e%2e%2fx",
            "/demos/%252e%252e%252fx",
            "/demos/..",
            "/demos/.",
            "/demos/%00x",
            "/demos/sub/dir.dm_26",
            "/demos/absolute.dm_26",
            "/demos/20260101-000000_mp_ffa3_main_s1.dm_26/../x",
        ] {
            let parsed = parse(&format!("GET {target} HTTP/1.1"));
            assert!(
                matches!(
                    parsed,
                    Ok(Request {
                        route: Route::NotFound,
                        ..
                    })
                ),
                "{target} parsed as {parsed:?}"
            );
        }
    }

    #[test]
    fn foreign_names_are_not_servable() {
        for name in [
            "notes.txt",
            "20260101-000000_mp_ffa3_main_s1.dm_25",
            "20260101-000000_mp_ffa3_main_s0.dm_26",
            "DEMO.dm_26",
            "20260101-000000_mp_ffa3_main_s1.dm_26.bak",
        ] {
            assert!(!safe_file_name(name), "{name} passed the pattern check");
        }
        assert!(safe_file_name("20260101-000000_mp_ffa3_main_s1.dm_26"));
        assert!(safe_file_name("20260101-000000_mp_ffa3_tffa2_s12.dm_26"));
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "proxy-http-test-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_demo(dir: &Path, name: &str, body: &[u8], terminated: bool) {
        let mut bytes = body.to_vec();
        if terminated {
            bytes.extend_from_slice(&TERMINATOR);
        }
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    fn config(dir: &Path) -> Config {
        Config {
            dir: dir.to_path_buf(),
            base_url: "https://example.com".to_string(),
            retention_hours: 24,
            open: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    fn http_get(addr: SocketAddr, request: &str) -> String {
        let mut stream = Client::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        String::from_utf8_lossy(&response).into_owned()
    }

    #[test]
    fn json_listing_is_an_array_of_name_url_size() {
        let entries = vec![
            ("20260101-000000_mp_ffa3_main_s1.dm_26".to_string(), 1234u64),
            ("20260101-000001_mp_ffa3_tffa2_s1.dm_26".to_string(), 42u64),
        ];
        let body = json_listing(&entries, "https://example.com");
        assert_eq!(
            body,
            "[{\"name\":\"20260101-000000_mp_ffa3_main_s1.dm_26\",\"url\":\"https://example.com/demos/20260101-000000_mp_ffa3_main_s1.dm_26\",\"size\":1234},{\"name\":\"20260101-000001_mp_ffa3_tffa2_s1.dm_26\",\"url\":\"https://example.com/demos/20260101-000001_mp_ffa3_tffa2_s1.dm_26\",\"size\":42}]\n"
        );
        assert_eq!(json_listing(&[], "https://example.com"), "[]\n");
        assert_eq!(json_escape("a\"b\\c\td"), "a\\\"b\\\\c\\td");
    }

    #[test]
    fn end_to_end_listing_head_and_file_body() {
        let dir = temp_dir("e2e");
        let name = fresh_name(1);
        let body: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
        write_demo(&dir, &name, &body, true);
        write_demo(&dir, "foreign.txt", b"not a demo", false);
        let open_name = fresh_name(2);
        write_demo(&dir, &open_name, b"still writing", true);
        let server_config = config(&dir);
        server_config
            .open
            .lock()
            .unwrap()
            .insert(open_name.to_string());

        let mut server = Server::start("127.0.0.1", 0, server_config).unwrap();
        let addr = server.local_addr();

        let listing = http_get(addr, "GET /demos HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(listing.starts_with("HTTP/1.1 200 OK"));
        assert!(listing.contains("Content-Length: "));
        assert!(
            listing.contains("Content-Type: application/json"),
            "{listing}"
        );
        let json = listing.split("\r\n\r\n").nth(1).unwrap();
        assert!(json.starts_with('['), "{json}");
        assert!(json.contains(&format!("\"name\":\"{name}\"")));
        assert!(json.contains(&format!("\"url\":\"https://example.com/demos/{name}\"")));
        assert!(!json.contains(&open_name), "open segment listed");
        assert!(!json.contains("foreign.txt"));

        let head = http_get(
            addr,
            &format!("HEAD /demos/{name} HTTP/1.1\r\nHost: x\r\n\r\n"),
        );
        assert!(head.starts_with("HTTP/1.1 200 OK"));
        assert!(head.contains("Content-Type: application/octet-stream"));
        assert!(head.contains(&format!("Content-Length: {}", body.len() + 8)));

        let mut client = Client::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
            .write_all(format!("GET /demos/{name} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .unwrap();
        let mut raw = Vec::new();
        client.read_to_end(&mut raw).unwrap();
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert_eq!(raw[split..].len(), body.len() + 8);
        assert_eq!(&raw[split..split + body.len()], &body[..]);

        let open = http_get(
            addr,
            &format!("GET /demos/{open_name} HTTP/1.1\r\nHost: x\r\n\r\n"),
        );
        assert!(open.starts_with("HTTP/1.1 404"), "{open}");

        let traversal = http_get(addr, "GET /demos/../x HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(traversal.starts_with("HTTP/1.1 404"));

        let api = http_get(addr, "GET /api/x HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(api.starts_with("HTTP/1.1 501"));

        server.stop();
        let refused = Client::connect(addr);
        assert!(refused.is_err(), "listener still bound after stop");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversized_request_line_is_refused() {
        let dir = temp_dir("oversize");
        let mut server = Server::start("127.0.0.1", 0, config(&dir)).unwrap();
        let addr = server.local_addr();
        let mut client = Client::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let request = format!(
            "GET /{} HTTP/1.0\r\n\r\n",
            "a".repeat(MAX_HEADER_BYTES + 64)
        );
        // The server answers 431 and closes; the write may race with the close.
        let _ = client.write_all(request.as_bytes());
        let mut response = String::new();
        let _ = BufReader::new(client).read_line(&mut response);
        assert!(response.starts_with("HTTP/1.1 431"), "{response}");
        server.stop();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn symlink_escape_is_not_served() {
        let dir = temp_dir("symlink");
        let outside = temp_dir("symlink-outside");
        let name = fresh_name(1);
        write_demo(&outside, &name, b"secret", true);
        let link = dir.join(&name);
        std::os::unix::fs::symlink(outside.join(&name), &link).unwrap();
        let mut server = Server::start("127.0.0.1", 0, config(&dir)).unwrap();
        let addr = server.local_addr();
        let response = http_get(
            addr,
            &format!("GET /demos/{name} HTTP/1.1\r\nHost: x\r\n\r\n"),
        );
        assert!(response.starts_with("HTTP/1.1 404"), "{response}");
        server.stop();
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}
