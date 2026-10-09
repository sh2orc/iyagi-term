//! Scheduling over real SQLite transactions. Provider execution is tested
//! separately: these tests prove reservations, budgets, snapshots and CAS.
use std::sync::Arc;

use iyagi_termd_lib::mission::{artifacts::ArtifactStore, workflow, MissionService};
use serde_json::json;
use term_contracts::ids::U64String;
use term_contracts::mission::types::*;
use term_storage::mission::types::{ApplyMissionTransition, ApplyMode};
use term_storage::Storage;

#[path = "support/mission_costs.rs"]
mod mission_costs;
#[path = "support/mission_rate_limits.rs"]
mod mission_rate_limits;

struct Rig {
    _dir: tempfile::TempDir,
    storage: Arc<Storage>,
    service: Arc<MissionService>,
    binding: Binding,
}

#[test]
fn dispatch_does_not_promote_legacy_client_capabilities_into_a_new_run() {
    let rig = Rig::new();
    let mut document = rig.storage.mission_bindings().unwrap().remove(0);
    document["runtime"] = json!("codex");
    document["provider_id"] = json!("openai");
    document["auth_route"] = json!("subscription");
    document["runtime_version"] = json!("0.154.0");
    document["checked_at"] = json!("2026-09-16T00:00:00Z");
    document["capabilities"] =
        serde_json::to_value(iyagi_termd_lib::agent_runtime::fake::fake_binding().capabilities)
            .unwrap();
    rig.storage
        .save_mission_binding(
            Id::generate(),
            "legacy-fixture",
            &"c".repeat(64),
            1,
            document,
            "2026-09-16T00:00:00Z".into(),
        )
        .unwrap();
    let id = rig.seed(MissionState::Running, 1, 64);
    rig.service.dispatch_tick().unwrap();
    let snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
    assert!(snapshot.runs.is_empty());
    assert_eq!(
        snapshot.tasks[0].blocked_code.as_deref(),
        Some("capability_structured_result")
    );
    assert_eq!(snapshot.tasks[0].attempt_count, 0);
    assert_eq!(snapshot.mission.automatic_start_count, 0);
    let revision = snapshot.mission.revision;
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    assert_eq!(
        workflow::load_entities(&rig.storage, &id)
            .unwrap()
            .mission
            .revision,
        revision,
        "unchanged unsupported connections must not churn events"
    );
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path().join("state.db")).unwrap());
        let service = Arc::new(MissionService::new(
            storage.clone(),
            ArtifactStore::new(storage.clone(), dir.path().join("missions")),
        ));
        let binding: Binding = serde_json::from_value(json!({
            "id": Id::generate(), "revision": "0", "label": "fixture", "runtime": "fake",
            "program": "/fixture", "runtime_version": null, "provider_id": "fake", "model_id": "explicit-model",
            "effort": null, "auth_route": "local", "credential_ref": null, "endpoint_ref": null,
            "capabilities": iyagi_termd_lib::agent_runtime::claude::claude_capabilities(),
            "checked_at": null, "enabled": true,
            "resource_policy": {"reservation_bytes": "1", "cpu_slots": 1, "enforcement": "observe", "memory_max_bytes": null, "cpu_max_cores": null, "pids_max": null}
        })).unwrap();
        storage
            .save_mission_binding(
                Id::generate(),
                "binding.save",
                &"a".repeat(64),
                0,
                serde_json::to_value(&binding).unwrap(),
                "2026-09-15T00:00:00Z".into(),
            )
            .unwrap();
        Self {
            _dir: dir,
            storage,
            service,
            binding,
        }
    }

    fn seed(&self, state: MissionState, count: usize, budget: u32) -> Id {
        let id = Id::generate();
        let goal = json!({"id": Id::generate(), "sha256": "a".repeat(64), "bytes": "1", "media_type": "text/plain"});
        let mission: Mission = serde_json::from_value(json!({
            "id": id, "revision": "1", "state": state, "phase": "implementing", "title": "dispatch fixture",
            "repository_path": "/fixture", "repository_id": Id::generate(), "base_oid": "a".repeat(40), "goal_ref": goal,
            "requirements": [], "policy": {
                "max_parallel_runs": 4, "max_attempts_per_task": 3, "max_repair_cycles": 3,
                "max_automatic_starts": budget, "active_time_limit_ms": "14400000", "run_time_limit_ms": "2700000",
                "max_cost_usd_micros": null, "unknown_cost": "allow_with_notice", "allow_network": false,
                "allow_automatic_plan_apply": true, "allow_recovery_of_unsent": true,
                "allowed_binding_ids": [self.binding.id], "allowed_roles": ["builder"], "allowed_verification_ids": [],
                "require_independent_review": true, "require_enforced_verification": false
            }, "role_bindings": [], "plan_revision": 0, "candidate_id": null, "open_decision_count": 0,
            "active_time_ms": "0", "automatic_start_count": 0,
            "created_at": "2026-09-15T00:00:00Z", "updated_at": "2026-09-15T00:00:00Z",
            "archived_at": null, "accepted_at": null, "failure_code": null
        })).unwrap();
        let mut upserts = vec![Entity::Mission(Box::new(mission))];
        for ordinal in 0..count {
            let task: Task = serde_json::from_value(json!({
                "id": Id::generate(), "mission_id": id, "title": "writer", "kind": "implement", "role": "builder",
                "state": "ready", "required": true, "parent_task_id": null, "depends_on": [],
                "contract": {"objective_ref": goal, "requirement_ids": [], "input_artifact_ids": [], "allowed_paths": ["src/"], "expected_outputs": ["patch"], "verification_ids": [], "specialty": null},
                "binding_id": self.binding.id, "active_run_id": null, "ordinal": ordinal, "attempt_count": 0, "repair_cycle": 0,
                "replacement_of": null, "blocked_code": null, "workspace_id": null,
                "created_at": "2026-09-15T00:00:00Z", "updated_at": "2026-09-15T00:00:00Z"
            })).unwrap();
            upserts.push(Entity::Task(Box::new(task)));
        }
        self.storage
            .apply_mission_transition(ApplyMissionTransition {
                request_id: Id::generate(),
                method: "fixture".into(),
                fingerprint: "b".repeat(64),
                mission_id: id.clone(),
                mode: ApplyMode::Create,
                transaction_id: Id::generate(),
                event_type: MissionEventType::Created,
                upserts,
                deletes: vec![],
                changes_ref: None,
                outbox: vec![],
                outbox_updates: vec![],
                adopt_staged_artifacts: vec![],
                created_at: "2026-09-15T00:00:00Z".into(),
            })
            .unwrap();
        id
    }
}

#[test]
fn dispatches_multiple_tasks_with_fresh_revisions_and_complete_model_snapshots() {
    let rig = Rig::new();
    let id = rig.seed(MissionState::Running, 3, 64);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 2);
    let snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
    assert_eq!(snapshot.mission.revision.get(), 3);
    assert_eq!(snapshot.mission.automatic_start_count, 2);
    assert_eq!(snapshot.runs.len(), 2);
    for run in &snapshot.runs {
        assert_eq!(run.binding_snapshot.as_ref().unwrap().id, rig.binding.id);
        assert_eq!(run.requested_model.as_deref(), Some("explicit-model"));
        assert_eq!(run.attempt, 1);
        assert_eq!(run.dispatch_state, RunDispatchState::Unsent);
    }
    assert_eq!(rig.storage.mission_outbox().unwrap().len(), 2);
    assert_eq!(
        rig.service.dispatch_tick().unwrap(),
        0,
        "a repeat tick cannot exceed the binding cap"
    );
    assert_eq!(
        workflow::load_entities(&rig.storage, &id)
            .unwrap()
            .mission
            .revision
            .get(),
        3,
        "capacity waits produce no event churn"
    );
}

#[test]
fn pausing_missions_still_consume_shared_binding_slots() {
    let rig = Rig::new();
    let a = rig.seed(MissionState::Running, 2, 64);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 2);
    let mut snapshot = workflow::load_entities(&rig.storage, &a).unwrap();
    snapshot.mission.state = MissionState::Pausing;
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "pause",
        "pause",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    let b = rig.seed(MissionState::Running, 1, 64);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    assert!(workflow::load_entities(&rig.storage, &b)
        .unwrap()
        .runs
        .is_empty());
}

#[test]
fn scans_beyond_the_first_fifty_missions() {
    let rig = Rig::new();
    for _ in 0..55 {
        rig.seed(MissionState::Running, 1, 64);
    }
    let mut cursor = None;
    let mut ordered = vec![];
    loop {
        let (page, next) = rig.storage.mission_list(cursor, 50, false).unwrap();
        ordered.extend(page);
        if next.is_none() {
            break;
        }
        cursor = next;
    }
    let target = ordered.last().unwrap().id.clone();
    for mut mission in ordered.into_iter().filter(|m| m.id != target) {
        mission.state = MissionState::Paused;
        workflow::commit_upserts(
            &rig.service,
            mission,
            "pause",
            "pause",
            MissionEventType::Changed,
            vec![],
        )
        .unwrap();
    }
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    assert_eq!(
        workflow::load_entities(&rig.storage, &target)
            .unwrap()
            .runs
            .len(),
        1
    );
}

#[test]
fn automatic_start_budget_applies_between_dispatches_in_one_tick() {
    let rig = Rig::new();
    let id = rig.seed(MissionState::Running, 2, 1);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    let snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
    assert_eq!(snapshot.mission.automatic_start_count, 1);
    assert!(snapshot
        .tasks
        .iter()
        .any(|task| task.blocked_code.as_deref() == Some("automatic_start_limit")));
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
}

#[test]
fn simultaneous_ticks_share_one_reservation_budget() {
    let rig = Rig::new();
    let id = rig.seed(MissionState::Running, 8, 64);
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let service = rig.service.clone();
            std::thread::spawn(move || service.dispatch_tick().unwrap())
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .sum::<usize>(),
        2
    );
    assert_eq!(
        workflow::load_entities(&rig.storage, &id)
            .unwrap()
            .runs
            .len(),
        2
    );
    assert_eq!(rig.storage.mission_outbox().unwrap().len(), 2);
}

#[test]
fn hard_block_and_stale_ready_dependency_do_not_launch() {
    let rig = Rig::new();
    let id = rig.seed(MissionState::Running, 2, 64);
    let mut snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
    snapshot.tasks[0].state = TaskState::Blocked;
    snapshot.tasks[0].blocked_code = Some("approval_denied".into());
    snapshot.tasks[1].depends_on = vec![snapshot.tasks[0].id.clone()];
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "block",
        "block",
        MissionEventType::Changed,
        snapshot
            .tasks
            .into_iter()
            .map(|t| Entity::Task(Box::new(t)))
            .collect(),
    )
    .unwrap();
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    assert!(rig.storage.mission_outbox().unwrap().is_empty());
}

#[test]
fn missing_model_blocks_once_without_fabricating_a_binding() {
    let rig = Rig::new();
    let id = rig.seed(MissionState::Running, 1, 64);
    let mut snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
    snapshot.tasks[0].binding_id = None;
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "unbind",
        "unbind",
        MissionEventType::Changed,
        vec![Entity::Task(Box::new(snapshot.tasks.remove(0)))],
    )
    .unwrap();
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    let snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
    assert_eq!(
        snapshot.tasks[0].blocked_code.as_deref(),
        Some("binding_missing")
    );
    let revision: U64String = snapshot.mission.revision;
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    assert_eq!(
        workflow::load_entities(&rig.storage, &id)
            .unwrap()
            .mission
            .revision,
        revision
    );
    assert!(rig.storage.mission_outbox().unwrap().is_empty());
}

fn make_verifications(rig: &Rig, id: &Id) {
    let mut snapshot = workflow::load_entities(&rig.storage, id).unwrap();
    snapshot.mission.policy.max_parallel_runs = 20;
    snapshot.mission.phase = Phase::Validating;
    snapshot.mission.candidate_id = Some(Id::generate());
    for task in &mut snapshot.tasks {
        task.kind = TaskKind::Verify;
        task.role = None;
        task.binding_id = None;
    }
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "verify",
        "verify",
        MissionEventType::Changed,
        snapshot
            .tasks
            .into_iter()
            .map(|t| Entity::Task(Box::new(t)))
            .collect(),
    )
    .unwrap();
}

#[test]
fn global_and_hard_mission_caps_cover_bindingless_verification_work() {
    let rig = Rig::new();
    let ids: Vec<_> = (0..3)
        .map(|_| rig.seed(MissionState::Running, 10, 64))
        .collect();
    for id in &ids {
        make_verifications(&rig, id);
    }
    assert_eq!(rig.service.dispatch_tick().unwrap(), 8);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    for id in &ids {
        let snapshot = workflow::load_entities(&rig.storage, id).unwrap();
        assert!(!snapshot.runs.is_empty(), "all missions get a turn");
        assert!(
            snapshot.runs.len() <= 4,
            "policy cannot exceed the hard mission cap"
        );
        assert!(snapshot
            .runs
            .iter()
            .all(|run| run.binding_snapshot.is_none()));
    }
    assert!(rig
        .storage
        .mission_outbox()
        .unwrap()
        .iter()
        .all(
            |intent| intent.operation == term_storage::mission::types::OutboxOperation::Verify
                && intent.payload["binding_id"].is_null()
        ));
}

#[test]
fn mission_cap_remains_four_when_policy_requests_more() {
    let rig = Rig::new();
    let id = rig.seed(MissionState::Running, 10, 64);
    make_verifications(&rig, &id);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 4);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
}

fn control(rig: &Rig, id: &Id, action: &str) {
    let mission = workflow::load_entities(&rig.storage, id).unwrap().mission;
    rig.service.handle(&term_contracts::ids::ConnectionId::generate(), "mission.control", &json!({
        "request_id": Id::generate(), "mission_id": id, "expected_revision": mission.revision, "action": action
    })).unwrap();
}

#[test]
fn pause_holds_unsent_runs_and_cancel_confirms_them_without_starting() {
    let rig = Rig::new();
    let id = rig.seed(MissionState::Running, 3, 64);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 2);
    control(&rig, &id, "pause");
    assert_eq!(
        workflow::load_entities(&rig.storage, &id)
            .unwrap()
            .mission
            .state,
        MissionState::Paused
    );
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    control(&rig, &id, "cancel");
    let snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
    assert_eq!(snapshot.mission.state, MissionState::Cancelled);
    assert!(snapshot
        .runs
        .iter()
        .all(|run| run.state == RunState::Cancelled && run.ended_at.is_some()));
    assert!(snapshot
        .tasks
        .iter()
        .all(|task| task.state == TaskState::Cancelled && task.active_run_id.is_none()));
}

#[test]
fn cancel_preserves_unconfirmed_and_unknown_execution_ownership() {
    let rig = Rig::new();
    let id = rig.seed(MissionState::Running, 2, 64);
    rig.service.dispatch_tick().unwrap();
    let mut snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
    snapshot.runs[0].state = RunState::Running;
    snapshot.runs[0].dispatch_state = RunDispatchState::Acknowledged;
    snapshot.runs[1].state = RunState::Unknown;
    snapshot.runs[1].dispatch_state = RunDispatchState::MayHaveSent;
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "sent",
        "sent",
        MissionEventType::Changed,
        snapshot
            .runs
            .into_iter()
            .map(|run| Entity::Run(Box::new(run)))
            .collect(),
    )
    .unwrap();
    control(&rig, &id, "cancel");
    let snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
    assert_eq!(snapshot.mission.state, MissionState::Stopping);
    assert!(snapshot
        .runs
        .iter()
        .any(|run| run.state == RunState::Unknown));
    assert!(snapshot
        .runs
        .iter()
        .any(|run| run.state == RunState::Stopping));
    assert!(snapshot
        .tasks
        .iter()
        .all(|task| task.active_run_id.is_some()));
    let cancels: Vec<_> = rig
        .storage
        .mission_outbox()
        .unwrap()
        .into_iter()
        .filter(|intent| intent.operation == term_storage::mission::types::OutboxOperation::Cancel)
        .collect();
    assert_eq!(cancels.len(), 2);
    for intent in cancels {
        let run = snapshot
            .runs
            .iter()
            .find(|run| Some(&run.id) == intent.run_id.as_ref())
            .unwrap();
        assert_eq!(intent.fencing_token, run.fencing_token.get());
    }
}

#[test]
fn draft_cancellation_needs_no_provider_execution() {
    let rig = Rig::new();
    let id = rig.seed(MissionState::Draft, 0, 64);
    control(&rig, &id, "cancel");
    assert_eq!(
        workflow::load_entities(&rig.storage, &id)
            .unwrap()
            .mission
            .state,
        MissionState::Cancelled
    );
    assert!(rig.storage.mission_outbox().unwrap().is_empty());
}

#[test]
fn engine_artifact_reads_check_mission_scope_budget_and_actual_content() {
    use term_contracts::mission::MissionErrorCode;
    let rig = Rig::new();
    let a = rig.seed(MissionState::Draft, 0, 64);
    let b = rig.seed(MissionState::Draft, 0, 64);
    let root = rig._dir.path().join("missions");
    let artifacts = ArtifactStore::new(rig.storage.clone(), root.clone());
    let reference = workflow::store_artifact(&artifacts, &a, "text/plain", b"goal").unwrap();
    assert_eq!(
        artifacts.read_mission_body(&a, &reference, 1024).unwrap(),
        b"goal"
    );
    assert_eq!(
        artifacts
            .read_mission_body(&b, &reference, 1024)
            .unwrap_err()
            .0,
        MissionErrorCode::PolicyDenied
    );
    assert_eq!(
        artifacts
            .read_mission_body(&a, &reference, 3)
            .unwrap_err()
            .0,
        MissionErrorCode::ContextTooLarge
    );
    let mut forged = reference.clone();
    forged.bytes = U64String::new(1).unwrap();
    assert_eq!(
        artifacts
            .read_mission_body(&a, &forged, 1024)
            .unwrap_err()
            .0,
        MissionErrorCode::IntegrityFailed
    );
    std::fs::write(
        root.join("artifacts")
            .join(&reference.id.as_str()[..2])
            .join(reference.id.as_str()),
        b"evil",
    )
    .unwrap();
    assert_eq!(
        artifacts
            .read_mission_body(&a, &reference, 1024)
            .unwrap_err()
            .0,
        MissionErrorCode::IntegrityFailed
    );
}

#[test]
fn outbox_claim_and_run_transition_commit_atomically_and_reject_stale_claims() {
    use term_storage::mission::types::{OutboxState, OutboxUpdate};
    let rig = Rig::new();
    let id = rig.seed(MissionState::Running, 1, 64);
    rig.service.dispatch_tick().unwrap();
    let intent = rig.storage.mission_outbox().unwrap().remove(0);
    let claim = |token: u64| {
        let mut snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
        let revision = snapshot.mission.revision.get();
        snapshot.mission.revision = U64String::new(revision + 1).unwrap();
        let mut run = snapshot.runs.remove(0);
        run.state = RunState::Starting;
        run.dispatch_state = RunDispatchState::MayHaveSent;
        rig.storage
            .apply_mission_transition(ApplyMissionTransition {
                request_id: Id::generate(),
                method: "engine.claim".into(),
                fingerprint: "c".repeat(64),
                mission_id: id.clone(),
                mode: ApplyMode::Mutate {
                    expected_revision: revision,
                },
                transaction_id: Id::generate(),
                event_type: MissionEventType::Changed,
                upserts: vec![
                    Entity::Mission(Box::new(snapshot.mission)),
                    Entity::Run(Box::new(run)),
                ],
                deletes: vec![],
                changes_ref: None,
                outbox: vec![],
                outbox_updates: vec![OutboxUpdate {
                    id: intent.id.clone(),
                    expected_state: OutboxState::Prepared,
                    state: OutboxState::Sending,
                    fencing_token: token,
                }],
                adopt_staged_artifacts: vec![],
                created_at: "2026-09-15T00:00:00Z".into(),
            })
    };
    assert!(claim(intent.fencing_token + 1).is_err());
    let unclaimed = workflow::load_entities(&rig.storage, &id).unwrap();
    assert_eq!(unclaimed.mission.revision.get(), 2);
    assert_eq!(unclaimed.runs[0].state, RunState::Prepared);
    assert_eq!(
        rig.storage.mission_outbox().unwrap()[0].state,
        OutboxState::Prepared
    );
    claim(intent.fencing_token).unwrap();
    assert_eq!(
        rig.storage.mission_outbox().unwrap()[0].state,
        OutboxState::Sending
    );
    assert!(
        claim(intent.fencing_token).is_err(),
        "a claimed start may never be claimed again"
    );
    let claimed = workflow::load_entities(&rig.storage, &id).unwrap();
    assert_eq!(
        claimed.mission.revision.get(),
        3,
        "failed claim rolls back its revision and event"
    );
    assert_eq!(claimed.runs[0].state, RunState::Starting);
    assert_eq!(
        claimed.runs[0].dispatch_state,
        RunDispatchState::MayHaveSent
    );
}

#[test]
fn cancelling_a_task_then_its_mission_reuses_the_same_cancel_intent() {
    let rig = Rig::new();
    let id = rig.seed(MissionState::Running, 1, 64);
    rig.service.dispatch_tick().unwrap();
    let mut snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
    snapshot.runs[0].state = RunState::Running;
    snapshot.runs[0].dispatch_state = RunDispatchState::Acknowledged;
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "sent",
        "sent",
        MissionEventType::Changed,
        snapshot
            .runs
            .into_iter()
            .map(|run| Entity::Run(Box::new(run)))
            .collect(),
    )
    .unwrap();
    let snapshot = workflow::load_entities(&rig.storage, &id).unwrap();
    let params = json!({
        "request_id": Id::generate(), "mission_id": id, "expected_revision": snapshot.mission.revision,
        "task_id": snapshot.tasks[0].id, "action": "cancel", "binding_id": null
    });
    let conn = term_contracts::ids::ConnectionId::generate();
    let cancelled = rig
        .service
        .handle(&conn, "mission.task.control", &params)
        .unwrap();
    let replayed = rig
        .service
        .handle(&conn, "mission.task.control", &params)
        .unwrap();
    assert_eq!(cancelled.result, replayed.result);
    let mut conflict = params;
    conflict["binding_id"] = json!(Id::generate());
    assert_eq!(
        rig.service
            .handle(&conn, "mission.task.control", &conflict)
            .err()
            .unwrap()
            .code,
        term_contracts::mission::MissionErrorCode::RequestConflict
    );
    assert_eq!(
        workflow::load_entities(&rig.storage, &id).unwrap().runs[0].state,
        RunState::Stopping
    );
    control(&rig, &id, "cancel");
    let cancels = rig
        .storage
        .mission_outbox()
        .unwrap()
        .into_iter()
        .filter(|intent| intent.operation == term_storage::mission::types::OutboxOperation::Cancel)
        .count();
    assert_eq!(cancels, 1);
    assert_eq!(
        workflow::load_entities(&rig.storage, &id)
            .unwrap()
            .mission
            .state,
        MissionState::Stopping
    );
}
