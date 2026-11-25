//! Metadata extracted from a closed server demo.
//!
//! The `/demos` listing uses this module to describe a file without replaying
//! it: map and gametype come from the recorded `CS_SERVERINFO`, players from
//! the final `scores` command and the `CS_PLAYERS` configstrings, team scores
//! from the `scores` header, and the duration from the first and last snapshot
//! `serverTime`.
//!
//! The scan never needs the entity or playerstate field tables. Server
//! commands are written before the snapshot in every message, so the reader
//! walks the commands and stops at `svc_snapshot` after its `serverTime`. The
//! game state frame is the same: configstrings precede the baselines, so the
//! scan stops at the first `svc_baseline`.
//!
//! Parsing is pure and independent of the engine, like the Phase 1 writer. It
//! also accepts files from older runs: the metadata is read from the bytes on
//! disk, not captured at recording time.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use super::gamestate::MAX_CONFIGSTRINGS;
use super::msg::MsgReader;
use crate::instances::info_value;
use crate::sdk::{CS_PLAYERS, CS_SCORES1, CS_SCORES2, CS_SERVERINFO, MAX_CLIENTS, TEAM_SPECTATOR};

// svc_ops_e (`qcommon.h`).
const SVC_GAMESTATE: u8 = 2;
const SVC_CONFIGSTRING: u8 = 3;
const SVC_BASELINE: u8 = 4;
const SVC_SERVERCOMMAND: u8 = 5;
const SVC_DOWNLOAD: u8 = 6;
const SVC_SNAPSHOT: u8 = 7;
const SVC_SETGAME: u8 = 8;
const SVC_MAPCHANGE: u8 = 9;
const SVC_EOF: u8 = 10;

/// Per-entry ints of a pristine `scores` command (D-009).
const SCORE_ENTRY_INTS: usize = 14;
/// TaystJK's 15-int form (deaths appended). Recorded demos strip the key, but
/// the reader accepts both so a client-recorded file still parses.
const SCORE_ENTRY_INTS_DEATHS: usize = 15;

/// `MSG_ReadString` cap for a server command; the engine's reliable command
/// buffer is 1024 bytes and a `bcs` chunk is below that.
const COMMAND_STRING_LIMIT: usize = 4096;
/// `BIG_INFO_STRING` for a game-state configstring.
const CONFIGSTRING_LIMIT: usize = 8192;
/// `MAX_MSGLEN` is 16 KiB; leave room for a protocol-extension frame while
/// refusing a corrupt length field.
const MAX_FRAME_BYTES: usize = 64 * 1024;
/// Parsed metadata kept in memory between listings.
const MAX_CACHE_ENTRIES: usize = 1024;

/// One player on the final scoreboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoPlayer {
    /// Client slot (`scores` entry int 0).
    pub num: i32,
    /// `CS_PLAYERS` `n` key (colour codes kept as recorded).
    pub name: String,
    /// `CS_PLAYERS` `t` key; `0` when the key is absent.
    pub team: i32,
    /// `scores` entry `PERS_SCORE` (int 1).
    pub score: i32,
}

/// Team totals from the `scores` header (team gametypes only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamScores {
    pub red: i32,
    pub blue: i32,
}

/// What `/demos` reports for one closed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoMetadata {
    pub map: Option<String>,
    pub gametype: Option<i32>,
    pub players: Vec<DemoPlayer>,
    pub team_scores: Option<TeamScores>,
    /// First-to-last snapshot `serverTime` in milliseconds.
    pub duration_ms: i64,
    pub snapshots: u32,
}

impl DemoMetadata {
    /// Whether this was not produced by a real demo stream (used by the
    /// listing to emit `metadata: null`). A valid file always has a map or at
    /// least a snapshot, so an all-empty result is reported as unknown.
    pub fn is_empty(&self) -> bool {
        self.map.is_none()
            && self.gametype.is_none()
            && self.players.is_empty()
            && self.team_scores.is_none()
            && self.snapshots == 0
    }
}

/// `GT_TEAM`, `GT_SIEGE`, `GT_CTF`, `GT_CTY`: gametypes whose `scores` header
/// carries RED/BLUE totals (`bg_public.h:183-198`).
fn team_gametype(gametype: i32) -> bool {
    matches!(gametype, 6..=9)
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

struct Scan {
    configstrings: Vec<Option<Vec<u8>>>,
    /// An unfinished `bcs0`/`bcs1`/`bcs2` long-configstring reassembly.
    bcs: Option<(u16, Vec<u8>)>,
    /// Last parsed `scores` command, header included.
    scores: Option<Vec<i32>>,
    first_time: Option<i32>,
    last_time: Option<i32>,
    snapshots: u32,
    frames: u32,
}

impl Scan {
    fn new() -> Self {
        Self {
            configstrings: vec![None; usize::from(MAX_CONFIGSTRINGS)],
            bcs: None,
            scores: None,
            first_time: None,
            last_time: None,
            snapshots: 0,
            frames: 0,
        }
    }

    fn set_configstring(&mut self, index: u16, value: Vec<u8>) {
        if let Some(slot) = self.configstrings.get_mut(usize::from(index)) {
            *slot = (!value.is_empty()).then_some(value);
        }
    }

    fn configstring(&self, index: u16) -> Option<&[u8]> {
        self.configstrings
            .get(usize::from(index))
            .and_then(|value| value.as_deref())
    }

    fn configstring_string(&self, index: u16) -> Option<String> {
        self.configstring(index)
            .map(|value| String::from_utf8_lossy(value).into_owned())
    }

    fn apply_command(&mut self, text: &str) {
        if let Some(rest) = text.strip_prefix("cs ") {
            if let Some((index, value)) = configstring_args(rest) {
                self.set_configstring(index, value.into_bytes());
            }
            return;
        }
        for (keyword, final_chunk) in [("bcs0 ", false), ("bcs1 ", false), ("bcs2 ", true)] {
            if let Some(rest) = text.strip_prefix(keyword) {
                if let Some((index, value)) = configstring_args(rest) {
                    self.apply_bcs_chunk(index, &value, final_chunk);
                }
                return;
            }
        }
        if let Some(rest) = text.strip_prefix("scores ") {
            let mut numbers = Vec::new();
            for token in rest.split_whitespace() {
                match token.parse::<i32>() {
                    Ok(value) => numbers.push(value),
                    Err(_) => return,
                }
            }
            if numbers.len() >= 3 {
                self.scores = Some(numbers);
            }
        }
    }

    fn apply_bcs_chunk(&mut self, index: u16, chunk: &str, final_chunk: bool) {
        if final_chunk {
            let (pending_index, mut value) = match self.bcs.take() {
                Some(pending) if pending.0 == index => pending,
                Some(pending) => {
                    // A different configstring started: drop the stale one.
                    self.bcs = Some(pending);
                    return;
                }
                None => (index, Vec::new()),
            };
            value.extend_from_slice(chunk.as_bytes());
            self.set_configstring(pending_index, value);
            return;
        }
        match &mut self.bcs {
            Some((pending_index, value)) if *pending_index == index => {
                value.extend_from_slice(chunk.as_bytes());
            }
            _ => self.bcs = Some((index, chunk.as_bytes().to_vec())),
        }
    }

    fn finish(self) -> Option<DemoMetadata> {
        if self.frames == 0 {
            return None;
        }
        let serverinfo = self
            .configstring_string(CS_SERVERINFO as u16)
            .unwrap_or_default();
        let map = info_value(&serverinfo, "mapname").filter(|value| !value.is_empty());
        let gametype =
            info_value(&serverinfo, "g_gametype").and_then(|value| value.parse::<i32>().ok());
        let team_mode = gametype.is_some_and(team_gametype);

        let mut players = Vec::new();
        for entry in score_entries(self.scores.as_deref().unwrap_or(&[])) {
            let Some(slot) = usize::try_from(entry.client).ok() else {
                continue;
            };
            if slot >= MAX_CLIENTS {
                continue;
            }
            let info = self
                .configstring_string(CS_PLAYERS as u16 + slot as u16)
                .unwrap_or_default();
            let name = info_value(&info, "n").unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let team = info_value(&info, "t")
                .and_then(|value| value.parse::<i32>().ok())
                .unwrap_or(0);
            if team == TEAM_SPECTATOR || (team_mode && !matches!(team, 1 | 2)) {
                continue;
            }
            players.push(DemoPlayer {
                num: entry.client,
                name,
                team,
                score: entry.score,
            });
        }

        let team_scores = team_mode.then(|| {
            self.scores
                .as_ref()
                .filter(|numbers| numbers.len() >= 3)
                .map(|numbers| TeamScores {
                    red: numbers[1],
                    blue: numbers[2],
                })
                .or_else(|| {
                    let red = self
                        .configstring_string(CS_SCORES1 as u16)
                        .and_then(|value| value.trim().parse::<i32>().ok());
                    let blue = self
                        .configstring_string(CS_SCORES2 as u16)
                        .and_then(|value| value.trim().parse::<i32>().ok());
                    red.zip(blue).map(|(red, blue)| TeamScores { red, blue })
                })
        });
        let team_scores = team_scores.flatten();

        let duration_ms = match (self.first_time, self.last_time) {
            (Some(first), Some(last)) => (i64::from(last) - i64::from(first)).max(0),
            _ => 0,
        };

        Some(DemoMetadata {
            map,
            gametype,
            players,
            team_scores,
            duration_ms,
            snapshots: self.snapshots,
        })
    }
}

struct ScoreEntry {
    client: i32,
    score: i32,
}

/// The per-client entries of a `scores` payload. `scores <count> <red> <blue>`
/// is followed by 14 ints per client in the stock layout (15 with TaystJK's
/// deaths field). A payload that is not a whole number of entries is ignored.
///
/// 210 ints can be read as 15×14 or 14×15, so the header's `count` decides
/// when it matches one of the two widths; otherwise the stock 14-int layout
/// wins (recorded demos strip the key that advertises 15 ints, D-009).
fn score_entries(numbers: &[i32]) -> Vec<ScoreEntry> {
    let Some(body) = numbers.get(3..) else {
        return Vec::new();
    };
    let header = usize::try_from(numbers[0]).ok();
    let width = if body.len() % SCORE_ENTRY_INTS != 0 {
        if body.len() % SCORE_ENTRY_INTS_DEATHS != 0 {
            return Vec::new();
        }
        SCORE_ENTRY_INTS_DEATHS
    } else if body.len() % SCORE_ENTRY_INTS_DEATHS != 0 {
        SCORE_ENTRY_INTS
    } else if header == Some(body.len() / SCORE_ENTRY_INTS_DEATHS) {
        SCORE_ENTRY_INTS_DEATHS
    } else {
        SCORE_ENTRY_INTS
    };
    body.chunks_exact(width)
        .map(|entry| ScoreEntry {
            client: entry[0],
            score: entry[1],
        })
        .collect()
}

/// `cs <index> "<value>"` / `bcsN <index> "<chunk>"` argument split. The value
/// is the quoted token with its surrounding quotes, or the raw remainder when
/// the engine wrote an unquoted one.
fn configstring_args(rest: &str) -> Option<(u16, String)> {
    let rest = rest.trim_start();
    let (index, rest) = rest.split_once(char::is_whitespace)?;
    let index = index.parse::<u16>().ok()?;
    Some((index, quoted_value(rest)))
}

fn quoted_value(text: &str) -> String {
    let text = text.trim();
    let Some(inner) = text.strip_prefix('"') else {
        return text.to_owned();
    };
    match inner.rfind('"') {
        Some(end) => inner[..end].to_owned(),
        None => inner.to_owned(),
    }
}

fn scan_payload(payload: &[u8], scan: &mut Scan) {
    let mut reader = MsgReader::new(payload);
    // `SV_SendClientSnapshot` starts every client message with the client's
    // last acknowledged command number.
    if reader.read_long().is_none() {
        return;
    }
    loop {
        let Some(command) = reader.read_byte() else {
            return;
        };
        match command {
            SVC_EOF => return,
            SVC_GAMESTATE => {
                // The gamestate's configstrings are complete before its
                // baselines; the baseline delta needs the entity field table,
                // which this scan deliberately does not load.
                if reader.read_long().is_none() {
                    return;
                }
                loop {
                    match reader.read_byte() {
                        Some(SVC_CONFIGSTRING) => {
                            let Some(index) = reader.read_short() else {
                                return;
                            };
                            let Some(value) = reader.read_string(CONFIGSTRING_LIMIT) else {
                                return;
                            };
                            scan.set_configstring(index as u16, value);
                        }
                        Some(SVC_BASELINE) | Some(SVC_EOF) | None => return,
                        _ => return,
                    }
                }
            }
            SVC_SERVERCOMMAND => {
                if reader.read_long().is_none() {
                    return;
                }
                let Some(text) = reader.read_string(COMMAND_STRING_LIMIT) else {
                    return;
                };
                scan.apply_command(&String::from_utf8_lossy(&text));
            }
            SVC_SNAPSHOT => {
                let Some(server_time) = reader.read_long() else {
                    return;
                };
                scan.snapshots = scan.snapshots.saturating_add(1);
                scan.first_time.get_or_insert(server_time);
                scan.last_time = Some(server_time);
                // The snapshot body needs the playerstate/entity tables and
                // holds nothing this scan reports.
                return;
            }
            SVC_SETGAME => {
                if reader.read_string(1024).is_none() {
                    return;
                }
            }
            SVC_DOWNLOAD => {
                let Some(block) = reader.read_short() else {
                    return;
                };
                if block < 0 {
                    return;
                }
                if block == 0 {
                    if reader.read_long().is_none() {
                        return;
                    }
                } else if reader.skip_bits(i32::from(block) * 8).is_none() {
                    return;
                }
            }
            SVC_MAPCHANGE => {}
            // XBOX-only svc_plyrPos0..9 carry a 24-bit position.
            11..=20 => {
                if reader.skip_bits(24).is_none() {
                    return;
                }
            }
            _ => return,
        }
    }
}

// ---------------------------------------------------------------------------
// File and cache
// ---------------------------------------------------------------------------

/// Read one demo file. Returns `None` when the file cannot be opened or holds
/// no frame at all; a truncated or partly foreign file still yields what the
/// readable prefix contained.
pub fn read_file(path: &Path) -> Option<DemoMetadata> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut scan = Scan::new();
    let mut payload = Vec::new();
    loop {
        let mut header = [0u8; 8];
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => break,
            Err(_) => break,
        }
        let sequence = i32::from_le_bytes(header[..4].try_into().expect("four bytes"));
        let length = i32::from_le_bytes(header[4..].try_into().expect("four bytes"));
        if sequence == -1 || length < 0 {
            break;
        }
        let length = length as usize;
        if length > MAX_FRAME_BYTES {
            break;
        }
        payload.clear();
        payload.resize(length, 0);
        if reader.read_exact(&mut payload).is_err() {
            break;
        }
        scan.frames = scan.frames.saturating_add(1);
        scan_payload(&payload, &mut scan);
    }
    scan.finish().filter(|metadata| !metadata.is_empty())
}

#[derive(Clone)]
struct CacheEntry {
    size: u64,
    modified: Option<SystemTime>,
    metadata: Option<DemoMetadata>,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// [`read_file`] with an in-memory cache keyed by path, size and modification
/// time. The HTTP listing calls this for every servable name; a closed demo is
/// parsed once and re-used until it changes on disk.
pub fn read_cached(path: &Path, size: u64) -> Option<DemoMetadata> {
    let modified = std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    {
        let cache = cache().lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(entry) = cache.get(path) {
            if entry.size == size && entry.modified == modified {
                return entry.metadata.clone();
            }
        }
    }
    let metadata = read_file(path);
    let mut cache = cache().lock().unwrap_or_else(|poison| poison.into_inner());
    if cache.len() >= MAX_CACHE_ENTRIES {
        cache.clear();
    }
    cache.insert(
        path.to_path_buf(),
        CacheEntry {
            size,
            modified,
            metadata: metadata.clone(),
        },
    );
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::file::DemoWriter;
    use crate::demo::gamestate::{ConfigString, GamestateHeader, RmgTail, encode_gamestate};
    use crate::demo::msg::MsgWriter;

    fn fixture_dir() -> PathBuf {
        let root = std::env::var_os("JAMP_REPO_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."));
        root.join("tests/fixtures/demo")
    }

    fn snapshot_frame(time: i32, commands: &[&[u8]]) -> Vec<u8> {
        let mut msg = MsgWriter::new();
        msg.write_long(0).unwrap();
        for command in commands {
            msg.write_byte(SVC_SERVERCOMMAND).unwrap();
            msg.write_long(1).unwrap();
            msg.write_data(command).unwrap();
            msg.write_byte(0).unwrap();
        }
        msg.write_byte(SVC_SNAPSHOT).unwrap();
        msg.write_long(time).unwrap();
        msg.into_payload()
    }

    fn score_entry(client: i32, score: i32, ping: i32) -> String {
        let mut values = vec![client, score, ping];
        values.resize(SCORE_ENTRY_INTS, 0);
        values
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn sample_demo() -> Vec<u8> {
        let configstrings = [
            ConfigString::new(
                CS_SERVERINFO as u16,
                b"\\mapname\\mp/duel1\\g_gametype\\6\\sv_hostname\\srv",
            ),
            ConfigString::new(CS_PLAYERS as u16, b"n\\Alice\\t\\1\\model\\kyle"),
            ConfigString::new(CS_PLAYERS as u16 + 1, b"n\\Bob\\t\\2\\model\\kyle"),
            ConfigString::new(CS_PLAYERS as u16 + 2, b"n\\Spec\\t\\3\\model\\kyle"),
        ];
        let gamestate = encode_gamestate(
            GamestateHeader {
                last_client_command: 0,
                reliable_sequence: 0,
                client_num: 31,
                checksum_feed: 1,
            },
            &configstrings,
            &[],
            RmgTail::NoManager,
        )
        .unwrap();

        let scores = format!(
            "scores 3 2 1 {} {} {}",
            score_entry(0, 5, 30),
            score_entry(1, 3, 25),
            score_entry(2, 0, 10)
        );
        let renamed = b"cs 1132 \"n\\Robert\\t\\2\\model\\kyle\"";
        let tail = snapshot_frame(40_000, &[renamed]);

        let mut writer = DemoWriter::new(Vec::new(), 64 * 1024);
        writer.write_frame(1, &gamestate).unwrap();
        writer
            .write_frame(2, &snapshot_frame(10_000, &[scores.as_bytes()]))
            .unwrap();
        writer.write_frame(3, &tail).unwrap();
        writer.finish().unwrap();
        writer.sink().clone()
    }

    #[test]
    fn fixture_gamestate_reports_map_and_gametype() {
        let metadata = read_file(&fixture_dir().join("server-ffa3-phase0.dm_26")).unwrap();
        assert_eq!(metadata.map.as_deref(), Some("mp/ffa3"));
        assert_eq!(metadata.gametype, Some(0));
        assert!(metadata.team_scores.is_none());
        assert!(metadata.snapshots > 0);
    }

    #[test]
    fn final_scores_and_latest_player_names_are_reported() {
        let bytes = sample_demo();
        let dir = std::env::temp_dir().join(format!(
            "proxy-demo-meta-{}-{:?}.dm_26",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&dir, &bytes).unwrap();
        let metadata = read_file(&dir).unwrap();
        std::fs::remove_file(&dir).ok();

        assert_eq!(metadata.map.as_deref(), Some("mp/duel1"));
        assert_eq!(metadata.gametype, Some(6));
        assert_eq!(metadata.team_scores, Some(TeamScores { red: 2, blue: 1 }));
        assert_eq!(metadata.duration_ms, 30_000);
        assert_eq!(metadata.snapshots, 2);
        assert_eq!(
            metadata.players,
            vec![
                DemoPlayer {
                    num: 0,
                    name: "Alice".to_string(),
                    team: 1,
                    score: 5,
                },
                DemoPlayer {
                    num: 1,
                    name: "Robert".to_string(),
                    team: 2,
                    score: 3,
                },
            ]
        );
    }

    #[test]
    fn non_team_gametypes_keep_free_and_duel_players() {
        let configstrings = [
            ConfigString::new(CS_SERVERINFO as u16, b"\\mapname\\mp/ffa3\\g_gametype\\0"),
            ConfigString::new(CS_PLAYERS as u16, b"n\\Free\\t\\0"),
            ConfigString::new(CS_PLAYERS as u16 + 1, b"n\\Spec\\t\\3"),
        ];
        let gamestate = encode_gamestate(
            GamestateHeader {
                last_client_command: 0,
                reliable_sequence: 0,
                client_num: 31,
                checksum_feed: 1,
            },
            &configstrings,
            &[],
            RmgTail::NoManager,
        )
        .unwrap();
        let scores = format!(
            "scores 2 0 0 {} {}",
            score_entry(0, 1, 9),
            score_entry(1, 0, 8)
        );
        let frame = snapshot_frame(5_000, &[scores.as_bytes()]);
        let mut writer = DemoWriter::new(Vec::new(), 64 * 1024);
        writer.write_frame(1, &gamestate).unwrap();
        writer.write_frame(2, &frame).unwrap();
        writer.finish().unwrap();

        let dir = std::env::temp_dir().join(format!(
            "proxy-demo-meta-ffa-{}-{:?}.dm_26",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&dir, writer.sink()).unwrap();
        let metadata = read_file(&dir).unwrap();
        std::fs::remove_file(&dir).ok();

        assert_eq!(metadata.map.as_deref(), Some("mp/ffa3"));
        assert_eq!(metadata.players.len(), 1);
        assert_eq!(metadata.players[0].name, "Free");
        assert_eq!(metadata.players[0].team, 0);
        assert!(metadata.team_scores.is_none());
    }

    #[test]
    fn chunked_configstrings_are_reassembled() {
        let mut scan = Scan::new();
        scan.apply_command("bcs0 1133 \"n\\Long");
        scan.apply_command("bcs1 1133 \"Name");
        scan.apply_command("bcs2 1133 \"\\t\\1\"");
        assert_eq!(
            scan.configstring_string(1133).as_deref(),
            Some("n\\LongName\\t\\1")
        );
    }

    #[test]
    fn malformed_input_is_not_a_panic() {
        assert!(read_file(Path::new("/nonexistent/proxy-demo.dm_26")).is_none());
        let mut scan = Scan::new();
        scan.frames = 1;
        scan_payload(&[0xff, 0xff, 0xff, 0xff], &mut scan);
        scan_payload(&[], &mut scan);
        assert!(scan.finish().is_some());
    }

    #[test]
    fn score_entry_width_uses_the_header_when_both_layouts_fit() {
        // 210 ints: 15 stock entries or 14 TaystJK entries.
        let mut stock = vec![15, 0, 0];
        for client in 0..15 {
            let mut entry = vec![client, 0, 0];
            entry.resize(SCORE_ENTRY_INTS, 0);
            stock.extend_from_slice(&entry);
        }
        assert_eq!(score_entries(&stock).len(), 15);

        let mut deaths = vec![14, 0, 0];
        for client in 0..14 {
            let mut entry = vec![client, 0, 0];
            entry.resize(SCORE_ENTRY_INTS_DEATHS, 0);
            deaths.extend_from_slice(&entry);
        }
        assert_eq!(score_entries(&deaths).len(), 14);
        assert_eq!(score_entries(&deaths)[13].client, 13);
    }

    #[test]
    fn quoted_values_keep_interior_quotes_and_raw_backslashes() {
        assert_eq!(quoted_value(" \"n\\viper\"\n"), "n\\viper");
        assert_eq!(quoted_value(" \"a\"b\"\n"), "a\"b");
        assert_eq!(quoted_value(" raw "), "raw");
        assert_eq!(
            configstring_args(" 1131 \"n\\viper\""),
            Some((1131, "n\\viper".to_string()))
        );
    }
}
