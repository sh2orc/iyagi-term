use super::*;
use iyagi_termd_lib::exec::{gated::GateConfig, ExecSupervisor};

pub(super) fn supervisor(rig: &Rig, service: &Arc<MissionService>) -> Arc<ExecSupervisor> {
    Arc::new(ExecSupervisor::persistent(
        term_core::AdmissionConfig {
            logical_cpus: 8,
            managed_concurrency: 2,
            telemetry_stale_ms: 3000,
            host_reserve_min_bytes: 2 << 30,
            host_reserve_percent: 15,
            managed_budget_percent: 50,
        },
        service.exec_persistence(),
        term_core::AdmissionHost {
            total_bytes: 16 << 30,
            available_bytes: Some(12 << 30),
            sample_age_ms: 0,
            reconciliation_required: false,
            pressure: term_contracts::metrics::PressureLevel::Normal,
        },
        GateConfig {
            helper_program: env!("CARGO_BIN_EXE_iyagi-termd").into(),
            directory: rig.dir.path().join("gates"),
            platform: Arc::from(term_platform::group::select_backend()),
            timeout: Duration::from_secs(5),
        },
    ))
}

fn actor(rig: &Rig) -> (tokio::runtime::Runtime, Arc<ExecSupervisor>, MissionActor) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let supervisor = supervisor(rig, &rig.service);
    supervisor.refresh_recovery().unwrap();
    let actor = rig
        .actor(factory(Arc::new(Mutex::new(vec![]))))
        .with_verification_exec(supervisor.clone(), runtime.handle().clone());
    (runtime, supervisor, actor)
}

fn marker_bytes(rig: &Rig) -> Vec<u8> {
    rig.snapshot()
        .workspaces
        .iter()
        .filter(|w| w.kind == WorkspaceKind::Verification)
        .filter_map(|w| {
            let p = std::path::Path::new(&w.path);
            let output =
                p.with_file_name(format!("{}.verification-output", p.file_name()?.to_str()?));
            std::fs::read(output.join("invocations")).ok()
        })
        .flatten()
        .collect()
}

fn command(rig: &Rig, program: &str, argv: Vec<String>, timeout: u64) {
    let mut command: VerificationCommand = serde_json::from_value(
        rig.storage
            .mission_configs("verification")
            .unwrap()
            .remove(0),
    )
    .unwrap();
    command.program = program.into();
    command.argv = argv;
    command.timeout_ms = timeout;
    rpc(
        &rig.service,
        &rig.conn,
        "verification.save",
        json!({"request_id":Id::generate(),"expected_revision":command.revision,"command":command}),
    );
}

fn fault(rig: &Rig, trigger: &str) {
    rusqlite::Connection::open(rig.dir.path().join("state.db"))
        .unwrap()
        .execute_batch(trigger)
        .unwrap();
}

fn body(rig: &Rig, reference: &ArtifactRef) -> Value {
    serde_json::from_slice(
        &ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"))
            .read_mission_body(&rig.id, reference, 256 * 1024)
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn verified_result_links_one_owned_exec_and_a_frozen_command_contract() {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let (_runtime, supervisor, mut actor) = actor(&rig);
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let after = rig.snapshot();
    let run = after
        .runs
        .iter()
        .find(|r| r.binding_snapshot.is_none())
        .unwrap();
    let exec = after
        .execs
        .iter()
        .find(|e| Some(&e.id) == run.exec_id.as_ref())
        .unwrap();
    assert_eq!(after.execs.len(), 1);
    assert_eq!(exec.state, ExecState::Exited);
    assert!(exec.ended_at.is_some() && exec.identity.is_some());
    assert_eq!(exec.run_id, run.id);
    assert_eq!(after.verifications[0].run_id, run.id);
    assert_eq!(after.verifications[0].status, VerificationStatus::Passed);
    let context = body(&rig, &run.context_ref);
    let manifest = body(&rig, &exec.launch_manifest_ref);
    assert_eq!(context["kind"], "verification_exec");
    assert_eq!(
        context["candidate_id"],
        serde_json::to_value(&after.mission.candidate_id).unwrap()
    );
    assert_eq!(context["version"], 2);
    assert_eq!(manifest["program"], "/usr/bin/sandbox-exec");
    assert_eq!(manifest["argv"][4], context["program"]);
    assert_eq!(
        manifest["argv"].as_array().unwrap()[5..],
        *context["command"]["argv"].as_array().unwrap()
    );
    assert_eq!(manifest["env_clear"], true);
    assert!(manifest["env_keys"]
        .as_array()
        .unwrap()
        .contains(&json!("IYAGI_VERIFICATION_OUTPUT")));
    assert_eq!(
        after.verifications[0].input_integrity,
        InputIntegrity::Enforced
    );
    let environment = body(&rig, &after.verifications[0].environment_ref);
    assert_eq!(environment["isolation"], context["isolation"]);
    assert_eq!(environment["input_integrity"], "enforced");
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
    assert_eq!(supervisor.ledger().active_count(), 0);
    actor.shutdown();
}

#[test]
#[cfg(target_os = "macos")]
fn strict_verification_accepts_only_isolated_input_and_separate_output() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let snapshot = rig.snapshot();
    let mut policy = snapshot.mission.policy.clone();
    policy.require_enforced_verification = true;
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({
        "request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,
        "policy":policy,"role_bindings":snapshot.mission.role_bindings}),
    );
    command(&rig, "/bin/sh", vec!["-c".into(),
        "test \"$HOME\" = \"$IYAGI_VERIFICATION_OUTPUT\" && test \"$TMPDIR\" = \"$HOME\" && test -z \"$OPENAI_API_KEY$ANTHROPIC_API_KEY$SSH_AUTH_SOCK$NODE_OPTIONS\" && ! (printf changed > api.txt) && printf proof > \"$IYAGI_VERIFICATION_OUTPUT/result\" && git ls-files --error-unmatch api.txt ui.txt".into()], 10000);
    let (_runtime, supervisor, mut actor) = actor(&rig);
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let after = rig.snapshot();
    assert_eq!(
        after.verifications[0].input_integrity,
        InputIntegrity::Enforced
    );
    let environment = body(&rig, &after.verifications[0].environment_ref);
    let output = Path::new(environment["isolation"]["output"].as_str().unwrap());
    assert_eq!(std::fs::read(output.join("result")).unwrap(), b"proof");
    let worktree = Path::new(environment["worktree"].as_str().unwrap());
    assert!(git(worktree, &["status", "--porcelain"]).is_empty());
    rpc(
        &rig.service,
        &rig.conn,
        "mission.accept",
        json!({"request_id":Id::generate(),
        "mission_id":rig.id,"expected_revision":after.mission.revision,
        "candidate_id":after.mission.candidate_id,"acknowledged_verification_ids":[],"human_requirement_ids":[]}),
    );
    assert_eq!(rig.snapshot().mission.state, MissionState::Completed);
    assert_eq!(supervisor.ledger().active_count(), 0);
    actor.shutdown();
}

#[test]
#[cfg(target_os = "macos")]
fn changed_input_after_start_cannot_be_reported_as_enforced_or_passed() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let release = rig.dir.path().join("release-verifier");
    command(&rig, "/bin/sh", vec!["-c".into(),
        "printf x > \"$IYAGI_VERIFICATION_OUTPUT/invocations\"; while [ ! -f \"$1\" ]; do sleep 0.02; done; printf completed".into(),
        "verify".into(), release.to_string_lossy().into_owned()], 10000);
    let (_runtime, supervisor, mut actor) = actor(&rig);
    rig.tick_until(&mut actor, |_| !marker_bytes(&rig).is_empty());
    let workspace = rig
        .snapshot()
        .workspaces
        .into_iter()
        .find(|w| w.kind == WorkspaceKind::Verification)
        .unwrap();
    // A separate host process is outside the verifier's sandbox. Its changes
    // must invalidate the result even when the isolated command exits zero.
    std::fs::write(
        Path::new(&workspace.path).join("api.txt"),
        "changed by external writer",
    )
    .unwrap();
    std::fs::write(release, "").unwrap();
    rig.tick_until(&mut actor, |s| !s.verifications.is_empty());
    let after = rig.snapshot();
    let verification = &after.verifications[0];
    assert_eq!(verification.exit_code, Some(0));
    assert_eq!(verification.status, VerificationStatus::Failed);
    assert_eq!(verification.input_integrity, InputIntegrity::Unknown);
    let environment = body(&rig, &verification.environment_ref);
    assert_eq!(environment["input_matches_candidate"], false);
    assert!(environment["input_error"]
        .as_str()
        .unwrap()
        .contains("bytes differ"));
    assert_eq!(after.execs[0].state, ExecState::Exited);
    assert_eq!(supervisor.ledger().active_count(), 0);
    actor.shutdown();
}

#[test]
fn pending_durable_exec_exit_keeps_the_verifier_and_its_reservation() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    command(
        &rig,
        "/bin/sh",
        vec![
            "-c".into(),
            "printf x >> \"$IYAGI_VERIFICATION_OUTPUT/$1\"".into(),
            "verify".into(),
            "invocations".into(),
        ],
        5000,
    );
    fault(&rig, "CREATE TRIGGER hold_verify_exit BEFORE UPDATE ON orch_execs WHEN NEW.state = 'exited' BEGIN SELECT RAISE(ABORT, 'exit write outage'); END;");
    let (_runtime, supervisor, mut actor) = actor(&rig);
    rig.tick_until(&mut actor, |_| !marker_bytes(&rig).is_empty());
    std::thread::sleep(Duration::from_millis(150));
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert!(rig.snapshot().verifications.is_empty());
    assert_eq!(rig.snapshot().execs[0].state, ExecState::Spawned);
    assert_eq!(supervisor.ledger().active_count(), 1);
    assert!(actor.live_count() > 0);
    fault(&rig, "DROP TRIGGER hold_verify_exit;");
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    assert_eq!(marker_bytes(&rig), b"x");
    assert_eq!(rig.snapshot().execs.len(), 1);
    actor.shutdown();
}

#[test]
fn verification_evidence_write_retries_without_executing_the_command_again() {
    for cancel_during_save in [false, true] {
        let rig = Rig::new(true, &["status", "--porcelain"]);
        command(
            &rig,
            "/bin/sh",
            vec![
                "-c".into(),
                "printf x >> \"$IYAGI_VERIFICATION_OUTPUT/$1\"".into(),
                "verify".into(),
                "invocations".into(),
            ],
            5000,
        );
        fault(&rig, "CREATE TRIGGER hold_verify_evidence BEFORE INSERT ON orch_entities WHEN NEW.kind = 'verification' BEGIN SELECT RAISE(ABORT, 'evidence write outage'); END;");
        let (_runtime, _, mut actor) = actor(&rig);
        rig.tick_until(&mut actor, |s| {
            s.execs.iter().any(|e| e.state == ExecState::Exited)
        });
        for _ in 0..5 {
            actor.tick().unwrap();
        }
        let pending = rig.snapshot();
        assert!(pending.verifications.is_empty());
        assert!(pending
            .runs
            .iter()
            .any(|r| r.exec_id.is_some() && r.state == RunState::Running));
        assert!(actor.live_count() > 0);
        if cancel_during_save {
            rpc(
                &rig.service,
                &rig.conn,
                "mission.control",
                json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":"cancel"}),
            );
            for _ in 0..5 {
                actor.tick().unwrap();
            }
            assert_eq!(rig.snapshot().mission.state, MissionState::Stopping);
            assert!(rig.snapshot().verifications.is_empty());
        }
        fault(&rig, "DROP TRIGGER hold_verify_evidence;");
        rig.tick_until(&mut actor, |s| {
            if cancel_during_save {
                s.mission.state == MissionState::Cancelled
            } else {
                s.mission.phase == Phase::AwaitingAcceptance
            }
        });
        assert_eq!(
            rig.snapshot().verifications[0].status,
            if cancel_during_save {
                VerificationStatus::Cancelled
            } else {
                VerificationStatus::Passed
            }
        );
        assert_eq!(marker_bytes(&rig), b"x");
        assert_eq!(rig.snapshot().verifications.len(), 1);
        actor.shutdown();
    }
}

#[test]
fn verification_timeout_confirms_exec_cleanup_before_recording_failure() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    command(&rig, "/bin/sleep", vec!["30".into()], 50);
    let (_runtime, _, mut actor) = actor(&rig);
    rig.tick_until(&mut actor, |s| !s.verifications.is_empty());
    let after = rig.snapshot();
    assert_eq!(after.verifications[0].status, VerificationStatus::Failed);
    assert_eq!(after.execs[0].state, ExecState::Exited);
    assert!(after.execs[0].ended_at.is_some());
    assert!(after
        .runs
        .iter()
        .find(|r| r.id == after.verifications[0].run_id)
        .unwrap()
        .ended_at
        .is_some());
    actor.shutdown();
}

#[test]
fn stopping_a_verifier_cancels_its_owned_process_before_settlement() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    command(&rig, "/bin/sleep", vec!["30".into()], 60000);
    let (_runtime, _, mut actor) = actor(&rig);
    rig.tick_until(&mut actor, |s| {
        s.execs.iter().any(|e| e.state == ExecState::Spawned)
    });
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":"cancel"}),
    );
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Cancelled);
    let after = rig.snapshot();
    assert_eq!(after.verifications[0].status, VerificationStatus::Cancelled);
    assert_eq!(after.execs[0].state, ExecState::Exited);
    assert!(!after.runs.iter().any(|r| r.holds_execution_slot()));
    actor.shutdown();
}

#[test]
fn cancelled_verifier_waits_for_explicit_retry_then_uses_a_new_exec_and_workspace() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let release = rig.dir.path().join("cancel-retry-release");
    command(&rig, "/bin/sh", vec!["-c".into(),
        "printf x >> \"$IYAGI_VERIFICATION_OUTPUT/$1\"; while [ ! -f \"$2\" ]; do sleep 0.02; done".into(),
        "verify".into(), "invocations".into(), release.to_string_lossy().into_owned()], 10000);
    let (_runtime, _, mut actor) = actor(&rig);
    rig.tick_until(&mut actor, |_| !marker_bytes(&rig).is_empty());
    let task = rig
        .snapshot()
        .tasks
        .into_iter()
        .find(|t| t.kind == TaskKind::Verify)
        .unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"task_id":task.id,"action":"cancel","binding_id":null}),
    );
    rig.tick_until(&mut actor, |s| {
        s.tasks.iter().any(|t| {
            t.id == task.id && t.state == TaskState::Cancelled && t.active_run_id.is_none()
        }) && s.execs.iter().any(|e| e.state == ExecState::Exited)
    });
    let before = rig.snapshot();
    let old = before
        .runs
        .iter()
        .find(|r| r.task_id == task.id)
        .unwrap()
        .clone();
    assert_eq!(old.state, RunState::Cancelled);
    for _ in 0..4 {
        actor.tick().unwrap();
    }
    assert_eq!(rig.snapshot().mission.phase, Phase::Validating);
    assert_eq!(marker_bytes(&rig), b"x");
    std::fs::write(release, "").unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"task_id":task.id,"action":"retry","binding_id":null}),
    );
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let after = rig.snapshot();
    let next = after
        .runs
        .iter()
        .filter(|r| r.task_id == task.id)
        .max_by_key(|r| r.attempt)
        .unwrap();
    assert_eq!(marker_bytes(&rig), b"xx");
    assert_eq!(after.runs.iter().find(|r| r.id == old.id), Some(&old));
    assert_eq!(next.state, RunState::Succeeded);
    assert_ne!(old.exec_id, next.exec_id);
    assert_ne!(old.workspace_id, next.workspace_id);
    assert!(after
        .verifications
        .iter()
        .any(|v| v.run_id == next.id && v.status == VerificationStatus::Passed));
    assert!(after
        .verifications
        .iter()
        .any(|v| v.run_id == old.id && v.status == VerificationStatus::Cancelled));
    actor.shutdown();
}

#[test]
fn log_upload_failures_preserve_completed_output_without_relaunch() {
    for trigger in [
        "CREATE TRIGGER hold_log BEFORE INSERT ON orch_uploads BEGIN SELECT RAISE(ABORT, 'log begin outage'); END;",
        "CREATE TRIGGER hold_log BEFORE UPDATE OF next_offset ON orch_uploads BEGIN SELECT RAISE(ABORT, 'log chunk outage'); END;",
        "CREATE TRIGGER hold_log BEFORE INSERT ON orch_artifacts BEGIN SELECT RAISE(ABORT, 'log commit outage'); END;",
    ] {
        let rig = Rig::new(true, &["status", "--porcelain"]);
            let release = rig.dir.path().join("release");
        command(&rig, "/bin/sh", vec!["-c".into(),
            "printf x >> \"$IYAGI_VERIFICATION_OUTPUT/$1\"; while [ ! -f \"$2\" ]; do sleep 0.02; done; printf retained-output".into(),
            "verify".into(), "invocations".into(), release.to_string_lossy().into_owned()], 10000);
        let (_runtime, _, mut actor) = actor(&rig);
        rig.tick_until(&mut actor, |_| !marker_bytes(&rig).is_empty());
        fault(&rig, trigger);
        std::fs::write(&release, "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !rig.snapshot().execs.iter().any(|e| e.state == ExecState::Exited) {
            assert!(Instant::now() < deadline, "Exec exit persists independently of log uploads");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(200));
        assert!(rig.snapshot().verifications.is_empty(), "{trigger}");
        assert!(actor.live_count() > 0);
        fault(&rig, "DROP TRIGGER hold_log;");
        rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
        assert_eq!(marker_bytes(&rig), b"x");
        let after = rig.snapshot();
        assert_eq!(after.execs.len(), 1);
        let bytes = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"))
            .read_mission_body(&rig.id, &after.verifications[0].log_ref, 256 * 1024).unwrap();
        assert!(String::from_utf8(bytes).unwrap().contains("retained-output"));
        actor.shutdown();
    }
}

#[test]
fn published_artifact_can_commit_after_store_reopen_and_rejects_changed_bytes() {
    use term_contracts::ids::U64String;
    use term_contracts::mission::{rpc::*, MissionErrorCode};
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let root = rig.dir.path().join("missions");
    let artifacts = ArtifactStore::new(rig.storage.clone(), root.clone());
    let bytes = b"retained log";
    let (upload_id, _) = artifacts
        .begin(
            &rig.conn,
            &ArtifactBeginParams {
                request_id: Id::generate(),
                mission_id: Some(rig.id.clone()),
                media_type: "text/plain".into(),
                bytes: U64String::new(bytes.len() as u64).unwrap(),
                sha256: format!("{:x}", Sha256::digest(bytes)),
            },
        )
        .unwrap();
    let chunk = ArtifactWriteParams {
        upload_id: upload_id.clone(),
        offset: U64String::new(0).unwrap(),
        data_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
    };
    fault(&rig, "CREATE TRIGGER hold_chunk BEFORE UPDATE OF next_offset ON orch_uploads BEGIN SELECT RAISE(ABORT, 'cursor outage'); END;");
    assert_eq!(
        artifacts.write(&chunk).unwrap_err().0,
        MissionErrorCode::StorageUnavailable
    );
    let mut changed = chunk.clone();
    changed.data_b64 = base64::engine::general_purpose::STANDARD.encode(b"tampered log");
    assert_eq!(
        artifacts.write(&changed).unwrap_err().0,
        MissionErrorCode::RequestConflict
    );
    fault(&rig, "DROP TRIGGER hold_chunk;");
    assert_eq!(artifacts.write(&chunk).unwrap(), bytes.len() as u64);
    changed.data_b64 = base64::engine::general_purpose::STANDARD.encode(b"retained log and extra");
    assert_eq!(
        artifacts.write(&changed).unwrap_err().0,
        MissionErrorCode::RequestConflict
    );
    let params = ArtifactCommitParams {
        upload_id: upload_id.clone(),
    };
    fault(&rig, "CREATE TRIGGER hold_artifact BEFORE INSERT ON orch_artifacts BEGIN SELECT RAISE(ABORT, 'artifact outage'); END;");
    assert_eq!(
        artifacts.commit(&params).unwrap_err().0,
        MissionErrorCode::StorageUnavailable
    );
    let body = root
        .join("artifacts")
        .join(&upload_id.as_str()[..2])
        .join(upload_id.as_str());
    assert_eq!(std::fs::read(&body).unwrap(), bytes);
    assert!(!root
        .join("artifacts/uploads")
        .join(format!("{upload_id}.part"))
        .exists());
    drop(artifacts);
    fault(&rig, "DROP TRIGGER hold_artifact;");
    let reopened = ArtifactStore::new(rig.storage.clone(), root);
    std::fs::write(&body, b"tampered log").unwrap();
    assert_eq!(
        reopened.commit(&params).unwrap_err().0,
        MissionErrorCode::IntegrityFailed
    );
    std::fs::write(&body, bytes).unwrap();
    let reference = reopened.commit(&params).unwrap();
    assert_eq!(reference.id, upload_id);
    assert_eq!(reopened.commit(&params).unwrap(), reference);
    assert_eq!(
        reopened
            .read_mission_body(&rig.id, &reference, 1024)
            .unwrap(),
        bytes
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn recovered_verifier_rejects_a_changed_owned_command_contract() {
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
    command(&rig, "/bin/sleep", vec!["30".into()], 60000);
    let (_runtime, _, mut actor) = actor(&rig);
    rig.tick_until(&mut actor, |s| {
        s.execs.iter().any(|e| e.state == ExecState::Spawned)
    });
    let after = rig.snapshot();
    let exec = after.execs[0].clone();
    let run = after
        .runs
        .iter()
        .find(|r| r.id == exec.run_id)
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
        ("/command/argv", json!(["0"])),
        ("/program", json!("/bin/true")),
        ("/cwd", json!("/tmp")),
        ("/candidate_id", json!(Id::generate())),
        ("/candidate_commit_oid", json!("a".repeat(40))),
        ("/workspace_id", json!(Id::generate())),
        ("/task_id", json!(Id::generate())),
        ("/run_id", json!(Id::generate())),
        ("/resource_policy/cpu_slots", json!(2)),
        ("/isolation/output", json!("/tmp")),
        ("/isolation/allow_network", json!(true)),
        ("/isolation/env/PATH", json!("/some/different/toolchain")),
        ("/isolation/env/HOME", json!("/tmp")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let mut replaced = run.clone();
        replaced.context_ref = workflow::store_artifact(
            &artifacts,
            &rig.id,
            "application/json",
            &serde_json::to_vec(&changed).unwrap(),
        )
        .unwrap();
        workflow::commit_upserts(
            &rig.service,
            rig.snapshot().mission,
            "fixture.verifier_contract",
            "tamper",
            MissionEventType::Changed,
            vec![Entity::Run(Box::new(replaced))],
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
        "fixture.verifier_contract",
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
}

#[test]
fn verification_cannot_execute_before_its_frozen_contract_is_durable() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    command(
        &rig,
        "/bin/sh",
        vec![
            "-c".into(),
            "printf x > \"$IYAGI_VERIFICATION_OUTPUT/$1\"".into(),
            "verify".into(),
            "invocations".into(),
        ],
        5000,
    );
    fault(&rig, "CREATE TRIGGER hold_verifier_contract BEFORE UPDATE ON orch_runs WHEN NEW.state = 'starting' AND json_extract(NEW.document_json, '$.binding_snapshot') IS NULL AND json_extract(NEW.document_json, '$.context_ref.id') <> json_extract(OLD.document_json, '$.context_ref.id') BEGIN SELECT RAISE(ABORT, 'contract outage'); END;");
    let (_runtime, supervisor, mut actor) = actor(&rig);
    rig.tick_until(&mut actor, |s| {
        s.runs
            .iter()
            .any(|r| r.binding_snapshot.is_none() && r.state == RunState::Failed)
    });
    assert!(marker_bytes(&rig).is_empty());
    assert!(rig.snapshot().execs.is_empty());
    assert_eq!(supervisor.ledger().active_count(), 0);
    fault(&rig, "DROP TRIGGER hold_verifier_contract;");
    actor.shutdown();
}
