//! O1 mission RPC error vocabulary (reference `contracts.ts` `ErrorCode`).
//!
//! This enum is deliberately separate from the R1 [`crate::error::ErrorCode`]:
//! mission methods carry their own closed code set and richer details. The
//! wire shape (`{code, message, retryable, details}`) matches the existing
//! RPC error envelope so transports stay unchanged; `details` is structured
//! instead of a free-form JSON value.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::types::Id;
use crate::ids::U64String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MissionErrorCode {
    InvalidArgument,
    InvalidState,
    NotFound,
    RequestConflict,
    RevisionConflict,
    CapabilityUnsupported,
    ModelUnavailable,
    AuthRequired,
    ProviderRateLimited,
    ProviderUnavailable,
    BudgetExceeded,
    UnknownCost,
    PolicyDenied,
    DirtyWorktree,
    WorkspaceBusy,
    PlanCycle,
    PlanLimit,
    ContextTooLarge,
    StaleDecision,
    StaleCandidate,
    ResultInvalid,
    OutcomeUnknown,
    ContentExpired,
    SnapshotExpired,
    CursorExpired,
    ArtifactLimit,
    IntegrityFailed,
    StorageUnavailable,
    Internal,
}

impl MissionErrorCode {
    /// Transport-level retry policy. Only provider/availability classes are
    /// retryable; user-input and state conflicts never are (01 §7).
    pub fn retryable(self) -> bool {
        matches!(
            self,
            MissionErrorCode::ProviderRateLimited
                | MissionErrorCode::ProviderUnavailable
                | MissionErrorCode::StorageUnavailable
        )
    }
}

/// Structured error details (01 §7). Every field optional; absent means
/// genuinely unknown, never zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub struct MissionErrorDetails {
    /// Present on REVISION_CONFLICT: the mission's current revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<U64String>,
    /// Machine-readable reason slug (e.g. unsupported capability reason).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Present on PROVIDER_RATE_LIMITED when the reset window is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// Present on STALE_DECISION: the decision currently awaiting an answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<Id>,
}

/// Mission RPC error body. Same JSON shape as the R1 `RpcError`; the code
/// vocabulary is the O1 set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MissionRpcError {
    pub code: MissionErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(default)]
    pub details: MissionErrorDetails,
}

impl MissionRpcError {
    pub fn new(code: MissionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
            details: MissionErrorDetails::default(),
        }
    }

    pub fn with_details(
        code: MissionErrorCode,
        message: impl Into<String>,
        details: MissionErrorDetails,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
            details,
        }
    }
}

impl std::fmt::Display for MissionRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for MissionRpcError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_as_screaming_snake() {
        assert_eq!(
            serde_json::to_string(&MissionErrorCode::RevisionConflict).unwrap(),
            "\"REVISION_CONFLICT\""
        );
        assert_eq!(
            serde_json::to_string(&MissionErrorCode::CursorExpired).unwrap(),
            "\"CURSOR_EXPIRED\""
        );
    }

    #[test]
    fn error_shape_matches_rpc_envelope() {
        let err = MissionRpcError::with_details(
            MissionErrorCode::RevisionConflict,
            "mission moved on",
            MissionErrorDetails {
                current_revision: Some(U64String::new(7).unwrap()),
                ..Default::default()
            },
        );
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], "REVISION_CONFLICT");
        assert_eq!(json["retryable"], false);
        assert_eq!(json["details"]["current_revision"], "7");
        assert!(json["details"]["decision_id"].is_null());
        // round trip
        let back: MissionRpcError = serde_json::from_value(json).unwrap();
        assert_eq!(back, err);
    }

    #[test]
    fn retryable_policy_is_closed() {
        assert!(MissionErrorCode::ProviderUnavailable.retryable());
        assert!(!MissionErrorCode::RevisionConflict.retryable());
        assert!(!MissionErrorCode::InvalidArgument.retryable());
    }
}
