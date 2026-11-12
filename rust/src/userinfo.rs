//! Info-string helpers and the OpenJK `G_ValidateUserinfo` port (O8,
//! upstream `3b3b252`).
//!
//! Why the proxy needs its own copy: the game's `Info_SetValueForKey` returns
//! `void` and silently adds nothing when the result would not fit, while
//! `Info_ValueForKey` matches a key without case. A userinfo close to
//! `MAX_INFO_STRING` with a capitalised `Forcepowers` (or two `forcepowers`
//! keys) therefore kept the bad value: the remover compares with case, the
//! writer adds nothing, and the game reads the old value case-insensitively.
//!
//! The C++ proxy fixes this on its SDK copy (return value) plus OpenJK's
//! validation. This module is the Rust equivalent: `validate_userinfo` runs
//! before anything is read, and every write goes through
//! [`set_value_for_key`], which reports failure so the sanitiser can refuse
//! the client instead of leaving the value in place.
//!
//! The functions here are pure: no engine or game call, so the rules are unit
//! tested directly. `shared_api::sanitize_userinfo` is the caller.

use crate::sdk::MAX_INFO_STRING;

/// One entry of OpenJK's `userinfoFields` table
/// (`codemp/game/g_client.c`, `g_userinfoValidate` default `25165823`):
/// the number of times each known key may appear, counted without case.
struct Field {
    name: &'static str,
    min: u32,
    max: u32,
}

const FIELDS: [Field; 21] = [
    // Not allowed, q3fill protection.
    Field {
        name: "cl_guid",
        min: 0,
        max: 0,
    },
    Field {
        name: "cl_punkbuster",
        min: 0,
        max: 0,
    },
    // The engine adds this.
    Field {
        name: "ip",
        min: 0,
        max: 1,
    },
    Field {
        name: "name",
        min: 1,
        max: 1,
    },
    Field {
        name: "rate",
        min: 1,
        max: 1,
    },
    Field {
        name: "snaps",
        min: 1,
        max: 1,
    },
    Field {
        name: "model",
        min: 1,
        max: 1,
    },
    Field {
        name: "forcepowers",
        min: 1,
        max: 1,
    },
    Field {
        name: "color1",
        min: 1,
        max: 1,
    },
    Field {
        name: "color2",
        min: 1,
        max: 1,
    },
    Field {
        name: "handicap",
        min: 1,
        max: 1,
    },
    Field {
        name: "sex",
        min: 0,
        max: 1,
    },
    Field {
        name: "cg_predictItems",
        min: 1,
        max: 1,
    },
    Field {
        name: "saber1",
        min: 1,
        max: 1,
    },
    Field {
        name: "saber2",
        min: 1,
        max: 1,
    },
    Field {
        name: "char_color_red",
        min: 1,
        max: 1,
    },
    Field {
        name: "char_color_green",
        min: 1,
        max: 1,
    },
    Field {
        name: "char_color_blue",
        min: 1,
        max: 1,
    },
    // Optional.
    Field {
        name: "teamtask",
        min: 0,
        max: 1,
    },
    Field {
        name: "password",
        min: 0,
        max: 1,
    },
    // Only registered in cgame, not sent when connecting.
    Field {
        name: "teamoverlay",
        min: 0,
        max: 1,
    },
];

/// `Info_ValueForKey` (`game/q_shared.c:1051`): the value of the first pair
/// whose key matches without case, or an empty slice.
pub fn value_for_key<'a>(info: &'a [u8], key: &[u8]) -> &'a [u8] {
    let mut pos = 0;
    while pos < info.len() {
        let (pair_start, key_start, key_end, value_start, value_end, next) = parse_pair(info, pos);
        let _ = (pair_start, value_start);
        if info[key_start..key_end].eq_ignore_ascii_case(key) {
            return &info[value_start..value_end];
        }
        if next <= pos {
            break;
        }
        pos = next;
    }
    &[]
}

/// A copy of `info` with the first pair whose key matches exactly removed
/// (`Info_RemoveKey`, `strcmp` semantics). Used to keep the proxy's own
/// feature keys out of recorded demo serverinfo.
pub fn without_key(info: &[u8], key: &[u8]) -> Vec<u8> {
    let mut out = info.to_vec();
    remove_key(&mut out, key);
    out
}

/// `Q_stricmpn(value, prefix, prefix.len()) == 0` for an ASCII prefix, with the
/// implicit "value is long enough" test. Model names are asset names (ASCII);
/// the game's `tolower` leaves bytes >= 0x80 unchanged in the C locale, so
/// this is the same comparison.
pub fn starts_with_ignore_case(value: &[u8], prefix: &[u8]) -> bool {
    value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// `Info_SetValueForKey` with a result (upstream `3b3b252`): write the pair in
/// front of `info`, on the NUL-terminated buffer. Returns `false` when a
/// forbidden character would be written or when the result does not fit, in
/// which case `info` is unchanged.
///
/// The length test is `new + old >= MAX_INFO_STRING`, as the fixed SDK copy:
/// the retail `>` let a pair of exactly `MAX_INFO_STRING` write the terminator
/// one byte past the buffer (O12).
pub fn set_value_for_key(info: &mut [u8], key: &[u8], value: &[u8]) -> bool {
    if info.is_empty() {
        return false;
    }
    let len = info.iter().position(|&b| b == 0).unwrap_or(info.len());
    let mut bytes = info[..len].to_vec();

    if !set_value_for_key_vec(&mut bytes, key, value) {
        return false;
    }
    if bytes.len() >= info.len() {
        return false;
    }

    info[..bytes.len()].copy_from_slice(&bytes);
    info[bytes.len()] = 0;
    true
}

/// The byte-string core of [`set_value_for_key`], for callers that already
/// hold the userinfo as a `Vec` (the sanitiser core). Same `MAX_INFO_STRING`
/// bound and same forbidden characters.
pub fn set_value_for_key_vec(info: &mut Vec<u8>, key: &[u8], value: &[u8]) -> bool {
    if info.len() >= MAX_INFO_STRING {
        return false;
    }
    if key
        .iter()
        .chain(value)
        .any(|&b| matches!(b, b'\\' | b';' | b'"'))
    {
        return false;
    }

    remove_key(info, key);

    if value.is_empty() {
        return true;
    }

    let mut new_pair = Vec::with_capacity(2 + key.len() + value.len() + info.len());
    new_pair.push(b'\\');
    new_pair.extend_from_slice(key);
    new_pair.push(b'\\');
    new_pair.extend_from_slice(value);

    if new_pair.len() + info.len() >= MAX_INFO_STRING {
        return false;
    }

    new_pair.extend_from_slice(info);
    *info = new_pair;
    true
}

/// `Info_RemoveKey` (`game/q_shared.c:1147`): remove the first pair whose key
/// matches **with case** (`strcmp`), leaving the others.
fn remove_key(info: &mut Vec<u8>, key: &[u8]) {
    let mut pos = 0;
    while pos < info.len() {
        let (pair_start, key_start, key_end, _value_start, _value_end, next) =
            parse_pair(info, pos);
        if info[key_start..key_end] == *key {
            info.drain(pair_start..next);
            return;
        }
        if next <= pos {
            break;
        }
        pos = next;
    }
}

/// Parse one `\key\value` pair starting at or after `pos`.
///
/// Returns `(pair_start, key_start, key_end, value_start, value_end, next)`.
/// `next` is the start of the following pair (the `\` before its key) or
/// `info.len()` at the end. Mirrors the loops of `Info_ValueForKey` /
/// `Info_RemoveKey`: a leading `\` is optional, a key runs to the next `\`,
/// and a value runs to the next `\` or the end.
fn parse_pair(info: &[u8], pos: usize) -> (usize, usize, usize, usize, usize, usize) {
    let pair_start = pos;
    let mut s = pos;
    if s < info.len() && info[s] == b'\\' {
        s += 1;
    }
    let key_start = s;
    while s < info.len() && info[s] != b'\\' {
        s += 1;
    }
    let key_end = s;
    if s < info.len() {
        s += 1; // the '\' after the key
    }
    let value_start = s;
    while s < info.len() && info[s] != b'\\' {
        s += 1;
    }
    let value_end = s;
    (pair_start, key_start, key_end, value_start, value_end, s)
}

/// OpenJK's `G_ValidateUserinfo` with its default settings: size, leading and
/// trailing slash, even number of slashes, the characters `\n \r ; "`, and the
/// number of each known key counted without case. Extended ASCII is not
/// checked. Returns the reason on failure.
///
/// The retail client sends every required key with its default settings, so a
/// normal connection passes. See `docs/ybeproxy-2026-delta.md` (S6).
pub fn validate_userinfo(info: &[u8]) -> Result<(), String> {
    if info.is_empty() {
        return Err("Userinfo too short".to_string());
    }
    if info.len() >= MAX_INFO_STRING {
        return Err("Userinfo too long".to_string());
    }
    if info[0] != b'\\' {
        return Err("Missing leading slash".to_string());
    }
    if info[info.len() - 1] == b'\\' {
        return Err("Trailing slash".to_string());
    }
    if info.iter().filter(|&&b| b == b'\\').count() % 2 != 0 {
        return Err("Bad number of slashes".to_string());
    }
    if info
        .iter()
        .any(|&b| matches!(b, b'\n' | b'\r' | b';' | b'"'))
    {
        return Err("Invalid characters found".to_string());
    }

    let mut counts = [0u32; FIELDS.len()];
    let mut pos = 0;
    while pos < info.len() {
        let (_, key_start, key_end, _, _, next) = parse_pair(info, pos);
        let key = &info[key_start..key_end];
        if key.is_empty() || next <= pos {
            break;
        }
        for (i, field) in FIELDS.iter().enumerate() {
            if key.eq_ignore_ascii_case(field.name.as_bytes()) {
                counts[i] += 1;
            }
        }
        pos = next;
    }

    for (i, field) in FIELDS.iter().enumerate() {
        if field.min > 0 && counts[i] == 0 {
            return Err(format!("{} field not found", field.name));
        }
        if counts[i] > field.max {
            return Err(format!(
                "Too many {} fields ({}/{})",
                field.name, counts[i], field.max
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A userinfo with every required key, as the retail client sends it.
    fn valid_userinfo() -> Vec<u8> {
        b"\\ip\\127.0.0.1:29070\\name\\Padawan\\rate\\25000\\snaps\\20\
          \\model\\kyle/default\\forcepowers\\7-1-030000000000003332\
          \\color1\\4\\color2\\4\\handicap\\100\\cg_predictItems\\1\
          \\saber1\\single_1\\saber2\\none\\char_color_red\\255\
          \\char_color_green\\255\\char_color_blue\\255"
            .to_vec()
    }

    #[test]
    fn valid_retail_userinfo_passes() {
        assert_eq!(validate_userinfo(&valid_userinfo()), Ok(()));
    }

    #[test]
    fn value_for_key_matches_without_case_and_returns_the_first_pair() {
        let info = b"\\Name\\first\\name\\second\\model\\kyle";
        assert_eq!(value_for_key(info, b"name"), b"first");
        assert_eq!(value_for_key(info, b"NAME"), b"first");
        assert_eq!(value_for_key(info, b"model"), b"kyle");
        assert_eq!(value_for_key(info, b"missing"), b"");
        assert_eq!(value_for_key(b"", b"name"), b"");
    }

    #[test]
    fn set_value_for_key_removes_with_case_and_prepends() {
        // The retail remover compares with case: `Name` stays, and the game
        // reads the prepended `name` first.
        let initial = b"\\Name\\old\\model\\kyle";
        let mut info = [0u8; MAX_INFO_STRING];
        info[..initial.len()].copy_from_slice(initial);
        assert!(set_value_for_key(&mut info, b"name", b"new"));
        let len = info.iter().position(|&b| b == 0).unwrap();
        assert_eq!(&info[..len], b"\\name\\new\\Name\\old\\model\\kyle");
        assert_eq!(
            value_for_key(&info[..len], b"name"),
            b"new",
            "the sanitised value is read first"
        );
    }

    #[test]
    fn set_value_for_key_refuses_forbidden_characters_and_overflow() {
        let mut info = b"\\model\\kyle\0".to_vec();
        assert!(!set_value_for_key(&mut info, b"name", b"a;b"));
        assert!(!set_value_for_key(&mut info, b"name", b"a\\b"));
        assert!(!set_value_for_key(&mut info, b"name", b"a\"b"));
        assert_eq!(&info[..5], b"\\mode");

        // A 1023-byte info string: one more pair would reach
        // MAX_INFO_STRING, which the retail `>` test let through.
        let mut buf = vec![b'x'; MAX_INFO_STRING - 1];
        buf[0] = b'\\';
        buf[1] = b'a';
        buf[2] = b'\\';
        buf[3] = b'b';
        buf.push(0);
        let before = buf.clone();
        assert!(
            !set_value_for_key(&mut buf, b"k", b"v"),
            "adding one pair must not reach MAX_INFO_STRING"
        );
        assert_eq!(buf, before, "a refused write leaves the buffer unchanged");
    }

    #[test]
    fn validation_rejects_the_openjk_shapes() {
        let base = valid_userinfo();

        // Missing required key.
        let mut missing = base.clone();
        missing = String::from_utf8(missing)
            .unwrap()
            .replace("\\rate\\25000", "")
            .into_bytes();
        assert_eq!(
            validate_userinfo(&missing),
            Err("rate field not found".to_string())
        );

        // Duplicate key, including another case (the O8 bypass).
        let mut duplicate = base.clone();
        duplicate.extend_from_slice(b"\\Forcepowers\\7-1-");
        assert_eq!(
            validate_userinfo(&duplicate),
            Err("Too many forcepowers fields (2/1)".to_string())
        );

        // q3fill keys are not allowed at all.
        let mut guid = base.clone();
        guid.extend_from_slice(b"\\cl_guid\\abc");
        assert_eq!(
            validate_userinfo(&guid),
            Err("Too many cl_guid fields (1/0)".to_string())
        );

        // Two `ip` keys.
        let mut ip = base.clone();
        ip.extend_from_slice(b"\\ip\\1.2.3.4");
        assert_eq!(
            validate_userinfo(&ip),
            Err("Too many ip fields (2/1)".to_string())
        );

        // Leading slash, trailing slash, odd slash count.
        assert_eq!(
            validate_userinfo(b"name\\Padawan"),
            Err("Missing leading slash".to_string())
        );
        assert_eq!(
            validate_userinfo(b"\\name\\Padawan\\"),
            Err("Trailing slash".to_string())
        );
        assert_eq!(
            validate_userinfo(b"\\name\\Padawan\\model"),
            Err("Bad number of slashes".to_string())
        );

        // Control characters and quote.
        let mut control = base.clone();
        control.push(b'\n');
        assert_eq!(
            validate_userinfo(&control),
            Err("Invalid characters found".to_string())
        );

        // Empty.
        assert_eq!(
            validate_userinfo(b""),
            Err("Userinfo too short".to_string())
        );
    }
}
