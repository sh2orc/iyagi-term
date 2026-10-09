//! Error surface for term-core decision logic.

use term_contracts::error::ErrorCode;
use term_contracts::ids::WorkloadId;
use term_contracts::snapshot::QueueReason;

/// Failures produced by admission, reservation, and queue logic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    /// Bounded queue rejected the enqueue (`QUEUE_FULL` semantics,
    /// `limits.queued_workloads` = 64).
    #[error("queue full: {len}/{capacity} queued workloads")]
    QueueFull { len: usize, capacity: u32 },
    /// Admission denied with the first failing check in the normative order.
    /// Invariant: `reason` is never [`QueueReason::Admit`].
    #[error("admission denied: {reason:?}")]
    AdmissionDenied { reason: QueueReason },
    /// A workload id tried to acquire a second live reservation.
    #[error("workload {workload_id} already holds a reservation")]
    DuplicateReservation { workload_id: WorkloadId },
    /// A workload id tried to enqueue while already queued.
    #[error("workload {workload_id} is already queued")]
    DuplicateQueueEntry { workload_id: WorkloadId },
}

impl CoreError {
    /// Stable wire code for the daemon RPC layer. Transient wait reasons
    /// (telemetry, pressure, concurrency, slots, budget, headroom) map to
    /// `BUSY` — retryable by contract, and the workload is queued, not failed.
    /// `RESOURCE_UNSCHEDULABLE` keeps its dedicated code: it needs a config
    /// change, not a retry.
    pub fn rpc_code(&self) -> ErrorCode {
        match self {
            CoreError::QueueFull { .. } => ErrorCode::QueueFull,
            CoreError::AdmissionDenied {
                reason: QueueReason::ResourceUnschedulable,
            } => ErrorCode::ResourceUnschedulable,
            CoreError::AdmissionDenied { .. } => ErrorCode::Busy,
            CoreError::DuplicateReservation { .. } | CoreError::DuplicateQueueEntry { .. } => {
                ErrorCode::InvalidState
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_codes_follow_retryability_semantics() {
        assert_eq!(
            CoreError::QueueFull {
                len: 64,
                capacity: 64
            }
            .rpc_code(),
            ErrorCode::QueueFull
        );
        assert_eq!(
            CoreError::AdmissionDenied {
                reason: QueueReason::ResourceUnschedulable
            }
            .rpc_code(),
            ErrorCode::ResourceUnschedulable
        );
        assert_eq!(
            CoreError::AdmissionDenied {
                reason: QueueReason::WaitConcurrency
            }
            .rpc_code(),
            ErrorCode::Busy
        );
        assert!(ErrorCode::Busy.retryable());
        assert!(!ErrorCode::QueueFull.retryable());
    }

    #[test]
    fn display_messages_are_stable_and_id_scoped() {
        let err = CoreError::QueueFull {
            len: 64,
            capacity: 64,
        };
        assert_eq!(err.to_string(), "queue full: 64/64 queued workloads");
        let err = CoreError::AdmissionDenied {
            reason: QueueReason::WaitTelemetry,
        };
        assert_eq!(err.to_string(), "admission denied: WaitTelemetry");
    }
}
