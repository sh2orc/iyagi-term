use super::*;
use term_contracts::mission::MissionErrorCode;

fn failed_factory(code: MissionErrorCode) -> AdapterFactory {
    Arc::new(move |_| {
        Ok(scripted(FakeScript {
            steps: vec![FakeStep::Fail {
                code: serde_json::to_value(code).unwrap().as_str().unwrap().into(),
                message: "fixture confirmed failure".into(),
            }],
            ..Default::default()
        }))
    })
}
fn failed_rig() -> (Rig, MissionActor) {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let mut actor = rig.actor(failed_factory(MissionErrorCode::AuthRequired));
    rig.tick_until(&mut actor, |s| s.decisions.iter().any(is_failure));
    (rig, actor)
}
fn is_failure(d: &Decision) -> bool {
    d.state == DecisionState::Open
        && d.kind == DecisionKind::Recovery
        && d.options.iter().any(|o| o.id == "stop_failed_mission")
}
fn decision(rig: &Rig) -> Decision {
    rig.snapshot()
        .decisions
        .into_iter()
        .find(is_failure)
        .unwrap()
}
fn answer(rig: &Rig, choice: &Decision, option: &str) -> Value {
    json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"decision_id":choice.id,"option_id":option,"answer_ref":null})
}
fn control(rig: &Rig, action: &str) {
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":action}),
    );
}
fn retry(rig: &Rig, task: &Task, binding: Option<&Id>) -> Value {
    json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"task_id":task.id,"action":"retry","binding_id":binding})
}
fn queue_instruction(rig: &Rig) {
    let artifacts = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
    let body = workflow::store_artifact(
        &artifacts,
        &rig.id,
        "text/plain",
        b"Keep the original requirements and continue after recovery.",
    )
    .unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.message",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"target_task_id":null,"body_ref":body}),
    );
}

#[test]
fn authentication_failure_and_new_instructions_wait_for_one_explicit_recovery() {
    let (rig, mut actor) = failed_rig();
    queue_instruction(&rig);
    let before = rig.snapshot();
    let choice = decision(&rig);
    assert!(
        !choice.blocking,
        "independent tasks must remain dispatchable"
    );
    assert_eq!(choice.requesting_run_id, Some(before.runs[0].id.clone()));
    for _ in 0..8 {
        actor.tick().unwrap();
    }
    let after = rig.snapshot();
    assert_eq!(after.mission.revision, before.mission.revision);
    assert_eq!(after.tasks.len(), 1);
    assert_eq!(after.runs.len(), 1);
    assert_eq!(after.decisions.iter().filter(|d| is_failure(d)).count(), 1);
    assert_eq!(after.mission.automatic_start_count, 1);
    actor.shutdown();
}

#[test]
fn preparation_failure_with_queued_context_does_not_create_a_plan_loop() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    queue_instruction(&rig);
    let snapshot = rig.snapshot();
    let mut task = snapshot.tasks[0].clone();
    let artifacts = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
    task.contract.objective_ref = workflow::store_artifact(
        &artifacts,
        &rig.id,
        "text/plain",
        &vec![
            b'x';
            term_contracts::mission::validation::MissionLimits::load().max_context_bytes + 1
        ],
    )
    .unwrap();
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "fixture.context",
        "oversized",
        MissionEventType::Changed,
        vec![Entity::Task(Box::new(task))],
    )
    .unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let mut actor = rig.actor(Arc::new(move |_| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(std::io::Error::other("must not reach provider"))
    }));
    rig.tick_until(&mut actor, |s| s.decisions.iter().any(is_failure));
    let before = rig.snapshot();
    assert_eq!(
        before.runs[0].failure_code,
        Some(MissionErrorCode::ContextTooLarge)
    );
    for _ in 0..8 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
    assert_eq!(rig.snapshot().tasks.len(), 1);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    actor.shutdown();
}

#[test]
fn retry_decision_is_atomic_replays_and_preserves_the_failed_run() {
    let (rig, mut actor) = failed_rig();
    actor.shutdown();
    let old = rig.snapshot().runs[0].clone();
    let choice = decision(&rig);
    let params = answer(&rig, &choice, "retry_failed_task");
    let result = rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        params.clone(),
    );
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.tasks[0].state, TaskState::Ready);
    assert_eq!(
        snapshot.tasks[0].attempt_count, 1,
        "only dispatch consumes an attempt"
    );
    assert_eq!(snapshot.mission.open_decision_count, 0);
    assert_eq!(
        snapshot
            .decisions
            .iter()
            .find(|d| d.id == choice.id)
            .unwrap()
            .state,
        DecisionState::Answered
    );
    assert_eq!(
        rpc(
            &rig.service,
            &rig.conn,
            "mission.decision.answer",
            params.clone()
        ),
        result
    );
    let mut actor = rig.actor(factory(Arc::new(Mutex::new(vec![]))));
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let final_state = rig.snapshot();
    assert_eq!(
        final_state.runs.iter().find(|r| r.id == old.id).unwrap(),
        &old
    );
    assert_eq!(
        final_state
            .runs
            .iter()
            .filter(|r| r.task_id == old.task_id)
            .count(),
        2
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.decision.answer", params),
        result
    );
    actor.shutdown();
}

#[test]
fn paused_recovery_stays_ready_until_resume_and_obsoletes_an_old_choice() {
    let (rig, mut actor) = failed_rig();
    let old_choice = decision(&rig);
    control(&rig, "pause");
    let task = rig.snapshot().tasks[0].clone();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        retry(&rig, &task, None),
    );
    actor.tick().unwrap();
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.mission.state, MissionState::Paused);
    assert_eq!(snapshot.runs.len(), 1);
    assert_eq!(snapshot.tasks[0].state, TaskState::Ready);
    assert_eq!(
        snapshot
            .decisions
            .iter()
            .find(|d| d.id == old_choice.id)
            .unwrap()
            .state,
        DecisionState::Obsolete
    );
    assert_eq!(
        rig.service
            .handle(
                &rig.conn,
                "mission.decision.answer",
                &answer(&rig, &old_choice, "retry_failed_task")
            )
            .err()
            .unwrap()
            .code,
        MissionErrorCode::StaleDecision
    );
    actor.shutdown();
    control(&rig, "resume");
    let mut actor = rig.actor(factory(Arc::new(Mutex::new(vec![]))));
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    actor.shutdown();
}

#[test]
fn retry_rechecks_attempt_budget_and_replaces_decisions_when_policy_changes() {
    let (rig, mut actor) = failed_rig();
    let first = decision(&rig);
    let mut mission = rig.snapshot().mission;
    mission.policy.max_attempts_per_task = 1;
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.budget",
        "reduce",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    actor.tick().unwrap();
    let exhausted = decision(&rig);
    assert_ne!(first.id, exhausted.id);
    assert!(!exhausted
        .options
        .iter()
        .any(|o| o.id == "retry_failed_task"));
    let before = rig.snapshot();
    assert_eq!(
        rig.service
            .handle(
                &rig.conn,
                "mission.task.control",
                &retry(&rig, &before.tasks[0], None)
            )
            .err()
            .unwrap()
            .code,
        MissionErrorCode::PolicyDenied
    );
    assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
    let mut policy = before.mission.policy.clone();
    policy.max_attempts_per_task = 2;
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":before.mission.revision,"policy":policy,"role_bindings":before.mission.role_bindings}),
    );
    actor.tick().unwrap();
    let expanded = decision(&rig);
    assert_ne!(expanded.id, exhausted.id);
    assert!(expanded.options.iter().any(|o| o.id == "retry_failed_task"));
    actor.shutdown();
}

#[test]
fn binding_change_and_retry_commit_once_without_rewriting_old_execution() {
    let (rig, mut actor) = failed_rig();
    let original = rig.snapshot();
    let task = original.tasks[0].clone();
    let denied = retry(&rig, &task, Some(&Id::generate()));
    assert_eq!(
        rig.service
            .handle(&rig.conn, "mission.task.control", &denied)
            .err()
            .unwrap()
            .code,
        MissionErrorCode::PolicyDenied
    );
    assert_eq!(rig.snapshot().mission.revision, original.mission.revision);
    let mut binding = fake_binding();
    binding.model_id = "replacement-model".into();
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":"0","binding":binding}),
    );
    let mut policy = original.mission.policy.clone();
    policy.allowed_binding_ids.push(binding.id.clone());
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"policy":policy,"role_bindings":original.mission.role_bindings}),
    );
    let before = rig.snapshot();
    let params = retry(&rig, &task, Some(&binding.id));
    let result = rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        params.clone(),
    );
    let after = rig.snapshot();
    assert_eq!(
        after.mission.revision.get(),
        before.mission.revision.get() + 1
    );
    assert_eq!(after.tasks[0].binding_id.as_ref(), Some(&binding.id));
    assert_eq!(after.tasks[0].state, TaskState::Ready);
    assert_eq!(after.tasks[0].attempt_count, 1);
    assert_eq!(after.runs[0], before.runs[0]);
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.task.control", params),
        result
    );
    actor.tick().unwrap();
    let next = rig
        .snapshot()
        .runs
        .into_iter()
        .max_by_key(|r| r.attempt)
        .unwrap();
    assert_eq!(next.binding_snapshot.unwrap().id, binding.id);
    actor.shutdown();
}

fn add_independent_and_dependents(rig: &Rig) -> Id {
    let snapshot = rig.snapshot();
    let failed = &snapshot.tasks[0];
    let mut independent = failed.clone();
    independent.id = Id::generate();
    independent.role = Some(Role::Builder);
    independent.kind = TaskKind::Implement;
    independent.state = TaskState::Planned;
    independent.attempt_count = 0;
    independent.blocked_code = None;
    independent.active_run_id = None;
    independent.ordinal = 2;
    independent.contract.allowed_paths = vec!["independent.txt".into()];
    independent.contract.expected_outputs = vec![ExpectedOutput::Patch];
    let mut dependent = independent.clone();
    dependent.id = Id::generate();
    dependent.ordinal = 3;
    dependent.depends_on = vec![failed.id.clone()];
    let mut transitive = dependent.clone();
    transitive.id = Id::generate();
    transitive.ordinal = 4;
    transitive.depends_on = vec![dependent.id.clone()];
    let id = independent.id.clone();
    let mut mission = snapshot.mission;
    mission.phase = Phase::Implementing;
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.tasks",
        "independent",
        MissionEventType::Changed,
        vec![
            Entity::Task(Box::new(independent)),
            Entity::Task(Box::new(dependent)),
            Entity::Task(Box::new(transitive)),
        ],
    )
    .unwrap();
    id
}

#[test]
fn dependency_failures_are_visible_while_independent_work_continues() {
    let (rig, mut actor) = failed_rig();
    actor.shutdown();
    let independent = add_independent_and_dependents(&rig);
    let expected = independent.clone();
    let mut actor = rig.actor(Arc::new(move |run| {
        assert_eq!(context(run)["task_id"], expected.as_str());
        std::fs::write(
            run.workspace.as_ref().unwrap().join("independent.txt"),
            "independent result",
        )
        .unwrap();
        Ok(scripted(script(ProviderResult::Patch {
            report_text: "independent success".into(),
            verification_claims: vec![],
        })))
    }));
    rig.tick_until(&mut actor, |s| {
        s.tasks
            .iter()
            .any(|t| t.id == independent && t.state == TaskState::Succeeded)
    });
    let snapshot = rig.snapshot();
    assert_eq!(
        snapshot
            .tasks
            .iter()
            .filter(|t| t.blocked_code.as_deref() == Some("dependency_failed"))
            .count(),
        2
    );
    assert_eq!(snapshot.runs.len(), 2);
    assert_eq!(snapshot.mission.phase, Phase::Implementing);
    assert!(!decision(&rig).blocking);
    actor.shutdown();
}

#[test]
fn ending_a_failed_mission_drains_other_owned_runs_before_terminal_state() {
    let (rig, mut actor) = failed_rig();
    actor.shutdown();
    let independent = add_independent_and_dependents(&rig);
    let mut actor = rig.actor(Arc::new(|_| {
        Ok(scripted(FakeScript {
            steps: vec![
                FakeStep::Started {
                    session_id: None,
                    turn_id: None,
                },
                FakeStep::Approval {
                    request_id: "pending-action".into(),
                    question: "awaiting user".into(),
                },
            ],
            ..Default::default()
        }))
    }));
    rig.tick_until(&mut actor, |s| {
        s.runs
            .iter()
            .any(|r| r.task_id == independent && r.state == RunState::AwaitingInput)
    });
    let choice = decision(&rig);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        answer(&rig, &choice, "stop_failed_mission"),
    );
    let stopping = rig.snapshot();
    assert_eq!(stopping.mission.state, MissionState::Stopping);
    assert!(stopping.runs.iter().any(|r| r.state.holds_execution_slot()));
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Failed);
    let stopped = rig.snapshot();
    assert_eq!(
        stopped.mission.failure_code,
        Some(MissionErrorCode::AuthRequired)
    );
    assert!(!stopped.runs.iter().any(|r| r.state.holds_execution_slot()));
    assert_eq!(stopped.mission.open_decision_count, 0);
    actor.shutdown();
}

#[test]
fn failed_decision_storage_is_reconciled_without_another_provider_start() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let connection = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_recovery_decision BEFORE INSERT ON orch_entities WHEN NEW.kind = 'decision' BEGIN SELECT RAISE(FAIL, 'fixture recovery storage outage'); END").unwrap();
    let mut actor = rig.actor(failed_factory(MissionErrorCode::AuthRequired));
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Err(error) = actor.tick() {
            assert_eq!(error.code, MissionErrorCode::StorageUnavailable);
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rig.snapshot().runs[0].state, RunState::Failed);
    assert!(rig.snapshot().decisions.is_empty());
    connection
        .execute_batch("DROP TRIGGER fail_recovery_decision")
        .unwrap();
    actor.tick().unwrap();
    let before = rig.snapshot();
    for _ in 0..4 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
    assert_eq!(rig.snapshot().runs.len(), 1);
    assert_eq!(
        rig.snapshot()
            .decisions
            .iter()
            .filter(|d| is_failure(d))
            .count(),
        1
    );
    actor.shutdown();
}

#[test]
fn recovery_cannot_retry_an_unknown_execution_or_a_stale_failed_run() {
    let (rig, mut actor) = failed_rig();
    actor.shutdown();
    let old_choice = decision(&rig);
    let task = rig.snapshot().tasks[0].clone();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        retry(&rig, &task, None),
    );
    let mut actor = rig.actor(Arc::new(|_| {
        Ok(scripted(FakeScript {
            steps: vec![FakeStep::Disconnect],
            ..Default::default()
        }))
    }));
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::Unknown)
    });
    let before = rig.snapshot();
    assert_eq!(
        rig.service
            .handle(
                &rig.conn,
                "mission.task.control",
                &retry(&rig, &before.tasks[0], None)
            )
            .err()
            .unwrap()
            .code,
        MissionErrorCode::InvalidState
    );
    assert_eq!(
        rig.service
            .handle(
                &rig.conn,
                "mission.decision.answer",
                &answer(&rig, &old_choice, "retry_failed_task")
            )
            .err()
            .unwrap()
            .code,
        MissionErrorCode::StaleDecision
    );
    assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
    assert_eq!(rig.snapshot().runs.len(), 2);
    assert!(rig
        .snapshot()
        .runs
        .iter()
        .any(|r| r.state == RunState::Unknown && r.state.holds_execution_slot()));
    actor.shutdown();
}

#[test]
fn retry_rechecks_disabled_bindings_and_mission_budget_without_mutation() {
    let (rig, mut actor) = failed_rig();
    let before = rig.snapshot();
    let mut binding: Binding =
        serde_json::from_value(rig.storage.mission_bindings().unwrap().remove(0)).unwrap();
    binding.enabled = false;
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":binding.revision,"binding":binding}),
    );
    let choice = decision(&rig);
    assert_eq!(
        rig.service
            .handle(
                &rig.conn,
                "mission.decision.answer",
                &answer(&rig, &choice, "retry_failed_task")
            )
            .err()
            .unwrap()
            .code,
        MissionErrorCode::ModelUnavailable
    );
    assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
    let mut mission = before.mission;
    mission.policy.max_automatic_starts = mission.automatic_start_count;
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.budget",
        "mission",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    let snapshot = rig.snapshot();
    assert_eq!(
        rig.service
            .handle(
                &rig.conn,
                "mission.task.control",
                &retry(&rig, &snapshot.tasks[0], None)
            )
            .err()
            .unwrap()
            .code,
        MissionErrorCode::BudgetExceeded
    );
    assert_eq!(rig.snapshot().mission.revision, snapshot.mission.revision);
    assert_eq!(rig.snapshot().tasks[0].state, TaskState::Failed);
    assert_eq!(decision(&rig).id, choice.id);
    actor.shutdown();
}
