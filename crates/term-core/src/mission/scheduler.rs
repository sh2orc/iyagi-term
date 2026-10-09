//! Mission scheduling decisions (02 §6): pure selection over the snapshot.
//! Execution order: running missions only, round-robin between missions,
//! oldest-ready within a mission, then dependency/decision/policy/binding
//! gates and the global/mission/binding run caps. Simple slot shortage
//! keeps the task `ready` (queue reason only); hard policy or dependency
//! failures mark `blocked`.

use std::collections::HashMap;

use term_contracts::mission::types::{
    DecisionState, Id, MissionState, Policy, RunState, Task, TaskState,
};

/// defaults.json caps (08 §2 table): global 8 / mission 4 / binding 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapLimits {
    pub global_runs: usize,
    pub per_mission_runs: usize,
    pub per_binding_runs: usize,
}

impl Default for CapLimits {
    fn default() -> Self {
        CapLimits {
            global_runs: 8,
            per_mission_runs: 4,
            per_binding_runs: 2,
        }
    }
}

/// Why a ready task was not dispatched this tick (observable queue reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    GlobalCapFull,
    MissionCapFull,
    BindingCapFull { binding_id: Id },
    OpenBlockingDecision { decision_id: Id },
    MissionPausing,
    MissionStopping,
    AwaitingDependency { task_id: Id },
    MissingBinding,
    AttemptLimit,
}

/// The scheduler's verdict for one candidate task.
#[derive(Debug, Clone, PartialEq)]
pub enum DispatchVerdict {
    Dispatch(DispatchChoice),
    Skip(SkipReason),
}

/// A dispatch the daemon must turn into prepared Run + outbox rows.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchChoice {
    pub mission_id: Id,
    pub task_id: Id,
    /// Deterministic verification tasks do not use a provider binding.
    pub binding_id: Option<Id>,
    /// New attempt number (task.attempt_count + 1).
    pub attempt: u32,
}

/// One mission's projection slice the scheduler needs.
pub struct MissionSlice<'a> {
    pub mission_id: Id,
    pub state: MissionState,
    pub policy: &'a Policy,
    pub ready_tasks: Vec<&'a Task>,
    pub live_runs: Vec<(Id, Option<Id>)>, // (run_id, binding_id)
    pub open_blocking_decisions: Vec<Id>,
    /// Round-robin position bookkeeping key (updated by the daemon).
    pub fairness_cursor: usize,
}

/// Global live-run count across all missions.
pub fn select_dispatches(
    slices: &mut [MissionSlice<'_>],
    caps: CapLimits,
) -> Vec<(DispatchChoice, DispatchVerdict)> {
    let mut results = Vec::new();
    let mut global_live = 0;
    let mut binding_live: HashMap<Id, usize> = HashMap::new();
    // Pausing/stopping/unknown runs still own slots, even though their
    // missions cannot dispatch. Binding caps span every mission.
    for slice in slices.iter() {
        global_live += slice.live_runs.len();
        for (_, binding) in &slice.live_runs {
            if let Some(binding) = binding {
                *binding_live.entry(binding.clone()).or_default() += 1;
            }
        }
    }
    let mut reported_skips = std::collections::HashSet::new();
    loop {
        let mut progressed = false;
        // The caller rotates mission order between ticks. In one pass
        // each mission gets at most one slot before the next mission.
        for slice in slices.iter_mut() {
            if slice.state != MissionState::Running || slice.ready_tasks.is_empty() {
                continue;
            }
            let len = slice.ready_tasks.len();
            let mut picked = None;
            for offset in 0..len {
                let index = (slice.fairness_cursor + offset) % len;
                let task = slice.ready_tasks[index];
                let choice = DispatchChoice {
                    mission_id: slice.mission_id.clone(),
                    task_id: task.id.clone(),
                    binding_id: task.execution_binding_id().cloned(),
                    attempt: task.attempt_count.saturating_add(1),
                };
                match evaluate(task, slice, global_live, &binding_live, caps) {
                    None => {
                        picked = Some((index, choice));
                        break;
                    }
                    Some(reason) => {
                        if reported_skips.insert(task.id.clone()) {
                            results.push((choice, DispatchVerdict::Skip(reason)));
                        }
                    }
                }
            }
            if let Some((index, choice)) = picked {
                slice.ready_tasks.remove(index);
                // A scheduling reservation, not a minted provider Run.
                slice
                    .live_runs
                    .push((choice.task_id.clone(), choice.binding_id.clone()));
                if let Some(binding) = &choice.binding_id {
                    *binding_live.entry(binding.clone()).or_default() += 1;
                }
                slice.fairness_cursor = index % slice.ready_tasks.len().max(1);
                global_live += 1;
                results.push((choice.clone(), DispatchVerdict::Dispatch(choice)));
                progressed = true;
            }
        }
        if !progressed || global_live >= caps.global_runs {
            break;
        }
    }
    results
}

fn evaluate(
    task: &Task,
    slice: &MissionSlice<'_>,
    global_live: usize,
    binding_live: &HashMap<Id, usize>,
    caps: CapLimits,
) -> Option<SkipReason> {
    if let Some(decision_id) = slice.open_blocking_decisions.first() {
        return Some(SkipReason::OpenBlockingDecision {
            decision_id: decision_id.clone(),
        });
    }
    if task.attempt_count >= slice.policy.max_attempts_per_task {
        return Some(SkipReason::AttemptLimit);
    }
    if task.binding_id.is_none()
        && task.kind != term_contracts::mission::types::TaskKind::Verify
        && !task.is_deterministic_integration()
    {
        return Some(SkipReason::MissingBinding);
    }
    if global_live >= caps.global_runs {
        return Some(SkipReason::GlobalCapFull);
    }
    let mission_cap = (slice.policy.max_parallel_runs as usize).min(caps.per_mission_runs);
    if slice.live_runs.len() >= mission_cap {
        return Some(SkipReason::MissionCapFull);
    }
    if let Some(binding) = task.execution_binding_id() {
        if binding_live.get(binding).copied().unwrap_or(0) >= caps.per_binding_runs {
            return Some(SkipReason::BindingCapFull {
                binding_id: binding.clone(),
            });
        }
    }
    None
}

/// Task readiness (02 §3 planned → ready): dependencies succeeded and no
/// blocking reason. Pure helper the daemon's snapshot builder uses.
pub fn task_is_ready(
    task: &Task,
    tasks: &HashMap<Id, TaskState>,
    open_blocking_decisions: &[Id],
) -> bool {
    if !matches!(
        task.state,
        TaskState::Planned | TaskState::Ready | TaskState::Blocked
    ) || task.active_run_id.is_some()
    {
        return false;
    }
    if !open_blocking_decisions.is_empty() {
        return false;
    }
    if task.state == TaskState::Blocked
        && !matches!(
            task.blocked_code.as_deref(),
            None | Some("awaiting_dependency" | "dependency_failed" | "resume_pending")
        )
    {
        return false;
    }
    task.depends_on.iter().all(|dep| {
        tasks
            .get(dep)
            .map(|state| matches!(state, TaskState::Succeeded))
            .unwrap_or(false)
    })
}

/// Live-run counting helper (RunState::is_live mirrors the SQL partial
/// index).
pub fn count_live(runs: &[RunState]) -> usize {
    runs.iter().filter(|state| state.is_live()).count()
}

/// Decision openness (the mission's open_decision_count source).
pub fn open_blocking(decisions: &[(Id, DecisionState, bool)]) -> Vec<Id> {
    decisions
        .iter()
        .filter(|(_, state, blocking)| *state == DecisionState::Open && *blocking)
        .map(|(id, _, _)| id.clone())
        .collect()
}
