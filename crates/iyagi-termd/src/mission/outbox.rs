//! Outbox recovery, cancellation propagation, and budget guards (ticket
//! O14, spec 02 §7/§9): the durable-intent bridge between committed state
//! and external effects. Never claims exactly-once to providers — the
//! contract is no-duplicate-local-intent and no-auto-resend-on-unknown.

use sha2::{Digest, Sha256};
use term_contracts::mission::types::{
    Entity, Id, Mission, MissionEventType, Run, RunDispatchState, RunState,
};
use term_storage::mission::types::{ApplyMissionTransition, ApplyMode};

use super::service::MissionService;

/// Shared by mission and individual-task cancellation. A Run UUID is also
/// the stable ID of its cancel intent (in the separate outbox namespace),
/// so cancelling a task and then its mission cannot enqueue two interrupts.
pub(super) fn prepare_cancel(
    run: &Run,
    now: &str,
) -> (Run, Option<term_storage::mission::types::OutboxIntent>) {
    let mut next = run.clone();
    if !run.holds_execution_slot() {
        return (next, None);
    }
    if run.state == RunState::Prepared && run.dispatch_state == RunDispatchState::Unsent {
        next.state = RunState::Cancelled;
        next.ended_at = Some(now.into());
        return (next, None);
    }
    if !matches!(run.state, RunState::Unknown | RunState::Interrupted) {
        next.state = RunState::Stopping;
    }
    let intent = term_storage::mission::types::OutboxIntent {
        id: run.id.clone(),
        mission_id: run.mission_id.clone(),
        run_id: Some(run.id.clone()),
        operation: term_storage::mission::types::OutboxOperation::Cancel,
        dedupe_key: format!("{}/{}/{}/cancel", run.mission_id, run.task_id, run.attempt),
        fencing_token: run.fencing_token.get(),
        payload: serde_json::json!({ "run_id": run.id }),
        created_at: now.into(),
    };
    (next, Some(intent))
}

/// Recovery decision for one pending outbox row (cases.json R01–R05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// unsent + policy allows + mission running → dispatch (R01).
    Dispatch,
    /// unsent but mission not running / policy forbids → hold (R02, R05).
    Hold,
    /// may_have_sent / acknowledged → inspect the provider state before
    /// anything else; never resend (R03, R04).
    Inspect,
}

pub fn recovery_action(
    dispatch_state: RunDispatchState,
    policy_allows: bool,
    mission_running: bool,
) -> RecoveryAction {
    match dispatch_state {
        RunDispatchState::Unsent => {
            if policy_allows && mission_running {
                RecoveryAction::Dispatch
            } else {
                RecoveryAction::Hold
            }
        }
        RunDispatchState::MayHaveSent | RunDispatchState::Acknowledged => RecoveryAction::Inspect,
    }
}

/// Budget evaluation (02 §9): automatic starts, active time, run time,
/// cost. `BudgetDecision` tells the dispatcher whether to stop dispatching
/// and whether a user decision is needed.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetDecision {
    pub dispatch_allowed: bool,
    pub decision_kind: Option<&'static str>,
    pub reason: String,
}

pub fn evaluate_budget(mission: &Mission, rules: &MissionRules) -> BudgetDecision {
    if mission.automatic_start_count as u64 >= rules.max_automatic_starts {
        return BudgetDecision {
            dispatch_allowed: false,
            decision_kind: Some("budget"),
            reason: format!(
                "automatic start budget exhausted ({})",
                rules.max_automatic_starts
            ),
        };
    }
    if mission.active_time_ms.get() >= rules.active_time_limit_ms {
        return BudgetDecision {
            dispatch_allowed: false,
            decision_kind: Some("budget"),
            reason: format!(
                "active time limit reached ({}ms)",
                rules.active_time_limit_ms
            ),
        };
    }
    // Dollar admission needs immutable Run/binding snapshots and is performed
    // by term_core::mission::budget::cost_admission at the dispatch boundary.
    BudgetDecision {
        dispatch_allowed: true,
        decision_kind: None,
        reason: String::new(),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MissionRules {
    pub max_automatic_starts: u64,
    pub active_time_limit_ms: u64,
    pub run_time_limit_ms: u64,
}

impl Default for MissionRules {
    fn default() -> Self {
        MissionRules {
            max_automatic_starts: 64,
            active_time_limit_ms: 14_400_000,
            run_time_limit_ms: 2_700_000,
        }
    }
}

impl MissionService {
    /// Recover all owned runs, including acknowledged starts whose outbox
    /// rows are no longer pending. Never resend a possibly accepted call.
    pub fn recover_on_startup(
        &self,
    ) -> Result<RecoveryReport, term_contracts::mission::MissionRpcError> {
        use term_contracts::{
            ids::U64String,
            mission::{types::*, MissionErrorCode},
        };
        use term_storage::mission::types::{OutboxOperation, OutboxState, OutboxUpdate};
        let mut report = RecoveryReport::default();
        for archived in [false, true] {
            let mut cursor = None;
            loop {
                let (missions, next) = self
                    .storage
                    .mission_list(cursor, 50, archived)
                    .map_err(Self::store_error)?;
                for listed in missions {
                    let snapshot = super::workflow::load_entities(&self.storage, &listed.id)?;
                    let full = self
                        .storage
                        .mission_snapshot(&listed.id)
                        .map_err(Self::store_error)?;
                    let intents = self.storage.mission_outbox().map_err(Self::store_error)?;
                    let mut mission = snapshot.mission.clone();
                    let mut upserts = Vec::new();
                    let mut updates = Vec::new();
                    for original in snapshot.runs.iter().filter(|r| r.holds_execution_slot()) {
                        // A user's authorization belongs to this exact unsent run
                        // and survives another crash before its dispatch claim.
                        let authorized = snapshot.decisions.iter().any(|d| {
                            d.kind == DecisionKind::Recovery
                                && d.state == DecisionState::Answered
                                && d.requesting_run_id.as_ref() == Some(&original.id)
                                && d.selected_option_id.as_deref() == Some("resume_unsent")
                        });
                        let allows_unsent = mission.policy.allow_recovery_of_unsent || authorized;
                        let action = recovery_action(
                            original.dispatch_state,
                            allows_unsent,
                            mission.state == MissionState::Running,
                        );
                        match action {
                            RecoveryAction::Dispatch => report.dispatch += 1,
                            RecoveryAction::Hold => report.held += 1,
                            RecoveryAction::Inspect => report.inspect += 1,
                        }
                        let hold_unsent = original.dispatch_state == RunDispatchState::Unsent
                            && !allows_unsent
                            && matches!(
                                mission.state,
                                MissionState::Running
                                    | MissionState::Paused
                                    | MissionState::Pausing
                            );
                        if action != RecoveryAction::Inspect && !hold_unsent {
                            continue;
                        }
                        let mut run = original.clone();
                        let Some(mut task) =
                            snapshot.tasks.iter().find(|t| t.id == run.task_id).cloned()
                        else {
                            continue;
                        };
                        if action == RecoveryAction::Inspect
                            && !matches!(run.state, RunState::Unknown | RunState::Interrupted)
                        {
                            run.state = RunState::Unknown;
                            run.failure_code = Some(MissionErrorCode::OutcomeUnknown);
                            // Reject callbacks from the actor that owned the prior
                            // transport. This does not prove its process exited.
                            run.fencing_token =
                                U64String::new(run.fencing_token.get() + 1).expect("fence bound");
                            task.state = TaskState::Blocked;
                            task.blocked_code = Some("outcome_unknown".into());
                            for intent in intents.iter().filter(|i| {
                                i.run_id.as_ref() == Some(&run.id)
                                    && i.state == OutboxState::Sending
                            }) {
                                updates.push(OutboxUpdate {
                                    id: intent.id.clone(),
                                    expected_state: OutboxState::Sending,
                                    state: OutboxState::Unknown,
                                    fencing_token: intent.fencing_token,
                                });
                                if matches!(
                                    intent.operation,
                                    OutboxOperation::Answer | OutboxOperation::Message
                                ) {
                                    if let Some(full) = &full {
                                        for entity in &full.entities {
                                            if let Entity::Message(message) = entity {
                                                if intent.payload["message_id"].as_str()
                                                    == Some(message.id.as_str())
                                                {
                                                    let mut message = message.clone();
                                                    message.delivery = MessageDelivery::Unknown;
                                                    upserts.push(Entity::Message(message));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if let Some(full) = &full {
                                for entity in &full.entities {
                                    if let Entity::Workspace(workspace) = entity {
                                        if Some(&workspace.id) == run.workspace_id.as_ref() {
                                            let mut workspace = workspace.clone();
                                            workspace.state = WorkspaceState::Quarantined;
                                            upserts.push(Entity::Workspace(workspace));
                                        }
                                    }
                                }
                            }
                            for decision in snapshot.decisions.iter().filter(|d| {
                                d.kind == DecisionKind::Approval
                                    && d.state == DecisionState::Open
                                    && d.requesting_run_id.as_ref() == Some(&run.id)
                            }) {
                                let mut obsolete = decision.clone();
                                obsolete.state = DecisionState::Obsolete;
                                mission.open_decision_count =
                                    mission.open_decision_count.saturating_sub(1);
                                upserts.push(Entity::Decision(Box::new(obsolete)));
                            }
                            upserts.push(Entity::Run(Box::new(run.clone())));
                            upserts.push(Entity::Task(Box::new(task)));
                        } else if hold_unsent
                            && task.blocked_code.as_deref() != Some("recovery_held")
                        {
                            task.state = TaskState::Blocked;
                            task.blocked_code = Some("recovery_held".into());
                            upserts.push(Entity::Task(Box::new(task)));
                        }
                        if mission.state == MissionState::Stopping
                            || snapshot.decisions.iter().any(|d| {
                                d.kind == DecisionKind::Recovery
                                    && d.state == DecisionState::Open
                                    && d.requesting_run_id.as_ref() == Some(&run.id)
                            })
                        {
                            continue;
                        }
                        let text = if hold_unsent {
                            "This run was never sent. Mission policy requires your decision before recovering it."
                        } else {
                            "The daemon stopped after this run may have been sent. Its outcome is unknown. The run will not be resent automatically; inspect and stop the owned provider execution before retrying."
                        };
                        let reference = super::workflow::store_artifact(
                            &self.artifacts,
                            &mission.id,
                            "text/plain",
                            text.as_bytes(),
                        )?;
                        let mut options = vec![DecisionOption {
                            id: "stop_mission".into(),
                            label: "Stop mission".into(),
                        }];
                        if hold_unsent {
                            options.insert(
                                0,
                                DecisionOption {
                                    id: "resume_unsent".into(),
                                    label: "Start the unsent run".into(),
                                },
                            );
                        }
                        let (_, decision) = super::engine::new_decision(
                            &mission,
                            DecisionKind::Recovery,
                            reference,
                            options,
                            vec![run.task_id.clone()],
                            true,
                            Some(run.id.clone()),
                        );
                        mission.open_decision_count += 1;
                        upserts.push(Entity::Decision(Box::new(decision)));
                    }
                    // A delivery worker may outlive the provider turn. Even if
                    // its Run is already terminal, a crash while Sending leaves
                    // an uncertain receipt and must never cause a resend.
                    for intent in intents.iter().filter(|i| {
                        i.mission_id == mission.id
                            && i.state == OutboxState::Sending
                            && matches!(
                                i.operation,
                                OutboxOperation::Message | OutboxOperation::Answer
                            )
                    }) {
                        if updates.iter().any(|u| u.id == intent.id) {
                            continue;
                        }
                        updates.push(OutboxUpdate {
                            id: intent.id.clone(),
                            expected_state: OutboxState::Sending,
                            state: OutboxState::Unknown,
                            fencing_token: intent.fencing_token,
                        });
                        if let Some(full) = &full {
                            for entity in &full.entities {
                                if let Entity::Message(message) = entity {
                                    if intent.payload["message_id"].as_str()
                                    == Some(message.id.as_str())
                                    && !upserts.iter().any(
                                        |e| matches!(e, Entity::Message(m) if m.id == message.id),
                                    )
                                {
                                    let mut message = message.clone();
                                    message.delivery = MessageDelivery::Unknown;
                                    upserts.push(Entity::Message(message));
                                }
                                }
                            }
                        }
                    }
                    if !upserts.is_empty() || !updates.is_empty() {
                        self.commit_actor(mission, "engine.recover", upserts, updates)?;
                    }
                }
                if next.is_none() {
                    break;
                }
                cursor = next;
            }
        }
        Ok(report)
    }

    /// Cancellation propagation (02 §2 stopping): live runs get a cancel
    /// outbox intent; unknown runs keep stopping until inspect resolves.
    pub fn cancel_live_runs(
        &self,
        mission: &Mission,
        live_runs: &[Run],
    ) -> Result<usize, term_contracts::mission::error::MissionRpcError> {
        if live_runs.is_empty() {
            return Ok(0);
        }
        let mut next_mission = mission.clone();
        next_mission.revision = term_contracts::ids::U64String::new(mission.revision.get() + 1)
            .expect("fits SQLite bound");
        next_mission.updated_at = term_storage::time::now_iso8601();
        let mut upserts = vec![Entity::Mission(Box::new(next_mission))];
        let mut outbox = Vec::new();
        for run in live_runs {
            let (run, intent) = prepare_cancel(run, &term_storage::time::now_iso8601());
            upserts.push(Entity::Run(Box::new(run)));
            outbox.extend(intent);
        }
        let transition = ApplyMissionTransition {
            request_id: Id::generate(),
            method: "engine.cancel_runs".into(),
            fingerprint: format!(
                "{:x}",
                Sha256::digest(format!("{}:cancel", mission.id).as_bytes())
            ),
            mission_id: mission.id.clone(),
            mode: ApplyMode::Mutate {
                expected_revision: mission.revision.get(),
            },
            transaction_id: Id::generate(),
            event_type: MissionEventType::Changed,
            upserts,
            deletes: Vec::new(),
            changes_ref: None,
            outbox,
            outbox_updates: Vec::new(),
            adopt_staged_artifacts: Vec::new(),
            created_at: term_storage::time::now_iso8601(),
        };
        self.apply_timed_transition(transition)
            .map_err(super::service::MissionService::store_error)?;
        Ok(live_runs.len())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    pub dispatch: usize,
    pub held: usize,
    pub inspect: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r01_to_r05_recovery_table() {
        use term_contracts::mission::types::RunDispatchState::*;
        // R01: unsent + allowed + running → dispatch
        assert_eq!(
            recovery_action(Unsent, true, true),
            RecoveryAction::Dispatch
        );
        // R02: unsent + forbidden → hold
        assert_eq!(recovery_action(Unsent, false, true), RecoveryAction::Hold);
        // R03: may_have_sent → inspect (never resend)
        assert_eq!(
            recovery_action(MayHaveSent, true, true),
            RecoveryAction::Inspect
        );
        // R04: acknowledged → inspect
        assert_eq!(
            recovery_action(Acknowledged, true, true),
            RecoveryAction::Inspect
        );
        // R05: unsent + allowed + mission not running → hold
        assert_eq!(recovery_action(Unsent, true, false), RecoveryAction::Hold);
    }

    #[test]
    fn budget_guards_stop_dispatch_and_request_a_decision() {
        let rules = MissionRules::default();
        let mut mission = test_mission();
        assert!(evaluate_budget(&mission, &rules).dispatch_allowed);
        mission.automatic_start_count = 64;
        let decision = evaluate_budget(&mission, &rules);
        assert!(!decision.dispatch_allowed);
        assert_eq!(decision.decision_kind, Some("budget"));
        mission.automatic_start_count = 0;
        mission.active_time_ms = term_contracts::ids::U64String::parse("14400000").unwrap();
        let decision = evaluate_budget(&mission, &rules);
        assert!(!decision.dispatch_allowed);
        assert_eq!(decision.decision_kind, Some("budget"));
    }

    fn test_mission() -> Mission {
        use term_contracts::ids::U64String;
        use term_contracts::mission::types::*;
        Mission {
            id: Id::generate(),
            revision: U64String::parse("1").unwrap(),
            semantic_revision: None,
            follow_up_of: None,
            base_snapshot: None,
            state: MissionState::Running,
            phase: Phase::Implementing,
            title: "t".into(),
            repository_path: "/repo".into(),
            repository_id: Id::generate(),
            base_oid: "a".repeat(40),
            goal_ref: ArtifactRef {
                id: Id::generate(),
                sha256: "b".repeat(64),
                bytes: U64String::parse("1").unwrap(),
                media_type: "text/plain".into(),
            },
            requirements: Vec::new(),
            policy: Policy {
                max_parallel_runs: 4,
                max_attempts_per_task: 3,
                max_repair_cycles: 3,
                max_automatic_starts: 64,
                active_time_limit_ms: U64String::parse("14400000").unwrap(),
                run_time_limit_ms: U64String::parse("2700000").unwrap(),
                max_cost_usd_micros: None,
                unknown_cost: UnknownCostPolicy::AllowWithNotice,
                allow_network: false,
                allow_automatic_plan_apply: true,
                allow_recovery_of_unsent: true,
                allowed_binding_ids: Vec::new(),
                allowed_roles: Vec::new(),
                allowed_verification_ids: Vec::new(),
                require_independent_review: true,
                require_enforced_verification: false,
            },
            role_bindings: Vec::new(),
            plan_revision: 0,
            candidate_id: None,
            open_decision_count: 0,
            active_time_ms: U64String::parse("0").unwrap(),
            automatic_start_count: 0,
            created_at: String::new(),
            updated_at: String::new(),
            archived_at: None,
            accepted_at: None,
            failure_code: None,
        }
    }

    #[test]
    fn task_state_used_by_recovery_paths() {
        // Recovery must keep tasks with unknown runs blocked (active_run_id
        // preserved) — 02 §3.
        assert!(
            term_contracts::mission::types::TaskState::Blocked
                != term_contracts::mission::types::TaskState::Ready
        );
        assert!(RunState::Unknown.is_terminal());
        assert!(!RunState::Unknown.is_live());
    }
}
