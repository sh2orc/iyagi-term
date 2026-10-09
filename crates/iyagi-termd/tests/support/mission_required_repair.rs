use super::*;
use iyagi_termd_lib::agent_runtime::{CancelReceipt, DeliveryReceipt, EventStream, RunProbe};
use std::sync::atomic::{AtomicBool, Ordering};
use term_contracts::mission::MissionErrorCode;

struct HeldClose {
    inner: Arc<dyn AgentAdapter>,
    release: Arc<AtomicBool>,
}
impl AgentAdapter for HeldClose {
    fn name(&self) -> &'static str {
        "held-cleanup"
    }
    fn start(&self, run: RunStart) -> std::io::Result<()> {
        self.inner.start(run)
    }
    fn send_message(&self, id: &Id, text: &str) -> DeliveryReceipt {
        self.inner.send_message(id, text)
    }
    fn answer(&self, id: &Id, request: &str, text: &str) -> DeliveryReceipt {
        self.inner.answer(id, request, text)
    }
    fn interrupt(&self, id: &Id) -> CancelReceipt {
        self.inner.interrupt(id)
    }
    fn inspect(&self, id: &Id) -> RunProbe {
        self.inner.inspect(id)
    }
    fn subscribe(&self) -> EventStream {
        self.inner.subscribe()
    }
    fn close(&self, id: &Id) -> CancelReceipt {
        if self.release.load(Ordering::SeqCst) {
            self.inner.close(id)
        } else {
            CancelReceipt::Accepted
        }
    }
}
fn hold_independent(inner: AdapterFactory, release: Arc<AtomicBool>) -> AdapterFactory {
    Arc::new(move |run| {
        let adapter = inner(run)?;
        if context(run)["task_contract"]["allowed_paths"][0] == "ui.txt" {
            Ok(Arc::new(HeldClose {
                inner: adapter,
                release: release.clone(),
            }))
        } else {
            Ok(adapter)
        }
    })
}

fn save(rig: &Rig, mission: Mission, entities: Vec<Entity>) {
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.required_repair",
        "change",
        MissionEventType::Changed,
        entities,
    )
    .unwrap();
}
fn policy(rig: &Rig, attempts: u32, cycles: u32) {
    let mut mission = rig.snapshot().mission;
    mission.policy.max_attempts_per_task = attempts;
    mission.policy.max_repair_cycles = cycles;
    save(rig, mission, vec![]);
}
fn failure() -> Arc<dyn AgentAdapter> {
    scripted(FakeScript {
        steps: vec![
            FakeStep::Started {
                session_id: Some("failed-builder".into()),
                turn_id: Some("failed-turn".into()),
            },
            FakeStep::Fail {
                code: "PROVIDER_UNAVAILABLE".into(),
                message: "compiler failure marker: no acceptable patch was produced".into(),
            },
        ],
        ..Default::default()
    })
}
fn replacement(ctx: &Value) -> ProviderResult {
    let mut tasks = Vec::new();
    let mut retired = Vec::new();
    for (index, failed) in ctx["failure_repair"]["failed_tasks"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let old = &failed["task"];
        retired.push(serde_json::from_value(old["id"].clone()).unwrap());
        tasks.push(serde_json::from_value(json!({"local_key":format!("replacement{index}"),"title":"Repair the failed implementation","kind":old["kind"],"role":old["role"],"required":true,"parent_key":null,"depends_on_keys":[],"objective_text":"Use the retained diagnostic and produce a corrected implementation.","requirement_ids":old["contract"]["requirement_ids"],"input_artifact_ids":[],"allowed_paths":old["contract"]["allowed_paths"],"expected_outputs":old["contract"]["expected_outputs"],"verification_ids":old["contract"]["verification_ids"],"specialty":null,"binding_id":old["binding_id"],"replacement_of":old["id"]})).unwrap());
    }
    ProviderResult::Plan { based_on_plan_revision: ctx["plan_revision"].as_u64().unwrap() as u32, tasks, retire_task_ids: retired, rationale_text: "Diagnosed the failed local implementation from retained evidence; replace it while preserving completed independent work.".into() }
}
fn repairing(always_fail: bool, seen: Arc<Mutex<Vec<Value>>>) -> AdapterFactory {
    let normal = factory(Arc::new(Mutex::new(vec![])));
    Arc::new(move |run| {
        let ctx = context(run);
        seen.lock().unwrap().push(ctx.clone());
        if !ctx["failure_repair"].is_null() {
            assert_eq!(
                run.workspace_access,
                iyagi_termd_lib::agent_runtime::WorkspaceAccess::ReadOnly
            );
            return Ok(scripted(script(replacement(&ctx))));
        }
        if ctx["task_kind"] == "implement" && ctx["task_contract"]["allowed_paths"][0] == "api.txt"
        {
            let current = ctx["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["id"] == ctx["task_id"])
                .unwrap();
            if always_fail || current["replacement_of"].is_null() {
                return Ok(failure());
            }
        }
        normal(run)
    })
}
fn failed_source(rig: &Rig) -> Run {
    rig.snapshot()
        .runs
        .into_iter()
        .find(|r| {
            r.state == RunState::Failed
                && r.provider_session_id.as_deref() == Some("failed-builder")
        })
        .unwrap()
}

#[test]
fn exhausted_worker_gets_one_lead_plan_with_diagnostic_and_a_verified_replacement() {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    policy(&rig, 1, 3);
    let seen = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(repairing(false, seen.clone()));
    rig.tick_until(&mut actor, |s| {
        s.tasks.iter().any(|t| !t.failure_repair_run_ids.is_empty())
    });
    let old = failed_source(&rig);
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let after = rig.snapshot();
    assert_eq!(after.runs.iter().find(|r| r.id == old.id), Some(&old));
    assert_eq!(
        after
            .tasks
            .iter()
            .find(|t| t.id == old.task_id)
            .unwrap()
            .state,
        TaskState::Superseded
    );
    assert_eq!(
        after
            .tasks
            .iter()
            .filter(|t| !t.failure_repair_run_ids.is_empty())
            .count(),
        1
    );
    let repaired = after
        .tasks
        .iter()
        .find(|t| t.replacement_of.as_ref() == Some(&old.task_id))
        .unwrap();
    assert_eq!(repaired.state, TaskState::Succeeded);
    assert_eq!(repaired.repair_cycle, 1);
    assert_eq!(repaired.attempt_count, 1);
    let new_run = after
        .runs
        .iter()
        .find(|r| r.task_id == repaired.id)
        .unwrap();
    assert_ne!(old.workspace_id, new_run.workspace_id);
    let contexts = seen.lock().unwrap();
    let repairs: Vec<_> = contexts
        .iter()
        .filter(|c| !c["failure_repair"].is_null())
        .collect();
    assert_eq!(repairs.len(), 1);
    assert_eq!(
        repairs[0]["failure_repair"]["failed_tasks"][0]["run_id"],
        old.id.as_str()
    );
    assert!(
        repairs[0]["failure_repair"]["failed_tasks"][0]["diagnostic"]
            .as_str()
            .unwrap()
            .contains("compiler failure marker")
    );
    assert!(!after
        .decisions
        .iter()
        .any(|d| d.state == DecisionState::Open && d.kind == DecisionKind::Recovery));
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
    actor.shutdown();
}

#[test]
fn exhausted_repair_cycles_end_the_mission_and_preserve_all_failed_runs() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    policy(&rig, 1, 1);
    let seen = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(repairing(true, seen.clone()));
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Stopping);
    let stopping = rig.snapshot();
    assert_eq!(
        stopping
            .tasks
            .iter()
            .filter(|t| !t.failure_repair_run_ids.is_empty())
            .count(),
        1
    );
    let failures: Vec<_> = stopping
        .runs
        .iter()
        .filter(|r| r.state == RunState::Failed)
        .cloned()
        .collect();
    assert_eq!(failures.len(), 2);
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Failed);
    let after = rig.snapshot();
    assert_eq!(
        after.mission.failure_code,
        Some(MissionErrorCode::ProviderUnavailable)
    );
    assert_eq!(after.mission.open_decision_count, 0);
    assert!(!after.runs.iter().any(|r| r.holds_execution_slot()));
    for failed in failures {
        assert_eq!(after.runs.iter().find(|r| r.id == failed.id), Some(&failed));
    }
    let before = seen.lock().unwrap().len();
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(seen.lock().unwrap().len(), before);
    actor.shutdown();
}

#[test]
fn repair_creation_is_atomic_and_survives_a_store_outage_and_service_restart() {
    let mut rig = Rig::new(true, &["status", "--porcelain"]);
    policy(&rig, 1, 3);
    let seen = Arc::new(Mutex::new(vec![]));
    let factory = repairing(false, seen.clone());
    let mut actor = rig.actor(factory.clone());
    let db = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_repair BEFORE INSERT ON orch_tasks WHEN json_array_length(NEW.document_json,'$.failure_repair_run_ids') > 0 BEGIN SELECT RAISE(FAIL,'repair store outage'); END;").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Err(e) = actor.tick() {
            assert_eq!(e.code, MissionErrorCode::StorageUnavailable);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let failed = failed_source(&rig);
    assert!(!rig
        .snapshot()
        .tasks
        .iter()
        .any(|t| !t.failure_repair_run_ids.is_empty()));
    assert!(!seen
        .lock()
        .unwrap()
        .iter()
        .any(|c| !c["failure_repair"].is_null()));
    // Keep dispatch off while the pending worker completion and durable repair
    // request settle, so restart has no live provider to turn Unknown.
    actor.set_dispatch_permitted(false);
    db.execute_batch("DROP TRIGGER deny_repair").unwrap();
    rig.tick_until(&mut actor, |s| {
        s.tasks.iter().any(|t| !t.failure_repair_run_ids.is_empty())
            && !s.runs.iter().any(|r| r.holds_execution_slot())
    });
    actor.shutdown();
    let before = rig.snapshot();
    rig.service = Arc::new(MissionService::new(
        rig.storage.clone(),
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
    ));
    rig.service.recover_on_startup().unwrap();
    let mut actor = rig.actor(factory);
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    assert_eq!(
        rig.snapshot()
            .tasks
            .iter()
            .filter(|t| !t.failure_repair_run_ids.is_empty())
            .count(),
        1
    );
    assert_eq!(
        rig.snapshot().runs.iter().find(|r| r.id == failed.id),
        Some(&failed)
    );
    assert_eq!(
        before
            .tasks
            .iter()
            .filter(|t| !t.failure_repair_run_ids.is_empty())
            .count(),
        1
    );
    actor.shutdown();
}

#[test]
fn cancelling_a_pending_repair_does_not_recreate_it() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    policy(&rig, 3, 3);
    let seen = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(repairing(false, seen.clone()));
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::Failed)
    });
    actor.set_dispatch_permitted(false);
    policy(&rig, 1, 3);
    rig.tick_until(&mut actor, |s| {
        s.tasks.iter().any(|t| !t.failure_repair_run_ids.is_empty())
    });
    let repair = rig
        .snapshot()
        .tasks
        .into_iter()
        .find(|t| !t.failure_repair_run_ids.is_empty())
        .unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"task_id":repair.id,"action":"cancel","binding_id":null}),
    );
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    let after = rig.snapshot();
    assert_eq!(
        after
            .tasks
            .iter()
            .filter(|t| !t.failure_repair_run_ids.is_empty())
            .count(),
        1
    );
    assert_eq!(
        after
            .tasks
            .iter()
            .find(|t| t.id == repair.id)
            .unwrap()
            .state,
        TaskState::Cancelled
    );
    assert!(after
        .decisions
        .iter()
        .any(|d| d.state == DecisionState::Open
            && d.options.iter().any(|o| o.id == "stop_failed_mission")));
    assert!(!seen
        .lock()
        .unwrap()
        .iter()
        .any(|c| !c["failure_repair"].is_null()));
    actor.shutdown();
}

#[test]
fn corrected_proposal_cannot_leave_a_failed_required_source_behind() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    policy(&rig, 1, 3);
    let normal = repairing(false, Arc::new(Mutex::new(vec![])));
    let mut actor = rig.actor(Arc::new(move |run| {
        let ctx = context(run);
        if !ctx["failure_repair"].is_null() {
            let mut result = replacement(&ctx);
            if let ProviderResult::Plan { tasks, .. } = &mut result {
                tasks[0].replacement_of = None;
            }
            return Ok(scripted(script(result)));
        }
        normal(run)
    }));
    rig.tick_until(&mut actor, |s| {
        s.tasks
            .iter()
            .any(|t| !t.failure_repair_run_ids.is_empty() && t.state == TaskState::Failed)
    });
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.mission.plan_revision, 1);
    assert!(!snapshot.tasks.iter().any(|t| t.replacement_of.is_some()));
    assert!(snapshot
        .decisions
        .iter()
        .any(|d| d.state == DecisionState::Open && d.kind == DecisionKind::Recovery));
    actor.shutdown();
}

#[test]
fn lead_and_replacement_progress_while_an_independent_run_still_owns_its_workspace() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    policy(&rig, 1, 3);
    let release = Arc::new(AtomicBool::new(false));
    let mut actor = rig.actor(hold_independent(
        repairing(false, Arc::new(Mutex::new(vec![]))),
        release.clone(),
    ));
    rig.tick_until(&mut actor, |s| {
        s.tasks
            .iter()
            .any(|t| t.replacement_of.is_some() && t.state == TaskState::Succeeded)
    });
    let before = rig.snapshot();
    assert_eq!(before.mission.phase, Phase::Implementing);
    assert!(before.runs.iter().any(|r| r.holds_execution_slot()));
    assert!(before
        .tasks
        .iter()
        .any(|t| t.contract.allowed_paths == ["ui.txt"] && t.state == TaskState::Running));
    release.store(true, Ordering::SeqCst);
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    actor.shutdown();
}

#[test]
fn final_failure_waits_for_adapter_cleanup_and_missing_exec_evidence() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    policy(&rig, 1, 1);
    let release = Arc::new(AtomicBool::new(false));
    let mut actor = rig.actor(hold_independent(
        repairing(true, Arc::new(Mutex::new(vec![]))),
        release.clone(),
    ));
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Stopping);
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().mission.state, MissionState::Stopping);
    assert!(rig.snapshot().runs.iter().any(|r| r.holds_execution_slot()));
    let original = failed_source(&rig);
    let mut missing = original.clone();
    missing.exec_id = Some(Id::generate());
    save(
        &rig,
        rig.snapshot().mission,
        vec![Entity::Run(Box::new(missing))],
    );
    release.store(true, Ordering::SeqCst);
    rig.tick_until(&mut actor, |s| {
        !s.runs.iter().any(|r| r.holds_execution_slot())
    });
    assert_eq!(actor.live_count(), 0);
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(
        rig.snapshot().mission.state,
        MissionState::Stopping,
        "missing durable Exec proof cannot settle failure"
    );
    save(
        &rig,
        rig.snapshot().mission,
        vec![Entity::Run(Box::new(original.clone()))],
    );
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Failed);
    assert_eq!(
        rig.snapshot().runs.iter().find(|r| r.id == original.id),
        Some(&original)
    );
    actor.shutdown();
}

#[test]
fn optional_unexhausted_unconfirmed_and_paused_failures_do_not_start_a_repair() {
    for change in [
        "optional",
        "unexhausted",
        "missing_exec",
        "unknown_code",
        "unknown_state",
        "paused",
        "disabled_lead",
    ] {
        let rig = Rig::new(true, &["status", "--porcelain"]);
        let mut actor = rig.actor(repairing(false, Arc::new(Mutex::new(vec![]))));
        rig.tick_until(&mut actor, |s| {
            s.runs.iter().any(|r| r.state == RunState::Failed)
        });
        actor.set_dispatch_permitted(false);
        let snapshot = rig.snapshot();
        let mut mission = snapshot.mission;
        let mut run = failed_source(&rig);
        let mut task = snapshot
            .tasks
            .iter()
            .find(|t| t.id == run.task_id)
            .unwrap()
            .clone();
        mission.policy.max_attempts_per_task = 1;
        match change {
            "optional" => task.required = false,
            "unexhausted" => mission.policy.max_attempts_per_task = 3,
            "missing_exec" => run.exec_id = Some(Id::generate()),
            "unknown_code" => run.failure_code = Some(MissionErrorCode::OutcomeUnknown),
            "unknown_state" => run.state = RunState::Unknown,
            "paused" => mission.state = MissionState::Paused,
            "disabled_lead" => mission.policy.allowed_binding_ids.clear(),
            _ => unreachable!(),
        }
        save(
            &rig,
            mission,
            vec![Entity::Task(Box::new(task)), Entity::Run(Box::new(run))],
        );
        for _ in 0..5 {
            actor.tick().unwrap();
        }
        assert!(
            !rig.snapshot()
                .tasks
                .iter()
                .any(|t| !t.failure_repair_run_ids.is_empty()),
            "{change}"
        );
        assert!(
            !matches!(
                rig.snapshot().mission.state,
                MissionState::Stopping | MissionState::Failed
            ),
            "{change}"
        );
        actor.shutdown();
    }
}

#[test]
fn original_failure_cannot_race_its_lead_repair_with_a_separate_retry() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    policy(&rig, 3, 3);
    let mut actor = rig.actor(repairing(false, Arc::new(Mutex::new(vec![]))));
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::Failed)
    });
    actor.set_dispatch_permitted(false);
    policy(&rig, 1, 3);
    rig.tick_until(&mut actor, |s| {
        s.tasks.iter().any(|t| !t.failure_repair_run_ids.is_empty())
    });
    let source = failed_source(&rig);
    // Restore attempts so the rejection tests ownership, not just the cap.
    policy(&rig, 3, 3);
    let before = rig.snapshot();
    let error = rig.service.handle(&rig.conn, "mission.task.control", &json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":before.mission.revision,"task_id":source.task_id,"action":"retry","binding_id":null})).err().unwrap();
    assert_eq!(error.code, MissionErrorCode::InvalidState);
    assert!(error.message.contains("Lead repair plan"));
    assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
    assert_eq!(
        rig.snapshot().runs.iter().find(|r| r.id == source.id),
        Some(&source)
    );
    actor.shutdown();
}

fn rejected_candidate(review: bool, cycles: u32) -> (Rig, MissionActor) {
    let rig = if review {
        Rig::new(true, &["status", "--porcelain"])
    } else {
        Rig::new(true, &["ls-files", "--error-unmatch", "missing.txt"])
    };
    policy(&rig, 3, cycles);
    let normal = factory(Arc::new(Mutex::new(vec![])));
    let actor = rig.actor(Arc::new(move |run| {
                let ctx = context(run);
                if ctx["task_kind"] == "review" && review {
                    return Ok(scripted(script(ProviderResult::Review {
                        candidate_id: serde_json::from_value(ctx["candidate"]["id"].clone()).unwrap(),
                        report_text: "Required behavior is still incorrect.".into(),
                        findings: [FindingSeverity::Major, FindingSeverity::Blocking].into_iter().map(|severity| ProviderFindingDraft { severity, path: Some("api.txt".into()), line: Some(1), evidence_text: "Retained incorrect implementation".into(), requirement_id: None }).collect(),
                    })));
                }
                if ctx["task_kind"] == "plan" && ctx["plan_revision"] == 1 {
                    let spec = serde_json::from_value(json!({"local_key":"repair_api","title":"Repair API","kind":"implement","role":"builder","required":true,"parent_key":null,"depends_on_keys":[],"objective_text":"Try correcting the retained failure","requirement_ids":[ctx["requirements"][0]["id"]],"input_artifact_ids":[],"allowed_paths":["api.txt"],"expected_outputs":["patch"],"verification_ids":[],"specialty":null,"binding_id":null,"replacement_of":null})).unwrap();
                    let retired = ctx["tasks"].as_array().unwrap().iter().filter(|t| t["kind"] == "verify" && t["state"] == "failed").map(|t| serde_json::from_value(t["id"].clone()).unwrap()).collect();
                    return Ok(scripted(script(ProviderResult::Plan { based_on_plan_revision: 1, tasks: vec![spec], retire_task_ids: retired, rationale_text: "One bounded repair of the rejected candidate.".into() })));
                }
                if ctx["task_kind"] == "implement" && ctx["plan_revision"] == 2 {
                    std::fs::write(run.workspace.as_ref().unwrap().join("api.txt"), "still insufficient repair\n").unwrap();
                    return Ok(scripted(script(ProviderResult::Patch { report_text: "Changed the implementation; validation decides whether it works.".into(), verification_claims: vec![] })));
                }
                normal(run)
            }));
    (rig, actor)
}

#[test]
fn mandatory_verification_exhaustion_cleans_up_before_final_failure() {
    for cycles in [0, 1] {
        let (rig, mut actor) = rejected_candidate(false, cycles);
        rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Failed);
        let after = rig.snapshot();
        assert_eq!(
            after.mission.failure_code,
            Some(MissionErrorCode::PlanLimit)
        );
        assert_eq!(after.verifications.len(), (cycles + 1) as usize);
        assert!(!after.runs.iter().any(|r| r.holds_execution_slot()));
        assert_eq!(after.mission.open_decision_count, 0);
        assert_eq!(
            after
                .tasks
                .iter()
                .filter(|t| t.kind == TaskKind::Plan)
                .count(),
            (cycles + 1) as usize
        );
        assert!(after
            .verifications
            .iter()
            .all(|v| v.status == VerificationStatus::Failed));
        actor.shutdown();
    }
}

fn review_budget(rig: &Rig, actor: &mut MissionActor) -> Decision {
    rig.tick_until(actor, |s| {
        s.decisions.iter().any(|d| {
            d.state == DecisionState::Open && d.options.iter().any(|o| o.id == "stop_review_repair")
        })
    });
    rig.snapshot()
        .decisions
        .into_iter()
        .find(|d| {
            d.state == DecisionState::Open && d.options.iter().any(|o| o.id == "stop_review_repair")
        })
        .unwrap()
}

fn dismiss(rig: &Rig, finding: &Finding) {
    let store = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
    let reason = workflow::store_artifact(&store, &rig.id, "text/plain", b"Inspected the implementation and verification evidence; the existing behavior is required by the public contract.").unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.finding.resolve",
        json!({
            "request_id": Id::generate(), "mission_id": rig.id, "expected_revision": rig.snapshot().mission.revision,
            "finding_id": finding.id, "resolution": "dismissed", "reason_ref": reason
        }),
    );
}

#[test]
fn exhausted_review_waits_for_reasons_and_last_dismissal_allows_acceptance() {
    for cycles in [0, 1] {
        let (rig, mut actor) = rejected_candidate(true, cycles);
        let decision = review_budget(&rig, &mut actor);
        let before = rig.snapshot();
        assert_eq!(before.mission.state, MissionState::Running);
        assert_eq!(before.mission.phase, Phase::Reviewing);
        assert_eq!(before.verifications.len(), (cycles + 1) as usize);
        assert_eq!(
            before
                .tasks
                .iter()
                .filter(|t| t.kind == TaskKind::Plan)
                .count(),
            (cycles + 1) as usize
        );
        for _ in 0..5 {
            actor.tick().unwrap();
        }
        assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
        let findings: Vec<_> = before
            .findings
            .iter()
            .filter(|f| Some(&f.candidate_id) == before.mission.candidate_id.as_ref())
            .collect();
        assert_eq!(findings.len(), 2);
        dismiss(&rig, findings[0]);
        assert_eq!(
            rig.snapshot()
                .decisions
                .iter()
                .find(|d| d.id == decision.id)
                .unwrap()
                .state,
            DecisionState::Open
        );
        dismiss(&rig, findings[1]);
        assert_eq!(
            rig.snapshot()
                .decisions
                .iter()
                .find(|d| d.id == decision.id)
                .unwrap()
                .state,
            DecisionState::Obsolete
        );
        rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
        let after = rig.snapshot();
        assert_eq!(after.mission.state, MissionState::Running);
        assert_eq!(after.mission.open_decision_count, 0);
        assert_eq!(after.mission.candidate_id, before.mission.candidate_id);
        assert_eq!(after.runs, before.runs);
        actor.shutdown();
    }
}

#[test]
fn review_dismissal_preserves_unrelated_and_old_candidate_budget_decisions() {
    let (rig, mut actor) = rejected_candidate(true, 0);
    let original = review_budget(&rig, &mut actor);
    let mut unrelated = original.clone();
    unrelated.id = Id::generate();
    unrelated.options[0].id = "stop_cost_mission".into();
    let mut stale = original.clone();
    stale.id = Id::generate();
    stale.candidate_id = Some(Id::generate());
    let mut mission = rig.snapshot().mission;
    mission.open_decision_count += 2;
    save(
        &rig,
        mission,
        vec![
            Entity::Decision(Box::new(unrelated.clone())),
            Entity::Decision(Box::new(stale.clone())),
        ],
    );
    for finding in rig.snapshot().findings {
        dismiss(&rig, &finding);
    }
    let after = rig.snapshot();
    assert_eq!(after.mission.open_decision_count, 2);
    for id in [unrelated.id, stale.id] {
        assert_eq!(
            after.decisions.iter().find(|d| d.id == id).unwrap().state,
            DecisionState::Open
        );
    }
    assert_eq!(
        after
            .decisions
            .iter()
            .find(|d| d.id == original.id)
            .unwrap()
            .state,
        DecisionState::Obsolete
    );
    actor.shutdown();
}

#[test]
fn exhausted_review_can_be_stopped_explicitly() {
    let (rig, mut actor) = rejected_candidate(true, 0);
    let decision = review_budget(&rig, &mut actor);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        json!({
            "request_id": Id::generate(), "mission_id": rig.id, "expected_revision": rig.snapshot().mission.revision,
            "decision_id": decision.id, "option_id": "stop_review_repair", "answer_ref": null
        }),
    );
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Cancelled);
    assert!(rig
        .snapshot()
        .findings
        .iter()
        .all(|f| f.resolution == FindingResolution::Open));
    assert!(!rig.snapshot().runs.iter().any(|r| r.holds_execution_slot()));
    actor.shutdown();
}

#[test]
fn review_budget_expansion_allows_exactly_one_more_repair_cycle() {
    let (rig, mut actor) = rejected_candidate(true, 0);
    let decision = review_budget(&rig, &mut actor);
    let before = rig.snapshot();
    let mut expanded = before.mission.policy.clone();
    expanded.max_repair_cycles = 1;
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({
            "request_id": Id::generate(), "mission_id": rig.id, "expected_revision": before.mission.revision,
            "policy": expanded, "role_bindings": before.mission.role_bindings
        }),
    );
    assert_eq!(
        rig.snapshot()
            .decisions
            .iter()
            .find(|d| d.id == decision.id)
            .unwrap()
            .state,
        DecisionState::Obsolete
    );
    let next = review_budget(&rig, &mut actor);
    let after = rig.snapshot();
    assert_ne!(next.id, decision.id);
    assert_ne!(after.mission.candidate_id, before.mission.candidate_id);
    assert_eq!(after.mission.state, MissionState::Running);
    assert_eq!(after.mission.open_decision_count, 1);
    assert_eq!(
        after
            .tasks
            .iter()
            .filter(|t| t.kind == TaskKind::Plan)
            .count(),
        2
    );
    assert_eq!(after.verifications.len(), 2);
    actor.shutdown();
}

#[test]
fn required_replacement_waits_for_manual_plan_adoption() {
    let rig = Rig::new(false, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    policy(&rig, 1, 1);
    let mut actor = rig.actor(repairing(false, Arc::new(Mutex::new(vec![]))));
    for revision in [0, 1] {
        rig.tick_until(&mut actor, |s| {
            s.decisions
                .iter()
                .any(|d| d.kind == DecisionKind::Plan && d.state == DecisionState::Open)
        });
        let before = rig.snapshot();
        assert_eq!(before.mission.plan_revision, revision);
        assert!(!before.tasks.iter().any(|t| t.replacement_of.is_some()));
        if revision == 1 {
            assert!(before
                .tasks
                .iter()
                .any(|t| !t.failure_repair_run_ids.is_empty() && t.state == TaskState::Succeeded));
            assert!(before
                .tasks
                .iter()
                .any(|t| t.required && t.state == TaskState::Failed));
        }
        for _ in 0..3 {
            actor.tick().unwrap();
        }
        assert_eq!(rig.snapshot().mission.plan_revision, revision);
        let proposal = before
            .decisions
            .iter()
            .find(|d| d.kind == DecisionKind::Plan && d.state == DecisionState::Open)
            .unwrap()
            .question_ref
            .clone();
        rpc(
            &rig.service,
            &rig.conn,
            "mission.plan.apply",
            json!({
                "request_id": Id::generate(), "mission_id": rig.id,
                "expected_revision": rig.snapshot().mission.revision, "proposal_ref": proposal
            }),
        );
    }
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    assert_eq!(
        rig.snapshot()
            .tasks
            .iter()
            .filter(|t| t.replacement_of.is_some() && t.state == TaskState::Succeeded)
            .count(),
        1
    );
    actor.shutdown();
}
