use super::*;
use iyagi_termd_lib::agent_runtime::AdapterEvent;
use std::sync::atomic::{AtomicU64, Ordering};
use term_contracts::ids::ConnectionId;

fn clock_service(rig: &Rig, time: Arc<AtomicU64>) -> Arc<MissionService> {
    let instant = std::time::Instant::now();
    Arc::new(
        MissionService::new(
            rig.storage.clone(),
            ArtifactStore::new(rig.storage.clone(), rig._dir.path().join("missions")),
        )
        .with_wall_clock_millis(move || time.load(Ordering::SeqCst))
        .with_monotonic_clock(move || instant),
    )
}

fn limited_run(rig: &Rig, until: u64) -> (Id, Run) {
    let id = rig.seed(MissionState::Running, 1, 64);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    let run = workflow::load_entities(&rig.storage, &id)
        .unwrap()
        .runs
        .remove(0);
    assert!(!rig
        .service
        .apply_adapter_event(&id, &event(&run, 1000, until), None)
        .unwrap()); // nonterminal event
    (
        id.clone(),
        workflow::load_entities(&rig.storage, &id)
            .unwrap()
            .runs
            .remove(0),
    )
}

fn event(run: &Run, observed: u64, until: u64) -> AdapterEvent {
    AdapterEvent::RateLimited {
        run_id: run.id.clone(),
        fencing_token: run.fencing_token.get(),
        observation: RateLimitObservation {
            observed_at_unix_ms: U64String::new(observed).unwrap(),
            resets_at_unix_ms: U64String::new(until).unwrap(),
        },
    }
}

#[test]
fn persisted_reset_holds_new_missions_across_restart_and_archive_without_spending_attempts() {
    let mut rig = Rig::new();
    let time = Arc::new(AtomicU64::new(1000));
    rig.service = clock_service(&rig, time.clone());
    let (source, run) = limited_run(&rig, 10_000);
    let snapshot = workflow::load_entities(&rig.storage, &source).unwrap();
    let mut mission = snapshot.mission;
    mission.state = MissionState::Failed;
    mission.archived_at = Some("2026-09-15T00:00:00Z".into());
    let mut old = run.clone();
    old.state = RunState::Failed;
    old.ended_at = Some("2026-09-15T00:00:00Z".into());
    let mut task = snapshot.tasks[0].clone();
    task.state = TaskState::Failed;
    task.active_run_id = None;
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.archive",
        "archive",
        MissionEventType::Changed,
        vec![
            Entity::Run(Box::new(old.clone())),
            Entity::Task(Box::new(task)),
        ],
    )
    .unwrap();
    let waiting = rig.seed(MissionState::Running, 2, 64);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    let held = workflow::load_entities(&rig.storage, &waiting).unwrap();
    assert!(held.runs.is_empty());
    assert!(held.decisions.is_empty());
    assert_eq!(held.mission.automatic_start_count, 0);
    assert!(held.tasks.iter().all(|t| t.state == TaskState::Blocked
        && t.attempt_count == 0
        && t.dispatch_after_unix_ms.as_ref().unwrap().get() == 10_000));
    rig.service = clock_service(&rig, time.clone());
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    assert_eq!(
        workflow::load_entities(&rig.storage, &waiting)
            .unwrap()
            .mission
            .revision,
        held.mission.revision
    );
    time.store(9_999, Ordering::SeqCst);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    time.store(10_000, Ordering::SeqCst);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 2);
    let after = workflow::load_entities(&rig.storage, &waiting).unwrap();
    assert_eq!(after.mission.automatic_start_count, 2);
    assert!(after
        .tasks
        .iter()
        .all(|t| t.attempt_count == 1 && t.dispatch_after_unix_ms.is_none()));
    assert_eq!(
        workflow::load_entities(&rig.storage, &source).unwrap().runs[0],
        old
    );
    assert!(rig
        .storage
        .mission_rate_limit_runs(10_000)
        .unwrap()
        .is_empty());
}

#[test]
fn independent_binding_runs_and_model_reassignment_rechecks_the_scope() {
    let mut rig = Rig::new();
    let time = Arc::new(AtomicU64::new(1000));
    rig.service = clock_service(&rig, time);
    limited_run(&rig, 10_000);
    let waiting = rig.seed(MissionState::Running, 1, 64);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    let mut binding = rig.binding.clone();
    binding.id = Id::generate();
    binding.revision = U64String::new(0).unwrap();
    rig.storage
        .save_mission_binding(
            Id::generate(),
            "binding.save",
            &"a".repeat(64),
            0,
            serde_json::to_value(&binding).unwrap(),
            "2026-09-15T00:00:00Z".into(),
        )
        .unwrap();
    let snapshot = workflow::load_entities(&rig.storage, &waiting).unwrap();
    let mut mission = snapshot.mission;
    mission.policy.allowed_binding_ids.push(binding.id.clone());
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.allow",
        "allow",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    let snapshot = workflow::load_entities(&rig.storage, &waiting).unwrap();
    let params = json!({"request_id":Id::generate(),"mission_id":waiting,"expected_revision":snapshot.mission.revision,"task_id":snapshot.tasks[0].id,"action":"reassign","binding_id":binding.id});
    let response = rig
        .service
        .handle(&ConnectionId::generate(), "mission.task.control", &params)
        .unwrap()
        .result;
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    let after = workflow::load_entities(&rig.storage, &waiting).unwrap();
    assert_eq!(
        after.runs[0].binding_snapshot.as_ref().unwrap().id,
        binding.id
    );
    assert_eq!(
        rig.service
            .handle(&ConnectionId::generate(), "mission.task.control", &params)
            .unwrap()
            .result,
        response
    );
}

#[test]
fn observation_is_fenced_monotonic_and_atomic_with_its_run_event() {
    let mut rig = Rig::new();
    rig.service = clock_service(&rig, Arc::new(AtomicU64::new(1000)));
    let id = rig.seed(MissionState::Running, 1, 64);
    rig.service.dispatch_tick().unwrap();
    let before = workflow::load_entities(&rig.storage, &id).unwrap();
    let run = &before.runs[0];
    let mut stale = event(run, 1000, 10_000);
    if let AdapterEvent::RateLimited { fencing_token, .. } = &mut stale {
        *fencing_token += 1;
    }
    assert!(!rig.service.apply_adapter_event(&id, &stale, None).unwrap());
    assert!(!rig
        .service
        .apply_adapter_event(&id, &event(run, 1000, 999), None)
        .unwrap());
    let db = rusqlite::Connection::open(rig._dir.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_rate_event BEFORE INSERT ON orch_events BEGIN SELECT RAISE(FAIL,'fixture persistence failure'); END;").unwrap();
    assert!(rig
        .service
        .apply_adapter_event(&id, &event(run, 1000, 10_000), None)
        .is_err());
    assert!(rig
        .storage
        .mission_rate_limit_runs(1000)
        .unwrap()
        .is_empty());
    assert_eq!(
        workflow::load_entities(&rig.storage, &id).unwrap().runs,
        before.runs
    );
    db.execute_batch("DROP TRIGGER fail_rate_event").unwrap();
    assert!(!rig
        .service
        .apply_adapter_event(&id, &event(run, 1000, 10_000), None)
        .unwrap());
    assert!(!rig
        .service
        .apply_adapter_event(&id, &event(run, 1000, 9000), None)
        .unwrap());
    assert_eq!(
        rig.storage.mission_rate_limit_runs(1000).unwrap()[0]
            .rate_limit
            .as_ref()
            .unwrap()
            .resets_at_unix_ms
            .get(),
        10_000
    );
}

#[test]
fn label_and_estimate_edits_do_not_erase_a_known_account_reset() {
    let mut rig = Rig::new();
    rig.service = clock_service(&rig, Arc::new(AtomicU64::new(1000)));
    let (_, run) = limited_run(&rig, 10_000);
    let mut binding = run.binding_snapshot.as_ref().unwrap().clone();
    binding.label = "renamed".into();
    binding.estimated_run_cost_usd_micros = Some(U64String::new(20).unwrap());
    assert_eq!(
        term_core::mission::rate_limits::reset_deadline(&binding, std::slice::from_ref(&run), 1000),
        Some(10_000)
    );
    binding.credential_ref = Some("different-account".into());
    assert_eq!(
        term_core::mission::rate_limits::reset_deadline(&binding, &[run], 1000),
        None
    );
}

#[test]
fn expiry_while_paused_does_not_launch_and_stopping_rejects_model_reassignment() {
    let mut rig = Rig::new();
    let time = Arc::new(AtomicU64::new(1000));
    rig.service = clock_service(&rig, time.clone());
    limited_run(&rig, 10_000);
    let id = rig.seed(MissionState::Paused, 1, 64);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    assert_eq!(
        workflow::load_entities(&rig.storage, &id).unwrap().tasks[0].state,
        TaskState::Blocked
    );
    time.store(10_000, Ordering::SeqCst);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    let before = workflow::load_entities(&rig.storage, &id).unwrap();
    assert_eq!(before.tasks[0].state, TaskState::Ready);
    assert_eq!(before.tasks[0].attempt_count, 0);
    let mut mission = before.mission;
    mission.state = MissionState::Stopping;
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.stop",
        "stop",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    let before = workflow::load_entities(&rig.storage, &id).unwrap();
    let error=rig.service.handle(&ConnectionId::generate(),"mission.task.control",&json!({"request_id":Id::generate(),"mission_id":id,"expected_revision":before.mission.revision,"task_id":before.tasks[0].id,"action":"reassign","binding_id":rig.binding.id})).err().unwrap();
    assert_eq!(
        error.code,
        term_contracts::mission::MissionErrorCode::InvalidState
    );
    assert_eq!(
        workflow::load_entities(&rig.storage, &id).unwrap().tasks,
        before.tasks
    );
}
