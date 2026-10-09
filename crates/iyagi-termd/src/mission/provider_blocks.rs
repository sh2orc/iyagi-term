//! A provider `blocked` result ends its Run successfully but leaves the Task
//! waiting for a person. Every such Run gets one current Recovery decision so
//! the mission never stalls silently. Answers reuse the existing retry,
//! reassignment, and Lead planning paths; attempt, start, time, binding, and
//! capability rules are checked again when the answer is committed.
use std::collections::HashSet;

use serde_json::json;
use term_contracts::mission::{types::*, MissionErrorCode, MissionErrorDetails, MissionRpcError};

use super::{
    service::MissionService,
    workflow::{self, MissionEntities},
};

pub(super) const BLOCKED_CODE_PREFIX: &str = "provider_blocked:";
pub(super) const RETRY_WITH_INSTRUCTION: &str = "retry_with_instruction";
pub(super) const CHANGE_MODEL: &str = "change_model";
pub(super) const REPLAN: &str = "replan";
pub(super) const STOP_MISSION: &str = "stop_mission";
/// Bound for report/instruction text embedded into a follow-up context, well
/// inside `max_context_bytes` together with the goal and task list.
pub(super) const MAX_EMBEDDED_BYTES: usize = 65_536;

/// Recovery decisions for a successfully ended Run with only these option ids.
/// Failure, reconciliation, and unsent-run decisions reference Failed, Unknown,
/// or live Runs, so the classification holds even when only `stop_mission`
/// remains (attempts exhausted on a Plan task).
pub(super) fn is_provider_block_decision(snapshot: &MissionEntities, decision: &Decision) -> bool {
    decision.kind == DecisionKind::Recovery
        && decision.options.iter().any(|o| o.id == STOP_MISSION)
        && decision.options.iter().all(|o| {
            matches!(
                o.id.as_str(),
                RETRY_WITH_INSTRUCTION | CHANGE_MODEL | REPLAN | STOP_MISSION
            )
        })
        && decision.requesting_run_id.as_ref().is_some_and(|id| {
            snapshot
                .runs
                .iter()
                .any(|r| &r.id == id && r.state == RunState::Succeeded)
        })
}

fn reason(
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

/// Truncate on a UTF-8 boundary; the caller records that it was truncated.
pub(super) fn bounded_text(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// The ended Run whose `blocked` result currently holds `task`. Its owned
/// Exec, if any, must have exited: a retry never races a live process.
pub(super) fn blocking_run<'a>(snapshot: &'a MissionEntities, task: &Task) -> Option<&'a Run> {
    if task.state != TaskState::Blocked
        || task.active_run_id.is_some()
        || !task
            .blocked_code
            .as_deref()
            .is_some_and(|code| code.starts_with(BLOCKED_CODE_PREFIX))
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
            r.state == RunState::Succeeded && r.ended_at.is_some() && r.result_ref.is_some()
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

/// A Lead plan that is actually progressing toward a plan. A Blocked plan
/// task (a provider block, an attempt/time/budget limit, a capability or an
/// uncertain run) waits for its own resolution and is not planning.
fn lead_plan_in_progress(snapshot: &MissionEntities) -> bool {
    snapshot.tasks.iter().any(|t| {
        t.kind == TaskKind::Plan
            && t.role == Some(Role::Lead)
            && matches!(
                t.state,
                TaskState::Planned
                    | TaskState::Ready
                    | TaskState::Running
                    | TaskState::AwaitingInput
                    | TaskState::AwaitingReview
            )
    })
}

/// Phase effect of an accepted `replan`, matching the existing Lead replans.
/// A plan task runs alongside implementation (failure_repair.rs). An
/// unfinished integration is abandoned like a candidate exclusion
/// (integration_exclusion.rs): planning resumes without the prior candidate
/// and the next integration mints a new one. After a candidate was produced
/// (validation, review, acceptance) it stays the base of the replacement work
/// while planning resumes, like a verification/review repair plan
/// (pipeline.rs `request_repair`).
pub(super) fn enter_replan(mission: &mut Mission) {
    match mission.phase {
        Phase::Planning | Phase::Implementing => {}
        Phase::Integrating => {
            mission.phase = Phase::Planning;
            mission.candidate_id = None;
        }
        _ => mission.phase = Phase::Planning,
    }
}

/// A `replan` answer owns this Run until its Lead plan (and any plan decision
/// it raised) concludes. If the plan did not retire the task, ask again.
fn awaiting_replan(snapshot: &MissionEntities, run_id: &Id) -> bool {
    snapshot.decisions.iter().any(|d| {
        d.state == DecisionState::Answered
            && d.requesting_run_id.as_ref() == Some(run_id)
            && is_provider_block_decision(snapshot, d)
            && d.selected_option_id.as_deref() == Some(REPLAN)
    }) && (lead_plan_in_progress(snapshot)
        || snapshot
            .decisions
            .iter()
            .any(|d| d.state == DecisionState::Open && d.blocking))
}

/// Effects of an accepted answer, applied by `decision_answer` in the same
/// transaction as the answered decision.
pub(super) enum ProviderBlockAnswer {
    Stop,
    /// `retry_with_instruction` / `change_model`: the task becomes Ready.
    Retry(Box<Task>),
    /// A new Lead Plan task; the blocked task stays blocked until that plan
    /// retires or replaces it. Not offered for a blocked Plan task, whose
    /// retry already asks the Lead again (plans are not replaceable outputs).
    Replan(Box<Task>),
}

impl MissionService {
    fn provider_block_options(snapshot: &MissionEntities, task: &Task) -> Vec<DecisionOption> {
        let mut options = Vec::new();
        if task.attempt_count < snapshot.mission.policy.max_attempts_per_task {
            options.push(DecisionOption {
                id: RETRY_WITH_INSTRUCTION.into(),
                label: "Retry with an instruction".into(),
            });
            if task.execution_binding_id().is_some() && task.kind != TaskKind::Verify {
                options.push(DecisionOption {
                    id: CHANGE_MODEL.into(),
                    label: "Retry with a different model".into(),
                });
            }
        }
        if task.kind != TaskKind::Plan
            && snapshot
                .mission
                .role_bindings
                .iter()
                .any(|r| r.role == Role::Lead)
        {
            options.push(DecisionOption {
                id: REPLAN.into(),
                label: "Ask the Lead for an alternative plan".into(),
            });
        }
        options.push(DecisionOption {
            id: STOP_MISSION.into(),
            label: "Stop mission".into(),
        });
        options
    }

    /// `(code, report_ref)` from the validated `blocked` result. An expired or
    /// unreadable result still gets a decision, just without the report link.
    fn blocked_report(
        &self,
        snapshot: &MissionEntities,
        task: &Task,
        run: &Run,
    ) -> (String, Option<ArtifactRef>) {
        let fallback = task
            .blocked_code
            .as_deref()
            .and_then(|code| code.strip_prefix(BLOCKED_CODE_PREFIX))
            .unwrap_or_default()
            .to_string();
        let Some(reference) = run.result_ref.as_ref() else {
            return (fallback, None);
        };
        match self
            .artifacts
            .read_mission_body(
                &snapshot.mission.id,
                reference,
                self.limits.max_context_bytes,
            )
            .ok()
            .and_then(|bytes| serde_json::from_slice::<AgentResult>(&bytes).ok())
        {
            Some(AgentResult::Blocked { code, report_ref }) => (code, Some(report_ref)),
            _ => (fallback, None),
        }
    }

    /// Durable projections decide, so a crash between the result commit and
    /// this decision recreates it. Repeated passes make no no-op revisions.
    pub(super) fn reconcile_provider_blocks(
        &self,
        snapshot: &MissionEntities,
    ) -> Result<bool, MissionRpcError> {
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
            .filter(|d| d.state == DecisionState::Open && is_provider_block_decision(snapshot, d))
        {
            let current = snapshot
                .tasks
                .iter()
                .find(|t| decision.affected_task_ids == [t.id.clone()])
                .and_then(|t| blocking_run(snapshot, t).map(|r| (t, r)))
                .filter(|(_, r)| decision.requesting_run_id.as_ref() == Some(&r.id));
            if actionable
                && current.is_some_and(|(task, _)| {
                    decision.options == Self::provider_block_options(snapshot, task)
                })
                && decision.plan_revision == mission.plan_revision
                && decision.candidate_id == mission.candidate_id
            {
                if let Some((_, run)) = current {
                    retained.insert(run.id.clone());
                }
            } else {
                let mut obsolete = decision.clone();
                obsolete.state = DecisionState::Obsolete;
                mission.open_decision_count = mission.open_decision_count.saturating_sub(1);
                entities.push(Entity::Decision(Box::new(obsolete)));
            }
        }
        if actionable {
            for task in &snapshot.tasks {
                let Some(run) = blocking_run(snapshot, task) else {
                    continue;
                };
                if retained.contains(&run.id) || awaiting_replan(snapshot, &run.id) {
                    continue;
                }
                let (code, report_ref) = self.blocked_report(snapshot, task, run);
                let document = json!({
                    "kind": "provider_blocked",
                    "version": 1,
                    "task_id": task.id,
                    "task_title": task.title,
                    "run_id": run.id,
                    "code": code,
                    "report_ref": report_ref,
                    "attempt_count": task.attempt_count,
                    "max_attempts_per_task": mission.policy.max_attempts_per_task,
                    "message": format!("Task '{}' stopped because its agent reported that it is blocked ({code}). Read the agent's report, then retry with an instruction, retry with a different model, ask the Lead for an alternative plan, or stop the mission. Independent tasks continue.", task.title),
                });
                let question = workflow::store_artifact(
                    &self.artifacts,
                    &mission.id,
                    "application/json",
                    document.to_string().as_bytes(),
                )?;
                let (_, decision) = super::engine::new_decision(
                    &mission,
                    DecisionKind::Recovery,
                    question,
                    Self::provider_block_options(snapshot, task),
                    vec![task.id.clone()],
                    false,
                    Some(run.id.clone()),
                );
                mission.open_decision_count += 1;
                entities.push(Entity::Decision(Box::new(decision)));
            }
        }
        if entities.is_empty() {
            return Ok(false);
        }
        self.commit_actor(mission, "engine.provider_block_decision", entities, vec![])?;
        Ok(true)
    }

    /// Validate an answer against the exact blocked Run and prepare its
    /// effects. `instruction` is the already size-checked answer text.
    pub(super) fn prepare_provider_block_answer(
        &self,
        snapshot: &MissionEntities,
        decision: &Decision,
        option_id: Option<&str>,
        instruction: Option<&str>,
    ) -> Result<ProviderBlockAnswer, MissionRpcError> {
        let stale = |message: &str| {
            MissionRpcError::with_details(
                MissionErrorCode::StaleDecision,
                message,
                MissionErrorDetails {
                    decision_id: Some(decision.id.clone()),
                    ..Default::default()
                },
            )
        };
        if decision.state != DecisionState::Open {
            return Err(stale("the decision is no longer open"));
        }
        if !matches!(
            snapshot.mission.state,
            MissionState::Running | MissionState::Pausing | MissionState::Paused
        ) {
            return Err(reason(
                MissionErrorCode::InvalidState,
                "mission_not_active",
                "mission cannot resolve a blocked task in its current state",
            ));
        }
        let (task, run) = snapshot
            .tasks
            .iter()
            .find(|t| decision.affected_task_ids == [t.id.clone()])
            .and_then(|t| blocking_run(snapshot, t).map(|r| (t, r)))
            .filter(|(_, r)| decision.requesting_run_id.as_ref() == Some(&r.id))
            .ok_or_else(|| stale("the task is no longer blocked by this run"))?;
        let option = option_id.ok_or_else(|| {
            reason(
                MissionErrorCode::InvalidArgument,
                "option_required",
                "choose how to continue the blocked task",
            )
        })?;
        if !decision.options.iter().any(|o| o.id == option) {
            return Err(reason(
                MissionErrorCode::InvalidArgument,
                "option_invalid",
                "choose one of the blocked task's recovery options",
            ));
        }
        match option {
            STOP_MISSION => Ok(ProviderBlockAnswer::Stop),
            RETRY_WITH_INSTRUCTION | CHANGE_MODEL => {
                if option == CHANGE_MODEL {
                    // No automatic substitution: the user reassigns the task
                    // (mission.task.control reassign) before choosing this.
                    let previous = run.binding_snapshot.as_ref().map(|b| &b.id);
                    if task.execution_binding_id().is_none()
                        || task.execution_binding_id() == previous
                    {
                        return Err(reason(
                            MissionErrorCode::InvalidState,
                            "model_not_changed",
                            "reassign the task to a different model before retrying with it",
                        ));
                    }
                }
                let next = self.prepare_task_retry(snapshot, task, None)?;
                Ok(ProviderBlockAnswer::Retry(Box::new(next)))
            }
            REPLAN if task.kind != TaskKind::Plan => {
                let binding = snapshot
                    .mission
                    .role_bindings
                    .iter()
                    .find(|r| r.role == Role::Lead)
                    .map(|r| r.primary_binding_id.clone())
                    .ok_or_else(|| {
                        reason(
                            MissionErrorCode::ModelUnavailable,
                            "lead_missing",
                            "the mission has no Lead binding",
                        )
                    })?;
                if lead_plan_in_progress(snapshot) {
                    return Err(reason(
                        MissionErrorCode::InvalidState,
                        "lead_plan_in_progress",
                        "the Lead is already planning; wait for that plan or send it a message",
                    ));
                }
                if snapshot.tasks.len() >= self.limits.max_tasks_per_mission
                    || snapshot.mission.plan_revision >= self.limits.max_plan_revisions
                {
                    return Err(reason(
                        MissionErrorCode::PlanLimit,
                        "plan_limit",
                        "an alternative plan would exceed the mission's task or plan limit",
                    ));
                }
                let (code, report_ref) = self.blocked_report(snapshot, task, run);
                let report = report_ref
                    .as_ref()
                    .and_then(|reference| {
                        self.artifacts
                            .read_mission_body(
                                &snapshot.mission.id,
                                reference,
                                self.limits.max_context_bytes,
                            )
                            .ok()
                    })
                    .and_then(|bytes| String::from_utf8(bytes).ok());
                let (report_text, report_truncated) = match report.as_deref() {
                    Some(text) => {
                        let (text, truncated) = bounded_text(text, MAX_EMBEDDED_BYTES / 2);
                        (Some(text.to_string()), truncated)
                    }
                    None => (None, false),
                };
                let objective = workflow::store_artifact(
                    &self.artifacts,
                    &snapshot.mission.id,
                    "application/json",
                    json!({
                        "kind": "provider_blocked_replan",
                        "version": 1,
                        "instruction": "An agent reported that the task below is blocked. Propose an alternative plan that still satisfies the original requirements: retire the blocked task with retire_task_ids and add replacement tasks linked with replacement_of, or return a question/blocked result if no safe alternative exists. Do not repeat the blocked approach unchanged. Stay within the mission's allowed roles, models, verification commands and workspace policy.",
                        "blocked_task_id": task.id,
                        "blocked_task_title": task.title,
                        "blocked_run_id": run.id,
                        "code": code,
                        "report": report_text,
                        "report_truncated": report_truncated,
                        "user_instruction": instruction,
                    })
                    .to_string()
                    .as_bytes(),
                )?;
                let mut lead = super::pipeline::task(
                    &snapshot.mission,
                    &snapshot.tasks,
                    TaskKind::Plan,
                    Some(Role::Lead),
                    format!("Alternative plan for blocked task '{}'", task.title),
                    objective,
                    Some(binding.clone()),
                );
                lead.contract.verification_ids =
                    snapshot.mission.policy.allowed_verification_ids.clone();
                lead.state = TaskState::Ready;
                self.validate_task_binding(&snapshot.mission, &lead, &binding)?;
                Ok(ProviderBlockAnswer::Replan(Box::new(lead)))
            }
            _ => Err(reason(
                MissionErrorCode::InvalidArgument,
                "option_invalid",
                "choose one of the blocked task's recovery options",
            )),
        }
    }
}
