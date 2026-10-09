use super::mission_verification_exec::supervisor;
use super::*;
use iyagi_termd_lib::exec::ExecSupervisor;
use std::path::PathBuf;

#[path = "mission_candidate_exclusion.rs"]
mod candidate_exclusion;

fn host(available: u64) -> term_core::AdmissionHost {
    term_core::AdmissionHost {
        total_bytes: 16 << 30,
        available_bytes: Some(available),
        sample_age_ms: 0,
        reconciliation_required: false,
        pressure: term_contracts::metrics::PressureLevel::Normal,
    }
}
fn actor(
    rig: &Rig,
    available: u64,
) -> (tokio::runtime::Runtime, Arc<ExecSupervisor>, MissionActor) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let supervisor = supervisor(rig, &rig.service);
    supervisor.refresh_recovery().unwrap();
    supervisor.update_host(host(available));
    let actor = rig
        .actor(factory(Arc::new(Mutex::new(vec![]))))
        .with_deterministic_exec(supervisor.clone(), runtime.handle().clone());
    (runtime, supervisor, actor)
}
fn internal_run(snapshot: &workflow::MissionEntities) -> Option<&Run> {
    snapshot
        .tasks
        .iter()
        .find(|t| t.is_internal_integration())
        .and_then(|task| {
            snapshot
                .runs
                .iter()
                .filter(|r| r.task_id == task.id)
                .max_by_key(|r| r.attempt)
        })
}
fn body(rig: &Rig, reference: &ArtifactRef) -> Value {
    serde_json::from_slice(
        &ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"))
            .read_mission_body(&rig.id, reference, 256 * 1024)
            .unwrap(),
    )
    .unwrap()
}

fn resolving_factory(mode: &'static str) -> AdapterFactory {
    let shared = if mode == "parent_symlink" {
        "src/shared.txt"
    } else {
        "shared.txt"
    };
    Arc::new(move |run| {
        let ctx = context(run);
        let result = match ctx["task_kind"].as_str().unwrap() {
            "plan" => ProviderResult::Plan {
                based_on_plan_revision: 0,
                tasks: ["a", "b", "c"].iter().map(|key| serde_json::from_value(json!({
                    "local_key": key, "title": key, "kind": "implement", "role": "builder", "required": true,
                    "parent_key": null, "depends_on_keys": [], "objective_text": "Create the assigned file",
                    "requirement_ids": [ctx["requirements"][0]["id"]], "input_artifact_ids": [],
                    "allowed_paths": [if *key == "c" && mode != "multiple" { "tail.txt" } else { shared }],
                    "expected_outputs": ["patch"], "verification_ids": [], "specialty": null,
                    "binding_id": null, "replacement_of": null } )).unwrap()).collect(),
                retire_task_ids: vec![], rationale_text: "Conflicting writers followed by another source".into(),
            },
            "implement" => {
                let name = ctx["task_contract"]["allowed_paths"][0].as_str().unwrap();
                std::fs::create_dir_all(run.workspace.as_ref().unwrap().join(name).parent().unwrap()).unwrap();
                std::fs::write(run.workspace.as_ref().unwrap().join(name), ctx["task_id"].as_str().unwrap()).unwrap();
                ProviderResult::Patch { report_text: "Writer result".into(), verification_claims: vec![] }
            }
            "integrate" => {
                assert_eq!(ctx["role"], "integrator");
                let objective: Value = serde_json::from_str(ctx["objective"].as_str().unwrap()).unwrap();
                assert!(objective["applied_count"] == 1 || (mode == "multiple" && objective["applied_count"] == 2));
                assert_eq!(objective["input_sources"].as_array().unwrap().len(), 3);
                let path = run.workspace.as_ref().unwrap();
                assert!(!path.join("tail.txt").exists(), "remaining source is applied after resolution");
                assert!(std::fs::read_to_string(path.join(shared)).unwrap().contains("<<<<<<<"));
                if mode != "keep_markers" { std::fs::write(path.join(shared), "resolved together\n").unwrap(); }
                #[cfg(unix)]
                if mode == "parent_symlink" {
                    let outside = path.parent().unwrap().join("external-resolution");
                    std::fs::create_dir(&outside).unwrap();
                    std::fs::write(outside.join("shared.txt"), "outside contents\n").unwrap();
                    std::fs::remove_dir_all(path.join("src")).unwrap();
                    std::os::unix::fs::symlink(outside, path.join("src")).unwrap();
                }
                if mode == "out_of_scope" { std::fs::write(path.join("base.txt"), "forbidden change\n").unwrap(); }
                ProviderResult::Patch { report_text: "Resolution report requiring host validation".into(), verification_claims: vec![] }
            }
            "review" => {
                let path = run.workspace.as_ref().unwrap();
                assert_eq!(std::fs::read_to_string(path.join("shared.txt")).unwrap(), "resolved together\n");
                if mode != "multiple" { assert!(path.join("tail.txt").is_file()); }
                ProviderResult::Review { candidate_id: serde_json::from_value(ctx["candidate"]["id"].clone()).unwrap(),
                    report_text: "Inspected resolved and remaining changes".into(), findings: vec![] }
            }
            _ => panic!("unexpected task"),
        };
        Ok(scripted(script(result)))
    })
}

#[test]
fn conflict_resolution_reuses_task_and_workspace_then_verifies_and_accepts() {
    let rig = Rig::new(
        true,
        &["ls-files", "--error-unmatch", "shared.txt", "tail.txt"],
    );
    let (runtime, supervisor, _) = actor(&rig, 12 << 30);
    let mut actor = rig
        .actor(resolving_factory("resolve"))
        .with_deterministic_exec(supervisor.clone(), runtime.handle().clone());
    rig.tick_until(&mut actor, |s| {
        s.decisions
            .iter()
            .any(|d| d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)
    });
    let before = rig.snapshot();
    let failed = internal_run(&before).unwrap().clone();
    let task = before
        .tasks
        .iter()
        .find(|t| t.id == failed.task_id)
        .unwrap();
    assert_eq!(task.role, Some(Role::Integrator));
    assert!(task.binding_id.is_some() && failed.binding_snapshot.is_none());
    let mut binding: Binding =
        serde_json::from_value(rig.storage.mission_bindings().unwrap().remove(0)).unwrap();
    binding.id = Id::generate();
    binding.revision = term_contracts::ids::U64String::new(0).unwrap();
    binding.label = "Explicit conflict integrator".into();
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id": Id::generate(), "expected_revision": "0", "binding": binding}),
    );
    let mut policy = before.mission.policy.clone();
    policy.allowed_binding_ids.push(binding.id.clone());
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id": Id::generate(),
        "mission_id": rig.id, "expected_revision": before.mission.revision, "policy": policy,
        "role_bindings": before.mission.role_bindings}),
    );
    let reassign = json!({"request_id": Id::generate(), "mission_id": rig.id,
        "expected_revision": rig.snapshot().mission.revision, "task_id": task.id,
        "action": "reassign", "binding_id": binding.id});
    let assigned = rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        reassign.clone(),
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.task.control", reassign),
        assigned
    );
    assert_eq!(internal_run(&rig.snapshot()).unwrap(), &failed);
    assert_eq!(
        rig.snapshot()
            .tasks
            .iter()
            .find(|t| t.id == task.id)
            .unwrap()
            .state,
        TaskState::Failed
    );
    assert_eq!(
        rig.snapshot()
            .decisions
            .iter()
            .filter(|d| d.state == DecisionState::Open)
            .count(),
        1
    );
    control(&rig, "pause");
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Paused);
    let paused = rig.snapshot();
    let decision = paused
        .decisions
        .iter()
        .find(|d| d.kind == DecisionKind::Conflict)
        .unwrap();
    let params = json!({"request_id": Id::generate(), "mission_id": rig.id,
        "expected_revision": paused.mission.revision, "decision_id": decision.id,
        "option_id": "resolve_and_reintegrate", "answer_ref": null});
    fault(&rig, "CREATE TRIGGER hold_resolution BEFORE UPDATE ON orch_tasks WHEN json_extract(NEW.document_json, '$.integration.step.resolving') IS NOT NULL BEGIN SELECT RAISE(ABORT, 'decision outage'); END;");
    assert!(rig
        .service
        .handle(&rig.conn, "mission.decision.answer", &params)
        .is_err());
    assert_eq!(rig.snapshot().mission.revision, paused.mission.revision);
    assert_eq!(
        rig.snapshot()
            .decisions
            .iter()
            .find(|d| d.id == decision.id)
            .unwrap()
            .state,
        DecisionState::Open
    );
    fault(&rig, "DROP TRIGGER hold_resolution;");
    let answer = rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        params.clone(),
    );
    assert_eq!(
        rpc(
            &rig.service,
            &rig.conn,
            "mission.decision.answer",
            params.clone()
        ),
        answer
    );
    for _ in 0..3 {
        actor.tick().unwrap();
    }
    assert_eq!(internal_run(&rig.snapshot()).unwrap().id, failed.id);
    control(&rig, "resume");
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let final_state = rig.snapshot();
    let mut runs: Vec<_> = final_state
        .runs
        .iter()
        .filter(|r| r.task_id == failed.task_id)
        .collect();
    runs.sort_by_key(|r| r.attempt);
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[0], &failed);
    assert_eq!(
        runs[1].binding_snapshot.as_ref().map(|b| &b.id),
        Some(&binding.id)
    );
    assert_eq!(runs[1].state, RunState::Succeeded);
    assert_eq!(runs[2].state, RunState::Succeeded);
    assert!(runs[2].binding_snapshot.is_none());
    assert!(runs.iter().all(|r| r.workspace_id == failed.workspace_id));
    assert_eq!(
        final_state
            .workspaces
            .iter()
            .filter(|w| w.kind == WorkspaceKind::Integration)
            .count(),
        1
    );
    let candidate = final_state
        .candidates
        .iter()
        .find(|c| Some(&c.id) == final_state.mission.candidate_id.as_ref())
        .unwrap();
    assert!(candidate.source_run_ids.contains(&runs[1].id));
    assert_eq!(
        body(&rig, &candidate.manifest_ref)["resolution_run_ids"],
        json!([runs[1].id])
    );
    assert_eq!(candidate.source_run_ids.len(), 4);
    assert_eq!(
        final_state.execs.len(),
        3,
        "two integration helpers and one verifier"
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.decision.answer", params),
        answer
    );
    rpc(
        &rig.service,
        &rig.conn,
        "mission.accept",
        json!({"request_id": Id::generate(), "mission_id": rig.id,
        "expected_revision": final_state.mission.revision, "candidate_id": candidate.id,
        "acknowledged_verification_ids": [final_state.verifications[0].id], "human_requirement_ids": []}),
    );
    assert_eq!(rig.snapshot().mission.state, MissionState::Completed);
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
    assert_eq!(git(rig.repo.path(), &["status", "--porcelain"]), "");
    assert_eq!(supervisor.ledger().active_count(), 0);
    actor.shutdown();
}

#[test]
fn resolver_report_cannot_publish_unresolved_or_out_of_scope_files() {
    for mode in ["keep_markers", "out_of_scope", "parent_symlink"] {
        if mode == "parent_symlink" && !cfg!(unix) {
            continue;
        }
        let rig = Rig::new(true, &["status", "--porcelain"]);
        let (runtime, supervisor, _) = actor(&rig, 12 << 30);
        let mut actor = rig
            .actor(resolving_factory(mode))
            .with_deterministic_exec(supervisor.clone(), runtime.handle().clone());
        rig.tick_until(&mut actor, |s| {
            s.decisions
                .iter()
                .any(|d| d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)
        });
        let snapshot = rig.snapshot();
        let decision = snapshot
            .decisions
            .iter()
            .find(|d| d.kind == DecisionKind::Conflict)
            .unwrap();
        rpc(
            &rig.service,
            &rig.conn,
            "mission.decision.answer",
            json!({"request_id": Id::generate(),
            "mission_id": rig.id, "expected_revision": snapshot.mission.revision,
            "decision_id": decision.id, "option_id": "resolve_and_reintegrate", "answer_ref": null}),
        );
        rig.tick_until(&mut actor, |s| {
            internal_run(s).is_some_and(|r| r.attempt == 3 && r.state == RunState::Failed)
        });
        assert!(rig.snapshot().mission.candidate_id.is_none());
        if mode == "parent_symlink" {
            let snapshot = rig.snapshot();
            let failed = internal_run(&snapshot).unwrap();
            let workspace = snapshot
                .workspaces
                .iter()
                .find(|w| Some(&w.id) == failed.workspace_id.as_ref())
                .unwrap();
            let outside = Path::new(&workspace.path)
                .parent()
                .unwrap()
                .join("external-resolution/shared.txt");
            assert_eq!(
                std::fs::read_to_string(outside).unwrap(),
                "outside contents\n"
            );
            let evidence = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"))
                .read_mission_body(&rig.id, failed.result_ref.as_ref().unwrap(), 256 * 1024)
                .unwrap();
            assert!(std::str::from_utf8(&evidence)
                .unwrap()
                .contains("cannot follow a symlink"));
        }
        assert_eq!(supervisor.ledger().active_count(), 0);
        assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
        actor.shutdown();
    }
}

#[test]
fn later_conflicts_keep_prior_resolutions_and_reuse_the_same_exclusive_workspace() {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "shared.txt"]);
    let before = rig.snapshot();
    let mut policy = before.mission.policy.clone();
    policy.max_attempts_per_task = 5;
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id": Id::generate(),
        "mission_id": rig.id, "expected_revision": before.mission.revision, "policy": policy,
        "role_bindings": before.mission.role_bindings}),
    );
    let (runtime, supervisor, _) = actor(&rig, 12 << 30);
    let mut actor = rig
        .actor(resolving_factory("multiple"))
        .with_deterministic_exec(supervisor.clone(), runtime.handle().clone());
    let mut failed = Vec::new();
    for _ in 0..2 {
        rig.tick_until(&mut actor, |s| {
            s.decisions
                .iter()
                .any(|d| d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)
        });
        let snapshot = rig.snapshot();
        failed.push(internal_run(&snapshot).unwrap().clone());
        let decision = snapshot
            .decisions
            .iter()
            .find(|d| d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)
            .unwrap();
        rpc(
            &rig.service,
            &rig.conn,
            "mission.decision.answer",
            json!({"request_id": Id::generate(),
            "mission_id": rig.id, "expected_revision": snapshot.mission.revision,
            "decision_id": decision.id, "option_id": "resolve_and_reintegrate", "answer_ref": null}),
        );
    }
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    let mut runs: Vec<_> = snapshot
        .runs
        .iter()
        .filter(|r| r.task_id == failed[0].task_id)
        .collect();
    runs.sort_by_key(|r| r.attempt);
    assert_eq!(runs.len(), 5);
    assert_eq!(runs[0], &failed[0]);
    assert_eq!(runs[2], &failed[1]);
    assert!(runs
        .iter()
        .all(|r| r.workspace_id == failed[0].workspace_id));
    let candidate = snapshot
        .candidates
        .iter()
        .find(|c| Some(&c.id) == snapshot.mission.candidate_id.as_ref())
        .unwrap();
    assert_eq!(
        body(&rig, &candidate.manifest_ref)["resolution_run_ids"],
        json!([runs[1].id, runs[3].id])
    );
    assert_eq!(candidate.source_run_ids.len(), 5);
    assert_eq!(snapshot.mission.open_decision_count, 0);
    assert_eq!(supervisor.ledger().active_count(), 0);
    actor.shutdown();
}
fn fault(rig: &Rig, sql: &str) {
    rusqlite::Connection::open(rig.dir.path().join("state.db"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}
fn control(rig: &Rig, action: &str) {
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id": Id::generate(),
        "mission_id": rig.id, "expected_revision": rig.snapshot().mission.revision, "action": action}),
    );
}
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}
fn install_barrier(rig: &Rig, worktree: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let marker = rig.dir.path().join("integration-hook-started");
    let release = rig.dir.path().join("release-integration-hook");
    let hook = rig.repo.path().join(".git/hooks/post-checkout");
    let canonical = worktree
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap()
        .join(worktree.file_name().unwrap());
    let text = format!("#!/bin/sh\n[ \"$(pwd -P)\" = {} ] || exit 0\nprintf x >> {}\ncount=0\nwhile [ ! -f {} ]; do count=$((count + 1)); [ \"$count\" -lt 300 ] || exit 1; sleep 0.05; done\n",
        quote(&canonical), quote(&marker), quote(&release));
    std::fs::write(&hook, text).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    (marker, release)
}
fn prepared(rig: &Rig, actor: &mut MissionActor) -> (Run, Workspace) {
    rig.tick_until(actor, |s| {
        internal_run(s).is_some_and(|r| r.state == RunState::Starting)
    });
    let snapshot = rig.snapshot();
    let run = internal_run(&snapshot).unwrap().clone();
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|w| Some(&w.id) == run.workspace_id.as_ref())
        .unwrap()
        .clone();
    assert!(
        !Path::new(&workspace.path).exists(),
        "admission denial must precede every integration Git operation"
    );
    assert!(run.exec_id.is_none());
    (run, workspace)
}

#[test]
fn integration_links_one_durable_exec_frozen_input_and_retained_workspace() {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let (_runtime, supervisor, mut actor) = actor(&rig, 12 << 30);
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    let run = internal_run(&snapshot).unwrap();
    let exec = snapshot
        .execs
        .iter()
        .find(|e| Some(&e.id) == run.exec_id.as_ref())
        .unwrap();
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|w| Some(&w.id) == run.workspace_id.as_ref())
        .unwrap();
    assert_eq!(run.state, RunState::Succeeded);
    assert!(run.binding_snapshot.is_none());
    assert_eq!(exec.state, ExecState::Exited);
    assert!(exec.identity.is_some() && exec.group_identity.is_some() && exec.ended_at.is_some());
    assert_eq!(
        snapshot.execs.iter().filter(|e| e.run_id == run.id).count(),
        1
    );
    assert_eq!(snapshot.execs.len(), 2, "one integration and one verifier");
    assert_eq!(workspace.state, WorkspaceState::Retained);
    assert!(workspace.writer_run_id.is_none());
    let context = body(&rig, &run.context_ref);
    let launch = body(&rig, &exec.launch_manifest_ref);
    assert_eq!(context["kind"], "integration_exec");
    assert_eq!(context["program"], launch["program"]);
    assert_eq!(launch["argv"][0], "--integration-helper");
    assert_eq!(launch["argv"][2], context["input_ref"]["sha256"]);
    let result = body(&rig, run.result_ref.as_ref().unwrap());
    assert_eq!(result["input_sha256"], context["input_ref"]["sha256"]);
    assert_eq!(
        result["result"]["Ok"]["outcome"]["sources"],
        context["input"]["plan"]["sources"]
    );
    assert_eq!(supervisor.ledger().active_count(), 0);
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
    actor.shutdown();
}

#[test]
fn integration_conflict_finishes_the_exec_and_waits_for_one_conflict_decision() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let supervisor = supervisor(&rig, &rig.service);
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
                retire_task_ids: vec![], rationale_text: "Exercise independent conflicting writers".into(),
            },
            "implement" => {
                std::fs::write(run.workspace.as_ref().unwrap().join("shared.txt"), ctx["task_id"].as_str().unwrap()).unwrap();
                ProviderResult::Patch { report_text: "Captured writer result".into(), verification_claims: vec![] }
            },
            _ => panic!("a conflict must stop review dispatch"),
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
    let run = internal_run(&snapshot).unwrap();
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(snapshot.execs[0].state, ExecState::Exited);
    assert!(snapshot.mission.candidate_id.is_none());
    assert_eq!(snapshot.mission.state, MissionState::Running);
    assert_eq!(
        snapshot.decisions.len(),
        1,
        "no duplicate generic retry decision"
    );
    let decision = &snapshot.decisions[0];
    assert_eq!(decision.requesting_run_id.as_ref(), Some(&run.id));
    assert!(decision.blocking);
    assert!(body(&rig, run.result_ref.as_ref().unwrap())["result"]["Ok"]["integrated"].is_null());
    assert_eq!(supervisor.ledger().active_count(), 0);
    assert!(rig
        .service
        .handle(
            &rig.conn,
            "mission.task.control",
            &json!({"request_id": Id::generate(),
        "mission_id": rig.id,"expected_revision":snapshot.mission.revision,"task_id":run.task_id,
        "action":"retry","binding_id":null})
        )
        .is_err());
    actor.shutdown();
}

#[test]
fn admission_wait_keeps_one_integration_attempt_and_cancel_never_creates_worktree() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let (_runtime, supervisor, mut actor) = actor(&rig, 0);
    let (run, workspace) = prepared(&rig, &mut actor);
    let before = rig.snapshot().mission;
    let rejected = rig.service.handle(&rig.conn, "mission.message", &json!({
        "request_id": Id::generate(), "mission_id": rig.id, "expected_revision": before.revision,
        "target_task_id": run.task_id, "body_ref": before.goal_ref,
    })).err().expect("deterministic integration cannot receive model instructions");
    assert_eq!(
        rejected.code,
        term_contracts::mission::MissionErrorCode::InvalidState
    );
    assert_eq!(rig.snapshot().mission.revision, before.revision);
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(internal_run(&rig.snapshot()).unwrap().id, run.id);
    assert_eq!(supervisor.ledger().active_count(), 0);
    control(&rig, "cancel");
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Cancelled);
    assert!(!Path::new(&workspace.path).exists());
    assert!(rig.snapshot().execs.is_empty());
    assert_eq!(
        internal_run(&rig.snapshot()).unwrap().state,
        RunState::Cancelled
    );
    actor.shutdown();
}

#[test]
fn rejected_resolution_retries_the_integrator_before_another_capture() {
    let rig = Rig::new(
        true,
        &["ls-files", "--error-unmatch", "shared.txt", "tail.txt"],
    );
    let before = rig.snapshot();
    let mut policy = before.mission.policy.clone();
    policy.max_attempts_per_task = 5;
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id": Id::generate(),
        "mission_id": rig.id, "expected_revision": before.mission.revision, "policy": policy,
        "role_bindings": before.mission.role_bindings}),
    );
    let (runtime, supervisor, _) = actor(&rig, 12 << 30);
    let mut first = rig
        .actor(resolving_factory("keep_markers"))
        .with_deterministic_exec(supervisor.clone(), runtime.handle().clone());
    rig.tick_until(&mut first, |s| {
        s.decisions
            .iter()
            .any(|d| d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)
    });
    let before = rig.snapshot();
    let decision = before
        .decisions
        .iter()
        .find(|d| d.kind == DecisionKind::Conflict)
        .unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        json!({"request_id": Id::generate(),
        "mission_id": rig.id, "expected_revision": before.mission.revision,
        "decision_id": decision.id, "option_id": "resolve_and_reintegrate", "answer_ref": null}),
    );
    rig.tick_until(&mut first, |s| {
        internal_run(s).is_some_and(|r| r.attempt == 3 && r.state == RunState::Failed)
    });
    first.shutdown();
    let failed = internal_run(&rig.snapshot()).unwrap().clone();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        json!({"request_id": Id::generate(),
        "mission_id": rig.id, "expected_revision": rig.snapshot().mission.revision,
        "task_id": failed.task_id, "action": "retry", "binding_id": null}),
    );
    assert!(rig
        .snapshot()
        .tasks
        .iter()
        .find(|t| t.id == failed.task_id)
        .unwrap()
        .is_resolving_integration());
    let mut retry = rig
        .actor(resolving_factory("resolve"))
        .with_deterministic_exec(supervisor.clone(), runtime.handle().clone());
    rig.tick_until(&mut retry, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    let run = snapshot
        .runs
        .iter()
        .find(|r| r.task_id == failed.task_id && r.attempt == 4)
        .unwrap();
    assert!(run.binding_snapshot.is_some());
    assert_eq!(run.workspace_id, failed.workspace_id);
    assert_eq!(
        snapshot.runs.iter().find(|r| r.id == failed.id),
        Some(&failed)
    );
    let candidate = snapshot
        .candidates
        .iter()
        .find(|c| Some(&c.id) == snapshot.mission.candidate_id.as_ref())
        .unwrap();
    assert_eq!(
        body(&rig, &candidate.manifest_ref)["resolution_run_ids"],
        json!([run.id])
    );
    assert_eq!(supervisor.ledger().active_count(), 0);
    retry.shutdown();
}

#[test]
fn cancelled_integration_can_retry_explicitly_with_a_new_run_and_workspace() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let (_runtime, supervisor, mut actor) = actor(&rig, 0);
    let (old_run, workspace) = prepared(&rig, &mut actor);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        json!({"request_id": Id::generate(),
        "mission_id": rig.id, "expected_revision": rig.snapshot().mission.revision, "task_id": old_run.task_id,
        "action": "cancel", "binding_id": null}),
    );
    rig.tick_until(&mut actor, |s| {
        internal_run(s).is_some_and(|r| r.state == RunState::Cancelled)
            && s.decisions.iter().any(|d| {
                d.state == DecisionState::Open
                    && d.options.iter().any(|o| o.id == "retry_failed_task")
            })
    });
    let cancelled = internal_run(&rig.snapshot()).unwrap().clone();
    assert!(!Path::new(&workspace.path).exists());
    assert!(matches!(
        cancelled.retry_evidence,
        Some(RetryEvidence::RequestNotSubmitted { .. })
    ));
    assert!(cancelled.exec_id.is_none());
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        json!({"request_id": Id::generate(),
        "mission_id": rig.id, "expected_revision": rig.snapshot().mission.revision, "task_id": old_run.task_id,
        "action": "retry", "binding_id": null}),
    );
    supervisor.update_host(host(12 << 30));
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    assert_eq!(
        snapshot.runs.iter().find(|r| r.id == old_run.id),
        Some(&cancelled)
    );
    let next = internal_run(&snapshot).unwrap();
    assert_eq!(next.attempt, 2);
    assert_ne!(next.id, old_run.id);
    assert_ne!(next.workspace_id, old_run.workspace_id);
    actor.shutdown();
}

#[test]
fn changed_helper_input_is_rejected_before_any_git_operation() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let (_runtime, supervisor, mut actor) = actor(&rig, 0);
    let (run, workspace) = prepared(&rig, &mut actor);
    let launch = body(&rig, &run.context_ref);
    let path = Path::new(launch["input_path"].as_str().unwrap());
    std::fs::write(path, b"{}").unwrap();
    supervisor.update_host(host(12 << 30));
    rig.tick_until(&mut actor, |s| {
        internal_run(s).is_some_and(|r| r.state == RunState::Failed)
    });
    assert!(!Path::new(&workspace.path).exists());
    assert!(rig.snapshot().mission.candidate_id.is_none());
    assert_eq!(supervisor.ledger().active_count(), 0);
    actor.shutdown();
}

#[test]
fn integration_timeout_stops_git_children_and_retains_the_failed_workspace() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let snapshot = rig.snapshot();
    let mut policy = snapshot.mission.policy.clone();
    // Allow the native helper and Git checkout to reach the hook under test
    // contention. Its 15-second barrier still outlives this execution budget.
    policy.run_time_limit_ms = term_contracts::ids::U64String::new(3_000).unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id": Id::generate(),
        "mission_id": rig.id, "expected_revision": snapshot.mission.revision,
        "policy": policy, "role_bindings": snapshot.mission.role_bindings}),
    );
    let (_runtime, supervisor, mut actor) = actor(&rig, 0);
    let (_, workspace) = prepared(&rig, &mut actor);
    let (marker, _) = install_barrier(&rig, Path::new(&workspace.path));
    supervisor.update_host(host(12 << 30));
    rig.tick_until(&mut actor, |s| {
        internal_run(s).is_some_and(|r| r.state == RunState::Failed)
    });
    let snapshot = rig.snapshot();
    assert_eq!(std::fs::read(marker).unwrap(), b"x");
    assert_eq!(
        internal_run(&snapshot).unwrap().failure_code,
        Some(term_contracts::mission::MissionErrorCode::BudgetExceeded)
    );
    assert_eq!(snapshot.execs[0].state, ExecState::Exited);
    assert!(snapshot.mission.candidate_id.is_none());
    assert_eq!(
        snapshot
            .workspaces
            .iter()
            .find(|w| w.id == workspace.id)
            .unwrap()
            .state,
        WorkspaceState::Retained
    );
    assert_eq!(supervisor.ledger().active_count(), 0);
    actor.shutdown();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn recovered_integration_rejects_changed_contract_without_signalling_the_process() {
    use iyagi_termd_lib::exec::persistence::RecoveredAction;
    if cfg!(target_os = "linux")
        && term_platform::group::select_backend()
            .capabilities()
            .memory_limit_kind
            .support
            != term_contracts::snapshot::LimitSupport::Supported
    {
        assert_ne!(
            std::env::var("IYAGI_CGROUP_REQUIRE_DELEGATION").as_deref(),
            Ok("1")
        );
        return;
    }
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let (_runtime, supervisor, mut actor) = actor(&rig, 0);
    let (_, workspace) = prepared(&rig, &mut actor);
    let (marker, _) = install_barrier(&rig, Path::new(&workspace.path));
    supervisor.update_host(host(12 << 30));
    rig.tick_until(&mut actor, |_| marker.exists());
    let snapshot = rig.snapshot();
    let run = internal_run(&snapshot).unwrap().clone();
    let exec = snapshot
        .execs
        .iter()
        .find(|e| Some(&e.id) == run.exec_id.as_ref())
        .unwrap()
        .clone();
    let original = body(&rig, &run.context_ref);
    let artifacts = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
    let reopened = Arc::new(MissionService::new(
        rig.storage.clone(),
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
    ));
    assert_eq!(
        reopened.exec_persistence().recovered_action(&exec).unwrap(),
        RecoveredAction::Observe
    );
    for (pointer, value) in [
        ("/program", json!("/bin/true")),
        ("/input_path", json!("/tmp/another-input")),
        ("/input/run_id", json!(Id::generate())),
        ("/input/workspace_id", json!(Id::generate())),
        ("/input/plan/sources/0/commit_oid", json!(rig.base)),
        ("/resource_policy/cpu_slots", json!(2)),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let mut changed_run = run.clone();
        changed_run.context_ref = workflow::store_artifact(
            &artifacts,
            &rig.id,
            "application/json",
            &serde_json::to_vec(&changed).unwrap(),
        )
        .unwrap();
        workflow::commit_upserts(
            &rig.service,
            rig.snapshot().mission,
            "fixture.integration_contract",
            "change",
            MissionEventType::Changed,
            vec![Entity::Run(Box::new(changed_run))],
        )
        .unwrap();
        assert!(
            reopened.exec_persistence().recovered_action(&exec).is_err(),
            "{pointer}"
        );
        let identity = exec.identity.as_ref().unwrap();
        assert_eq!(
            term_platform::identity::process_identity(identity.pid).as_ref(),
            Some(identity)
        );
    }
    workflow::commit_upserts(
        &rig.service,
        rig.snapshot().mission,
        "fixture.integration_contract",
        "restore",
        MissionEventType::Changed,
        vec![Entity::Run(Box::new(run))],
    )
    .unwrap();
    assert_eq!(
        reopened.exec_persistence().recovered_action(&exec).unwrap(),
        RecoveredAction::Observe
    );
    actor.shutdown();
    assert_eq!(supervisor.ledger().active_count(), 0);
    assert!(!internal_run(&rig.snapshot())
        .unwrap()
        .holds_execution_slot());
}

#[test]
fn cancel_during_git_worktree_creation_waits_for_owned_group_and_never_publishes() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let (_runtime, supervisor, mut actor) = actor(&rig, 0);
    let (run, workspace) = prepared(&rig, &mut actor);
    let (marker, _) = install_barrier(&rig, Path::new(&workspace.path));
    supervisor.update_host(host(12 << 30));
    rig.tick_until(&mut actor, |_| marker.exists());
    assert_eq!(supervisor.ledger().active_count(), 1);
    assert!(rig.snapshot().mission.candidate_id.is_none());
    control(&rig, "cancel");
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Cancelled);
    let snapshot = rig.snapshot();
    assert_eq!(internal_run(&snapshot).unwrap().id, run.id);
    assert_eq!(internal_run(&snapshot).unwrap().state, RunState::Cancelled);
    assert_eq!(snapshot.execs[0].state, ExecState::Exited);
    assert!(snapshot.execs[0].ended_at.is_some());
    assert!(snapshot.candidates.iter().all(|c| c.revision == 0));
    assert!(Path::new(&workspace.path).exists());
    assert_eq!(std::fs::read(&marker).unwrap(), b"x");
    assert_eq!(supervisor.ledger().active_count(), 0);
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
    actor.shutdown();
}

#[test]
fn pause_drains_integration_before_settling_and_defers_verification_until_resume() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let (_runtime, supervisor, mut actor) = actor(&rig, 0);
    let (run, workspace) = prepared(&rig, &mut actor);
    let (marker, release) = install_barrier(&rig, Path::new(&workspace.path));
    supervisor.update_host(host(12 << 30));
    rig.tick_until(&mut actor, |_| marker.exists());
    control(&rig, "pause");
    for _ in 0..4 {
        actor.tick().unwrap();
        assert_eq!(rig.snapshot().mission.state, MissionState::Pausing);
        assert_eq!(supervisor.ledger().active_count(), 1);
    }
    std::fs::write(release, b"release").unwrap();
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Paused);
    let snapshot = rig.snapshot();
    assert_eq!(internal_run(&snapshot).unwrap().id, run.id);
    assert_eq!(internal_run(&snapshot).unwrap().state, RunState::Succeeded);
    assert!(snapshot.mission.candidate_id.is_some());
    assert_eq!(snapshot.execs.len(), 1, "verification waits for resume");
    assert_eq!(snapshot.execs[0].state, ExecState::Exited);
    assert_eq!(actor.live_count(), 0);
    assert_eq!(supervisor.ledger().active_count(), 0);
    control(&rig, "resume");
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    assert_eq!(rig.snapshot().execs.len(), 2);
    assert_eq!(std::fs::read(marker).unwrap(), b"x");
    actor.shutdown();
}

#[test]
fn failed_exec_exit_commit_holds_integration_lease_and_result_until_retry() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let (_runtime, supervisor, mut actor) = actor(&rig, 0);
    let (_, workspace) = prepared(&rig, &mut actor);
    let (marker, release) = install_barrier(&rig, Path::new(&workspace.path));
    fault(&rig, "CREATE TRIGGER hold_integration_exit BEFORE UPDATE ON orch_execs WHEN NEW.state = 'exited' BEGIN SELECT RAISE(ABORT, 'exit outage'); END;");
    std::fs::write(release, b"release").unwrap();
    supervisor.update_host(host(12 << 30));
    rig.tick_until(&mut actor, |_| marker.exists());
    std::thread::sleep(Duration::from_millis(250));
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    let pending = rig.snapshot();
    assert!(pending.mission.candidate_id.is_none());
    assert_eq!(pending.execs[0].state, ExecState::Spawned);
    assert!(internal_run(&pending).unwrap().holds_execution_slot());
    assert_eq!(supervisor.ledger().active_count(), 1);
    fault(&rig, "DROP TRIGGER hold_integration_exit;");
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    assert_eq!(std::fs::read(marker).unwrap(), b"x");
    assert_eq!(rig.snapshot().execs.len(), 2);
    actor.shutdown();
}

#[test]
fn candidate_commit_outage_retries_publication_without_rerunning_git_or_losing_cancel() {
    for cancel in [false, true] {
        let rig = Rig::new(true, &["status", "--porcelain"]);
        let (_runtime, supervisor, mut actor) = actor(&rig, 0);
        let (_, workspace) = prepared(&rig, &mut actor);
        let (marker, release) = install_barrier(&rig, Path::new(&workspace.path));
        fault(&rig, "CREATE TRIGGER hold_integrated_candidate BEFORE INSERT ON orch_entities WHEN NEW.kind = 'candidate' AND json_extract(NEW.document_json, '$.revision') > 0 BEGIN SELECT RAISE(ABORT, 'candidate outage'); END;");
        std::fs::write(release, b"release").unwrap();
        supervisor.update_host(host(12 << 30));
        rig.tick_until(&mut actor, |s| {
            s.execs.iter().any(|e| e.state == ExecState::Exited)
        });
        for _ in 0..5 {
            actor.tick().unwrap();
        }
        let pending = rig.snapshot();
        assert!(pending.mission.candidate_id.is_none());
        assert_eq!(
            pending
                .workspaces
                .iter()
                .find(|w| w.id == workspace.id)
                .unwrap()
                .state,
            WorkspaceState::Busy
        );
        assert!(internal_run(&pending).unwrap().holds_execution_slot());
        if cancel {
            control(&rig, "cancel");
        }
        fault(&rig, "DROP TRIGGER hold_integrated_candidate;");
        rig.tick_until(&mut actor, |s| {
            if cancel {
                s.mission.state == MissionState::Cancelled
            } else {
                s.mission.phase == Phase::AwaitingAcceptance
            }
        });
        assert_eq!(std::fs::read(marker).unwrap(), b"x");
        assert_eq!(internal_run(&rig.snapshot()).unwrap().attempt, 1);
        if cancel {
            assert!(rig.snapshot().mission.candidate_id.is_none());
        }
        actor.shutdown();
    }
}
