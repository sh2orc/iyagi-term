//! Plan results become immutable, mission-owned proposals before any task
//! is scheduled. Both automatic adoption and the user RPC use these checks.
use std::collections::HashMap;

use serde_json::Value;
use term_contracts::mission::error::{MissionErrorCode, MissionRpcError};
use term_contracts::mission::rpc::{methods, MissionPlanApplyParams};
use term_contracts::mission::types::*;
use term_core::mission::plan::{
    validate_proposal_with_exclusions, PlanApplication, PlanCandidate, PlanError,
};
use term_storage::mission::types::ApplyMode;

use super::service::{ApplyPlan, Handled, MissionService};
use super::workflow::{load_entities, store_artifact, MissionEntities};

fn invalid(message: impl Into<String>) -> MissionRpcError {
    MissionRpcError::new(MissionErrorCode::ResultInvalid, message)
}

fn plan_error(error: PlanError) -> MissionRpcError {
    let code = match &error {
        PlanError::Graph(term_contracts::mission::plan::PlanGraphError::Cycle) => {
            MissionErrorCode::PlanCycle
        }
        PlanError::TooManyTasks { .. } | PlanError::TooManyRevisions { .. } => {
            MissionErrorCode::PlanLimit
        }
        PlanError::PolicyDenied(_)
        | PlanError::BindingNotAllowed(..)
        | PlanError::BadPath { .. } => MissionErrorCode::PolicyDenied,
        _ => MissionErrorCode::ResultInvalid,
    };
    let mut error = MissionRpcError::new(code, error.to_string());
    if matches!(
        code,
        MissionErrorCode::ResultInvalid | MissionErrorCode::PlanCycle
    ) {
        error.details.reason_code = Some(super::plan_repair::FORMAT_REJECTED.into());
    }
    error
}

impl MissionService {
    /// Mint IDs once, and resolve local names using the pure plan validator.
    pub fn resolve_provider_plan(
        &self,
        snapshot: &MissionEntities,
        based_on: u32,
        specs: &[ProviderTaskSpec],
        retire: &[Id],
        rationale: &str,
    ) -> Result<PlanProposal, MissionRpcError> {
        if based_on != snapshot.mission.plan_revision {
            return Err(invalid("proposal is based on an obsolete plan revision"));
        }
        if specs.len() + snapshot.tasks.len() > self.limits.max_tasks_per_mission {
            return Err(MissionRpcError::new(
                MissionErrorCode::PlanLimit,
                "task cap exceeded",
            ));
        }
        let mut refs = HashMap::new();
        let mut candidates = Vec::new();
        for spec in specs {
            let mut spec = spec.clone();
            if spec.objective_text.is_empty()
                || spec.objective_text.len() > self.limits.max_context_bytes
            {
                return Err(super::plan_repair::format_error(
                    "task objective is empty or exceeds the context limit",
                ));
            }
            if spec.kind != TaskKind::Verify && spec.binding_id.is_none() {
                spec.binding_id = snapshot
                    .mission
                    .role_bindings
                    .iter()
                    .find(|binding| Some(binding.role) == spec.role)
                    .map(|binding| binding.primary_binding_id.clone());
            }
            let id = Id::generate();
            refs.insert(
                id.clone(),
                store_artifact(
                    &self.artifacts,
                    &snapshot.mission.id,
                    "text/plain",
                    spec.objective_text.as_bytes(),
                )?,
            );
            candidates.push(PlanCandidate { id, spec });
        }
        let excluded = self.integration_exclusions(snapshot)?;
        let application = validate_proposal_with_exclusions(
            &snapshot.mission,
            &snapshot.tasks,
            &candidates,
            retire,
            &refs,
            &self.limits,
            &excluded.task_ids,
        )
        .map_err(plan_error)?;
        let rationale_ref = store_artifact(
            &self.artifacts,
            &snapshot.mission.id,
            "text/plain",
            rationale.as_bytes(),
        )?;
        let proposal = PlanProposal {
            id: Id::generate(),
            mission_id: snapshot.mission.id.clone(),
            based_on_plan_revision: based_on,
            tasks: application
                .tasks
                .into_iter()
                .map(|task| TaskSpec {
                    id: task.id,
                    title: task.title,
                    kind: task.kind,
                    role: task.role,
                    required: task.required,
                    parent_task_id: task.parent_task_id,
                    depends_on: task.depends_on,
                    contract: task.contract,
                    binding_id: task.binding_id,
                    replacement_of: task.replacement_of,
                })
                .collect(),
            retire_task_ids: retire.to_vec(),
            rationale_ref,
        };
        self.validate_resolved_plan(snapshot, &proposal)?;
        Ok(proposal)
    }

    pub(super) fn validate_resolved_plan(
        &self,
        snapshot: &MissionEntities,
        proposal: &PlanProposal,
    ) -> Result<PlanApplication, MissionRpcError> {
        let mission = &snapshot.mission;
        if proposal.mission_id != mission.id
            || proposal.based_on_plan_revision != mission.plan_revision
        {
            return Err(invalid(
                "proposal scope or plan revision does not match this mission",
            ));
        }
        self.validate_failure_replacement(snapshot, proposal)?;
        self.validate_exclusion_replacements(
            snapshot,
            Some(proposal),
            super::integration_exclusion::ExclusionCheck::Planning,
        )?;
        for task in snapshot
            .tasks
            .iter()
            .filter(|t| proposal.retire_task_ids.contains(&t.id))
        {
            if snapshot
                .runs
                .iter()
                .any(|r| r.task_id == task.id && r.holds_execution_slot())
                || (task.state == TaskState::Cancelled
                    && !super::failures::cancelled_task_is_settled(snapshot, task))
            {
                return Err(invalid(
                    "retirement requires confirmed execution termination",
                ));
            }
        }
        self.artifacts
            .read_mission_body(
                &mission.id,
                &proposal.rationale_ref,
                self.limits.max_context_bytes,
            )
            .map_err(|(code, message)| MissionRpcError::new(code, message))?;
        let bindings: Vec<Binding> = self
            .storage
            .mission_bindings()
            .map_err(Self::store_error)?
            .iter()
            .map(|value| self.observed_binding(value))
            .collect::<Result<_, _>>()?;
        let commands: Vec<VerificationCommand> = self
            .storage
            .mission_configs("verification")
            .map_err(Self::store_error)?
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .map_err(|e| invalid(format!("stored verification command: {e}")))?;
        let keys: HashMap<Id, String> = proposal
            .tasks
            .iter()
            .enumerate()
            .map(|(n, t)| (t.id.clone(), format!("task{n}")))
            .collect();
        let key = |id: &Id| keys.get(id).cloned().unwrap_or_else(|| id.to_string());
        let mut refs = HashMap::new();
        let mut candidates = Vec::new();
        for task in &proposal.tasks {
            if let Some(id) = &task.binding_id {
                if let Some(binding) = bindings.iter().find(|binding| &binding.id == id) {
                    self.require_binding_capability(binding, task.kind)?;
                }
                if !bindings
                    .iter()
                    .any(|binding| &binding.id == id && binding.enabled)
                {
                    return Err(MissionRpcError::new(
                        MissionErrorCode::ModelUnavailable,
                        "plan binding is missing or disabled",
                    ));
                }
            }
            for id in &task.contract.verification_ids {
                if !commands.iter().any(|command| {
                    &command.id == id && command.repository_id == mission.repository_id
                }) {
                    return Err(invalid(
                        "verification command does not belong to this repository",
                    ));
                }
            }
            self.artifacts
                .read_mission_body(
                    &mission.id,
                    &task.contract.objective_ref,
                    self.limits.max_context_bytes,
                )
                .map_err(|(code, message)| MissionRpcError::new(code, message))?;
            for id in &task.contract.input_artifact_ids {
                let row = self
                    .storage
                    .mission_artifact(id)
                    .map_err(Self::store_error)?;
                if !row.is_some_and(|row| {
                    row.mission_id.as_ref() == Some(&mission.id) && row.content_state == "available"
                }) {
                    return Err(invalid(
                        "plan input artifact is unavailable or outside this mission",
                    ));
                }
            }
            refs.insert(task.id.clone(), task.contract.objective_ref.clone());
            candidates.push(PlanCandidate {
                id: task.id.clone(),
                spec: ProviderTaskSpec {
                    local_key: key(&task.id),
                    title: task.title.clone(),
                    kind: task.kind,
                    role: task.role,
                    required: task.required,
                    parent_key: task.parent_task_id.as_ref().map(&key),
                    depends_on_keys: task.depends_on.iter().map(&key).collect(),
                    objective_text: String::new(),
                    requirement_ids: task.contract.requirement_ids.clone(),
                    input_artifact_ids: task.contract.input_artifact_ids.clone(),
                    allowed_paths: task.contract.allowed_paths.clone(),
                    expected_outputs: task.contract.expected_outputs.clone(),
                    verification_ids: task.contract.verification_ids.clone(),
                    specialty: task.contract.specialty.clone(),
                    binding_id: task.binding_id.clone(),
                    replacement_of: task.replacement_of.clone(),
                },
            });
        }
        let excluded = self.integration_exclusions(snapshot)?;
        validate_proposal_with_exclusions(
            mission,
            &snapshot.tasks,
            &candidates,
            &proposal.retire_task_ids,
            &refs,
            &self.limits,
            &excluded.task_ids,
        )
        .map_err(plan_error)
    }

    /// Assemble a single atomic adoption alongside an optional adapter final.
    pub(super) fn plan_entities(
        &self,
        snapshot: &MissionEntities,
        proposal: &PlanProposal,
    ) -> Result<(Mission, Vec<Entity>), MissionRpcError> {
        if !matches!(
            snapshot.mission.state,
            MissionState::Running | MissionState::Paused | MissionState::Pausing
        ) {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidState,
                "mission cannot adopt a plan in its current state",
            ));
        }
        let application = self.validate_resolved_plan(snapshot, proposal)?;
        let now = term_storage::time::now_iso8601();
        let repair_cycle = snapshot
            .tasks
            .iter()
            .map(|t| t.repair_cycle)
            .max()
            .unwrap_or(0);
        let mut upserts: Vec<Entity> = application
            .tasks
            .into_iter()
            .map(|mut task| {
                task.created_at = now.clone();
                task.updated_at = now.clone();
                task.repair_cycle = repair_cycle;
                Entity::Task(Box::new(task))
            })
            .collect();
        for task in snapshot
            .tasks
            .iter()
            .filter(|task| application.retired_task_ids.contains(&task.id))
        {
            let mut task = task.clone();
            task.state = TaskState::Superseded;
            task.updated_at = now.clone();
            upserts.push(Entity::Task(Box::new(task)));
        }
        let mut mission = snapshot.mission.clone();
        mission.plan_revision = application.next_plan_revision;
        mission.phase = Phase::Implementing;
        for decision in snapshot
            .decisions
            .iter()
            .filter(|d| d.state == DecisionState::Open && d.kind == DecisionKind::Plan)
        {
            let mut decision = decision.clone();
            decision.state = DecisionState::Obsolete;
            mission.open_decision_count = mission.open_decision_count.saturating_sub(1);
            upserts.push(Entity::Decision(Box::new(decision)));
        }
        Ok((mission, upserts))
    }

    pub(super) fn plan_apply(&self, raw: &Value) -> Result<Handled, MissionRpcError> {
        let params: MissionPlanApplyParams = Self::parse(raw)?;
        let snapshot = load_entities(&self.storage, &params.mission_id)?;
        let bytes = self
            .artifacts
            .read_mission_body(
                &params.mission_id,
                &params.proposal_ref,
                self.limits.max_context_bytes,
            )
            .map_err(|(code, message)| MissionRpcError::new(code, message))?;
        let proposal: PlanProposal = serde_json::from_slice(&bytes)
            .map_err(|e| invalid(format!("invalid proposal: {e}")))?;
        let (mut mission, mut upserts) = self.plan_entities(&snapshot, &proposal)?;
        mission.revision = term_contracts::ids::U64String::new(params.expected_revision.get() + 1)
            .expect("revision bound");
        mission.updated_at = term_storage::time::now_iso8601();
        upserts.insert(0, Entity::Mission(Box::new(mission)));
        let applied = self.apply(ApplyPlan {
            request_id: params.request_id,
            method: methods::MISSION_PLAN_APPLY.into(),
            params: raw.clone(),
            mission_id: params.mission_id,
            mode: ApplyMode::Mutate {
                expected_revision: params.expected_revision.get(),
            },
            event_type: MissionEventType::PlanApplied,
            upserts,
            deletes: vec![],
            outbox: vec![],
            adopt: vec![],
        })?;
        Ok(Self::mutation_handled(&applied))
    }
}
