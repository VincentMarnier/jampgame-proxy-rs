//! Out-of-band rate limiting (10-25 F6 / O2).
//!
//! Port of the 2026 C++ proxy's `Engine/RateLimit.cpp`, itself a modified
//! OpenJK `SVC_RateLimit` / `SVC_BucketForAddress` (GPL):
//!
//! - one leaky bucket `(10, 1000)` per IPv4 address, shared by `getstatus`,
//!   `getinfo`, `getchallenge` and `connect`;
//! - a global outbound `(10, 100)` bucket for `getstatus`/`getinfo`;
//! - a global `(10, 1000)` bucket for `rcon` with a bad password; a valid
//!   password is never limited, so an admin keeps rcon during a spoofed flood;
//! - times are unsigned, so `Sys_Milliseconds` wrapping after ~24.8 days does
//!   not break the arithmetic (the wrap looks like one period elapsed);
//! - at most 65536 address entries; a new address beyond the cap is dropped.
//!
//! Only IPv4 (`NA_IP`) sources are limited: loopback, bots and bad addresses
//! are never limited. Dropped requests are logged only with
//! `proxy_sv_debugLog`, the Rust equivalent of upstream's `com_developer`.
//!
//! The 2026 proxy stores its buckets in the game module, so they reset on each
//! map change. The Rust module is pinned (D-007), so they persist; that only
//! makes the limit stricter across a map change, never looser.

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex};

use crate::diagnostics;
use crate::hooks::engine_sv::Netadr;
use crate::syscall;

/// `netadr_t.type` for an IPv4 address (`NA_IP`, `qcommon.h:120-128`).
const NA_IP: i32 = 4;

const MAX_BUCKETS: usize = 65536;
const ADDRESS_BURST: i32 = 10;
const ADDRESS_PERIOD_MS: u32 = 1000;
const STATUS_BURST: i32 = 10;
const STATUS_PERIOD_MS: u32 = 100;
const RCON_BURST: i32 = 10;
const RCON_PERIOD_MS: u32 = 1000;

#[derive(Clone, Copy, Default)]
struct LeakyBucket {
    last_time: u32,
    burst: i32,
}

#[derive(Default)]
struct Table {
    // `BTreeMap`, not `HashMap`: the prebuilt `std` is compiled with SSE2 and
    // its `HashMap` (hashbrown) instantiates an aligned `_mm_load_si128` group
    // load even though this crate disables SSE. The engine enters the game
    // module with a 4-byte-aligned stack (D-001), so the first connectionless
    // packet (getchallenge/getstatus/rcon) crashed in `address_allowed`
    // (D-018). The tree uses no aligned SIMD; a few comparisons per packet at
    // the 65536-address cap are cheap.
    buckets: BTreeMap<u32, LeakyBucket>,
    last_gc: u32,
    /// `getstatus`/`getinfo`, every address.
    outbound: LeakyBucket,
    /// `rcon` with a bad password, every address.
    bad_password: LeakyBucket,
}

static TABLE: LazyLock<Mutex<Table>> = LazyLock::new(|| Mutex::new(Table::default()));

/// OpenJK's `SVC_RateLimit`, with unsigned time. Returns `true` when the
/// request must be dropped.
fn limit(bucket: &mut LeakyBucket, burst: i32, period: u32, now: u32) -> bool {
    let interval = now.wrapping_sub(bucket.last_time);
    let expired = interval / period;

    if expired > bucket.burst as u32 {
        bucket.burst = 0;
        bucket.last_time = now;
    } else {
        bucket.burst -= expired as i32;
        bucket.last_time = now.wrapping_sub(interval % period);
    }

    if bucket.burst < burst {
        bucket.burst += 1;
        false
    } else {
        true
    }
}

/// Run `f` with the shared table.
fn with_table<T>(f: impl FnOnce(&mut Table) -> T) -> T {
    let mut table = TABLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut table)
}

/// One per-address bucket, garbage-collected at most once per second.
/// Returns `false` when the table is full (the caller drops the request).
fn address_allowed_at(table: &mut Table, from: &Netadr, now: u32) -> bool {
    if from.kind != NA_IP {
        return true;
    }

    if now.wrapping_sub(table.last_gc) > 1000 {
        table.last_gc = now;
        // An empty bucket holds nothing: it lives `burst * period` after its
        // last update, 0..10 s for a spoofed address that sent one packet.
        table.buckets.retain(|_, bucket| {
            now.wrapping_sub(bucket.last_time)
                <= (bucket.burst.max(0) as u32).saturating_mul(ADDRESS_PERIOD_MS)
        });
    }

    let key = u32::from_be_bytes(from.ip);
    if !table.buckets.contains_key(&key) {
        if table.buckets.len() >= MAX_BUCKETS {
            return false;
        }
        table.buckets.insert(
            key,
            LeakyBucket {
                last_time: now,
                burst: 0,
            },
        );
    }

    let bucket = table
        .buckets
        .get_mut(&key)
        .expect("the bucket was inserted above");
    !limit(bucket, ADDRESS_BURST, ADDRESS_PERIOD_MS, now)
}

/// Per-address limit for `getchallenge` and `connect` (Quake3e). Returns
/// `false` to drop the request.
pub fn address_allowed(from: &Netadr, command: &str) -> bool {
    let now = syscall::milliseconds();
    let allowed = with_table(|table| address_allowed_at(table, from, now));

    if !allowed {
        log_drop(command, from);
    }

    allowed
}

/// `getstatus`/`getinfo`: per address, then the global outbound bucket
/// `(10, 100)`, as ioq3. Returns `false` to drop the request.
pub fn status_allowed(from: &Netadr, command: &str) -> bool {
    if !address_allowed(from, command) {
        return false;
    }

    let now = syscall::milliseconds();
    let dropped =
        with_table(|table| limit(&mut table.outbound, STATUS_BURST, STATUS_PERIOD_MS, now));

    if dropped {
        log_drop(command, from);
    }

    !dropped
}

/// `rcon`: a valid password is never limited. A bad password shares one global
/// bucket `(10, 1000)`, about one guess per second whatever the source
/// addresses; a dropped request gets no reply, which also caps the
/// `Bad rconpassword.` packets sent to spoofed addresses. Returns `false` to
/// drop the request.
pub fn rcon_allowed(valid_password: bool) -> bool {
    if valid_password {
        return true;
    }

    let now = syscall::milliseconds();
    let dropped =
        with_table(|table| limit(&mut table.bad_password, RCON_BURST, RCON_PERIOD_MS, now));

    if dropped {
        // Dropped without a reply.
        if diagnostics::suspicious_enabled() {
            diagnostics::suspicious("ratelimit", "command=\"rcon\" valid=false dropped=true");
        }
    }

    !dropped
}

fn log_drop(command: &str, from: &Netadr) {
    if diagnostics::suspicious_enabled() {
        diagnostics::suspicious(
            "ratelimit",
            format!(
                "command=\"{}\" from=\"{}\" dropped=true",
                command,
                crate::hooks::engine_sv::netadr_string(from)
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> Netadr {
        Netadr {
            kind: NA_IP,
            ip: [a, b, c, d],
            ipx: [0; 10],
            port: 0,
        }
    }

    #[test]
    fn burst_is_allowed_then_one_per_period() {
        let mut bucket = LeakyBucket::default();
        let now = 1000;

        for _ in 0..10 {
            assert!(!limit(&mut bucket, 10, 1000, now));
        }
        assert!(
            limit(&mut bucket, 10, 1000, now),
            "11th in the burst is dropped"
        );

        // One period later: one token is back.
        assert!(!limit(&mut bucket, 10, 1000, now + 1000));
        assert!(limit(&mut bucket, 10, 1000, now + 1000));
    }

    #[test]
    fn time_wrap_looks_like_a_short_interval() {
        // last_time just before the u32 wrap, now just after: the interval is
        // 1000 ms, so exactly one token is refilled (not a full reset).
        let mut bucket = LeakyBucket {
            last_time: u32::MAX - 499,
            burst: 10,
        };
        assert!(!limit(&mut bucket, 10, 1000, 500));
        assert_eq!(bucket.burst, 10);
        assert_eq!(bucket.last_time, 500);
    }

    #[test]
    fn address_bucket_is_shared_and_keyed_by_ip() {
        let mut table = Table::default();
        let now: u32 = 10_000;
        let victim = ip(1, 2, 3, 4);
        let other = ip(5, 6, 7, 8);

        for _ in 0..10 {
            assert!(address_allowed_at(&mut table, &victim, now));
        }
        assert!(
            !address_allowed_at(&mut table, &victim, now),
            "the 11th request from the same address is dropped"
        );
        assert!(
            address_allowed_at(&mut table, &other, now),
            "another address has its own bucket"
        );

        // Loopback, bots and bad addresses are never limited.
        let mut loopback = victim;
        loopback.kind = 2;
        for _ in 0..100 {
            assert!(address_allowed_at(&mut table, &loopback, now));
        }
    }

    #[test]
    fn full_table_drops_a_new_address() {
        let mut table = Table::default();
        let now: u32 = 10_000;
        table.last_gc = now;
        for i in 0..MAX_BUCKETS {
            table.buckets.insert(i as u32, LeakyBucket::default());
        }
        assert!(!address_allowed_at(&mut table, &ip(9, 9, 9, 9), now));
        // An address already in the table keeps its bucket.
        table.buckets.insert(
            0,
            LeakyBucket {
                last_time: now,
                burst: 0,
            },
        );
        assert!(address_allowed_at(&mut table, &ip(0, 0, 0, 0), now));
    }

    #[test]
    fn garbage_collection_removes_empty_buckets() {
        let mut table = Table::default();
        let now: u32 = 10_000;
        table.last_gc = 0;
        table.buckets.insert(
            0x0102_0304,
            LeakyBucket {
                last_time: now.wrapping_sub(11_000),
                burst: 10,
            },
        );
        table.buckets.insert(
            0x0506_0708,
            LeakyBucket {
                last_time: now,
                burst: 10,
            },
        );

        assert!(address_allowed_at(&mut table, &ip(42, 42, 42, 42), now));
        assert!(
            !table.buckets.contains_key(&0x0102_0304),
            "an empty old bucket is removed"
        );
        assert!(
            table.buckets.contains_key(&0x0506_0708),
            "a fresh bucket is kept"
        );
    }
}
