//! User-owned policy changes and candidate-bound finding dispositions.
use super::{
    service::{ApplyPlan, Handled, MissionService},
    workflow,
};
use serde_json::Value;
use std::collections::HashSet;
use term_contracts::{
    ids::U64String,
    mission::{
        rpc::*,
        types::*,
        validation::{validate_policy, validate_role_bindings},
        MissionErrorCode, MissionRpcError,
    },
};
use term_storage::mission::types::ApplyMode;

fn invalid(message: &str) -> MissionRpcError {
    MissionRpcError::new(MissionErrorCode::InvalidState, message)
}
impl MissionService {
    pub(super) fn policy_update(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: MissionPolicyUpdateParams = Self::parse(params)?;
        let snapshot = workflow::load_entities(&self.storage, &params.mission_id)?;
        let mut mission = snapshot.mission;
        if !matches!(
            mission.state,
            MissionState::Draft | MissionState::Running | MissionState::Paused
        ) {
            return Err(invalid(
                "policy updates require a draft, running, or paused mission",
            ));
        }
        validate_policy(&params.policy, &self.ceiling)?;
        validate_role_bindings(&params.role_bindings, &params.policy)?;
        let mut roles = HashSet::new();
        if params.role_bindings.iter().any(|r| !roles.insert(r.role)) {
            return Err(invalid("role bindings must be unique"));
        }
        if mission.state != MissionState::Draft
            && (!roles.contains(&Role::Lead)
                || !roles.contains(&Role::Builder)
                || (params.policy.require_independent_review && !roles.contains(&Role::Reviewer)))
        {
            return Err(invalid(
                "the active mission requires its Lead, Builder and (with independent review) Reviewer bindings",
            ));
        }
        let bindings: Vec<Binding> = self
            .storage
            .mission_bindings()
            .map_err(Self::store_error)?
            .iter()
            .map(|value| self.observed_binding(value))
            .collect::<Result<_, _>>()?;
        for role in &params.role_bindings {
            for id in
                std::iter::once(&role.primary_binding_id).chain(role.fallback_binding_ids.iter())
            {
                if let Some(binding) = bindings.iter().find(|binding| &binding.id == id) {
                    self.require_binding_capability(
                        binding,
                        term_core::mission::capability::role_kind(role.role),
                    )?;
                }
                if !bindings.iter().any(|b| &b.id == id && b.enabled) {
                    return Err(invalid(
                        "role binding must name an enabled registered model",
                    ));
                }
            }
        }
        let commands: Vec<VerificationCommand> = self
            .storage
            .mission_configs("verification")
            .map_err(Self::store_error)?
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .map_err(|e| invalid(&format!("invalid stored verification: {e}")))?;
        if params.policy.allowed_verification_ids.iter().any(|id| {
            !commands
                .iter()
                .any(|c| &c.id == id && c.repository_id == mission.repository_id)
        }) {
            return Err(invalid(
                "verification allowlist must use this repository's registered commands",
            ));
        }
        if mission
            .requirements
            .iter()
            .flat_map(|r| &r.verification_ids)
            .any(|id| !params.policy.allowed_verification_ids.contains(id))
        {
            return Err(invalid(
                "policy cannot remove a command required by the original acceptance criteria",
            ));
        }
        let live: Vec<_> = snapshot
            .runs
            .iter()
            .filter(|r| r.holds_execution_slot())
            .collect();
        mission.active_time_ms = self.effective_active_time(&mission);
        let old = &mission.policy;
        let new = &params.policy;
        if !live.is_empty()
            && (live.len() > new.max_parallel_runs as usize
                || new.run_time_limit_ms < old.run_time_limit_ms
                || (old.allow_network && !new.allow_network)
                || (new.max_cost_usd_micros.is_some()
                    && (old.max_cost_usd_micros.is_none()
                        || new.max_cost_usd_micros < old.max_cost_usd_micros))
                || new.active_time_limit_ms.get() <= mission.active_time_ms.get()
                || new.max_automatic_starts < mission.automatic_start_count
                || live.iter().any(|r| {
                    r.attempt > new.max_attempts_per_task
                        || r.binding_snapshot
                            .as_ref()
                            .is_some_and(|b| !new.allowed_binding_ids.contains(&b.id))
                        || snapshot
                            .tasks
                            .iter()
                            .find(|t| t.id == r.task_id)
                            .is_some_and(|t| {
                                t.role
                                    .is_some_and(|role| !new.allowed_roles.contains(&role))
                                    || t.repair_cycle > new.max_repair_cycles
                                    || t.contract
                                        .verification_ids
                                        .iter()
                                        .any(|id| !new.allowed_verification_ids.contains(id))
                            })
                }))
        {
            return Err(invalid("the reduced policy conflicts with an owned run; pause and drain or cancel it first"));
        }
        let mut upserts = Vec::new();
        // Turning independent review off retires review work that never
        // started (no Run), like a plan retirement: the acceptance gate still
        // requires every required task to be succeeded or superseded, and no
        // one would run it. Running or finished reviews keep their state.
        let mut retired_reviews = HashSet::new();
        if old.require_independent_review && !new.require_independent_review {
            let now = term_storage::time::now_iso8601();
            for task in snapshot.tasks.iter().filter(|t| {
                t.kind == TaskKind::Review
                    && t.active_run_id.is_none()
                    && matches!(
                        t.state,
                        TaskState::Planned | TaskState::Ready | TaskState::Blocked
                    )
                    && !snapshot.runs.iter().any(|r| r.task_id == t.id)
            }) {
                let mut retired = task.clone();
                retired.state = TaskState::Superseded;
                retired.dispatch_after_unix_ms = None;
                retired.updated_at = now.clone();
                retired_reviews.insert(retired.id.clone());
                upserts.push(Entity::Task(Box::new(retired)));
            }
        }
        let expanded = new.max_automatic_starts > old.max_automatic_starts
            || new.active_time_limit_ms > old.active_time_limit_ms
            || new.max_repair_cycles > old.max_repair_cycles
            || new.max_attempts_per_task > old.max_attempts_per_task;
        if expanded
            && mission.automatic_start_count < new.max_automatic_starts
            && mission.active_time_ms < new.active_time_limit_ms
            && snapshot
                .tasks
                .iter()
                .map(|t| t.repair_cycle)
                .max()
                .unwrap_or(0)
                < new.max_repair_cycles
        {
            for mut decision in snapshot.decisions.into_iter().filter(|d| {
                d.kind == DecisionKind::Budget
                    && d.state == DecisionState::Open
                    && !super::costs::is_cost_decision(d)
            }) {
                decision.state = DecisionState::Obsolete;
                mission.open_decision_count = mission.open_decision_count.saturating_sub(1);
                upserts.push(Entity::Decision(Box::new(decision)));
            }
        }
        let held = snapshot.tasks.into_iter().filter(|t| {
            t.state == TaskState::Blocked
                && t.active_run_id.is_none()
                && !retired_reviews.contains(&t.id)
        });
        for mut task in held {
            let released = match task.blocked_code.as_deref() {
                Some("attempt_limit") => task.attempt_count < new.max_attempts_per_task,
                Some("automatic_start_limit") => {
                    mission.automatic_start_count < new.max_automatic_starts
                }
                Some("active_time_limit") => mission.active_time_ms < new.active_time_limit_ms,
                Some("binding_missing") => task.binding_id.as_ref().is_some_and(|id| {
                    new.allowed_binding_ids.contains(id)
                        && bindings.iter().any(|b| &b.id == id && b.enabled)
                }),
                _ => false,
            };
            if released {
                task.state = TaskState::Planned;
                task.blocked_code = None;
                upserts.push(Entity::Task(Box::new(task)));
            }
        }
        mission.policy = params.policy.clone();
        mission.role_bindings = params.role_bindings.clone();
        self.apply_user_configuration(
            methods::MISSION_POLICY_UPDATE,
            params.request_id.clone(),
            params.expected_revision.get(),
            serde_json::to_value(&params).expect("policy params"),
            mission,
            upserts,
        )
    }

    pub(super) fn finding_resolve(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: MissionFindingResolveParams = Self::parse(params)?;
        let snapshot = workflow::load_entities(&self.storage, &params.mission_id)?;
        if !matches!(
            snapshot.mission.state,
            MissionState::Running | MissionState::Paused
        ) {
            return Err(invalid(
                "findings can only be dismissed on an active mission",
            ));
        }
        let mut finding = snapshot
            .findings
            .iter()
            .find(|f| f.id == params.finding_id)
            .cloned()
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::NotFound,
                    "finding not found in this mission",
                )
            })?;
        if Some(&finding.candidate_id) != snapshot.mission.candidate_id.as_ref() {
            return Err(MissionRpcError::new(
                MissionErrorCode::StaleCandidate,
                "finding belongs to a previous candidate",
            ));
        }
        if finding.resolution != FindingResolution::Open {
            return Err(invalid("finding is already resolved"));
        }
        let reason = self
            .artifacts
            .read_mission_body(
                &params.mission_id,
                &params.reason_ref,
                self.limits.max_message_bytes,
            )
            .map_err(|(code, message)| MissionRpcError::new(code, message))?;
        if reason.is_empty() || !std::str::from_utf8(&reason).is_ok_and(|s| !s.trim().is_empty()) {
            return Err(invalid("a non-empty text reason is required"));
        }
        finding.resolution = FindingResolution::Dismissed;
        finding.resolution_ref = Some(params.reason_ref.clone());
        let still_blocking = snapshot.findings.iter().any(|f| {
            f.id != finding.id
                && f.candidate_id == finding.candidate_id
                && f.resolution == FindingResolution::Open
                && matches!(
                    f.severity,
                    FindingSeverity::Blocking | FindingSeverity::Major
                )
        });
        let mut mission = snapshot.mission;
        let mut upserts = vec![Entity::Finding(Box::new(finding))];
        if !still_blocking {
            for mut decision in snapshot.decisions.into_iter().filter(|d| {
                d.state == DecisionState::Open
                    && d.candidate_id == mission.candidate_id
                    && d.plan_revision == mission.plan_revision
                    && super::pipeline::is_review_repair_decision(d)
            }) {
                decision.state = DecisionState::Obsolete;
                mission.open_decision_count = mission.open_decision_count.saturating_sub(1);
                upserts.push(Entity::Decision(Box::new(decision)));
            }
        }
        self.apply_user_configuration(
            methods::MISSION_FINDING_RESOLVE,
            params.request_id.clone(),
            params.expected_revision.get(),
            serde_json::to_value(&params).expect("finding params"),
            mission,
            upserts,
        )
    }

    fn apply_user_configuration(
        &self,
        method: &str,
        request_id: Id,
        expected_revision: u64,
        params: Value,
        mut mission: Mission,
        mut upserts: Vec<Entity>,
    ) -> Result<Handled, MissionRpcError> {
        mission.revision = U64String::new(expected_revision + 1).expect("revision bound");
        mission.updated_at = term_storage::time::now_iso8601();
        let id = mission.id.clone();
        upserts.insert(0, Entity::Mission(Box::new(mission)));
        let applied = self.apply(ApplyPlan {
            request_id,
            method: method.into(),
            params,
            mission_id: id,
            mode: ApplyMode::Mutate { expected_revision },
            event_type: MissionEventType::Changed,
            upserts,
            deletes: vec![],
            outbox: vec![],
            adopt: vec![],
        })?;
        Ok(Self::mutation_handled(&applied))
    }
}
