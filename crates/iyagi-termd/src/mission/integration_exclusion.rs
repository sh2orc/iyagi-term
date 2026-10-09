//! Explicit input exclusion preserves successful history and requires a new plan.
use super::{
    pipeline,
    workflow::{self, MissionEntities},
    MissionService,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use term_contracts::mission::{types::*, MissionErrorCode, MissionRpcError};

pub(super) const EXCLUDE: &str = "exclude_candidate";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ExclusionCheck {
    Planning,
    Integrating,
    Accepting,
}

fn is_check(kind: TaskKind) -> bool {
    matches!(kind, TaskKind::Verify | TaskKind::Review)
}

fn preserves_contract(old: &Task, kind: TaskKind, contract: &TaskContract) -> bool {
    let phase_matches = match old.kind {
        TaskKind::Verify | TaskKind::Review => kind == old.kind,
        _ => kind != TaskKind::Plan && !is_check(kind),
    };
    phase_matches
        && old
            .contract
            .requirement_ids
            .iter()
            .all(|id| contract.requirement_ids.contains(id))
        && old
            .contract
            .verification_ids
            .iter()
            .all(|id| contract.verification_ids.contains(id))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    kind: String,
    version: u32,
    instruction: String,
    decision_id: Id,
    conflicting_candidate_id: Id,
    excluded_task_ids: Vec<Id>,
    excluded_run_ids: Vec<Id>,
    user_answer_ref: Option<ArtifactRef>,
}

pub(super) struct ExclusionAction {
    pub lead: Task,
    pub retired_integration: Task,
    pub evidence_ref: ArtifactRef,
    pub retired_tasks: Vec<Task>,
}

#[derive(Default)]
pub(super) struct Exclusions {
    pub task_ids: HashSet<Id>,
    pub run_ids: HashSet<Id>,
    pub decision_ids: Vec<Id>,
}

fn invalid(message: &str) -> MissionRpcError {
    MissionRpcError::new(MissionErrorCode::InvalidState, message)
}

fn replaces(id: &Id, original: &Id, links: &HashMap<&Id, &Id>) -> bool {
    let mut cursor = id;
    let mut visited = HashSet::new();
    while visited.insert(cursor) {
        let Some(parent) = links.get(cursor) else {
            return false;
        };
        if *parent == original {
            return true;
        }
        cursor = parent;
    }
    false
}

impl MissionService {
    pub(super) fn integration_exclusions(
        &self,
        snapshot: &MissionEntities,
    ) -> Result<Exclusions, MissionRpcError> {
        let mut result = Exclusions::default();
        for decision in snapshot.decisions.iter().filter(|d| {
            d.kind == DecisionKind::Conflict
                && d.state == DecisionState::Answered
                && d.selected_option_id.as_deref() == Some(EXCLUDE)
        }) {
            let message = snapshot
                .messages
                .iter()
                .find(|m| {
                    Some(&m.id) == decision.answer_message_id.as_ref()
                        && m.mission_id == snapshot.mission.id
                        && m.delivery == MessageDelivery::Delivered
                        && m.role == MessageRole::System
                        && m.run_id.is_none()
                        && m.supersedes_message_id.is_none()
                })
                .ok_or_else(|| invalid("exclusion answer evidence is missing"))?;
            let bytes = self
                .artifacts
                .read_mission_body(
                    &snapshot.mission.id,
                    &message.body_ref,
                    self.limits.max_context_bytes,
                )
                .map_err(|(c, m)| MissionRpcError::new(c, m))?;
            let evidence: Evidence = serde_json::from_slice(&bytes)
                .map_err(|_| invalid("invalid exclusion evidence"))?;
            let question: serde_json::Value = serde_json::from_slice(
                &self
                    .artifacts
                    .read_mission_body(
                        &snapshot.mission.id,
                        &decision.question_ref,
                        self.limits.max_context_bytes,
                    )
                    .map_err(|(c, m)| MissionRpcError::new(c, m))?,
            )
            .map_err(|_| invalid("invalid original conflict evidence"))?;
            if evidence.kind != "integration_exclusion"
                || evidence.version != 1
                || evidence.decision_id != decision.id
                || evidence.excluded_task_ids.is_empty()
                || evidence.excluded_run_ids.is_empty()
                || question["conflict"]["candidate_id"]
                    != evidence.conflicting_candidate_id.as_str()
                || !snapshot.tasks.iter().any(|t| {
                    Some(&t.id) == message.target_task_id.as_ref()
                        && t.kind == TaskKind::Plan
                        && t.role == Some(Role::Lead)
                        && t.contract.objective_ref == message.body_ref
                })
                || evidence
                    .excluded_task_ids
                    .iter()
                    .any(|id| !snapshot.tasks.iter().any(|t| &t.id == id))
                || evidence.excluded_run_ids.iter().any(|id| {
                    !snapshot
                        .runs
                        .iter()
                        .any(|r| &r.id == id && evidence.excluded_task_ids.contains(&r.task_id))
                })
                || !snapshot.candidates.iter().any(|c| {
                    c.id == evidence.conflicting_candidate_id
                        && c.source_run_ids
                            .iter()
                            .all(|id| evidence.excluded_run_ids.contains(id))
                })
            {
                return Err(invalid(
                    "exclusion evidence does not match its stored sources",
                ));
            }
            result.task_ids.extend(evidence.excluded_task_ids);
            result.run_ids.extend(evidence.excluded_run_ids);
            result.decision_ids.push(decision.id.clone());
        }
        result
            .decision_ids
            .sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(result)
    }

    pub(super) fn prepare_integration_exclusion(
        &self,
        snapshot: &MissionEntities,
        decision: &Decision,
        user_answer_ref: Option<ArtifactRef>,
    ) -> Result<ExclusionAction, MissionRpcError> {
        let run = snapshot
            .runs
            .iter()
            .find(|r| Some(&r.id) == decision.requesting_run_id.as_ref())
            .ok_or_else(|| invalid("exclusion needs a recorded integration run"))?;
        let task = snapshot
            .tasks
            .iter()
            .find(|t| t.id == run.task_id && t.is_internal_integration())
            .ok_or_else(|| invalid("exclusion needs an internal integration task"))?;
        if decision.state != DecisionState::Open
            || snapshot.mission.phase != Phase::Integrating
            || !matches!(
                snapshot.mission.state,
                MissionState::Running | MissionState::Paused | MissionState::Pausing
            )
            || task.state != TaskState::Failed
            || task.active_run_id.is_some()
            || snapshot.runs.iter().any(|r| r.holds_execution_slot())
            || snapshot
                .runs
                .iter()
                .filter(|r| r.task_id == task.id)
                .max_by_key(|r| r.attempt)
                .map(|r| &r.id)
                != Some(&run.id)
        {
            return Err(invalid("conflict no longer owns a completed integration"));
        }
        let input = self.integration_conflict_input(snapshot, task, &run.id)?;
        let conflicting = &input.plan.sources[input.resume.applied_count - 1];
        let mut excluded: HashSet<Id> = snapshot
            .runs
            .iter()
            .filter(|r| conflicting.source_run_ids.contains(&r.id))
            .map(|r| r.task_id.clone())
            .collect();
        if excluded.is_empty() {
            return Err(invalid("conflicting candidate has no source tasks"));
        }
        loop {
            let prior = excluded.len();
            for task in &snapshot.tasks {
                // Dependency work and repair work based on an integrated tree
                // containing the excluded input must be rebuilt as well.
                let depends = task.depends_on.iter().any(|id| excluded.contains(id));
                let based_on = snapshot
                    .runs
                    .iter()
                    .filter(|r| r.task_id == task.id)
                    .any(|r| {
                        snapshot
                            .workspaces
                            .iter()
                            .filter(|w| Some(&w.id) == r.workspace_id.as_ref())
                            .any(|w| {
                                snapshot
                                    .candidates
                                    .iter()
                                    .filter(|c| c.commit_oid == w.base_oid)
                                    .any(|c| {
                                        snapshot.runs.iter().any(|source| {
                                            c.source_run_ids.contains(&source.id)
                                                && excluded.contains(&source.task_id)
                                        })
                                    })
                            })
                    });
                if !task.is_internal_integration() && (depends || based_on) {
                    excluded.insert(task.id.clone());
                }
            }
            if excluded.len() == prior {
                break;
            }
        }
        if snapshot
            .tasks
            .iter()
            .any(|t| excluded.contains(&t.id) && t.active_run_id.is_some())
        {
            return Err(invalid("excluded work still has execution ownership"));
        }
        for source in snapshot
            .runs
            .iter()
            .filter(|r| excluded.contains(&r.task_id))
        {
            if source.exec_id.as_ref().is_some_and(|id| {
                !snapshot.execs.iter().any(|e| {
                    &e.id == id
                        && e.run_id == source.id
                        && e.state == ExecState::Exited
                        && e.ended_at.is_some()
                })
            }) {
                return Err(invalid(
                    "excluded source execution has not confirmed termination",
                ));
            }
        }
        let cycle = snapshot
            .tasks
            .iter()
            .map(|t| t.repair_cycle)
            .max()
            .unwrap_or(0)
            + 1;
        if cycle > snapshot.mission.policy.max_repair_cycles
            || snapshot.tasks.len() >= self.limits.max_tasks_per_mission
        {
            return Err(MissionRpcError::new(
                MissionErrorCode::PlanLimit,
                "exclusion replan exceeds the task or repair budget",
            ));
        }
        let binding = snapshot
            .mission
            .role_bindings
            .iter()
            .find(|b| b.role == Role::Lead)
            .map(|b| b.primary_binding_id.clone())
            .ok_or_else(|| invalid("Lead binding is missing"))?;
        let mut lead = pipeline::task(
            &snapshot.mission,
            &snapshot.tasks,
            TaskKind::Plan,
            Some(Role::Lead),
            "Replan after excluding conflicting input".into(),
            snapshot.mission.goal_ref.clone(),
            Some(binding.clone()),
        );
        self.validate_task_binding(&snapshot.mission, &lead, &binding)?;
        let mut excluded_task_ids: Vec<_> = excluded.into_iter().collect();
        excluded_task_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut excluded_run_ids: Vec<_> = snapshot
            .runs
            .iter()
            .filter(|r| excluded_task_ids.contains(&r.task_id))
            .map(|r| r.id.clone())
            .collect();
        excluded_run_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let evidence = Evidence { kind:"integration_exclusion".into(), version:1,
            instruction:"The user excluded this conflicting candidate and work depending on it. Preserve all old task/run/candidate records. Propose a new plan that supplies a required replacement_of successor for EACH excluded required non-Plan task, covering its original requirements. A replacement may itself be replaced through its recorded replacement_of chain. Preserve Verify/Review task kinds and required verification command IDs. Verification and review replacements run after the new integration, and must succeed on that new candidate before acceptance. Never depend on excluded tasks or retire successful history. Optional excluded work can be omitted. An empty plan is allowed only when remaining required work already covers every requirement. New work starts from the mission base and retained non-excluded dependencies; verification and review will run on a new candidate.".into(),
            decision_id:decision.id.clone(), conflicting_candidate_id:conflicting.candidate_id.clone(),
            excluded_task_ids, excluded_run_ids, user_answer_ref };
        let evidence_ref = workflow::store_artifact(
            &self.artifacts,
            &snapshot.mission.id,
            "application/json",
            &serde_json::to_vec(&evidence).expect("exclusion evidence"),
        )?;
        lead.contract.objective_ref = evidence_ref.clone();
        lead.repair_cycle = cycle;
        let mut retired_integration = task.clone();
        retired_integration.state = TaskState::Superseded;
        retired_integration.updated_at = term_storage::time::now_iso8601();
        let retired_tasks = snapshot
            .tasks
            .iter()
            .filter(|t| {
                evidence.excluded_task_ids.contains(&t.id)
                    && !matches!(t.state, TaskState::Succeeded | TaskState::Superseded)
            })
            .map(|t| {
                let mut next = t.clone();
                next.state = TaskState::Superseded;
                next.dispatch_after_unix_ms = None;
                next.updated_at = retired_integration.updated_at.clone();
                next
            })
            .collect();
        Ok(ExclusionAction {
            lead,
            retired_integration,
            evidence_ref,
            retired_tasks,
        })
    }

    pub(super) fn validate_exclusion_replacements(
        &self,
        snapshot: &MissionEntities,
        proposal: Option<&PlanProposal>,
        check: ExclusionCheck,
    ) -> Result<(), MissionRpcError> {
        let excluded = self.integration_exclusions(snapshot)?;
        if excluded.task_ids.is_empty() {
            return Ok(());
        }
        // A replacement can itself fail, be retired, or be excluded later.
        // The final required successor must preserve the original coverage.
        let mut links: HashMap<&Id, &Id> = snapshot
            .tasks
            .iter()
            .filter_map(|t| t.replacement_of.as_ref().map(|old| (&t.id, old)))
            .collect();
        if let Some(proposal) = proposal {
            links.extend(
                proposal
                    .tasks
                    .iter()
                    .filter_map(|t| t.replacement_of.as_ref().map(|old| (&t.id, old))),
            );
        }
        for old in snapshot
            .tasks
            .iter()
            .filter(|t| t.required && t.kind != TaskKind::Plan && excluded.task_ids.contains(&t.id))
        {
            let mut existing = false;
            for next in snapshot.tasks.iter().filter(|t| {
                t.required
                    && !excluded.task_ids.contains(&t.id)
                    && replaces(&t.id, &old.id, &links)
                    && !matches!(t.state, TaskState::Cancelled | TaskState::Superseded)
                    && proposal.is_none_or(|p| !p.retire_task_ids.contains(&t.id))
                    && preserves_contract(old, t.kind, &t.contract)
            }) {
                if self.exclusion_replacement_ready(snapshot, next, check)? {
                    existing = true;
                    break;
                }
            }
            let proposed = proposal.is_some_and(|p| {
                p.tasks.iter().any(|t| {
                    t.required
                        && replaces(&t.id, &old.id, &links)
                        && preserves_contract(old, t.kind, &t.contract)
                })
            });
            if !existing && !proposed {
                return Err(super::plan_repair::format_error("every excluded required task needs a required replacement covering its original requirements"));
            }
        }
        if check != ExclusionCheck::Planning
            && snapshot
                .candidates
                .iter()
                .find(|c| Some(&c.id) == snapshot.mission.candidate_id.as_ref())
                .is_some_and(|c| {
                    c.source_run_ids
                        .iter()
                        .any(|id| excluded.run_ids.contains(id))
                })
        {
            return Err(invalid("candidate still includes excluded work"));
        }
        Ok(())
    }

    fn exclusion_replacement_ready(
        &self,
        snapshot: &MissionEntities,
        task: &Task,
        check: ExclusionCheck,
    ) -> Result<bool, MissionRpcError> {
        if check == ExclusionCheck::Planning
            || (check == ExclusionCheck::Integrating && is_check(task.kind))
        {
            // Candidate-bound checks cannot execute until integration has
            // produced the new candidate. Their contracts must already exist.
            return Ok(true);
        }
        if task.state != TaskState::Succeeded {
            return Ok(false);
        }
        if !is_check(task.kind) {
            return Ok(true);
        }
        let Some(candidate) = snapshot
            .candidates
            .iter()
            .find(|c| Some(&c.id) == snapshot.mission.candidate_id.as_ref())
        else {
            return Ok(false);
        };
        if task.kind == TaskKind::Verify {
            return Ok(snapshot.verifications.iter().any(|v| {
                v.task_id == task.id
                    && v.candidate_id == candidate.id
                    && v.status == VerificationStatus::Passed
                    && snapshot.runs.iter().any(|r| {
                        r.id == v.run_id && r.task_id == task.id && r.state == RunState::Succeeded
                    })
            }));
        }
        workflow::review_task_complete(self, snapshot, task, candidate)
    }
}
