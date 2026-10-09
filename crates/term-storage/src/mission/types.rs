//! Mission storage types: the transaction unit crossing the storage API and
//! its results. These are daemon-facing (not wire) types; the daemon service
//! builds them from core reducer output (01 §2, ticket O03).

use serde::{Deserialize, Serialize};
use term_contracts::mission::types::{
    ArtifactRef, Change, Entity, EntityKind, Id, MissionEvent, MissionEventType, MutationResult,
    Timestamp,
};

/// Errors from mission storage operations. The daemon maps these onto
/// `MissionRpcError` codes; storage never fabricates user-facing messages
/// beyond diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum MissionStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("request {0} was already recorded with a different payload")]
    RequestConflict(Id),
    #[error("mission revision is {current_revision}, not {expected_revision}")]
    RevisionConflict {
        expected_revision: u64,
        current_revision: u64,
    },
    #[error("{what} {id} not found")]
    NotFound { what: &'static str, id: String },
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("corrupt mission row: {0}")]
    Corrupt(String),
    #[error("storage writer closed")]
    WriterClosed,
}

pub type MissionStoreResult<T> = Result<T, MissionStoreError>;

/// Outbox operations (schema `orch_outbox.operation` CHECK list).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxOperation {
    Start,
    Message,
    Cancel,
    Answer,
    Verify,
    WorkspacePrepare,
    WorkspaceCapture,
}

impl OutboxOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            OutboxOperation::Start => "start",
            OutboxOperation::Message => "message",
            OutboxOperation::Cancel => "cancel",
            OutboxOperation::Answer => "answer",
            OutboxOperation::Verify => "verify",
            OutboxOperation::WorkspacePrepare => "workspace_prepare",
            OutboxOperation::WorkspaceCapture => "workspace_capture",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "start" => OutboxOperation::Start,
            "message" => OutboxOperation::Message,
            "cancel" => OutboxOperation::Cancel,
            "answer" => OutboxOperation::Answer,
            "verify" => OutboxOperation::Verify,
            "workspace_prepare" => OutboxOperation::WorkspacePrepare,
            "workspace_capture" => OutboxOperation::WorkspaceCapture,
            _ => return None,
        })
    }
}

/// Durable side-effect intent recorded in the same transaction as the state
/// change (02 §7: run + outbox commit atomically).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxIntent {
    pub id: Id,
    pub mission_id: Id,
    pub run_id: Option<Id>,
    pub operation: OutboxOperation,
    /// `mission-id/task-id/attempt/operation` style key; unique per intent.
    pub dedupe_key: String,
    pub fencing_token: u64,
    pub payload: serde_json::Value,
    pub created_at: Timestamp,
}

/// An actor claims or finishes an external effect in the same transaction
/// as its Run projection. The expected state and token prevent stale actors
/// from dispatching or acknowledging somebody else's intent.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxUpdate {
    pub id: Id,
    pub expected_state: OutboxState,
    pub state: OutboxState,
    pub fencing_token: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    /// mission.create: mission must not exist yet; revision starts at 1.
    Create,
    /// Every other mutation: expected_revision CAS against the stored value.
    Mutate { expected_revision: u64 },
}

/// One atomic mission transaction: projections + event + outbox + request
/// row + revision bump, committed together or not at all (01 §2).
#[derive(Debug, Clone)]
pub struct ApplyMissionTransition {
    pub request_id: Id,
    /// RPC method name (fingerprint namespace).
    pub method: String,
    /// SHA-256 of method + normalized payload (request_id excluded).
    pub fingerprint: String,
    pub mission_id: Id,
    pub mode: ApplyMode,
    pub transaction_id: Id,
    pub event_type: MissionEventType,
    /// Full entity values to upsert. Exactly one row per (kind, id).
    pub upserts: Vec<Entity>,
    /// Content-projection deletes (never history rows).
    pub deletes: Vec<Change>,
    /// Optional artifact carrying the change list when the inline event
    /// payload would exceed the event budget (01 §2: exactly one of
    /// changes/changes_ref non-null — storage derives the inline side).
    pub changes_ref: Option<ArtifactRef>,
    pub outbox: Vec<OutboxIntent>,
    pub outbox_updates: Vec<OutboxUpdate>,
    /// Staged artifacts to adopt into this mission in the same transaction
    /// (mission.create; the creating connection must own the staging).
    pub adopt_staged_artifacts: Vec<Id>,
    pub created_at: Timestamp,
}

/// Successful apply outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedTransition {
    pub result: MutationResult,
    /// True when the stored first response was replayed (duplicate request).
    pub replayed: bool,
}

/// Materialized read snapshot of one mission (no artifact bodies).
#[derive(Debug, Clone, PartialEq)]
pub struct MissionSnapshotData {
    pub mission_id: Id,
    pub revision: u64,
    pub event_seq: u64,
    pub entities: Vec<Entity>,
}

/// A committed event row as returned to readers.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub event: MissionEvent,
}

/// Stored request dedupe row.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRequest {
    pub request_id: Id,
    pub mission_id: Option<Id>,
    pub method: String,
    pub fingerprint: String,
    pub response: MutationResult,
    pub created_at: Timestamp,
}

/// Outbox row as seen by the recovery scanner.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredOutbox {
    pub id: Id,
    pub mission_id: Id,
    pub run_id: Option<Id>,
    pub operation: OutboxOperation,
    pub dedupe_key: String,
    pub fencing_token: u64,
    pub state: OutboxState,
    pub payload: serde_json::Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxState {
    Prepared,
    Sending,
    Acknowledged,
    Failed,
    Unknown,
}

impl OutboxState {
    pub fn as_str(self) -> &'static str {
        match self {
            OutboxState::Prepared => "prepared",
            OutboxState::Sending => "sending",
            OutboxState::Acknowledged => "acknowledged",
            OutboxState::Failed => "failed",
            OutboxState::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "prepared" => OutboxState::Prepared,
            "sending" => OutboxState::Sending,
            "acknowledged" => OutboxState::Acknowledged,
            "failed" => OutboxState::Failed,
            "unknown" => OutboxState::Unknown,
            _ => return None,
        })
    }
}

/// Keyset pagination cursor for mission.list (updated_at DESC, id DESC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionListCursor {
    pub updated_at: String,
    pub id: Id,
}

pub fn entity_kind_str(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Message => "message",
        EntityKind::Decision => "decision",
        EntityKind::Workspace => "workspace",
        EntityKind::Candidate => "candidate",
        EntityKind::Verification => "verification",
        EntityKind::Finding => "finding",
        EntityKind::Knowledge => "knowledge",
        // The kinds below live in dedicated tables, not orch_entities.
        EntityKind::Mission => "mission",
        EntityKind::Task => "task",
        EntityKind::Run => "run",
        EntityKind::Exec => "exec",
    }
}
