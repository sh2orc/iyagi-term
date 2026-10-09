use super::*;
use iyagi_termd_lib::{
    agent_runtime::AdapterEvent,
    exec::{gated::GateConfig, ExecSupervisor},
};
use term_contracts::{metrics::PressureLevel, snapshot::QueueReason};
use term_core::{AdmissionConfig, AdmissionHost, AdmissionRequest};

fn previous_exec() -> (Rig, ExecRecord) {
    let instant = Instant::now();
    let rig = Rig::with_clock(
        false,
        &["status", "--porcelain"],
        true,
        Some(Arc::new(move || instant)),
    );
    let (mut exec, body) = prepared_exec(&rig);
    exec.launch_manifest_ref = rig
        .service
        .exec_persistence()
        .prepare(exec.clone(), &body)
        .unwrap();
    (rig, exec)
}
fn restarted(rig: &Rig) -> Arc<MissionService> {
    Arc::new(MissionService::new(
        rig.storage.clone(),
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
    ))
}
fn host() -> AdmissionHost {
    AdmissionHost {
        total_bytes: 16 << 30,
        available_bytes: Some(14 << 30),
        sample_age_ms: 0,
        reconciliation_required: false,
        pressure: PressureLevel::Normal,
    }
}
fn supervisor(rig: &Rig, service: &Arc<MissionService>) -> ExecSupervisor {
    ExecSupervisor::persistent(
        AdmissionConfig {
            logical_cpus: 8,
            managed_concurrency: 1,
            telemetry_stale_ms: 3000,
            host_reserve_min_bytes: 2 << 30,
            host_reserve_percent: 15,
            managed_budget_percent: 50,
        },
        service.exec_persistence(),
        host(),
        GateConfig {
            helper_program: env!("CARGO_BIN_EXE_iyagi-termd").into(),
            directory: rig.dir.path().join("gates"),
            platform: Arc::from(term_platform::group::select_backend()),
            timeout: Duration::from_secs(5),
        },
    )
}
fn denial(supervisor: &ExecSupervisor, reason: QueueReason) {
    let result = supervisor.ledger().try_admit_and_reserve(
        &host(),
        &Id::generate(),
        AdmissionRequest {
            reservation_bytes: 1,
            cpu_slots: 1,
        },
    );
    assert!(
        matches!(result, Err(iyagi_termd_lib::exec::ExecError::AdmissionDenied { reason: actual }) if actual == reason)
    );
}
fn exit(rig: &Rig, mut exec: ExecRecord) -> ExecRecord {
    exec.state = ExecState::Exited;
    exec.ended_at = Some(term_storage::time::now_iso8601());
    rig.service.exec_persistence().update(exec.clone()).unwrap();
    exec
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct NativeChild(std::process::Child, Option<std::process::Child>);
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for NativeChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        if let Some(guardian) = &mut self.1 {
            let _ = guardian.kill();
            let _ = guardian.wait();
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn previous_native_exec() -> Option<(Rig, ExecRecord, NativeChild)> {
    use term_contracts::{
        ids::{SessionId, WorkloadId},
        workload::{GroupKind, WorkloadDescriptor},
    };
    let platform = term_platform::group::select_backend();
    if cfg!(target_os = "linux")
        && platform.capabilities().memory_limit_kind.support
            != term_contracts::snapshot::LimitSupport::Supported
    {
        assert_ne!(
            std::env::var("IYAGI_CGROUP_REQUIRE_DELEGATION").as_deref(),
            Ok("1")
        );
        return None;
    }
    let at = Instant::now();
    let rig = Rig::with_clock(
        false,
        &["status", "--porcelain"],
        true,
        Some(Arc::new(move || at)),
    );
    let mut binding: Binding =
        serde_json::from_value(rig.storage.mission_bindings().unwrap().remove(0)).unwrap();
    binding.program = "/bin/sleep".into();
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":binding.revision,"binding":binding}),
    );
    let (mut exec, body) = prepared_exec(&rig);
    let mut manifest: Value = serde_json::from_slice(&body).unwrap();
    manifest["argv"] = json!(["60"]);
    let body = serde_json::to_vec(&manifest).unwrap();
    exec.launch_manifest_ref.sha256 = format!("{:x}", Sha256::digest(&body));
    exec.launch_manifest_ref.bytes =
        term_contracts::ids::U64String::new(body.len() as u64).unwrap();
    exec.launch_manifest_ref = rig
        .service
        .exec_persistence()
        .prepare(exec.clone(), &body)
        .unwrap();
    let descriptor = WorkloadDescriptor {
        workload_id: WorkloadId::parse(exec.id.as_str()).unwrap(),
        session_id: SessionId::generate(),
        cwd: manifest["cwd"].as_str().unwrap().into(),
        program: "/bin/sleep".into(),
        argv: vec!["60".into()],
        env_overrides: Default::default(),
        cols: 80,
        rows: 24,
        policy: exec.resource_policy.clone(),
    };
    #[cfg(target_os = "linux")]
    let (group, guardian) = (platform.create_group(&descriptor).unwrap(), None);
    #[cfg(target_os = "macos")]
    let (group, guardian) = {
        use std::os::unix::fs::DirBuilderExt;
        let directory = rig.dir.path().join("observer");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let endpoint = directory.join("g");
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_iyagi-termd"))
            .args([
                "--exec-guardian",
                endpoint.to_str().unwrap(),
                exec.id.as_str(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let identity = term_platform::process_identity(child.id()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let group = loop {
            if let Ok(group) = term_platform::group::macos_guardian::connect_group(
                &descriptor.workload_id,
                endpoint.to_str().unwrap(),
                &identity,
            ) {
                break group;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        (group, Some(child))
    };
    let child = NativeChild(
        std::process::Command::new("/bin/sleep")
            .arg("60")
            .current_dir(&descriptor.cwd)
            .spawn()
            .unwrap(),
        guardian,
    );
    exec.identity = Some(term_platform::process_identity(child.0.id()).unwrap());
    platform
        .attach_pid(&group, exec.identity.as_ref().unwrap())
        .unwrap();
    exec.group_kind = Some(match group.kind {
        GroupKind::Cgroup => ExecGroupKind::Cgroup,
        _ => ExecGroupKind::ObservedTree,
    });
    exec.group_reference = Some(group.reference.clone());
    exec.group_identity = platform.recovery_identity(&group).unwrap();
    assert!(exec.group_identity.is_some());
    exec.state = ExecState::Spawned;
    exec.started_at = Some(term_storage::time::now_iso8601());
    rig.service.exec_persistence().update(exec.clone()).unwrap();
    // No old supervisor or native handle remains to observe/terminate this
    // child. The replacement must recover using only the persisted proof.
    drop(group);
    drop(platform);
    Some((rig, exec, child))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_recovery_stops_only_after_cancel_and_retries_failed_exit_commit_before_releasing() {
    let Some((rig, exec, mut child)) = previous_native_exec() else {
        return;
    };
    let fresh = restarted(&rig);
    fresh.recover_on_startup().unwrap();
    let replacement = supervisor(&rig, &fresh);
    replacement.refresh_recovery().unwrap();
    assert!(replacement.reconcile_native_recovery().is_empty());
    assert!(child.0.try_wait().unwrap().is_none());
    assert_eq!(rig.snapshot().execs[0], exec);
    let old_run = rig.snapshot().runs[0].clone();
    assert_eq!(old_run.state, RunState::Unknown);
    rpc(
        &fresh,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"action":"cancel"}),
    );
    let db = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_recovered_exit BEFORE INSERT ON orch_events BEGIN SELECT RAISE(FAIL,'fixture recovery DB failure'); END;").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let errors = replacement.reconcile_native_recovery();
        if !errors.is_empty() {
            assert_eq!(errors[0].0, exec.id);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "recovered process never terminated"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    child.0.wait().unwrap();
    assert_eq!(rig.snapshot().execs[0].state, ExecState::Spawned);
    assert!(Path::new(exec.group_reference.as_ref().unwrap()).exists());
    assert_eq!(replacement.refresh_recovery().unwrap(), 1);
    denial(&replacement, QueueReason::WaitConcurrency);
    assert!(rig.snapshot().runs[0].reconciliation_ref.is_none());
    // Simulate another restart during the failed commit: durable native
    // evidence, not the first recovery worker's in-memory handle, is enough.
    drop(replacement);
    let replacement = supervisor(&rig, &fresh);
    replacement.refresh_recovery().unwrap();
    db.execute_batch("DROP TRIGGER deny_recovered_exit")
        .unwrap();
    assert!(replacement.reconcile_native_recovery().is_empty());
    let ended = rig.snapshot().execs[0].clone();
    assert_eq!(ended.state, ExecState::Exited);
    assert_eq!(ended.owner_daemon_id, exec.owner_daemon_id);
    assert_eq!(ended.group_identity, exec.group_identity);
    assert!(ended.ended_at.is_some());
    assert!(ended.exit_code.is_none());
    assert_eq!(
        replacement.ledger().active_count(),
        1,
        "only a consistent DB refresh releases the reservation"
    );
    assert_eq!(replacement.refresh_recovery().unwrap(), 0);
    // The guardian acknowledges retirement before its observer thread joins
    // and its socket is removed. The acknowledgement is not a process wait.
    let cleanup_deadline = Instant::now() + Duration::from_secs(5);
    while Path::new(exec.group_reference.as_ref().unwrap()).exists() {
        assert!(
            Instant::now() < cleanup_deadline,
            "native evidence cleanup stalled"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!Path::new(exec.group_reference.as_ref().unwrap()).exists());
    let mut actor = MissionActor::new(
        fresh,
        rig.dir.path().join("missions"),
        Arc::new(|_| panic!("recovery must not rerun the provider")),
    );
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Cancelled);
    let mut expected = old_run;
    expected.reconciliation_ref = rig.snapshot().runs[0].reconciliation_ref.clone();
    expected.reconciliation_kind = Some(ReconciliationKind::ExecExited);
    assert!(expected.reconciliation_ref.is_some());
    assert_eq!(rig.snapshot().runs[0], expected);
    assert!(Path::new(&rig.snapshot().workspaces[0].path).exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_recovery_never_signals_after_durable_ownership_changes() {
    let Some((rig, exec, mut child)) = previous_native_exec() else {
        return;
    };
    let fresh = restarted(&rig);
    fresh.recover_on_startup().unwrap();
    let replacement = supervisor(&rig, &fresh);
    replacement.refresh_recovery().unwrap();
    rpc(
        &fresh,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"action":"cancel"}),
    );
    // Corrupt the durable native identity after the initial reservation read.
    // The pre-signal store validation must reject the stale snapshot.
    let db = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    db.execute("UPDATE orch_execs SET document_json=json_set(document_json,'$.group_reference','wrong-group') WHERE id=?1", [exec.id.as_str()]).unwrap();
    assert!(!replacement.reconcile_native_recovery().is_empty());
    assert!(child.0.try_wait().unwrap().is_none());
    assert!(replacement.refresh_recovery().is_err());
    denial(&replacement, QueueReason::WaitTelemetry);
    // Restore the record and let the normal authorized recovery clean up.
    db.execute(
        "UPDATE orch_execs SET document_json=?1 WHERE id=?2",
        [serde_json::to_string(&exec).unwrap(), exec.id.to_string()],
    )
    .unwrap();
    replacement.refresh_recovery().unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while rig.snapshot().execs[0].state != ExecState::Exited {
        assert!(replacement.reconcile_native_recovery().is_empty());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    child.0.wait().unwrap();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_recovery_observes_exit_only_with_valid_manifest_and_keeps_retry_explicit() {
    let Some((rig, exec, mut child)) = previous_native_exec() else {
        return;
    };
    let fresh = restarted(&rig);
    fresh.recover_on_startup().unwrap();
    let replacement = supervisor(&rig, &fresh);
    replacement.refresh_recovery().unwrap();
    let id = exec.launch_manifest_ref.id.as_str();
    let body_path = rig
        .dir
        .path()
        .join("missions/artifacts")
        .join(&id[..2])
        .join(id);
    let original = std::fs::read(&body_path).unwrap();
    std::fs::write(&body_path, b"{}").unwrap();
    assert!(!replacement.reconcile_native_recovery().is_empty());
    assert!(child.0.try_wait().unwrap().is_none());
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(!replacement.reconcile_native_recovery().is_empty());
    assert_eq!(rig.snapshot().execs[0].state, ExecState::Spawned);
    assert_eq!(replacement.refresh_recovery().unwrap(), 1);
    std::fs::write(&body_path, original).unwrap();
    assert!(replacement.reconcile_native_recovery().is_empty());
    assert_eq!(rig.snapshot().execs[0].state, ExecState::Exited);
    assert_eq!(replacement.refresh_recovery().unwrap(), 0);
    assert_eq!(fresh.dispatch_tick().unwrap(), 0);
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.runs.len(), 1);
    assert_eq!(snapshot.runs[0].state, RunState::Unknown);
    assert!(snapshot.runs[0].reconciliation_ref.is_some());
    assert!(snapshot
        .decisions
        .iter()
        .any(|d| d.state == DecisionState::Open
            && d.options.iter().any(|o| o.id == "retry_reconciled_task")));
    assert!(Path::new(&snapshot.workspaces[0].path).exists());
}

#[test]
fn persistent_reservations_restore_before_admission_and_release_only_after_stored_exit() {
    let (rig, exec) = previous_exec();
    let fresh = restarted(&rig);
    let supervisor = supervisor(&rig, &fresh);
    denial(&supervisor, QueueReason::WaitTelemetry);
    assert_eq!(supervisor.refresh_recovery().unwrap(), 1);
    assert_eq!(supervisor.ledger().active_count(), 1);
    assert!(!supervisor.ledger().release(&exec.id));
    for _ in 0..3 {
        assert_eq!(supervisor.refresh_recovery().unwrap(), 1);
    }
    denial(&supervisor, QueueReason::WaitConcurrency);
    let ended = exit(&rig, exec.clone());
    assert_eq!(
        fresh
            .exec_persistence()
            .recovery_records(&[exec.id.clone()])
            .unwrap(),
        vec![ended]
    );
    assert_eq!(supervisor.refresh_recovery().unwrap(), 0);
    assert_eq!(supervisor.ledger().active_count(), 0);
    let slot = supervisor
        .ledger()
        .try_admit_and_reserve(
            &host(),
            &Id::generate(),
            AdmissionRequest {
                reservation_bytes: 1,
                cpu_slots: 1,
            },
        )
        .unwrap();
    assert!(supervisor.ledger().release(&slot.exec_id));
    assert!(supervisor.refresh_recovery().is_ok());
}

#[test]
fn recovery_query_ignores_current_owner_but_keeps_terminal_and_archived_mission_executions() {
    let (rig, mut exec) = previous_exec();
    assert!(rig
        .service
        .exec_persistence()
        .recovery_records(&[])
        .unwrap()
        .is_empty());
    let fresh = restarted(&rig);
    for state in [
        ExecState::Prepared,
        ExecState::Spawned,
        ExecState::Stopping,
        ExecState::Unknown,
    ] {
        exec.state = state;
        let mut snapshot = rig.snapshot();
        snapshot.mission.state = MissionState::Cancelled;
        snapshot.mission.archived_at = Some(term_storage::time::now_iso8601());
        let mut run = snapshot.runs[0].clone();
        run.state = RunState::Failed;
        workflow::commit_upserts(
            &rig.service,
            snapshot.mission,
            "fixture.ownership",
            "historical",
            MissionEventType::Changed,
            vec![
                Entity::Exec(Box::new(exec.clone())),
                Entity::Run(Box::new(run)),
            ],
        )
        .unwrap();
        assert_eq!(
            fresh.exec_persistence().recovery_records(&[]).unwrap(),
            vec![exec.clone()]
        );
    }
}

#[test]
fn missing_row_or_corrupt_link_does_not_free_a_restored_reservation_or_clear_the_gate() {
    for fault in ["delete", "run_link", "exec_columns", "resource_policy"] {
        let (rig, exec) = previous_exec();
        let fresh = restarted(&rig);
        let supervisor = supervisor(&rig, &fresh);
        supervisor.refresh_recovery().unwrap();
        let db = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
        match fault {
            "delete" => {
                db.execute("DELETE FROM orch_execs WHERE id=?1", [exec.id.as_str()])
                    .unwrap();
            }
            "run_link" => {
                db.execute("UPDATE orch_runs SET document_json=json_set(document_json,'$.exec_id',NULL) WHERE id=?1", [exec.run_id.as_str()]).unwrap();
            }
            "exec_columns" => {
                db.execute("UPDATE orch_execs SET document_json=json_set(document_json,'$.state','unknown') WHERE id=?1", [exec.id.as_str()]).unwrap();
            }
            "resource_policy" => {
                db.execute("UPDATE orch_execs SET document_json=json_set(document_json,'$.resource_policy.cpu_slots',99) WHERE id=?1", [exec.id.as_str()]).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(supervisor.refresh_recovery().is_err(), "{fault}");
        assert!(supervisor.ledger().is_active(&exec.id));
        supervisor.update_host(host());
        assert!(!supervisor.ledger().recovery_ready());
        denial(&supervisor, QueueReason::WaitTelemetry);
    }
}

#[test]
fn recovery_query_failure_blocks_startup_and_successful_retry_reopens_admission() {
    let rig = Rig::new(false, &["status", "--porcelain"]);
    let fresh = restarted(&rig);
    let supervisor = supervisor(&rig, &fresh);
    let db = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    db.execute_batch("ALTER TABLE orch_execs RENAME TO fixture_unavailable_execs;")
        .unwrap();
    assert!(supervisor.refresh_recovery().is_err());
    denial(&supervisor, QueueReason::WaitTelemetry);
    db.execute_batch("ALTER TABLE fixture_unavailable_execs RENAME TO orch_execs;")
        .unwrap();
    assert_eq!(supervisor.refresh_recovery().unwrap(), 0);
    assert!(supervisor.ledger().recovery_ready());
}

#[test]
fn admission_recovery_hold_prevents_new_reservations_but_still_drains_cancellation() {
    let rig = Rig::new(false, &["status", "--porcelain"]);
    let adapter = scripted(FakeScript {
        steps: vec![
            FakeStep::Started {
                session_id: None,
                turn_id: None,
            },
            FakeStep::Approval {
                request_id: "hold".into(),
                question: "hold".into(),
            },
        ],
        ..Default::default()
    });
    let factory: AdapterFactory = Arc::new(move |_| Ok(adapter.clone()));
    let mut actor = rig.actor(factory);
    actor.set_dispatch_permitted(false);
    for _ in 0..3 {
        actor.tick().unwrap();
    }
    assert!(rig.snapshot().runs.is_empty());
    assert_eq!(rig.snapshot().mission.automatic_start_count, 0);
    actor.set_dispatch_permitted(true);
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::AwaitingInput)
    });
    actor.set_dispatch_permitted(false);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"action":"cancel"}),
    );
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Cancelled);
    assert_eq!(actor.live_count(), 0);
    assert_eq!(rig.snapshot().runs.len(), 1);
}

#[test]
fn native_identity_does_not_make_a_missing_adapter_an_absent_process() {
    let (rig, exec) = previous_exec();
    let spawned = observed_spawn(exec);
    rig.service
        .exec_persistence()
        .update(spawned.clone())
        .unwrap();
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
    let fresh = restarted(&rig);
    fresh.recover_on_startup().unwrap();
    let supervisor = supervisor(&rig, &fresh);
    assert_eq!(supervisor.refresh_recovery().unwrap(), 1);
    assert_eq!(
        supervisor.inspect(&spawned.id),
        iyagi_termd_lib::exec::ExecProbe::Absent
    );
    assert!(supervisor.ledger().is_active(&spawned.id));
    assert!(rig.snapshot().runs[0].holds_execution_slot());
    assert!(rig.snapshot().runs[0].reconciliation_ref.is_none());
}

#[tokio::test]
async fn replacement_supervisor_accounts_for_a_real_surviving_native_process_until_cleanup_commits()
{
    use iyagi_termd_lib::exec::SpawnRequest;
    let rig = Rig::new(false, &["status", "--porcelain"]);
    let daemon = Path::new(env!("CARGO_BIN_EXE_iyagi-termd"));
    let fixture = daemon.parent().unwrap().join(if cfg!(windows) {
        "term-fixture.exe"
    } else {
        "term-fixture"
    });
    let mut binding: Binding =
        serde_json::from_value(rig.storage.mission_bindings().unwrap().remove(0)).unwrap();
    binding.program = fixture.to_string_lossy().into_owned();
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":binding.revision,"binding":binding}),
    );
    let (record, body) = prepared_exec(&rig);
    let manifest: Value = serde_json::from_slice(&body).unwrap();
    let original = supervisor(&rig, &rig.service);
    original.refresh_recovery().unwrap();
    let handle = original
        .spawn(SpawnRequest {
            exec_id: record.id.clone(),
            mission_id: rig.id.clone(),
            run_id: record.run_id.clone(),
            owner_daemon_id: record.owner_daemon_id,
            program: fixture,
            argv: vec![
                "flood".into(),
                "--bytes".into(),
                "0".into(),
                "--hold-ms".into(),
                "60000".into(),
            ],
            cwd: manifest["cwd"].as_str().unwrap().into(),
            env_overrides: Default::default(),
            env_clear: false,
            stdin: None,
            resource_policy: record.resource_policy,
            spool_bytes: 1024,
            redactor: None,
            sink: Arc::new(|_, _| {}),
            validate_path: None,
        })
        .await
        .unwrap();
    let identity = handle.identity().unwrap();
    assert!(term_platform::identity::process_identity(identity.pid)
        .is_some_and(|p| p.same_process(&identity)));
    let fresh = restarted(&rig);
    fresh.recover_on_startup().unwrap();
    let replacement = supervisor(&rig, &fresh);
    assert_eq!(replacement.refresh_recovery().unwrap(), 1);
    denial(&replacement, QueueReason::WaitConcurrency);
    assert!(replacement.ledger().is_active(&record.id));
    assert_eq!(handle.inspect(), iyagi_termd_lib::exec::ExecProbe::Running);
    handle.stop(Duration::ZERO, Duration::ZERO).await.unwrap();
    assert_eq!(rig.snapshot().execs[0].state, ExecState::Exited);
    assert_eq!(replacement.refresh_recovery().unwrap(), 0);
    assert_eq!(original.ledger().active_count(), 0);
    assert_eq!(replacement.ledger().active_count(), 0);
    assert_eq!(fresh.dispatch_tick().unwrap(), 0);
    assert!(rig.snapshot().runs[0].reconciliation_ref.is_some());
}
