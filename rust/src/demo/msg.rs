//! Pure-Rust port of the `MSG_*` write and read paths needed by the demo
//! modules.
//!
//! `MSG_WriteBits` (see `docs/demo-format.md` §2) writes the low `bits & 7`
//! bits raw (LSB first) and every following whole byte as a symbol of the
//! **static** `msg_hData` Huffman tree. The tree is preloaded once by
//! `MSG_initHuffman`; `MSG_WriteBits` only walks it (`Huff_offsetTransmit` does
//! not call `Huff_addRef`), so the embedded code table is enough and there is
//! no state to carry between messages. The demo metadata reader walks the same
//! static tree in the other direction ([`MsgReader`]).

use std::fmt;
use std::sync::OnceLock;

use super::fields::HUFFMAN_CODES;

/// `msg_t` bitstream behaviour that can be violated by a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgError {
    /// `MSG_WriteBits`: `bits == 0 || bits < -31 || bits > 32`.
    InvalidBitCount(i32),
    /// `MSG_WriteBigString`: `strlen(s) >= BIG_INFO_STRING` (8192).
    StringTooLong(usize),
}

impl fmt::Display for MsgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MsgError::InvalidBitCount(bits) => write!(f, "MSG_WriteBits: {bits} bits"),
            MsgError::StringTooLong(len) => {
                write!(f, "MSG_WriteBigString: {len} bytes >= BIG_INFO_STRING")
            }
        }
    }
}

impl std::error::Error for MsgError {}

/// `MSG_WriteBigString`'s buffer size (`BIG_INFO_STRING`, `q_shared.h:388`).
pub const BIG_INFO_STRING: usize = 8192;

/// The `msg_t` bit cursor and output buffer (`data` + `bit` fields).
#[derive(Debug, Clone, Default)]
pub struct MsgWriter {
    data: Vec<u8>,
    bit: usize,
}

impl MsgWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            data: Vec::with_capacity(bytes),
            bit: 0,
        }
    }

    /// The engine's `msg->bit` (compressed bit offset).
    pub fn bit(&self) -> usize {
        self.bit
    }

    /// The encoded payload (`msg->data[..cursize]`).
    ///
    /// `cursize` follows the engine's `MSG_WriteBits` convention
    /// (`msg.cpp:207`): `(bit >> 3) + 1`. The extra byte is reserved as soon
    /// as the cursor lands on a byte boundary, so a message whose last symbol
    /// ends there is one byte longer than the bits alone suggest. The demo
    /// client's `MSG_ReadByte` ("readcount > cursize") depends on it: without
    /// the byte, the trailing `svc_EOF` read reports -1 and
    /// `CL_ParseServerMessage` rejects the message.
    pub fn payload(&self) -> &[u8] {
        &self.data
    }

    pub fn into_payload(self) -> Vec<u8> {
        self.data
    }

    /// `Huff_putBit`: set one bit, zeroing a byte when it is first opened.
    fn put_bit(&mut self, bit: u32) {
        let index = self.bit >> 3;
        if index == self.data.len() {
            self.data.push(0);
        }
        self.data[index] |= ((bit & 1) as u8) << (self.bit & 7);
        self.bit += 1;
        if self.bit & 7 == 0 {
            // `MSG_WriteBits` ends with `msg->cursize = (msg->bit >> 3) + 1`:
            // reserve the byte the next symbol would start in. When the last
            // symbol ends exactly on a boundary this zero byte is the
            // `cursize` byte the reader expects; it is never decoded.
            self.data.push(0);
        }
    }

    /// `Huff_offsetTransmit(msgHuff.compressor, byte)` for one symbol.
    fn put_symbol(&mut self, byte: u8) {
        let (code, len) = HUFFMAN_CODES[byte as usize];
        for i in 0..len {
            self.put_bit(u32::from((code >> i) & 1));
        }
    }

    /// `MSG_WriteBits`: low `bits & 7` raw bits, then whole bytes as Huffman
    /// symbols. A negative `bits` means a signed value only when *reading*; on
    /// the wire the count is the same and the value is masked by the shifts.
    pub fn write_bits(&mut self, value: i32, bits: i32) -> Result<(), MsgError> {
        if bits == 0 || !(-31..=32).contains(&bits) {
            return Err(MsgError::InvalidBitCount(bits));
        }
        let mut bits = bits.unsigned_abs();
        let mut value = value as u32;

        let raw = bits & 7;
        for _ in 0..raw {
            self.put_bit(value & 1);
            value >>= 1;
        }
        bits -= raw;

        while bits > 0 {
            self.put_symbol((value & 0xff) as u8);
            value >>= 8;
            bits = bits.saturating_sub(8);
        }
        Ok(())
    }

    pub fn write_byte(&mut self, value: u8) -> Result<(), MsgError> {
        self.write_bits(i32::from(value), 8)
    }

    pub fn write_short(&mut self, value: u16) -> Result<(), MsgError> {
        self.write_bits(i32::from(value), 16)
    }

    pub fn write_long(&mut self, value: i32) -> Result<(), MsgError> {
        self.write_bits(value, 32)
    }

    /// `MSG_WriteData`: each byte through the MSG path (not a raw memcpy).
    pub fn write_data(&mut self, bytes: &[u8]) -> Result<(), MsgError> {
        for &byte in bytes {
            self.write_byte(byte)?;
        }
        Ok(())
    }

    /// `MSG_WriteString`: the bytes followed by a NUL.
    pub fn write_string(&mut self, bytes: &[u8]) -> Result<(), MsgError> {
        self.write_data(bytes)?;
        self.write_byte(0)
    }

    /// `MSG_WriteBigString`: same wire form, but `BIG_INFO_STRING` bounds it.
    pub fn write_big_string(&mut self, bytes: &[u8]) -> Result<(), MsgError> {
        if bytes.len() >= BIG_INFO_STRING {
            return Err(MsgError::StringTooLong(bytes.len()));
        }
        self.write_string(bytes)
    }
}

// ---------------------------------------------------------------------------
// Read path
// ---------------------------------------------------------------------------

/// One node of the static decoding tree. The tree is built from
/// [`HUFFMAN_CODES`]: `bit 0` of a code is the first transmitted bit, so child
/// 0/1 is entered with bit 0/1 of the cursor.
#[derive(Default)]
struct HuffNode {
    child: [Option<u16>; 2],
    symbol: Option<u8>,
}

struct HuffDecoder {
    nodes: Vec<HuffNode>,
}

impl HuffDecoder {
    fn build() -> Self {
        let mut nodes = vec![HuffNode::default()];
        for (symbol, &(code, len)) in HUFFMAN_CODES.iter().enumerate() {
            let mut node = 0usize;
            for bit in 0..usize::from(len) {
                let branch = usize::from((code >> bit) & 1);
                let next = match nodes[node].child[branch] {
                    Some(next) => usize::from(next),
                    None => {
                        nodes.push(HuffNode::default());
                        let next = nodes.len() - 1;
                        nodes[node].child[branch] = Some(next as u16);
                        next
                    }
                };
                node = next;
            }
            nodes[node].symbol = Some(symbol as u8);
        }
        Self { nodes }
    }

    fn symbol(&self, next_bit: impl FnMut() -> Option<u32>) -> Option<u8> {
        let mut next_bit = next_bit;
        let mut node = 0usize;
        for _ in 0..16 {
            let branch = next_bit()? as usize;
            let next = self.nodes[node].child[branch]?;
            node = usize::from(next);
            if let Some(symbol) = self.nodes[node].symbol {
                return Some(symbol);
            }
        }
        None
    }
}

fn decoder() -> &'static HuffDecoder {
    static DECODER: OnceLock<HuffDecoder> = OnceLock::new();
    DECODER.get_or_init(HuffDecoder::build)
}

/// A `msg_t` reader for the demo metadata scan.
///
/// It mirrors `MSG_ReadBits`: the low `bits & 7` bits are raw (LSB first) and
/// each following byte is one symbol of the static Huffman tree. Every read
/// returns `None` on the end of the payload, so a truncated or foreign file
/// cannot panic the HTTP thread. It does not need the `readcount`/`cursize`
/// convention: the demo container already gives each payload its exact length.
pub struct MsgReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> MsgReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    /// The consumed bit offset (`msg->bit`).
    pub fn bit(&self) -> usize {
        self.bit
    }

    fn get_bit(&mut self) -> Option<u32> {
        let index = self.bit >> 3;
        let byte = *self.data.get(index)?;
        let bit = u32::from((byte >> (self.bit & 7)) & 1);
        self.bit += 1;
        Some(bit)
    }

    fn read_symbol(&mut self) -> Option<u8> {
        let data = self.data;
        let mut bit = self.bit;
        let symbol = decoder().symbol(|| {
            let index = bit >> 3;
            let byte = *data.get(index)?;
            let value = u32::from((byte >> (bit & 7)) & 1);
            bit += 1;
            Some(value)
        })?;
        self.bit = bit;
        Some(symbol)
    }

    /// `MSG_ReadBits` (the non-OOB form).
    pub fn read_bits(&mut self, bits: i32) -> Option<u32> {
        if !(1..=32).contains(&bits) {
            return None;
        }
        let bits = bits as u32;
        let raw = bits & 7;
        let mut value = 0u64;
        for index in 0..raw {
            value |= u64::from(self.get_bit()?) << index;
        }
        let mut shift = raw;
        let mut remaining = bits - raw;
        while remaining > 0 {
            value |= u64::from(self.read_symbol()?) << shift;
            shift += 8;
            remaining = remaining.saturating_sub(8);
        }
        Some(value as u32)
    }

    /// `MSG_ReadByte`.
    pub fn read_byte(&mut self) -> Option<u8> {
        self.read_bits(8).map(|value| value as u8)
    }

    /// `MSG_ReadShort` as the signed 16-bit wire value.
    pub fn read_short(&mut self) -> Option<i16> {
        self.read_bits(16).map(|value| value as i16)
    }

    /// `MSG_ReadLong` as the signed 32-bit wire value.
    pub fn read_long(&mut self) -> Option<i32> {
        self.read_bits(32).map(|value| value as i32)
    }

    /// `MSG_ReadData` / consume `bits` bits without keeping them.
    pub fn skip_bits(&mut self, bits: i32) -> Option<()> {
        let mut remaining = bits;
        while remaining > 0 {
            let chunk = remaining.min(32);
            self.read_bits(chunk)?;
            remaining -= chunk;
        }
        Some(())
    }

    /// `MSG_ReadString`/`MSG_ReadBigString`: bytes up to the NUL (or `max`).
    pub fn read_string(&mut self, max: usize) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        while out.len() + 1 < max {
            let byte = self.read_byte()?;
            if byte == 0 {
                return Some(out);
            }
            out.push(byte);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_round_trips_writer_values() {
        let mut writer = MsgWriter::new();
        writer.write_long(0x1234_5678).unwrap();
        writer.write_byte(0).unwrap();
        writer.write_short(0xfffe).unwrap();
        writer
            .write_string(b"n\\viper\\model\\luke/default")
            .unwrap();
        let payload = writer.into_payload();

        let mut reader = MsgReader::new(&payload);
        assert_eq!(reader.read_long(), Some(0x1234_5678));
        assert_eq!(reader.read_byte(), Some(0));
        assert_eq!(reader.read_short(), Some(-2));
        assert_eq!(
            reader.read_string(1024).as_deref(),
            Some(&b"n\\viper\\model\\luke/default"[..])
        );
    }

    #[test]
    fn reader_covers_all_symbols_and_rejects_truncation() {
        for value in 0..=255u8 {
            let mut writer = MsgWriter::new();
            writer.write_byte(value).unwrap();
            let mut reader = MsgReader::new(writer.payload());
            assert_eq!(reader.read_byte(), Some(value));
        }
        assert_eq!(MsgReader::new(&[]).read_long(), None);
        assert_eq!(MsgReader::new(&[]).read_byte(), None);
    }

    #[test]
    fn huffman_table_covers_all_symbols_and_is_prefix_free() {
        assert_eq!(HUFFMAN_CODES.len(), 256);
        let decoded: Vec<(u16, u8)> = HUFFMAN_CODES.to_vec();
        for (code, len) in &decoded {
            assert!((1..=16).contains(len), "bad length for 0x{code:x}: {len}");
            assert!(*code < (1u16 << len), "code 0x{code:x} exceeds {len} bits");
        }
        // No code is a prefix of another one.
        for (i, (a, alen)) in decoded.iter().enumerate() {
            for (b, blen) in decoded.iter().skip(i + 1) {
                match alen.cmp(blen) {
                    std::cmp::Ordering::Equal => assert_ne!(a, b),
                    std::cmp::Ordering::Less => {
                        let mask = (1u16 << alen) - 1;
                        assert_ne!(a & mask, b & mask, "0x{a:x}/{alen} prefixes 0x{b:x}/{blen}");
                    }
                    std::cmp::Ordering::Greater => {
                        let mask = (1u16 << blen) - 1;
                        assert_ne!(b & mask, a & mask, "0x{b:x}/{blen} prefixes 0x{a:x}/{alen}");
                    }
                }
            }
        }
    }

    #[test]
    fn huffman_table_hash_is_pinned() {
        // FNV-1a over `(code.to_le_bytes(), len)` for all entries. Generated
        // by tools/gen_demo_tables.py; a hand edit must update this pin.
        fn mix(hash: &mut u64, byte: u8) {
            *hash ^= u64::from(byte);
            *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for (code, len) in HUFFMAN_CODES {
            for byte in code.to_le_bytes() {
                mix(&mut hash, byte);
            }
            mix(&mut hash, len);
        }
        assert_eq!(hash, 0xbf82_9e7b_f68f_9306);
    }

    #[test]
    fn write_bits_matches_the_static_huffman_codes() {
        let mut msg = MsgWriter::new();
        msg.write_bits(0, 8).unwrap();
        assert_eq!(msg.payload(), &[0x02]);

        let mut msg = MsgWriter::new();
        msg.write_bits(1, 8).unwrap();
        assert_eq!(msg.payload(), &[0x1b]);

        // 16 bits: two whole-byte symbols, low byte first.
        let mut msg = MsgWriter::new();
        msg.write_bits(0x1234, 16).unwrap();
        let mut low = MsgWriter::new();
        low.write_bits(0x34, 8).unwrap();
        let low_bit = low.bit();
        let mut low_bytes = low.into_payload();
        // `cursize = (bit >> 3) + 1` reserves the byte *after* a symbol that
        // ends on a boundary; when the next symbol follows, that byte is
        // shared, not duplicated.
        if low_bit & 7 == 0 {
            low_bytes.pop();
        }
        let mut high = MsgWriter::new();
        high.write_bits(0x12, 8).unwrap();
        let high_bytes = high.into_payload();
        let mut expected = low_bytes;
        expected.extend_from_slice(&high_bytes);
        assert_eq!(msg.payload(), expected.as_slice());
    }

    #[test]
    fn payload_length_uses_the_engine_cursize_convention() {
        // msg.cpp:207: `msg->cursize = (msg->bit >> 3) + 1` after every write.
        // A message whose last symbol ends exactly on a byte boundary is
        // therefore one byte longer than its bits; the demo client's
        // MSG_ReadByte ("readcount > cursize") relies on that extra byte for
        // the trailing svc_EOF.
        assert!(MsgWriter::new().payload().is_empty());
        let mut boundary = 0;
        for value in 0..=255u8 {
            let mut msg = MsgWriter::new();
            msg.write_byte(value).unwrap();
            let bit = msg.bit();
            assert_eq!(
                msg.payload().len(),
                (bit >> 3) + 1,
                "symbol 0x{value:02x} ({bit} bits)"
            );
            if bit & 7 == 0 {
                boundary += 1;
            }
        }
        assert!(boundary > 0, "no single-symbol write lands on a boundary");
    }

    #[test]
    fn raw_and_huffman_bits_share_the_cursor() {
        // 13 bits (FLOAT_INT_BITS): 5 raw bits + one Huffman byte.
        let mut msg = MsgWriter::new();
        msg.write_bits(0x1f90, 13).unwrap();
        assert_eq!(msg.bit(), 5 + usize::from(HUFFMAN_CODES[0xfc].1));
        assert_eq!(msg.payload().len(), (msg.bit() >> 3) + 1);
        // The low five bits are raw, so they must be the low five of 0x1f90.
        assert_eq!(msg.payload()[0] & 0x1f, 0x10);
    }

    #[test]
    fn negative_bits_count_is_unsigned_on_the_wire() {
        let mut signed = MsgWriter::new();
        signed.write_bits(-1, -16).unwrap();
        let mut unsigned = MsgWriter::new();
        unsigned.write_bits(0xffff, 16).unwrap();
        assert_eq!(signed.payload(), unsigned.payload());
    }

    #[test]
    fn invalid_bit_counts_are_rejected() {
        let mut msg = MsgWriter::new();
        assert_eq!(msg.write_bits(0, 0), Err(MsgError::InvalidBitCount(0)));
        assert_eq!(msg.write_bits(0, 33), Err(MsgError::InvalidBitCount(33)));
        assert_eq!(msg.write_bits(0, -32), Err(MsgError::InvalidBitCount(-32)));
        assert!(msg.payload().is_empty());
    }

    #[test]
    fn big_strings_are_bounded() {
        let mut msg = MsgWriter::new();
        let too_long = vec![b'a'; BIG_INFO_STRING];
        assert_eq!(
            msg.write_big_string(&too_long),
            Err(MsgError::StringTooLong(BIG_INFO_STRING))
        );
        let mut ok = MsgWriter::new();
        ok.write_big_string(&[b'a'; BIG_INFO_STRING - 1]).unwrap();
        assert!(!ok.payload().is_empty());
    }
}
