//! Mission/task/run control reducers (02 §2/§3): pure next-state decisions
//! for user control actions and adapter terminal outcomes. The daemon owns
//! the transaction; these functions never touch I/O.

use term_contracts::mission::types::{Id, Mission, MissionState, Run, Task, TaskState};

/// Policy envelope the reducer needs (02 §9 defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissionRules {
    pub max_attempts_per_task: u32,
    pub max_repair_cycles: u32,
    pub max_automatic_starts: u64,
}

impl Default for MissionRules {
    fn default() -> Self {
        MissionRules {
            max_attempts_per_task: 3,
            max_repair_cycles: 3,
            max_automatic_starts: 64,
        }
    }
}

/// Side effects a control application wants (the daemon turns these into
/// outbox rows + adapter calls after commit).
#[derive(Debug, Clone, PartialEq)]
pub enum ControlIntent {
    None,
    CancelRuns { run_ids: Vec<Id> },
    StartBootstrapPlan,
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("transition {from:?} → {to:?} is not allowed")]
    Illegal {
        from: MissionState,
        to: MissionState,
    },
    #[error("start requires a non-empty binding allowlist")]
    NoBindings,
    #[error("automatic start budget exhausted ({0})")]
    BudgetExhausted(u64),
}

/// Apply a user control action to the mission projection (02 §2 table).
/// With no engine-spawned runs yet (pausing completes instantly), the
/// reducer still refuses illegal edges and computes the resulting state +
/// intents the daemon must execute.
pub fn apply_control(
    mission: &Mission,
    live_run_count: usize,
    action: MissionAction,
    rules: &MissionRules,
) -> Result<(MissionState, ControlIntent), ControlError> {
    use MissionAction::*;
    let next = match action {
        Start => {
            if !matches!(mission.state, MissionState::Draft) {
                return Err(ControlError::Illegal {
                    from: mission.state,
                    to: MissionState::Running,
                });
            }
            if mission.policy.allowed_binding_ids.is_empty() {
                return Err(ControlError::NoBindings);
            }
            if mission.automatic_start_count as u64 >= rules.max_automatic_starts {
                return Err(ControlError::BudgetExhausted(rules.max_automatic_starts));
            }
            MissionState::Running
        }
        Pause => match mission.state {
            MissionState::Running => {
                if live_run_count == 0 {
                    MissionState::Paused
                } else {
                    MissionState::Pausing
                }
            }
            other => {
                return Err(ControlError::Illegal {
                    from: other,
                    to: MissionState::Pausing,
                })
            }
        },
        Resume => match mission.state {
            MissionState::Paused | MissionState::Pausing => MissionState::Running,
            other => {
                return Err(ControlError::Illegal {
                    from: other,
                    to: MissionState::Running,
                })
            }
        },
        Cancel => match mission.state {
            MissionState::Draft => MissionState::Cancelled,
            MissionState::Running | MissionState::Pausing | MissionState::Paused => {
                MissionState::Stopping
            }
            other => {
                return Err(ControlError::Illegal {
                    from: other,
                    to: MissionState::Stopping,
                })
            }
        },
    };
    let intent = match action {
        Start => ControlIntent::StartBootstrapPlan,
        Cancel if live_run_count > 0 => ControlIntent::CancelRuns {
            run_ids: Vec::new(),
        },
        _ => ControlIntent::None,
    };
    Ok((next, intent))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionAction {
    Start,
    Pause,
    Resume,
    Cancel,
}

/// Terminal adapter outcome application (02 §3): the run's final state
/// drives the task transition. Late results after cancellation keep the
/// task cancelled (E10) — `apply_run_terminal` refuses resurrection.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskOutcome {
    pub task_state: TaskState,
    pub blocked_code: Option<String>,
    pub mission_paused_now: bool,
}

pub fn apply_run_terminal(
    mission: &Mission,
    task: &Task,
    run: &Run,
    succeeded: bool,
    failure_code: Option<&str>,
    rules: &MissionRules,
) -> TaskOutcome {
    let _ = mission;
    let _ = run;
    if task.state == TaskState::Cancelled || task.state == TaskState::Superseded {
        // E10: cancelled tasks never resurrect on late results.
        return TaskOutcome {
            task_state: task.state,
            blocked_code: None,
            mission_paused_now: false,
        };
    }
    if succeeded {
        return TaskOutcome {
            task_state: TaskState::Succeeded,
            blocked_code: None,
            mission_paused_now: false,
        };
    }
    if task.attempt_count + 1 >= rules.max_attempts_per_task {
        TaskOutcome {
            task_state: TaskState::Failed,
            blocked_code: failure_code.map(str::to_string),
            mission_paused_now: false,
        }
    } else {
        TaskOutcome {
            task_state: TaskState::Ready,
            blocked_code: None,
            mission_paused_now: false,
        }
    }
}

/// Pause drain (02 §2): when the last live run finishes during pausing,
/// the mission lands in paused.
pub fn pause_drained(mission: &Mission, live_run_count: usize) -> Option<MissionState> {
    if mission.state == MissionState::Pausing && live_run_count == 0 {
        Some(MissionState::Paused)
    } else {
        None
    }
}

/// Stopping confirmation (02 §2): all executions ended → cancelled;
/// anything unverifiable keeps stopping (OUTCOME_UNKNOWN display).
pub fn stopping_confirmed(
    mission: &Mission,
    live_run_count: usize,
    unknown_run_count: usize,
) -> Option<MissionState> {
    if mission.state != MissionState::Stopping {
        return None;
    }
    if live_run_count > 0 || unknown_run_count > 0 {
        return None;
    }
    Some(MissionState::Cancelled)
}
