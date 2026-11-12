//! Server-side demo recording: pure-Rust file writer and gamestate encoder.
//!
//! Phase 1 of the `proxy_sv_enableDemoRecord` plan. This module has **no
//! engine dependency**: `cargo test` exercises it on the host. The recorder
//! core (frame ring, playerstate/entity snapshots, instance selection) is
//! Phase 2 and lives outside this module.
//!
//! Wire-format evidence: `docs/demo-format.md`; decisions: Phase 5 will add
//! `docs/decisions/0027-server-demos.md`.
#![allow(dead_code, unused_imports)] // consumed by the Phase 2 recorder

pub mod file;
pub mod gamestate;
pub mod msg;
pub mod recorder;

pub(crate) mod fields;

pub use file::{
    DEFAULT_BUFFER_BYTES, DEMO_EXTENSION, DemoName, DemoSink, DemoWriter, FileSink, InstanceLabel,
    MAX_MAP_SANITIZED_LEN, NameError, Retention, Timestamp, sanitize_map,
};
pub use gamestate::{
    Baseline, ConfigString, ENTITY_STATE_SIZE, GamestateError, GamestateHeader, MAX_CONFIGSTRINGS,
    MAX_GENTITIES, RmgTail, encode_gamestate,
};
pub use msg::{BIG_INFO_STRING, MsgError, MsgWriter};
