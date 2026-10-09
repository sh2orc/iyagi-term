//! Give exhausted required tasks one durable Lead owner per failed Run.
//! Repair cycles add validated replacements; they never reset task attempts.
use serde_json::{json, Value};
use term_contracts::mission::{types::*, MissionErrorCode, MissionRpcError};

use super::{
    failures, pipeline,
    service::MissionService,
    workflow::{self, MissionEntities},
};

pub(super) fn has_repair_owner(snapshot: &MissionEntities, run_id: &Id) -> bool {
    snapshot.tasks.iter().any(|t| {
        t.kind == TaskKind::Plan
            && t.role == Some(Role::Lead)
            && !matches!(t.state, TaskState::Cancelled | TaskState::Superseded)
            && t.failure_repair_run_ids.contains(run_id)
    })
}

pub(super) fn exec_cleanup_pending(snapshot: &MissionEntities) -> bool {
    snapshot
        .execs
        .iter()
        .any(|e| e.state != ExecState::Exited || e.ended_at.is_none())
        || snapshot.runs.iter().any(|r| {
            r.exec_id.as_ref().is_some_and(|id| {
                !snapshot.execs.iter().any(|e| {
                    &e.id == id
                        && e.run_id == r.id
                        && e.mission_id == r.mission_id
                        && e.state == ExecState::Exited
                        && e.ended_at.is_some()
                })
            })
        })
}

fn source<'a>(
    snapshot: &'a MissionEntities,
    run_id: &Id,
) -> Result<(&'a Task, &'a Run), MissionRpcError> {
    let run = snapshot
        .runs
        .iter()
        .find(|r| &r.id == run_id)
        .ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::OutcomeUnknown,
                "repair source Run is unavailable",
            )
        })?;
    let task = snapshot
        .tasks
        .iter()
        .find(|t| t.id == run.task_id && t.required && t.state == TaskState::Failed)
        .filter(|t| failures::ended_failure(snapshot, t).is_some_and(|r| &r.id == run_id))
        .ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::StaleDecision,
                "repair source is no longer the current ended failure",
            )
        })?;
    Ok((task, run))
}

impl MissionService {
    pub(super) fn reconcile_required_failures(
        &self,
        snapshot: &MissionEntities,
    ) -> Result<bool, MissionRpcError> {
        if snapshot.mission.state != MissionState::Running
            || snapshot
                .tasks
                .iter()
                .any(|t| t.kind == TaskKind::Plan && !t.state.is_terminal())
        {
            return Ok(false);
        }
        let mut sources: Vec<_> = snapshot
            .tasks
            .iter()
            .filter(|t| {
                t.required
                    && !t.is_internal_integration()
                    && t.kind != TaskKind::Plan
                    && t.state == TaskState::Failed
                    && t.attempt_count >= snapshot.mission.policy.max_attempts_per_task
            })
            .filter_map(|t| failures::ended_failure(snapshot, t).map(|r| (t, r)))
            .filter(|(_, r)| r.failure_code != Some(MissionErrorCode::OutcomeUnknown))
            .filter(|(t, r)| {
                !(t.kind == TaskKind::Verify
                    && snapshot.verifications.iter().any(|v| v.run_id == r.id))
            })
            // Even cancelling a repair task must not recreate it each tick.
            .filter(|(_, r)| {
                !snapshot
                    .tasks
                    .iter()
                    .any(|t| t.failure_repair_run_ids.contains(&r.id))
            })
            .collect();
        sources.sort_by_key(|(t, _)| t.ordinal);
        if sources.is_empty() {
            return Ok(false);
        }
        let cycle = snapshot
            .tasks
            .iter()
            .map(|t| t.repair_cycle)
            .max()
            .unwrap_or(0);
        if cycle >= snapshot.mission.policy.max_repair_cycles
            || snapshot.tasks.len() >= self.limits.max_tasks_per_mission
        {
            self.stop_exhausted_failure(
                snapshot,
                sources[0]
                    .1
                    .failure_code
                    .unwrap_or(MissionErrorCode::ResultInvalid),
            )?;
            return Ok(true);
        }
        if snapshot
            .decisions
            .iter()
            .any(|d| d.state == DecisionState::Open && d.blocking)
        {
            return Ok(false);
        }
        let Some(binding) = snapshot
            .mission
            .role_bindings
            .iter()
            .find(|r| r.role == Role::Lead)
            .map(|r| r.primary_binding_id.clone())
        else {
            return Ok(false);
        };
        let mut repair = pipeline::task(
            &snapshot.mission,
            &snapshot.tasks,
            TaskKind::Plan,
            Some(Role::Lead),
            format!("Repair failed tasks (cycle {})", cycle + 1),
            snapshot.mission.goal_ref.clone(),
            Some(binding.clone()),
        );
        if let Err(error) = self.validate_task_binding(&snapshot.mission, &repair, &binding) {
            if matches!(
                error.code,
                MissionErrorCode::PolicyDenied | MissionErrorCode::ModelUnavailable
            ) {
                return Ok(false);
            }
            return Err(error);
        }
        repair.contract.objective_ref = workflow::store_artifact(&self.artifacts, &snapshot.mission.id, "text/plain",
            b"Required tasks exhausted their attempt budget. Diagnose the retained failures and propose a complete repair plan. Retire each listed failed task and its obsolete dependents, and provide required replacement tasks linked with replacement_of. Preserve the original requirements and completed evidence. Assess the failure and external effects before proposing further work; do not blindly repeat failed actions. Stay within the mission's allowed roles, models, verification commands and workspace policy.")?;
        repair.repair_cycle = cycle + 1;
        repair.failure_repair_run_ids = sources.iter().map(|(_, r)| r.id.clone()).collect();
        let mut mission = snapshot.mission.clone();
        // Plan tasks can run during implementation. Independent work keeps its
        // place and its workspace while this Lead diagnoses the failure.
        if mission.phase != Phase::Implementing {
            mission.phase = Phase::Planning;
        }
        let mut changes = vec![Entity::Task(Box::new(repair))];
        for (task, _) in sources {
            changes.extend(failures::obsolete_failure_decisions(
                &mut mission,
                &snapshot.decisions,
                &task.id,
            ));
        }
        self.commit_actor(mission, "engine.required_failure_repair", changes, vec![])?;
        Ok(true)
    }

    pub(super) fn stop_exhausted_failure(
        &self,
        snapshot: &MissionEntities,
        failure_code: MissionErrorCode,
    ) -> Result<(), MissionRpcError> {
        let now = term_storage::time::now_iso8601();
        let mut mission = snapshot.mission.clone();
        mission.state = MissionState::Stopping;
        mission.failure_code = Some(failure_code);
        mission.open_decision_count = 0;
        let mut changes = Vec::new();
        let mut intents = Vec::new();
        for run in snapshot.runs.iter().filter(|r| r.holds_execution_slot()) {
            let (next, intent) = super::outbox::prepare_cancel(run, &now);
            changes.push(Entity::Run(Box::new(next)));
            intents.extend(intent);
        }
        for task in snapshot.tasks.iter().filter(|t| !t.state.is_terminal()) {
            let mut next = task.clone();
            next.state = TaskState::Cancelled;
            next.dispatch_after_unix_ms = None;
            next.updated_at = now.clone();
            if changes.iter().any(|e| matches!(e, Entity::Run(r) if Some(&r.id) == next.active_run_id.as_ref() && !r.holds_execution_slot())) {
                next.active_run_id = None;
            }
            changes.push(Entity::Task(Box::new(next)));
        }
        for decision in snapshot
            .decisions
            .iter()
            .filter(|d| d.state == DecisionState::Open)
        {
            let mut next = decision.clone();
            next.state = DecisionState::Obsolete;
            changes.push(Entity::Decision(Box::new(next)));
        }
        self.commit_actor_effects(
            mission,
            "engine.required_failure_exhausted",
            changes,
            intents,
            vec![],
        )
    }

    pub(super) fn failure_repair_context(
        &self,
        snapshot: &MissionEntities,
        task: &Task,
    ) -> Result<Option<Value>, MissionRpcError> {
        if task.failure_repair_run_ids.is_empty() {
            return Ok(None);
        }
        if task.kind != TaskKind::Plan || task.role != Some(Role::Lead) {
            return Err(MissionRpcError::new(
                MissionErrorCode::PolicyDenied,
                "failure diagnosis belongs to a Lead Plan",
            ));
        }
        let mut failures = Vec::new();
        for id in &task.failure_repair_run_ids {
            let (failed_task, run) = source(snapshot, id)?;
            let reference = run.result_ref.as_ref().ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::ResultInvalid,
                    "failure diagnostic is unavailable",
                )
            })?;
            let bytes = self
                .artifacts
                .read_mission_body(
                    &snapshot.mission.id,
                    reference,
                    self.limits.max_context_bytes,
                )
                .map_err(|(code, message)| MissionRpcError::new(code, message))?;
            let diagnostic = String::from_utf8(bytes).map_err(|_| {
                MissionRpcError::new(
                    MissionErrorCode::ResultInvalid,
                    "failure diagnostic is not UTF-8",
                )
            })?;
            failures.push(json!({"task":failed_task,"run_id":run.id,"failure_code":run.failure_code,"diagnostic":diagnostic,"workspace_id":run.workspace_id,"result_ref":reference}));
        }
        Ok(Some(
            json!({"cycle":task.repair_cycle,"failed_tasks":failures}),
        ))
    }

    pub(super) fn validate_failure_replacement(
        &self,
        snapshot: &MissionEntities,
        proposal: &PlanProposal,
    ) -> Result<(), MissionRpcError> {
        for repair in snapshot.tasks.iter().filter(|t| {
            !matches!(t.state, TaskState::Cancelled | TaskState::Superseded)
                && !t.failure_repair_run_ids.is_empty()
        }) {
            for id in &repair.failure_repair_run_ids {
                // Previously adopted repairs remain history. Only sources that
                // still need replacement constrain a new proposal.
                let old = snapshot
                    .runs
                    .iter()
                    .find(|r| &r.id == id)
                    .and_then(|r| snapshot.tasks.iter().find(|t| t.id == r.task_id));
                if old.is_some_and(|t| t.state == TaskState::Superseded) {
                    continue;
                }
                let (failed, _) = source(snapshot, id)?;
                if !proposal.retire_task_ids.contains(&failed.id)
                    || !proposal.tasks.iter().any(|t| {
                        t.required
                            && t.kind != TaskKind::Plan
                            && t.replacement_of.as_ref() == Some(&failed.id)
                            && failed
                                .contract
                                .requirement_ids
                                .iter()
                                .all(|id| t.contract.requirement_ids.contains(id))
                    })
                {
                    return Err(super::plan_repair::format_error("repair plan must retire each failed source task and provide a required replacement covering its requirements"));
                }
            }
        }
        Ok(())
    }
}
