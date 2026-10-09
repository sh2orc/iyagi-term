//! Reconcile local termination without rewriting an uncertain provider outcome.
//! An absent adapter/PID is never evidence that its descendants have ended.
use serde::{Deserialize, Serialize};
use term_contracts::{
    ids::U64String,
    mission::{types::*, MissionErrorCode, MissionRpcError},
};

use super::{
    service::MissionService,
    workflow::{self, MissionEntities},
};

pub(super) const RETRY_RECONCILED: &str = "retry_reconciled_task";
pub(super) const STOP_RECONCILED: &str = "stop_reconciled_mission";
pub(super) const ENDED: &str = "outcome_unknown_ended";
const PROOF_LIMIT: usize = 512 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminationProof {
    version: u32,
    kind: String,
    mission_id: Id,
    run_id: Id,
    fencing_token: U64String,
    observed_at: Timestamp,
    exec: ExecRecord,
}

pub(super) fn is_reconciled_decision(decision: &Decision) -> bool {
    decision.kind == DecisionKind::Recovery
        && decision.options.iter().any(|o| o.id == STOP_RECONCILED)
}

pub(super) fn uncertain(run: &Run) -> bool {
    matches!(run.state, RunState::Unknown | RunState::Interrupted)
}

pub(super) fn ended_exec<'a>(snapshot: &'a MissionEntities, run: &Run) -> Option<&'a ExecRecord> {
    let exec = snapshot
        .execs
        .iter()
        .find(|e| Some(&e.id) == run.exec_id.as_ref())?;
    (exec.mission_id == run.mission_id
        && exec.run_id == run.id
        && exec.state == ExecState::Exited
        && exec.ended_at.is_some()
        && !snapshot
            .execs
            .iter()
            .any(|e| e.run_id == run.id && e.id != exec.id))
    .then_some(exec)
}

impl MissionService {
    /// Covers both daemon restarts and a disconnect whose supervisor later
    /// commits group/stream cleanup. The supervisor is the termination authority;
    /// it only writes Exited after the owned group and output readers finish.
    pub(super) fn reconcile_unknown_runs(&self) -> Result<(), MissionRpcError> {
        let mut cursor = None;
        loop {
            let (missions, next) = self
                .storage
                .mission_list(cursor, 50, false)
                .map_err(Self::store_error)?;
            for mission in missions {
                let snapshot = workflow::load_entities(&self.storage, &mission.id)?;
                if let Err(error) = self.reconcile_termination_snapshot(snapshot) {
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

    fn reconcile_termination_snapshot(
        &self,
        mut snapshot: MissionEntities,
    ) -> Result<(), MissionRpcError> {
        let mut mission = snapshot.mission.clone();
        let mut upserts = vec![];
        for index in 0..snapshot.runs.len() {
            let mut run = snapshot.runs[index].clone();
            if !uncertain(&run) || run.reconciliation_ref.is_some() {
                continue;
            }
            let Some(exec) = ended_exec(&snapshot, &run) else {
                continue;
            };
            // Hash/size/scope verification excludes missing or substituted manifests.
            if self
                .artifacts
                .read_mission_body(&mission.id, &exec.launch_manifest_ref, 256 * 1024)
                .is_err()
            {
                continue;
            }
            let proof = TerminationProof {
                version: 1,
                kind: "durable_exec_exit".into(),
                mission_id: mission.id.clone(),
                run_id: run.id.clone(),
                fencing_token: run.fencing_token.clone(),
                observed_at: term_storage::time::now_iso8601(),
                exec: exec.clone(),
            };
            run.reconciliation_ref = Some(workflow::store_artifact(
                &self.artifacts,
                &mission.id,
                "application/json",
                &serde_json::to_vec(&proof).expect("serializable termination evidence"),
            )?);
            run.reconciliation_kind = Some(ReconciliationKind::ExecExited);
            // Keep the dirty directory quarantined. A retry receives a fresh
            // worktree; its previous output is never silently adopted or deleted.
            for workspace in &mut snapshot.workspaces {
                if Some(&workspace.id) == run.workspace_id.as_ref()
                    && workspace.writer_run_id.as_ref() == Some(&run.id)
                    && workspace.owned_by_daemon
                {
                    workspace.writer_run_id = None;
                    workspace.state = WorkspaceState::Quarantined;
                    upserts.push(Entity::Workspace(Box::new(workspace.clone())));
                }
            }
            for task in &mut snapshot.tasks {
                if task.id == run.task_id && task.active_run_id.as_ref() == Some(&run.id) {
                    task.active_run_id = None;
                    task.workspace_id = None;
                    if !task.state.is_terminal() {
                        task.state = TaskState::Blocked;
                        task.blocked_code = Some(ENDED.into());
                        task.dispatch_after_unix_ms = None;
                    }
                    task.updated_at = term_storage::time::now_iso8601();
                    upserts.push(Entity::Task(Box::new(task.clone())));
                }
            }
            snapshot.runs[index] = run.clone();
            upserts.push(Entity::Run(Box::new(run)));
        }
        let actionable = matches!(
            mission.state,
            MissionState::Running | MissionState::Paused | MissionState::Pausing
        );
        let mut retained = std::collections::HashSet::new();
        for decision in &snapshot.decisions {
            if decision.kind != DecisionKind::Recovery || decision.state != DecisionState::Open {
                continue;
            }
            let reconciled = snapshot.runs.iter().any(|r| {
                Some(&r.id) == decision.requesting_run_id.as_ref()
                    && uncertain(r)
                    && r.reconciliation_ref.is_some()
            });
            if !is_reconciled_decision(decision) && !reconciled {
                continue;
            }
            let current = snapshot.tasks.iter().find(|t| {
                decision.affected_task_ids == [t.id.clone()]
                    && self
                        .reconciled_task_run(&snapshot, t)
                        .is_some_and(|r| Some(&r.id) == decision.requesting_run_id.as_ref())
            });
            if actionable
                && current.is_some_and(|t| {
                    decision.options.iter().any(|o| o.id == RETRY_RECONCILED)
                        == (t.attempt_count < mission.policy.max_attempts_per_task)
                })
                && is_reconciled_decision(decision)
                && decision.plan_revision == mission.plan_revision
                && decision.candidate_id == mission.candidate_id
            {
                retained.insert(current.expect("current task").id.clone());
            } else {
                let mut next = decision.clone();
                next.state = DecisionState::Obsolete;
                mission.open_decision_count = mission.open_decision_count.saturating_sub(1);
                upserts.push(Entity::Decision(Box::new(next)));
            }
        }
        if actionable {
            for task in &snapshot.tasks {
                let Some(run) = self.reconciled_task_run(&snapshot, task) else {
                    continue;
                };
                if retained.contains(&task.id) {
                    continue;
                }
                let text = if task.is_internal_integration() {
                    format!("The local integration execution for '{}' has ended, but its outcome remains unknown. Inspect its quarantined output before rebuilding the recorded source candidates in a fresh workspace. The old Run and workspace remain unchanged. A new conflict requires a new integrator decision; uncertain output is not adopted.", task.title)
                } else {
                    format!("The local execution for '{}' has ended. Its provider outcome and external effects remain unknown. Inspect retained output before explicitly starting a new attempt. The old Run and quarantined worktree remain unchanged; uncertain messages are not resent.", task.title)
                };
                let question = workflow::store_artifact(
                    &self.artifacts,
                    &mission.id,
                    "text/plain",
                    text.as_bytes(),
                )?;
                let mut options = vec![];
                if task.attempt_count < mission.policy.max_attempts_per_task {
                    options.push(DecisionOption {
                        id: RETRY_RECONCILED.into(),
                        label: if task.is_internal_integration() {
                            "Rebuild integration while preserving quarantined output"
                        } else {
                            "Start a new attempt after reviewing effects"
                        }
                        .into(),
                    });
                }
                options.push(DecisionOption {
                    id: STOP_RECONCILED.into(),
                    label: "Stop mission".into(),
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
                upserts.push(Entity::Decision(Box::new(decision)));
            }
        }
        if !upserts.is_empty() {
            self.commit_actor(mission, "engine.execution_reconciled", upserts, vec![])?;
        }
        Ok(())
    }

    fn reconciled_task_run<'a>(
        &self,
        snapshot: &'a MissionEntities,
        task: &Task,
    ) -> Option<&'a Run> {
        if task.state != TaskState::Blocked
            || task.blocked_code.as_deref() != Some(ENDED)
            || task.active_run_id.is_some()
            || snapshot
                .runs
                .iter()
                .any(|r| r.task_id == task.id && r.holds_execution_slot())
        {
            return None;
        }
        let run = snapshot
            .runs
            .iter()
            .filter(|r| r.task_id == task.id)
            .max_by_key(|r| r.attempt)?;
        if !uncertain(run) {
            return None;
        }
        self.verified_termination(snapshot, run).then_some(run)
    }

    fn verified_termination(&self, snapshot: &MissionEntities, run: &Run) -> bool {
        self.termination_proof(snapshot, run).is_some()
    }

    fn termination_proof(&self, snapshot: &MissionEntities, run: &Run) -> Option<()> {
        // A user attestation releases local ownership like an observed exit,
        // but it is verified (and recorded) as its own kind.
        if run.reconciliation_kind == Some(ReconciliationKind::UserAttested) {
            return self.attested_termination(snapshot, run).then_some(());
        }
        let exec = ended_exec(snapshot, run)?;
        self.artifacts
            .read_mission_body(&run.mission_id, &exec.launch_manifest_ref, 256 * 1024)
            .ok()?;
        let bytes = self
            .artifacts
            .read_mission_body(
                &snapshot.mission.id,
                run.reconciliation_ref.as_ref()?,
                PROOF_LIMIT,
            )
            .ok()?;
        let proof: TerminationProof = serde_json::from_slice(&bytes).ok()?;
        (proof.version == 1
            && proof.kind == "durable_exec_exit"
            && proof.mission_id == run.mission_id
            && proof.run_id == run.id
            && proof.fencing_token == run.fencing_token
            && proof.exec == *exec)
            .then_some(())
    }

    pub(super) fn reconciled_decision_task<'a>(
        &self,
        snapshot: &'a MissionEntities,
        decision: &Decision,
    ) -> Result<&'a Task, MissionRpcError> {
        snapshot
            .tasks
            .iter()
            .find(|t| {
                decision.affected_task_ids == [t.id.clone()]
                    && self
                        .reconciled_task_run(snapshot, t)
                        .is_some_and(|r| Some(&r.id) == decision.requesting_run_id.as_ref())
            })
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::StaleDecision,
                    "this task no longer has verified termination evidence",
                )
            })
    }

    pub(super) fn prepare_reconciled_retry(
        &self,
        snapshot: &MissionEntities,
        decision: &Decision,
    ) -> Result<Task, MissionRpcError> {
        let task = self.reconciled_decision_task(snapshot, decision)?;
        if task.is_internal_integration() {
            let run = self
                .reconciled_task_run(snapshot, task)
                .expect("validated termination evidence");
            self.prepare_integration_restart(snapshot, task, run)
        } else {
            self.prepare_task_retry(snapshot, task, None)
        }
    }

    /// Acceptance acknowledges reviewed external effects, never changes the
    /// old outcome, and requires a completed replacement outside quarantine.
    pub(super) fn reviewed_reconciliations(
        &self,
        snapshot: &MissionEntities,
        ids: &[Id],
    ) -> Result<Vec<serde_json::Value>, MissionRpcError> {
        let mut seen = std::collections::HashSet::new();
        let mut evidence = Vec::new();
        for id in ids {
            let invalid = || {
                MissionRpcError::new(MissionErrorCode::InvalidState, "reconciled acceptance requires verified termination, explicit recovery and a completed replacement outside quarantine")
            };
            if !seen.insert(id) {
                return Err(MissionRpcError::new(
                    MissionErrorCode::InvalidArgument,
                    "duplicate reconciled run acknowledgement",
                ));
            }
            let run = snapshot
                .runs
                .iter()
                .find(|r| &r.id == id && uncertain(r))
                .ok_or_else(invalid)?;
            if !self.verified_termination(snapshot, run) || run.holds_execution_slot() {
                return Err(invalid());
            }
            let task = snapshot
                .tasks
                .iter()
                .find(|t| {
                    t.id == run.task_id
                        && t.state == TaskState::Succeeded
                        && t.active_run_id.is_none()
                })
                .ok_or_else(invalid)?;
            let workspace = snapshot
                .workspaces
                .iter()
                .find(|w| {
                    Some(&w.id) == run.workspace_id.as_ref()
                        && w.mission_id == run.mission_id
                        && w.owned_by_daemon
                        && w.state == WorkspaceState::Quarantined
                        && w.writer_run_id.is_none()
                })
                .ok_or_else(invalid)?;
            if snapshot
                .runs
                .iter()
                .any(|r| r.workspace_id.as_ref() == Some(&workspace.id) && r.holds_execution_slot())
            {
                return Err(invalid());
            }
            let replacement = snapshot
                .runs
                .iter()
                .find(|r| {
                    r.task_id == task.id
                        && r.attempt == task.attempt_count
                        && r.attempt > run.attempt
                        && r.state == RunState::Succeeded
                        && r.ended_at.is_some()
                        && r.workspace_id.is_some()
                        && r.workspace_id != run.workspace_id
                        && r.workspace_id == task.workspace_id
                        && ended_exec(snapshot, r).is_some()
                })
                .ok_or_else(invalid)?;
            let decision = snapshot
                .decisions
                .iter()
                .find(|d| {
                    is_reconciled_decision(d)
                        && d.state == DecisionState::Answered
                        && d.requesting_run_id.as_ref() == Some(id)
                        && d.affected_task_ids == [task.id.clone()]
                        && d.selected_option_id.as_deref() == Some(RETRY_RECONCILED)
                        && d.answered_at.is_some()
                })
                .ok_or_else(invalid)?;
            if !snapshot.workspaces.iter().any(|w| {
                Some(&w.id) == replacement.workspace_id.as_ref()
                    && w.mission_id == run.mission_id
                    && w.owned_by_daemon
                    && w.state == WorkspaceState::Retained
                    && w.writer_run_id.is_none()
            }) {
                return Err(invalid());
            }
            if !snapshot.messages.iter().any(|m| {
                Some(&m.id) == decision.answer_message_id.as_ref()
                    && m.mission_id == run.mission_id
                    && m.target_task_id.as_ref() == Some(&task.id)
                    && m.delivery == MessageDelivery::Delivered
            }) {
                return Err(invalid());
            }
            let candidate = snapshot
                .candidates
                .iter()
                .find(|c| Some(&c.id) == snapshot.mission.candidate_id.as_ref())
                .ok_or_else(invalid)?;
            if candidate.source_run_ids.contains(id) {
                return Err(invalid());
            }
            evidence.push(serde_json::json!({"run_id": id, "termination_ref": run.reconciliation_ref,
                "termination_kind": run.reconciliation_kind.unwrap_or(ReconciliationKind::ExecExited),
                "recovery_decision_id": decision.id, "replacement_run_id": replacement.id, "quarantined_workspace_id": workspace.id}));
        }
        Ok(evidence)
    }
}
