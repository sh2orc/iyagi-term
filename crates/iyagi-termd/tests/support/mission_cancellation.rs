//! Task cancellation preserves requirements and needs an explicit new intent.
use super::*;
use iyagi_termd_lib::agent_runtime::AdapterEvent;
use term_contracts::mission::MissionErrorCode;

#[test]
fn dispatched_cancellation_requires_persisted_exec_exit_before_retry() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let (mut exec, body) = prepared_exec(&rig);
    exec.launch_manifest_ref = rig
        .service
        .exec_persistence()
        .prepare(exec.clone(), &body)
        .unwrap();
    exec = observed_spawn(exec);
    rig.service.exec_persistence().update(exec.clone()).unwrap();
    let task = rig.snapshot().tasks[0].clone();
    cancel(&rig, &task);
    let run = rig.snapshot().runs[0].clone();
    assert!(rig
        .service
        .handle(
            &rig.conn,
            "mission.task.control",
            &task_params(&rig, &task, "retry")
        )
        .is_err());
    // A late terminal callback cannot replace the durable supervisor evidence.
    rig.service
        .apply_adapter_event(
            &rig.id,
            &AdapterEvent::Failed {
                run_id: run.id.clone(),
                fencing_token: run.fencing_token.get(),
                code: MissionErrorCode::ProviderUnavailable,
                message: "fixture cancellation completed".into(),
            },
            None,
        )
        .unwrap();
    assert!(rig
        .service
        .handle(
            &rig.conn,
            "mission.task.control",
            &task_params(&rig, &task, "retry")
        )
        .is_err());
    exec.state = ExecState::Exited;
    exec.ended_at = Some(term_storage::time::now_iso8601());
    rig.service.exec_persistence().update(exec.clone()).unwrap();
    assert!(
        rig.service
            .handle(
                &rig.conn,
                "mission.task.control",
                &task_params(&rig, &task, "retry")
            )
            .is_err(),
        "retained writer ownership still prevents retry"
    );
    let snapshot = rig.snapshot();
    let mut workspace = snapshot.workspaces[0].clone();
    workspace.state = WorkspaceState::Retained;
    workspace.writer_run_id = None;
    save(
        &rig,
        snapshot.mission,
        vec![Entity::Workspace(Box::new(workspace))],
    );
    let before = rig.snapshot();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        task_params(&rig, &task, "retry"),
    );
    let after = rig.snapshot();
    assert_eq!(after.runs, before.runs);
    assert_eq!(after.execs, before.execs);
    assert_eq!(after.workspaces, before.workspaces);
    assert_eq!(after.tasks[0].state, TaskState::Ready);
    assert_eq!(after.tasks[0].attempt_count, 1);
}

fn control(rig: &Rig, action: &str) {
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),
        "mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":action}),
    );
}
fn task_params(rig: &Rig, task: &Task, action: &str) -> Value {
    json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,
        "task_id":task.id,"action":action,"binding_id":null})
}
fn save(rig: &Rig, mission: Mission, changes: Vec<Entity>) {
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.cancellation",
        "change",
        MissionEventType::Changed,
        changes,
    )
    .unwrap();
}
fn planned() -> (Rig, MissionActor) {
    let rig = Rig::new(false, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let mut actor = rig.actor(factory(Arc::new(Mutex::new(vec![]))));
    rig.tick_until(&mut actor, |s| {
        s.decisions.iter().any(|d| d.kind == DecisionKind::Plan)
    });
    control(&rig, "pause");
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Paused);
    let snapshot = rig.snapshot();
    let proposal = snapshot
        .decisions
        .iter()
        .find(|d| d.kind == DecisionKind::Plan)
        .unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.plan.apply",
        json!({"request_id":Id::generate(),
        "mission_id":rig.id,"expected_revision":snapshot.mission.revision,"proposal_ref":proposal.question_ref}),
    );
    (rig, actor)
}
fn writer(rig: &Rig) -> Task {
    rig.snapshot()
        .tasks
        .into_iter()
        .find(|t| t.contract.allowed_paths == ["api.txt"])
        .unwrap()
}
fn cancel(rig: &Rig, task: &Task) {
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        task_params(rig, task, "cancel"),
    );
}
fn accept(rig: &Rig) {
    let snapshot = rig.snapshot();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.accept",
        json!({"request_id":Id::generate(),
        "mission_id":rig.id,"expected_revision":snapshot.mission.revision,"candidate_id":snapshot.mission.candidate_id,
        "acknowledged_verification_ids":snapshot.verifications.iter().map(|v| &v.id).collect::<Vec<_>>(),"human_requirement_ids":[]}),
    );
    assert_eq!(rig.snapshot().mission.state, MissionState::Completed);
}

#[test]
fn cancelled_required_writer_waits_through_restart_then_explicit_retry_completes() {
    let (rig, mut actor) = planned();
    let old = writer(&rig);
    cancel(&rig, &old);
    actor.shutdown();
    let mut actor = rig.actor(factory(Arc::new(Mutex::new(vec![]))));
    control(&rig, "resume");
    rig.tick_until(&mut actor, |s| {
        s.tasks
            .iter()
            .any(|t| t.contract.allowed_paths == ["ui.txt"] && t.state == TaskState::Succeeded)
    });
    for _ in 0..6 {
        actor.tick().unwrap();
    }
    let before = rig.snapshot();
    assert_eq!(before.mission.phase, Phase::Implementing);
    assert!(before.mission.candidate_id.is_none());
    assert!(!before.runs.iter().any(|r| r.task_id == old.id));
    let params = task_params(&rig, &old, "retry");
    let receipt = rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        params.clone(),
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.task.control", params),
        receipt
    );
    assert_eq!(
        rig.snapshot()
            .tasks
            .iter()
            .find(|t| t.id == old.id)
            .unwrap()
            .attempt_count,
        0
    );
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    assert_eq!(
        rig.snapshot()
            .runs
            .iter()
            .filter(|r| r.task_id == old.id)
            .count(),
        1
    );
    accept(&rig);
    actor.shutdown();
}

fn replacement_params(rig: &Rig, old: &Task) -> Value {
    let snapshot = rig.snapshot();
    let replacement = TaskSpec {
        id: Id::generate(),
        title: old.title.clone(),
        kind: old.kind,
        role: old.role,
        required: true,
        parent_task_id: None,
        depends_on: vec![],
        contract: old.contract.clone(),
        binding_id: old.binding_id.clone(),
        replacement_of: Some(old.id.clone()),
    };
    let proposal = PlanProposal {
        id: Id::generate(),
        mission_id: rig.id.clone(),
        based_on_plan_revision: snapshot.mission.plan_revision,
        tasks: vec![replacement],
        retire_task_ids: vec![old.id.clone()],
        rationale_ref: snapshot.mission.goal_ref.clone(),
    };
    let artifacts = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
    let reference = workflow::store_artifact(
        &artifacts,
        &rig.id,
        "application/json",
        &serde_json::to_vec(&proposal).unwrap(),
    )
    .unwrap();
    json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"proposal_ref":reference})
}

#[test]
fn cancelled_writer_replacement_rolls_back_replays_and_preserves_requirements() {
    let (rig, mut actor) = planned();
    let old = writer(&rig);
    cancel(&rig, &old);
    let before = rig.snapshot();
    let params = replacement_params(&rig, &old);
    let db = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_cancel_replan BEFORE INSERT ON orch_events BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    assert_eq!(
        rig.service
            .handle(&rig.conn, "mission.plan.apply", &params)
            .err()
            .unwrap()
            .code,
        MissionErrorCode::StorageUnavailable
    );
    assert_eq!(rig.snapshot().mission, before.mission);
    assert_eq!(rig.snapshot().tasks, before.tasks);
    db.execute_batch("DROP TRIGGER deny_cancel_replan;")
        .unwrap();
    let receipt = rpc(
        &rig.service,
        &rig.conn,
        "mission.plan.apply",
        params.clone(),
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.plan.apply", params),
        receipt
    );
    let after = rig.snapshot();
    assert_eq!(after.mission.requirements, before.mission.requirements);
    assert_eq!(
        after.tasks.iter().find(|t| t.id == old.id).unwrap().state,
        TaskState::Superseded
    );
    assert_eq!(
        after
            .tasks
            .iter()
            .filter(|t| t.replacement_of.as_ref() == Some(&old.id))
            .count(),
        1
    );
    control(&rig, "resume");
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    assert!(!rig.snapshot().runs.iter().any(|r| r.task_id == old.id));
    accept(&rig);
    actor.shutdown();
}

#[test]
fn lead_message_replaces_cancelled_work_and_reaches_acceptance() {
    let (rig, mut actor) = planned();
    let old = writer(&rig);
    cancel(&rig, &old);
    actor.shutdown();
    let normal = factory(Arc::new(Mutex::new(vec![])));
    let source_id = old.id.clone();
    let mut actor = rig.actor(Arc::new(move |run| {
        let ctx = context(run);
        if ctx["task_kind"] != "plan" { return normal(run); }
        assert!(!ctx["messages"].as_array().unwrap().is_empty());
        let old = ctx["tasks"].as_array().unwrap().iter().find(|t| t["id"] == source_id.as_str()).unwrap();
        assert_eq!(old["state"], "cancelled");
        let replacement = serde_json::from_value(json!({"local_key":"replace_cancelled", "title":"Replace cancelled writer",
            "kind":old["kind"],"role":old["role"],"required":true,"parent_key":null,"depends_on_keys":[],
            "objective_text":"Implement the original requirement after explicit replanning.","requirement_ids":old["contract"]["requirement_ids"],
            "input_artifact_ids":[],"allowed_paths":old["contract"]["allowed_paths"],"expected_outputs":["patch"],
            "verification_ids":[],"specialty":null,"binding_id":old["binding_id"],"replacement_of":source_id})).unwrap();
        Ok(scripted(script(ProviderResult::Plan { based_on_plan_revision: ctx["plan_revision"].as_u64().unwrap() as u32,
            tasks: vec![replacement], retire_task_ids: vec![source_id.clone()], rationale_text: "Explicitly replace the cancelled writer without changing requirements.".into() })))
    }));
    let mut mission = rig.snapshot().mission;
    mission.policy.allow_automatic_plan_apply = true;
    save(&rig, mission, vec![]);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.message",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"target_task_id":null,"body_ref":rig.snapshot().mission.goal_ref}),
    );
    control(&rig, "resume");
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let after = rig.snapshot();
    assert_eq!(
        after.tasks.iter().find(|t| t.id == old.id).unwrap().state,
        TaskState::Superseded
    );
    assert_eq!(
        after
            .tasks
            .iter()
            .filter(|t| t.replacement_of.as_ref() == Some(&old.id))
            .count(),
        1
    );
    accept(&rig);
    actor.shutdown();
}

#[test]
fn unsent_cancellation_retry_retains_run_and_rejects_missing_or_unknown_exit_evidence() {
    for fault in ["none", "missing_exec", "unknown", "active"] {
        let (rig, mut actor) = planned();
        control(&rig, "resume");
        rig.service.dispatch_tick().unwrap();
        let old = writer(&rig);
        cancel(&rig, &old);
        let snapshot = rig.snapshot();
        let mut run = snapshot
            .runs
            .iter()
            .find(|r| r.task_id == old.id)
            .unwrap()
            .clone();
        assert_eq!(run.state, RunState::Cancelled);
        let mut task = snapshot
            .tasks
            .iter()
            .find(|t| t.id == old.id)
            .unwrap()
            .clone();
        match fault {
            "missing_exec" => run.exec_id = Some(Id::generate()),
            "unknown" => run.state = RunState::Unknown,
            "active" => task.active_run_id = Some(run.id.clone()),
            _ => (),
        }
        save(
            &rig,
            snapshot.mission,
            vec![
                Entity::Run(Box::new(run.clone())),
                Entity::Task(Box::new(task.clone())),
            ],
        );
        let retry = task_params(&rig, &task, "retry");
        if fault == "none" {
            rpc(&rig.service, &rig.conn, "mission.task.control", retry);
            assert_eq!(
                rig.snapshot().runs.iter().find(|r| r.id == run.id).unwrap(),
                &run
            );
        } else {
            let before = rig.snapshot();
            assert_eq!(
                rig.service
                    .handle(&rig.conn, "mission.task.control", &retry)
                    .err()
                    .unwrap()
                    .code,
                MissionErrorCode::InvalidState
            );
            let replacement = replacement_params(&rig, &task);
            assert!(rig
                .service
                .handle(&rig.conn, "mission.plan.apply", &replacement)
                .is_err());
            assert_eq!(rig.snapshot().mission, before.mission);
        }
        actor.shutdown();
    }
}

#[test]
fn cancelled_required_verification_or_review_is_not_automatically_recreated() {
    for kind in [TaskKind::Verify, TaskKind::Review] {
        let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
        let mut actor = rig.actor(factory(Arc::new(Mutex::new(vec![]))));
        rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
        let mut snapshot = rig.snapshot();
        let mut task = snapshot
            .tasks
            .iter()
            .find(|t| t.kind == kind)
            .unwrap()
            .clone();
        task.id = Id::generate();
        task.ordinal = snapshot.tasks.iter().map(|t| t.ordinal).max().unwrap() + 1;
        task.state = TaskState::Planned;
        task.attempt_count = 0;
        task.active_run_id = None;
        task.workspace_id = None;
        snapshot.mission.phase = if kind == TaskKind::Verify {
            Phase::Validating
        } else {
            Phase::Reviewing
        };
        save(
            &rig,
            snapshot.mission,
            vec![Entity::Task(Box::new(task.clone()))],
        );
        cancel(&rig, &task);
        let before = rig.snapshot();
        for _ in 0..6 {
            actor.tick().unwrap();
        }
        let after = rig.snapshot();
        assert_eq!(after.mission.phase, before.mission.phase);
        assert_eq!(after.tasks, before.tasks);
        assert_eq!(after.runs, before.runs);
        rpc(
            &rig.service,
            &rig.conn,
            "mission.task.control",
            task_params(&rig, &task, "retry"),
        );
        rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
        assert_eq!(
            rig.snapshot()
                .tasks
                .iter()
                .find(|t| t.id == task.id)
                .unwrap()
                .state,
            TaskState::Succeeded
        );
        accept(&rig);
        actor.shutdown();
    }
}

#[test]
fn optional_unstarted_work_is_not_silently_skipped_at_integration() {
    let (rig, mut actor) = planned();
    let old = writer(&rig);
    let mut optional = old.clone();
    optional.id = Id::generate();
    optional.ordinal = rig
        .snapshot()
        .tasks
        .iter()
        .map(|t| t.ordinal)
        .max()
        .unwrap()
        + 1;
    optional.required = false;
    optional.title = "Optional documentation".into();
    optional.contract.allowed_paths = vec!["notes.txt".into()];
    optional.state = TaskState::Blocked;
    optional.blocked_code = Some("manual_fixture_hold".into());
    save(
        &rig,
        rig.snapshot().mission,
        vec![Entity::Task(Box::new(optional.clone()))],
    );
    control(&rig, "resume");
    rig.tick_until(&mut actor, |s| {
        s.tasks
            .iter()
            .filter(|t| t.required && t.kind == TaskKind::Implement)
            .all(|t| t.state == TaskState::Succeeded)
    });
    for _ in 0..6 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().mission.phase, Phase::Implementing);
    cancel(&rig, &optional);
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let before = rig.snapshot();
    assert_eq!(
        rig.service
            .handle(
                &rig.conn,
                "mission.task.control",
                &task_params(&rig, &optional, "retry")
            )
            .err()
            .unwrap()
            .code,
        MissionErrorCode::InvalidState
    );
    assert_eq!(rig.snapshot().mission, before.mission);
    accept(&rig);
    actor.shutdown();
}
