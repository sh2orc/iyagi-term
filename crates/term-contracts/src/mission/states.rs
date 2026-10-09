//! Allowed state transitions — the executable mirror of
//! `docs/orchestration/states.json`. Edge conditions beyond the raw graph
//! (who may trigger, side effects) live in the engine docs (02); this table
//! only answers "is the edge legal at all".

use super::types::{MissionState, Run, RunState, TaskState};

const MISSION_EDGES: &[(MissionState, &[MissionState])] = &[
    (
        MissionState::Draft,
        &[MissionState::Running, MissionState::Cancelled],
    ),
    (
        MissionState::Running,
        &[
            MissionState::Pausing,
            MissionState::Stopping,
            MissionState::Completed,
            MissionState::Failed,
        ],
    ),
    (
        MissionState::Pausing,
        &[
            MissionState::Paused,
            MissionState::Stopping,
            MissionState::Failed,
        ],
    ),
    (
        MissionState::Paused,
        &[MissionState::Running, MissionState::Stopping],
    ),
    (
        MissionState::Stopping,
        &[MissionState::Cancelled, MissionState::Failed],
    ),
    (MissionState::Completed, &[]),
    (MissionState::Failed, &[]),
    (MissionState::Cancelled, &[]),
];

const TASK_EDGES: &[(TaskState, &[TaskState])] = &[
    (
        TaskState::Planned,
        &[
            TaskState::Ready,
            TaskState::Blocked,
            TaskState::Cancelled,
            TaskState::Superseded,
        ],
    ),
    (
        TaskState::Ready,
        &[
            TaskState::Running,
            TaskState::Blocked,
            TaskState::Cancelled,
            TaskState::Superseded,
        ],
    ),
    (
        TaskState::Running,
        &[
            TaskState::AwaitingInput,
            TaskState::AwaitingReview,
            TaskState::Succeeded,
            TaskState::Failed,
            TaskState::Blocked,
            TaskState::Cancelled,
        ],
    ),
    (
        TaskState::AwaitingInput,
        &[
            TaskState::Running,
            TaskState::Blocked,
            TaskState::Failed,
            TaskState::Cancelled,
        ],
    ),
    (
        TaskState::AwaitingReview,
        &[
            TaskState::Succeeded,
            TaskState::Failed,
            TaskState::Cancelled,
        ],
    ),
    (
        TaskState::Blocked,
        &[
            TaskState::Ready,
            TaskState::Failed,
            TaskState::Cancelled,
            TaskState::Superseded,
        ],
    ),
    (
        TaskState::Failed,
        &[
            TaskState::Ready,
            TaskState::Cancelled,
            TaskState::Superseded,
        ],
    ),
    (TaskState::Succeeded, &[]),
    (TaskState::Cancelled, &[TaskState::Ready, TaskState::Superseded]),
    (TaskState::Superseded, &[]),
];

const RUN_EDGES: &[(RunState, &[RunState])] = &[
    (
        RunState::Prepared,
        &[RunState::Starting, RunState::Cancelled],
    ),
    (
        RunState::Starting,
        &[
            RunState::Running,
            RunState::Stopping,
            RunState::Failed,
            RunState::Interrupted,
            RunState::Unknown,
        ],
    ),
    (
        RunState::Running,
        &[
            RunState::AwaitingInput,
            RunState::Stopping,
            RunState::Succeeded,
            RunState::Failed,
            RunState::Interrupted,
            RunState::Unknown,
        ],
    ),
    (
        RunState::AwaitingInput,
        &[
            RunState::Running,
            RunState::Stopping,
            RunState::Failed,
            RunState::Interrupted,
            RunState::Unknown,
        ],
    ),
    (
        RunState::Stopping,
        &[
            RunState::Cancelled,
            RunState::Failed,
            RunState::Interrupted,
            RunState::Unknown,
        ],
    ),
    (RunState::Succeeded, &[]),
    (RunState::Failed, &[]),
    (RunState::Cancelled, &[]),
    (RunState::Interrupted, &[]),
    (RunState::Unknown, &[]),
];

fn has_edge<S: Copy + PartialEq>(table: &[(S, &[S])], from: S, to: S) -> bool {
    table
        .iter()
        .find(|(src, _)| *src == from)
        .map(|(_, targets)| targets.contains(&to))
        .unwrap_or(false)
}

impl MissionState {
    pub fn can_transition(self, to: MissionState) -> bool {
        has_edge(MISSION_EDGES, self, to)
    }
}

impl TaskState {
    pub fn can_transition(self, to: TaskState) -> bool {
        has_edge(TASK_EDGES, self, to)
    }
    /// Quiescent tasks keep their results. Cancelled tasks require explicit
    /// verified retry or replacement before scheduling can resume.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Succeeded | TaskState::Cancelled | TaskState::Superseded
        )
    }
}

impl RunState {
    pub fn can_transition(self, to: RunState) -> bool {
        has_edge(RUN_EDGES, self, to)
    }
    /// A "live" run occupies a task's single live-run slot and a run cap.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            RunState::Prepared
                | RunState::Starting
                | RunState::Running
                | RunState::AwaitingInput
                | RunState::Stopping
        )
    }
    pub fn is_terminal(self) -> bool {
        !self.is_live()
    }

    /// Conservative state-only bound: uncertain terminal states may still
    /// own external processes. Controllers use `Run::holds_execution_slot`
    /// to include verified termination evidence; cost may remain unsettled.
    pub fn holds_execution_slot(self) -> bool {
        self.is_live() || matches!(self, RunState::Unknown | RunState::Interrupted)
    }
}

impl Run {
    /// Only the daemon's verified reconciliation path may attach this proof.
    /// The historical outcome stays unknown even after local execution ends.
    pub fn holds_execution_slot(&self) -> bool {
        self.state.is_live()
            || (matches!(self.state, RunState::Unknown | RunState::Interrupted)
                && self.reconciliation_ref.is_none())
    }
}
