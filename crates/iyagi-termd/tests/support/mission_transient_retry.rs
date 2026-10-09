use super::*;
use iyagi_termd_lib::agent_runtime::opencode::runtime::OpenCodeRuntimeAdapter;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use term_contracts::mission::MissionErrorCode;

fn setup() -> (Rig, Arc<AtomicU64>, Arc<AtomicUsize>, AdapterFactory) {
    let mut rig = Rig::new(true, &["status", "--porcelain"]);
    let time = Arc::new(AtomicU64::new(1_000));
    rig.service = service(&rig, time.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let factory: AdapterFactory = Arc::new(move |_| {
        let seen = seen.clone();
        Ok(OpenCodeRuntimeAdapter::with_factory(Arc::new(
            move |_, _| {
                seen.fetch_add(1, Ordering::SeqCst);
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "fixture readiness timeout",
                ))
            },
        )))
    });
    (rig, time, calls, factory)
}
fn service(rig: &Rig, time: Arc<AtomicU64>) -> Arc<MissionService> {
    let instant = Instant::now();
    Arc::new(
        MissionService::new(
            rig.storage.clone(),
            ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
        )
        .with_wall_clock_millis(move || time.load(Ordering::SeqCst))
        .with_monotonic_clock(move || instant),
    )
}
fn waiting(rig: &Rig, actor: &mut MissionActor, attempt: u32) -> (Run, u64) {
    rig.tick_until(actor, |s| {
        s.tasks.iter().any(|t| {
            t.attempt_count == attempt && t.blocked_code.as_deref() == Some("transient_retry")
        })
    });
    let snapshot = rig.snapshot();
    let task = &snapshot.tasks[0];
    assert_eq!(task.state, TaskState::Blocked);
    let run = snapshot
        .runs
        .into_iter()
        .find(|r| r.attempt == attempt)
        .unwrap();
    assert_eq!(run.state, RunState::Failed);
    assert!(run.retry_evidence.is_some());
    (run, task.dispatch_after_unix_ms.as_ref().unwrap().get())
}
fn control(rig: &Rig, action: &str) {
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":action}),
    );
}

#[test]
fn automatic_retries_keep_deadlines_across_restart_and_stop_after_two_new_attempts() {
    let (mut rig, time, calls, factory) = setup();
    let mut actor = rig.actor(factory.clone());
    let (first, deadline) = waiting(&rig, &mut actor, 1);
    let RetryEvidence::RequestNotSubmitted {
        observed_at_unix_ms,
        ..
    } = first.retry_evidence.as_ref().unwrap()
    else {
        panic!("missing unsubmitted evidence")
    };
    assert!((2_000..=2_400).contains(&(deadline - observed_at_unix_ms.get())));
    let revision = rig.snapshot().mission.revision;
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().mission.revision, revision);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    actor.shutdown();
    rig.service = service(&rig, time.clone());
    rig.service.recover_on_startup().unwrap();
    let mut actor = rig.actor(factory);
    time.store(deadline - 1, Ordering::SeqCst);
    actor.tick().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        rig.snapshot().tasks[0]
            .dispatch_after_unix_ms
            .as_ref()
            .unwrap()
            .get(),
        deadline
    );
    time.store(deadline, Ordering::SeqCst);
    let (second, second_deadline) = waiting(&rig, &mut actor, 2);
    let RetryEvidence::RequestNotSubmitted {
        observed_at_unix_ms,
        ..
    } = second.retry_evidence.as_ref().unwrap()
    else {
        panic!("missing unsubmitted evidence")
    };
    assert!((10_000..=12_000).contains(&(second_deadline - observed_at_unix_ms.get())));
    assert_ne!(first.id, second.id);
    assert_ne!(first.workspace_id, second.workspace_id);
    assert_eq!(
        rig.snapshot().runs.iter().find(|r| r.id == first.id),
        Some(&first)
    );
    time.store(second_deadline, Ordering::SeqCst);
    rig.tick_until(&mut actor, |s| {
        s.runs.len() == 3
            && s.decisions.iter().any(|d| {
                d.state == DecisionState::Open
                    && d.options.iter().any(|o| o.id == "stop_failed_mission")
            })
    });
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(rig.snapshot().tasks[0].attempt_count, 3);
    assert_eq!(rig.snapshot().tasks[0].state, TaskState::Failed);
    actor.shutdown();
}

#[test]
fn pause_and_cancel_never_launch_a_waiting_retry() {
    let (rig, time, calls, factory) = setup();
    let mut actor = rig.actor(factory);
    let (_, deadline) = waiting(&rig, &mut actor, 1);
    control(&rig, "pause");
    time.store(deadline + 1, Ordering::SeqCst);
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(rig.snapshot().mission.state, MissionState::Paused);
    control(&rig, "cancel");
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(rig.snapshot().runs.len(), 1);
    actor.shutdown();
}

#[test]
fn failed_retry_commit_preserves_the_deadline_and_never_starts_early() {
    let (rig, time, calls, factory) = setup();
    let mut actor = rig.actor(factory);
    let (old, deadline) = waiting(&rig, &mut actor, 1);
    let db = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_retry BEFORE INSERT ON orch_events BEGIN SELECT RAISE(FAIL,'fixture retry save outage'); END;").unwrap();
    time.store(deadline, Ordering::SeqCst);
    assert!(actor.tick().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        rig.snapshot().tasks[0]
            .dispatch_after_unix_ms
            .as_ref()
            .unwrap()
            .get(),
        deadline
    );
    assert_eq!(rig.snapshot().runs[0], old);
    db.execute_batch("DROP TRIGGER deny_retry").unwrap();
    waiting(&rig, &mut actor, 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    actor.shutdown();
}

#[test]
fn retry_policy_rechecks_attempt_and_automatic_start_budgets() {
    for task_limit in [true, false] {
        let (rig, time, calls, factory) = setup();
        let mut actor = rig.actor(factory);
        let (_, deadline) = waiting(&rig, &mut actor, 1);
        let snapshot = rig.snapshot();
        let mut mission = snapshot.mission;
        if task_limit {
            mission.policy.max_attempts_per_task = 1;
        } else {
            mission.policy.max_automatic_starts = 1;
        }
        workflow::commit_upserts(
            &rig.service,
            mission,
            "fixture.retry.policy",
            "limit",
            MissionEventType::Changed,
            vec![],
        )
        .unwrap();
        time.store(deadline, Ordering::SeqCst);
        rig.tick_until(&mut actor, |s| {
            s.tasks[0].state == TaskState::Failed
                && s.decisions.iter().any(|d| d.state == DecisionState::Open)
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(rig.snapshot().runs.len(), 1);
        actor.shutdown();
    }
}

#[test]
fn retry_evidence_rejects_unknown_acknowledged_invalid_and_nontransient_failures() {
    let (rig, _, _, factory) = setup();
    let mut actor = rig.actor(factory);
    let (run, original) = waiting(&rig, &mut actor, 1);
    actor.shutdown();
    let deadline = term_core::mission::retry::retry_deadline;
    for code in [
        MissionErrorCode::AuthRequired,
        MissionErrorCode::ModelUnavailable,
        MissionErrorCode::PolicyDenied,
        MissionErrorCode::ResultInvalid,
        MissionErrorCode::OutcomeUnknown,
    ] {
        let mut changed = run.clone();
        changed.failure_code = Some(code);
        assert_eq!(deadline(&changed), None);
    }
    let mut changed = run.clone();
    changed.retry_evidence = None;
    assert_eq!(deadline(&changed), None);
    let mut changed = run.clone();
    changed.state = RunState::Unknown;
    assert_eq!(deadline(&changed), None);
    let mut changed = run.clone();
    changed.dispatch_state = RunDispatchState::Acknowledged;
    assert_eq!(deadline(&changed), None);
    let mut changed = run.clone();
    changed.attempt = 3;
    assert_eq!(deadline(&changed), None);
    let mut changed = run.clone();
    if let Some(RetryEvidence::RequestNotSubmitted {
        retry_after_unix_ms,
        ..
    }) = &mut changed.retry_evidence
    {
        *retry_after_unix_ms =
            Some(term_contracts::ids::U64String::new(original + 50_000).unwrap());
    }
    assert_eq!(deadline(&changed), Some(original + 50_000));
    let legacy = serde_json::to_value(&run).unwrap();
    let mut legacy = legacy.as_object().unwrap().clone();
    legacy.remove("retry_evidence");
    assert_eq!(
        serde_json::from_value::<Run>(Value::Object(legacy))
            .unwrap()
            .retry_evidence,
        None
    );
}

#[test]
fn a_transient_code_without_submission_evidence_keeps_explicit_recovery() {
    let (rig, _, _, _) = setup();
    let mut actor = rig.actor(Arc::new(|_| {
        Ok(scripted(FakeScript {
            steps: vec![FakeStep::Fail {
                code: "PROVIDER_UNAVAILABLE".into(),
                message: "ambiguous provider failure".into(),
            }],
            ..Default::default()
        }))
    }));
    rig.tick_until(&mut actor, |s| {
        s.decisions.iter().any(|d| d.state == DecisionState::Open)
    });
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().runs.len(), 1);
    assert_eq!(rig.snapshot().runs[0].retry_evidence, None);
    assert_eq!(rig.snapshot().tasks[0].state, TaskState::Failed);
    actor.shutdown();
}

#[test]
fn unfinished_or_missing_exec_blocks_automatic_and_explicit_retry() {
    let (rig, time, calls, factory) = setup();
    let mut actor = rig.actor(factory);
    let (old, deadline) = waiting(&rig, &mut actor, 1);
    let mut exec = ExecRecord {
        id: Id::generate(),
        mission_id: rig.id.clone(),
        run_id: old.id.clone(),
        state: ExecState::Prepared,
        identity: None,
        group_kind: None,
        group_reference: None,
        group_identity: None,
        resource_policy: old
            .binding_snapshot
            .as_ref()
            .unwrap()
            .resource_policy
            .clone(),
        launch_manifest_ref: workflow::store_artifact(
            &ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
            &rig.id,
            "application/json",
            b"{}",
        )
        .unwrap(),
        owner_daemon_id: Id::generate(),
        started_at: None,
        ended_at: None,
        exit_code: None,
    };
    let mut run = old.clone();
    run.exec_id = Some(exec.id.clone());
    workflow::commit_upserts(
        &rig.service,
        rig.snapshot().mission,
        "fixture.retry.exec",
        "missing",
        MissionEventType::Changed,
        vec![Entity::Run(Box::new(run.clone()))],
    )
    .unwrap();
    time.store(deadline, Ordering::SeqCst);
    for _ in 0..3 {
        actor.tick().unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    workflow::commit_upserts(
        &rig.service,
        rig.snapshot().mission,
        "fixture.retry.exec",
        "unfinished",
        MissionEventType::Changed,
        vec![Entity::Exec(Box::new(exec.clone()))],
    )
    .unwrap();
    for _ in 0..3 {
        actor.tick().unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Exercise the manual-retry boundary with an otherwise retryable Task,
    // so rejection must come from the unfinished Exec, not the blocked state.
    let mut failed_task = rig.snapshot().tasks[0].clone();
    failed_task.state = TaskState::Failed;
    failed_task.blocked_code = Some("ProviderUnavailable".into());
    failed_task.dispatch_after_unix_ms = None;
    workflow::commit_upserts(
        &rig.service,
        rig.snapshot().mission,
        "fixture.retry.exec",
        "failed-task",
        MissionEventType::Changed,
        vec![Entity::Task(Box::new(failed_task))],
    )
    .unwrap();
    assert!(rig.service.handle(&rig.conn, "mission.task.control", &json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"task_id":run.task_id,"action":"retry","binding_id":null})).is_err());
    exec.state = ExecState::Exited;
    exec.ended_at = Some(term_storage::time::now_iso8601());
    workflow::commit_upserts(
        &rig.service,
        rig.snapshot().mission,
        "fixture.retry.exec",
        "ended",
        MissionEventType::Changed,
        vec![Entity::Exec(Box::new(exec))],
    )
    .unwrap();
    waiting(&rig, &mut actor, 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    actor.shutdown();
}

#[test]
fn disabled_binding_stops_the_automatic_retry_and_opens_recovery() {
    let (rig, time, calls, factory) = setup();
    let mut actor = rig.actor(factory);
    let (_, deadline) = waiting(&rig, &mut actor, 1);
    let mut binding: Binding =
        serde_json::from_value(rig.storage.mission_bindings().unwrap().remove(0)).unwrap();
    binding.enabled = false;
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":binding.revision,"binding":binding}),
    );
    time.store(deadline, Ordering::SeqCst);
    rig.tick_until(&mut actor, |s| {
        s.tasks[0].state == TaskState::Failed
            && s.decisions.iter().any(|d| d.state == DecisionState::Open)
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    actor.shutdown();
}

#[test]
fn model_reassignment_preserves_the_delay_and_old_binding_snapshot() {
    let (rig, time, calls, factory) = setup();
    let mut actor = rig.actor(factory);
    let (old, deadline) = waiting(&rig, &mut actor, 1);
    let mut binding = old.binding_snapshot.clone().unwrap();
    binding.id = Id::generate();
    binding.revision = term_contracts::ids::U64String::new(0).unwrap();
    binding.model_id = "alternative-fixture-model".into();
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":"0","binding":binding}),
    );
    let mut mission = rig.snapshot().mission;
    mission.policy.allowed_binding_ids.push(binding.id.clone());
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.retry.binding",
        "allow",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    let params = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"task_id":old.task_id,"action":"reassign","binding_id":binding.id});
    let response = rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        params.clone(),
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.task.control", params),
        response
    );
    assert_eq!(
        rig.snapshot().tasks[0]
            .dispatch_after_unix_ms
            .as_ref()
            .unwrap()
            .get(),
        deadline
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    time.store(deadline, Ordering::SeqCst);
    let (next, _) = waiting(&rig, &mut actor, 2);
    assert_eq!(next.binding_snapshot.as_ref().unwrap().id, binding.id);
    assert_eq!(
        rig.snapshot().runs.iter().find(|r| r.id == old.id),
        Some(&old)
    );
    actor.shutdown();
}
