//! Game-state message encoder for the first demo frame.
//!
//! Wire layout (verified against the shipped server, the reference client and
//! the 49 recorded demos; see `docs/demo-format.md` §3):
//!
//! ```text
//! int32  lastClientCommand
//! byte   svc_gamestate (2)
//! int32  reliableSequence
//! { byte svc_configstring (3), int16 index, bytes, NUL }*   // non-empty only
//! { byte svc_baseline (4), MSG_WriteDeltaEntity(null, baseline, 1) }*
//! byte   svc_EOF (10)
//! int32  clientNum
//! int32  checksumFeed
//! int16  rmg            // 0: no random mission (RMG manager null)
//! byte   svc_EOF (10)
//! ```
//!
//! The encoder is pure Rust: it does not call the engine, so `cargo test`
//! exercises it on the host. The entity field table is the shipped binary's,
//! not the stale source-tree table (see `fields.rs`).
//!
//! Limitation: if the server's `TheRandomMissionManager` is non-null, the
//! shipped `SV_SendClientGameState` writes a variable heightmap/seed/automap
//! block instead of `int16 0`. This encoder refuses that case with
//! [`GamestateError::RmgUnsupported`]; the caller must skip the segment.

use std::fmt;

use super::fields::ENTITY_FIELDS;
use super::msg::{MsgError, MsgWriter};

/// `offsetof(entityState_t, ...)` table size anchor: `sizeof(entityState_t)`
/// in the shipped PC/MP build (the field table's last offset is 528).
pub const ENTITY_STATE_SIZE: usize = 532;

/// `MAX_GENTITIES` (`q_shared.h`): entity-number range.
pub const MAX_GENTITIES: i32 = 1024;

/// `MAX_CONFIGSTRINGS` (`q_shared.h:2037`).
pub const MAX_CONFIGSTRINGS: u16 = 1700;

const SVC_GAMESTATE: u8 = 2;
const SVC_CONFIGSTRING: u8 = 3;
const SVC_BASELINE: u8 = 4;
const SVC_EOF: u8 = 10;
const GENTITYNUM_BITS: i32 = 10;
const FLOAT_INT_BITS: i32 = 13;
const FLOAT_INT_BIAS: i32 = 1 << (FLOAT_INT_BITS - 1);

/// One non-empty configstring (the caller filters empties and may pass any
/// order; the encoder sorts by index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigString<'a> {
    pub index: u16,
    pub value: &'a [u8],
}

impl<'a> ConfigString<'a> {
    pub const fn new(index: u16, value: &'a [u8]) -> Self {
        Self { index, value }
    }
}

/// A copy of one `entityState_t` baseline (the engine's
/// `server.svEntities[n].baseline`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    state: [u8; ENTITY_STATE_SIZE],
}

impl Baseline {
    pub const fn from_state(state: [u8; ENTITY_STATE_SIZE]) -> Self {
        Self { state }
    }

    pub fn state(&self) -> &[u8; ENTITY_STATE_SIZE] {
        &self.state
    }

    /// `entityState_t.number` (offset 0, LE i32).
    pub fn number(&self) -> i32 {
        read_i32(&self.state, 0)
    }
}

/// The non-gamestate header fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GamestateHeader {
    /// `client_t.lastClientCommand`; `0` for a server-side recorder.
    pub last_client_command: i32,
    /// `client_t.reliableSequence` (reliable server-command counter).
    pub reliable_sequence: i32,
    /// `clc.clientNum`: the recording client's own slot in a client demo.
    pub client_num: i32,
    /// `server.checksumFeed`.
    pub checksum_feed: i32,
}

/// Whether the engine has a live `TheRandomMissionManager`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RmgTail {
    /// `TheRandomMissionManager == NULL`: the tail is `int16 0`.
    NoManager,
    /// Non-null: the variable RMG block is not supported (refuse the segment).
    ManagerPresent,
}

/// Errors from [`encode_gamestate`]. The caller should treat
/// [`GamestateError::RmgUnsupported`] as "skip this segment".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GamestateError {
    /// The RMG manager is live; the encoder cannot emit the variable tail.
    RmgUnsupported,
    /// A `MSG_WriteBits`/`MSG_WriteBigString` constraint was violated.
    Msg(MsgError),
    /// A configstring index is outside `0..MAX_CONFIGSTRINGS`.
    ConfigStringOutOfRange(u16),
    /// The same configstring index was supplied twice.
    DuplicateConfigString(u16),
    /// A baseline's `entityState_t.number` is outside `0..MAX_GENTITIES`.
    BaselineNumberOutOfRange(i32),
    /// The same baseline entity number was supplied twice.
    DuplicateBaseline(i32),
    /// More baselines than entity slots.
    TooManyBaselines(usize),
}

impl fmt::Display for GamestateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GamestateError::RmgUnsupported => {
                write!(f, "RMG gamestate tail is not supported")
            }
            GamestateError::Msg(e) => write!(f, "{e}"),
            GamestateError::ConfigStringOutOfRange(index) => {
                write!(f, "configstring index {index} >= {MAX_CONFIGSTRINGS}")
            }
            GamestateError::DuplicateConfigString(index) => {
                write!(f, "duplicate configstring index {index}")
            }
            GamestateError::BaselineNumberOutOfRange(number) => {
                write!(f, "baseline entity number {number} out of range")
            }
            GamestateError::DuplicateBaseline(number) => {
                write!(f, "duplicate baseline entity number {number}")
            }
            GamestateError::TooManyBaselines(count) => {
                write!(f, "{count} baselines > MAX_GENTITIES {MAX_GENTITIES}")
            }
        }
    }
}

impl std::error::Error for GamestateError {}

impl From<MsgError> for GamestateError {
    fn from(value: MsgError) -> Self {
        GamestateError::Msg(value)
    }
}

fn read_i32(state: &[u8; ENTITY_STATE_SIZE], offset: usize) -> i32 {
    i32::from_le_bytes([
        state[offset],
        state[offset + 1],
        state[offset + 2],
        state[offset + 3],
    ])
}

/// `MSG_WriteDeltaEntity(msg, nullstate, baseline, force = qtrue)` for one
/// baseline: entity number, not-removed, has-delta, field count, then the
/// changed fields (the same table order and float/integer rules as the
/// shipped function).
fn write_baseline(msg: &mut MsgWriter, state: &[u8; ENTITY_STATE_SIZE]) -> Result<(), MsgError> {
    let number = read_i32(state, 0);
    let mut lc = 0usize;
    for (i, (offset, _bits)) in ENTITY_FIELDS.iter().enumerate() {
        if read_i32(state, usize::from(*offset)) != 0 {
            lc = i + 1;
        }
    }

    msg.write_bits(number, GENTITYNUM_BITS)?;
    msg.write_bits(0, 1)?; // not removed
    if lc == 0 {
        msg.write_bits(0, 1)?; // no delta
        return Ok(());
    }
    msg.write_bits(1, 1)?; // has delta
    msg.write_byte(lc as u8)?;

    // The field list is a prefix of the table; fields past `lc` are unchanged.
    for (offset, bits) in ENTITY_FIELDS.iter().take(lc) {
        let raw = read_i32(state, usize::from(*offset));
        if raw == 0 {
            msg.write_bits(0, 1)?; // no change
            continue;
        }
        msg.write_bits(1, 1)?; // changed

        if *bits == 0 {
            // Float: 0 = zero, 1 + 0 + 13 bits = integral, 1 + 1 + 32 = full.
            let value = f32::from_bits(raw as u32);
            if value == 0.0 {
                msg.write_bits(0, 1)?;
                continue;
            }
            let trunc = value as i32;
            let biased = i64::from(trunc) + i64::from(FLOAT_INT_BIAS);
            let integral = (trunc as f32) == value;
            if integral && (0..(1i64 << FLOAT_INT_BITS)).contains(&biased) {
                msg.write_bits(1, 1)?;
                msg.write_bits(0, 1)?;
                msg.write_bits(biased as i32, FLOAT_INT_BITS)?;
            } else {
                msg.write_bits(1, 1)?;
                msg.write_bits(1, 1)?;
                msg.write_bits(raw, 32)?;
            }
        } else {
            // Integer: zero flag, then the value in the field width.
            if raw == 0 {
                msg.write_bits(0, 1)?;
            } else {
                msg.write_bits(1, 1)?;
                msg.write_bits(raw, i32::from(*bits))?;
            }
        }
    }
    Ok(())
}

/// Encode the first demo frame (gamestate). `configstrings` may be in any
/// order; empty values are skipped and the rest are sorted by index.
pub fn encode_gamestate(
    header: GamestateHeader,
    configstrings: &[ConfigString<'_>],
    baselines: &[Baseline],
    rmg: RmgTail,
) -> Result<Vec<u8>, GamestateError> {
    if rmg == RmgTail::ManagerPresent {
        return Err(GamestateError::RmgUnsupported);
    }
    if baselines.len() > MAX_GENTITIES as usize {
        return Err(GamestateError::TooManyBaselines(baselines.len()));
    }

    let mut strings: Vec<&ConfigString<'_>> = configstrings
        .iter()
        .filter(|c| !c.value.is_empty())
        .collect();
    strings.sort_by_key(|c| c.index);
    for pair in strings.windows(2) {
        if pair[0].index == pair[1].index {
            return Err(GamestateError::DuplicateConfigString(pair[0].index));
        }
    }

    // Rough size: the fixture's 62 configstrings + 49 baselines fit in ~4 KiB
    // of compressed payload.
    // `SV_SendClientGameState` skips `sv.svEntities[n].baseline` when its
    // number is 0; do the same for a caller that passes the whole array.
    let mut sorted_baselines: Vec<&Baseline> = baselines
        .iter()
        .filter(|baseline| baseline.number() != 0)
        .collect();
    sorted_baselines.sort_by_key(|baseline| baseline.number());
    for pair in sorted_baselines.windows(2) {
        if pair[0].number() == pair[1].number() {
            return Err(GamestateError::DuplicateBaseline(pair[0].number()));
        }
    }

    let mut msg = MsgWriter::with_capacity(4096 + strings.len() * 32 + baselines.len() * 256);

    msg.write_long(header.last_client_command)?;
    msg.write_byte(SVC_GAMESTATE)?;
    msg.write_long(header.reliable_sequence)?;

    for configstring in strings {
        if configstring.index >= MAX_CONFIGSTRINGS {
            return Err(GamestateError::ConfigStringOutOfRange(configstring.index));
        }
        msg.write_byte(SVC_CONFIGSTRING)?;
        msg.write_short(configstring.index)?;
        msg.write_big_string(configstring.value)?;
    }

    for baseline in sorted_baselines {
        let number = baseline.number();
        if !(0..MAX_GENTITIES).contains(&number) {
            return Err(GamestateError::BaselineNumberOutOfRange(number));
        }
        msg.write_byte(SVC_BASELINE)?;
        write_baseline(&mut msg, &baseline.state)?;
    }

    msg.write_byte(SVC_EOF)?;
    msg.write_long(header.client_num)?;
    msg.write_long(header.checksum_feed)?;
    msg.write_short(0)?; // normal-map RMG tail
    msg.write_byte(SVC_EOF)?; // every demo payload ends with svc_EOF

    Ok(msg.into_payload())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    const SIDECAR_MAGIC: &[u8; 8] = b"DMGS1\0\0\0";

    fn fixture_dir() -> PathBuf {
        let root = std::env::var_os("JAMP_REPO_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."));
        root.join("tests/fixtures/demo")
    }

    /// The first payload of the demo-framed fixture.
    fn first_frame() -> Vec<u8> {
        let data = fs::read(fixture_dir().join("server-ffa3-phase0.dm_26")).unwrap();
        let sequence = i32::from_le_bytes(data[0..4].try_into().unwrap());
        let length = i32::from_le_bytes(data[4..8].try_into().unwrap());
        assert!(sequence >= 0);
        assert!(length >= 0);
        data[8..8 + length as usize].to_vec()
    }

    struct Sidecar {
        last_client_command: i32,
        reliable_sequence: i32,
        client_num: i32,
        checksum_feed: i32,
        meaningful_bits: usize,
        configstrings: Vec<ConfigString<'static>>,
        baselines: Vec<Baseline>,
    }

    impl Sidecar {
        fn read() -> Self {
            let data = fs::read(fixture_dir().join("server-ffa3-phase0.gamestate.bin")).unwrap();
            assert_eq!(&data[..8], SIDECAR_MAGIC, "sidecar magic");
            let mut cursor = 8usize;

            fn take<'a>(data: &'a [u8], cursor: &mut usize, n: usize) -> &'a [u8] {
                let start = *cursor;
                *cursor += n;
                &data[start..start + n]
            }

            let last_client_command =
                i32::from_le_bytes(take(&data, &mut cursor, 4).try_into().unwrap());
            let reliable_sequence =
                i32::from_le_bytes(take(&data, &mut cursor, 4).try_into().unwrap());
            let client_num = i32::from_le_bytes(take(&data, &mut cursor, 4).try_into().unwrap());
            let checksum_feed = i32::from_le_bytes(take(&data, &mut cursor, 4).try_into().unwrap());
            let rmg = i16::from_le_bytes(take(&data, &mut cursor, 2).try_into().unwrap());
            assert_eq!(rmg, 0, "fixture is a normal (non-RMG) map");
            let meaningful_bits =
                u32::from_le_bytes(take(&data, &mut cursor, 4).try_into().unwrap()) as usize;

            let count =
                u32::from_le_bytes(take(&data, &mut cursor, 4).try_into().unwrap()) as usize;
            let mut configstrings = Vec::with_capacity(count);
            for _ in 0..count {
                let index = u16::from_le_bytes(take(&data, &mut cursor, 2).try_into().unwrap());
                let len =
                    u32::from_le_bytes(take(&data, &mut cursor, 4).try_into().unwrap()) as usize;
                let raw = take(&data, &mut cursor, len);
                // `Box::leak` keeps the sidecar's borrowed slices alive for the
                // test process; the fixture is a few KiB.
                let value: &'static [u8] = Box::leak(raw.to_vec().into_boxed_slice());
                configstrings.push(ConfigString::new(index, value));
            }

            let count =
                u32::from_le_bytes(take(&data, &mut cursor, 4).try_into().unwrap()) as usize;
            let mut baselines = Vec::with_capacity(count);
            for _ in 0..count {
                let state: [u8; ENTITY_STATE_SIZE] = take(&data, &mut cursor, ENTITY_STATE_SIZE)
                    .try_into()
                    .unwrap();
                baselines.push(Baseline::from_state(state));
            }
            assert_eq!(cursor, data.len(), "sidecar has trailing bytes");
            Self {
                last_client_command,
                reliable_sequence,
                client_num,
                checksum_feed,
                meaningful_bits,
                configstrings,
                baselines,
            }
        }
    }

    #[test]
    fn entity_field_table_is_sane() {
        assert_eq!(ENTITY_FIELDS.len(), 132);
        let mut offsets = Vec::with_capacity(ENTITY_FIELDS.len());
        for (offset, bits) in ENTITY_FIELDS {
            assert!(usize::from(offset) + 4 <= ENTITY_STATE_SIZE);
            assert!((-32..=32).contains(&i32::from(bits)));
            assert!(!offsets.contains(&offset), "duplicate offset {offset}");
            offsets.push(offset);
        }
    }

    #[test]
    fn entity_field_table_hash_is_pinned() {
        // FNV-1a over `(offset.to_le_bytes(), bits as u8)`; generated by
        // tools/gen_demo_tables.py. A hand edit must update this pin.
        fn mix(hash: &mut u64, byte: u8) {
            *hash ^= u64::from(byte);
            *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for (offset, bits) in ENTITY_FIELDS {
            for byte in offset.to_le_bytes() {
                mix(&mut hash, byte);
            }
            mix(&mut hash, bits as u8);
        }
        assert_eq!(hash, 0x9eb4_df38_29e3_aeaa);
    }

    #[test]
    fn rejects_rmg_tail_before_writing() {
        let header = GamestateHeader {
            last_client_command: 0,
            reliable_sequence: 0,
            client_num: 0,
            checksum_feed: 0,
        };
        assert_eq!(
            encode_gamestate(header, &[], &[], RmgTail::ManagerPresent),
            Err(GamestateError::RmgUnsupported)
        );
    }

    #[test]
    fn rejects_out_of_range_and_duplicate_configstrings() {
        let header = GamestateHeader {
            last_client_command: 0,
            reliable_sequence: 0,
            client_num: 0,
            checksum_feed: 0,
        };
        let too_high = [ConfigString::new(MAX_CONFIGSTRINGS, b"x")];
        assert_eq!(
            encode_gamestate(header, &too_high, &[], RmgTail::NoManager),
            Err(GamestateError::ConfigStringOutOfRange(MAX_CONFIGSTRINGS))
        );
        let duplicate = [ConfigString::new(7, b"a"), ConfigString::new(7, b"b")];
        assert_eq!(
            encode_gamestate(header, &duplicate, &[], RmgTail::NoManager),
            Err(GamestateError::DuplicateConfigString(7))
        );
    }

    #[test]
    fn rejects_out_of_range_baseline_number() {
        let header = GamestateHeader {
            last_client_command: 0,
            reliable_sequence: 0,
            client_num: 0,
            checksum_feed: 0,
        };
        let mut state = [0u8; ENTITY_STATE_SIZE];
        state[0..4].copy_from_slice(&MAX_GENTITIES.to_le_bytes());
        let baseline = Baseline::from_state(state);
        assert_eq!(
            encode_gamestate(header, &[], &[baseline], RmgTail::NoManager),
            Err(GamestateError::BaselineNumberOutOfRange(MAX_GENTITIES))
        );
    }

    #[test]
    fn baselines_are_sorted_by_number_and_duplicates_are_rejected() {
        let header = GamestateHeader {
            last_client_command: 0,
            reliable_sequence: 0,
            client_num: 0,
            checksum_feed: 0,
        };
        let make = |number: i32| {
            let mut state = [0u8; ENTITY_STATE_SIZE];
            state[0..4].copy_from_slice(&number.to_le_bytes());
            Baseline::from_state(state)
        };
        let one = make(1);
        let two = make(2);
        let ascending =
            encode_gamestate(header, &[], &[one.clone(), two.clone()], RmgTail::NoManager).unwrap();
        let descending =
            encode_gamestate(header, &[], &[two, one.clone()], RmgTail::NoManager).unwrap();
        assert_eq!(ascending, descending);

        assert_eq!(
            encode_gamestate(header, &[], &[one.clone(), one], RmgTail::NoManager),
            Err(GamestateError::DuplicateBaseline(1))
        );

        // `SV_SendClientGameState` skips a zero-number baseline; so do we.
        let zero = Baseline::from_state([0u8; ENTITY_STATE_SIZE]);
        let with_zero = encode_gamestate(header, &[], &[zero], RmgTail::NoManager).unwrap();
        let without = encode_gamestate(header, &[], &[], RmgTail::NoManager).unwrap();
        assert_eq!(with_zero, without);
    }

    #[test]
    fn reencodes_the_phase0_fixture_meaningfully_byte_exact() {
        let sidecar = Sidecar::read();
        let header = GamestateHeader {
            last_client_command: sidecar.last_client_command,
            reliable_sequence: sidecar.reliable_sequence,
            client_num: sidecar.client_num,
            checksum_feed: sidecar.checksum_feed,
        };
        let encoded = encode_gamestate(
            header,
            &sidecar.configstrings,
            &sidecar.baselines,
            RmgTail::NoManager,
        )
        .unwrap();
        let original = first_frame();

        let full = sidecar.meaningful_bits / 8;
        let tail = sidecar.meaningful_bits % 8;
        assert!(
            encoded.len() >= full,
            "encoded {} < meaningful {full}",
            encoded.len()
        );
        assert_eq!(encoded[..full], original[..full], "meaningful bytes differ");
        if tail > 0 {
            let mask = (1u8 << tail) - 1;
            assert_eq!(
                encoded[full] & mask,
                original[full] & mask,
                "last meaningful bits differ"
            );
        }
        // The Phase 0 fixture has no uninitialised padding byte; the encoder
        // writes the same payload length.
        assert_eq!(encoded.len(), original.len());
    }
}
