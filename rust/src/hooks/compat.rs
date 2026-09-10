//! Proxy-local helpers for the download-validation family — ports of
//! `Proxy_cmd.cpp:17-112` (`Cmd_TokenizeString2`), `FS_CheckDirTraversal`
//! and `FS_FilenameCompare` (`Proxy_sv_client.cpp:426-432,520-547`).
//!
//! `Cmd_TokenizeString2` is deliberately a *proxy-local* function here, not a
//! detour on the engine's `Cmd_TokenizeString` (D-002: the quoted/comment
//! handling the original proxy installed as a hook is only needed by the
//! download-validation stub, so the hook itself is dropped). It writes into
//! the same engine `cmd_argc`/`cmd_argv`/`cmd_tokenized` globals the engine's
//! tokenizer uses, exactly like the original (harmless there: the referenced-
//! paks check runs inside a client-message handler, not a command context).

use core::ffi::{c_char, c_int};

use crate::engine;
use crate::sdk::MAX_STRING_TOKENS;

/// `FS_CheckDirTraversal` (`Proxy_sv_client.cpp:426-432`): reject `../` or
/// `..\` escapes.
pub fn check_dir_traversal(checkdir: &[u8]) -> bool {
    checkdir.windows(3).any(|w| w == b"../" || w == b"..\\")
}

/// `FS_FilenameCompare` (`Proxy_sv_client.cpp:520-547`): case-folded, `\`/`:`
/// normalised to `/`, byte compare; returns true when the strings *differ*
/// (the original returns `qtrue` = different).
pub fn filename_compare(s1: &[u8], s2: &[u8]) -> bool {
    let mut i = 0usize;
    loop {
        let mut c1 = s1.get(i).copied().unwrap_or(0);
        let mut c2 = s2.get(i).copied().unwrap_or(0);
        if c1.is_ascii_lowercase() {
            c1 -= b'a' - b'A';
        }
        if c2.is_ascii_lowercase() {
            c2 -= b'a' - b'A';
        }
        if c1 == b'\\' || c1 == b':' {
            c1 = b'/';
        }
        if c2 == b'\\' || c2 == b':' {
            c2 = b'/';
        }
        if c1 != c2 {
            return true; // strings not equal
        }
        if c1 == 0 {
            return false; // strings are equal
        }
        i += 1;
    }
}

/// `Cmd_TokenizeString2(text_in, ignoreQuotes)` (`Proxy_cmd.cpp:17-112`),
/// writing into the engine's `cmd_argc`/`cmd_argv`/`cmd_tokenized` globals.
///
/// The original copies `text_in` into a `static char cmd_cmd[BIG_INFO_STRING]`
/// first and never reads it again; we skip the copy but honour the same
/// `BIG_INFO_STRING` input bound so `cmd_tokenized` cannot overflow.
///
/// # Safety
///
/// `text_in` must be a NUL-terminated string; the engine cmd globals are
/// valid (memory layer initialised).
pub unsafe fn cmd_tokenize_string2(text_in: *const c_char, ignore_quotes: bool) {
    // SAFETY: engine RW var from the fixed table.
    unsafe { engine::set_cmd_argc(0) };
    if text_in.is_null() {
        return;
    }
    // SAFETY: caller guarantees NUL-termination.
    let text = unsafe { core::ffi::CStr::from_ptr(text_in) };
    let input = text.to_bytes();
    let cap = input.len().min(crate::sdk::BIG_INFO_STRING - 1);

    let mut text_pos = 0usize;
    let mut out_pos = 0usize;
    let mut argc: c_int = 0;

    loop {
        if argc == MAX_STRING_TOKENS as c_int {
            return; // malicious input guard
        }

        // Skip whitespace / comments to the next token start.
        loop {
            while text_pos < cap && input[text_pos] <= b' ' {
                text_pos += 1;
            }
            if text_pos >= cap {
                return; // all tokens parsed
            }
            if input[text_pos] == b'/' && text_pos + 1 < cap && input[text_pos + 1] == b'/' {
                return; // // comment
            }
            if input[text_pos] == b'/' && text_pos + 1 < cap && input[text_pos + 1] == b'*' {
                text_pos += 2;
                while text_pos < cap
                    && !(input[text_pos] == b'*'
                        && text_pos + 1 < cap
                        && input[text_pos + 1] == b'/')
                {
                    text_pos += 1;
                }
                if text_pos >= cap {
                    return; // unterminated block comment
                }
                text_pos += 2;
            } else {
                break; // ready to parse a token
            }
        }

        // Quoted string.
        if !ignore_quotes && input[text_pos] == b'"' {
            let token_start = out_pos;
            text_pos += 1;
            while text_pos < cap && input[text_pos] != b'"' {
                write_token(&mut out_pos, input[text_pos]);
                text_pos += 1;
            }
            write_nul(&mut out_pos);
            set_argv(argc as usize, token_start);
            argc += 1;
            if text_pos >= cap {
                return;
            }
            text_pos += 1;
            continue;
        }

        // Regular token.
        let token_start = out_pos;
        set_argv(argc as usize, token_start);
        argc += 1;

        while text_pos < cap && input[text_pos] > b' ' {
            if !ignore_quotes && input[text_pos] == b'"' {
                break;
            }
            if input[text_pos] == b'/' && text_pos + 1 < cap && input[text_pos + 1] == b'/' {
                break;
            }
            if input[text_pos] == b'/' && text_pos + 1 < cap && input[text_pos + 1] == b'*' {
                break;
            }
            write_token(&mut out_pos, input[text_pos]);
            text_pos += 1;
        }
        write_nul(&mut out_pos);

        if text_pos >= cap {
            return;
        }
    }
}

/// Write one output byte into `cmd_tokenized` at `out_pos`.
fn write_token(out_pos: &mut usize, b: u8) {
    // SAFETY: out_pos is bounded by the BIG_INFO_STRING input cap; the engine
    // cmd_tokenized buffer is 8192+MAX_STRING_TOKENS bytes.
    unsafe {
        *(crate::engine::CMD_TOKENIZED_BASE as *mut u8).add(*out_pos) = b;
    }
    *out_pos += 1;
}

fn write_nul(out_pos: &mut usize) {
    // SAFETY: same bound as write_token.
    unsafe {
        *(crate::engine::CMD_TOKENIZED_BASE as *mut u8).add(*out_pos) = 0;
    }
    *out_pos += 1;
}

/// Record `cmd_argv[argc] = &cmd_tokenized[token_start]` and bump the engine's
/// `cmd_argc`.
fn set_argv(argc: usize, token_start: usize) {
    // SAFETY: argc < MAX_STRING_TOKENS (checked); addresses from the fixed
    // engine tables.
    unsafe {
        engine::set_cmd_argv(
            argc,
            (crate::engine::CMD_TOKENIZED_BASE + token_start) as *const c_char,
        );
        engine::set_cmd_argc(engine::cmd_argc() + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_traversal_detection() {
        assert!(check_dir_traversal(b"../foo.pk3"));
        assert!(check_dir_traversal(b"..\\foo.pk3"));
        assert!(!check_dir_traversal(b"maps/foo.pk3"));
        assert!(!check_dir_traversal(b"base"));
    }

    #[test]
    fn filename_compare_case_and_sep_insensitive() {
        // Returns true when strings *differ* (the original's qtrue).
        assert!(!filename_compare(b"foo.pk3", b"FOO.PK3"));
        assert!(!filename_compare(b"a/b.pk3", b"a\\b.pk3"));
        assert!(!filename_compare(b"a/b.pk3", b"a:b.pk3"));
        assert!(filename_compare(b"foo.pk3", b"bar.pk3"));
        assert!(!filename_compare(b"", b""));
    }
}
