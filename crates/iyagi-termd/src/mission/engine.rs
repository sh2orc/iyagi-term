//! Mission engine dispatch (ticket O11): bridges the pure core decisions to
//! storage transactions. Runs on the daemon's mission tick: materialize
//! running missions, compute readiness, apply the scheduler's choices as
//! prepared Run + outbox rows in one transaction per dispatch, and apply
//! adapter terminal outcomes back onto tasks. Messages and decisions (O12)
//! share the same transaction discipline.

use std::collections::HashMap;

use serde_json::json;
use sha2::{Digest, Sha256};
use term_contracts::ids::U64String;
use term_contracts::mission::error::{MissionErrorCode, MissionErrorDetails, MissionRpcError};
use term_contracts::mission::types::{
    Decision, DecisionKind, DecisionOption, DecisionState, Entity, Id, Message, MessageDelivery,
    MessageRole, Mission, MissionEventType, Run, RunDispatchState, RunState, Task, TaskState,
};
use term_core::mission::scheduler::{self, CapLimits, MissionSlice, SkipReason};
use term_storage::mission::types::OutboxOperation;
use term_storage::mission::types::{ApplyMissionTransition, ApplyMode, OutboxIntent};

use super::service::MissionService;

/// Budget-decision option that only routes the UI to the policy editor.
/// Answering it would change nothing, so decision.answer rejects it.
pub(super) const ADJUST_LIMITS: &str = "adjust_limits";

pub(super) fn phase_allows(mission: &Mission, task: &Task) -> bool {
    use term_contracts::mission::types::{Phase, TaskKind};
    if task.is_internal_integration() {
        return mission.phase == Phase::Integrating;
    }
    match task.kind {
        TaskKind::Verify => mission.phase == Phase::Validating && mission.candidate_id.is_some(),
        TaskKind::Review => mission.phase == Phase::Reviewing && mission.candidate_id.is_some(),
        TaskKind::Plan => matches!(mission.phase, Phase::Planning | Phase::Implementing),
        _ => mission.phase == Phase::Implementing,
    }
}

impl MissionService {
    /// Schedule against all missions, including slots owned by pausing and
    /// stopping missions. The guard prevents two daemon ticks reserving the
    /// same global capacity from independent snapshots.
    pub fn dispatch_tick(&self) -> Result<usize, MissionRpcError> {
        self.checkpoint_time()?;
        self.reconcile_unknown_runs()?;
        self.reconcile_cost_blocks()?;
        self.reconcile_capability_blocks()?;
        let _guard = self
            .dispatch_guard
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        self.reconcile_rate_limits()?;
        let mut cursor = None;
        let mut snapshots = Vec::new();
        loop {
            let (missions, next) = self
                .storage
                .mission_list(cursor, 50, false)
                .map_err(Self::store_error)?;
            for mission in missions {
                let snapshot = super::workflow::load_entities(&self.storage, &mission.id)?;
                snapshots.push(snapshot);
            }
            match next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        // Stable identity order survives updated_at changes caused by our
        // own commits. Rotate between ticks so a single free slot is fair.
        snapshots.sort_by(|a, b| a.mission.id.as_str().cmp(b.mission.id.as_str()));
        let len = snapshots.len();
        if len > 0 {
            let start = self
                .dispatch_cursor
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                % len;
            snapshots.rotate_left(start);
        }
        let mut slices = Vec::new();
        for snapshot in &snapshots {
            let states: HashMap<Id, TaskState> = snapshot
                .tasks
                .iter()
                .map(|task| (task.id.clone(), task.state))
                .collect();
            let blocking: Vec<Id> = snapshot
                .decisions
                .iter()
                .filter(|d| d.state == DecisionState::Open && d.blocking)
                .map(|d| d.id.clone())
                .collect();
            let mut ready: Vec<&Task> = snapshot
                .tasks
                .iter()
                .filter(|task| {
                    phase_allows(&snapshot.mission, task)
                        && scheduler::task_is_ready(task, &states, &blocking)
                })
                .collect();
            ready.sort_by_key(|task| task.ordinal);
            slices.push(MissionSlice {
                mission_id: snapshot.mission.id.clone(),
                state: snapshot.mission.state,
                policy: &snapshot.mission.policy,
                ready_tasks: ready,
                live_runs: snapshot
                    .runs
                    .iter()
                    .filter(|run| run.holds_execution_slot())
                    .map(|run| {
                        (
                            run.id.clone(),
                            run.binding_snapshot
                                .as_ref()
                                .map(|b| b.id.clone())
                                .or_else(|| {
                                    snapshot
                                        .tasks
                                        .iter()
                                        .find(|t| t.id == run.task_id)
                                        .and_then(|t| t.execution_binding_id().cloned())
                                }),
                        )
                    })
                    .collect(),
                open_blocking_decisions: blocking,
                fairness_cursor: 0,
            });
        }
        let mut dispatched = 0;
        for (choice, verdict) in scheduler::select_dispatches(&mut slices, CapLimits::default()) {
            let result = match verdict {
                scheduler::DispatchVerdict::Dispatch(_) => self.commit_dispatch(&choice),
                scheduler::DispatchVerdict::Skip(SkipReason::MissingBinding) => self
                    .block_task(&choice.mission_id, &choice.task_id, "binding_missing")
                    .map(|_| false),
                scheduler::DispatchVerdict::Skip(SkipReason::AttemptLimit) => self
                    .block_task(&choice.mission_id, &choice.task_id, "attempt_limit")
                    .map(|_| false),
                // Capacity shortages leave tasks ready; do not generate a
                // persisted no-op event on every 250 ms tick.
                scheduler::DispatchVerdict::Skip(_) => Ok(false),
            };
            match result {
                Ok(true) => dispatched += 1,
                Ok(false) => {}
                // A user mutation won the CAS. Recompute on the next tick;
                // never replay a scheduling choice against a newer contract.
                Err(error) if error.code == MissionErrorCode::RevisionConflict => {}
                Err(error) => return Err(error),
            }
        }
        Ok(dispatched)
    }

    /// Re-read after every prior dispatch, then commit the complete binding,
    /// task, Run, counter, and start intent atomically.
    fn commit_dispatch(&self, choice: &scheduler::DispatchChoice) -> Result<bool, MissionRpcError> {
        let snapshot = super::workflow::load_entities(&self.storage, &choice.mission_id)?;
        let mission = &snapshot.mission;
        if mission.state != term_contracts::mission::types::MissionState::Running {
            return Ok(false);
        }
        let Some(task) = snapshot.tasks.iter().find(|task| task.id == choice.task_id) else {
            return Ok(false);
        };
        let states = snapshot
            .tasks
            .iter()
            .map(|t| (t.id.clone(), t.state))
            .collect();
        let blocking: Vec<Id> = snapshot
            .decisions
            .iter()
            .filter(|d| d.state == DecisionState::Open && d.blocking)
            .map(|d| d.id.clone())
            .collect();
        if !phase_allows(mission, task)
            || !scheduler::task_is_ready(task, &states, &blocking)
            || task.attempt_count.saturating_add(1) != choice.attempt
            || task.execution_binding_id() != choice.binding_id.as_ref()
            || snapshot
                .runs
                .iter()
                .any(|run| run.task_id == task.id && run.holds_execution_slot())
        {
            return Ok(false);
        }
        let blocked = if mission.automatic_start_count >= mission.policy.max_automatic_starts {
            Some("automatic_start_limit")
        } else if self.effective_active_time(mission) >= mission.policy.active_time_limit_ms {
            Some("active_time_limit")
        } else {
            None
        };
        if let Some(code) = blocked {
            self.block_task(&mission.id, &task.id, code)?;
            return Ok(false);
        }
        let binding = if let Some(binding_id) = &choice.binding_id {
            let binding = self
                .storage
                .mission_bindings()
                .map_err(Self::store_error)?
                .into_iter()
                .map(|document| self.observed_binding(&document))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .find(|binding| &binding.id == binding_id);
            match binding {
                Some(binding)
                    if binding.enabled
                        && mission.policy.allowed_binding_ids.contains(binding_id) =>
                {
                    Some(binding)
                }
                _ => {
                    self.block_task(&mission.id, &task.id, "binding_unavailable")?;
                    return Ok(false);
                }
            }
        } else {
            None
        };
        if let Some(binding) = binding.as_ref() {
            if let Some(missing) = Self::missing_binding_capability(binding, task.kind) {
                self.block_task(&mission.id, &task.id, &format!("capability_{missing}"))?;
                return Ok(false);
            }
            if let Some(until) = self.rate_limit_deadline(binding)? {
                self.hold_for_rate_limit(&snapshot, task, until)?;
                return Ok(false);
            }
        }
        if let Err(reason) = term_core::mission::budget::cost_admission(
            &mission.policy,
            &snapshot.runs,
            binding.as_ref(),
        ) {
            self.block_cost(&snapshot, task, reason)?;
            return Ok(false);
        }
        let run_id = Id::generate();
        let now = self.now_iso();
        let mut next_task = task.clone();
        next_task.state = TaskState::Running;
        next_task.blocked_code = None;
        next_task.dispatch_after_unix_ms = None;
        next_task.attempt_count = choice.attempt;
        next_task.active_run_id = Some(run_id.clone());
        if !task.is_internal_integration() {
            next_task.workspace_id = None;
        }
        next_task.updated_at = now.clone();
        let mut next_mission = mission.clone();
        next_mission.revision =
            U64String::new(mission.revision.get() + 1).expect("fits SQLite bound");
        next_mission.automatic_start_count += 1;
        next_mission.updated_at = now.clone();
        let run = Run {
            id: run_id.clone(),
            mission_id: mission.id.clone(),
            task_id: task.id.clone(),
            attempt: choice.attempt,
            state: RunState::Prepared,
            requested_model: binding.as_ref().map(|b| b.model_id.clone()),
            binding_snapshot: binding,
            observed_model: None,
            provider_session_id: None,
            provider_turn_id: None,
            exec_id: None,
            pty_session_id: None,
            workspace_id: None,
            fencing_token: U64String::new(mission.revision.get() + 1).expect("fits SQLite bound"),
            dispatch_state: RunDispatchState::Unsent,
            context_ref: task.contract.objective_ref.clone(),
            result_ref: None,
            usage: term_contracts::mission::validation::unknown_usage(),
            last_activity_at: None,
            active_time_ms: U64String::new(0).expect("fits SQLite bound"),
            started_at: None,
            ended_at: None,
            failure_code: None,
            reconciliation_ref: None,
            reconciliation_kind: None,
            rate_limit: None,
            retry_evidence: None,
        };
        self.apply_timed_transition(ApplyMissionTransition {
            request_id: Id::generate(),
            method: "engine.dispatch".into(),
            fingerprint: fingerprint_hex(&format!("{}:{}:{}", mission.id, task.id, choice.attempt)),
            mission_id: mission.id.clone(),
            mode: ApplyMode::Mutate {
                expected_revision: mission.revision.get(),
            },
            transaction_id: Id::generate(),
            event_type: MissionEventType::RunDispatched,
            upserts: vec![
                Entity::Mission(Box::new(next_mission)),
                Entity::Task(Box::new(next_task)),
                Entity::Run(Box::new(run)),
            ],
            deletes: Vec::new(),
            changes_ref: None,
            outbox: vec![OutboxIntent {
                id: Id::generate(),
                mission_id: mission.id.clone(),
                run_id: Some(run_id),
                operation: if task.kind == term_contracts::mission::types::TaskKind::Verify {
                    OutboxOperation::Verify
                } else {
                    OutboxOperation::Start
                },
                dedupe_key: format!("{}/{}/{}/start", mission.id, task.id, choice.attempt),
                fencing_token: mission.revision.get() + 1,
                payload: json!({ "task_id": task.id, "binding_id": choice.binding_id }),
                created_at: now.clone(),
            }],
            outbox_updates: Vec::new(),
            adopt_staged_artifacts: Vec::new(),
            created_at: now,
        })
        .map_err(Self::store_error)?;
        Ok(true)
    }

    fn block_task(&self, mission_id: &Id, task_id: &Id, code: &str) -> Result<(), MissionRpcError> {
        let snapshot = super::workflow::load_entities(&self.storage, mission_id)?;
        let Some(mut task) = snapshot.tasks.into_iter().find(|t| &t.id == task_id) else {
            return Ok(());
        };
        if task.active_run_id.is_some()
            || !matches!(
                task.state,
                TaskState::Ready | TaskState::Planned | TaskState::Blocked
            )
            || (task.state == TaskState::Blocked && task.blocked_code.as_deref() == Some(code))
        {
            return Ok(());
        }
        task.state = TaskState::Blocked;
        task.blocked_code = Some(code.into());
        task.updated_at = self.now_iso();
        let mut mission = snapshot.mission;
        let mut upserts = vec![Entity::Task(Box::new(task))];
        if matches!(code, "automatic_start_limit" | "active_time_limit")
            && !snapshot.decisions.iter().any(|d| {
                d.kind == DecisionKind::Budget && d.state == DecisionState::Open && d.blocking
            })
        {
            let question = super::workflow::store_artifact(
                &self.artifacts, mission_id, "text/plain",
                format!("The mission execution budget ({code}) is exhausted. New runs are paused. Inspect retained progress, then increase the policy limit or stop the mission. Existing runs retain their own timeout.").as_bytes(),
            )?;
            let (_, decision) = new_decision(
                &mission,
                DecisionKind::Budget,
                question,
                vec![
                    DecisionOption {
                        id: "stop_mission".into(),
                        label: "Stop mission".into(),
                    },
                    // UI route only: raising the limit is mission.policy.update,
                    // which obsoletes this decision and releases the task.
                    DecisionOption {
                        id: ADJUST_LIMITS.into(),
                        label: "Adjust mission limits, then continue".into(),
                    },
                ],
                vec![task_id.clone()],
                true,
                None,
            );
            mission.open_decision_count += 1;
            upserts.push(Entity::Decision(Box::new(decision)));
        }
        super::workflow::commit_upserts(
            self,
            mission,
            "engine.block",
            &format!("{task_id}:{code}"),
            MissionEventType::Changed,
            upserts,
        )?;
        Ok(())
    }

    fn now_iso(&self) -> String {
        term_storage::time::now_iso8601()
    }
}

/// O12: mission.decision.answer — the exact-decision CAS (E17/E18).
pub fn decision_answer_transition(
    mission: &Mission,
    decision: &Decision,
    answer: &term_contracts::mission::rpc::MissionDecisionAnswerParams,
    answer_body_ref: Option<term_contracts::mission::types::ArtifactRef>,
    system_message_id: Id,
) -> Result<ApplyMissionTransition, MissionRpcError> {
    if decision.state != DecisionState::Open {
        return Err(MissionRpcError::with_details(
            MissionErrorCode::StaleDecision,
            "the decision is no longer open",
            MissionErrorDetails {
                decision_id: Some(decision.id.clone()),
                ..Default::default()
            },
        ));
    }
    if decision.plan_revision != mission.plan_revision
        || (decision.candidate_id.is_some() && decision.candidate_id != mission.candidate_id)
    {
        return Err(MissionRpcError::with_details(
            MissionErrorCode::StaleDecision,
            "the decision predates the current plan",
            MissionErrorDetails {
                decision_id: Some(decision.id.clone()),
                ..Default::default()
            },
        ));
    }
    if let (Some(option_id), false) = (
        answer.option_id.as_deref(),
        decision
            .options
            .iter()
            .any(|o| Some(o.id.as_str()) == answer.option_id.as_deref()),
    ) {
        let _ = option_id;
        return Err(MissionRpcError::new(
            MissionErrorCode::InvalidArgument,
            "option id is not one of the decision's options",
        ));
    }
    if answer.option_id.is_none() && answer_body_ref.is_none() {
        return Err(MissionRpcError::new(
            MissionErrorCode::InvalidArgument,
            "an answer needs an option or an answer body",
        ));
    }
    let now = term_storage::time::now_iso8601();
    let mut next_decision = decision.clone();
    next_decision.state = DecisionState::Answered;
    next_decision.answer_ref = answer_body_ref.clone();
    next_decision.selected_option_id = answer.option_id.clone();
    next_decision.answer_message_id = Some(system_message_id.clone());
    next_decision.answered_at = Some(now.clone());
    let mut next_mission = mission.clone();
    next_mission.revision = U64String::new(mission.revision.get() + 1).expect("fits SQLite bound");
    next_mission.open_decision_count = next_mission.open_decision_count.saturating_sub(1);
    next_mission.updated_at = now.clone();
    let message = Message {
        id: system_message_id,
        mission_id: mission.id.clone(),
        target_task_id: None,
        role: MessageRole::System,
        run_id: None,
        body_ref: answer_body_ref.unwrap_or_else(|| decision.question_ref.clone()),
        delivery: MessageDelivery::Queued,
        supersedes_message_id: None,
        created_at: now.clone(),
    };
    Ok(ApplyMissionTransition {
        request_id: answer.request_id.clone(),
        method: "mission.decision.answer".into(),
        fingerprint: decision_fingerprint(answer),
        mission_id: mission.id.clone(),
        mode: ApplyMode::Mutate {
            expected_revision: answer.expected_revision.get(),
        },
        transaction_id: Id::generate(),
        event_type: MissionEventType::DecisionAnswered,
        upserts: vec![
            Entity::Mission(Box::new(next_mission)),
            Entity::Decision(Box::new(next_decision)),
            Entity::Message(Box::new(message)),
        ],
        deletes: Vec::new(),
        changes_ref: None,
        outbox: Vec::new(),
        outbox_updates: Vec::new(),
        adopt_staged_artifacts: Vec::new(),
        created_at: now,
    })
}

fn decision_fingerprint(
    answer: &term_contracts::mission::rpc::MissionDecisionAnswerParams,
) -> String {
    MissionService::fingerprint(
        "mission.decision.answer",
        &serde_json::to_value(answer).expect("serializable answer"),
    )
}

fn answer_error(
    code: MissionErrorCode,
    reason_code: &str,
    message: impl Into<String>,
) -> MissionRpcError {
    MissionRpcError::with_details(
        code,
        message,
        MissionErrorDetails {
            reason_code: Some(reason_code.into()),
            ..Default::default()
        },
    )
}

fn fingerprint_hex(input: &str) -> String {
    format!("{:x}", Sha256::digest(input.as_bytes()))
}

/// O12: decision factory for adapter questions and consult requests.
pub fn new_decision(
    mission: &Mission,
    kind: DecisionKind,
    question_ref: term_contracts::mission::types::ArtifactRef,
    options: Vec<DecisionOption>,
    affected_task_ids: Vec<Id>,
    blocking: bool,
    requesting_run_id: Option<Id>,
) -> (Id, Decision) {
    let id = Id::generate();
    let decision = Decision {
        id: id.clone(),
        mission_id: mission.id.clone(),
        requesting_run_id,
        kind,
        state: DecisionState::Open,
        question_ref,
        options,
        affected_task_ids,
        blocking,
        plan_revision: mission.plan_revision,
        candidate_id: mission.candidate_id.clone(),
        answer_ref: None,
        selected_option_id: None,
        answer_message_id: None,
        created_at: term_storage::time::now_iso8601(),
        answered_at: None,
    };
    (id, decision)
}

impl MissionService {
    /// mission.decision.answer: exact-decision CAS (E17/E18) — the answer
    /// commits once; delivery to the provider is a separate outbox concern.
    pub(crate) fn decision_answer(
        &self,
        params: &serde_json::Value,
    ) -> Result<super::service::Handled, MissionRpcError> {
        let params: term_contracts::mission::rpc::MissionDecisionAnswerParams =
            serde_json::from_value(params.clone()).map_err(|e| {
                MissionRpcError::new(
                    MissionErrorCode::InvalidArgument,
                    format!("params mismatch: {e}"),
                )
            })?;
        let snapshot = super::workflow::load_entities(&self.storage, &params.mission_id)?;
        let mission = snapshot.mission.clone();
        let decision = snapshot
            .decisions
            .iter()
            .find(|d| d.id == params.decision_id)
            .cloned()
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::NotFound,
                    format!("decision {} not found", params.decision_id),
                )
            })?;
        let failure = super::failures::is_failure_decision(&decision);
        let cost = super::costs::is_cost_decision(&decision);
        let reconciled = super::reconciliation::is_reconciled_decision(&decision);
        if reconciled {
            if !matches!(
                mission.state,
                term_contracts::mission::types::MissionState::Running
                    | term_contracts::mission::types::MissionState::Paused
                    | term_contracts::mission::types::MissionState::Pausing
            ) {
                return Err(MissionRpcError::new(
                    MissionErrorCode::InvalidState,
                    "mission cannot resolve uncertain executions in its current state",
                ));
            }
            self.reconciled_decision_task(&snapshot, &decision)?;
            if !matches!(
                params.option_id.as_deref(),
                Some(
                    super::reconciliation::RETRY_RECONCILED
                        | super::reconciliation::STOP_RECONCILED
                )
            ) {
                return Err(MissionRpcError::new(
                    MissionErrorCode::InvalidArgument,
                    "choose an explicit execution recovery action",
                ));
            }
        }
        if cost {
            let active = matches!(
                mission.state,
                term_contracts::mission::types::MissionState::Running
                    | term_contracts::mission::types::MissionState::Pausing
                    | term_contracts::mission::types::MissionState::Paused
            );
            let current = snapshot.tasks.iter().any(|task| {
                decision.affected_task_ids == [task.id.clone()]
                    && task.state == TaskState::Blocked
                    && task.active_run_id.is_none()
                    && matches!(
                        task.blocked_code.as_deref(),
                        Some("cost_limit" | "cost_unknown")
                    )
            });
            if !active || !current {
                return Err(MissionRpcError::new(
                    MissionErrorCode::StaleDecision,
                    "cost hold is no longer current",
                ));
            }
            if params.option_id.as_deref() != Some(super::costs::STOP_COST_MISSION) {
                return Err(MissionRpcError::new(
                    MissionErrorCode::InvalidArgument,
                    "choose a cost recovery action or update the cost policy",
                ));
            }
        }
        if failure {
            if !matches!(
                mission.state,
                term_contracts::mission::types::MissionState::Running
                    | term_contracts::mission::types::MissionState::Pausing
                    | term_contracts::mission::types::MissionState::Paused
            ) {
                return Err(MissionRpcError::new(
                    MissionErrorCode::InvalidState,
                    "mission cannot resolve task failures in its current state",
                ));
            }
            self.failure_decision_task(&snapshot, &decision)?;
            if !matches!(
                params.option_id.as_deref(),
                Some(super::failures::RETRY_FAILED_TASK | super::failures::STOP_FAILED_MISSION)
            ) {
                return Err(MissionRpcError::new(
                    MissionErrorCode::InvalidArgument,
                    "choose a task recovery action",
                ));
            }
        }
        let exclusion = if decision.kind == DecisionKind::Conflict
            && params.option_id.as_deref() == Some(super::integration_exclusion::EXCLUDE)
        {
            Some(self.prepare_integration_exclusion(
                &snapshot,
                &decision,
                params.answer_ref.clone(),
            )?)
        } else {
            None
        };
        let integration_task = if decision.kind == DecisionKind::Conflict && exclusion.is_none() {
            self.prepare_integration_answer(&snapshot, &decision, params.option_id.as_deref())?
        } else {
            None
        };
        if params.option_id.as_deref() == Some(ADJUST_LIMITS)
            && decision.options.iter().any(|o| o.id == ADJUST_LIMITS)
        {
            return Err(answer_error(
                MissionErrorCode::InvalidArgument,
                "policy_update_required",
                "raise the limit with mission.policy.update; this decision closes when new runs are allowed",
            ));
        }
        let answer_text = match &params.answer_ref {
            Some(reference) => String::from_utf8(
                self.artifacts
                    .read_mission_body(&mission.id, reference, self.limits.max_message_bytes)
                    .map_err(|(code, message)| MissionRpcError::new(code, message))?,
            )
            .ok(),
            None => None,
        };
        // Text that is embedded next to an option or into a follow-up plan
        // objective must be bounded UTF-8 so the next context fits its budget.
        if exclusion.is_none()
            && params.answer_ref.is_some()
            && (params.option_id.is_some() || decision.kind == DecisionKind::Plan)
        {
            match &answer_text {
                None => {
                    return Err(answer_error(
                        MissionErrorCode::InvalidArgument,
                        "answer_not_text",
                        "the answer must be UTF-8 text",
                    ))
                }
                Some(text) if text.len() > super::provider_blocks::MAX_EMBEDDED_BYTES => {
                    return Err(answer_error(
                        MissionErrorCode::InvalidArgument,
                        "answer_too_large",
                        format!(
                            "the answer exceeds {} bytes",
                            super::provider_blocks::MAX_EMBEDDED_BYTES
                        ),
                    ))
                }
                Some(_) => {}
            }
        }
        let mut provider_block =
            if super::provider_blocks::is_provider_block_decision(&snapshot, &decision) {
                Some(self.prepare_provider_block_answer(
                    &snapshot,
                    &decision,
                    params.option_id.as_deref(),
                    answer_text.as_deref(),
                )?)
            } else {
                None
            };
        let mut transition = decision_answer_transition(
            &mission,
            &decision,
            &params,
            params.answer_ref.clone(),
            Id::generate(),
        )?;
        // The conversation records the selected answer, not the original
        // question reused as if it were the user's response. A free-text-only
        // answer is its own body; any option is recorded as a structured
        // `decision_answer` document (01 §8) carrying the text alongside.
        let body = if let Some(action) = &exclusion {
            action.evidence_ref.clone()
        } else if let (Some(reference), None) = (&params.answer_ref, &params.option_id) {
            reference.clone()
        } else {
            let option_label = params
                .option_id
                .as_deref()
                .and_then(|id| decision.options.iter().find(|o| o.id == id))
                .map(|o| o.label.clone());
            super::workflow::store_artifact(
                &self.artifacts,
                &mission.id,
                "application/json",
                json!({
                    "kind": "decision_answer",
                    "version": 1,
                    "decision_id": decision.id,
                    "decision_kind": decision.kind,
                    "option_id": params.option_id,
                    "option_label": option_label,
                    "answer_ref": params.answer_ref,
                    "answer_text": answer_text,
                })
                .to_string()
                .as_bytes(),
            )?
        };
        let mut message_id = None;
        for entity in &mut transition.upserts {
            if let Entity::Message(message) = entity {
                message.body_ref = body.clone();
                if failure
                    || cost
                    || matches!(
                        decision.kind,
                        DecisionKind::Recovery | DecisionKind::Conflict
                    )
                {
                    message.delivery = MessageDelivery::Delivered;
                }
                message.target_task_id = if decision.affected_task_ids.len() == 1 {
                    decision.affected_task_ids.first().cloned()
                } else {
                    None
                };
                message_id = Some(message.id.clone());
            }
        }
        if params.option_id.as_deref() == Some("stop_mission")
            || (super::pipeline::is_review_repair_decision(&decision)
                && params.option_id.as_deref() == Some(super::pipeline::STOP_REVIEW_REPAIR))
            || (reconciled
                && params.option_id.as_deref() == Some(super::reconciliation::STOP_RECONCILED))
            || (cost && params.option_id.as_deref() == Some(super::costs::STOP_COST_MISSION))
            || (failure
                && params.option_id.as_deref() == Some(super::failures::STOP_FAILED_MISSION))
        {
            for entity in &mut transition.upserts {
                if let Entity::Mission(m) = entity {
                    m.state = term_contracts::mission::types::MissionState::Stopping;
                    if failure {
                        m.failure_code = snapshot
                            .runs
                            .iter()
                            .find(|r| Some(&r.id) == decision.requesting_run_id.as_ref())
                            .and_then(|r| r.failure_code)
                            .or(Some(MissionErrorCode::ResultInvalid));
                    }
                }
            }
            for task in snapshot.tasks.iter().filter(|t| !t.state.is_terminal()) {
                let mut next = task.clone();
                next.state = TaskState::Cancelled;
                if let Some(run) = snapshot
                    .runs
                    .iter()
                    .find(|r| Some(&r.id) == task.active_run_id.as_ref())
                {
                    let (run, intent) =
                        super::outbox::prepare_cancel(run, &term_storage::time::now_iso8601());
                    if run.state == RunState::Cancelled {
                        next.active_run_id = None;
                    }
                    transition.upserts.push(Entity::Run(Box::new(run)));
                    transition.outbox.extend(intent);
                }
                transition.upserts.push(Entity::Task(Box::new(next)));
            }
        } else {
            match decision.kind {
                DecisionKind::Conflict => {
                    if let Some(action) = exclusion {
                        for entity in &mut transition.upserts {
                            match entity {
                                Entity::Mission(m) => {
                                    m.phase = term_contracts::mission::types::Phase::Planning;
                                    m.candidate_id = None;
                                }
                                Entity::Message(m) => {
                                    m.target_task_id = Some(action.lead.id.clone())
                                }
                                _ => (),
                            }
                        }
                        transition.upserts.push(Entity::Task(Box::new(action.lead)));
                        transition
                            .upserts
                            .push(Entity::Task(Box::new(action.retired_integration)));
                        transition.upserts.extend(
                            action
                                .retired_tasks
                                .into_iter()
                                .map(|t| Entity::Task(Box::new(t))),
                        );
                    }
                    if let Some(task) = integration_task {
                        for entity in &mut transition.upserts {
                            if let Entity::Message(message) = entity {
                                message.target_task_id = Some(task.id.clone());
                            }
                        }
                        transition.upserts.push(Entity::Task(Box::new(task)));
                    }
                }
                DecisionKind::Recovery
                    if reconciled
                        && params.option_id.as_deref()
                            == Some(super::reconciliation::RETRY_RECONCILED) =>
                {
                    let next = self.prepare_reconciled_retry(&snapshot, &decision)?;
                    transition.upserts.push(Entity::Task(Box::new(next)));
                }
                DecisionKind::Recovery
                    if failure
                        && params.option_id.as_deref()
                            == Some(super::failures::RETRY_FAILED_TASK) =>
                {
                    let task = self.failure_decision_task(&snapshot, &decision)?;
                    let next = self.retry_failed_task(&snapshot, task, None)?;
                    transition.upserts.push(Entity::Task(Box::new(next)));
                }
                DecisionKind::Recovery if params.option_id.as_deref() == Some("resume_unsent") => {
                    let run = snapshot
                        .runs
                        .iter()
                        .find(|r| {
                            Some(&r.id) == decision.requesting_run_id.as_ref()
                                && r.state == RunState::Prepared
                                && r.dispatch_state == RunDispatchState::Unsent
                        })
                        .ok_or_else(|| {
                            MissionRpcError::new(
                                MissionErrorCode::StaleDecision,
                                "the run is no longer unsent",
                            )
                        })?;
                    let mut task = snapshot
                        .tasks
                        .iter()
                        .find(|t| t.id == run.task_id && t.active_run_id.as_ref() == Some(&run.id))
                        .cloned()
                        .ok_or_else(|| {
                            MissionRpcError::new(
                                MissionErrorCode::StaleDecision,
                                "the task no longer owns this run",
                            )
                        })?;
                    task.state = TaskState::Running;
                    task.blocked_code = None;
                    transition.upserts.push(Entity::Task(Box::new(task)));
                }
                DecisionKind::Plan if params.option_id.as_deref() == Some("apply") => {
                    let bytes = self
                        .artifacts
                        .read_mission_body(
                            &mission.id,
                            &decision.question_ref,
                            self.limits.max_context_bytes,
                        )
                        .map_err(|(code, message)| MissionRpcError::new(code, message))?;
                    let proposal: term_contracts::mission::types::PlanProposal =
                        serde_json::from_slice(&bytes).map_err(|e| {
                            MissionRpcError::new(MissionErrorCode::ResultInvalid, e.to_string())
                        })?;
                    let (mut applied, entities) = self.plan_entities(&snapshot, &proposal)?;
                    applied.revision =
                        U64String::new(params.expected_revision.get() + 1).expect("revision");
                    applied.updated_at = term_storage::time::now_iso8601();
                    transition
                        .upserts
                        .retain(|e| !matches!(e, Entity::Mission(_)));
                    transition
                        .upserts
                        .insert(0, Entity::Mission(Box::new(applied)));
                    transition.upserts.extend(
                        entities
                            .into_iter()
                            .filter(|e| !matches!(e,Entity::Decision(d) if d.id==decision.id)),
                    );
                    for entity in &mut transition.upserts {
                        if let Entity::Message(m) = entity {
                            m.delivery = MessageDelivery::Delivered;
                        }
                    }
                }
                DecisionKind::Approval => {
                    // `Running` counts as well as `AwaitingInput`: a provider
                    // can hold several approvals at once, and a build that
                    // resumed the run on the first answer leaves the rest to
                    // be answered against a running run. The provider is still
                    // holding those request ids, so refusing here would strand
                    // the turn forever; delivery decides what the provider
                    // actually accepts. An ended run has nothing to answer.
                    let run = snapshot
                        .runs
                        .iter()
                        .find(|r| {
                            Some(&r.id) == decision.requesting_run_id.as_ref()
                                && matches!(r.state, RunState::AwaitingInput | RunState::Running)
                        })
                        .ok_or_else(|| {
                            MissionRpcError::new(
                                MissionErrorCode::StaleDecision,
                                "approval run is no longer live",
                            )
                        })?;
                    let bytes = self
                        .artifacts
                        .read_mission_body(
                            &mission.id,
                            &decision.question_ref,
                            self.limits.max_context_bytes,
                        )
                        .map_err(|(code, message)| MissionRpcError::new(code, message))?;
                    let question: serde_json::Value =
                        serde_json::from_slice(&bytes).map_err(|_| {
                            MissionRpcError::new(
                                MissionErrorCode::ResultInvalid,
                                "approval request has no provider identity",
                            )
                        })?;
                    let provider_request_id = question
                        .get("provider_request_id")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            MissionRpcError::new(
                                MissionErrorCode::ResultInvalid,
                                "approval request has no provider identity",
                            )
                        })?;
                    let answer = params.option_id.clone().ok_or_else(|| {
                        MissionRpcError::new(
                            MissionErrorCode::InvalidArgument,
                            "approval requires an explicit allow or deny option",
                        )
                    })?;
                    transition.outbox.push(OutboxIntent{id:decision.id.clone(),mission_id:mission.id.clone(),run_id:Some(run.id.clone()),operation:OutboxOperation::Answer,
                    dedupe_key:format!("{}/{}/answer",mission.id,decision.id),fencing_token:run.fencing_token.get(),payload:json!({"provider_request_id":provider_request_id,"answer":answer,"message_id":message_id}),created_at:term_storage::time::now_iso8601()});
                }
                DecisionKind::Recovery if provider_block.is_some() => {
                    match provider_block.take() {
                        Some(super::provider_blocks::ProviderBlockAnswer::Retry(next)) => {
                            transition.upserts.push(Entity::Task(next));
                            // The instruction is new input for the next Run of
                            // this task: queue it and route it as that Run's
                            // context, like mission.message, so it is marked
                            // delivered only after the provider accepts it.
                            if params.answer_ref.is_some() {
                                let mut routed = None;
                                for entity in &mut transition.upserts {
                                    if let Entity::Message(message) = entity {
                                        message.delivery = MessageDelivery::Queued;
                                        routed =
                                            Some(super::messaging::route_intent(message, None));
                                    }
                                }
                                transition.outbox.extend(routed);
                            }
                        }
                        Some(super::provider_blocks::ProviderBlockAnswer::Replan(lead)) => {
                            for entity in &mut transition.upserts {
                                if let Entity::Mission(m) = entity {
                                    super::provider_blocks::enter_replan(m);
                                }
                            }
                            transition.upserts.push(Entity::Task(lead));
                        }
                        Some(super::provider_blocks::ProviderBlockAnswer::Stop) | None => {}
                    }
                }
                DecisionKind::Product | DecisionKind::Plan => {
                    if decision.kind == DecisionKind::Plan
                        && snapshot.tasks.len() >= self.limits.max_tasks_per_mission
                    {
                        return Err(MissionRpcError::new(
                            MissionErrorCode::PlanLimit,
                            "plan revision would exceed the task limit",
                        ));
                    }
                    // A revised plan gets its own objective: the user's request
                    // plus the proposal that was not applied (bounded), instead
                    // of the bare answer record.
                    let revision_objective = if decision.kind == DecisionKind::Plan {
                        let rejected = self
                            .artifacts
                            .read_mission_body(
                                &mission.id,
                                &decision.question_ref,
                                self.limits.max_context_bytes,
                            )
                            .ok()
                            .filter(|bytes| {
                                bytes.len() <= super::provider_blocks::MAX_EMBEDDED_BYTES
                            })
                            .and_then(|bytes| {
                                serde_json::from_slice::<serde_json::Value>(&bytes).ok()
                            });
                        Some(super::workflow::store_artifact(
                            &self.artifacts,
                            &mission.id,
                            "application/json",
                            json!({
                                "kind": "plan_revision_request",
                                "version": 1,
                                "decision_id": decision.id,
                                "instruction": "The user did not apply the proposed plan. Propose a revised plan for the same goal and original requirements that addresses the user's request. Do not resubmit the rejected proposal unchanged.",
                                "option_id": params.option_id,
                                "user_request": answer_text,
                                "rejected_proposal_omitted": rejected.is_none(),
                                "rejected_proposal": rejected,
                            })
                            .to_string()
                            .as_bytes(),
                        )?)
                    } else {
                        None
                    };
                    for task in snapshot
                        .tasks
                        .iter()
                        .filter(|t| decision.affected_task_ids.contains(&t.id))
                    {
                        if task.active_run_id.is_none()
                            && (task.state == TaskState::AwaitingInput
                                || decision.kind == DecisionKind::Plan)
                        {
                            if task.state == TaskState::Succeeded
                                && decision.kind == DecisionKind::Plan
                            {
                                // A revision request gets a new plan task; the
                                // successful earlier planning run stays immutable.
                                let mut next = task.clone();
                                next.id = Id::generate();
                                next.depends_on = vec![];
                                next.parent_task_id = None;
                                next.ordinal =
                                    snapshot.tasks.iter().map(|t| t.ordinal).max().unwrap_or(0) + 1;
                                next.attempt_count = 0;
                                next.workspace_id = None;
                                next.contract.objective_ref =
                                    revision_objective.clone().unwrap_or_else(|| body.clone());
                                next.state = TaskState::Ready;
                                next.created_at = term_storage::time::now_iso8601();
                                next.updated_at = next.created_at.clone();
                                transition.upserts.push(Entity::Task(Box::new(next)));
                            } else {
                                let mut next = task.clone();
                                next.state = TaskState::Ready;
                                next.blocked_code = None;
                                next.updated_at = term_storage::time::now_iso8601();
                                transition.upserts.push(Entity::Task(Box::new(next)));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let applied = self
            .apply_timed_transition(transition)
            .map_err(Self::store_error)?;
        Ok(super::service::Handled {
            result: serde_json::to_value(&applied.result).unwrap_or(serde_json::Value::Null),
            notify: Some((mission.id.clone(), applied.result.revision.get())),
        })
    }

    /// mission.task.control: cancel/retry with state validation; reassign
    /// requires the old run to be terminal first (05 §8 table).
    pub(crate) fn task_control(
        &self,
        params: &serde_json::Value,
    ) -> Result<super::service::Handled, MissionRpcError> {
        let params: term_contracts::mission::rpc::MissionTaskControlParams =
            serde_json::from_value(params.clone()).map_err(|e| {
                MissionRpcError::new(
                    MissionErrorCode::InvalidArgument,
                    format!("params mismatch: {e}"),
                )
            })?;
        let snapshot = super::workflow::load_entities(&self.storage, &params.mission_id)?;
        let mission = snapshot.mission.clone();
        if matches!(
            mission.state,
            term_contracts::mission::types::MissionState::Completed
                | term_contracts::mission::types::MissionState::Cancelled
                | term_contracts::mission::types::MissionState::Failed
        ) {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidState,
                "cannot change tasks in a terminal mission",
            ));
        }
        let task = snapshot
            .tasks
            .iter()
            .find(|t| t.id == params.task_id)
            .cloned();
        let live_run = snapshot
            .runs
            .iter()
            .find(|r| r.task_id == params.task_id && r.holds_execution_slot())
            .cloned();
        let mut task = task.ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::NotFound,
                format!("task {} not found", params.task_id),
            )
        })?;
        use term_contracts::mission::rpc::TaskControlAction;
        let mut run_upserts = Vec::new();
        let mut outbox = Vec::new();
        match params.action {
            TaskControlAction::Cancel => {
                if task.state.is_terminal() {
                    return Err(MissionRpcError::new(
                        MissionErrorCode::InvalidState,
                        "task is already terminal",
                    ));
                }
                if let Some(run) = &live_run {
                    let (next, intent) = super::outbox::prepare_cancel(run, &self.now_iso());
                    if !next.holds_execution_slot() {
                        task.active_run_id = None;
                    }
                    run_upserts.push(Entity::Run(Box::new(next)));
                    outbox.extend(intent);
                }
                task.state = TaskState::Cancelled;
                task.dispatch_after_unix_ms = None;
                task.updated_at = self.now_iso();
            }
            TaskControlAction::Retry => {
                task = self.retry_failed_task(&snapshot, &task, params.binding_id.as_ref())?;
            }
            TaskControlAction::Reassign => {
                if mission.state == term_contracts::mission::types::MissionState::Stopping {
                    return Err(MissionRpcError::new(
                        MissionErrorCode::InvalidState,
                        "cannot reassign while mission cleanup is in progress",
                    ));
                }
                let new_binding = params.binding_id.clone().ok_or_else(|| {
                    MissionRpcError::new(
                        MissionErrorCode::InvalidArgument,
                        "reassign needs binding_id",
                    )
                })?;
                let assigning_integrator = task.integration.is_some()
                    && task.state == TaskState::Failed
                    && snapshot.decisions.iter().any(|d| {
                        d.kind == DecisionKind::Conflict
                            && d.state == DecisionState::Open
                            && d.requesting_run_id.as_ref().is_some_and(|id| {
                                snapshot
                                    .runs
                                    .iter()
                                    .any(|r| &r.id == id && r.task_id == task.id)
                            })
                    });
                if task.state.is_terminal() && !assigning_integrator {
                    return Err(MissionRpcError::new(
                        MissionErrorCode::InvalidState,
                        "cannot reassign a terminal task",
                    ));
                }
                if let Some(run) = &live_run {
                    return Err(MissionRpcError::new(
                        MissionErrorCode::InvalidState,
                        format!("live run {} must end before reassign", run.id),
                    ));
                }
                self.validate_task_binding(&mission, &task, &new_binding)?;
                task.binding_id = Some(new_binding);
                task.updated_at = self.now_iso();
            }
        }
        let mut next_mission = mission.clone();
        if matches!(
            params.action,
            TaskControlAction::Cancel | TaskControlAction::Retry
        ) {
            run_upserts.extend(super::failures::obsolete_failure_decisions(
                &mut next_mission,
                &snapshot.decisions,
                &task.id,
            ));
        }
        next_mission.revision =
            U64String::new(params.expected_revision.get() + 1).expect("fits SQLite bound");
        next_mission.updated_at = self.now_iso();
        let params_json = serde_json::to_value(&params).unwrap_or(serde_json::Value::Null);
        run_upserts.insert(0, Entity::Mission(Box::new(next_mission)));
        run_upserts.push(Entity::Task(Box::new(task)));
        let transition = ApplyMissionTransition {
            request_id: params.request_id.clone(),
            method: "mission.task.control".into(),
            fingerprint: Self::fingerprint("mission.task.control", &params_json),
            mission_id: params.mission_id.clone(),
            mode: ApplyMode::Mutate {
                expected_revision: params.expected_revision.get(),
            },
            transaction_id: Id::generate(),
            event_type: MissionEventType::Changed,
            upserts: run_upserts,
            deletes: Vec::new(),
            changes_ref: None,
            outbox,
            outbox_updates: Vec::new(),
            adopt_staged_artifacts: Vec::new(),
            created_at: self.now_iso(),
        };
        let applied = self
            .apply_timed_transition(transition)
            .map_err(Self::store_error)?;
        Ok(super::service::Handled {
            result: serde_json::to_value(&applied.result).unwrap_or(serde_json::Value::Null),
            notify: Some((params.mission_id.clone(), applied.result.revision.get())),
        })
    }
}
