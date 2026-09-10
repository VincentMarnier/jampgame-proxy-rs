//! Proxy-local string/name helpers (`Proxy_Utils.{hpp,cpp}`,
//! `Proxy_Engine_Utils.hpp`). These are the proxy's *own* code (not the game's)
//! and are implemented verbatim in Rust.

use crate::sdk::Q_COLOR_ESCAPE;

/// `Q_IsValidAsciiStr` (`Proxy_Utils.cpp:3-17`): every byte must be in
/// `0x20..=0x7E` before the NUL terminator.
pub fn is_valid_ascii_str(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| (0x20..=0x7E).contains(&b))
}

/// `Q_strchrs` (`Proxy_Utils.cpp:28-47`): index of the first byte of `bytes`
/// that occurs in `search`, or `None`.
pub fn strchrs(bytes: &[u8], search: &[u8]) -> Option<usize> {
    bytes.iter().position(|&b| search.contains(&b))
}

/// Proxy-local `Q_IsColorStringExt` (`Proxy_Utils.hpp:5`): `^[0-9]` at the
/// start of `bytes`.
pub fn is_color_string_ext(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == Q_COLOR_ESCAPE && bytes[1].is_ascii_digit()
}

/// `Proxy_ClientCleanName` (`Proxy_Engine_Utils.hpp:22-103`): sanitise a client
/// name into `out` (which must be `out_size` bytes, e.g. `MAX_NETNAME`).
/// Byte-for-byte port of the C loop.
pub fn client_clean_name(input: &[u8], out: &mut [u8]) {
    let out_size = out.len();
    let mut in_off = 0usize;

    // discard leading spaces and asterisks
    while in_off < input.len() && input[in_off] == b' ' {
        in_off += 1;
    }
    while in_off < input.len() && input[in_off] == b'*' {
        in_off += 1;
    }

    let mut outpos = 0usize;
    let mut colorless_len: i32 = 0;

    while in_off < input.len() && (outpos as i32) < (out_size as i32 - 1) {
        let c = input[in_off];
        out[outpos] = c;

        if in_off + 1 < input.len() && in_off + 2 < input.len() {
            // don't allow more than 3 consecutive spaces
            if c == b' ' && input[in_off + 1] == b' ' && input[in_off + 2] == b' ' {
                in_off += 1;
                continue;
            }
            // don't allow too many consecutive @ signs
            if c == b'@' && input[in_off + 1] == b'@' && input[in_off + 2] == b'@' {
                in_off += 1;
                continue;
            }
        }

        if c < 0x20 {
            in_off += 1;
            continue;
        }

        match c {
            0x81 | 0x8D | 0x8F | 0x90 | 0x9D | 0xA0 | 0xAD => {
                in_off += 1;
                continue;
            }
            _ => {}
        }

        if outpos > 0 && out[outpos - 1] == Q_COLOR_ESCAPE {
            if is_color_string_ext(&out[outpos - 1..]) {
                colorless_len -= 1;
            } else {
                colorless_len += 1;
            }
        } else {
            colorless_len += 1;
        }
        outpos += 1;
        in_off += 1;
    }

    out[outpos] = 0;

    // don't allow empty names
    if out[0] == 0 || colorless_len == 0 {
        strncpyz(out, b"Padawan");
    }
}

/// `Q_strncpyz` semantics: copy `src` into `out` up to `len-1` bytes, NUL-terminate.
pub fn strncpyz(out: &mut [u8], src: &[u8]) {
    let n = out.len();
    if n == 0 {
        return;
    }
    let copy = n - 1;
    let m = copy.min(src.len());
    out[..m].copy_from_slice(&src[..m]);
    out[m] = 0;
}

/// C `atoi`: skip leading whitespace, optional sign, digits; 0 if no digits.
pub fn atoi(bytes: &[u8]) -> i32 {
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let neg = i < bytes.len() && bytes[i] == b'-';
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let mut acc: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        acc = acc * 10 + i64::from(bytes[i] - b'0');
        i += 1;
    }
    let acc = if neg { -acc } else { acc };
    acc.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// `strlen` of a NUL-terminated byte slice region (helper for the trap guard).
pub fn c_strlen(bytes: &[u8]) -> usize {
    bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len())
}

/// `Q_StripColor` (`Proxy_Utils.cpp:49-83`): repeatedly remove `^[0-9]` color
/// escapes from `text` in place.
pub fn strip_color(text: &mut [u8]) {
    let mut do_pass = true;
    while do_pass {
        do_pass = false;
        let mut read = 0usize;
        let mut write = 0usize;
        while read < text.len() && text[read] != 0 {
            if is_color_string_ext(&text[read..]) {
                do_pass = true;
                read += 2;
            } else {
                if write != read {
                    text[write] = text[read];
                }
                write += 1;
                read += 1;
            }
        }
        if write < read && write < text.len() {
            text[write] = 0;
        }
    }
}

/// `calcRatio` (`Proxy_Utils.cpp:85-103`): K/D or damage ratio used by the
/// intermission stats tables.
pub fn calc_ratio(kill: i32, death: i32) -> f32 {
    if kill == 0 && death == 0 {
        1.00
    } else if kill < 1 && death >= 1 {
        0.00
    } else if kill >= 1 && death <= 1 {
        kill as f32
    } else {
        kill as f32 / death as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ascii() {
        assert!(is_valid_ascii_str(b"abc DEF09~"));
        assert!(!is_valid_ascii_str(b"abc\x01"));
        assert!(!is_valid_ascii_str(b"abc\x7f"));
    }

    #[test]
    fn strchrs_finds_any() {
        assert_eq!(strchrs(b"say hello", b"\r\n"), None);
        assert_eq!(strchrs(b"a;b", b";"), Some(1));
        assert_eq!(strchrs(b"a\rb", b"\r\n"), Some(1));
        assert_eq!(strchrs(b"a\nb", b"\r\n"), Some(1));
    }

    #[test]
    fn color_escape() {
        assert!(is_color_string_ext(b"^3red"));
        assert!(!is_color_string_ext(b"^fred"));
        assert!(!is_color_string_ext(b"^"));
        assert!(!is_color_string_ext(b"abc"));
    }

    #[test]
    fn clean_name_leading_junk() {
        let mut out = [0u8; 36];
        // Leading spaces and asterisks are discarded; the spaces *after* the
        // asterisks are ordinary content (the C loop only strips the prefix).
        client_clean_name(b"  **  Hello", &mut out);
        assert_eq!(c_strlen(&out), 7);
        assert_eq!(&out[..8], b"  Hello\0");
    }

    #[test]
    fn clean_name_empty_falls_back_to_padawan() {
        let mut out = [0u8; 36];
        client_clean_name(b"   ", &mut out);
        assert_eq!(c_strlen(&out), 7);
        assert_eq!(&out[..7], b"Padawan");
    }

    #[test]
    fn clean_name_triple_space_collapse() {
        let mut out = [0u8; 36];
        client_clean_name(b"a   b", &mut out);
        // "a   b" -> "a  b" (three-space run allowed to leave two: see the
        // original's `continue`-then-overwrite behaviour); pin the bytes.
        assert_eq!(&out[..5], b"a  b\0");
    }

    #[test]
    fn atoi_matches_c() {
        assert_eq!(atoi(b"42"), 42);
        assert_eq!(atoi(b"  -7"), -7);
        assert_eq!(atoi(b"abc"), 0);
        assert_eq!(atoi(b"2147483648"), i32::MAX);
        assert_eq!(atoi(b"-2147483649"), i32::MIN);
        assert_eq!(atoi(b"12x"), 12);
    }
}
