//! RPC error codes and retryability (spec `01-contracts.md` §7).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Stable machine-readable codes. Messages must never echo full commands,
/// tokens, or environment values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum ErrorCode {
    #[serde(rename = "INVALID_ARGUMENT")]
    InvalidArgument,
    #[serde(rename = "PROGRAM_NOT_FOUND")]
    ProgramNotFound,
    #[serde(rename = "CWD_UNAVAILABLE")]
    CwdUnavailable,
    #[serde(rename = "CAPABILITY_UNAVAILABLE")]
    CapabilityUnavailable,
    #[serde(rename = "QUEUE_FULL")]
    QueueFull,
    #[serde(rename = "SESSION_LIMIT")]
    SessionLimit,
    #[serde(rename = "REQUEST_CONFLICT")]
    RequestConflict,
    #[serde(rename = "RESOURCE_UNSCHEDULABLE")]
    ResourceUnschedulable,
    #[serde(rename = "SPAWN_FAILED")]
    SpawnFailed,
    #[serde(rename = "GROUP_ATTACH_FAILED")]
    GroupAttachFailed,
    #[serde(rename = "STALE_EPOCH")]
    StaleEpoch,
    #[serde(rename = "NOT_INPUT_OWNER")]
    NotInputOwner,
    #[serde(rename = "INPUT_OUTCOME_UNKNOWN")]
    InputOutcomeUnknown,
    #[serde(rename = "JOURNAL_LIMIT")]
    JournalLimit,
    #[serde(rename = "DISK_FULL")]
    DiskFull,
    #[serde(rename = "REPLAY_UNAVAILABLE")]
    ReplayUnavailable,
    #[serde(rename = "JOURNAL_CORRUPT")]
    JournalCorrupt,
    #[serde(rename = "PROCESS_IDENTITY_CHANGED")]
    ProcessIdentityChanged,
    #[serde(rename = "INVALID_STATE")]
    InvalidState,
    #[serde(rename = "BUSY")]
    Busy,
    #[serde(rename = "DAEMON_UNAVAILABLE")]
    DaemonUnavailable,
    #[serde(rename = "PROTOCOL_MISMATCH")]
    ProtocolMismatch,
}

impl ErrorCode {
    /// Whether an RPC *query* may be transparently retried. This is never a
    /// license to re-run a launch: ambiguous launches stay unresolved.
    pub fn retryable(self) -> bool {
        matches!(self, ErrorCode::DaemonUnavailable | ErrorCode::Busy)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RpcError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    /// Structured, non-secret context (e.g. missing capability names).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl RpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            retryable: code.retryable(),
            code,
            message,
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_through_json_renames() {
        for code in [
            ErrorCode::InvalidArgument,
            ErrorCode::CapabilityUnavailable,
            ErrorCode::ResourceUnschedulable,
            ErrorCode::ProcessIdentityChanged,
            ErrorCode::ProtocolMismatch,
        ] {
            let text = serde_json::to_string(&code).unwrap();
            assert_eq!(serde_json::from_str::<ErrorCode>(&text).unwrap(), code);
        }
    }

    #[test]
    fn only_transport_errors_are_retryable() {
        assert!(ErrorCode::DaemonUnavailable.retryable());
        assert!(!ErrorCode::SpawnFailed.retryable());
        assert!(!ErrorCode::RequestConflict.retryable());
    }
}
