//! Candidate-bound verification and independent review scheduling.
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::json;
use term_contracts::mission::{types::*, MissionErrorCode, MissionRpcError};
use term_storage::mission::types::{OutboxState, OutboxUpdate, StoredOutbox};

use super::{
    service::MissionService,
    workflow::{self, MissionEntities},
};

pub(super) const STOP_REVIEW_REPAIR: &str = "stop_review_repair";

pub(super) fn is_review_repair_decision(decision: &Decision) -> bool {
    decision.kind == DecisionKind::Budget
        && decision.options.iter().any(|o| o.id == STOP_REVIEW_REPAIR)
}

pub(super) struct VerificationJob {
    pub mission_id: Id,
    pub command: VerificationCommand,
    pub candidate_id: Id,
    pub repository: PathBuf,
    pub workspace: Workspace,
    pub task_id: Id,
    pub run_id: Id,
    pub token: u64,
    pub requirement_ids: Vec<Id>,
}

fn error(code: MissionErrorCode, message: impl Into<String>) -> MissionRpcError {
    MissionRpcError::new(code, message)
}

pub(super) fn task(
    mission: &Mission,
    tasks: &[Task],
    kind: TaskKind,
    role: Option<Role>,
    title: String,
    objective: ArtifactRef,
    binding: Option<Id>,
) -> Task {
    let now = term_storage::time::now_iso8601();
    Task {
        id: Id::generate(),
        mission_id: mission.id.clone(),
        title,
        kind,
        role,
        state: TaskState::Planned,
        required: true,
        parent_task_id: None,
        depends_on: vec![],
        contract: TaskContract {
            objective_ref: objective,
            requirement_ids: mission.requirements.iter().map(|r| r.id.clone()).collect(),
            input_artifact_ids: vec![],
            allowed_paths: vec![],
            expected_outputs: vec![match kind {
                TaskKind::Review => ExpectedOutput::Review,
                TaskKind::Plan => ExpectedOutput::Report,
                _ => ExpectedOutput::Verification,
            }],
            verification_ids: vec![],
            specialty: None,
        },
        binding_id: binding,
        active_run_id: None,
        ordinal: tasks.iter().map(|t| t.ordinal).max().unwrap_or(0) + 1,
        attempt_count: 0,
        repair_cycle: 0,
        failure_repair_run_ids: vec![],
        integration: None,
        replacement_of: None,
        blocked_code: None,
        dispatch_after_unix_ms: None,
        workspace_id: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

impl MissionService {
    pub(super) fn prepare_verification(
        &self,
        intent: &StoredOutbox,
        root: &Path,
        supervised: bool,
    ) -> Result<Option<VerificationJob>, MissionRpcError> {
        let snapshot = workflow::load_entities(&self.storage, &intent.mission_id)?;
        if snapshot.mission.state != MissionState::Running
            || snapshot.mission.phase != Phase::Validating
        {
            return Ok(None);
        }
        let Some(mut run) = snapshot
            .runs
            .iter()
            .find(|r| Some(&r.id) == intent.run_id.as_ref())
            .cloned()
        else {
            return Ok(None);
        };
        if run.state != RunState::Prepared
            || run.dispatch_state != RunDispatchState::Unsent
            || run.fencing_token.get() != intent.fencing_token
        {
            return Ok(None);
        }
        let mut task = snapshot
            .tasks
            .iter()
            .find(|t| t.id == run.task_id)
            .cloned()
            .ok_or_else(|| error(MissionErrorCode::Internal, "verification task missing"))?;
        if task.state != TaskState::Running {
            return Ok(None);
        }
        if task.kind != TaskKind::Verify
            || task.binding_id.is_some()
            || task.contract.verification_ids.len() != 1
        {
            return Err(error(
                MissionErrorCode::ResultInvalid,
                "verify task must name one deterministic command",
            ));
        }
        let candidate = snapshot
            .candidates
            .iter()
            .find(|c| Some(&c.id) == snapshot.mission.candidate_id.as_ref())
            .ok_or_else(|| {
                error(
                    MissionErrorCode::StaleCandidate,
                    "verification has no candidate",
                )
            })?;
        let command = self
            .storage
            .mission_configs("verification")
            .map_err(Self::store_error)?
            .into_iter()
            .map(serde_json::from_value::<VerificationCommand>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| error(MissionErrorCode::Internal, e.to_string()))?
            .into_iter()
            .find(|c| {
                c.id == task.contract.verification_ids[0]
                    && c.repository_id == snapshot.mission.repository_id
            })
            .ok_or_else(|| {
                error(
                    MissionErrorCode::NotFound,
                    "verification command missing from this repository",
                )
            })?;
        if !snapshot
            .mission
            .policy
            .allowed_verification_ids
            .contains(&command.id)
        {
            return Err(error(
                MissionErrorCode::PolicyDenied,
                "verification command is outside the allowlist",
            ));
        }
        if command.env_profile_ref.is_some() {
            return Err(error(
                MissionErrorCode::CapabilityUnsupported,
                "verification environment profile is not configured",
            ));
        }
        let id = Id::generate();
        let path = root
            .join(snapshot.mission.id.as_str())
            .join("workspaces")
            .join(id.as_str());
        let workspace = Workspace {
            id: id.clone(),
            mission_id: snapshot.mission.id.clone(),
            path: path.to_string_lossy().into_owned(),
            kind: WorkspaceKind::Verification,
            base_oid: candidate.commit_oid.clone(),
            head_oid: candidate.commit_oid.clone(),
            writer_run_id: Some(run.id.clone()),
            lease_token: run.fencing_token.clone(),
            state: WorkspaceState::Preparing,
            owned_by_daemon: true,
        };
        if !task
            .contract
            .input_artifact_ids
            .contains(&candidate.manifest_ref.id)
        {
            task.contract
                .input_artifact_ids
                .push(candidate.manifest_ref.id.clone());
        }
        run.workspace_id = Some(id.clone());
        run.state = if supervised {
            RunState::Starting
        } else {
            RunState::Running
        };
        run.dispatch_state = RunDispatchState::MayHaveSent;
        run.started_at = Some(term_storage::time::now_iso8601());
        task.workspace_id = Some(id);
        let job = VerificationJob {
            mission_id: snapshot.mission.id.clone(),
            command,
            candidate_id: candidate.id.clone(),
            repository: PathBuf::from(&snapshot.mission.repository_path),
            workspace: workspace.clone(),
            task_id: task.id.clone(),
            run_id: run.id.clone(),
            token: run.fencing_token.get(),
            requirement_ids: task.contract.requirement_ids.clone(),
        };
        self.commit_actor(
            snapshot.mission,
            "engine.claim_verify",
            vec![
                Entity::Task(Box::new(task)),
                Entity::Run(Box::new(run)),
                Entity::Workspace(Box::new(workspace)),
            ],
            vec![OutboxUpdate {
                id: intent.id.clone(),
                expected_state: OutboxState::Prepared,
                state: OutboxState::Sending,
                fencing_token: intent.fencing_token,
            }],
        )?;
        Ok(Some(job))
    }

    pub(super) fn advance_workflows(
        &self,
        root: &Path,
        supervised: bool,
    ) -> Result<(), MissionRpcError> {
        let mut cursor = None;
        loop {
            let (missions, next) = self
                .storage
                .mission_list(cursor, 50, false)
                .map_err(Self::store_error)?;
            for mission in missions {
                if mission.state != MissionState::Running {
                    continue;
                }
                let snapshot = workflow::load_entities(&self.storage, &mission.id)?;
                if snapshot
                    .decisions
                    .iter()
                    .any(|d| d.state == DecisionState::Open && d.blocking)
                {
                    continue;
                }
                let result = self.advance_workflow(snapshot, root, supervised);
                if let Err(e) = result {
                    if e.code != MissionErrorCode::RevisionConflict {
                        return Err(e);
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

    fn advance_workflow(
        &self,
        snapshot: MissionEntities,
        root: &Path,
        supervised: bool,
    ) -> Result<(), MissionRpcError> {
        let mission = &snapshot.mission;
        let excluded = self.integration_exclusions(&snapshot)?;
        // Cancelling execution does not waive a required contract. Wait for an
        // explicit retry or a validated replacement plan, including before
        // generating verification/review tasks that would undo the cancellation.
        if snapshot
            .tasks
            .iter()
            .any(|t| t.required && t.state == TaskState::Cancelled)
        {
            return Ok(());
        }
        match mission.phase {
            Phase::Implementing if !snapshot.runs.iter().any(|r| r.holds_execution_slot()) => {
                let active: Vec<&Task> = snapshot
                    .tasks
                    .iter()
                    .filter(|t| {
                        !excluded.task_ids.contains(&t.id)
                            && !matches!(t.state, TaskState::Superseded | TaskState::Cancelled)
                            && !matches!(t.kind, TaskKind::Verify | TaskKind::Review)
                    })
                    .collect();
                if active.iter().any(|t| {
                    !t.state.is_terminal() || (t.required && t.state != TaskState::Succeeded)
                }) {
                    return Ok(());
                }
                self.validate_exclusion_replacements(
                    &snapshot,
                    None,
                    super::integration_exclusion::ExclusionCheck::Integrating,
                )?;
                // Include every succeeded writer, in the plan's dependency order.
                let mut writers: Vec<&Task> = active
                    .into_iter()
                    .filter(|t| {
                        matches!(
                            t.kind,
                            TaskKind::Implement
                                | TaskKind::TestAuthor
                                | TaskKind::Document
                                | TaskKind::Integrate
                        ) && t.state == TaskState::Succeeded
                            && !t.is_internal_integration()
                    })
                    .collect();
                writers.sort_by_key(|t| t.ordinal);
                let mut sources = Vec::new();
                for task in writers {
                    let run = snapshot
                        .runs
                        .iter()
                        .filter(|r| r.task_id == task.id && r.state == RunState::Succeeded)
                        .max_by_key(|r| r.attempt)
                        .ok_or_else(|| {
                            error(
                                MissionErrorCode::ResultInvalid,
                                "succeeded writer has no successful run",
                            )
                        })?;
                    let candidate = snapshot
                        .candidates
                        .iter()
                        .find(|c| c.revision == 0 && c.source_run_ids.contains(&run.id))
                        .ok_or_else(|| {
                            error(
                                MissionErrorCode::ResultInvalid,
                                "writer result has no captured candidate",
                            )
                        })?;
                    sources.push((candidate.id.clone(), candidate.source_run_ids.clone()));
                }
                if supervised {
                    return self.schedule_integration(&snapshot, &sources);
                }
                let path = root
                    .join(mission.id.as_str())
                    .join("workspaces")
                    .join(Id::generate().as_str());
                std::fs::create_dir_all(path.parent().expect("workspaces parent"))
                    .map_err(|e| error(MissionErrorCode::Internal, e.to_string()))?;
                workflow::integrate_and_mint(
                    self,
                    &self.artifacts,
                    &mission.id,
                    Path::new(&mission.repository_path),
                    &path,
                    &sources,
                )?;
            }
            Phase::Validating => {
                let candidate_id = mission.candidate_id.as_ref().ok_or_else(|| {
                    error(
                        MissionErrorCode::StaleCandidate,
                        "validation needs a candidate",
                    )
                })?;
                let required: HashSet<Id> = mission
                    .requirements
                    .iter()
                    .flat_map(|r| r.verification_ids.iter().cloned())
                    .collect();
                let commands: Vec<VerificationCommand> = self
                    .storage
                    .mission_configs("verification")
                    .map_err(Self::store_error)?
                    .into_iter()
                    .map(serde_json::from_value)
                    .collect::<Result<_, _>>()
                    .map_err(|e| error(MissionErrorCode::Internal, e.to_string()))?;
                let mut all_passed = true;
                for id in required {
                    let existing = snapshot.tasks.iter().find(|t| {
                        t.kind == TaskKind::Verify
                            && !matches!(t.state, TaskState::Superseded | TaskState::Cancelled)
                            && t.contract.verification_ids.contains(&id)
                            && (t.attempt_count == 0
                                || snapshot.candidates.iter().any(|c| {
                                    c.id == *candidate_id
                                        && t.contract
                                            .input_artifact_ids
                                            .contains(&c.manifest_ref.id)
                                }))
                    });
                    if let Some(existing) = existing {
                        if snapshot.verifications.iter().any(|v| {
                            v.candidate_id == *candidate_id
                                && v.task_id == existing.id
                                && v.status == VerificationStatus::Passed
                        }) {
                            continue;
                        }
                        if existing.state == TaskState::Failed {
                            return self.request_repair(&snapshot,"Verification failed on the current candidate. Diagnose the retained logs and propose replacement tasks; retire failed or obsolete tasks without discarding evidence.");
                        }
                        all_passed = false;
                        continue;
                    }
                    let command = commands
                        .iter()
                        .find(|c| c.id == id && c.repository_id == mission.repository_id)
                        .ok_or_else(|| {
                            error(
                                MissionErrorCode::NotFound,
                                "required verification command is missing",
                            )
                        })?;
                    let objective = workflow::store_artifact(
                        &self.artifacts,
                        &mission.id,
                        "application/json",
                        &serde_json::to_vec(command).expect("command"),
                    )?;
                    let mut new = task(
                        mission,
                        &snapshot.tasks,
                        TaskKind::Verify,
                        None,
                        format!("Verify: {}", command.title),
                        objective,
                        None,
                    );
                    new.contract.verification_ids = vec![id.clone()];
                    new.contract.requirement_ids = mission
                        .requirements
                        .iter()
                        .filter(|r| r.verification_ids.contains(&id))
                        .map(|r| r.id.clone())
                        .collect();
                    self.commit_actor(
                        mission.clone(),
                        "engine.verify_task",
                        vec![Entity::Task(Box::new(new))],
                        vec![],
                    )?;
                    return Ok(()); // Recompute after the committed revision.
                }
                if all_passed
                    && snapshot
                        .tasks
                        .iter()
                        .filter(|t| t.kind == TaskKind::Verify)
                        .all(|t| t.state.is_terminal())
                {
                    let mut mission = mission.clone();
                    mission.phase = Phase::Reviewing;
                    self.commit_actor(mission, "engine.verification_complete", vec![], vec![])?;
                }
            }
            Phase::Reviewing => {
                if snapshot.runs.iter().any(|r| r.holds_execution_slot()) {
                    return Ok(());
                }
                if snapshot.findings.iter().any(|f| {
                    Some(&f.candidate_id) == mission.candidate_id.as_ref()
                        && f.resolution == FindingResolution::Open
                        && matches!(
                            f.severity,
                            FindingSeverity::Blocking | FindingSeverity::Major
                        )
                }) {
                    return self.request_repair(&snapshot,"Independent review found blocking or major issues. Propose a repair plan tied to these findings; preserve the original requirements and previous evidence.");
                }
                let reviewed = !mission.policy.require_independent_review
                    || workflow::current_review_complete(self, &snapshot)?;
                let required_tasks_finished = snapshot.tasks.iter().all(|task| {
                    !task.required
                        || matches!(task.state, TaskState::Succeeded | TaskState::Superseded)
                });
                if reviewed && required_tasks_finished {
                    let mut mission = mission.clone();
                    mission.phase = Phase::AwaitingAcceptance;
                    self.commit_actor(mission, "engine.ready_for_acceptance", vec![], vec![])?;
                    return Ok(());
                }
                if reviewed
                    || snapshot
                        .tasks
                        .iter()
                        .any(|t| t.kind == TaskKind::Review && !t.state.is_terminal())
                {
                    return Ok(());
                }
                let binding = mission
                    .role_bindings
                    .iter()
                    .find(|r| r.role == Role::Reviewer)
                    .map(|r| r.primary_binding_id.clone())
                    .ok_or_else(|| {
                        error(
                            MissionErrorCode::ModelUnavailable,
                            "configure a Reviewer role binding",
                        )
                    })?;
                let objective=workflow::store_artifact(&self.artifacts,&mission.id,"text/plain",b"Independently inspect the immutable candidate against every requirement. Read the diff, code, and verification evidence. Return a Review with concrete findings, or an empty findings list when satisfied.")?;
                let new = task(
                    mission,
                    &snapshot.tasks,
                    TaskKind::Review,
                    Some(Role::Reviewer),
                    "Independent review".into(),
                    objective,
                    Some(binding),
                );
                self.commit_actor(
                    mission.clone(),
                    "engine.review_task",
                    vec![Entity::Task(Box::new(new))],
                    vec![],
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    fn request_repair(
        &self,
        snapshot: &MissionEntities,
        reason: &str,
    ) -> Result<(), MissionRpcError> {
        let mission = &snapshot.mission;
        if snapshot.runs.iter().any(|r| r.holds_execution_slot()) {
            return Ok(());
        }
        let cycle = snapshot
            .tasks
            .iter()
            .map(|t| t.repair_cycle)
            .max()
            .unwrap_or(0)
            + 1;
        if cycle > mission.policy.max_repair_cycles
            || snapshot.tasks.len() >= self.limits.max_tasks_per_mission
        {
            // Review findings can still be dismissed with a retained user
            // reason. Mandatory verification failures have no such override.
            if mission.phase == Phase::Reviewing {
                let question = workflow::store_artifact(&self.artifacts, &mission.id, "text/plain",
                    b"Automatic review repair is exhausted. Inspect the current findings and dismiss them with reasons if justified, increase the repair limit in mission settings, or stop the mission.")?;
                let (_, decision) = super::engine::new_decision(
                    mission,
                    DecisionKind::Budget,
                    question,
                    vec![
                        DecisionOption {
                            id: STOP_REVIEW_REPAIR.into(),
                            label: "Stop mission".into(),
                        },
                        // UI route to the repair-limit policy editor; not answerable.
                        DecisionOption {
                            id: super::engine::ADJUST_LIMITS.into(),
                            label: "Adjust the repair limit, then continue".into(),
                        },
                    ],
                    vec![],
                    true,
                    None,
                );
                let mut next = mission.clone();
                next.open_decision_count += 1;
                return self.commit_actor(
                    next,
                    "engine.review_repair_limit",
                    vec![Entity::Decision(Box::new(decision))],
                    vec![],
                );
            }
            return self.stop_exhausted_failure(snapshot, MissionErrorCode::PlanLimit);
        }
        let binding = mission
            .role_bindings
            .iter()
            .find(|r| r.role == Role::Lead)
            .map(|r| r.primary_binding_id.clone())
            .ok_or_else(|| {
                error(
                    MissionErrorCode::ModelUnavailable,
                    "Lead binding is missing",
                )
            })?;
        let objective=workflow::store_artifact(&self.artifacts,&mission.id,"application/json",json!({"instruction":reason,"candidate_id":mission.candidate_id,"findings":snapshot.findings,"verifications":snapshot.verifications,"repair_cycle":cycle}).to_string().as_bytes())?;
        let mut new = task(
            mission,
            &snapshot.tasks,
            TaskKind::Plan,
            Some(Role::Lead),
            format!("Repair plan {cycle}"),
            objective,
            Some(binding),
        );
        new.repair_cycle = cycle;
        let mut next = mission.clone();
        next.phase = Phase::Planning;
        self.commit_actor(
            next,
            "engine.repair_plan",
            vec![Entity::Task(Box::new(new))],
            vec![],
        )
    }
}
