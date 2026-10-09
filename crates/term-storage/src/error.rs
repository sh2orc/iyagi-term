//! Storage error vocabulary (spec `01-contracts.md` §7).
//!
//! Two variants intentionally mirror spec RPC error codes so the daemon can
//! surface them without translation: [`StorageError::RequestConflict`] is
//! `REQUEST_CONFLICT` and [`StorageError::InvalidState`] is `INVALID_STATE`.
//! Display strings never include argv/env values (nothing sensitive is ever
//! persisted, so nothing sensitive can leak through errors).

use term_contracts::state::WorkloadState;

pub type StorageResult<T> = Result<T, StorageError>;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Underlying rusqlite/SQLite failure (constraint violations, IO, ...).
    #[error("sqlite failure: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A migration could not be applied. The version stays unrecorded, so the
    /// next open retries (or fails loudly on torn DDL) instead of silently
    /// running a half schema.
    #[error("migration {version} failed: {message}")]
    MigrationFailed { version: i64, message: String },

    /// API-level input validation (mirrors `INVALID_ARGUMENT` semantics).
    #[error("invalid storage input: {0}")]
    InvalidArgument(&'static str),

    /// Same request id reused with a different fingerprint — spec §6:
    /// `REQUEST_CONFLICT`. The stored request is left untouched.
    #[error(
        "REQUEST_CONFLICT: request {request_id} is already recorded with a different fingerprint"
    )]
    RequestConflict { request_id: String },

    /// Illegal workload state transition — spec §5, `INVALID_STATE` semantics.
    /// The stored state and lifecycle history are unchanged.
    #[error("INVALID_STATE: workload {workload_id} cannot move {from:?} -> {to:?}")]
    InvalidState {
        workload_id: String,
        from: WorkloadState,
        to: WorkloadState,
    },

    #[error("workload {workload_id} not found")]
    WorkloadNotFound { workload_id: String },

    #[error("session {session_id} not found")]
    SessionNotFound { session_id: String },

    /// A persisted sequence number would exceed the SQLite signed-INTEGER
    /// bound (spec §1: explicit error, never a wrap).
    #[error(
        "last_seq {attempted} for session {session_id} exceeds the SQLite signed-INTEGER bound"
    )]
    SeqOverflow { session_id: String, attempted: u64 },

    /// Same bound check for journal byte counts.
    #[error("journal_bytes {attempted} for session {session_id} exceeds the SQLite signed-INTEGER bound")]
    JournalBytesOverflow { session_id: String, attempted: u64 },

    /// Sequences are monotonic; a backwards update indicates a journal bug.
    #[error(
        "last_seq moved backwards for session {session_id}: stored {stored}, attempted {attempted}"
    )]
    SeqRegression {
        session_id: String,
        stored: u64,
        attempted: u64,
    },

    /// A stored TEXT value does not map to a known contract enum. SQL CHECK
    /// constraints make this near-impossible; it still must not panic.
    #[error("stored value is not a known contract shape: {0}")]
    Corrupt(String),

    /// The serialized writer thread is gone (shut down or panicked).
    #[error("storage writer thread is not running")]
    WriterClosed,
}

impl StorageError {
    pub(crate) fn corrupt(detail: impl Into<String>) -> Self {
        StorageError::Corrupt(detail.into())
    }
}
