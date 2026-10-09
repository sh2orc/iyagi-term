//! O13 mission workflow integration tests (ticket O13, 06 §3): the full
//! goal→completed pipeline on a real temporary Git repository and a real
//! mission store — two writer worktrees → capture → integration → minted
//! candidate → real verification command → typed review findings →
//! mission.accept — plus the negative paths (W03/W05/W07–W10).

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use iyagi_termd_lib::mission::artifacts::ArtifactStore;
use iyagi_termd_lib::mission::service::MissionService;
use iyagi_termd_lib::mission::workflow::{
    acceptance_ready, commit_upserts, load_entities, mint_verify_task, record_findings,
    run_verification, AcceptanceInputs, MintResult, Rejection, VerificationRequest,
    VerificationRunResult,
};
use iyagi_termd_lib::workspace::{add_detached_worktree, capture, CapturedCandidate};
use term_contracts::ids::{ConnectionId, U64String};
use term_contracts::mission::error::{MissionErrorCode, MissionRpcError};
use term_contracts::mission::types::{
    ArtifactRef, Candidate, Decision, DecisionKind, DecisionState, Entity, ExpectedOutput,
    FindingResolution, FindingSeverity, Id, InputIntegrity, Mission, MissionEventType,
    MissionState, Phase, Policy, ProviderFindingDraft, Requirement, Role, Run, RunDispatchState,
    RunState, Task, TaskContract, TaskKind, TaskState, UnknownCostPolicy, Verification,
    VerificationCommand, VerificationStatus,
};
use term_contracts::mission::validation::unknown_usage;
use term_storage::Storage;

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    iyagi_termd_lib::workspace::git::run_git_for_test(dir, args)
}

fn rpc(service: &MissionService, conn: &ConnectionId, method: &str, params: &Value) -> Value {
    service.handle(conn, method, params).expect(method).result
}

/// Stage one artifact through begin/write/commit as the test connection.
fn upload_staged(service: &MissionService, conn: &ConnectionId, body: &[u8]) -> Value {
    use base64::Engine;
    let digest: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let begin = rpc(
        service,
        conn,
        "artifact.begin",
        &json!({
            "request_id": Id::generate().to_string(),
            "mission_id": null,
            "media_type": "text/plain",
            "bytes": body.len().to_string(),
            "sha256": digest,
        }),
    );
    let upload_id = begin["upload_id"].as_str().expect("upload id").to_string();
    let chunk = begin["chunk_bytes"].as_u64().expect("chunk size") as usize;
    let mut offset = 0u64;
    for slice in body.chunks(chunk.max(1)) {
        let next = rpc(
            service,
            conn,
            "artifact.write",
            &json!({
                "upload_id": upload_id,
                "offset": offset.to_string(),
                "data_b64": base64::engine::general_purpose::STANDARD.encode(slice),
            }),
        );
        offset = next["next_offset"]
            .as_str()
            .expect("next offset")
            .parse()
            .unwrap();
    }
    rpc(
        service,
        conn,
        "artifact.commit",
        &json!({ "upload_id": upload_id }),
    )
}

/// One full test rig: a real git repo with a base commit, a real mission
/// store + artifact root, and a running mission with one verification
/// command in the allowlist.
struct Rig {
    repo: tempfile::TempDir,
    base: String,
    work: tempfile::TempDir,
    #[allow(dead_code)]
    data: tempfile::TempDir,
    missions_root: PathBuf,
    storage: Arc<Storage>,
    service: MissionService,
    artifacts: ArtifactStore,
    conn: ConnectionId,
    mission_id: Id,
    command_id: Id,
    requirement_id: Id,
}

impl Rig {
    fn new(strict_integrity: bool) -> Rig {
        let repo = tempfile::tempdir().expect("repo dir");
        git(repo.path(), &["init", "-q"]);
        git(repo.path(), &["config", "user.email", "test@iyagi.local"]);
        git(repo.path(), &["config", "user.name", "test"]);
        std::fs::write(repo.path().join("base.txt"), "base\n").expect("base file");
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-m", "base", "-q"]);
        let base = git(repo.path(), &["rev-parse", "HEAD"]);

        let data = tempfile::tempdir().expect("data dir");
        let storage = Arc::new(Storage::open(data.path().join("test.db")).expect("storage"));
        let missions_root = data.path().join("missions");
        let artifacts = ArtifactStore::new(Arc::clone(&storage), missions_root.clone());
        let service = MissionService::new(
            Arc::clone(&storage),
            ArtifactStore::new(Arc::clone(&storage), missions_root.clone()),
        );
        let conn = ConnectionId::generate();
        let command_id = Id::generate();
        let requirement_id = Id::generate();
        let binding_id = Id::generate();
        let mut binding = iyagi_termd_lib::agent_runtime::fake::fake_binding();
        binding.id = binding_id.clone();
        rpc(
            &service,
            &conn,
            "binding.save",
            &json!({"request_id": Id::generate(), "expected_revision": "0", "binding": binding}),
        );

        let goal = upload_staged(
            &service,
            &conn,
            "로그인 기능과 검증 시나리오를 완성한다.".as_bytes(),
        );
        let created = rpc(
            &service,
            &conn,
            "mission.create",
            &json!({
                "request_id": Id::generate().to_string(),
                "title": "O13 workflow mission",
                "repository_path": repo.path().to_string_lossy(),
                "expected_base_oid": base,
                "goal_ref": goal,
                "requirements": [{
                    "id": requirement_id.to_string(),
                    "text": "변경이 통합되고 검증 명령이 통과해야 한다.",
                    "verification_ids": [command_id.to_string()],
                    "human_check": false,
                }],
                "policy": {
                    "max_parallel_runs": 4,
                    "max_attempts_per_task": 3,
                    "max_repair_cycles": 3,
                    "max_automatic_starts": 64,
                    "active_time_limit_ms": "14400000",
                    "run_time_limit_ms": "2700000",
                    "max_cost_usd_micros": null,
                    "unknown_cost": "allow_with_notice",
                    "allow_network": false,
                    "allow_automatic_plan_apply": true,
                    "allow_recovery_of_unsent": true,
                    "allowed_binding_ids": [binding_id.to_string()],
                    "allowed_roles": ["lead", "builder", "reviewer", "integrator"],
                    "allowed_verification_ids": [command_id.to_string()],
                    "require_independent_review": true,
                    "require_enforced_verification": strict_integrity,
                },
                // Start requires Lead and Builder, plus Reviewer with review.
                "role_bindings": [
                    { "role": "lead", "primary_binding_id": binding_id.to_string(), "fallback_binding_ids": [] },
                    { "role": "builder", "primary_binding_id": binding_id.to_string(), "fallback_binding_ids": [] },
                    { "role": "reviewer", "primary_binding_id": binding_id.to_string(), "fallback_binding_ids": [] },
                ],
            }),
        );
        let mission_id = Id::parse(created["mission_id"].as_str().expect("mission id"))
            .expect("mission id is a uuid v4");
        rpc(
            &service,
            &conn,
            "mission.control",
            &json!({
                "request_id": Id::generate().to_string(),
                "mission_id": mission_id.to_string(),
                "expected_revision": "1",
                "action": "start",
            }),
        );

        // This suite starts at the workflow boundary with an accepted
        // fixture plan. The bootstrap task now exists in production; mark
        // that fixture planning step complete before exercising integration.
        let planned = load_entities(&storage, &mission_id).unwrap();
        let plan_tasks = planned
            .tasks
            .into_iter()
            .map(|mut task| {
                assert_eq!(task.kind, TaskKind::Plan);
                task.state = TaskState::Succeeded;
                Entity::Task(Box::new(task))
            })
            .collect();
        commit_upserts(
            &service,
            planned.mission,
            "fixture.plan",
            "accepted fixture plan",
            MissionEventType::Changed,
            plan_tasks,
        )
        .unwrap();

        Rig {
            repo,
            base,
            work: tempfile::tempdir().expect("work root"),
            data,
            missions_root,
            storage,
            service,
            artifacts,
            conn,
            mission_id,
            command_id,
            requirement_id,
        }
    }

    fn mission(&self) -> Mission {
        load_entities(&self.storage, &self.mission_id)
            .expect("mission read")
            .mission
    }

    /// Seed one already-finished task + terminal run (writer/review done).
    fn seed_succeeded_task(
        &self,
        kind: TaskKind,
        role: Option<Role>,
        title: &str,
        allowed_paths: &[&str],
    ) -> (Id, Id) {
        let mission = self.mission();
        let entities = load_entities(&self.storage, &self.mission_id).expect("entities");
        let ordinal = entities.tasks.len() as u32 + 1;
        let timestamp = term_storage::time::now_iso8601();
        let task_id = Id::generate();
        let run_id = Id::generate();
        let task = Task {
            id: task_id.clone(),
            mission_id: self.mission_id.clone(),
            title: title.into(),
            kind,
            role,
            state: TaskState::Succeeded,
            required: true,
            parent_task_id: None,
            depends_on: Vec::new(),
            contract: TaskContract {
                objective_ref: mission.goal_ref.clone(),
                requirement_ids: Vec::new(),
                input_artifact_ids: Vec::new(),
                allowed_paths: allowed_paths.iter().map(|p| p.to_string()).collect(),
                expected_outputs: vec![if kind == TaskKind::Review {
                    ExpectedOutput::Review
                } else {
                    ExpectedOutput::Patch
                }],
                verification_ids: Vec::new(),
                specialty: None,
            },
            binding_id: None,
            active_run_id: None,
            ordinal,
            attempt_count: 1,
            repair_cycle: 0,
            failure_repair_run_ids: vec![],
            integration: None,
            replacement_of: None,
            blocked_code: None,
            dispatch_after_unix_ms: None,
            workspace_id: None,
            created_at: timestamp.clone(),
            updated_at: timestamp.clone(),
        };
        let run = Run {
            id: run_id.clone(),
            mission_id: self.mission_id.clone(),
            task_id: task_id.clone(),
            attempt: 1,
            state: RunState::Succeeded,
            binding_snapshot: None,
            requested_model: None,
            observed_model: None,
            provider_session_id: None,
            provider_turn_id: None,
            exec_id: None,
            pty_session_id: None,
            workspace_id: None,
            fencing_token: U64String::new(1).unwrap(),
            dispatch_state: RunDispatchState::Acknowledged,
            context_ref: mission.goal_ref.clone(),
            result_ref: None,
            usage: unknown_usage(),
            last_activity_at: None,
            active_time_ms: U64String::new(0).unwrap(),
            started_at: Some(timestamp.clone()),
            ended_at: Some(timestamp),
            failure_code: None,
            reconciliation_ref: None,
            reconciliation_kind: None,
            rate_limit: None,
            retry_evidence: None,
        };
        commit_upserts(
            &self.service,
            mission,
            "test.seed",
            &format!("{}:{task_id}", self.mission_id),
            MissionEventType::Changed,
            vec![Entity::Task(Box::new(task)), Entity::Run(Box::new(run))],
        )
        .expect("seed commit");
        (task_id, run_id)
    }

    /// Mint the verify task/run and execute the command against the current
    /// candidate in a fresh verification worktree.
    fn verify(&self, program: &str, argv: &[&str], timeout_ms: u64) -> VerificationRunResult {
        let mission = self.mission();
        let candidate_id = mission.candidate_id.clone().expect("candidate minted");
        let command = VerificationCommand {
            id: self.command_id.clone(),
            title: "integration check".into(),
            program: program.into(),
            argv: argv.iter().map(|arg| arg.to_string()).collect(),
            revision: U64String::new(1).unwrap(),
            repository_id: mission.repository_id.clone(),
            cwd_relative: String::new(),
            timeout_ms,
            env_profile_ref: None,
            allowed_network: false,
        };
        let (task_id, run_id) = mint_verify_task(
            &self.service,
            &self.artifacts,
            &self.mission_id,
            &command,
            vec![self.requirement_id.clone()],
        )
        .expect("verify task minted");
        let worktree = self.work.path().join(format!("verify-{}", Id::generate()));
        let request = VerificationRequest {
            mission_id: &self.mission_id,
            command: &command,
            candidate_id: &candidate_id,
            repository: self.repo.path(),
            worktree: &worktree,
            verify_task_id: &task_id,
            verify_run_id: &run_id,
            requirement_ids: vec![self.requirement_id.clone()],
        };
        run_verification(&self.service, &self.artifacts, &request).expect("verification executed")
    }

    /// Independent review task + typed findings (empty list = clean review).
    fn review(&self, drafts: &[ProviderFindingDraft]) {
        let (_task_id, run_id) = self.seed_succeeded_task(
            TaskKind::Review,
            Some(Role::Reviewer),
            "independent review",
            &[],
        );
        {
            let candidate = self.mission().candidate_id.clone().expect("candidate");
            let findings = record_findings(
                &self.service,
                &self.artifacts,
                &self.mission_id,
                &run_id,
                &candidate,
                drafts,
            )
            .expect("findings recorded");
            assert_eq!(findings.len(), drafts.len());
        }
    }

    fn accept(&self, candidate: &Id, acknowledged: &[Id]) -> Result<Value, MissionRpcError> {
        let mission = self.mission();
        self.service
            .handle(
                &self.conn,
                "mission.accept",
                &json!({
                    "request_id": Id::generate().to_string(),
                    "mission_id": self.mission_id.to_string(),
                    "expected_revision": mission.revision.get().to_string(),
                    "candidate_id": candidate.to_string(),
                    "acknowledged_verification_ids": acknowledged
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>(),
                    "human_requirement_ids": [],
                }),
            )
            .map(|handled| handled.result)
    }
}

/// A writer worktree at the base with the given edits, captured as a
/// candidate on its private ref.
fn writer_candidate(
    rig: &Rig,
    name: &str,
    edits: &[(&str, &str)],
    allowed: &[&str],
    source_run_id: Id,
) -> CapturedCandidate {
    let worktree = rig.work.path().join(name);
    add_detached_worktree(rig.repo.path(), &rig.base, &worktree).expect("writer worktree");
    for (path, content) in edits {
        let target = worktree.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("writer dir");
        }
        std::fs::write(target, content).expect("writer file");
    }
    let captured = capture(
        &worktree,
        &rig.mission_id,
        vec![source_run_id],
        &allowed.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        &rig.base,
    )
    .expect("capture");
    persist_captured(rig, &captured);
    captured
}

fn persist_captured(rig: &Rig, captured: &CapturedCandidate) {
    let manifest_ref = iyagi_termd_lib::mission::workflow::store_artifact(
        &rig.artifacts,
        &rig.mission_id,
        "application/json",
        &serde_json::to_vec(&captured.manifest).unwrap(),
    )
    .unwrap();
    let candidate = Candidate {
        id: captured.candidate_id.clone(),
        mission_id: rig.mission_id.clone(),
        revision: 0,
        base_oid: captured.base_oid.clone(),
        commit_oid: captured.commit_oid.clone(),
        tree_oid: captured.tree_oid.clone(),
        source_run_ids: captured.source_run_ids.clone(),
        manifest_ref,
        created_at: term_storage::time::now_iso8601(),
        supersedes_id: None,
    };
    commit_upserts(
        &rig.service,
        rig.mission(),
        "test.capture",
        &captured.candidate_id.to_string(),
        MissionEventType::Changed,
        vec![Entity::Candidate(Box::new(candidate))],
    )
    .unwrap();
}

fn integrate(rig: &Rig, sources: &[(Id, Vec<Id>)]) -> MintResult {
    let worktree = rig.work.path().join(format!("integ-{}", Id::generate()));
    iyagi_termd_lib::mission::workflow::integrate_and_mint(
        &rig.service,
        &rig.artifacts,
        &rig.mission_id,
        rig.repo.path(),
        &worktree,
        sources,
    )
    .expect("integration")
}

#[test]
fn integration_freezes_stored_candidate_and_records_exact_source_oids() {
    let rig = Rig::new(false);
    let (_, run) = rig.seed_succeeded_task(
        TaskKind::Implement,
        Some(Role::Builder),
        "source",
        &["source.txt"],
    );
    let original = writer_candidate(
        &rig,
        "original",
        &[("source.txt", "recorded\n")],
        &["source.txt"],
        run,
    );
    let (_, other_run) = rig.seed_succeeded_task(
        TaskKind::Implement,
        Some(Role::Builder),
        "other",
        &["source.txt"],
    );
    let other = writer_candidate(
        &rig,
        "other",
        &[("source.txt", "wrong\n")],
        &["source.txt"],
        other_run,
    );
    let reference = iyagi_termd_lib::workspace::git::candidate_ref(
        rig.mission_id.as_str(),
        original.candidate_id.as_str(),
    );
    git(
        rig.repo.path(),
        &["update-ref", &reference, &other.commit_oid],
    );
    let MintResult::Integrated { candidate, .. } = integrate(
        &rig,
        &[(
            original.candidate_id.clone(),
            original.source_run_ids.clone(),
        )],
    ) else {
        panic!("clean integration")
    };
    assert_eq!(candidate.tree_oid, original.tree_oid);
    assert_eq!(candidate.source_run_ids, original.source_run_ids);
    let manifest: Value = serde_json::from_slice(
        &rig.artifacts
            .read_mission_body(&rig.mission_id, &candidate.manifest_ref, 256 * 1024)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["tree_oid"], candidate.tree_oid);
    assert_eq!(
        manifest["sources"],
        json!([{
            "candidate_id": original.candidate_id,
            "source_run_ids": original.source_run_ids,
            "base_oid": original.base_oid,
            "commit_oid": original.commit_oid,
            "tree_oid": original.tree_oid,
        }])
    );
    assert_eq!(
        git(rig.repo.path(), &["rev-parse", &reference]),
        other.commit_oid
    );
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
}

#[test]
fn integration_rejects_unrecorded_or_mislabeled_sources_before_creating_worktree() {
    let rig = Rig::new(false);
    let (_, run) = rig.seed_succeeded_task(
        TaskKind::Implement,
        Some(Role::Builder),
        "source",
        &["source.txt"],
    );
    let source = writer_candidate(
        &rig,
        "original",
        &[("source.txt", "recorded\n")],
        &["source.txt"],
        run,
    );
    let (_, unrelated_run) = rig.seed_succeeded_task(
        TaskKind::Implement,
        Some(Role::Builder),
        "unrelated",
        &["other.txt"],
    );
    let valid = (source.candidate_id.clone(), source.source_run_ids.clone());
    let before = rig.mission();
    let refs_before = git(rig.repo.path(), &["show-ref"]);
    let worktrees_before = git(rig.repo.path(), &["worktree", "list", "--porcelain"]);
    for sources in [
        vec![(Id::generate(), source.source_run_ids.clone())],
        vec![(source.candidate_id.clone(), vec![unrelated_run])],
        vec![(source.candidate_id.clone(), vec![])],
        vec![valid.clone(), valid],
    ] {
        let path = rig.work.path().join(format!("rejected-{}", Id::generate()));
        let result = iyagi_termd_lib::mission::workflow::integrate_and_mint(
            &rig.service,
            &rig.artifacts,
            &rig.mission_id,
            rig.repo.path(),
            &path,
            &sources,
        );
        let error = result.err().expect("reject invalid source");
        assert_eq!(error.code, MissionErrorCode::IntegrityFailed);
        assert!(!path.exists());
        assert_eq!(rig.mission().revision, before.revision);
        assert_eq!(git(rig.repo.path(), &["show-ref"]), refs_before);
        assert_eq!(
            git(rig.repo.path(), &["worktree", "list", "--porcelain"]),
            worktrees_before
        );
        let snapshot = load_entities(&rig.storage, &rig.mission_id).unwrap();
        assert_eq!(snapshot.candidates.len(), 1);
        assert!(snapshot.workspaces.is_empty());
        assert!(snapshot.decisions.is_empty());
    }
}

/// Two writer candidates over disjoint paths, integrated cleanly: the rig
/// returns with a minted candidate and phase=validating.
fn pipeline_rig(strict: bool) -> Rig {
    let rig = Rig::new(strict);
    let (_t1, r1) = rig.seed_succeeded_task(
        TaskKind::Implement,
        Some(Role::Builder),
        "implement api",
        &["api.txt"],
    );
    let (_t2, r2) = rig.seed_succeeded_task(
        TaskKind::Implement,
        Some(Role::Builder),
        "implement ui",
        &["ui.txt"],
    );
    let api = writer_candidate(
        &rig,
        "w-api",
        &[("api.txt", "api change\n")],
        &["api.txt"],
        r1,
    );
    let ui = writer_candidate(&rig, "w-ui", &[("ui.txt", "ui change\n")], &["ui.txt"], r2);
    match integrate(
        &rig,
        &[
            (api.candidate_id.clone(), api.source_run_ids.clone()),
            (ui.candidate_id.clone(), ui.source_run_ids.clone()),
        ],
    ) {
        MintResult::Integrated { candidate, .. } => {
            assert_eq!(rig.mission().candidate_id, Some(candidate.id.clone()));
            assert_eq!(rig.mission().phase, Phase::Validating);
        }
        MintResult::Conflict { .. } => panic!("disjoint sources must integrate cleanly"),
    }
    rig
}

fn passed_verification_rig(strict: bool) -> (Rig, VerificationRunResult) {
    let rig = pipeline_rig(strict);
    let verification = rig.verify("git", &["rev-parse", "HEAD"], 30_000);
    (rig, verification)
}

// ---- happy path --------------------------------------------------------------

#[test]
fn goal_to_completed_full_pipeline() {
    let (rig, verification) = passed_verification_rig(false);
    assert_eq!(verification.verification.status, VerificationStatus::Passed);
    assert_eq!(verification.verification.exit_code, Some(0));
    assert!(!verification.timed_out);
    // W06: honest integrity — observed, never silently promoted to enforced.
    assert_eq!(
        verification.verification.input_integrity,
        InputIntegrity::Observed
    );
    assert_eq!(rig.mission().phase, Phase::Reviewing);

    // Independent review with only a note-level finding (does not block).
    rig.review(&[ProviderFindingDraft {
        severity: FindingSeverity::Note,
        path: Some("api.txt".into()),
        line: Some(1),
        evidence_text: "스타일 제안 — 표기만 다르고 동작 문제 없음".into(),
        requirement_id: None,
    }]);

    let candidate = rig.mission().candidate_id.clone().expect("candidate");

    // W09 (normal policy): observed verification needs explicit confirmation.
    let unacknowledged = rig
        .accept(&candidate, &[])
        .expect_err("unacknowledged observed verification must reject");
    assert_eq!(unacknowledged.code, MissionErrorCode::PolicyDenied);
    assert_eq!(
        unacknowledged.details.reason_code.as_deref(),
        Some("observed_not_acknowledged")
    );

    // Acknowledged accept completes the mission atomically.
    let accepted = rig
        .accept(
            &candidate,
            std::slice::from_ref(&verification.verification.id),
        )
        .expect("accept succeeds");
    assert!(accepted["revision"].as_str().is_some());
    let mission = rig.mission();
    assert_eq!(mission.state, MissionState::Completed);
    assert_eq!(mission.phase, Phase::Done);
    assert!(mission.accepted_at.is_some());

    // The Accepted event committed in the same transaction (04 §6 step 8).
    let (events, _) = rig
        .storage
        .mission_events(&rig.mission_id, 0, 1000)
        .unwrap();
    assert!(events
        .iter()
        .any(|stored| matches!(stored.event.event_type, MissionEventType::Accepted)));

    // The user checkout never moved (W02 invariant).
    assert!(git(rig.repo.path(), &["status", "--porcelain"]).is_empty());

    // Re-accepting the completed mission is refused.
    let again = rig
        .accept(
            &candidate,
            std::slice::from_ref(&verification.verification.id),
        )
        .expect_err("completed missions cannot accept again");
    assert_eq!(again.code, MissionErrorCode::InvalidState);
}

#[test]
fn accept_tolerates_housekeeping_commits_but_not_meaningful_changes() {
    let (rig, verification) = passed_verification_rig(false);
    rig.review(&[]);
    let candidate = rig.mission().candidate_id.clone().expect("candidate");
    let accept_at = |revision: &U64String| {
        rig.service
            .handle(
                &rig.conn,
                "mission.accept",
                &json!({
                    "request_id": Id::generate().to_string(),
                    "mission_id": rig.mission_id.to_string(),
                    "expected_revision": revision,
                    "candidate_id": candidate.to_string(),
                    "acknowledged_verification_ids": [verification.verification.id.to_string()],
                    "human_requirement_ids": [],
                }),
            )
            .map(|handled| handled.result)
    };

    // A meaningful commit after the offered revision still rejects.
    let offered = rig.mission();
    commit_upserts(
        &rig.service,
        offered.clone(),
        "fixture.touch",
        "touch",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    let stale = accept_at(&offered.revision).expect_err("meaningful change conflicts");
    assert_eq!(stale.code, MissionErrorCode::RevisionConflict);
    assert_eq!(
        stale.details.reason_code.as_deref(),
        Some("revision_mismatch")
    );
    assert_eq!(rig.mission().state, MissionState::Running);

    // A housekeeping-only commit (time checkpoint) does not.
    let offered = rig.mission();
    commit_upserts(
        &rig.service,
        offered.clone(),
        "engine.time_checkpoint",
        "checkpoint",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    let checkpointed = rig.mission();
    assert_eq!(checkpointed.revision.get(), offered.revision.get() + 1);
    assert_eq!(checkpointed.semantic_revision, offered.semantic_revision);
    let accepted = accept_at(&offered.revision).expect("housekeeping keeps the offer current");
    assert_eq!(
        accepted["revision"],
        json!((checkpointed.revision.get() + 1).to_string())
    );
    let mission = rig.mission();
    assert_eq!(mission.state, MissionState::Completed);
    assert_eq!(mission.semantic_revision, Some(mission.revision.clone()));
}

// ---- W03: integration conflict → decision, not mission failure -----------------

#[test]
fn w03_conflict_records_blocking_decision_not_mission_failure() {
    let rig = Rig::new(false);
    let (_t1, r1) = rig.seed_succeeded_task(
        TaskKind::Implement,
        Some(Role::Builder),
        "implement shared A",
        &["shared.txt"],
    );
    let (_t2, r2) = rig.seed_succeeded_task(
        TaskKind::Implement,
        Some(Role::Builder),
        "implement shared B",
        &["shared.txt"],
    );
    let a = writer_candidate(
        &rig,
        "w-a",
        &[("shared.txt", "version A\n")],
        &["shared.txt"],
        r1,
    );
    let b = writer_candidate(
        &rig,
        "w-b",
        &[("shared.txt", "version B\nsecond line\n")],
        &["shared.txt"],
        r2,
    );
    let decision_id = match integrate(
        &rig,
        &[
            (a.candidate_id.clone(), a.source_run_ids.clone()),
            (b.candidate_id.clone(), b.source_run_ids.clone()),
        ],
    ) {
        MintResult::Conflict {
            outcome,
            decision_id,
        } => {
            let (conflicting, paths) = outcome.conflict.expect("conflict recorded");
            assert_eq!(conflicting, b.candidate_id);
            assert!(
                paths.iter().any(|path| path.contains("shared.txt")),
                "{paths:?}"
            );
            assert_eq!(outcome.sources.len(), 1, "first source applied");
            decision_id
        }
        MintResult::Integrated { .. } => panic!("conflicting sources must not mint"),
    };

    // The mission is NOT failed — it waits on a blocking Conflict decision.
    let mission = rig.mission();
    assert_eq!(mission.state, MissionState::Running);
    assert_eq!(mission.failure_code, None);
    assert_eq!(mission.open_decision_count, 1);
    assert!(mission.candidate_id.is_none());

    let entities = load_entities(&rig.storage, &rig.mission_id).unwrap();
    let decision = entities
        .decisions
        .iter()
        .find(|decision| decision.id == decision_id)
        .expect("decision stored");
    assert_eq!(decision.kind, DecisionKind::Conflict);
    assert_eq!(decision.state, DecisionState::Open);
    assert!(decision.blocking);
    assert!(
        !decision.affected_task_ids.is_empty(),
        "writer tasks implicated through their runs"
    );
    let question: Value = serde_json::from_slice(
        &rig.artifacts
            .read_mission_body(&rig.mission_id, &decision.question_ref, 256 * 1024)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(question["input_sources"].as_array().unwrap().len(), 2);
    assert_eq!(question["input_sources"][0]["commit_oid"], a.commit_oid);
    assert_eq!(question["input_sources"][1]["commit_oid"], b.commit_oid);
    assert_eq!(question["input_sources"][1]["tree_oid"], b.tree_oid);
    assert_eq!(
        question["applied_sources"],
        json!([question["input_sources"][0]])
    );
    assert_eq!(question["conflict"]["candidate_id"], json!(b.candidate_id));

    // No implicit ours: the user checkout and both worktrees keep their files.
    assert!(git(rig.repo.path(), &["status", "--porcelain"]).is_empty());
    assert!(rig.work.path().join("w-a").join("shared.txt").exists());
    assert!(rig.work.path().join("w-b").join("shared.txt").exists());

    // With no candidate minted, accept is STALE_CANDIDATE (04 §6 step 1).
    let rejected = rig
        .accept(&a.candidate_id, &[])
        .expect_err("accept without a minted candidate");
    assert_eq!(rejected.code, MissionErrorCode::StaleCandidate);
}

// ---- W07: failed command / timeout fail the task, never the mission ------------

#[test]
fn w07_failed_command_fails_task_not_mission_and_blocks_accept() {
    let rig = pipeline_rig(false);
    let verification = rig.verify(
        "git",
        &["rev-parse", "--verify", "definitely-not-a-ref"],
        30_000,
    );
    assert_eq!(verification.verification.status, VerificationStatus::Failed);
    assert_ne!(verification.verification.exit_code, Some(0));
    assert!(!verification.timed_out);

    let entities = load_entities(&rig.storage, &rig.mission_id).unwrap();
    let verify_task = entities
        .tasks
        .iter()
        .find(|task| task.kind == TaskKind::Verify)
        .expect("verify task");
    assert_eq!(verify_task.state, TaskState::Failed);
    let mission = rig.mission();
    assert_eq!(
        mission.state,
        MissionState::Running,
        "the task failed, not the mission"
    );

    let candidate = mission.candidate_id.clone().expect("candidate");
    let rejected = rig
        .accept(
            &candidate,
            std::slice::from_ref(&verification.verification.id),
        )
        .expect_err("accept after failed verification");
    assert_eq!(rejected.code, MissionErrorCode::InvalidState);
    assert_eq!(
        rejected.details.reason_code.as_deref(),
        Some("required_task_not_succeeded")
    );
}

#[test]
fn w07_timeout_kills_the_tree_and_forbids_passed() {
    let rig = pipeline_rig(false);
    let verification = if cfg!(windows) {
        rig.verify("ping", &["-n", "30", "127.0.0.1"], 400)
    } else {
        rig.verify("sleep", &["30"], 400)
    };
    assert!(verification.timed_out, "watchdog must fire");
    assert_eq!(verification.verification.status, VerificationStatus::Failed);

    // The log artifact is preserved even for a killed run.
    let log = &verification.verification.log_ref;
    assert!(log.bytes.get() > 0, "log body must exist");
    let row = rig
        .storage
        .mission_artifact(&log.id)
        .unwrap()
        .expect("log artifact row");
    assert!(rig.missions_root.join(&row.relative_path).is_file());

    let entities = load_entities(&rig.storage, &rig.mission_id).unwrap();
    let verify_task = entities
        .tasks
        .iter()
        .find(|task| task.kind == TaskKind::Verify)
        .expect("verify task");
    assert_eq!(verify_task.state, TaskState::Failed);
    assert_eq!(rig.mission().state, MissionState::Running);
}

// ---- W08: open blocking finding blocks acceptance ------------------------------

#[test]
fn w08_open_blocking_finding_rejects_acceptance() {
    let (rig, verification) = passed_verification_rig(false);
    rig.review(&[
        ProviderFindingDraft {
            severity: FindingSeverity::Blocking,
            path: Some("api.txt".into()),
            line: Some(1),
            evidence_text: "인증 우회 경로: 토큰 검증 누락".into(),
            requirement_id: Some(rig.requirement_id.clone()),
        },
        ProviderFindingDraft {
            severity: FindingSeverity::Major,
            path: Some("ui.txt".into()),
            line: None,
            evidence_text: "오류 상태가 사용자에게 표시되지 않음".into(),
            requirement_id: None,
        },
    ]);
    let candidate = rig.mission().candidate_id.clone().expect("candidate");
    let rejected = rig
        .accept(
            &candidate,
            std::slice::from_ref(&verification.verification.id),
        )
        .expect_err("open blocking finding must reject");
    assert_eq!(rejected.code, MissionErrorCode::InvalidState);
    assert_eq!(
        rejected.details.reason_code.as_deref(),
        Some("open_finding")
    );
    assert_eq!(rig.mission().state, MissionState::Running);

    // The findings are stored open against the current candidate.
    let entities = load_entities(&rig.storage, &rig.mission_id).unwrap();
    let open_blocking: Vec<_> = entities
        .findings
        .iter()
        .filter(|finding| {
            finding.resolution == FindingResolution::Open
                && matches!(
                    finding.severity,
                    FindingSeverity::Blocking | FindingSeverity::Major
                )
        })
        .collect();
    assert_eq!(open_blocking.len(), 2);
}

// ---- W10: stale candidate id ---------------------------------------------------

#[test]
fn w10_stale_candidate_id_rejects_acceptance() {
    let (rig, verification) = passed_verification_rig(false);
    rig.review(&[]);
    let stale = Id::generate();
    let rejected = rig
        .accept(&stale, std::slice::from_ref(&verification.verification.id))
        .expect_err("wrong candidate id must reject");
    assert_eq!(rejected.code, MissionErrorCode::StaleCandidate);
    assert_eq!(rig.mission().state, MissionState::Running);

    // The real candidate still accepts afterwards.
    let candidate = rig.mission().candidate_id.clone().expect("candidate");
    rig.accept(
        &candidate,
        std::slice::from_ref(&verification.verification.id),
    )
    .expect("the current candidate still accepts");
    assert_eq!(rig.mission().state, MissionState::Completed);
}

// ---- W09: strict integrity policy ----------------------------------------------

#[test]
fn w09_strict_policy_rejects_observed_only_verification() {
    let (rig, verification) = passed_verification_rig(true);
    assert_eq!(verification.verification.status, VerificationStatus::Passed);
    assert_eq!(
        verification.verification.input_integrity,
        InputIntegrity::Observed
    );
    rig.review(&[]);
    let candidate = rig.mission().candidate_id.clone().expect("candidate");
    // Acknowledgment cannot promote observed to enforced under strict policy.
    let rejected = rig
        .accept(
            &candidate,
            std::slice::from_ref(&verification.verification.id),
        )
        .expect_err("observed-only evidence must reject under strict policy");
    assert_eq!(rejected.code, MissionErrorCode::PolicyDenied);
    assert_eq!(
        rejected.details.reason_code.as_deref(),
        Some("integrity_policy_unmet")
    );
    assert_eq!(rig.mission().state, MissionState::Running);
}

// ---- W05: evidence is candidate-scoped ------------------------------------------

#[test]
fn w05_new_candidate_cannot_reuse_old_evidence() {
    let (rig, verification) = passed_verification_rig(false);
    rig.review(&[]);
    let mission = rig.mission();
    let first = mission.candidate_id.clone().expect("first candidate");

    // Repair cycle: a new writer change integrated alone mints candidate B.
    let (_task, run) = rig.seed_succeeded_task(
        TaskKind::Implement,
        Some(Role::Builder),
        "repair pass",
        &["fix.txt"],
    );
    let fix = writer_candidate(&rig, "w-fix", &[("fix.txt", "repair\n")], &["fix.txt"], run);
    let second = match integrate(
        &rig,
        &[(fix.candidate_id.clone(), fix.source_run_ids.clone())],
    ) {
        MintResult::Integrated { candidate, .. } => candidate,
        MintResult::Conflict { .. } => panic!("single-source repair must integrate cleanly"),
    };
    assert_ne!(second.id, first);
    assert_eq!(rig.mission().candidate_id, Some(second.id.clone()));

    // Old evidence is immutable and still bound to the first candidate.
    let entities = load_entities(&rig.storage, &rig.mission_id).unwrap();
    let old = entities
        .verifications
        .iter()
        .find(|row| row.id == verification.verification.id)
        .expect("old verification kept");
    assert_eq!(old.candidate_id, first);
    assert_eq!(old.status, VerificationStatus::Passed);

    // Accepting the new candidate is rejected: its commands never ran on it.
    let rejected = rig
        .accept(
            &second.id,
            std::slice::from_ref(&verification.verification.id),
        )
        .expect_err("evidence from the old candidate must not carry over");
    assert_eq!(rejected.code, MissionErrorCode::InvalidState);
    assert_eq!(
        rejected.details.reason_code.as_deref(),
        Some("verification_missing")
    );
    assert_eq!(rig.mission().state, MissionState::Running);
}

// ---- pure gate table (step order, 04 §6) -----------------------------------------

fn gate_artifact_ref() -> ArtifactRef {
    ArtifactRef {
        id: Id::generate(),
        sha256: "a".repeat(64),
        bytes: U64String::new(1).unwrap(),
        media_type: "text/plain".into(),
    }
}

fn gate_mission() -> Mission {
    let command_id = Id::generate();
    Mission {
        id: Id::generate(),
        revision: U64String::new(7).unwrap(),
        semantic_revision: None,
        follow_up_of: None,
        base_snapshot: None,
        state: MissionState::Running,
        phase: Phase::AwaitingAcceptance,
        title: "gate".into(),
        repository_path: "/repo".into(),
        repository_id: Id::generate(),
        base_oid: "a".repeat(40),
        goal_ref: gate_artifact_ref(),
        requirements: vec![
            Requirement {
                id: Id::generate(),
                text: "필수 검증".into(),
                verification_ids: vec![command_id.clone()],
                human_check: false,
            },
            Requirement {
                id: Id::generate(),
                text: "수동 확인".into(),
                verification_ids: Vec::new(),
                human_check: true,
            },
        ],
        policy: Policy {
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
            allowed_roles: Vec::new(),
            allowed_verification_ids: vec![command_id],
            require_independent_review: true,
            require_enforced_verification: false,
        },
        role_bindings: Vec::new(),
        plan_revision: 1,
        candidate_id: Some(Id::generate()),
        open_decision_count: 0,
        active_time_ms: U64String::new(0).unwrap(),
        automatic_start_count: 0,
        created_at: "2026-09-13T00:00:00.000Z".into(),
        updated_at: "2026-09-13T00:00:00.000Z".into(),
        archived_at: None,
        accepted_at: None,
        failure_code: None,
    }
}

fn gate_task(mission_id: &Id, kind: TaskKind, state: TaskState, commands: Vec<Id>) -> Task {
    Task {
        id: Id::generate(),
        mission_id: mission_id.clone(),
        title: format!("{kind:?}"),
        kind,
        role: None,
        state,
        required: true,
        parent_task_id: None,
        depends_on: Vec::new(),
        contract: TaskContract {
            objective_ref: gate_artifact_ref(),
            requirement_ids: Vec::new(),
            input_artifact_ids: Vec::new(),
            allowed_paths: Vec::new(),
            expected_outputs: vec![ExpectedOutput::Verification],
            verification_ids: commands,
            specialty: None,
        },
        binding_id: None,
        active_run_id: None,
        ordinal: 1,
        attempt_count: 1,
        repair_cycle: 0,
        failure_repair_run_ids: vec![],
        integration: None,
        replacement_of: None,
        blocked_code: None,
        dispatch_after_unix_ms: None,
        workspace_id: None,
        created_at: "2026-09-13T00:00:00.000Z".into(),
        updated_at: "2026-09-13T00:00:00.000Z".into(),
    }
}

fn gate_run(state: RunState) -> Run {
    Run {
        id: Id::generate(),
        mission_id: Id::generate(),
        task_id: Id::generate(),
        attempt: 1,
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
        dispatch_state: RunDispatchState::Acknowledged,
        context_ref: gate_artifact_ref(),
        result_ref: None,
        usage: unknown_usage(),
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

/// The varying pieces of one gate invocation (everything else is fixed).
struct GateCase<'a> {
    offered_revision: u64,
    offered_candidate: &'a Id,
    tasks: &'a [Task],
    runs: &'a [Run],
    verifications: &'a [Verification],
    decisions: &'a [Decision],
    human: &'a [Id],
}

fn gate_inputs<'a>(mission: &'a Mission, case: &GateCase<'a>) -> AcceptanceInputs<'a> {
    AcceptanceInputs {
        verified_reconciled_run_ids: &[],
        mission,
        offered_revision: case.offered_revision,
        offered_candidate_id: case.offered_candidate,
        tasks: case.tasks,
        runs: case.runs,
        verifications: case.verifications,
        findings: &[],
        decisions: case.decisions,
        acknowledged_verification_ids: &[],
        human_requirement_ids: case.human,
        review_complete: case
            .tasks
            .iter()
            .any(|task| task.kind == TaskKind::Review && task.state == TaskState::Succeeded),
    }
}

#[test]
fn acceptance_gate_rejection_table() {
    let mission = gate_mission();
    let candidate = mission.candidate_id.clone().unwrap();
    let command = mission.requirements[0].verification_ids[0].clone();
    let human_requirement = mission.requirements[1].id.clone();
    let verify_task = gate_task(
        &mission.id,
        TaskKind::Verify,
        TaskState::Succeeded,
        vec![command],
    );
    let verification = Verification {
        id: Id::generate(),
        mission_id: mission.id.clone(),
        candidate_id: candidate.clone(),
        task_id: verify_task.id.clone(),
        run_id: Id::generate(),
        command_snapshot_ref: gate_artifact_ref(),
        environment_ref: gate_artifact_ref(),
        requirement_ids: vec![mission.requirements[0].id.clone()],
        status: VerificationStatus::Passed,
        input_integrity: InputIntegrity::Enforced,
        exit_code: Some(0),
        log_ref: gate_artifact_ref(),
        started_at: "2026-09-13T00:00:00.000Z".into(),
        ended_at: "2026-09-13T00:00:00.000Z".into(),
    };
    let tasks = vec![
        verify_task,
        gate_task(&mission.id, TaskKind::Review, TaskState::Succeeded, vec![]),
        gate_task(
            &mission.id,
            TaskKind::Implement,
            TaskState::Succeeded,
            vec![],
        ),
    ];
    let terminal_runs = [gate_run(RunState::Succeeded)];
    let good = GateCase {
        offered_revision: 7,
        offered_candidate: &candidate,
        tasks: &tasks,
        runs: &terminal_runs,
        verifications: std::slice::from_ref(&verification),
        decisions: &[],
        human: std::slice::from_ref(&human_requirement),
    };

    // All gates green: current revision + candidate, verified and reviewed.
    assert_eq!(acceptance_ready(&gate_inputs(&mission, &good)), Ok(()));

    // A validated replacement retires a required task.
    let mut superseded = tasks.clone();
    superseded[2] = gate_task(
        &mission.id,
        TaskKind::Implement,
        TaskState::Superseded,
        vec![],
    );
    let case = GateCase {
        tasks: &superseded,
        ..good
    };
    assert_eq!(acceptance_ready(&gate_inputs(&mission, &case)), Ok(()));

    // Cancellation never waives required work, but optional cancellation can
    // be omitted without changing the original mission requirements.
    for required in [true, false] {
        let mut cancelled = tasks.clone();
        cancelled[2].state = TaskState::Cancelled;
        cancelled[2].required = required;
        let case = GateCase {
            tasks: &cancelled,
            ..good
        };
        let result = acceptance_ready(&gate_inputs(&mission, &case));
        if required {
            assert!(matches!(
                result,
                Err(Rejection::RequiredTaskNotSucceeded {
                    state: TaskState::Cancelled,
                    ..
                })
            ));
        } else {
            assert_eq!(result, Ok(()));
        }
    }

    // Step 1a: revision mismatch wins over everything else.
    let case = GateCase {
        offered_revision: 8,
        ..good
    };
    assert_eq!(
        acceptance_ready(&gate_inputs(&mission, &case)),
        Err(Rejection::RevisionMismatch {
            current: 7,
            offered: 8
        })
    );

    // Step 1a': an older offer passes only across housekeeping-only commits
    // (time checkpoints/activity), i.e. semantic_revision <= offered.
    let mut housekeeping = mission.clone();
    housekeeping.semantic_revision = Some(U64String::new(5).unwrap());
    for offered_revision in [5, 6, 7] {
        let case = GateCase {
            offered_revision,
            ..good
        };
        assert_eq!(acceptance_ready(&gate_inputs(&housekeeping, &case)), Ok(()));
    }
    let case = GateCase {
        offered_revision: 4,
        ..good
    };
    assert_eq!(
        acceptance_ready(&gate_inputs(&housekeeping, &case)),
        Err(Rejection::RevisionMismatch {
            current: 7,
            offered: 4
        })
    );
    let case = GateCase {
        offered_revision: 6,
        ..good
    };
    assert_eq!(
        acceptance_ready(&gate_inputs(&mission, &case)),
        Err(Rejection::RevisionMismatch {
            current: 7,
            offered: 6
        }),
        "legacy documents without semantic_revision keep exact matching"
    );

    // Step 1b: wrong candidate id (W10).
    let other = Id::generate();
    let case = GateCase {
        offered_candidate: &other,
        ..good
    };
    assert_eq!(
        acceptance_ready(&gate_inputs(&mission, &case)),
        Err(Rejection::CandidateMismatch {
            current: Some(candidate.clone()),
            offered: other,
        })
    );

    // Step 2: a required task that is neither succeeded nor excluded.
    let mut failed = tasks.clone();
    failed[2] = gate_task(&mission.id, TaskKind::Implement, TaskState::Failed, vec![]);
    let case = GateCase {
        tasks: &failed,
        ..good
    };
    assert!(matches!(
        acceptance_ready(&gate_inputs(&mission, &case)),
        Err(Rejection::RequiredTaskNotSucceeded { .. })
    ));

    // Step 5: independent review required but incomplete (the verify task
    // and its command coverage stay intact — only the review is missing).
    let no_review = vec![tasks[0].clone(), tasks[2].clone()];
    let case = GateCase {
        tasks: &no_review,
        ..good
    };
    assert_eq!(
        acceptance_ready(&gate_inputs(&mission, &case)),
        Err(Rejection::ReviewIncomplete)
    );

    // Step 6: human check missing.
    let case = GateCase { human: &[], ..good };
    assert_eq!(
        acceptance_ready(&gate_inputs(&mission, &case)),
        Err(Rejection::HumanCheckMissing {
            requirement_id: human_requirement.clone()
        })
    );

    // Step 7: live writer, unknown run, open blocking decision.
    let live_runs = [gate_run(RunState::Running)];
    let case = GateCase {
        runs: &live_runs,
        ..good
    };
    assert!(matches!(
        acceptance_ready(&gate_inputs(&mission, &case)),
        Err(Rejection::LiveRun { .. })
    ));
    for state in [RunState::Unknown, RunState::Interrupted] {
        for has_termination_ref in [false, true] {
            let mut unresolved = gate_run(state);
            unresolved.reconciliation_ref = has_termination_ref.then(gate_artifact_ref);
            let unknown_runs = [unresolved];
            let case = GateCase {
                runs: &unknown_runs,
                ..good
            };
            assert!(
                matches!(
                    acceptance_ready(&gate_inputs(&mission, &case)),
                    Err(Rejection::UnknownRun { .. })
                ),
                "local termination alone cannot acknowledge unknown effects"
            );
        }
    }
    let (_, blocking_decision) = iyagi_termd_lib::mission::engine::new_decision(
        &mission,
        DecisionKind::Conflict,
        gate_artifact_ref(),
        Vec::new(),
        Vec::new(),
        true,
        None,
    );
    let blocking = [blocking_decision];
    let case = GateCase {
        decisions: &blocking,
        ..good
    };
    assert!(matches!(
        acceptance_ready(&gate_inputs(&mission, &case)),
        Err(Rejection::OpenBlockingDecision { .. })
    ));
}
