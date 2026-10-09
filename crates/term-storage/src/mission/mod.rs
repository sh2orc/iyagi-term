//! # Mission persistence (O1)
//!
//! Same SQLite writer, `orch_*` tables (migration 0002). One transaction API
//! ([`apply_mission_transition`]) carries projections + event + outbox +
//! request dedupe + revision atomically; readers materialize snapshots and
//! event tails through the shared read pool (ticket O03).

pub mod artifacts;
pub mod ops;
pub mod queries;
pub mod types;

pub use artifacts::{ArtifactRow, UploadRow};
pub use types::{
    AppliedTransition, ApplyMissionTransition, ApplyMode, MissionListCursor, MissionSnapshotData,
    MissionStoreError, MissionStoreResult, OutboxIntent, OutboxOperation, OutboxState, StoredEvent,
    StoredOutbox, StoredRequest,
};
