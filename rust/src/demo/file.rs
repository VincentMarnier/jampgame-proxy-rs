//! Demo file container, naming and retention.
//!
//! Container (verified against the reference client and all 49 shipped demos;
//! `docs/demo-format.md` §2):
//!
//! ```text
//! frame := int32 sequence, int32 length, byte[length] payload
//! file  := frame*, int32 -1, int32 -1
//! ```
//!
//! [`DemoWriter`] buffers frame bytes and only calls the [`DemoSink`] when a
//! buffer threshold is reached (or on [`DemoWriter::finish`]), so a recording
//! does not perform one filesystem write per frame.
//!
//! Names are `<YYYYmmdd-HHMMSS>_<map>_<instance>_s<segment>.dm_26`, using UTC
//! (the proxy names its own files; this avoids a dependency on the server's
//! local timezone). [`DemoName::parse`] accepts only that exact shape, so the
//! retention sweep can never delete a foreign file.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// The protocol-26 demo extension (without the leading dot).
pub const DEMO_EXTENSION: &str = "dm_26";
/// Default buffered output size: at 20 Hz this is roughly half a minute of
/// typical snapshots before the first write.
pub const DEFAULT_BUFFER_BYTES: usize = 64 * 1024;
/// Upper bound for the sanitised map component (filesystem safety).
pub const MAX_MAP_SANITIZED_LEN: usize = 64;
const TERMINATOR: i32 = -1;
const SECONDS_PER_HOUR: i64 = 3600;
const SECONDS_PER_DAY: i64 = 86_400;

// ---------------------------------------------------------------------------
// Sink abstraction
// ---------------------------------------------------------------------------

/// Where [`DemoWriter`] flushes its buffer. Tests use `Vec<u8>` directly.
pub trait DemoSink {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl DemoSink for Vec<u8> {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

/// A file opened for a new demo. `create_new` refuses to overwrite an
/// existing file; the caller chooses the name.
#[derive(Debug)]
pub struct FileSink {
    file: File,
}

impl FileSink {
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self { file })
    }
}

impl DemoSink for FileSink {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

// ---------------------------------------------------------------------------
// Buffered frame writer
// ---------------------------------------------------------------------------

/// Writes demo frames into a sink through an appendable buffer.
///
/// `finish` appends the `-1/-1` terminator and flushes. It is explicit because
/// `Drop` cannot report I/O errors; a caller that forgets it produces an
/// unterminated (truncated) file.
pub struct DemoWriter<S: DemoSink> {
    sink: S,
    buffer: Vec<u8>,
    flush_at: usize,
    finished: bool,
    bytes: u64,
    frames: u64,
}

impl<S: DemoSink> DemoWriter<S> {
    pub fn new(sink: S, buffer_bytes: usize) -> Self {
        let buffer_bytes = buffer_bytes.max(8);
        Self {
            sink,
            buffer: Vec::with_capacity(buffer_bytes),
            flush_at: buffer_bytes,
            finished: false,
            bytes: 0,
            frames: 0,
        }
    }

    /// Bytes appended so far (buffered + already flushed).
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    pub fn frames_written(&self) -> u64 {
        self.frames
    }

    pub fn sink(&self) -> &S {
        &self.sink
    }

    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.sink.write_all(&self.buffer)?;
        self.buffer.clear();
        self.sink.flush()
    }

    /// Append one frame. `sequence` is the server-message sequence recorded in
    /// the frame header; it may contain gaps (the reader does not validate
    /// continuity) but cannot be negative.
    pub fn write_frame(&mut self, sequence: i32, payload: &[u8]) -> io::Result<()> {
        if self.finished {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "demo writer already finished",
            ));
        }
        if sequence < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "demo frame sequence must be non-negative",
            ));
        }
        if payload.len() > i32::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "demo frame payload exceeds int32 length",
            ));
        }

        let length = payload.len() as i32;
        self.buffer.extend_from_slice(&sequence.to_le_bytes());
        self.buffer.extend_from_slice(&length.to_le_bytes());
        self.buffer.extend_from_slice(payload);
        self.bytes += 8 + payload.len() as u64;
        self.frames += 1;
        if self.buffer.len() >= self.flush_at {
            self.flush_buffer()?;
        }
        Ok(())
    }

    /// Append the `-1/-1` terminator and flush. Idempotent.
    pub fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.buffer.extend_from_slice(&TERMINATOR.to_le_bytes());
        self.buffer.extend_from_slice(&TERMINATOR.to_le_bytes());
        self.bytes += 8;
        self.flush_buffer()?;
        self.finished = true;
        Ok(())
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Consume the writer and return its sink. Call [`Self::finish`] first if
    /// the file must be terminated.
    pub fn into_sink(self) -> S {
        self.sink
    }
}

// ---------------------------------------------------------------------------
// Timestamps (UTC, seconds since the Unix epoch)
// ---------------------------------------------------------------------------

/// A UTC timestamp that round-trips through the `YYYYmmdd-HHMMSS` name field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// Build a timestamp, rejecting years outside `1970..=9999` (the name
    /// field is four digits). The retention sweep only ever sees such values.
    pub fn from_unix_seconds(seconds: i64) -> Option<Self> {
        let (year, _, _) = civil_from_days(seconds.div_euclid(SECONDS_PER_DAY));
        (1970..=9999).contains(&year).then_some(Self(seconds))
    }

    pub const fn unix_seconds(self) -> i64 {
        self.0
    }

    pub fn now() -> io::Result<Self> {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .as_secs();
        Self::from_unix_seconds(seconds as i64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "system clock out of range"))
    }

    /// `YYYYmmdd-HHMMSS` (always 15 ASCII characters).
    pub fn format(self) -> String {
        let days = self.0.div_euclid(SECONDS_PER_DAY);
        let seconds = self.0.rem_euclid(SECONDS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        let hour = seconds / 3600;
        let minute = (seconds % 3600) / 60;
        let second = seconds % 60;
        format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
    }

    /// Parse exactly `YYYYmmdd-HHMMSS`; validate leap years and ranges.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 15 || bytes[8] != b'-' {
            return None;
        }
        let digits = |range: std::ops::Range<usize>| -> Option<i64> {
            let mut value = 0i64;
            for &byte in &bytes[range] {
                if !byte.is_ascii_digit() {
                    return None;
                }
                value = value * 10 + i64::from(byte - b'0');
            }
            Some(value)
        };
        let year = digits(0..4)?;
        let month = digits(4..6)?;
        let day = digits(6..8)?;
        let hour = digits(9..11)?;
        let minute = digits(11..13)?;
        let second = digits(13..15)?;
        if !(1970..=9999).contains(&year)
            || !(1..=12).contains(&month)
            || day < 1
            || day > i64::from(days_in_month(year, month as u32))
            || hour >= 24
            || minute >= 60
            || second >= 60
        {
            return None;
        }
        let seconds = days_from_civil(year, month as u32, day as u32) * SECONDS_PER_DAY
            + hour * 3600
            + minute * 60
            + second;
        Self::from_unix_seconds(seconds)
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Howard Hinnant's `days_from_civil` (proleptic Gregorian, UTC).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

// ---------------------------------------------------------------------------
// Instance labels and demo names
// ---------------------------------------------------------------------------

/// The recorded instance component of the file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstanceLabel {
    /// Non-instanced gametypes.
    Main,
    /// Parallel TFFA match `n` (1-based).
    Tffa(u32),
    /// Parallel duel court `n` (1-based).
    Duel(u32),
}

impl InstanceLabel {
    pub fn tffa(index: u32) -> Option<Self> {
        (index > 0).then_some(Self::Tffa(index))
    }

    pub fn duel(index: u32) -> Option<Self> {
        (index > 0).then_some(Self::Duel(index))
    }

    pub fn as_label(self) -> String {
        match self {
            InstanceLabel::Main => "main".to_string(),
            InstanceLabel::Tffa(index) => format!("tffa{index}"),
            InstanceLabel::Duel(index) => format!("duel{index}"),
        }
    }

    fn parse(text: &str) -> Option<Self> {
        if text == "main" {
            return Some(Self::Main);
        }
        let (kind, digits) = if let Some(rest) = text.strip_prefix("tffa") {
            (Self::Tffa as fn(u32) -> Self, rest)
        } else if let Some(rest) = text.strip_prefix("duel") {
            (Self::Duel as fn(u32) -> Self, rest)
        } else {
            return None;
        };
        let index = parse_positive_u32(digits)?;
        Some(kind(index))
    }
}

/// Errors from constructing a [`DemoName`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    /// Segment numbers start at 1 and have no leading zero.
    InvalidSegment(u32),
    /// `tffa0` / `duel0` are not valid instance labels.
    InvalidInstance,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameError::InvalidSegment(segment) => {
                write!(f, "demo segment must be >= 1 (got {segment})")
            }
            NameError::InvalidInstance => write!(f, "demo instance must be >= 1"),
        }
    }
}

impl std::error::Error for NameError {}

/// A parsed/generated demo file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoName {
    pub timestamp: Timestamp,
    pub map: String,
    pub instance: InstanceLabel,
    pub segment: u32,
}

impl DemoName {
    pub fn new(
        timestamp: Timestamp,
        map: &str,
        instance: InstanceLabel,
        segment: u32,
    ) -> Result<Self, NameError> {
        if segment == 0 {
            return Err(NameError::InvalidSegment(segment));
        }
        if matches!(instance, InstanceLabel::Tffa(0) | InstanceLabel::Duel(0)) {
            return Err(NameError::InvalidInstance);
        }
        Ok(Self {
            timestamp,
            map: sanitize_map(map),
            instance,
            segment,
        })
    }

    /// `<YYYYmmdd-HHMMSS>_<map>_<instance>_s<segment>.dm_26`
    pub fn file_name(&self) -> String {
        format!(
            "{}_{}_{}_s{}.{}",
            self.timestamp.format(),
            self.map,
            self.instance.as_label(),
            self.segment,
            DEMO_EXTENSION
        )
    }

    /// Strict parser: exactly our pattern, including the sanitised map.
    pub fn parse(file_name: &str) -> Option<Self> {
        let stem = file_name.strip_suffix(DEMO_EXTENSION)?;
        let stem = stem.strip_suffix('.')?;
        if stem.len() < 15 + 1 + 1 + 2 + 1 + 1 + 1 {
            return None;
        }
        let (timestamp_text, rest) = stem.split_at(15);
        let timestamp = Timestamp::parse(timestamp_text)?;
        let rest = rest.strip_prefix('_')?;

        // `rest` is `<map>_<instance>_s<segment>`; split from the right so a
        // map containing underscores stays intact.
        let (rest, segment_text) = rest.rsplit_once("_s")?;
        let segment = parse_positive_u32(segment_text)?;
        let (map, instance_text) = rest.rsplit_once('_')?;
        let instance = InstanceLabel::parse(instance_text)?;
        if map.is_empty() || map.len() > MAX_MAP_SANITIZED_LEN || sanitize_map(map) != map {
            return None;
        }
        Some(Self {
            timestamp,
            map: map.to_string(),
            instance,
            segment,
        })
    }
}

fn parse_positive_u32(text: &str) -> Option<u32> {
    if text.is_empty() || text.starts_with('0') || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u32>().ok().filter(|value| *value > 0)
}

/// Replace everything outside `[A-Za-z0-9_-]` with `_`, collapse runs of `_`,
/// trim them, and cap the result at [`MAX_MAP_SANITIZED_LEN`]. `mp/ffa3`
/// becomes `mp_ffa3`; an empty result becomes `map`.
pub fn sanitize_map(map: &str) -> String {
    let mut out = String::new();
    let mut last_underscore = false;
    for ch in map.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            ch
        } else {
            '_'
        };
        if mapped == '_' {
            if out.is_empty() || last_underscore {
                continue;
            }
            last_underscore = true;
        } else {
            last_underscore = false;
        }
        if out.len() >= MAX_MAP_SANITIZED_LEN {
            break;
        }
        out.push(mapped);
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "map".to_string()
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

/// `proxy_sv_demoRetentionHours` semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub hours: u32,
}

impl Retention {
    /// `0` = keep forever.
    pub const KEEP_FOREVER: Self = Self { hours: 0 };

    pub const fn new(hours: u32) -> Self {
        Self { hours }
    }

    /// True only for a file that matches [`DemoName::parse`]. Foreign files
    /// never match, so the sweep cannot delete them.
    pub fn matches(&self, file_name: &str) -> bool {
        DemoName::parse(file_name).is_some()
    }

    /// Whether a *matching* file is old enough to delete. A file whose
    /// timestamp is in the future (clock adjustment) is never deleted. Hours
    /// `0` means keep forever.
    pub fn is_expired(&self, file_name: &str, now: Timestamp) -> bool {
        if self.hours == 0 {
            return false;
        }
        let Some(name) = DemoName::parse(file_name) else {
            return false;
        };
        if now.unix_seconds() < name.timestamp.unix_seconds() {
            return false;
        }
        now.unix_seconds() - name.timestamp.unix_seconds()
            >= i64::from(self.hours) * SECONDS_PER_HOUR
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Default)]
    struct CountingSink {
        bytes: Vec<u8>,
        writes: usize,
    }

    impl DemoSink for CountingSink {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.bytes.extend_from_slice(bytes);
            self.writes += 1;
            Ok(())
        }
    }

    fn ts(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    #[test]
    fn timestamp_round_trips_and_matches_known_epochs() {
        let epoch = Timestamp::from_unix_seconds(0).unwrap();
        assert_eq!(epoch.format(), "19700101-000000");
        assert_eq!(Timestamp::parse("19700101-000000"), Some(epoch));

        // 2025-02-28 23:59:59 UTC
        let sample = ts("20250228-235959");
        assert_eq!(sample.format(), "20250228-235959");
        assert_eq!(Timestamp::parse(&sample.format()), Some(sample));

        // Leap day: 2024 is a leap year, 2025 is not.
        assert!(Timestamp::parse("20240229-000000").is_some());
        assert!(Timestamp::parse("20250229-000000").is_none());
        assert!(Timestamp::parse("20000229-000000").is_some());
        assert!(Timestamp::parse("21000229-000000").is_none());
        assert_eq!(
            Timestamp::parse("99991231-235959").unwrap().format(),
            "99991231-235959"
        );
        assert!(Timestamp::parse("99991232-000000").is_none());
        assert!(Timestamp::parse("20241301-000000").is_none());
        assert!(Timestamp::parse("20250101-240000").is_none());
        assert!(Timestamp::parse("20250101-000060").is_none());
        assert!(Timestamp::parse("2025011-000000").is_none());
        assert!(Timestamp::parse("20250101_000000").is_none());
    }

    #[test]
    fn known_epoch_seconds_are_stable() {
        // Python: datetime(2025, 1, 2, 3, 4, 5, tzinfo=UTC).timestamp()
        let value = Timestamp::from_unix_seconds(1_735_787_045).unwrap();
        assert_eq!(value.format(), "20250102-030405");
    }

    #[test]
    fn map_sanitisation() {
        assert_eq!(sanitize_map("mp/ffa3"), "mp_ffa3");
        assert_eq!(sanitize_map("mp\\duel 1"), "mp_duel_1");
        assert_eq!(sanitize_map("  ..  "), "map");
        assert_eq!(sanitize_map("ffa__yavin"), "ffa_yavin");
        assert_eq!(sanitize_map("ffa-3"), "ffa-3");
        let long = "a".repeat(MAX_MAP_SANITIZED_LEN + 10);
        assert!(sanitize_map(&long).len() <= MAX_MAP_SANITIZED_LEN);
    }

    #[test]
    fn instance_labels() {
        assert_eq!(InstanceLabel::Main.as_label(), "main");
        assert_eq!(InstanceLabel::tffa(2).unwrap().as_label(), "tffa2");
        assert_eq!(InstanceLabel::duel(3).unwrap().as_label(), "duel3");
        assert_eq!(InstanceLabel::tffa(0), None);
        assert_eq!(InstanceLabel::duel(0), None);
        assert_eq!(InstanceLabel::parse("tffa2"), Some(InstanceLabel::Tffa(2)));
        assert_eq!(InstanceLabel::parse("main"), Some(InstanceLabel::Main));
        assert_eq!(InstanceLabel::parse("tffa0"), None);
        assert_eq!(InstanceLabel::parse("tffa02"), None);
    }

    #[test]
    fn name_round_trips() {
        let name =
            DemoName::new(ts("20251112-134455"), "mp/ffa3", InstanceLabel::Tffa(2), 3).unwrap();
        assert_eq!(name.file_name(), "20251112-134455_mp_ffa3_tffa2_s3.dm_26");
        assert_eq!(DemoName::parse(&name.file_name()), Some(name));

        let main =
            DemoName::new(ts("20251112-134455"), "mp_duel1", InstanceLabel::Main, 1).unwrap();
        assert_eq!(DemoName::parse(&main.file_name()), Some(main));
        assert_eq!(
            DemoName::new(ts("20251112-134455"), "map", InstanceLabel::Main, 0),
            Err(NameError::InvalidSegment(0))
        );
        assert_eq!(
            DemoName::new(ts("20251112-134455"), "map", InstanceLabel::Tffa(0), 1),
            Err(NameError::InvalidInstance)
        );
    }

    #[test]
    fn name_parser_rejects_foreign_files() {
        let valid = "20251112-134455_mp_ffa3_main_s1.dm_26";
        assert!(DemoName::parse(valid).is_some());
        for foreign in [
            "demo2025-09-12_13-17-36.dm_26",
            "20251112-134455_mp_ffa3_main_s1.dm_16",
            "20251112-134455_mp_ffa3_main_s1.dm_26.bak",
            "20251112-134455_mp_ffa3_main_s1.dm_26x",
            "20251112-134455_mp_ffa3_main_s01.dm_26",
            "20251112-134455_mp_ffa3_main_s0.dm_26",
            "20251112-134455_mp_ffa3_tffa0_s1.dm_26",
            "20251112-134455_mp_ffa3_tffa02_s1.dm_26",
            "20251112-134455_mp/ffa3_main_s1.dm_26",
            "20251112-134455__main_s1.dm_26",
            "20251112-134455_mp_ffa3_s1.dm_26",
            "20251312-134455_mp_ffa3_main_s1.dm_26",
            "20251112-134455_mp_ffa3_main_s1.dm_26.part",
            "20251112-134455_mp_ffa3_main_s1",
            "",
        ] {
            assert_eq!(DemoName::parse(foreign), None, "{foreign}");
        }
    }

    #[test]
    fn retention_only_matches_our_names_and_honours_zero() {
        let name = DemoName::new(ts("20251112-134455"), "mp_ffa3", InstanceLabel::Main, 1).unwrap();
        let file = name.file_name();
        let now = ts("20251112-144455"); // +1 h

        assert!(!Retention::KEEP_FOREVER.is_expired(&file, now));
        assert!(Retention::new(1).is_expired(&file, now));
        assert!(!Retention::new(2).is_expired(&file, now));
        assert!(Retention::new(1).matches(&file));
        assert!(!Retention::new(1).matches("demo2025-09-12_13-17-36.dm_26"));
        assert!(!Retention::new(1).is_expired("demo2025-09-12_13-17-36.dm_26", now));

        // A clock moved before the file timestamp never deletes it.
        let earlier = ts("20251112-000000");
        assert!(!Retention::new(1).is_expired(&file, earlier));
        // Exactly one hour is expired; one second less is not.
        assert!(Retention::new(1).is_expired(&file, ts("20251112-144455")));
        assert!(!Retention::new(1).is_expired(&file, ts("20251112-144454")));
    }

    #[test]
    fn frames_are_framed_and_terminated() {
        let mut writer = DemoWriter::new(Vec::new(), 64);
        writer.write_frame(7, b"abc").unwrap();
        writer.write_frame(9, b"de").unwrap();
        writer.finish().unwrap();
        let bytes = writer.into_sink();

        let mut expected = Vec::new();
        expected.extend_from_slice(&7i32.to_le_bytes());
        expected.extend_from_slice(&3i32.to_le_bytes());
        expected.extend_from_slice(b"abc");
        expected.extend_from_slice(&9i32.to_le_bytes());
        expected.extend_from_slice(&2i32.to_le_bytes());
        expected.extend_from_slice(b"de");
        expected.extend_from_slice(&(-1i32).to_le_bytes());
        expected.extend_from_slice(&(-1i32).to_le_bytes());
        assert_eq!(bytes, expected);
    }

    #[test]
    fn buffering_avoids_one_sink_call_per_frame() {
        let mut writer = DemoWriter::new(CountingSink::default(), 1024);
        for i in 0..100 {
            writer.write_frame(i, &[0u8; 100]).unwrap();
        }
        writer.finish().unwrap();
        let sink = writer.into_sink();
        // 100 * (8 + 100) bytes = 10800; at 1024-byte flushes this is ~11
        // writes, not 100.
        assert!(sink.writes <= 16, "writes = {}", sink.writes);
        assert_eq!(sink.bytes.len(), 100 * 108 + 8);
    }

    #[test]
    fn finish_is_idempotent_and_write_after_finish_fails() {
        let mut writer = DemoWriter::new(Vec::new(), 16);
        writer.write_frame(1, b"x").unwrap();
        writer.finish().unwrap();
        writer.finish().unwrap();
        let err = writer.write_frame(2, b"y").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn invalid_frame_arguments_are_rejected() {
        let mut writer = DemoWriter::new(Vec::new(), 16);
        assert_eq!(
            writer.write_frame(-1, b"x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(writer.frames_written(), 0);
    }

    #[test]
    fn file_sink_writes_and_refuses_overwrite() {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "jampgame-proxy-demo-test-{}-{}.dm_26",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        {
            let sink = FileSink::create(&path).unwrap();
            let mut writer = DemoWriter::new(sink, 32);
            writer.write_frame(1, b"abc").unwrap();
            writer.finish().unwrap();
        }
        let data = std::fs::read(&path).unwrap();
        assert_eq!(data.len(), 8 + 3 + 8);
        assert_eq!(
            FileSink::create(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        std::fs::remove_file(&path).unwrap();
    }
}
