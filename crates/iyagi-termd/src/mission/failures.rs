//! Confirmed failures get a bounded, evidenced retry or a Run-bound choice.
//! A failure never grants approval, widens a contract, or releases an unknown Run.
use std::collections::HashSet;

use term_contracts::mission::{types::*, MissionErrorCode, MissionRpcError};

use super::{
    service::MissionService,
    workflow::{self, MissionEntities},
};

pub(super) const RETRY_FAILED_TASK: &str = "retry_failed_task";
pub(super) const STOP_FAILED_MISSION: &str = "stop_failed_mission";

/// Cancellation must have finished locally before retry or plan retirement.
/// A task flag alone is not termination evidence, and Unknown has its own flow.
pub(super) fn cancelled_task_is_settled(snapshot: &MissionEntities, task: &Task) -> bool {
    if task.state != TaskState::Cancelled
        || task.active_run_id.is_some()
        || snapshot
            .runs
            .iter()
            .any(|r| r.task_id == task.id && r.holds_execution_slot())
    {
        return false;
    }
    let latest = snapshot
        .runs
        .iter()
        .filter(|r| r.task_id == task.id)
        .max_by_key(|r| r.attempt);
    let Some(run) = latest else {
        return task.attempt_count == 0;
    };
    if run.attempt != task.attempt_count
        || run.state != RunState::Cancelled
        || run.ended_at.is_none()
    {
        return false;
    }
    if run.workspace_id.as_ref().is_some_and(|id| {
        !snapshot.workspaces.iter().any(|w| {
            &w.id == id
                && w.mission_id == snapshot.mission.id
                && w.owned_by_daemon
                && w.state == WorkspaceState::Retained
                && w.writer_run_id.is_none()
        })
    }) {
        return false;
    }
    let execs: Vec<_> = snapshot
        .execs
        .iter()
        .filter(|e| e.run_id == run.id)
        .collect();
    match run.exec_id.as_ref() {
        None => {
            execs.is_empty()
                && (run.dispatch_state == RunDispatchState::Unsent
                    || (matches!(task.kind, TaskKind::Verify)
                        || task.is_deterministic_integration())
                        && matches!(
                            run.retry_evidence,
                            Some(RetryEvidence::RequestNotSubmitted { .. })
                        ))
        }
        Some(id) => matches!(execs.as_slice(), [exec] if &exec.id == id
            && exec.mission_id == snapshot.mission.id && exec.state == ExecState::Exited
            && exec.ended_at.is_some()),
    }
}

pub(super) fn is_failure_decision(decision: &Decision) -> bool {
    decision.kind == DecisionKind::Recovery
        && decision.options.iter().any(|o| o.id == STOP_FAILED_MISSION)
}

fn latest_failure<'a>(snapshot: &'a MissionEntities, task: &Task) -> Option<&'a Run> {
    (task.state == TaskState::Failed
        || (task.is_internal_integration() && task.state == TaskState::Cancelled))
        .then(|| ended_failure(snapshot, task))
        .flatten()
        .filter(|run| {
            !task.is_internal_integration()
                || !snapshot.decisions.iter().any(|d| {
                    d.kind == DecisionKind::Conflict
                        && d.state == DecisionState::Open
                        && d.requesting_run_id.as_ref() == Some(&run.id)
                })
        })
}

pub(super) fn ended_failure<'a>(snapshot: &'a MissionEntities, task: &Task) -> Option<&'a Run> {
    if task.active_run_id.is_some()
        || snapshot
            .runs
            .iter()
            .any(|r| r.task_id == task.id && r.holds_execution_slot())
    {
        return None;
    }
    snapshot
        .runs
        .iter()
        .filter(|r| r.task_id == task.id)
        .max_by_key(|r| r.attempt)
        .filter(|r| {
            (r.state == RunState::Failed
                || (task.is_internal_integration() && r.state == RunState::Cancelled))
                && r.ended_at.is_some()
        })
        .filter(|r| {
            r.exec_id.as_ref().is_none_or(|id| {
                snapshot.execs.iter().any(|e| {
                    &e.id == id
                        && e.run_id == r.id
                        && e.mission_id == r.mission_id
                        && e.state == ExecState::Exited
                        && e.ended_at.is_some()
                })
            })
        })
}

pub(super) fn obsolete_failure_decisions(
    mission: &mut Mission,
    decisions: &[Decision],
    task_id: &Id,
) -> Vec<Entity> {
    decisions
        .iter()
        .filter(|d| {
            d.state == DecisionState::Open
                && is_failure_decision(d)
                && d.affected_task_ids.contains(task_id)
        })
        .map(|d| {
            let mut next = d.clone();
            next.state = DecisionState::Obsolete;
            mission.open_decision_count = mission.open_decision_count.saturating_sub(1);
            Entity::Decision(Box::new(next))
        })
        .collect()
}

impl MissionService {
    pub(super) fn retry_cancelled_task(
        &self,
        snapshot: &MissionEntities,
        task: &Task,
        binding_id: Option<&Id>,
    ) -> Result<Task, MissionRpcError> {
        if !matches!(
            snapshot.mission.state,
            MissionState::Running | MissionState::Paused | MissionState::Pausing
        ) || !cancelled_task_is_settled(snapshot, task)
            || !super::engine::phase_allows(&snapshot.mission, task)
        {
            return Err(MissionRpcError::new(MissionErrorCode::InvalidState,
                "retry requires confirmed cancellation in the current phase; otherwise request a replacement plan"));
        }
        self.prepare_task_retry(snapshot, task, binding_id)
    }

    /// Reconcile before routing queued instructions and after terminal events.
    /// Durable projections also cover a crash between recording failure and
    /// creating its decision. Repeated passes make no no-op revisions.
    pub(super) fn reconcile_task_failures(&self) -> Result<(), MissionRpcError> {
        let mut cursor = None;
        loop {
            let (missions, next) = self
                .storage
                .mission_list(cursor, 50, false)
                .map_err(Self::store_error)?;
            for mission in missions {
                let snapshot = workflow::load_entities(&self.storage, &mission.id)?;
                let result = self.reconcile_failure_snapshot(&snapshot);
                if let Err(error) = result {
                    if error.code != MissionErrorCode::RevisionConflict {
                        return Err(error);
                    }
                }
            }
            if next.is_none() {
                break;
            }
            cursor = next;
        }
        Ok(())
    }

    fn reconcile_failure_snapshot(
        &self,
        snapshot: &MissionEntities,
    ) -> Result<(), MissionRpcError> {
        if self.reconcile_transient_retry_snapshot(snapshot)? {
            return Ok(());
        }
        if self.reconcile_plan_repair_snapshot(snapshot)? {
            return Ok(());
        }
        if self.reconcile_required_failures(snapshot)? {
            return Ok(());
        }
        if self.reconcile_provider_blocks(snapshot)? {
            return Ok(());
        }
        let mut mission = snapshot.mission.clone();
        let actionable = matches!(
            mission.state,
            MissionState::Running | MissionState::Pausing | MissionState::Paused
        );
        let mut entities = Vec::new();
        let mut retained = HashSet::new();
        for decision in snapshot
            .decisions
            .iter()
            .filter(|d| d.state == DecisionState::Open && is_failure_decision(d))
        {
            let task = snapshot
                .tasks
                .iter()
                .find(|t| decision.affected_task_ids == [t.id.clone()]);
            let current = task.and_then(|t| latest_failure(snapshot, t));
            let retry_allowed =
                task.is_some_and(|t| t.attempt_count < mission.policy.max_attempts_per_task);
            if actionable
                && !current
                    .is_some_and(|r| super::failure_repair::has_repair_owner(snapshot, &r.id))
                && current.is_some_and(|r| Some(&r.id) == decision.requesting_run_id.as_ref())
                && decision.plan_revision == mission.plan_revision
                && decision.candidate_id == mission.candidate_id
                && decision.options.iter().any(|o| o.id == RETRY_FAILED_TASK) == retry_allowed
            {
                retained.insert(task.expect("current failure task").id.clone());
            } else {
                let mut obsolete = decision.clone();
                obsolete.state = DecisionState::Obsolete;
                mission.open_decision_count = mission.open_decision_count.saturating_sub(1);
                entities.push(Entity::Decision(Box::new(obsolete)));
            }
        }
        if actionable {
            for task in &snapshot.tasks {
                let Some(run) = latest_failure(snapshot, task) else {
                    continue;
                };
                // A completed deterministic verification already drives the
                // bounded repair-plan path. Preparation/runner errors do not.
                if retained.contains(&task.id)
                    || super::failure_repair::has_repair_owner(snapshot, &run.id)
                    || (task.kind == TaskKind::Verify
                        && snapshot.verifications.iter().any(|v| v.run_id == run.id))
                {
                    continue;
                }
                let question = workflow::store_artifact(&self.artifacts, &mission.id, "text/plain",
                    format!("Task '{}' failed ({:?}). The previous execution has ended. Inspect its retained result and fix configuration or input before retrying. Retrying creates a new Run within the task attempt budget; unknown messages are not replayed. Independent tasks can continue.", task.title, run.failure_code).as_bytes())?;
                let mut options = vec![];
                if task.attempt_count < mission.policy.max_attempts_per_task {
                    options.push(DecisionOption {
                        id: RETRY_FAILED_TASK.into(),
                        label: "Retry after fixing the cause".into(),
                    });
                }
                options.push(DecisionOption {
                    id: STOP_FAILED_MISSION.into(),
                    label: "End mission as failed".into(),
                });
                let (_, decision) = super::engine::new_decision(
                    &mission,
                    DecisionKind::Recovery,
                    question,
                    options,
                    vec![task.id.clone()],
                    false,
                    Some(run.id.clone()),
                );
                mission.open_decision_count += 1;
                entities.push(Entity::Decision(Box::new(decision)));
            }
            // Explain dependency failure transitively without stopping
            // unrelated ready tasks or overwriting another blocking reason.
            let mut failed: HashSet<_> = snapshot
                .tasks
                .iter()
                .filter(|t| {
                    matches!(
                        t.state,
                        TaskState::Failed | TaskState::Cancelled | TaskState::Superseded
                    )
                })
                .map(|t| t.id.clone())
                .collect();
            loop {
                let before = failed.len();
                for task in &snapshot.tasks {
                    if task.active_run_id.is_none()
                        && matches!(
                            task.state,
                            TaskState::Planned | TaskState::Ready | TaskState::Blocked
                        )
                        && task.depends_on.iter().any(|id| failed.contains(id))
                    {
                        failed.insert(task.id.clone());
                    }
                }
                if before == failed.len() {
                    break;
                }
            }
            for task in &snapshot.tasks {
                if failed.contains(&task.id)
                    && task.active_run_id.is_none()
                    && (matches!(task.state, TaskState::Planned | TaskState::Ready)
                        || (task.state == TaskState::Blocked
                            && matches!(
                                task.blocked_code.as_deref(),
                                None | Some("awaiting_dependency")
                            )))
                {
                    let mut next = task.clone();
                    next.state = TaskState::Blocked;
                    next.blocked_code = Some("dependency_failed".into());
                    next.updated_at = term_storage::time::now_iso8601();
                    entities.push(Entity::Task(Box::new(next)));
                }
            }
        }
        if !entities.is_empty() {
            self.commit_actor(mission, "engine.task_failure_decision", entities, vec![])?;
        }
        Ok(())
    }

    pub(super) fn failure_decision_task<'a>(
        &self,
        snapshot: &'a MissionEntities,
        decision: &Decision,
    ) -> Result<&'a Task, MissionRpcError> {
        snapshot
            .tasks
            .iter()
            .find(|t| {
                decision.affected_task_ids == [t.id.clone()]
                    && latest_failure(snapshot, t)
                        .is_some_and(|r| Some(&r.id) == decision.requesting_run_id.as_ref())
            })
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::StaleDecision,
                    "the task no longer has this confirmed failed run",
                )
            })
    }

    pub(super) fn retry_failed_task(
        &self,
        snapshot: &MissionEntities,
        task: &Task,
        binding_id: Option<&Id>,
    ) -> Result<Task, MissionRpcError> {
        if task.state == TaskState::Cancelled {
            return self.retry_cancelled_task(snapshot, task, binding_id);
        }
        if !matches!(
            snapshot.mission.state,
            MissionState::Running | MissionState::Paused | MissionState::Pausing
        ) {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidState,
                "mission cannot retry tasks in its current state",
            ));
        }
        if latest_failure(snapshot, task).is_none() {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidState,
                "retry requires a confirmed failed run and no owned execution",
            ));
        }
        if latest_failure(snapshot, task)
            .is_some_and(|r| super::failure_repair::has_repair_owner(snapshot, &r.id))
        {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidState,
                "the Lead repair plan owns recovery; resolve or cancel that plan before retrying this task",
            ));
        }
        self.prepare_task_retry(snapshot, task, binding_id)
    }

    /// Caller must first validate the exact failed Run or its termination proof.
    pub(super) fn prepare_task_retry(
        &self,
        snapshot: &MissionEntities,
        task: &Task,
        binding_id: Option<&Id>,
    ) -> Result<Task, MissionRpcError> {
        if task.attempt_count >= snapshot.mission.policy.max_attempts_per_task {
            return Err(MissionRpcError::new(
                MissionErrorCode::PolicyDenied,
                "attempt budget exhausted; adjust the policy before retrying",
            ));
        }
        if snapshot.mission.automatic_start_count >= snapshot.mission.policy.max_automatic_starts
            || self.effective_active_time(&snapshot.mission)
                >= snapshot.mission.policy.active_time_limit_ms
        {
            return Err(MissionRpcError::new(
                MissionErrorCode::BudgetExceeded,
                "mission execution budget is exhausted; adjust the policy before retrying",
            ));
        }
        let mut next = task.clone();
        // A rejected continuation returns to the integrator for inspection;
        // repeating the same invalid helper input cannot resolve the files.
        if let Some(IntegrationTask { step, .. }) = next.integration.as_mut() {
            if let IntegrationStep::Continuing {
                conflict_run_id, ..
            } = step
            {
                *step = IntegrationStep::Resolving {
                    conflict_run_id: conflict_run_id.clone(),
                };
            }
        }
        if let Some(binding) = binding_id {
            self.validate_task_binding(&snapshot.mission, &next, binding)?;
            next.binding_id = Some(binding.clone());
        } else if let Some(binding) = next.execution_binding_id() {
            self.validate_task_binding(&snapshot.mission, &next, binding)?;
        } else if next.kind != TaskKind::Verify && !next.is_deterministic_integration() {
            return Err(MissionRpcError::new(
                MissionErrorCode::ModelUnavailable,
                "select a binding before retrying",
            ));
        }
        next.state = TaskState::Ready;
        next.active_run_id = None;
        next.blocked_code = None;
        next.dispatch_after_unix_ms = None;
        next.updated_at = term_storage::time::now_iso8601();
        Ok(next)
    }

    pub(super) fn validate_task_binding(
        &self,
        mission: &Mission,
        task: &Task,
        id: &Id,
    ) -> Result<(), MissionRpcError> {
        if task.kind == TaskKind::Verify
            || (task.is_deterministic_integration() && task.integration.is_none())
            || !mission.policy.allowed_binding_ids.contains(id)
        {
            return Err(MissionRpcError::new(
                MissionErrorCode::PolicyDenied,
                "binding is outside this task's policy",
            ));
        }
        let bindings: Vec<Binding> = self
            .storage
            .mission_bindings()
            .map_err(Self::store_error)?
            .iter()
            .map(|value| self.observed_binding(value))
            .collect::<Result<_, _>>()?;
        if let Some(binding) = bindings.iter().find(|binding| &binding.id == id) {
            self.require_binding_capability(binding, task.kind)?;
        }
        if !bindings.iter().any(|b| &b.id == id && b.enabled) {
            return Err(MissionRpcError::new(
                MissionErrorCode::ModelUnavailable,
                "select an enabled, configured binding",
            ));
        }
        Ok(())
    }
}
