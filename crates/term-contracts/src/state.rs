//! Workload state machine and connection states (spec `01-contracts.md` §5).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum WorkloadState {
    #[serde(rename = "QUEUED")]
    Queued,
    #[serde(rename = "STARTING")]
    Starting,
    #[serde(rename = "RUNNING")]
    Running,
    #[serde(rename = "STOPPING")]
    Stopping,
    #[serde(rename = "DRAINING")]
    Draining,
    #[serde(rename = "SUCCEEDED")]
    Succeeded,
    #[serde(rename = "FAILED")]
    Failed,
    #[serde(rename = "CANCELLED")]
    Cancelled,
    #[serde(rename = "INTERRUPTED")]
    Interrupted,
}

impl WorkloadState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            WorkloadState::Succeeded
                | WorkloadState::Failed
                | WorkloadState::Cancelled
                | WorkloadState::Interrupted
        )
    }

    pub fn is_active_for_admission(self) -> bool {
        matches!(
            self,
            WorkloadState::Starting
                | WorkloadState::Running
                | WorkloadState::Stopping
                | WorkloadState::Draining
        )
    }

    /// Legal transitions exactly as specified:
    ///
    /// ```text
    /// QUEUED -> STARTING -> RUNNING -> DRAINING -> SUCCEEDED | FAILED
    /// QUEUED -> CANCELLED
    /// STARTING -> FAILED | STOPPING
    /// RUNNING -> STOPPING -> DRAINING -> CANCELLED
    /// STARTING | RUNNING | STOPPING | DRAINING -> INTERRUPTED (daemon restart)
    /// QUEUED -> INTERRUPTED
    /// ```
    pub fn can_transition(self, to: WorkloadState) -> bool {
        use WorkloadState::*;
        matches!(
            (self, to),
            (Queued, Starting)
                | (Queued, Cancelled)
                | (Queued, Interrupted)
                | (Starting, Running)
                | (Starting, Failed)
                | (Starting, Stopping)
                | (Starting, Interrupted)
                | (Running, Draining)
                | (Running, Stopping)
                | (Running, Interrupted)
                | (Stopping, Draining)
                | (Stopping, Interrupted)
                | (Draining, Succeeded)
                | (Draining, Failed)
                | (Draining, Cancelled)
                | (Draining, Interrupted)
        )
    }
}

/// UI/session attachment state, deliberately independent from the workload
/// lifecycle (spec table `TerminalConnection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum TerminalConnection {
    Attached,
    Detached,
    Resyncing,
}

/// Agent-facing activity. R1 keeps every deep capability `Unknown` unless an
/// official event source exists (spec table `AgentActivity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivity {
    WaitingForInput,
    PermissionRequested,
    ResponseFinished,
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specified_transitions_are_legal() {
        use WorkloadState::*;
        let cases = [
            (Queued, Starting, true),
            (Starting, Running, true),
            (Running, Draining, true),
            (Draining, Succeeded, true),
            (Draining, Failed, true),
            (Queued, Cancelled, true),
            (Starting, Stopping, true),
            (Stopping, Draining, true),
            (Draining, Cancelled, true),
            (Running, Stopping, true),
            (Starting, Failed, true),
            (Running, Interrupted, true),
            (Queued, Interrupted, true),
            // illegal edges
            (Queued, Running, false),
            (Queued, Succeeded, false),
            (Running, Succeeded, false),
            (Succeeded, Running, false),
            (Cancelled, Queued, false),
            (Failed, Failed, false),
            (Running, Queued, false),
            (Starting, Draining, false),
            (Stopping, Succeeded, false),
            (Interrupted, Running, false),
        ];
        for (from, to, legal) in cases {
            assert_eq!(from.can_transition(to), legal, "{from:?} -> {to:?}");
        }
    }

    #[test]
    fn terminal_states_never_leave() {
        use WorkloadState::*;
        for state in [Succeeded, Failed, Cancelled, Interrupted] {
            for to in [
                Queued,
                Starting,
                Running,
                Stopping,
                Draining,
                Succeeded,
                Failed,
                Cancelled,
                Interrupted,
            ] {
                assert!(
                    !state.can_transition(to),
                    "{state:?} -> {to:?} must be illegal"
                );
            }
        }
    }

    #[test]
    fn admission_counts_draining_and_stopping_as_active() {
        use WorkloadState::*;
        assert!(Draining.is_active_for_admission());
        assert!(Stopping.is_active_for_admission());
        assert!(!Queued.is_active_for_admission());
        assert!(!Succeeded.is_active_for_admission());
    }
}
