//! Mission usability contracts (W1b): quick missions without independent
//! review, user attestation of stuck runs, user-requested workspace cleanup,
//! and follow-up missions based on an accepted result.
use super::*;
use iyagi_termd_lib::agent_runtime::AdapterEvent;
use term_contracts::mission::{MissionErrorCode, MissionRpcError};
use term_contracts::workload::GroupRecoveryIdentity;

fn upload_goal(rig: &Rig) -> Value {
    let body = format!("usability goal {}", Id::generate());
    let body = body.as_bytes();
    let sha = format!("{:x}", Sha256::digest(body));
    let upload = rpc(
        &rig.service,
        &rig.conn,
        "artifact.begin",
        json!({"request_id":Id::generate(),"mission_id":null,"media_type":"text/plain","bytes":body.len().to_string(),"sha256":sha}),
    );
    rpc(
        &rig.service,
        &rig.conn,
        "artifact.write",
        json!({"upload_id":upload["upload_id"],"offset":"0","data_b64":base64::engine::general_purpose::STANDARD.encode(body)}),
    );
    rpc(
        &rig.service,
        &rig.conn,
        "artifact.commit",
        json!({"upload_id":upload["upload_id"]}),
    )
}

/// A draft in the rig's repository with the rig's binding for `roles`.
fn draft(
    rig: &Rig,
    base: &str,
    review: bool,
    roles: &[&str],
    follow_up_of: Option<&Id>,
) -> Result<Id, MissionRpcError> {
    draft_including(rig, base, review, roles, follow_up_of, false)
}

/// The same draft, optionally asking for the working tree to be the base.
/// The field is sent only when asked so the default request shape is the one
/// older clients already send.
fn draft_including(
    rig: &Rig,
    base: &str,
    review: bool,
    roles: &[&str],
    follow_up_of: Option<&Id>,
    include_uncommitted: bool,
) -> Result<Id, MissionRpcError> {
    let mission = rig.snapshot().mission;
    let mut policy = serde_json::to_value(&mission.policy).unwrap();
    policy["require_independent_review"] = json!(review);
    policy["allowed_roles"] = json!(roles);
    let binding = &mission.policy.allowed_binding_ids[0];
    let role_bindings: Vec<Value> = roles
        .iter()
        .map(|role| json!({"role":role,"primary_binding_id":binding,"fallback_binding_ids":[]}))
        .collect();
    let mut params = json!({"request_id":Id::generate(),"title":"Usability","repository_path":rig.repo.path(),
        "expected_base_oid":base,"goal_ref":upload_goal(rig),
        "requirements":[{"id":Id::generate(),"text":"A follow-up result exists","verification_ids":[],"human_check":false}],
        "policy":policy,"role_bindings":role_bindings,"follow_up_of":follow_up_of});
    if include_uncommitted {
        params["include_uncommitted"] = json!(true);
    }
    rig.service
        .handle(&rig.conn, "mission.create", &params)
        .map(|handled| serde_json::from_value(handled.result["mission_id"].clone()).unwrap())
}

fn start(rig: &Rig, id: &Id) -> Result<Value, MissionRpcError> {
    rig.service
        .handle(
            &rig.conn,
            "mission.control",
            &json!({"request_id":Id::generate(),"mission_id":id,"expected_revision":"1","action":"start"}),
        )
        .map(|handled| handled.result)
}

fn reason(error: &MissionRpcError) -> Option<&str> {
    error.details.reason_code.as_deref()
}

/// Drive the standard two-writer mission to user acceptance.
fn accepted_rig() -> Rig {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let mut actor = rig.actor(factory(Arc::new(Mutex::new(vec![]))));
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.accept",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,
            "candidate_id":snapshot.mission.candidate_id,"acknowledged_verification_ids":[snapshot.verifications[0].id],"human_requirement_ids":[]}),
    );
    actor.shutdown();
    assert_eq!(rig.snapshot().mission.state, MissionState::Completed);
    rig
}

// ---- B: quick missions -----------------------------------------------------

#[test]
fn start_requires_lead_and_builder_and_a_reviewer_only_with_independent_review() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let lead_only = draft(&rig, &rig.base, false, &["lead"], None).unwrap();
    assert_eq!(
        reason(&start(&rig, &lead_only).unwrap_err()),
        Some("builder_missing")
    );
    let reviewed = draft(&rig, &rig.base, true, &["lead", "builder"], None).unwrap();
    let error = start(&rig, &reviewed).unwrap_err();
    assert_eq!(error.code, MissionErrorCode::InvalidArgument);
    assert_eq!(reason(&error), Some("reviewer_missing"));
    let quick = draft(&rig, &rig.base, false, &["lead", "builder"], None).unwrap();
    assert_eq!(start(&rig, &quick).unwrap()["revision"], "2");
    let integrator_optional = draft(
        &rig,
        &rig.base,
        true,
        &["lead", "builder", "reviewer"],
        None,
    )
    .unwrap();
    start(&rig, &integrator_optional).unwrap();
}

#[test]
fn quick_mission_without_reviewer_skips_review_and_accepts() {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let before = rig.snapshot();
    let mut policy = before.mission.policy.clone();
    policy.require_independent_review = false;
    policy.allowed_roles = vec![Role::Lead, Role::Builder];
    let role_bindings: Vec<RoleBinding> = before
        .mission
        .role_bindings
        .iter()
        .filter(|r| matches!(r.role, Role::Lead | Role::Builder))
        .cloned()
        .collect();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":before.mission.revision,
            "policy":policy,"role_bindings":role_bindings}),
    );
    let seen = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(factory(seen.clone()));
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    assert!(snapshot.tasks.iter().all(|t| t.kind != TaskKind::Review));
    let kinds = seen.lock().unwrap().clone();
    assert!(kinds.iter().all(|(kind, _)| kind != "review"));
    assert_eq!(snapshot.verifications[0].status, VerificationStatus::Passed);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.accept",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,
            "candidate_id":snapshot.mission.candidate_id,"acknowledged_verification_ids":[snapshot.verifications[0].id],"human_requirement_ids":[]}),
    );
    assert_eq!(rig.snapshot().mission.state, MissionState::Completed);
    actor.shutdown();
}

#[test]
fn turning_independent_review_off_supersedes_only_unstarted_reviews_so_acceptance_proceeds() {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let before = rig.snapshot();
    // A review that was scheduled but never started: no Run, and no Reviewer
    // binding will exist after the update, so nothing could ever finish it.
    let mut review = before.tasks[0].clone();
    review.id = Id::generate();
    review.title = "Independent review".into();
    review.kind = TaskKind::Review;
    review.role = Some(Role::Reviewer);
    review.state = TaskState::Planned;
    review.binding_id = None;
    review.active_run_id = None;
    review.workspace_id = None;
    review.attempt_count = 0;
    review.blocked_code = None;
    review.ordinal = before.tasks.iter().map(|t| t.ordinal).max().unwrap_or(0) + 1;
    workflow::commit_upserts(
        &rig.service,
        before.mission.clone(),
        "test.fixture",
        "unstarted-review",
        MissionEventType::Changed,
        vec![Entity::Task(Box::new(review.clone()))],
    )
    .unwrap();
    let before = rig.snapshot();
    let mut policy = before.mission.policy.clone();
    policy.require_independent_review = false;
    policy.allowed_roles = vec![Role::Lead, Role::Builder];
    let role_bindings: Vec<RoleBinding> = before
        .mission
        .role_bindings
        .iter()
        .filter(|r| matches!(r.role, Role::Lead | Role::Builder))
        .cloned()
        .collect();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":before.mission.revision,
            "policy":policy,"role_bindings":role_bindings}),
    );
    let updated = rig.snapshot();
    let retired = updated.tasks.iter().find(|t| t.id == review.id).unwrap();
    assert_eq!(retired.state, TaskState::Superseded);
    for task in updated.tasks.iter().filter(|t| t.id != review.id) {
        let original = before.tasks.iter().find(|t| t.id == task.id).unwrap();
        assert_eq!(
            task.state, original.state,
            "only the unstarted review changes"
        );
    }

    let seen = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(factory(seen.clone()));
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let kinds = seen.lock().unwrap().clone();
    assert!(kinds.iter().all(|(kind, _)| kind != "review"));
    let snapshot = rig.snapshot();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.accept",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,
            "candidate_id":snapshot.mission.candidate_id,"acknowledged_verification_ids":[snapshot.verifications[0].id],"human_requirement_ids":[]}),
    );
    assert_eq!(rig.snapshot().mission.state, MissionState::Completed);
    actor.shutdown();
}

#[cfg(unix)]
#[test]
fn integration_conflict_without_an_integrator_offers_no_integrator_resolution() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let before = rig.snapshot();
    let role_bindings: Vec<RoleBinding> = before
        .mission
        .role_bindings
        .iter()
        .filter(|r| r.role != Role::Integrator)
        .cloned()
        .collect();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":before.mission.revision,
            "policy":before.mission.policy,"role_bindings":role_bindings}),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let supervisor = super::mission_verification_exec::supervisor(&rig, &rig.service);
    supervisor.refresh_recovery().unwrap();
    let factory: AdapterFactory = Arc::new(|run| {
        let ctx = context(run);
        let result = match ctx["task_kind"].as_str().unwrap() {
            "plan" => ProviderResult::Plan {
                based_on_plan_revision: 0,
                tasks: ["a", "b"].iter().map(|key| serde_json::from_value(json!({
                    "local_key":key,"title":key,"kind":"implement","role":"builder","required":true,
                    "parent_key":null,"depends_on_keys":[],"objective_text":"Update the shared file",
                    "requirement_ids":[ctx["requirements"][0]["id"]],"input_artifact_ids":[],
                    "allowed_paths":["shared.txt"],"expected_outputs":["patch"],"verification_ids":[],
                    "specialty":null,"binding_id":null,"replacement_of":null})).unwrap()).collect(),
                retire_task_ids: vec![], rationale_text: "Conflicting writers".into(),
            },
            "implement" => {
                std::fs::write(run.workspace.as_ref().unwrap().join("shared.txt"), ctx["task_id"].as_str().unwrap()).unwrap();
                ProviderResult::Patch { report_text: "Captured".into(), verification_claims: vec![] }
            }
            other => panic!("unexpected task {other}"),
        };
        Ok(scripted(script(result)))
    });
    let mut actor = rig
        .actor(factory)
        .with_deterministic_exec(supervisor.clone(), runtime.handle().clone());
    rig.tick_until(&mut actor, |s| {
        s.decisions
            .iter()
            .any(|d| d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)
    });
    for _ in 0..4 {
        actor.tick().unwrap();
    }
    let snapshot = rig.snapshot();
    let decision = snapshot
        .decisions
        .iter()
        .find(|d| d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)
        .unwrap();
    let options: Vec<&str> = decision.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(options, ["exclude_candidate", "stop_mission"]);
    let error = rig
        .service
        .handle(
            &rig.conn,
            "mission.decision.answer",
            &json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,
                "decision_id":decision.id,"option_id":"resolve_and_reintegrate","answer_ref":null}),
        )
        .err()
        .unwrap();
    assert_eq!(reason(&error), Some("option_invalid"));
    assert_eq!(rig.snapshot().mission.revision, snapshot.mission.revision);

    // A policy that does not allow the role at all is reported as the policy.
    let mut policy = snapshot.mission.policy.clone();
    policy
        .allowed_roles
        .retain(|role| *role != Role::Integrator);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,
            "policy":policy,"role_bindings":snapshot.mission.role_bindings}),
    );
    let denied = rig
        .service
        .handle(
            &rig.conn,
            "mission.decision.answer",
            &json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,
                "decision_id":decision.id,"option_id":"resolve_and_reintegrate","answer_ref":null}),
        )
        .err()
        .unwrap();
    assert_eq!(denied.code, MissionErrorCode::PolicyDenied);
    assert_eq!(reason(&denied), None);
    actor.shutdown();
}

// ---- C: attesting a stuck run ----------------------------------------------

/// A spawned Exec of this service whose adapter disconnected (Unknown run).
fn stuck_run() -> (Rig, ExecRecord) {
    stuck_run_with(false)
}

/// `recoverable`: the Exec also carries the durable native group identity a
/// later daemon's recovery reclaims (macOS guardian + observed tree).
fn stuck_run_with(recoverable: bool) -> (Rig, ExecRecord) {
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
    if recoverable {
        exec.group_identity = Some(GroupRecoveryIdentity::MacosGuardian {
            guardian: term_platform::current_process_identity().expect("test process identity"),
            endpoint: format!("owned-test-guardian:{}", exec.id),
        });
    }
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

fn attest(
    service: &MissionService,
    rig: &Rig,
    run_id: &Id,
    request_id: &Id,
) -> Result<Value, MissionRpcError> {
    service
        .handle(
            &rig.conn,
            "mission.run.attest_exited",
            &json!({"request_id":request_id,"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,
                "run_id":run_id,"attestation":"process_absent_confirmed"}),
        )
        .map(|handled| handled.result)
}

#[test]
fn attestation_releases_only_runs_no_daemon_can_observe_and_stays_distinguishable() {
    let (rig, exec) = stuck_run();
    let run = rig.snapshot().runs[0].clone();
    assert_eq!(run.state, RunState::Unknown);
    assert!(run.holds_execution_slot());
    // The daemon that owns the process group is still its authority.
    let owned = attest(&rig.service, &rig, &run.id, &Id::generate()).unwrap_err();
    assert_eq!(owned.code, MissionErrorCode::InvalidState);
    assert_eq!(reason(&owned), Some("attestation_not_applicable"));
    // Invalid statement keys never reach the state checks.
    let invalid = rig
        .service
        .handle(
            &rig.conn,
            "mission.run.attest_exited",
            &json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,
                "run_id":run.id,"attestation":"Process gone!"}),
        )
        .err()
        .unwrap();
    assert_eq!(invalid.code, MissionErrorCode::InvalidArgument);

    // After a restart the new daemon cannot observe the old group.
    let restarted = Arc::new(MissionService::new(
        rig.storage.clone(),
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
    ));
    #[cfg(unix)]
    let supervisor = {
        let supervisor = super::mission_verification_exec::supervisor(&rig, &restarted);
        assert_eq!(
            supervisor.refresh_recovery().unwrap(),
            1,
            "restored reservation"
        );
        supervisor
    };
    let before = rig.snapshot();
    let request_id = Id::generate();
    let result = attest(&restarted, &rig, &run.id, &request_id).unwrap();
    let replay = restarted
        .handle(
            &rig.conn,
            "mission.run.attest_exited",
            &json!({"request_id":request_id,"mission_id":rig.id,"expected_revision":before.mission.revision,
                "run_id":run.id,"attestation":"process_absent_confirmed"}),
        )
        .unwrap()
        .result;
    assert_eq!(replay, result, "request replay returns the first response");
    let after = rig.snapshot();
    let attested = after.runs.iter().find(|r| r.id == run.id).unwrap();
    assert_eq!(attested.state, RunState::Unknown, "outcome stays unknown");
    assert_eq!(
        attested.reconciliation_kind,
        Some(ReconciliationKind::UserAttested)
    );
    assert!(!attested.holds_execution_slot());
    let mut expected = run.clone();
    expected.reconciliation_ref = attested.reconciliation_ref.clone();
    expected.reconciliation_kind = attested.reconciliation_kind;
    assert_eq!(attested, &expected, "only the proof link changes");
    let closed = after.execs.iter().find(|e| e.id == exec.id).unwrap();
    assert_eq!(closed.state, ExecState::Exited);
    assert!(closed.ended_at.is_some());
    assert_eq!(closed.exit_code, None, "no exit status was observed");
    assert_eq!(after.workspaces[0].state, WorkspaceState::Quarantined);
    assert!(after.workspaces[0].writer_run_id.is_none());
    assert!(after.tasks[0].active_run_id.is_none());
    assert_eq!(
        after.tasks[0].blocked_code.as_deref(),
        Some("outcome_unknown_ended")
    );
    let proof: Value = serde_json::from_slice(
        &ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"))
            .read_mission_body(
                &rig.id,
                attested.reconciliation_ref.as_ref().unwrap(),
                512 * 1024,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(proof["kind"], "user_attested");
    assert_eq!(proof["attestation"], "process_absent_confirmed");
    assert_eq!(proof["exec_before"]["state"], "spawned");
    assert_eq!(proof["exec_after"]["state"], "exited");
    #[cfg(unix)]
    assert_eq!(
        supervisor.refresh_recovery().unwrap(),
        0,
        "reservation released"
    );

    // Nothing is attested twice; the automatic reconciliation offers the
    // usual explicit recovery for the attested run.
    let again = attest(&restarted, &rig, &run.id, &Id::generate()).unwrap_err();
    assert_eq!(reason(&again), Some("attestation_not_applicable"));
    restarted.dispatch_tick().unwrap();
    let decision = rig
        .snapshot()
        .decisions
        .into_iter()
        .find(|d| {
            d.state == DecisionState::Open
                && d.options.iter().any(|o| o.id == "stop_reconciled_mission")
        })
        .expect("recovery decision for the attested run");
    assert_eq!(decision.requesting_run_id.as_ref(), Some(&run.id));
    rpc(
        &restarted,
        &rig.conn,
        "mission.decision.answer",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,
            "decision_id":decision.id,"option_id":"retry_reconciled_task","answer_ref":null}),
    );
    assert_eq!(rig.snapshot().tasks[0].state, TaskState::Ready);
}

#[test]
fn attestation_refuses_an_older_group_native_recovery_can_still_observe() {
    let (rig, exec) = stuck_run_with(true);
    let run = rig.snapshot().runs[0].clone();
    assert_eq!(run.state, RunState::Unknown);
    assert!(run.holds_execution_slot());
    // A new daemon instance: the Exec is not its own, but its recovery can
    // reclaim the guardian-owned group, so only that observation may close it.
    let restarted = MissionService::new(
        rig.storage.clone(),
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
    );
    let before = rig.snapshot();
    let refused = attest(&restarted, &rig, &run.id, &Id::generate()).unwrap_err();
    assert_eq!(refused.code, MissionErrorCode::InvalidState);
    assert_eq!(reason(&refused), Some("attestation_not_applicable"));
    let after = rig.snapshot();
    assert_eq!(after.mission.revision, before.mission.revision);
    assert_eq!(
        after.execs.iter().find(|e| e.id == exec.id),
        before.execs.iter().find(|e| e.id == exec.id),
        "the recoverable Exec stays open"
    );
    let unchanged = after.runs.iter().find(|r| r.id == run.id).unwrap();
    assert!(unchanged.reconciliation_ref.is_none());
    assert!(unchanged.holds_execution_slot());
}

// ---- D: workspace cleanup ------------------------------------------------------

#[test]
fn workspace_cleanup_removes_only_clean_daemon_worktrees_and_keeps_the_accepted_result() {
    let active = Rig::new(true, &["status", "--porcelain"]);
    let error = active
        .service
        .handle(
            &active.conn,
            "workspace.cleanup",
            &json!({"request_id":Id::generate(),"mission_id":active.id}),
        )
        .err()
        .unwrap();
    assert_eq!(reason(&error), Some("mission_active"));

    let rig = accepted_rig();
    let snapshot = rig.snapshot();
    let candidate = snapshot
        .candidates
        .iter()
        .find(|c| Some(&c.id) == snapshot.mission.candidate_id.as_ref())
        .unwrap()
        .clone();
    let on_disk: Vec<Workspace> = snapshot
        .workspaces
        .iter()
        .filter(|w| Path::new(&w.path).is_dir())
        .cloned()
        .collect();
    assert!(on_disk.len() >= 3);
    let dirty = on_disk
        .iter()
        .find(|w| w.kind == WorkspaceKind::Worker)
        .unwrap()
        .clone();
    std::fs::write(Path::new(&dirty.path).join("uncommitted.txt"), "keep me").unwrap();
    // A projection pointing into the user's checkout is never daemon-owned.
    let user_dir = rig.repo.path().join("user-work");
    std::fs::create_dir(&user_dir).unwrap();
    let mut foreign = dirty.clone();
    foreign.id = Id::generate();
    foreign.path = user_dir.to_string_lossy().into_owned();
    // A clean but quarantined workspace (uncertain execution) is kept too.
    let mut quarantined = on_disk.iter().find(|w| w.id != dirty.id).unwrap().clone();
    quarantined.state = WorkspaceState::Quarantined;
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission.clone(),
        "test.fixture",
        "foreign-workspace",
        MissionEventType::Changed,
        vec![
            Entity::Workspace(Box::new(foreign.clone())),
            Entity::Workspace(Box::new(quarantined.clone())),
        ],
    )
    .unwrap();

    let usage = rpc(
        &rig.service,
        &rig.conn,
        "workspace.usage",
        json!({"mission_id":rig.id}),
    );
    assert_eq!(usage["missions"][0]["cleanable"], true);
    assert_eq!(usage["missions"][0]["blocked_reason"], Value::Null);
    assert_eq!(usage["missions"][0]["workspaces"], json!(on_disk.len() + 1));
    let total: u64 = usage["total_bytes"].as_str().unwrap().parse().unwrap();
    assert!(total > 0);
    let all = rpc(
        &rig.service,
        &rig.conn,
        "workspace.usage",
        json!({"mission_id":null}),
    );
    let missions = all["missions"].as_array().unwrap();
    assert!(missions.iter().any(|m| m["mission_id"] == json!(rig.id)));

    let request = json!({"request_id":Id::generate(),"mission_id":rig.id});
    let result = rpc(
        &rig.service,
        &rig.conn,
        "workspace.cleanup",
        request.clone(),
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "workspace.cleanup", request),
        result
    );
    assert_eq!(result["removed"], json!(on_disk.len() - 2));
    let kept: Vec<(String, String)> = result["kept"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| {
            let text = |key: &str| k[key].as_str().unwrap().to_owned();
            (text("path"), text("reason"))
        })
        .collect();
    assert!(kept.contains(&(dirty.path.clone(), "dirty".into())));
    assert!(kept.contains(&(foreign.path.clone(), "not_daemon_owned".into())));
    assert!(kept.contains(&(quarantined.path.clone(), "quarantined".into())));
    assert!(Path::new(&dirty.path).join("uncommitted.txt").is_file());
    assert!(Path::new(&quarantined.path).is_dir());
    assert!(user_dir.is_dir());
    for workspace in on_disk
        .iter()
        .filter(|w| w.id != dirty.id && w.id != quarantined.id)
    {
        assert!(
            !Path::new(&workspace.path).exists(),
            "{} was not removed",
            workspace.path
        );
    }
    let listed = git(rig.repo.path(), &["worktree", "list", "--porcelain"]);
    let dirty_name = Path::new(&dirty.path).file_name().unwrap();
    assert!(listed.contains(dirty_name.to_str().unwrap()));
    // Accepted result commit stays fetchable; intermediate inputs are gone.
    let candidate_ref = format!("refs/iyagi/missions/{}/candidates/{}", rig.id, candidate.id);
    assert_eq!(
        git(rig.repo.path(), &["rev-parse", &candidate_ref]),
        candidate.commit_oid
    );
    let inputs = format!("refs/iyagi/missions/{}/inputs", rig.id);
    assert_eq!(
        git(
            rig.repo.path(),
            &["for-each-ref", "--format=%(refname)", &inputs]
        ),
        ""
    );
    // The user's checkout and branch never moved.
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
    assert!(rig.repo.path().join("base.txt").is_file());
    let usage = rpc(
        &rig.service,
        &rig.conn,
        "workspace.usage",
        json!({"mission_id":rig.id}),
    );
    assert_eq!(usage["missions"][0]["workspaces"], json!(3));
    // Only kept items remain (dirty, quarantined, not daemon-owned).
    assert_eq!(usage["missions"][0]["cleanable"], false);
    assert_eq!(usage["missions"][0]["blocked_reason"], Value::Null);
}

// ---- E: follow-up missions -------------------------------------------------------

#[test]
fn follow_up_starts_from_the_accepted_commit_even_with_a_dirty_checkout() {
    let rig = accepted_rig();
    let snapshot = rig.snapshot();
    let candidate = snapshot
        .candidates
        .iter()
        .find(|c| Some(&c.id) == snapshot.mission.candidate_id.as_ref())
        .unwrap()
        .clone();
    let roles = ["lead", "builder", "reviewer", "integrator"];
    let not_accepted = draft(&rig, &rig.base, true, &roles, None).unwrap();
    let error = draft(
        &rig,
        &candidate.commit_oid,
        true,
        &roles,
        Some(&not_accepted),
    )
    .unwrap_err();
    assert_eq!(error.code, MissionErrorCode::InvalidArgument);
    assert_eq!(reason(&error), Some("follow_up_not_accepted"));
    let error = draft(
        &rig,
        &candidate.commit_oid,
        true,
        &roles,
        Some(&Id::generate()),
    )
    .unwrap_err();
    assert_eq!(reason(&error), Some("follow_up_not_accepted"));
    let error = draft(&rig, &rig.base, true, &roles, Some(&rig.id)).unwrap_err();
    assert_eq!(reason(&error), Some("follow_up_base_mismatch"));

    let follow_up = draft(&rig, &candidate.commit_oid, true, &roles, Some(&rig.id)).unwrap();
    let mission = workflow::load_entities(&rig.storage, &follow_up)
        .unwrap()
        .mission;
    assert_eq!(mission.follow_up_of.as_ref(), Some(&rig.id));
    assert_eq!(mission.base_oid, candidate.commit_oid);

    // The user's checkout is dirty and still at the old HEAD: an ordinary
    // mission refuses, the follow-up does not depend on it.
    std::fs::write(rig.repo.path().join("scratch.txt"), "user edits").unwrap();
    let ordinary = draft(&rig, &rig.base, true, &roles, None).unwrap();
    assert_eq!(
        start(&rig, &ordinary).unwrap_err().code,
        MissionErrorCode::DirtyWorktree
    );
    start(&rig, &follow_up).unwrap();

    let heads = Arc::new(Mutex::new(Vec::new()));
    let seen = heads.clone();
    let factory: AdapterFactory = Arc::new(move |run| {
        if let Some(workspace) = &run.workspace {
            let head = git(workspace, &["rev-parse", "HEAD"]);
            seen.lock().unwrap().push(head);
        }
        Ok(scripted(script(ProviderResult::Blocked {
            code: "fixture_stop".into(),
            report_text: "Observed the workspace base.".into(),
        })))
    });
    let mut actor = rig.actor(factory);
    let until = Instant::now() + Duration::from_secs(15);
    while heads.lock().unwrap().is_empty() && Instant::now() < until {
        actor.tick().unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(heads.lock().unwrap().first(), Some(&candidate.commit_oid));
    let started = workflow::load_entities(&rig.storage, &follow_up).unwrap();
    for workspace in &started.workspaces {
        assert_eq!(workspace.base_oid, candidate.commit_oid);
    }
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
    assert_eq!(
        std::fs::read_to_string(rig.repo.path().join("scratch.txt")).unwrap(),
        "user edits"
    );
    actor.shutdown();
}

// ---- E: uncommitted work as the base ---------------------------------------

#[test]
fn uncommitted_work_can_be_the_base_without_committing_or_touching_the_checkout() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let repo = rig.repo.path();
    // One untracked file and one edit to the repository's committed file.
    std::fs::write(repo.join("scratch.txt"), "user edits").unwrap();
    std::fs::write(repo.join("base.txt"), "edited in place\n").unwrap();
    let roles = ["lead", "builder"];

    // Without the choice the repository is still refused, unchanged.
    let ordinary = draft(&rig, &rig.base, false, &roles, None).unwrap();
    assert_eq!(
        start(&rig, &ordinary).unwrap_err().code,
        MissionErrorCode::DirtyWorktree
    );

    let included = draft_including(&rig, &rig.base, false, &roles, None, true).unwrap();
    let mission = workflow::load_entities(&rig.storage, &included)
        .unwrap()
        .mission;
    let recorded = mission
        .base_snapshot
        .clone()
        .expect("working tree recorded");
    assert_eq!(recorded.head_oid, rig.base);
    assert_eq!(recorded.entry_count, 2);
    assert_ne!(mission.base_oid, rig.base);
    start(&rig, &included).unwrap();

    // The base carries the uncommitted work and sits on the user's HEAD.
    let base_oid = workflow::load_entities(&rig.storage, &included)
        .unwrap()
        .mission
        .base_oid;
    assert_eq!(
        git(
            repo,
            &["cat-file", "-p", &format!("{base_oid}:scratch.txt")]
        ),
        "user edits"
    );
    assert_eq!(
        git(repo, &["cat-file", "-p", &format!("{base_oid}:base.txt")]),
        "edited in place"
    );
    assert_eq!(git(repo, &["rev-parse", &format!("{base_oid}^")]), rig.base);

    // Nothing of the user's moved: same HEAD, nothing staged, the same
    // pending changes, and the files still hold their own text.
    assert_eq!(git(repo, &["rev-parse", "HEAD"]), rig.base);
    assert_eq!(git(repo, &["diff", "--cached", "--name-only"]), "");
    assert!(git(repo, &["status", "--porcelain"]).contains("scratch.txt"));
    assert_eq!(
        std::fs::read_to_string(repo.join("scratch.txt")).unwrap(),
        "user edits"
    );
}

#[test]
fn a_follow_up_cannot_extend_uncommitted_work_and_a_clean_tree_records_nothing() {
    let rig = accepted_rig();
    let candidate = rig
        .snapshot()
        .candidates
        .iter()
        .find(|c| Some(&c.id) == rig.snapshot().mission.candidate_id.as_ref())
        .unwrap()
        .clone();
    let roles = ["lead", "builder"];
    let error = draft_including(
        &rig,
        &candidate.commit_oid,
        false,
        &roles,
        Some(&rig.id),
        true,
    )
    .unwrap_err();
    assert_eq!(error.code, MissionErrorCode::InvalidArgument);
    assert_eq!(reason(&error), Some("follow_up_snapshot"));

    // Asking for uncommitted work when there is none is an ordinary mission.
    let clean = draft_including(&rig, &rig.base, false, &roles, None, true).unwrap();
    let mission = workflow::load_entities(&rig.storage, &clean)
        .unwrap()
        .mission;
    assert_eq!(mission.base_oid, rig.base);
    assert!(mission.base_snapshot.is_none());
}
