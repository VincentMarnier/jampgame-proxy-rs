//! Stateless challenges (2026 `ae62510`, after `6c8ce8c`).
//!
//! The retail engine stores every `getchallenge` in `svs.challenges` (1024
//! entries) and overwrites the oldest when full; `SV_DirectConnect` only
//! accepts a challenge still in the table. A spoofed flood of more than 1024
//! addresses during a player's round trip evicts that player's challenge, and
//! the client only gets `No or bad challenge for address.`.
//!
//! The proxy answers `getchallenge` itself and stores nothing: the challenge is
//! a SipHash-2-4 of the source address and a 16.384 s timestamp, so only the
//! address that received it can send it back. A `connect` with a valid
//! challenge gets its entry written into `svs.challenges` just before the
//! original `SV_DirectConnect` looks it up, so the table only holds addresses
//! that answered.
//!
//! A stateless challenge does not know when it was sent, so `sv_minPing` and
//! `sv_maxPing` can no longer be checked on connect. As OpenJK, TaystJK and
//! Quake3e, the entry gets `pingTime = svs.time - sv_minPing`, a ping that
//! passes both original tests (an `sv_minPing` above `sv_maxPing` already
//! refused everyone).
//!
//! The 2026 proxy keeps its key in a hidden cvar because the engine unloads it
//! on every map change. This module is pinned (D-007), so a process-lifetime
//! static is enough and a challenge sent just before a map change stays valid.

use core::ffi::{CStr, c_char};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::LazyLock;

use crate::engine;
use crate::hooks::engine_sv::{Netadr, svs_time};
use crate::sdk::{MAX_CHALLENGES, OFFSET_SVS_CHALLENGES, SIZEOF_CHALLENGE_T};
use crate::syscall;
use crate::userinfo;
use crate::utils;

/// `netadr_t.type` for an IPv4 address.
const NA_IP: i32 = 4;
/// `netsrc_t::NS_SERVER` (`qcommon.h:119-120`).
const NS_SERVER: i32 = 1;
/// The timestamp moves every 2^14 ms = 16.384 s; a challenge is valid for 16
/// to 32 s.
const TIMESTAMP_SHIFT: i32 = 14;

// `challenge_t` (SDK `server.h:193-201`) field offsets, i386.
const OFFSET_CHALLENGE: usize = 20;
const OFFSET_TIME: usize = 24;
const OFFSET_PING_TIME: usize = 28;
const OFFSET_FIRST_TIME: usize = 32;
const OFFSET_CONNECTED: usize = 36;
/// `netadr_t` port within the address.
const OFFSET_ADR_PORT: usize = 18;

static KEY: LazyLock<[u64; 2]> = LazyLock::new(random_key);

fn random_key() -> [u64; 2] {
    // `RandomState` is seeded from the OS (`getrandom`); two different hashes
    // of two counters give two independent-looking words. No dependency.
    let state = RandomState::new();
    let mut key = [0u64; 2];
    for (i, word) in key.iter_mut().enumerate() {
        let mut hasher = state.build_hasher();
        hasher.write_u64(i as u64);
        *word = hasher.finish();
    }
    key
}

fn sip_round(v0: &mut u64, v1: &mut u64, v2: &mut u64, v3: &mut u64) {
    *v0 = v0.wrapping_add(*v1);
    *v1 = v1.rotate_left(13) ^ *v0;
    *v0 = v0.rotate_left(32);
    *v2 = v2.wrapping_add(*v3);
    *v3 = v3.rotate_left(16) ^ *v2;
    *v0 = v0.wrapping_add(*v3);
    *v3 = v3.rotate_left(21) ^ *v0;
    *v2 = v2.wrapping_add(*v1);
    *v1 = v1.rotate_left(17) ^ *v2;
    *v2 = v2.rotate_left(32);
}

/// SipHash-2-4 (Aumasson and Bernstein) of a 16-byte message given as two
/// little-endian words. Port of the 2026 C++ proxy's `SipHash24`.
fn siphash24(key: &[u64; 2], m0: u64, m1: u64) -> u64 {
    let mut v0 = 0x736f_6d65_7073_6575 ^ key[0];
    let mut v1 = 0x646f_7261_6e64_6f6d ^ key[1];
    let mut v2 = 0x6c79_6765_6e65_7261 ^ key[0];
    let mut v3 = 0x7465_6462_7974_6573 ^ key[1];

    // The last word only holds the length of the message, in its top byte.
    for m in [m0, m1, 16u64 << 56] {
        v3 ^= m;
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
        v0 ^= m;
    }

    v2 ^= 0xff;
    for _ in 0..4 {
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    }

    v0 ^ v1 ^ v2 ^ v3
}

fn create_with_key(key: &[u64; 2], timestamp: i32, from: &Netadr) -> i32 {
    let ip = u32::from_ne_bytes(from.ip) as u64;
    let m0 = ip | ((from.port as u64) << 32);
    let m1 = timestamp as u32 as u64;
    let hash = siphash24(key, m0, m1);

    // 31 bits of the hash, and the parity of the timestamp in the top bit
    // (used by `verify`). The client reads it with `atoi`.
    (((hash as u32) & 0x7fff_ffff) | (((timestamp as u32) & 1) << 31)) as i32
}

fn verify_with_key(key: &[u64; 2], challenge: i32, timestamp: i32, from: &Netadr) -> bool {
    // The top bit is the parity of the timestamp the challenge was made with:
    // the current one, or the previous one when the timestamp changed between
    // the getchallenge and the connect.
    let parity = ((challenge as u32) >> 31) as i32;
    let previous = (timestamp ^ parity) & 1;

    challenge == create_with_key(key, timestamp - previous, from)
}

fn same_base(a: &Netadr, b: &Netadr) -> bool {
    if a.kind != b.kind {
        return false;
    }
    if a.kind == NA_IP {
        a.ip == b.ip
    } else {
        a.ipx == b.ipx
    }
}

/// `NET_OutOfBandPrint(NS_SERVER, from, "challengeResponse %i", challenge)`
/// (`0x807b744`, cdecl variadic).
///
/// # Safety
///
/// `from` is the address that asked for the challenge; the engine memory layer
/// is initialised (only reachable from the connectionless path).
unsafe fn out_of_band_print(from: &Netadr, challenge: i32) {
    // SAFETY: fixed engine address with the cdecl argument layout
    // `(netsrc_t, netadr_t, const char*, int)`, which is what a variadic
    // call with one int argument uses.
    let f: unsafe extern "C" fn(i32, Netadr, *const c_char, i32) =
        unsafe { core::mem::transmute(engine::functions::NET_OUT_OF_BAND_PRINT) };
    unsafe {
        f(
            NS_SERVER,
            *from,
            c"challengeResponse %i".as_ptr(),
            challenge,
        )
    };
}

/// Answer a `getchallenge` with a stateless challenge; the original engine
/// handler is not called.
pub fn answer(from: &Netadr) {
    // SAFETY: engine memory layer initialised (connectionless path).
    let timestamp = unsafe { svs_time() } >> TIMESTAMP_SHIFT;
    let challenge = create_with_key(&KEY, timestamp, from);
    // SAFETY: see `out_of_band_print`.
    unsafe { out_of_band_print(from, challenge) };
}

/// Validate the challenge of a `connect` and insert its entry into
/// `svs.challenges` before the original `SV_DirectConnect` looks it up.
///
/// A `connect` sent again (lost packet, full server) reuses its existing entry
/// even once the challenge expired, as upstream. An invalid challenge inserts
/// nothing: the original then answers `No or bad challenge for address.`.
pub fn admit(from: &Netadr) {
    // SAFETY: engine memory layer initialised (connectionless path).
    let now = unsafe { svs_time() };

    let userinfo_ptr = unsafe { engine::cmd_argv(1) };
    let info = if userinfo_ptr.is_null() {
        &b""[..]
    } else {
        // SAFETY: Cmd_Argv returns a NUL-terminated string in this context.
        unsafe { CStr::from_ptr(userinfo_ptr) }.to_bytes()
    };
    let challenge = utils::atoi(userinfo::value_for_key(info, b"challenge"));

    let base = engine::vars::SVS + OFFSET_SVS_CHALLENGES;
    let mut existing = None;
    let mut oldest = 0usize;
    let mut oldest_time = i32::MAX;

    for i in 0..MAX_CHALLENGES {
        let entry = base + i * SIZEOF_CHALLENGE_T;
        // SAFETY: `entry` is inside the engine's `svs.challenges` array.
        let stored = unsafe { core::ptr::read_unaligned((entry + OFFSET_CHALLENGE) as *const i32) };
        let port = unsafe { core::ptr::read_unaligned((entry + OFFSET_ADR_PORT) as *const u16) };
        let adr = unsafe { core::ptr::read_unaligned(entry as *const Netadr) };

        if stored == challenge && from.port == port && same_base(from, &adr) {
            existing = Some(entry);
            break;
        }

        let time = unsafe { core::ptr::read_unaligned((entry + OFFSET_TIME) as *const i32) };
        if time < oldest_time {
            oldest_time = time;
            oldest = entry;
        }
    }

    let entry = match existing {
        Some(entry) => entry,
        None => {
            if !verify_with_key(&KEY, challenge, now >> TIMESTAMP_SHIFT, from) {
                return;
            }

            // A valid challenge takes the oldest entry, as the original
            // `SV_GetChallenge` does when the table is full.
            // SAFETY: `oldest` is inside the array; 20-byte netadr_t then the
            // challenge_t fields.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    from as *const Netadr as *const u8,
                    oldest as *mut u8,
                    core::mem::size_of::<Netadr>(),
                );
                core::ptr::write_unaligned((oldest + OFFSET_CHALLENGE) as *mut i32, challenge);
                core::ptr::write_unaligned((oldest + OFFSET_TIME) as *mut i32, now);
                core::ptr::write_unaligned((oldest + OFFSET_FIRST_TIME) as *mut i32, now);
                core::ptr::write_unaligned((oldest + OFFSET_CONNECTED) as *mut i32, 0);
            }

            oldest
        }
    };

    // The original refuses a ping below sv_minPing or above sv_maxPing; a ping
    // of sv_minPing passes both, so neither is checked. The cvar is read as a
    // string and rounded up, as upstream (`ceil(atof())`): `sv_minPing` is a
    // float cvar.
    let mut min_ping_text = [0u8; 256];
    // SAFETY: engine syscall registered; the buffer is writable, the name is a
    // literal.
    unsafe { syscall::cvar_variable_string_buffer(c"sv_minPing", &mut min_ping_text) };
    let len = min_ping_text
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(min_ping_text.len());
    let min_ping = core::str::from_utf8(&min_ping_text[..len])
        .unwrap_or("")
        .trim()
        .parse::<f32>()
        .unwrap_or(0.0)
        .ceil() as i32;
    let min_ping = min_ping.max(0);
    // SAFETY: `entry` is the challenge_t selected above.
    unsafe {
        core::ptr::write_unaligned((entry + OFFSET_PING_TIME) as *mut i32, now - min_ping);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8, port: u16) -> Netadr {
        Netadr {
            kind: NA_IP,
            ip: [a, b, c, d],
            ipx: [0; 10],
            port,
        }
    }

    #[test]
    fn siphash24_matches_the_reference_vector() {
        // Key 00..0f, message 00..0f, as the 2026 static_assert pins it.
        let key = [0x0706_0504_0302_0100, 0x0f0e_0d0c_0b0a_0908];
        assert_eq!(
            siphash24(&key, 0x0706_0504_0302_0100, 0x0f0e_0d0c_0b0a_0908),
            0x3f2a_cc7f_57c2_9bdb
        );
    }

    #[test]
    fn a_challenge_is_valid_for_the_current_and_previous_timestamp() {
        let key = [1, 2];
        let from = ip(1, 2, 3, 4, 0x1234);
        let timestamp = 100; // even
        let challenge = create_with_key(&key, timestamp, &from);

        assert!(verify_with_key(&key, challenge, 100, &from));
        assert!(
            verify_with_key(&key, challenge, 101, &from),
            "the timestamp may roll between getchallenge and connect"
        );
        assert!(!verify_with_key(&key, challenge, 102, &from));

        let odd = create_with_key(&key, 101, &from);
        assert!(verify_with_key(&key, odd, 101, &from));
        assert!(
            !verify_with_key(&key, odd, 100, &from),
            "a challenge from the future timestamp is not accepted in the past"
        );
    }

    #[test]
    fn a_challenge_is_bound_to_the_address_and_port() {
        let key = [1, 2];
        let from = ip(1, 2, 3, 4, 0x1234);
        let challenge = create_with_key(&key, 100, &from);

        assert!(!verify_with_key(
            &key,
            challenge,
            100,
            &ip(1, 2, 3, 5, 0x1234)
        ));
        assert!(!verify_with_key(
            &key,
            challenge,
            100,
            &ip(1, 2, 3, 4, 0x1235)
        ));
        assert!(!verify_with_key(&[3, 4], challenge, 100, &from));
    }

    #[test]
    fn base_address_comparison_ignores_the_port() {
        let a = ip(1, 2, 3, 4, 1);
        let mut b = ip(1, 2, 3, 4, 2);
        assert!(same_base(&a, &b));
        b.ip[3] = 5;
        assert!(!same_base(&a, &b));
    }
}
