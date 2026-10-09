use super::*;
use term_contracts::{ids::U64String, mission::MissionErrorCode};

fn rig() -> (Rig, Arc<Mutex<Instant>>) {
    let clock = Arc::new(Mutex::new(Instant::now()));
    let source = clock.clone();
    (
        Rig::with_clock(
            true,
            &["status", "--porcelain"],
            true,
            Some(Arc::new(move || *source.lock().unwrap())),
        ),
        clock,
    )
}
fn advance(clock: &Mutex<Instant>, duration: Duration) {
    *clock.lock().unwrap() += duration;
}
fn control(rig: &Rig, action: &str) -> Value {
    json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":action})
}
fn hold() -> AdapterFactory {
    Arc::new(|_| {
        Ok(scripted(FakeScript {
            steps: vec![
                FakeStep::Started {
                    session_id: None,
                    turn_id: None,
                },
                FakeStep::Approval {
                    request_id: "timing-hold".into(),
                    question: "Hold for timing test".into(),
                },
            ],
            ..Default::default()
        }))
    })
}

#[test]
fn checkpoint_pause_resume_and_request_replay_account_only_active_intervals() {
    let (rig, clock) = rig();
    rig.service.dispatch_tick().unwrap();
    let before = rig.snapshot();
    advance(&clock, Duration::from_micros(999_500));
    rig.service.dispatch_tick().unwrap();
    assert_eq!(
        rig.snapshot().mission,
        before.mission,
        "quiet updates wait for the checkpoint interval"
    );
    advance(&clock, Duration::from_micros(500));
    rig.service.dispatch_tick().unwrap();
    assert_eq!(rig.snapshot().mission.active_time_ms.get(), 1000);
    assert_eq!(
        rig.snapshot().runs[0].active_time_ms.get(),
        0,
        "reservation is not a started Run"
    );
    // A checkpoint is housekeeping; only a revision older than the last
    // meaningful change (the dispatch) is stale.
    assert_eq!(
        rig.snapshot().mission.semantic_revision,
        Some(before.mission.revision.clone())
    );
    let stale = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":U64String::new(before.mission.revision.get() - 1).unwrap(),"action":"pause"});
    assert_eq!(
        rig.service
            .handle(&rig.conn, "mission.control", &stale)
            .err()
            .unwrap()
            .code,
        MissionErrorCode::RevisionConflict
    );
    advance(&clock, Duration::from_millis(500));
    let pause = control(&rig, "pause");
    let response = rpc(&rig.service, &rig.conn, "mission.control", pause.clone());
    assert_eq!(rig.snapshot().mission.state, MissionState::Paused);
    assert_eq!(rig.snapshot().mission.active_time_ms.get(), 1500);
    advance(&clock, Duration::from_secs(3600));
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.control", pause),
        response
    );
    rig.service.dispatch_tick().unwrap();
    assert_eq!(rig.snapshot().mission.active_time_ms.get(), 1500);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        control(&rig, "resume"),
    );
    advance(&clock, Duration::from_millis(250));
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        control(&rig, "pause"),
    );
    assert_eq!(rig.snapshot().mission.active_time_ms.get(), 1750);
}

#[test]
fn parallel_runs_and_pausing_do_not_multiply_mission_time() {
    let (rig, clock) = rig();
    let mut snapshot = rig.snapshot();
    snapshot.mission.phase = Phase::Implementing;
    let mut first = snapshot.tasks[0].clone();
    first.kind = TaskKind::Implement;
    first.role = Some(Role::Builder);
    let mut second = first.clone();
    second.id = Id::generate();
    second.ordinal += 1;
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "fixture.parallel",
        "timing",
        MissionEventType::Changed,
        vec![
            Entity::Task(Box::new(first)),
            Entity::Task(Box::new(second)),
        ],
    )
    .unwrap();
    let mut actor = rig.actor(hold());
    rig.tick_until(&mut actor, |s| {
        s.runs.len() == 2 && s.runs.iter().all(|r| r.state == RunState::AwaitingInput)
    });
    advance(&clock, Duration::from_millis(2500));
    actor.tick().unwrap();
    let timed = rig.snapshot();
    assert_eq!(timed.mission.active_time_ms.get(), 2500);
    assert!(timed.runs.iter().all(|r| r.active_time_ms.get() == 2500));
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        control(&rig, "pause"),
    );
    assert_eq!(rig.snapshot().mission.state, MissionState::Pausing);
    advance(&clock, Duration::from_millis(500));
    actor.shutdown();
    actor.tick().unwrap();
    let paused = rig.snapshot();
    assert_eq!(paused.mission.state, MissionState::Paused);
    assert_eq!(paused.mission.active_time_ms.get(), 3000);
    assert!(paused
        .runs
        .iter()
        .all(|r| r.active_time_ms.get() == 3000 && r.ended_at.is_some()));
    advance(&clock, Duration::from_secs(30));
    actor.tick().unwrap();
    assert_eq!(rig.snapshot().mission, paused.mission);
    assert_eq!(rig.snapshot().runs, paused.runs);
}

#[test]
fn failed_time_checkpoint_retries_elapsed_once_and_preserves_concurrent_projection() {
    let (rig, clock) = rig();
    rig.service.dispatch_tick().unwrap();
    let before = rig.snapshot();
    let connection = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_clock BEFORE UPDATE ON orch_missions BEGIN SELECT RAISE(FAIL, 'fixture timing outage'); END").unwrap();
    advance(&clock, Duration::from_millis(1250));
    assert_eq!(
        rig.service.dispatch_tick().err().unwrap().code,
        MissionErrorCode::StorageUnavailable
    );
    assert_eq!(rig.snapshot().mission, before.mission);
    advance(&clock, Duration::from_millis(1250));
    connection.execute_batch("DROP TRIGGER fail_clock").unwrap();
    let mut current = rig.snapshot().mission;
    current.title = "Concurrent title retained".into();
    workflow::commit_upserts(
        &rig.service,
        current,
        "fixture.title",
        "title",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    let after = rig.snapshot();
    assert_eq!(after.mission.active_time_ms.get(), 2500);
    assert_eq!(after.mission.title, "Concurrent title retained");
    rig.service.dispatch_tick().unwrap();
    assert_eq!(rig.snapshot().mission, after.mission);
}

#[test]
fn restart_keeps_persisted_time_without_charging_downtime_or_unknown_runs() {
    let (rig, clock) = rig();
    let mut actor = rig.actor(hold());
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::AwaitingInput)
    });
    advance(&clock, Duration::from_millis(2250));
    actor.tick().unwrap();
    let before = rig.snapshot();
    let restart_clock = Arc::new(Mutex::new(
        *clock.lock().unwrap() + Duration::from_secs(86_400),
    ));
    let source = restart_clock.clone();
    let restarted = Arc::new(
        MissionService::new(
            rig.storage.clone(),
            ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
        )
        .with_monotonic_clock(move || *source.lock().unwrap()),
    );
    restarted.recover_on_startup().unwrap();
    assert_eq!(
        rig.snapshot().mission.active_time_ms,
        before.mission.active_time_ms
    );
    assert_eq!(
        rig.snapshot().runs[0].active_time_ms,
        before.runs[0].active_time_ms
    );
    advance(&restart_clock, Duration::from_millis(1250));
    restarted.dispatch_tick().unwrap();
    let after = rig.snapshot();
    assert_eq!(after.mission.active_time_ms.get(), 3500);
    assert_eq!(after.runs[0].state, RunState::Unknown);
    assert_eq!(after.runs[0].active_time_ms.get(), 2250);
    actor.shutdown(); // obsolete fence cannot overwrite the recovered clock or Run
    assert_eq!(rig.snapshot().runs, after.runs);
}

#[test]
fn subsecond_limit_blocks_dispatch_once_and_policy_expansion_releases_it() {
    let (rig, clock) = rig();
    let snapshot = rig.snapshot();
    let mut policy = snapshot.mission.policy.clone();
    policy.active_time_limit_ms = U64String::new(100).unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"policy":policy,"role_bindings":snapshot.mission.role_bindings}),
    );
    advance(&clock, Duration::from_millis(101));
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    let blocked = rig.snapshot();
    assert_eq!(blocked.mission.active_time_ms.get(), 101);
    assert_eq!(
        blocked.tasks[0].blocked_code.as_deref(),
        Some("active_time_limit")
    );
    assert_eq!(blocked.mission.open_decision_count, 1);
    assert_eq!(blocked.decisions[0].kind, DecisionKind::Budget);
    assert_eq!(
        blocked.decisions[0]
            .options
            .iter()
            .map(|o| o.id.as_str())
            .collect::<Vec<_>>(),
        ["stop_mission", "adjust_limits"]
    );
    // `adjust_limits` only routes the UI to policy.update; answering is refused.
    let refused = rig
        .service
        .handle(
            &rig.conn,
            "mission.decision.answer",
            &json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":blocked.mission.revision,"decision_id":blocked.decisions[0].id,"option_id":"adjust_limits","answer_ref":null}),
        )
        .err()
        .unwrap();
    assert_eq!(refused.code, MissionErrorCode::InvalidArgument);
    assert_eq!(
        refused.details.reason_code.as_deref(),
        Some("policy_update_required")
    );
    assert!(blocked.runs.is_empty());
    rig.service.dispatch_tick().unwrap();
    assert_eq!(rig.snapshot().mission.revision, blocked.mission.revision);
    policy.active_time_limit_ms = U64String::new(1000).unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":blocked.mission.revision,"policy":policy,"role_bindings":blocked.mission.role_bindings}),
    );
    assert_eq!(rig.snapshot().decisions[0].state, DecisionState::Obsolete);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
}

#[test]
fn housekeeping_commits_do_not_reject_user_controls_but_meaningful_commits_do() {
    let (rig, clock) = rig();
    rig.service.dispatch_tick().unwrap();
    let before = rig.snapshot();
    assert_eq!(
        before.mission.semantic_revision,
        Some(before.mission.revision.clone())
    );
    advance(&clock, Duration::from_millis(1250));
    rig.service.dispatch_tick().unwrap();
    let checkpointed = rig.snapshot();
    assert_eq!(
        checkpointed.mission.revision.get(),
        before.mission.revision.get() + 1,
        "the time checkpoint still advances revision and event seq"
    );
    assert_eq!(
        checkpointed.mission.semantic_revision,
        before.mission.semantic_revision
    );
    assert_eq!(checkpointed.mission.active_time_ms.get(), 1250);

    // The user saw `before`; only housekeeping happened since.
    advance(&clock, Duration::from_millis(250));
    let pause = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":before.mission.revision,"action":"pause"});
    let applied = rpc(&rig.service, &rig.conn, "mission.control", pause.clone());
    let paused = rig.snapshot();
    assert_eq!(paused.mission.state, MissionState::Paused);
    assert_eq!(
        paused.mission.revision.get(),
        checkpointed.mission.revision.get() + 1
    );
    assert_eq!(applied["revision"], json!(paused.mission.revision));
    assert_eq!(
        paused.mission.semantic_revision,
        Some(paused.mission.revision.clone())
    );
    assert_eq!(
        paused.mission.active_time_ms.get(),
        1500,
        "rebasing never loses checkpointed time"
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.control", pause),
        applied,
        "request replay still returns the first response"
    );

    // The pause is meaningful: a request based on the checkpoint is stale.
    let stale = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":checkpointed.mission.revision,"action":"resume"});
    let error = rig
        .service
        .handle(&rig.conn, "mission.control", &stale)
        .err()
        .unwrap();
    assert_eq!(error.code, MissionErrorCode::RevisionConflict);
    assert_eq!(
        error.details.current_revision,
        Some(paused.mission.revision.clone())
    );
    let future = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":U64String::new(paused.mission.revision.get() + 1).unwrap(),"action":"resume"});
    assert_eq!(
        rig.service
            .handle(&rig.conn, "mission.control", &future)
            .err()
            .unwrap()
            .code,
        MissionErrorCode::RevisionConflict
    );
    assert_eq!(rig.snapshot().mission, paused.mission);
}

#[test]
fn awaiting_acceptance_does_not_consume_active_time() {
    let (rig, clock) = rig();
    advance(&clock, Duration::from_millis(400));
    let mut waiting = rig.snapshot().mission;
    waiting.phase = Phase::AwaitingAcceptance;
    workflow::commit_upserts(
        &rig.service,
        waiting,
        "fixture.awaiting_acceptance",
        "phase",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    assert_eq!(rig.snapshot().mission.active_time_ms.get(), 400);
    advance(&clock, Duration::from_secs(3600));
    rig.service.dispatch_tick().unwrap();
    let waited = rig.snapshot();
    assert_eq!(waited.mission.state, MissionState::Running);
    assert_eq!(
        waited.mission.active_time_ms.get(),
        400,
        "waiting for acceptance is not execution time"
    );
    assert!(
        waited.runs.is_empty(),
        "no dispatch while awaiting acceptance"
    );

    // Leaving the phase resumes accounting from that commit.
    let mut resumed = waited.mission;
    resumed.phase = Phase::Implementing;
    workflow::commit_upserts(
        &rig.service,
        resumed,
        "fixture.reopen",
        "phase",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    assert_eq!(rig.snapshot().mission.active_time_ms.get(), 400);
    advance(&clock, Duration::from_millis(1250));
    rig.service.dispatch_tick().unwrap();
    assert_eq!(rig.snapshot().mission.active_time_ms.get(), 1650);
}

#[test]
fn quiet_missions_decay_checkpoint_cadence_and_meaningful_commits_reset_it() {
    let (rig, clock) = rig();
    rig.service.dispatch_tick().unwrap(); // dispatch = last meaningful commit (t=0)
    advance(&clock, Duration::from_millis(1000));
    rig.service.dispatch_tick().unwrap();
    assert_eq!(rig.snapshot().mission.active_time_ms.get(), 1000);

    // Past MEANINGFUL_WINDOW with no run clocks: one commit per idle interval.
    advance(&clock, Duration::from_secs(60));
    rig.service.dispatch_tick().unwrap();
    let decayed = rig.snapshot();
    assert_eq!(decayed.mission.active_time_ms.get(), 61_000);
    advance(&clock, Duration::from_secs(30));
    rig.service.dispatch_tick().unwrap();
    assert_eq!(
        rig.snapshot().mission,
        decayed.mission,
        "half an idle interval must not rewrite the mission"
    );
    advance(&clock, Duration::from_secs(31));
    rig.service.dispatch_tick().unwrap();
    assert_eq!(rig.snapshot().mission.active_time_ms.get(), 122_000);

    // A meaningful commit restarts the fast cadence even while quiet.
    let mut current = rig.snapshot().mission;
    current.title = "Meaningful again".into();
    workflow::commit_upserts(
        &rig.service,
        current,
        "fixture.title",
        "title",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    advance(&clock, Duration::from_millis(1000));
    rig.service.dispatch_tick().unwrap();
    assert_eq!(rig.snapshot().mission.active_time_ms.get(), 123_000);
}
