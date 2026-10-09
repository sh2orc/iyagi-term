//! # term-storage
//!
//! SQLite metadata only — PTY bytes and telemetry never become rows (spec
//! `01-contracts.md` §7). Every migration applies a docs file verbatim via
//! `include_str!`, so the docs tree stays the single schema source: 0001 is
//! `docs/implementation/schema.sql`, 0002 is
//! `docs/implementation/schema-0002-agent-sessions.sql` (`agent_sessions`,
//! spec `02-runner.md` §8).
//!
//! Connection policy: every connection sets `foreign_keys=ON` and
//! `busy_timeout=5000`; the database runs in WAL; the dedicated writer
//! connection runs `synchronous=FULL` (conservative standing choice for
//! pre-gate-release durability). All writes are serialized through one writer
//! thread fed by an mpsc channel; reads run on a small pool of read-only
//! connections. The API is synchronous — callers wrap it in `spawn_blocking`.
//!
//! Sensitive launch inputs (argv/env) are never persisted; only the request
//! fingerprint, policy, and lifecycle metadata are stored (spec §6).

mod enums;
pub mod error;
mod ids;
mod migration;
pub mod mission;
mod ops;
mod queries;
mod storage;
pub mod time;
mod types;
mod writer;

pub use error::{StorageError, StorageResult};
pub use ids::{AttemptId, TaskId};
pub use storage::Storage;
pub use types::{
    AgentSessionUpsert, LaunchIntent, LaunchIntentOutcome, ProcessOwnershipRow, QueuedWorkload,
    ReconciledWorkload, RequestMethod, RequestOutcome, RequestResolution, SessionRecord,
};
