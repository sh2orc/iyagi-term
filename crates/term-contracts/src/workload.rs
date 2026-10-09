//! Workload descriptor and persisted record projection.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{ProcessIdentity, SessionId, U64String, WorkloadId};
use crate::launch::{Enforcement, LaunchMode, LaunchPolicy, Priority};
use crate::snapshot::QueueReason;
use crate::state::WorkloadState;

/// Daemon-side view of one workload row (mirrors `schema.sql` `workloads`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkloadRecord {
    pub id: WorkloadId,
    pub mode: LaunchMode,
    pub state: WorkloadState,
    pub priority: Priority,
    pub reservation_bytes: U64String,
    pub cpu_slots: u32,
    pub enforcement: Enforcement,
    pub memory_max_bytes: Option<U64String>,
    pub cpu_max_cores: Option<f64>,
    pub pids_max: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_reason: Option<QueueReason>,
    pub cancel_requested: bool,
    pub root_exited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
}

/// Everything the executor needs to run one attempt; sensitive argv/env stay
/// daemon-memory-only and never reach persistence or telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkloadDescriptor {
    pub workload_id: WorkloadId,
    pub session_id: SessionId,
    pub cwd: String,
    pub program: String,
    pub argv: Vec<String>,
    pub env_overrides: std::collections::BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
    pub policy: LaunchPolicy,
}

/// Ownership row (mirrors `schema.sql` `process_ownership`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProcessOwnership {
    pub workload_id: WorkloadId,
    pub identity: ProcessIdentity,
    pub group_kind: GroupKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_reference: Option<String>,
    pub coverage: crate::metrics::UsageCoverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    Cgroup,
    Job,
    ObservedTree,
}

/// Durable native identity, independent of a reusable path or process PID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GroupRecoveryIdentity {
    CgroupV2 {
        boot_id: String,
        // FILEID_KERNFS is the full 64-bit node ID, including its generation.
        kernel_id: String,
    },
    MacosGuardian {
        guardian: ProcessIdentity,
        endpoint: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trip_keeps_optional_fields() {
        let rec = WorkloadRecord {
            id: WorkloadId::generate(),
            mode: LaunchMode::Managed,
            state: WorkloadState::Queued,
            priority: Priority(1),
            reservation_bytes: U64String::new(1 << 31).unwrap(),
            cpu_slots: 1,
            enforcement: Enforcement::Observe,
            memory_max_bytes: None,
            cpu_max_cores: None,
            pids_max: None,
            queue_reason: None,
            cancel_requested: false,
            root_exited: false,
            exit_code: None,
            last_error_code: None,
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: WorkloadRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);
    }
}
