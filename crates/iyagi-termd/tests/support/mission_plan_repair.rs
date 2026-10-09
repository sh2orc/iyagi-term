use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use term_contracts::mission::MissionErrorCode;

fn is_recovery(d: &Decision) -> bool {
    d.state == DecisionState::Open && d.options.iter().any(|o| o.id == "stop_failed_mission")
}
fn control(rig: &Rig, action: &str) {
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":action}),
    );
}
fn wrong_kind() -> ProviderResult {
    ProviderResult::Patch {
        report_text: "Rejected answer marker; never execute this text.".into(),
        verification_claims: vec![],
    }
}
fn repair_factory(failures: usize, contexts: Arc<Mutex<Vec<(RunStart, Value)>>>) -> AdapterFactory {
    let normal = factory(Arc::new(Mutex::new(vec![])));
    let calls = AtomicUsize::new(0);
    Arc::new(move |run| {
        let ctx = context(run);
        if ctx["task_kind"] == "plan" {
            assert_eq!(
                run.workspace_access,
                iyagi_termd_lib::agent_runtime::WorkspaceAccess::ReadOnly
            );
            contexts.lock().unwrap().push((run.clone(), ctx));
            if calls.fetch_add(1, Ordering::SeqCst) < failures {
                return Ok(scripted(script(wrong_kind())));
            }
        }
        normal(run)
    })
}
fn wait(rig: &Rig, actor: &mut MissionActor, attempt: u32) -> Run {
    rig.tick_until(actor, |s| {
        s.tasks[0].attempt_count == attempt
            && s.tasks[0].blocked_code.as_deref() == Some("plan_format_repair")
    });
    rig.snapshot()
        .runs
        .into_iter()
        .find(|r| r.attempt == attempt)
        .unwrap()
}
fn save(rig: &Rig, mission: Mission, entities: Vec<Entity>) {
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.plan_repair",
        "change",
        MissionEventType::Changed,
        entities,
    )
    .unwrap();
}
fn body(rig: &Rig, reference: &ArtifactRef) -> String {
    String::from_utf8(
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"))
            .read_mission_body(&rig.id, reference, 262_144)
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn two_corrections_receive_the_exact_error_and_answer_then_complete_without_partial_adoption() {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let contexts = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(repair_factory(2, contexts.clone()));
    let first = wait(&rig, &mut actor, 1);
    let second = wait(&rig, &mut actor, 2);
    let failed = rig.snapshot();
    assert_eq!(failed.tasks.len(), 1);
    assert_eq!(failed.mission.plan_revision, 0);
    assert!(failed.candidates.is_empty() && failed.decisions.is_empty());
    assert_eq!(failed.runs.iter().find(|r| r.id == first.id), Some(&first));
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let final_state = rig.snapshot();
    assert_eq!(final_state.runs.len(), 7);
    assert_eq!(
        final_state.runs.iter().find(|r| r.id == first.id),
        Some(&first)
    );
    assert_eq!(
        final_state.runs.iter().find(|r| r.id == second.id),
        Some(&second)
    );
    assert!(!final_state.decisions.iter().any(is_recovery));
    let contexts = contexts.lock().unwrap();
    assert_eq!(contexts.len(), 3);
    assert!(contexts[0].1["plan_repair"].is_null());
    for (index, prior) in [&first, &second].iter().enumerate() {
        let ctx = &contexts[index + 1].1["plan_repair"];
        assert_eq!(ctx["failed_run_id"], prior.id.as_str());
        assert_eq!(
            ctx["diagnostic"],
            body(&rig, prior.result_ref.as_ref().unwrap())
        );
        assert!(ctx["diagnostic"].as_str().unwrap().contains("kind=plan"));
        let Some(RetryEvidence::PlanFormatRejected {
            plan_revision,
            rejected_result_ref,
        }) = &prior.retry_evidence
        else {
            panic!("missing validator evidence")
        };
        assert_eq!(*plan_revision, 0);
        assert_eq!(
            ctx["rejected_answer"],
            body(&rig, rejected_result_ref.as_ref().unwrap())
        );
        assert!(ctx["rejected_answer"]
            .as_str()
            .unwrap()
            .contains("Rejected answer marker"));
        assert_eq!(
            contexts[index].1["task_contract"],
            contexts[index + 1].1["task_contract"]
        );
        assert_ne!(contexts[index].0.workspace, contexts[index + 1].0.workspace);
    }
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
    actor.shutdown();
}

#[test]
fn two_automatic_corrections_are_the_limit_even_when_more_task_attempts_are_allowed() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let mut mission = rig.snapshot().mission;
    mission.policy.max_attempts_per_task = 10;
    save(&rig, mission, vec![]);
    let contexts = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(repair_factory(usize::MAX, contexts.clone()));
    rig.tick_until(&mut actor, |s| s.decisions.iter().any(is_recovery));
    let before = rig.snapshot();
    assert_eq!(before.runs.len(), 3);
    assert_eq!(before.tasks.len(), 1);
    assert_eq!(before.mission.plan_revision, 0);
    let decision = before.decisions.iter().find(|d| is_recovery(d)).unwrap();
    let last = before.runs.iter().max_by_key(|r| r.attempt).unwrap();
    assert_eq!(decision.requesting_run_id.as_ref(), Some(&last.id));
    assert!(decision.options.iter().any(|o| o.id == "retry_failed_task"));
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
    assert_eq!(contexts.lock().unwrap().len(), 3);
    actor.shutdown();
}

#[test]
fn paused_repair_survives_service_restart_and_resumes_once() {
    let mut rig = Rig::new(true, &["status", "--porcelain"]);
    let contexts = Arc::new(Mutex::new(vec![]));
    let factory = repair_factory(1, contexts.clone());
    let mut actor = rig.actor(factory.clone());
    let first = wait(&rig, &mut actor, 1);
    control(&rig, "pause");
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Paused);
    actor.shutdown();
    rig.service = Arc::new(MissionService::new(
        rig.storage.clone(),
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
    ));
    rig.service.recover_on_startup().unwrap();
    let mut actor = rig.actor(factory);
    let before = rig.snapshot();
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
    assert_eq!(contexts.lock().unwrap().len(), 1);
    assert_eq!(
        rig.snapshot().tasks[0].blocked_code.as_deref(),
        Some("plan_format_repair")
    );
    control(&rig, "resume");
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    assert_eq!(contexts.lock().unwrap().len(), 2);
    assert_eq!(
        rig.snapshot().runs.iter().find(|r| r.id == first.id),
        Some(&first)
    );
    actor.shutdown();
}

#[test]
fn repair_commit_outage_never_launches_an_uncommitted_attempt() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let contexts = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(repair_factory(1, contexts.clone()));
    let first = wait(&rig, &mut actor, 1);
    let db = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_repair BEFORE INSERT ON orch_events BEGIN SELECT RAISE(FAIL,'repair commit outage'); END;").unwrap();
    for _ in 0..3 {
        assert!(actor.tick().is_err());
    }
    assert_eq!(contexts.lock().unwrap().len(), 1);
    assert_eq!(rig.snapshot().runs, std::slice::from_ref(&first));
    db.execute_batch("DROP TRIGGER deny_repair").unwrap();
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    assert_eq!(contexts.lock().unwrap().len(), 2);
    assert_eq!(
        rig.snapshot().runs.iter().find(|r| r.id == first.id),
        Some(&first)
    );
    actor.shutdown();
}

#[test]
fn cancellation_and_changed_limits_or_revision_prevent_automatic_repair() {
    for reason in [
        "cancel",
        "attempts",
        "starts",
        "revision",
        "binding",
        "missing_exec",
    ] {
        let rig = Rig::new(true, &["status", "--porcelain"]);
        let contexts = Arc::new(Mutex::new(vec![]));
        let mut actor = rig.actor(repair_factory(1, contexts.clone()));
        wait(&rig, &mut actor, 1);
        let snapshot = rig.snapshot();
        let mut mission = snapshot.mission;
        let mut entities = vec![];
        match reason {
            "cancel" => control(&rig, "cancel"),
            "attempts" => mission.policy.max_attempts_per_task = 1,
            "starts" => mission.policy.max_automatic_starts = 1,
            "revision" => mission.plan_revision += 1,
            "binding" => mission.policy.allowed_binding_ids.clear(),
            "missing_exec" => {
                let mut run = snapshot.runs[0].clone();
                run.exec_id = Some(Id::generate());
                entities.push(Entity::Run(Box::new(run)));
            }
            _ => unreachable!(),
        }
        if reason != "cancel" {
            save(&rig, mission, entities);
        }
        for _ in 0..5 {
            actor.tick().unwrap();
        }
        let after = rig.snapshot();
        assert_eq!(contexts.lock().unwrap().len(), 1, "{reason}");
        assert_eq!(after.runs.len(), 1, "{reason}");
        if reason == "cancel" {
            assert_eq!(after.mission.state, MissionState::Cancelled);
        } else if reason == "missing_exec" {
            assert_eq!(after.tasks[0].state, TaskState::Blocked);
        } else {
            assert_eq!(after.tasks[0].state, TaskState::Failed, "{reason}");
            assert!(after.decisions.iter().any(is_recovery), "{reason}");
        }
        actor.shutdown();
    }
}

#[test]
fn a_generic_result_invalid_error_is_not_completed_answer_evidence() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let mut actor = rig.actor(Arc::new(|_| {
        Ok(scripted(FakeScript {
            steps: vec![FakeStep::Fail {
                code: "RESULT_INVALID".into(),
                message: "local preparation, malformed protocol or unsupported output".into(),
            }],
            ..Default::default()
        }))
    }));
    rig.tick_until(&mut actor, |s| s.decisions.iter().any(is_recovery));
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().runs.len(), 1);
    assert!(rig.snapshot().runs[0].retry_evidence.is_none());
    actor.shutdown();
}

#[test]
fn invalid_graph_can_be_corrected_but_outside_policy_paths_cannot() {
    for outside_policy in [false, true] {
        let rig = Rig::new(true, &["status", "--porcelain"]);
        let mut actor = rig.actor(Arc::new(move |run| {
            let ctx = context(run);
            let spec: ProviderTaskSpec = serde_json::from_value(json!({"local_key":"writer","title":"Write","kind":"implement","role":"builder","required":true,"parent_key":null,"depends_on_keys":if outside_policy { vec![] } else { vec!["writer"] },"objective_text":"Write a file","requirement_ids":[ctx["requirements"][0]["id"]],"input_artifact_ids":[],"allowed_paths":if outside_policy {vec!["../outside"]} else {vec!["api.txt"]},"expected_outputs":["patch"],"verification_ids":[],"specialty":null,"binding_id":null,"replacement_of":null})).unwrap();
            Ok(scripted(script(ProviderResult::Plan { based_on_plan_revision: 0, tasks: vec![spec], retire_task_ids: vec![], rationale_text: "test rejection".into() })))
        }));
        if outside_policy {
            rig.tick_until(&mut actor, |s| s.decisions.iter().any(is_recovery));
            assert_eq!(
                rig.snapshot().runs[0].failure_code,
                Some(MissionErrorCode::PolicyDenied)
            );
            assert!(rig.snapshot().runs[0].retry_evidence.is_none());
        } else {
            let run = wait(&rig, &mut actor, 1);
            assert_eq!(run.failure_code, Some(MissionErrorCode::ResultInvalid));
            assert!(body(&rig, run.result_ref.as_ref().unwrap()).contains("graph:"));
            assert!(matches!(
                run.retry_evidence,
                Some(RetryEvidence::PlanFormatRejected { .. })
            ));
        }
        assert_eq!(rig.snapshot().tasks.len(), 1);
        assert_eq!(rig.snapshot().mission.plan_revision, 0);
        actor.shutdown();
    }
}

#[test]
fn unowned_staging_evidence_never_reaches_a_repair_provider() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let contexts = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(repair_factory(1, contexts.clone()));
    wait(&rig, &mut actor, 1);
    let snapshot = rig.snapshot();
    let mut run = snapshot.runs[0].clone();
    let bytes = b"unowned private diagnostic";
    let upload = rpc(
        &rig.service,
        &rig.conn,
        "artifact.begin",
        json!({"request_id":Id::generate(),"mission_id":null,"media_type":"text/plain","bytes":bytes.len().to_string(),"sha256":format!("{:x}",Sha256::digest(bytes))}),
    );
    rpc(
        &rig.service,
        &rig.conn,
        "artifact.write",
        json!({"upload_id":upload["upload_id"],"offset":"0","data_b64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
    );
    let foreign = serde_json::from_value(rpc(
        &rig.service,
        &rig.conn,
        "artifact.commit",
        json!({"upload_id":upload["upload_id"]}),
    ))
    .unwrap();
    run.result_ref = Some(foreign);
    save(&rig, snapshot.mission, vec![Entity::Run(Box::new(run))]);
    rig.tick_until(&mut actor, |s| s.decisions.iter().any(is_recovery));
    assert_eq!(contexts.lock().unwrap().len(), 1);
    let after = rig.snapshot();
    let last = after.runs.iter().max_by_key(|r| r.attempt).unwrap();
    assert_eq!(last.attempt, 2);
    assert!(last.retry_evidence.is_none());
    assert_eq!(last.state, RunState::Failed);
    assert!(!body(&rig, last.result_ref.as_ref().unwrap()).contains("unowned private diagnostic"));
    actor.shutdown();
}

#[test]
fn correction_context_survives_an_unsubmitted_transient_attempt() {
    use iyagi_termd_lib::agent_runtime::opencode::runtime::OpenCodeRuntimeAdapter;
    let mut rig = Rig::new(true, &["status", "--porcelain"]);
    let time = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let wall = time.clone();
    let instant = Instant::now();
    rig.service = Arc::new(
        MissionService::new(
            rig.storage.clone(),
            ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
        )
        .with_wall_clock_millis(move || wall.load(Ordering::SeqCst))
        .with_monotonic_clock(move || instant),
    );
    let calls = AtomicUsize::new(0);
    let contexts = Arc::new(Mutex::new(vec![]));
    let seen = contexts.clone();
    let normal = factory(Arc::new(Mutex::new(vec![])));
    let mut actor = rig.actor(Arc::new(move |run| {
        let ctx = context(run);
        if ctx["task_kind"] == "plan" {
            seen.lock().unwrap().push(ctx);
            match calls.fetch_add(1, Ordering::SeqCst) {
                0 => return Ok(scripted(script(wrong_kind()))),
                1 => {
                    return Ok(OpenCodeRuntimeAdapter::with_factory(Arc::new(|_, _| {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "unsubmitted fixture readiness timeout",
                        ))
                    })))
                }
                _ => {}
            }
        }
        normal(run)
    }));
    rig.tick_until(&mut actor, |s| {
        s.tasks[0].blocked_code.as_deref() == Some("transient_retry")
    });
    let waiting = rig.snapshot();
    assert_eq!(waiting.tasks[0].attempt_count, 2);
    time.store(
        waiting.tasks[0]
            .dispatch_after_unix_ms
            .as_ref()
            .unwrap()
            .get(),
        Ordering::SeqCst,
    );
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let contexts = contexts.lock().unwrap();
    assert_eq!(contexts.len(), 3);
    assert!(!contexts[1]["plan_repair"].is_null());
    assert_eq!(contexts[1]["plan_repair"], contexts[2]["plan_repair"]);
    actor.shutdown();
}

#[test]
fn oversized_answer_or_combined_context_stops_before_another_provider_call() {
    for oversized_answer in [false, true] {
        let rig = Rig::new(true, &["status", "--porcelain"]);
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let limit = term_contracts::mission::validation::MissionLimits::load().max_context_bytes;
        let mut actor = rig.actor(Arc::new(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(scripted(script(ProviderResult::Patch {
                report_text: "x".repeat(if oversized_answer {
                    limit + 1
                } else {
                    limit - 100
                }),
                verification_claims: vec![],
            })))
        }));
        rig.tick_until(&mut actor, |s| s.decisions.iter().any(is_recovery));
        let after = rig.snapshot();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(after.mission.plan_revision, 0);
        assert_eq!(after.tasks.len(), 1);
        let first = after.runs.iter().min_by_key(|r| r.attempt).unwrap();
        let last = after.runs.iter().max_by_key(|r| r.attempt).unwrap();
        if oversized_answer {
            assert_eq!(after.runs.len(), 1);
            assert!(first.retry_evidence.is_none());
            assert_eq!(first.failure_code, Some(MissionErrorCode::ResultInvalid));
        } else {
            assert_eq!(after.runs.len(), 2);
            assert!(first.retry_evidence.is_some());
            assert_eq!(last.failure_code, Some(MissionErrorCode::ContextTooLarge));
            assert!(last.retry_evidence.is_none());
        }
        actor.shutdown();
    }
}

#[test]
fn corrected_plan_still_requires_the_users_configured_adoption_decision() {
    let rig = Rig::new(false, &["status", "--porcelain"]);
    let contexts = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(repair_factory(1, contexts.clone()));
    rig.tick_until(&mut actor, |s| {
        s.decisions
            .iter()
            .any(|d| d.state == DecisionState::Open && d.kind == DecisionKind::Plan)
    });
    let before = rig.snapshot();
    assert_eq!(contexts.lock().unwrap().len(), 2);
    assert_eq!(before.runs.len(), 2);
    assert_eq!(before.tasks.len(), 1);
    assert_eq!(before.mission.plan_revision, 0);
    assert!(before.candidates.is_empty());
    assert!(before
        .decisions
        .iter()
        .any(|d| d.state == DecisionState::Open && d.kind == DecisionKind::Plan && d.blocking));
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
    actor.shutdown();
}
