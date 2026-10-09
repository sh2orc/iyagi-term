//! O1 mission storage transaction tests (ticket O03): duplicate requests,
//! request conflicts, revision CAS, single-live-run, snapshot/event reads,
//! and R1 row preservation alongside the orch tables.

use std::sync::Arc;

use term_contracts::ids::U64String;
use term_contracts::mission::types::{
    ArtifactRef, Change, Entity, Id, Mission, MissionEventType, MissionState, Phase, Policy, Role,
    RoleBinding, Run, RunState, Task, TaskContract, TaskKind, TaskState, UnknownCostPolicy,
};
use term_storage::mission::types::{
    ApplyMissionTransition, ApplyMode, MissionStoreError, OutboxIntent, OutboxOperation,
};
use term_storage::Storage;

fn now() -> String {
    "2026-09-13T00:00:00Z".into()
}

fn artifact(tag: u8) -> ArtifactRef {
    ArtifactRef {
        id: Id::generate(),
        sha256: format!("{tag:02x}").repeat(32),
        bytes: U64String::new(tag as u64 + 1).unwrap(),
        media_type: "text/plain".into(),
    }
}

fn policy() -> Policy {
    Policy {
        max_parallel_runs: 4,
        max_attempts_per_task: 3,
        max_repair_cycles: 3,
        max_automatic_starts: 64,
        active_time_limit_ms: U64String::new(14_400_000).unwrap(),
        run_time_limit_ms: U64String::new(2_700_000).unwrap(),
        max_cost_usd_micros: None,
        unknown_cost: UnknownCostPolicy::AllowWithNotice,
        allow_network: false,
        allow_automatic_plan_apply: true,
        allow_recovery_of_unsent: true,
        allowed_binding_ids: Vec::new(),
        allowed_roles: vec![Role::Lead, Role::Builder, Role::Reviewer],
        allowed_verification_ids: Vec::new(),
        require_independent_review: true,
        require_enforced_verification: false,
    }
}

fn mission(id: &Id, revision: u64) -> Mission {
    Mission {
        id: id.clone(),
        revision: U64String::new(revision).unwrap(),
        semantic_revision: None,
        follow_up_of: None,
        base_snapshot: None,
        state: MissionState::Running,
        phase: Phase::Implementing,
        title: "로그인 기능".into(),
        repository_path: "/fixture/repo".into(),
        repository_id: Id::generate(),
        base_oid: "a".repeat(40),
        goal_ref: artifact(1),
        requirements: Vec::new(),
        policy: policy(),
        role_bindings: vec![RoleBinding {
            role: Role::Lead,
            primary_binding_id: Id::generate(),
            fallback_binding_ids: Vec::new(),
        }],
        plan_revision: 1,
        candidate_id: None,
        open_decision_count: 0,
        active_time_ms: U64String::new(0).unwrap(),
        automatic_start_count: 0,
        created_at: now(),
        updated_at: now(),
        archived_at: None,
        accepted_at: None,
        failure_code: None,
    }
}

fn task(id: &Id, mission_id: &Id, ordinal: u32) -> Task {
    Task {
        id: id.clone(),
        mission_id: mission_id.clone(),
        title: "API 구현".into(),
        kind: TaskKind::Implement,
        role: Some(Role::Builder),
        state: TaskState::Ready,
        required: true,
        parent_task_id: None,
        depends_on: Vec::new(),
        contract: TaskContract {
            objective_ref: artifact(2),
            requirement_ids: Vec::new(),
            input_artifact_ids: Vec::new(),
            allowed_paths: vec!["src/api/".into()],
            expected_outputs: vec![term_contracts::mission::types::ExpectedOutput::Patch],
            verification_ids: Vec::new(),
            specialty: None,
        },
        binding_id: Some(Id::generate()),
        active_run_id: None,
        ordinal,
        attempt_count: 0,
        repair_cycle: 0,
        failure_repair_run_ids: vec![],
        integration: None,
        replacement_of: None,
        blocked_code: None,
        dispatch_after_unix_ms: None,
        workspace_id: None,
        created_at: now(),
        updated_at: now(),
    }
}

fn run(id: &Id, mission_id: &Id, task_id: &Id, attempt: u32, state: RunState) -> Run {
    Run {
        id: id.clone(),
        mission_id: mission_id.clone(),
        task_id: task_id.clone(),
        attempt,
        state,
        binding_snapshot: None,
        requested_model: None,
        observed_model: None,
        provider_session_id: None,
        provider_turn_id: None,
        exec_id: None,
        pty_session_id: None,
        workspace_id: None,
        fencing_token: U64String::new(1).unwrap(),
        dispatch_state: term_contracts::mission::types::RunDispatchState::Unsent,
        context_ref: artifact(3),
        result_ref: None,
        usage: term_contracts::mission::validation::unknown_usage(),
        last_activity_at: None,
        active_time_ms: U64String::new(0).unwrap(),
        started_at: None,
        ended_at: None,
        failure_code: None,
        reconciliation_ref: None,
        reconciliation_kind: None,
        rate_limit: None,
        retry_evidence: None,
    }
}

fn seed(storage: &Storage) -> (Id, Id, Id) {
    let mission_id = Id::generate();
    let task_id = Id::generate();
    let run_id = Id::generate();
    let transition = ApplyMissionTransition {
        request_id: Id::generate(),
        method: "mission.control".into(),
        fingerprint: "1".repeat(64),
        mission_id: mission_id.clone(),
        mode: ApplyMode::Create,
        transaction_id: Id::generate(),
        event_type: MissionEventType::Created,
        upserts: vec![
            Entity::Mission(Box::new(mission(&mission_id, 1))),
            Entity::Task(Box::new(task(&task_id, &mission_id, 0))),
        ],
        deletes: Vec::new(),
        changes_ref: None,
        outbox: Vec::new(),
        outbox_updates: Vec::new(),
        adopt_staged_artifacts: Vec::new(),
        created_at: now(),
    };
    let applied = storage.apply_mission_transition(transition).unwrap();
    assert!(!applied.replayed);
    assert_eq!(applied.result.revision.get(), 1);
    (mission_id, task_id, run_id)
}

fn fingerprint(seed: u8) -> String {
    format!("{seed:02x}").repeat(32)
}

#[test]
fn e01_duplicate_request_replays_first_response_without_new_events() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("m.db")).unwrap();
    let (mission_id, task_id, _) = seed(&storage);

    let request_id = Id::generate();
    let transition = |fingerprint: String, request: Id| ApplyMissionTransition {
        request_id: request,
        method: "mission.task.control".into(),
        fingerprint,
        mission_id: mission_id.clone(),
        mode: ApplyMode::Mutate {
            expected_revision: 1,
        },
        transaction_id: Id::generate(),
        event_type: MissionEventType::RunDispatched,
        upserts: vec![
            Entity::Mission(Box::new(mission(&mission_id, 2))),
            Entity::Task(Box::new({
                let mut t = task(&task_id, &mission_id, 0);
                t.state = TaskState::Running;
                t.attempt_count = 1;
                t
            })),
            Entity::Run(Box::new(run(
                &Id::generate(),
                &mission_id,
                &task_id,
                1,
                RunState::Prepared,
            ))),
        ],
        deletes: Vec::new(),
        changes_ref: None,
        outbox_updates: Vec::new(),
        adopt_staged_artifacts: Vec::new(),
        outbox: vec![OutboxIntent {
            id: Id::generate(),
            mission_id: mission_id.clone(),
            run_id: None,
            operation: OutboxOperation::Start,
            dedupe_key: format!("{mission_id}/{task_id}/1/start"),
            fencing_token: 1,
            payload: serde_json::json!({}),
            created_at: now(),
        }],
        created_at: now(),
    };

    let first = storage
        .apply_mission_transition(transition(fingerprint(1), request_id.clone()))
        .unwrap();
    assert!(!first.replayed);
    assert_eq!(first.result.revision.get(), 2);

    // Same request id + identical payload → stored first response, one event.
    let replay = storage
        .apply_mission_transition(transition(fingerprint(1), request_id.clone()))
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.result, first.result);

    let (events, watermark) = storage.mission_events(&mission_id, 0, 50).unwrap();
    assert_eq!(events.len(), 2, "no extra events for the replay");
    assert_eq!(watermark, 2);
    // Exactly one run and one outbox row.
    let snapshot = storage.mission_snapshot(&mission_id).unwrap().unwrap();
    assert_eq!(
        snapshot
            .entities
            .iter()
            .filter(|e| matches!(e, Entity::Run(_)))
            .count(),
        1
    );
    assert_eq!(storage.mission_outbox().unwrap().len(), 1);
}

#[test]
fn e02_same_request_id_different_payload_is_conflict_with_no_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("m.db")).unwrap();
    let (mission_id, task_id, _) = seed(&storage);
    let request_id = Id::generate();
    let build = |fp: String, request: Id| ApplyMissionTransition {
        request_id: request,
        method: "mission.task.control".into(),
        fingerprint: fp,
        mission_id: mission_id.clone(),
        mode: ApplyMode::Mutate {
            expected_revision: 1,
        },
        transaction_id: Id::generate(),
        event_type: MissionEventType::Changed,
        upserts: vec![
            Entity::Mission(Box::new(mission(&mission_id, 2))),
            Entity::Task(Box::new(task(&task_id, &mission_id, 0))),
        ],
        deletes: Vec::new(),
        changes_ref: None,
        outbox: Vec::new(),
        outbox_updates: Vec::new(),
        adopt_staged_artifacts: Vec::new(),
        created_at: now(),
    };
    storage
        .apply_mission_transition(build(fingerprint(1), request_id.clone()))
        .unwrap();
    let err = storage
        .apply_mission_transition(build(fingerprint(2), request_id))
        .unwrap_err();
    assert!(matches!(err, MissionStoreError::RequestConflict(_)));
    // No third revision / no extra event.
    assert_eq!(
        storage
            .mission_snapshot(&mission_id)
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    let (events, _) = storage.mission_events(&mission_id, 0, 50).unwrap();
    assert_eq!(events.len(), 2);
}

#[test]
fn e03_revision_cas_allows_exactly_one_of_two_writers() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("m.db")).unwrap();
    let (mission_id, task_id, _) = seed(&storage);
    let build = |expected: u64, tag: u8| ApplyMissionTransition {
        request_id: Id::generate(),
        method: "mission.message".into(),
        fingerprint: fingerprint(tag),
        mission_id: mission_id.clone(),
        mode: ApplyMode::Mutate {
            expected_revision: expected,
        },
        transaction_id: Id::generate(),
        event_type: MissionEventType::Changed,
        upserts: vec![
            Entity::Mission(Box::new(mission(&mission_id, expected + 1))),
            Entity::Task(Box::new(task(&task_id, &mission_id, tag as u32))),
        ],
        deletes: Vec::new(),
        changes_ref: None,
        outbox: Vec::new(),
        outbox_updates: Vec::new(),
        adopt_staged_artifacts: Vec::new(),
        created_at: now(),
    };
    storage.apply_mission_transition(build(1, 1)).unwrap();
    let conflict = storage.apply_mission_transition(build(1, 2)).unwrap_err();
    match conflict {
        MissionStoreError::RevisionConflict {
            current_revision,
            expected_revision,
        } => {
            assert_eq!(current_revision, 2);
            assert_eq!(expected_revision, 1);
        }
        other => panic!("expected revision conflict, got {other:?}"),
    }
}

#[test]
fn e06_two_live_runs_on_one_task_second_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("m.db")).unwrap();
    let (mission_id, task_id, _) = seed(&storage);
    let first_run = Id::generate();
    let second_run = Id::generate();
    // The scheduler re-reads the revision between ticks; both dispatches
    // pass CAS so the single-live-run guard is what rejects the second.
    let build = |run_id: &Id, expected: u64| ApplyMissionTransition {
        request_id: Id::generate(),
        method: "engine.dispatch".into(),
        fingerprint: "f".repeat(64),
        mission_id: mission_id.clone(),
        mode: ApplyMode::Mutate {
            expected_revision: expected,
        },
        transaction_id: Id::generate(),
        event_type: MissionEventType::RunDispatched,
        upserts: vec![
            Entity::Mission(Box::new(mission(&mission_id, expected + 1))),
            Entity::Run(Box::new(run(
                run_id,
                &mission_id,
                &task_id,
                1,
                RunState::Prepared,
            ))),
        ],
        deletes: Vec::new(),
        changes_ref: None,
        outbox: Vec::new(),
        outbox_updates: Vec::new(),
        adopt_staged_artifacts: Vec::new(),
        created_at: now(),
    };
    storage
        .apply_mission_transition(build(&first_run, 1))
        .unwrap();
    let rejected = storage
        .apply_mission_transition(build(&second_run, 2))
        .unwrap_err();
    assert!(matches!(rejected, MissionStoreError::InvalidState(_)));
    // Only the first run exists.
    let snapshot = storage.mission_snapshot(&mission_id).unwrap().unwrap();
    assert_eq!(
        snapshot
            .entities
            .iter()
            .filter(|e| matches!(e, Entity::Run(_)))
            .count(),
        1
    );
    // Terminal runs release the slot: after the first run finishes, another
    // live run is accepted.
    storage
        .apply_mission_transition(ApplyMissionTransition {
            request_id: Id::generate(),
            method: "engine.finish".into(),
            fingerprint: "a".repeat(64),
            mission_id: mission_id.clone(),
            mode: ApplyMode::Mutate {
                expected_revision: 2,
            },
            transaction_id: Id::generate(),
            event_type: MissionEventType::Changed,
            upserts: vec![
                Entity::Mission(Box::new(mission(&mission_id, 3))),
                Entity::Run(Box::new(run(
                    &first_run,
                    &mission_id,
                    &task_id,
                    1,
                    RunState::Succeeded,
                ))),
            ],
            deletes: Vec::new(),
            changes_ref: None,
            outbox: Vec::new(),
            outbox_updates: Vec::new(),
            adopt_staged_artifacts: Vec::new(),
            created_at: now(),
        })
        .unwrap();
    storage
        .apply_mission_transition(ApplyMissionTransition {
            request_id: Id::generate(),
            method: "engine.dispatch".into(),
            fingerprint: "b".repeat(64),
            mission_id: mission_id.clone(),
            mode: ApplyMode::Mutate {
                expected_revision: 3,
            },
            transaction_id: Id::generate(),
            event_type: MissionEventType::RunDispatched,
            upserts: vec![
                Entity::Mission(Box::new(mission(&mission_id, 4))),
                Entity::Run(Box::new(run(
                    &second_run,
                    &mission_id,
                    &task_id,
                    2,
                    RunState::Prepared,
                ))),
            ],
            deletes: Vec::new(),
            changes_ref: None,
            outbox: Vec::new(),
            outbox_updates: Vec::new(),
            adopt_staged_artifacts: Vec::new(),
            created_at: now(),
        })
        .unwrap();
}

#[test]
fn e03_writer_serializes_concurrent_applies_for_the_same_revision() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path().join("m.db")).unwrap());
    let (mission_id, task_id, _) = seed(&storage);
    let mut handles = Vec::new();
    for tag in 1..=4u8 {
        let storage = Arc::clone(&storage);
        let mission_id = mission_id.clone();
        let task_id = task_id.clone();
        handles.push(std::thread::spawn(move || {
            storage.apply_mission_transition(ApplyMissionTransition {
                request_id: Id::generate(),
                method: "mission.message".into(),
                fingerprint: fingerprint(tag),
                mission_id: mission_id.clone(),
                mode: ApplyMode::Mutate {
                    expected_revision: 1,
                },
                transaction_id: Id::generate(),
                event_type: MissionEventType::Changed,
                upserts: vec![
                    Entity::Mission(Box::new(mission(&mission_id, 2))),
                    Entity::Task(Box::new(task(&task_id, &mission_id, tag as u32))),
                ],
                deletes: Vec::new(),
                changes_ref: None,
                outbox: Vec::new(),
                outbox_updates: Vec::new(),
                adopt_staged_artifacts: Vec::new(),
                created_at: now(),
            })
        }));
    }
    let mut ok = 0;
    let mut conflicts = 0;
    for handle in handles {
        match handle.join().unwrap() {
            Ok(_) => ok += 1,
            Err(MissionStoreError::RevisionConflict { .. }) => conflicts += 1,
            Err(other) => panic!("unexpected error {other:?}"),
        }
    }
    assert_eq!(ok, 1, "exactly one writer commits revision 2");
    assert_eq!(conflicts, 3);
    assert_eq!(
        storage
            .mission_snapshot(&mission_id)
            .unwrap()
            .unwrap()
            .revision,
        2
    );
}

#[test]
fn create_rejects_existing_mission_and_mutate_rejects_missing() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("m.db")).unwrap();
    let (mission_id, task_id, _) = seed(&storage);
    let err = storage
        .apply_mission_transition(ApplyMissionTransition {
            request_id: Id::generate(),
            method: "mission.create".into(),
            fingerprint: "c".repeat(64),
            mission_id: mission_id.clone(),
            mode: ApplyMode::Create,
            transaction_id: Id::generate(),
            event_type: MissionEventType::Created,
            upserts: vec![
                Entity::Mission(Box::new(mission(&mission_id, 1))),
                Entity::Task(Box::new(task(&task_id, &mission_id, 0))),
            ],
            deletes: Vec::new(),
            changes_ref: None,
            outbox: Vec::new(),
            outbox_updates: Vec::new(),
            adopt_staged_artifacts: Vec::new(),
            created_at: now(),
        })
        .unwrap_err();
    assert!(matches!(err, MissionStoreError::InvalidState(_)));

    let missing = storage
        .apply_mission_transition(ApplyMissionTransition {
            request_id: Id::generate(),
            method: "mission.control".into(),
            fingerprint: "d".repeat(64),
            mission_id: Id::generate(),
            mode: ApplyMode::Mutate {
                expected_revision: 1,
            },
            transaction_id: Id::generate(),
            event_type: MissionEventType::Changed,
            upserts: vec![],
            deletes: vec![Change {
                entity_kind: term_contracts::mission::types::EntityKind::Task,
                entity_id: Id::generate(),
                operation: term_contracts::mission::types::ChangeOperation::Delete,
            }],
            changes_ref: None,
            outbox: Vec::new(),
            outbox_updates: Vec::new(),
            adopt_staged_artifacts: Vec::new(),
            created_at: now(),
        })
        .unwrap_err();
    assert!(matches!(missing, MissionStoreError::NotFound { .. }));
}

#[test]
fn mission_read_keeps_one_wal_snapshot_while_the_writer_commits() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("m.db")).unwrap();
    let (mission_id, task_id, _) = seed(&storage);
    storage
        .read_mission(|conn| {
            let before = term_storage::mission::queries::materialize(conn, &mission_id)?.unwrap();
            assert_eq!(before.revision, 1);
            let mut updated = task(&task_id, &mission_id, 0);
            updated.title = "committed during the read".into();
            storage.apply_mission_transition(ApplyMissionTransition {
                request_id: Id::generate(),
                method: "engine.concurrent_write".into(),
                fingerprint: "8".repeat(64),
                mission_id: mission_id.clone(),
                mode: ApplyMode::Mutate {
                    expected_revision: 1,
                },
                transaction_id: Id::generate(),
                event_type: MissionEventType::Changed,
                upserts: vec![
                    Entity::Mission(Box::new(mission(&mission_id, 2))),
                    Entity::Task(Box::new(updated)),
                ],
                deletes: vec![],
                changes_ref: None,
                outbox: vec![],
                outbox_updates: vec![],
                adopt_staged_artifacts: vec![],
                created_at: now(),
            })?;
            let during = term_storage::mission::queries::materialize(conn, &mission_id)?.unwrap();
            assert_eq!(during.revision, before.revision);
            assert_eq!(during.event_seq, before.event_seq);
            assert_eq!(during.entities, before.entities);
            let (events, watermark) =
                term_storage::mission::queries::events_after(conn, &mission_id, 0, 50)?;
            assert_eq!(events.len(), 1);
            assert_eq!(watermark, 1);
            Ok(())
        })
        .unwrap();
    let after = storage.mission_snapshot(&mission_id).unwrap().unwrap();
    assert_eq!(after.revision, 2);
    assert_eq!(after.event_seq, 2);
    assert!(after.entities.iter().any(
        |entity| matches!(entity, Entity::Task(task) if task.title == "committed during the read")
    ));
}

#[test]
fn mission_read_releases_its_transaction_after_error_and_panic() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("m.db")).unwrap();
    let (mission_id, _, _) = seed(&storage);
    let error = storage.read_mission::<()>(|conn| {
        term_storage::mission::queries::materialize(conn, &mission_id)?;
        Err(MissionStoreError::Corrupt("injected read error".into()))
    });
    assert!(error.is_err());
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        storage.read_mission::<()>(|conn| {
            term_storage::mission::queries::materialize(conn, &mission_id)?;
            panic!("injected reader panic");
        })
    }));
    assert!(panic.is_err());
    assert_eq!(
        storage
            .mission_snapshot(&mission_id)
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}

#[test]
fn snapshot_materializes_all_entity_kinds_and_events_tail() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("m.db")).unwrap();
    let (mission_id, task_id, run_id) = seed(&storage);
    // Dispatch a run + decision entity.
    let dispatch_request = Id::generate();
    storage
        .apply_mission_transition(ApplyMissionTransition {
            request_id: dispatch_request.clone(),
            method: "engine.dispatch".into(),
            fingerprint: "e".repeat(64),
            mission_id: mission_id.clone(),
            mode: ApplyMode::Mutate {
                expected_revision: 1,
            },
            transaction_id: Id::generate(),
            event_type: MissionEventType::RunDispatched,
            upserts: vec![
                Entity::Mission(Box::new(mission(&mission_id, 2))),
                Entity::Run(Box::new(run(
                    &run_id,
                    &mission_id,
                    &task_id,
                    1,
                    RunState::Running,
                ))),
                Entity::Decision(Box::new(term_contracts::mission::types::Decision {
                    id: Id::generate(),
                    mission_id: mission_id.clone(),
                    requesting_run_id: Some(run_id.clone()),
                    kind: term_contracts::mission::types::DecisionKind::Product,
                    state: term_contracts::mission::types::DecisionState::Open,
                    question_ref: artifact(4),
                    options: Vec::new(),
                    affected_task_ids: vec![task_id.clone()],
                    blocking: true,
                    plan_revision: 1,
                    candidate_id: None,
                    answer_ref: None,
                    selected_option_id: None,
                    answer_message_id: None,
                    created_at: now(),
                    answered_at: None,
                })),
            ],
            deletes: Vec::new(),
            changes_ref: None,
            outbox: Vec::new(),
            outbox_updates: Vec::new(),
            adopt_staged_artifacts: Vec::new(),
            created_at: now(),
        })
        .unwrap();

    let snapshot = storage.mission_snapshot(&mission_id).unwrap().unwrap();
    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.event_seq, 2);
    let kinds: Vec<_> = snapshot.entities.iter().map(|e| e.kind()).collect();
    assert!(kinds.contains(&term_contracts::mission::types::EntityKind::Mission));
    assert!(kinds.contains(&term_contracts::mission::types::EntityKind::Task));
    assert!(kinds.contains(&term_contracts::mission::types::EntityKind::Run));
    assert!(kinds.contains(&term_contracts::mission::types::EntityKind::Decision));

    // Event tail: after_seq=1 returns only the second event.
    let (events, watermark) = storage.mission_events(&mission_id, 1, 50).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event.seq.get(), 2);
    assert_eq!(events[0].event.changes.as_ref().unwrap().len(), 3);
    assert_eq!(watermark, 2);

    // Request lookup round trip: the dispatch request resolves with its
    // stored first response.
    let stored = storage.mission_request(&dispatch_request).unwrap().unwrap();
    assert_eq!(stored.method, "engine.dispatch");
    assert_eq!(stored.response.revision.get(), 2);
    assert!(storage.mission_request(&Id::generate()).unwrap().is_none());
}

#[test]
fn mission_list_paginates_by_updated_at_desc() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("m.db")).unwrap();
    let mut ids = Vec::new();
    for index in 0..3u8 {
        let mission_id = Id::generate();
        ids.push(mission_id.clone());
        storage
            .apply_mission_transition(ApplyMissionTransition {
                request_id: Id::generate(),
                method: "mission.create".into(),
                fingerprint: fingerprint(index + 10),
                mission_id: mission_id.clone(),
                mode: ApplyMode::Create,
                transaction_id: Id::generate(),
                event_type: MissionEventType::Created,
                upserts: vec![Entity::Mission(Box::new({
                    let mut m = mission(&mission_id, 1);
                    m.created_at = format!("2026-09-13T00:00:{index:02}Z");
                    m.updated_at = format!("2026-09-13T00:00:{index:02}Z");
                    m
                }))],
                deletes: Vec::new(),
                changes_ref: None,
                outbox: Vec::new(),
                created_at: now(),
                outbox_updates: Vec::new(),
                adopt_staged_artifacts: Vec::new(),
            })
            .unwrap();
    }
    let (page1, next) = storage.mission_list(None, 2, false).unwrap();
    assert_eq!(page1.len(), 2);
    assert!(next.is_some());
    // newest first: updated_at 00:00:02 before 00:00:01
    assert!(page1[0].updated_at > page1[1].updated_at);
    let (page2, next2) = storage.mission_list(next, 2, false).unwrap();
    assert_eq!(page2.len(), 1);
    assert!(next2.is_none());
    assert_ne!(page2[0].id, page1[0].id);
    assert_ne!(page2[0].id, page1[1].id);
    // Archived list is empty until archived_at is set.
    let (archived, _) = storage.mission_list(None, 10, true).unwrap();
    assert!(archived.is_empty());
}

#[test]
fn r1_rows_survive_alongside_mission_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("m.db");
    // Migrate first (R1 0001 + O1 0002), then plant a legacy R1 row the way
    // an existing user database would look.
    let storage = Storage::open(&db).unwrap();
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, created_at) VALUES ('legacy', 'keep me', 'time')",
            [],
        )
        .unwrap();
    }
    let (mission_id, _, _) = seed(&storage);
    assert!(storage.mission_snapshot(&mission_id).unwrap().is_some());
    // R1 row untouched, and both schema families coexist.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let title: String = conn
        .query_row("SELECT title FROM tasks WHERE id = 'legacy'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(title, "keep me");
    let orch: i64 = conn
        .query_row("SELECT COUNT(*) FROM orch_missions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(orch, 1);
}

#[test]
fn retention_prunes_housekeeping_outside_the_tail_and_keeps_meaningful_rows() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("m.db")).unwrap();
    let (mission_id, _, _) = seed(&storage);
    // Revisions 2..=10: every third commit is meaningful, the rest are
    // housekeeping. Old fixed timestamps put every request past the age
    // window so both prunes are exercised.
    let old = "2020-01-01T00:00:00.000Z".to_string();
    for revision in 2u64..=10 {
        let meaningful = revision % 3 == 0;
        let mut doc = mission(&mission_id, revision);
        doc.updated_at = old.clone();
        storage
            .apply_mission_transition(ApplyMissionTransition {
                request_id: Id::generate(),
                method: if meaningful {
                    "mission.control".into()
                } else {
                    "engine.time_checkpoint".into()
                },
                fingerprint: fingerprint(revision as u8),
                mission_id: mission_id.clone(),
                mode: ApplyMode::Mutate {
                    expected_revision: revision - 1,
                },
                transaction_id: Id::generate(),
                event_type: MissionEventType::Changed,
                upserts: vec![Entity::Mission(Box::new(doc))],
                deletes: Vec::new(),
                changes_ref: None,
                outbox: Vec::new(),
                outbox_updates: Vec::new(),
                adopt_staged_artifacts: Vec::new(),
                created_at: old.clone(),
            })
            .unwrap();
    }
    // Housekeeping revisions: 2,4,5,7,8,10. Tail of 3 keeps seq >= 10-3 = 7,
    // so 2,4,5 are pruned (7 stays — seq <= 7 includes it). 7 is housekeeping:
    // 2,4,5,7 pruned; 8,10 kept.
    let pruned = storage
        .prune_mission_retention(3, 14)
        .expect("prune must succeed");
    assert_eq!(pruned.housekeeping_events, 4);
    // Requests: 6 housekeeping-method rows keep the newest 3 per mission
    // (3 pruned); 4 meaningful rows (create + 3 controls) are past the age
    // cutoff (4 pruned).
    assert_eq!(pruned.requests, 7);

    // The watermark and the surviving event sequence: created(1), controls
    // (3,6,9) and the two newest housekeeping rows (8,10) remain.
    let (events, watermark) = storage.mission_events(&mission_id, 0, 50).unwrap();
    let seqs: Vec<u64> = events.iter().map(|e| e.event.seq.get()).collect();
    assert_eq!(seqs, vec![1, 3, 6, 8, 9, 10]);
    assert_eq!(watermark, 10);

    // Mission state materializes from the head revision, untouched.
    let snapshot = storage.mission_snapshot(&mission_id).unwrap().unwrap();
    assert_eq!(snapshot.revision, 10);
    assert_eq!(snapshot.event_seq, 10);

    // Second pass is a no-op.
    let again = storage.prune_mission_retention(3, 14).unwrap();
    assert_eq!(again, term_storage::mission::ops::PrunedRows::default());
}
