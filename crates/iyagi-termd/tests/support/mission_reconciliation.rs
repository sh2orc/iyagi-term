//! Durable termination recovery: native cleanup itself is exercised by the
//! gated Exec test in the parent module. These cases inject lifecycle records.
use super::*;
use iyagi_termd_lib::agent_runtime::AdapterEvent;
use term_contracts::mission::MissionErrorCode;

fn uncertain_exec() -> (Rig, ExecRecord) {
    let instant = Instant::now();
    let rig = Rig::with_clock(
        false,
        &["status", "--porcelain"],
        true,
        Some(Arc::new(move || instant)),
    );
    let (mut exec, body) = prepared_exec(&rig);
    let store = rig.service.exec_persistence();
    exec.launch_manifest_ref = store.prepare(exec.clone(), &body).unwrap();
    exec = observed_spawn(exec);
    store.update(exec.clone()).unwrap();
    let run = rig.snapshot().runs[0].clone();
    rig.service
        .apply_adapter_event(
            &rig.id,
            &AdapterEvent::Disconnected {
                run_id: run.id,
                fencing_token: run.fencing_token.get(),
            },
            None,
        )
        .unwrap();
    (rig, exec)
}
fn finish(rig: &Rig, mut exec: ExecRecord) {
    exec.state = ExecState::Exited;
    exec.ended_at = Some(term_storage::time::now_iso8601());
    rig.service.exec_persistence().update(exec).unwrap();
}
fn decision(rig: &Rig) -> Decision {
    rig.snapshot()
        .decisions
        .into_iter()
        .find(|d| {
            d.state == DecisionState::Open
                && d.options.iter().any(|o| o.id == "stop_reconciled_mission")
        })
        .unwrap()
}
fn answer(rig: &Rig, decision: &Decision, option: &str) -> Value {
    json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,
        "decision_id":decision.id,"option_id":option,"answer_ref":null})
}

#[test]
fn missing_adapter_or_unfinished_exec_does_not_release_unknown_ownership() {
    let (rig, exec) = uncertain_exec();
    let mut actor = rig.actor(Arc::new(|_| panic!("must not start another provider")));
    for _ in 0..3 {
        actor.tick().unwrap();
    }
    let before = rig.snapshot();
    assert!(before.runs[0].holds_execution_slot());
    assert!(before.runs[0].reconciliation_ref.is_none());
    assert!(before.tasks[0].active_run_id.is_some());
    assert!(before.workspaces[0].writer_run_id.is_some());
    assert_eq!(before.execs[0], exec);
    let params = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":before.mission.revision,
        "task_id":before.tasks[0].id,"action":"retry","binding_id":null});
    assert_eq!(
        rig.service
            .handle(&rig.conn, "mission.task.control", &params)
            .err()
            .unwrap()
            .code,
        MissionErrorCode::InvalidState
    );
}

#[test]
fn termination_proof_workspace_release_and_recovery_decision_roll_back_together() {
    let (rig, exec) = uncertain_exec();
    finish(&rig, exec);
    let before = rig.snapshot();
    let db = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_reconcile BEFORE INSERT ON orch_events BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    assert!(rig.service.dispatch_tick().is_err());
    let failed = rig.snapshot();
    assert_eq!(failed.runs, before.runs);
    assert_eq!(failed.tasks, before.tasks);
    assert_eq!(failed.workspaces, before.workspaces);
    assert_eq!(failed.decisions, before.decisions);
    db.execute_batch("DROP TRIGGER fail_reconcile").unwrap();
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    let after = rig.snapshot();
    assert_eq!(
        term_core::mission::budget::summarize_cost(&before.runs),
        term_core::mission::budget::summarize_cost(&after.runs),
        "local cleanup cannot settle unknown provider usage"
    );
    let mut expected = before.runs[0].clone();
    expected.reconciliation_ref = after.runs[0].reconciliation_ref.clone();
    assert!(expected.reconciliation_ref.is_some());
    // The proof kind distinguishes an observed exit from a user attestation.
    assert_eq!(
        after.runs[0].reconciliation_kind,
        Some(ReconciliationKind::ExecExited)
    );
    expected.reconciliation_kind = after.runs[0].reconciliation_kind;
    assert_eq!(
        after.runs[0], expected,
        "historical outcome/times/fence were rewritten"
    );
    assert_eq!(after.workspaces[0].state, WorkspaceState::Quarantined);
    assert!(after.workspaces[0].writer_run_id.is_none());
    assert!(after.tasks[0].workspace_id.is_none());
    assert_eq!(
        after.tasks[0].blocked_code.as_deref(),
        Some("outcome_unknown_ended")
    );
    assert!(!decision(&rig).blocking);
    for _ in 0..3 {
        assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    }
    assert_eq!(
        rig.snapshot().mission.revision,
        after.mission.revision,
        "reconciliation churn"
    );
}

#[test]
fn explicit_recovery_creates_one_new_attempt_and_preserves_unknown_run_and_worktree() {
    let (rig, exec) = uncertain_exec();
    finish(&rig, exec);
    rig.service.recover_on_startup().unwrap();
    rig.service.dispatch_tick().unwrap();
    let before = rig.snapshot();
    let old = before.runs[0].clone();
    let choice = decision(&rig);
    let params = answer(&rig, &choice, "retry_reconciled_task");
    let response = rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        params.clone(),
    );
    assert_eq!(rig.snapshot().tasks[0].state, TaskState::Ready);
    assert_eq!(rig.snapshot().tasks[0].attempt_count, 1);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.decision.answer", params),
        response
    );
    let after = rig.snapshot();
    assert_eq!(after.runs.iter().find(|r| r.id == old.id).unwrap(), &old);
    let next = after.runs.iter().find(|r| r.id != old.id).unwrap();
    assert_eq!(next.attempt, 2);
    assert_eq!(next.dispatch_state, RunDispatchState::Unsent);
    assert_eq!(next.binding_snapshot, old.binding_snapshot);
    let intent = rig
        .storage
        .mission_outbox()
        .unwrap()
        .into_iter()
        .find(|o| o.run_id.as_ref() == Some(&next.id))
        .unwrap();
    let prepared = rig
        .service
        .prepare_run(&intent, &rig.dir.path().join("missions"))
        .unwrap()
        .unwrap();
    assert_ne!(Some(&prepared.workspace.id), old.workspace_id.as_ref());
    let retained = rig
        .snapshot()
        .workspaces
        .into_iter()
        .find(|w| Some(&w.id) == old.workspace_id.as_ref())
        .unwrap();
    assert_eq!(retained, before.workspaces[0]);
    assert!(Path::new(&retained.path).exists());
    assert!(!rig
        .service
        .apply_adapter_event(
            &rig.id,
            &AdapterEvent::Failed {
                run_id: old.id.clone(),
                fencing_token: old.fencing_token.get(),
                code: MissionErrorCode::ResultInvalid,
                message: "late callback".into(),
            },
            None
        )
        .unwrap());
    assert_eq!(
        rig.snapshot().runs.iter().find(|r| r.id == old.id).unwrap(),
        &old
    );
    assert!(rig.storage.mission_snapshot(&rig.id).unwrap().unwrap().entities.iter()
        .any(|e| matches!(e, Entity::Message(m) if m.role == MessageRole::System && m.delivery == MessageDelivery::Delivered)));
    let fresh = MissionService::new(
        rig.storage.clone(),
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
    );
    fresh.recover_on_startup().unwrap();
    assert_eq!(
        rig.snapshot().runs.iter().find(|r| r.id == old.id).unwrap(),
        &old
    );
}

#[test]
fn recovery_rejects_exhausted_budget_stale_task_and_mismatched_proof() {
    let (rig, exec) = uncertain_exec();
    finish(&rig, exec);
    rig.service.dispatch_tick().unwrap();
    let choice = decision(&rig);
    let mut snapshot = rig.snapshot();
    snapshot.mission.policy.max_attempts_per_task = 1;
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "fixture.budget",
        "limit",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    assert_eq!(
        rig.service
            .handle(
                &rig.conn,
                "mission.decision.answer",
                &answer(&rig, &choice, "retry_reconciled_task")
            )
            .err()
            .unwrap()
            .code,
        MissionErrorCode::PolicyDenied
    );
    rig.service.dispatch_tick().unwrap();
    let bounded = decision(&rig);
    assert!(!bounded
        .options
        .iter()
        .any(|o| o.id == "retry_reconciled_task"));
    assert_eq!(
        rig.snapshot()
            .decisions
            .iter()
            .find(|d| d.id == choice.id)
            .unwrap()
            .state,
        DecisionState::Obsolete
    );
    let mut snapshot = rig.snapshot();
    let mut run = snapshot.runs.remove(0);
    run.reconciliation_ref = Some(run.context_ref.clone());
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "fixture.bad-proof",
        "bad",
        MissionEventType::Changed,
        vec![Entity::Run(Box::new(run))],
    )
    .unwrap();
    assert_eq!(
        rig.service
            .handle(
                &rig.conn,
                "mission.decision.answer",
                &answer(&rig, &bounded, "stop_reconciled_mission")
            )
            .err()
            .unwrap()
            .code,
        MissionErrorCode::StaleDecision
    );
}

#[test]
fn stopping_unknown_mission_settles_only_after_durable_termination() {
    let (rig, exec) = uncertain_exec();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"action":"cancel"}),
    );
    let mut actor = rig.actor(Arc::new(|_| panic!("cancel must not start a provider")));
    actor.tick().unwrap();
    assert_eq!(rig.snapshot().mission.state, MissionState::Stopping);
    finish(&rig, exec);
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Cancelled);
    let after = rig.snapshot();
    assert_eq!(after.runs[0].state, RunState::Unknown);
    assert!(after.runs[0].reconciliation_ref.is_some());
    assert!(after.tasks[0].active_run_id.is_none());
    assert_eq!(after.tasks[0].state, TaskState::Cancelled);
    assert_eq!(after.mission.open_decision_count, 0);
}

#[test]
fn paused_reconciled_retry_waits_for_resume_and_does_not_reopen_recovery() {
    let (rig, exec) = uncertain_exec();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"action":"pause"}),
    );
    finish(&rig, exec);
    let mut actor = rig.actor(Arc::new(|_| panic!("paused retry cannot start")));
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Paused);
    let choice = decision(&rig);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        answer(&rig, &choice, "retry_reconciled_task"),
    );
    for _ in 0..3 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().runs.len(), 1);
    assert_eq!(rig.snapshot().tasks[0].state, TaskState::Ready);
    assert_eq!(rig.snapshot().mission.open_decision_count, 0);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"action":"resume"}),
    );
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
}

#[test]
fn interrupted_history_is_preserved_and_cannot_bypass_termination_checks() {
    let (rig, exec) = uncertain_exec();
    let mut snapshot = rig.snapshot();
    let mut run = snapshot.runs.remove(0);
    run.state = RunState::Interrupted;
    run.dispatch_state = RunDispatchState::MayHaveSent;
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "fixture.interrupted",
        "interrupt",
        MissionEventType::Changed,
        vec![Entity::Run(Box::new(run.clone()))],
    )
    .unwrap();
    rig.service.recover_on_startup().unwrap();
    assert_eq!(rig.snapshot().runs[0], run);
    assert!(run.holds_execution_slot());
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"action":"pause"}),
    );
    // Cancellation of an interrupted historical Run must not turn it live
    // again or permit a late provider callback to overwrite its outcome.
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"task_id":run.task_id,"action":"cancel","binding_id":null}),
    );
    assert_eq!(rig.snapshot().runs[0], run);
    finish(&rig, exec);
    rig.service.dispatch_tick().unwrap();
    let after = rig.snapshot();
    run.reconciliation_ref = after.runs[0].reconciliation_ref.clone();
    run.reconciliation_kind = Some(ReconciliationKind::ExecExited);
    assert_eq!(after.runs[0], run);
    assert!(!run.holds_execution_slot());
    assert_eq!(after.tasks[0].state, TaskState::Cancelled);
}

#[test]
fn missing_or_tampered_launch_manifest_keeps_the_workspace_owned() {
    let (rig, exec) = uncertain_exec();
    finish(&rig, exec);
    let snapshot = rig.snapshot();
    let mut exec = snapshot.execs[0].clone();
    exec.launch_manifest_ref.sha256 = "0".repeat(64);
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "fixture.corrupt",
        "manifest",
        MissionEventType::Changed,
        vec![Entity::Exec(Box::new(exec))],
    )
    .unwrap();
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    let after = rig.snapshot();
    assert!(after.runs[0].reconciliation_ref.is_none());
    assert!(after.runs[0].holds_execution_slot());
    assert!(after.workspaces[0].writer_run_id.is_some());
}
