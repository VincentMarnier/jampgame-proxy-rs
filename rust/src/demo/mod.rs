//! Server-side demo recording: pure-Rust file writer, gamestate encoder,
//! recorder and publication.
//!
//! `file`/`gamestate`/`msg` (Phase 1) have **no engine dependency** and are
//! exercised by `cargo test` on the host. `recorder` (Phases 2–3) owns the
//! engine-facing frame ring and the per-instance POVs; `publish` (Phase 4)
//! owns the generation list, retention/maintenance thread, `/demos` and
//! announcements. The HTTP surface itself lives in `crate::http` and reads
//! only the published configuration/open-set.
//!
//! Wire-format evidence: `docs/demo-format.md` §10–§13; decisions:
//! `docs/decisions/0027-server-demos.md`.
// The Phase 1/2 modules keep a few imports and helpers for their sibling
// modules and tests; the allow is inherited by the whole demo tree.
#![allow(dead_code, unused_imports)]

pub mod file;
pub mod gamestate;
pub mod msg;
pub mod publish;
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
