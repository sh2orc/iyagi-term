//! Bidirectional TEXT mapping for every persisted enum.
//!
//! The strings are exactly the rename strings term-contracts uses on the wire
//! (and the exact strings the schema CHECK constraints whitelist), so a value
//! can move DB -> contract -> DB without drift. Explicit match functions
//! instead of serde: unknown stored text becomes `None` (mapped to
//! [`crate::error::StorageError::Corrupt`]) instead of a serde parse error.

use term_contracts::launch::{Enforcement, LaunchMode};
use term_contracts::metrics::UsageCoverage;
use term_contracts::snapshot::QueueReason;
use term_contracts::state::WorkloadState;
use term_contracts::workload::GroupKind;

// --- WorkloadState (schema workloads.state CHECK list) ---

pub(crate) fn workload_state_to_str(state: WorkloadState) -> &'static str {
    match state {
        WorkloadState::Queued => "QUEUED",
        WorkloadState::Starting => "STARTING",
        WorkloadState::Running => "RUNNING",
        WorkloadState::Stopping => "STOPPING",
        WorkloadState::Draining => "DRAINING",
        WorkloadState::Succeeded => "SUCCEEDED",
        WorkloadState::Failed => "FAILED",
        WorkloadState::Cancelled => "CANCELLED",
        WorkloadState::Interrupted => "INTERRUPTED",
    }
}

pub(crate) fn workload_state_from_str(s: &str) -> Option<WorkloadState> {
    Some(match s {
        "QUEUED" => WorkloadState::Queued,
        "STARTING" => WorkloadState::Starting,
        "RUNNING" => WorkloadState::Running,
        "STOPPING" => WorkloadState::Stopping,
        "DRAINING" => WorkloadState::Draining,
        "SUCCEEDED" => WorkloadState::Succeeded,
        "FAILED" => WorkloadState::Failed,
        "CANCELLED" => WorkloadState::Cancelled,
        "INTERRUPTED" => WorkloadState::Interrupted,
        _ => return None,
    })
}

// --- LaunchMode (workloads.mode) ---

pub(crate) fn launch_mode_to_str(mode: LaunchMode) -> &'static str {
    match mode {
        LaunchMode::Shell => "shell",
        LaunchMode::Managed => "managed",
    }
}

pub(crate) fn launch_mode_from_str(s: &str) -> Option<LaunchMode> {
    Some(match s {
        "shell" => LaunchMode::Shell,
        "managed" => LaunchMode::Managed,
        _ => return None,
    })
}

// --- Enforcement (workloads.enforcement) ---

pub(crate) fn enforcement_to_str(value: Enforcement) -> &'static str {
    match value {
        Enforcement::Observe => "observe",
        Enforcement::Prefer => "prefer",
        Enforcement::Require => "require",
    }
}

pub(crate) fn enforcement_from_str(s: &str) -> Option<Enforcement> {
    Some(match s {
        "observe" => Enforcement::Observe,
        "prefer" => Enforcement::Prefer,
        "require" => Enforcement::Require,
        _ => return None,
    })
}

// --- GroupKind (process_ownership.group_kind) ---

pub(crate) fn group_kind_to_str(kind: GroupKind) -> &'static str {
    match kind {
        GroupKind::Cgroup => "cgroup",
        GroupKind::Job => "job",
        GroupKind::ObservedTree => "observed_tree",
    }
}

pub(crate) fn group_kind_from_str(s: &str) -> Option<GroupKind> {
    Some(match s {
        "cgroup" => GroupKind::Cgroup,
        "job" => GroupKind::Job,
        "observed_tree" => GroupKind::ObservedTree,
        _ => return None,
    })
}

// --- UsageCoverage (process_ownership.coverage) ---

pub(crate) fn usage_coverage_to_str(coverage: UsageCoverage) -> &'static str {
    match coverage {
        UsageCoverage::Group => "group",
        UsageCoverage::ObservedTree => "observed_tree",
        UsageCoverage::Partial => "partial",
    }
}

pub(crate) fn usage_coverage_from_str(s: &str) -> Option<UsageCoverage> {
    Some(match s {
        "group" => UsageCoverage::Group,
        "observed_tree" => UsageCoverage::ObservedTree,
        "partial" => UsageCoverage::Partial,
        _ => return None,
    })
}

// --- QueueReason (workloads.queue_reason; no SQL CHECK, contract is authoritative) ---

pub(crate) fn queue_reason_to_str(reason: QueueReason) -> &'static str {
    match reason {
        QueueReason::WaitTelemetry => "WAIT_TELEMETRY",
        QueueReason::ResourceUnschedulable => "RESOURCE_UNSCHEDULABLE",
        QueueReason::WaitHostPressure => "WAIT_HOST_PRESSURE",
        QueueReason::WaitConcurrency => "WAIT_CONCURRENCY",
        QueueReason::WaitCpuSlots => "WAIT_CPU_SLOTS",
        QueueReason::WaitReservationBudget => "WAIT_RESERVATION_BUDGET",
        QueueReason::WaitMemoryHeadroom => "WAIT_MEMORY_HEADROOM",
        QueueReason::Admit => "ADMIT",
    }
}

pub(crate) fn queue_reason_from_str(s: &str) -> Option<QueueReason> {
    Some(match s {
        "WAIT_TELEMETRY" => QueueReason::WaitTelemetry,
        "RESOURCE_UNSCHEDULABLE" => QueueReason::ResourceUnschedulable,
        "WAIT_HOST_PRESSURE" => QueueReason::WaitHostPressure,
        "WAIT_CONCURRENCY" => QueueReason::WaitConcurrency,
        "WAIT_CPU_SLOTS" => QueueReason::WaitCpuSlots,
        "WAIT_RESERVATION_BUDGET" => QueueReason::WaitReservationBudget,
        "WAIT_MEMORY_HEADROOM" => QueueReason::WaitMemoryHeadroom,
        "ADMIT" => QueueReason::Admit,
        _ => return None,
    })
}

// --- requests.outcome ---

pub(crate) fn request_outcome_to_str(outcome: crate::types::RequestOutcome) -> &'static str {
    match outcome {
        crate::types::RequestOutcome::Accepted => "accepted",
        crate::types::RequestOutcome::Completed => "completed",
        crate::types::RequestOutcome::Failed => "failed",
        crate::types::RequestOutcome::Unknown => "unknown",
    }
}

pub(crate) fn request_outcome_from_str(s: &str) -> Option<crate::types::RequestOutcome> {
    Some(match s {
        "accepted" => crate::types::RequestOutcome::Accepted,
        "completed" => crate::types::RequestOutcome::Completed,
        "failed" => crate::types::RequestOutcome::Failed,
        "unknown" => crate::types::RequestOutcome::Unknown,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_enum_round_trips_through_its_schema_string() {
        let states = [
            WorkloadState::Queued,
            WorkloadState::Starting,
            WorkloadState::Running,
            WorkloadState::Stopping,
            WorkloadState::Draining,
            WorkloadState::Succeeded,
            WorkloadState::Failed,
            WorkloadState::Cancelled,
            WorkloadState::Interrupted,
        ];
        for s in states {
            assert_eq!(workload_state_from_str(workload_state_to_str(s)), Some(s));
        }

        for m in [LaunchMode::Shell, LaunchMode::Managed] {
            assert_eq!(launch_mode_from_str(launch_mode_to_str(m)), Some(m));
        }
        for e in [
            Enforcement::Observe,
            Enforcement::Prefer,
            Enforcement::Require,
        ] {
            assert_eq!(enforcement_from_str(enforcement_to_str(e)), Some(e));
        }
        for g in [GroupKind::Cgroup, GroupKind::Job, GroupKind::ObservedTree] {
            assert_eq!(group_kind_from_str(group_kind_to_str(g)), Some(g));
        }
        for c in [
            UsageCoverage::Group,
            UsageCoverage::ObservedTree,
            UsageCoverage::Partial,
        ] {
            assert_eq!(usage_coverage_from_str(usage_coverage_to_str(c)), Some(c));
        }
        let reasons = [
            QueueReason::WaitTelemetry,
            QueueReason::ResourceUnschedulable,
            QueueReason::WaitHostPressure,
            QueueReason::WaitConcurrency,
            QueueReason::WaitCpuSlots,
            QueueReason::WaitReservationBudget,
            QueueReason::WaitMemoryHeadroom,
            QueueReason::Admit,
        ];
        for r in reasons {
            assert_eq!(queue_reason_from_str(queue_reason_to_str(r)), Some(r));
        }
    }

    #[test]
    fn exact_rename_strings_match_the_wire_format() {
        // Guard against drift from term-contracts serde renames: serialize the
        // contract value and compare with the storage string.
        assert_eq!(
            serde_json::to_string(&WorkloadState::Interrupted).unwrap(),
            format!("\"{}\"", workload_state_to_str(WorkloadState::Interrupted))
        );
        assert_eq!(
            serde_json::to_string(&LaunchMode::Managed).unwrap(),
            format!("\"{}\"", launch_mode_to_str(LaunchMode::Managed))
        );
        assert_eq!(
            serde_json::to_string(&Enforcement::Require).unwrap(),
            format!("\"{}\"", enforcement_to_str(Enforcement::Require))
        );
        assert_eq!(
            serde_json::to_string(&GroupKind::ObservedTree).unwrap(),
            format!("\"{}\"", group_kind_to_str(GroupKind::ObservedTree))
        );
        assert_eq!(
            serde_json::to_string(&QueueReason::WaitMemoryHeadroom).unwrap(),
            format!(
                "\"{}\"",
                queue_reason_to_str(QueueReason::WaitMemoryHeadroom)
            )
        );
        assert_eq!(
            serde_json::to_string(&UsageCoverage::Partial).unwrap(),
            format!("\"{}\"", usage_coverage_to_str(UsageCoverage::Partial))
        );
    }

    #[test]
    fn unknown_text_maps_to_none() {
        assert_eq!(workload_state_from_str("PAUSED"), None);
        assert_eq!(workload_state_from_str("queued"), None);
        assert_eq!(launch_mode_from_str("Managed"), None);
        assert_eq!(enforcement_from_str("force"), None);
        assert_eq!(group_kind_from_str("gang"), None);
        assert_eq!(usage_coverage_from_str("full"), None);
        assert_eq!(queue_reason_from_str("WAIT_A_BIT"), None);
    }
}
