//! O07 integration tests: exec supervisor + fake adapter against the real
//! `term-fixture agent-fake` child (docs/orchestration/07-tickets.md O07,
//! scenarios E11/E15/E19/E29-style from 03-adapters.md §7).
//!
//! The supervisor is exercised as a library — the mission service wiring
//! (O11+) is out of scope here. The fixture binary is resolved the same way
//! `tests/common/mod.rs` does (never via cargo, which would deadlock on the
//! target-dir lock).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iyagi_termd_lib::agent_runtime::fake::{
    fake_binding, FakeAdapter, FakeScript, FakeStep, ManualClock,
};
use iyagi_termd_lib::agent_runtime::{AdapterEvent, AgentAdapter, RunStart};
use iyagi_termd_lib::exec::{
    ExecProbe, ExecSupervisor, OutputSink, OutputVerdict, PathValidator, PathVerdict, PersistExec,
    SpawnRequest, StreamKind, DEFAULT_SPOOL_BYTES,
};
use term_contracts::ids::U64String;
use term_contracts::launch::{Enforcement, LaunchPolicy};
use term_contracts::metrics::PressureLevel;
use term_contracts::mission::types::{ExecRecord, ExecState, Id};
use term_contracts::mission::MissionErrorCode;
use term_core::{AdmissionConfig, AdmissionHost, ReservationLedger};

#[path = "support/claude_auth.rs"]
mod claude_auth;
#[path = "support/codex_live.rs"]
mod codex_live;
#[path = "support/credentials.rs"]
mod credentials;
#[cfg(target_os = "macos")]
#[path = "support/macos_guardian.rs"]
mod macos_guardian;

/// Path of the daemon binary built by this package's test run.
fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_iyagi-termd"))
}

/// Locate the term-fixture test program (dev-dependency sibling binary).
fn fixture_bin() -> PathBuf {
    if let Some(env) = std::env::var_os("IYAGI_FIXTURE") {
        let path = PathBuf::from(env);
        if path.is_file() {
            return path;
        }
    }
    let target_debug = daemon_bin()
        .parent()
        .expect("daemon exe has a parent")
        .to_path_buf();
    let candidate = target_debug.join(if cfg!(windows) {
        "term-fixture.exe"
    } else {
        "term-fixture"
    });
    assert!(
        candidate.is_file(),
        "term-fixture binary missing at {candidate:?}; it is a dev-dependency and must exist"
    );
    candidate
}

fn healthy_host() -> AdmissionHost {
    AdmissionHost {
        total_bytes: 16 << 30,
        available_bytes: Some(10 << 30),
        sample_age_ms: 0,
        reconciliation_required: false,
        pressure: PressureLevel::Normal,
    }
}

fn admission_config() -> AdmissionConfig {
    AdmissionConfig {
        logical_cpus: 8,
        managed_concurrency: 2,
        telemetry_stale_ms: 3_000,
        host_reserve_min_bytes: 2 << 30,
        host_reserve_percent: 15,
        managed_budget_percent: 50,
    }
}

fn policy(reservation_mib: u64) -> LaunchPolicy {
    LaunchPolicy {
        reservation_bytes: U64String::new(reservation_mib << 20).expect("fits"),
        cpu_slots: 1,
        enforcement: Enforcement::Prefer,
        memory_max_bytes: None,
        cpu_max_cores: None,
        pids_max: None,
    }
}

type PersistLog = Arc<Mutex<Vec<ExecRecord>>>;

fn recording_persist() -> (PersistExec, PersistLog) {
    let log: PersistLog = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    (
        Arc::new(move |record: ExecRecord| sink.lock().unwrap().push(record)),
        log,
    )
}

fn states_of(log: &PersistLog) -> Vec<ExecState> {
    log.lock().unwrap().iter().map(|r| r.state).collect()
}

struct RequestOpts {
    cwd: PathBuf,
    sink: OutputSink,
    validate_path: Option<PathValidator>,
}

impl Default for RequestOpts {
    fn default() -> Self {
        RequestOpts {
            cwd: std::env::temp_dir(),
            sink: Arc::new(|_, _| {}),
            validate_path: None,
        }
    }
}

fn spawn_request(fixture: &Path, script: &FakeScript, opts: RequestOpts) -> SpawnRequest {
    SpawnRequest {
        exec_id: Id::generate(),
        mission_id: Id::generate(),
        run_id: Id::generate(),
        owner_daemon_id: Id::generate(),
        program: fixture.to_path_buf(),
        argv: vec![
            "agent-fake".into(),
            script.to_argv_b64().expect("scenario b64"),
        ],
        cwd: opts.cwd,
        env_overrides: Default::default(),
        env_clear: false,
        stdin: Some(b"run prompt".to_vec()),
        resource_policy: policy(256),
        spool_bytes: DEFAULT_SPOOL_BYTES,
        redactor: None,
        sink: opts.sink,
        validate_path: opts.validate_path,
    }
}

fn run_start() -> RunStart {
    RunStart {
        task_kind: None,
        mission_id: Id::generate(),
        owner_daemon_id: Id::generate(),
        workspace_access: iyagi_termd_lib::agent_runtime::WorkspaceAccess::ReadOnly,
        allow_network: false,
        run_id: Id::generate(),
        fencing_token: 1,
        binding: fake_binding(),
        context_path: std::env::temp_dir(),
        workspace: None,
        prompt_stdin: "please fake this".into(),
    }
}

// ---- 1. happy path ---------------------------------------------------------

#[tokio::test]
async fn exec_record_lifecycle_happy_path_and_ledger_counted_once() {
    let (persist, log) = recording_persist();
    let supervisor = ExecSupervisor::new(admission_config(), persist, healthy_host());
    let script = FakeScript {
        steps: vec![
            FakeStep::Started {
                session_id: Some("sess-1".into()),
                turn_id: Some("turn-1".into()),
            },
            FakeStep::Activity {
                text: "hello exec".into(),
            },
            FakeStep::Delay { ms: 50 },
            FakeStep::Result {
                value: term_contracts::mission::types::ProviderResult::Report {
                    report_text: "done".into(),
                    knowledge: Vec::new(),
                },
            },
        ],
        ignore_interrupt: false,
        exit_late_ms: 0,
    };
    let request = spawn_request(&fixture_bin(), &script, RequestOpts::default());
    let exec_id = request.exec_id.clone();
    let handle = supervisor.spawn(request).await.expect("spawn");

    // Reservation held while running; the R1 workload ledger (a separate
    // instance) never sees this process (00 §4: counted exactly once).
    assert!(supervisor.ledger().is_active(&exec_id));
    assert_eq!(supervisor.ledger().active_count(), 1);
    let workload_ledger = ReservationLedger::new(admission_config());
    assert_eq!(workload_ledger.active_count(), 0);
    assert_eq!(handle.inspect(), ExecProbe::Running);
    assert_eq!(supervisor.inspect(&exec_id), ExecProbe::Running);
    assert_eq!(
        supervisor.inspect(&Id::generate()),
        ExecProbe::Absent,
        "unknown exec id is Absent"
    );
    assert!(handle.identity().is_some(), "pid+start_token+boot captured");

    let exit = handle.wait().await.expect("wait");
    assert_eq!(exit.code, Some(0));
    assert!(!exit.killed);

    // prepared → spawned → exited, in order, with the exit code recorded.
    assert_eq!(
        states_of(&log),
        vec![ExecState::Prepared, ExecState::Spawned, ExecState::Exited]
    );
    let exited = log.lock().unwrap().last().cloned().expect("exited record");
    assert_eq!(exited.exit_code, Some(0));
    assert!(exited.ended_at.is_some());
    let spawned = log.lock().unwrap()[1].clone();
    assert!(spawned.started_at.is_some());
    assert!(spawned.identity.is_some());
    assert_eq!(
        spawned.group_kind,
        Some(term_contracts::mission::types::ExecGroupKind::ObservedTree)
    );
    assert!(spawned.group_reference.is_some());

    // Finalized guard: reservation released exactly once; supervisor no
    // longer tracks the exec.
    assert!(!supervisor.ledger().is_active(&exec_id));
    assert_eq!(supervisor.ledger().active_count(), 0);
    assert_eq!(supervisor.inspect(&exec_id), ExecProbe::Absent);

    // Bounded output captured the protocol stream; stderr diagnostics tail
    // retained (fixture announces itself on stderr).
    let output = handle.take_output();
    assert!(output.contains("\"t\":\"started\""));
    assert!(output.contains("hello exec"));
    assert!(output.contains("\"t\":\"result\""));
    assert!(handle.take_diagnostics().contains("agent-fake"));
    assert_eq!(handle.output_verdict(), OutputVerdict::Valid);
}

// ---- 2. E19: partial JSON + exit 0 → RESULT_INVALID ------------------------

#[tokio::test]
async fn e19_partial_json_exit_zero_is_result_invalid_not_success() {
    let adapter = FakeAdapter::spawn(fixture_bin()).expect("fake adapter");
    let mut stream = adapter.subscribe();
    let run = run_start();
    let script = FakeScript {
        steps: vec![
            FakeStep::Started {
                session_id: None,
                turn_id: None,
            },
            FakeStep::Activity {
                text: "about to break".into(),
            },
            FakeStep::PartialJson,
        ],
        ignore_interrupt: false,
        exit_late_ms: 0,
    };
    adapter.start_scripted(run, script).expect("start");

    let mut saw_started = false;
    let mut saw_activity = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let Some(event) = stream.next_timeout(Duration::from_secs(5)).await else {
            continue;
        };
        match event {
            AdapterEvent::Started { .. } => saw_started = true,
            AdapterEvent::Activity { .. } => saw_activity = true,
            AdapterEvent::Failed { code, message, .. } => {
                assert_eq!(code, MissionErrorCode::ResultInvalid, "{message}");
                assert!(saw_started && saw_activity);
                return;
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
    panic!("no terminal Failed event within 20s (started={saw_started} activity={saw_activity})");
}

// ---- 3. E15: cancel accepted, process alive → ladder force-kills -----------

#[tokio::test]
async fn e15_late_exit_after_cancel_is_force_killed_within_graces() {
    let (persist, log) = recording_persist();
    let supervisor = ExecSupervisor::new(admission_config(), persist, healthy_host());
    let script = FakeScript {
        steps: vec![
            FakeStep::Started {
                session_id: None,
                turn_id: None,
            },
            FakeStep::Activity {
                text: "staying alive".into(),
            },
        ],
        // The child ignores the interrupt and holds for 30 s — only the
        // ladder's force step can end it (03 §7 cancel-accepted-but-alive).
        ignore_interrupt: true,
        exit_late_ms: 30_000,
    };
    let request = spawn_request(&fixture_bin(), &script, RequestOpts::default());
    let exec_id = request.exec_id.clone();
    let handle = supervisor.spawn(request).await.expect("spawn");

    // Interrupt accepted, exit unconfirmed (E15 semantics on the exec side).
    assert_eq!(handle.inspect(), ExecProbe::Running);
    // The child must be up before the ladder fires: a SIGINT delivered
    // while the child is still in exec/dyld startup kills it under the
    // default disposition — before agent-fake installs its
    // ignore_interrupt handler — and the run classifies as a plain
    // Exited{code: None} instead of exercising the force-kill ladder.
    // First protocol line (the marker IS the readiness signal).
    let ready_by = std::time::Instant::now() + Duration::from_secs(5);
    while !handle.take_output().contains("\"started\"") {
        assert!(
            std::time::Instant::now() < ready_by,
            "child never emitted its started marker"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let grace_interrupt = Duration::from_millis(300);
    let grace_kill = Duration::from_millis(800);
    let confirmed = handle
        .stop(grace_interrupt, grace_kill)
        .await
        .expect("stop");

    match confirmed {
        iyagi_termd_lib::exec::ConfirmedExit::Killed { elapsed_ms } => {
            // The ladder spent the interrupt grace (no graceful exit on
            // piped children) and the terminate step ended it well inside
            // grace sums + slack.
            assert!(elapsed_ms >= 280, "interrupt grace honored: {elapsed_ms}ms");
            let budget = (grace_interrupt + grace_kill + Duration::from_secs(10)).as_millis();
            assert!(
                elapsed_ms < budget,
                "kill ladder took {elapsed_ms}ms, budget {budget}ms"
            );
        }
        other => panic!("expected ConfirmedExit::Killed, got {other:?}"),
    }

    // 02 §9: termination is confirmed before the reservation is released.
    assert!(!supervisor.ledger().is_active(&exec_id));
    assert_eq!(supervisor.inspect(&exec_id), ExecProbe::Absent);
    assert_eq!(
        states_of(&log),
        vec![ExecState::Prepared, ExecState::Spawned, ExecState::Exited]
    );
    let exited = log.lock().unwrap().last().cloned().expect("exited record");
    assert!(
        exited.ended_at.is_some(),
        "Exited only after the confirmed reap"
    );
    // Never a success code for a force-killed child (None on Unix signal
    // kills, the terminate status on Windows).
    assert_ne!(exited.exit_code, Some(0));
}

// ---- 4. flood output: bounded memory, sink sees everything -----------------

#[tokio::test]
async fn flood_stdout_is_spool_capped_but_sink_complete_and_run_succeeds() {
    let seen: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&seen);
    let opts = RequestOpts {
        cwd: std::env::temp_dir(),
        sink: Arc::new(move |kind, line| {
            if kind == StreamKind::Stdout {
                counter.fetch_add(line.len() as u64, Ordering::Relaxed);
            }
        }),
        validate_path: None,
    };
    let (persist, _log) = recording_persist();
    let supervisor = ExecSupervisor::new(admission_config(), persist, healthy_host());
    let script = FakeScript {
        steps: vec![
            FakeStep::FloodStdout {
                bytes: 3 * 1024 * 1024,
            },
            FakeStep::Result {
                value: term_contracts::mission::types::ProviderResult::Report {
                    report_text: "flooded".into(),
                    knowledge: Vec::new(),
                },
            },
        ],
        ignore_interrupt: false,
        exit_late_ms: 0,
    };
    let request = spawn_request(&fixture_bin(), &script, opts);
    let handle = supervisor.spawn(request).await.expect("spawn");

    let exit = handle.wait().await.expect("wait");
    assert_eq!(exit.code, Some(0), "flood itself must not fail the run");

    let (total, retained, cuts) = handle.stream_stats(StreamKind::Stdout);
    assert!(
        total >= 3 * 1024 * 1024,
        "sink saw the whole stream: {total}"
    );
    assert_eq!(seen.load(Ordering::Relaxed), total, "sink is lossless");
    assert!(retained <= DEFAULT_SPOOL_BYTES, "spool capped: {retained}");
    assert!(retained > 0, "recent tail retained");
    assert_eq!(cuts, 0, "64 KiB lines are legal");
    assert_eq!(handle.output_verdict(), OutputVerdict::Valid);
    // Diagnostics retained for failure reporting even on success paths.
    assert!(handle.take_diagnostics().contains("agent-fake"));
}

// ---- 4b. newline-less 1 MiB+ stream → cut + RESULT_INVALID verdict ---------

#[tokio::test]
async fn no_newline_stream_is_cut_and_invalid() {
    let (persist, _log) = recording_persist();
    let supervisor = ExecSupervisor::new(admission_config(), persist, healthy_host());
    let script = FakeScript {
        steps: vec![
            FakeStep::Activity {
                text: "before the wall".into(),
            },
            FakeStep::NoNewline {
                bytes: (1024 * 1024 + 512 * 1024) as u64,
            },
        ],
        ignore_interrupt: false,
        exit_late_ms: 0,
    };
    let request = spawn_request(&fixture_bin(), &script, RequestOpts::default());
    let handle = supervisor.spawn(request).await.expect("spawn");
    let exit = handle.wait().await.expect("wait");
    assert_eq!(exit.code, Some(0));

    // 03 §2: the input without a newline is cut at the 1 MiB cap and the
    // run must be marked RESULT_INVALID when finalized.
    let (total, retained, cuts) = handle.stream_stats(StreamKind::Stdout);
    assert_eq!(cuts, 1, "exactly the cap cut");
    assert!(
        total >= 1024 * 1024,
        "cut bytes flushed to the sink: {total}"
    );
    assert!(retained <= DEFAULT_SPOOL_BYTES);
    match handle.output_verdict() {
        OutputVerdict::Invalid { cuts } => assert_eq!(cuts, 1),
        OutputVerdict::Valid => panic!("newline-less overflow must invalidate the run"),
    }
}

// ---- 5. E11: stale fencing token late callback ------------------------------

#[tokio::test]
async fn e11_stale_fencing_token_late_events_are_counted_and_dropped() {
    let clock = ManualClock::new(0);
    let adapter = FakeAdapter::in_process(
        Arc::clone(&clock) as Arc<dyn iyagi_termd_lib::agent_runtime::fake::FakeClock>
    );
    let mut stream = adapter.subscribe();
    let gate = Arc::clone(stream.gate());
    let run = run_start();
    let run_id = run.run_id.clone();
    let script = FakeScript {
        steps: vec![
            FakeStep::Started {
                session_id: None,
                turn_id: None,
            },
            FakeStep::Activity {
                text: "before takeover".into(),
            },
            FakeStep::Delay { ms: 10 },
            FakeStep::Usage {
                input_tokens: Some(10),
                output_tokens: None,
                cost_usd_micros: None,
            },
            FakeStep::Result {
                value: term_contracts::mission::types::ProviderResult::Report {
                    report_text: "stale final".into(),
                    knowledge: Vec::new(),
                },
            },
        ],
        // Cancel is accepted but the engine keeps playing (03 §7
        // cancel-accepted-but-alive), so late events really arrive.
        ignore_interrupt: true,
        exit_late_ms: 0,
    };
    adapter.start_scripted(run, script).expect("start");

    let first = stream
        .next_timeout(Duration::from_secs(5))
        .await
        .expect("started");
    assert!(matches!(first, AdapterEvent::Started { .. }));
    let second = stream
        .next_timeout(Duration::from_secs(5))
        .await
        .expect("activity");
    assert!(matches!(second, AdapterEvent::Activity { .. }));

    // A new DB actor takes over: the original token's callbacks are void.
    assert_eq!(gate.advance(&run_id, 9), Some(9));
    assert!(matches!(
        adapter.interrupt(&run_id),
        iyagi_termd_lib::agent_runtime::CancelReceipt::Accepted
    ));
    clock.advance_to(100);

    // Usage + Result still arrive from the engine carrying token 1: they
    // are counted and dropped; no state change is visible through the
    // stream.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if adapter.inspect(&run_id)
            == (iyagi_termd_lib::agent_runtime::RunProbe::Finished { exit: None })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        adapter.inspect(&run_id),
        iyagi_termd_lib::agent_runtime::RunProbe::Finished { exit: None },
        "engine finished playing"
    );
    assert!(
        stream.try_next().is_none(),
        "no event with the stale token is delivered"
    );
    assert_eq!(
        gate.dropped_stale(),
        2,
        "late usage + late result both counted"
    );
    assert_eq!(gate.dropped_stale() + gate.dropped_late(), 2);
}

// ---- 6. read-only violation via the capture hook ----------------------------

#[tokio::test]
async fn declared_write_outside_scope_is_reported_as_violation() {
    let dir = tempfile::tempdir().expect("workspace dir");
    let outside = tempfile::tempdir().expect("outside dir");
    let allowed_root = dir.path().to_path_buf();
    let validator: PathValidator = {
        let allowed_root = allowed_root.clone();
        Arc::new(
            move |path: &str, _bytes: u64| match Path::new(path).strip_prefix(&allowed_root) {
                Ok(_) => PathVerdict::Allow,
                Err(_) => PathVerdict::Deny,
            },
        )
    };
    let (persist, _log) = recording_persist();
    let supervisor = ExecSupervisor::new(admission_config(), persist, healthy_host());
    let script = FakeScript {
        steps: vec![
            FakeStep::FileWrite {
                path: allowed_root.join("ok.txt").to_string_lossy().into_owned(),
                bytes: 8,
            },
            FakeStep::FileWrite {
                path: outside
                    .path()
                    .join("secret.txt")
                    .to_string_lossy()
                    .into_owned(),
                bytes: 8,
            },
            FakeStep::Result {
                value: term_contracts::mission::types::ProviderResult::Report {
                    report_text: "wrote things".into(),
                    knowledge: Vec::new(),
                },
            },
        ],
        ignore_interrupt: false,
        exit_late_ms: 0,
    };
    let opts = RequestOpts {
        cwd: dir.path().to_path_buf(),
        sink: Arc::new(|_, _| {}),
        validate_path: Some(validator),
    };
    let request = spawn_request(&fixture_bin(), &script, opts);
    let handle = supervisor.spawn(request).await.expect("spawn");
    let exit = handle.wait().await.expect("wait");
    assert_eq!(exit.code, Some(0));

    // The hook reports only the out-of-scope write; full capture/O06 takes
    // the real diff. Both files exist on disk (observation, not blocking).
    let violations = handle.take_violations();
    assert_eq!(
        violations.len(),
        1,
        "exactly the out-of-scope write: {violations:?}"
    );
    assert_eq!(
        Path::new(&violations[0].path),
        outside.path().join("secret.txt"),
        "reported path is the declared out-of-scope write"
    );
    assert_eq!(violations[0].bytes, 8);
    assert!(allowed_root.join("ok.txt").is_file());
    assert!(outside.path().join("secret.txt").is_file());
    assert!(
        handle.take_violations().is_empty(),
        "violations are taken once"
    );
}

// ---- 7. admission: exec ledger refuses when the host is exhausted ----------

#[tokio::test]
async fn admission_denial_blocks_spawn_without_touching_the_child() {
    let (persist, log) = recording_persist();
    let supervisor = ExecSupervisor::new(admission_config(), persist, healthy_host());
    supervisor.update_host(AdmissionHost {
        total_bytes: 16 << 30,
        available_bytes: Some(1 << 30), // below reserve + reservation
        sample_age_ms: 0,
        reconciliation_required: false,
        pressure: PressureLevel::Normal,
    });
    let script = FakeScript::happy();
    let request = spawn_request(&fixture_bin(), &script, RequestOpts::default());
    let err = match supervisor.spawn(request).await {
        Ok(_) => panic!("headroom cannot fit the reservation; spawn must be refused"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        iyagi_termd_lib::exec::ExecError::AdmissionDenied { .. }
    ));
    assert!(
        log.lock().unwrap().is_empty(),
        "no ExecRecord before admission"
    );
    assert_eq!(supervisor.ledger().active_count(), 0);
}

// Real private launch gate with injectable durable-store failures.
#[derive(Default)]
struct FaultStore {
    log: PersistLog,
    fail_prepare: AtomicBool,
    fail_spawn: AtomicBool,
    fail_exit: AtomicBool,
    exit_attempts: AtomicU64,
    marker: Option<PathBuf>,
}
impl iyagi_termd_lib::exec::persistence::ExecPersistence for FaultStore {
    fn recovery_records(&self, _previous: &[Id]) -> std::io::Result<Vec<ExecRecord>> {
        // This fixture creates a fresh supervisor with no previous daemon.
        Ok(vec![])
    }
    fn prepare(
        &self,
        mut record: ExecRecord,
        body: &[u8],
    ) -> std::io::Result<term_contracts::mission::types::ArtifactRef> {
        if self.fail_prepare.load(Ordering::Acquire) {
            return Err(std::io::Error::other("prepare unavailable"));
        }
        let manifest: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert!(manifest.get("env_keys").is_some());
        assert!(manifest.get("env_overrides").is_none());
        record.launch_manifest_ref.id = Id::generate();
        let reference = record.launch_manifest_ref.clone();
        self.log.lock().unwrap().push(record);
        Ok(reference)
    }
    fn update(&self, record: ExecRecord) -> std::io::Result<()> {
        if record.state == ExecState::Spawned {
            assert!(record.identity.is_some());
            assert!(record.group_reference.is_some());
            if let Some(marker) = &self.marker {
                assert!(!marker.exists(), "target ran before ownership commit");
            }
            if self.fail_spawn.load(Ordering::Acquire) {
                return Err(std::io::Error::other("ownership unavailable"));
            }
        }
        if record.state == ExecState::Exited {
            self.exit_attempts.fetch_add(1, Ordering::AcqRel);
            if self.fail_exit.load(Ordering::Acquire) {
                return Err(std::io::Error::other("exit unavailable"));
            }
        }
        self.log.lock().unwrap().push(record);
        Ok(())
    }
}
fn gated_supervisor(store: Arc<FaultStore>, directory: &Path) -> ExecSupervisor {
    let supervisor = ExecSupervisor::persistent(
        admission_config(),
        store,
        healthy_host(),
        iyagi_termd_lib::exec::gated::GateConfig {
            helper_program: daemon_bin(),
            directory: directory.into(),
            platform: Arc::from(term_platform::group::select_backend()),
            timeout: Duration::from_secs(5),
        },
    );
    supervisor.refresh_recovery().unwrap();
    supervisor
}
fn marker_request(marker: &Path) -> SpawnRequest {
    let mut request = spawn_request(&fixture_bin(), &FakeScript::happy(), RequestOpts::default());
    request.argv = vec![
        "gate-observer".into(),
        "--marker".into(),
        marker.to_string_lossy().into_owned(),
    ];
    request.stdin = None;
    request
}
async fn eventually(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "condition did not become true");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
#[tokio::test]
async fn gated_target_starts_only_after_owned_identity_commit() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let store = Arc::new(FaultStore {
        marker: Some(marker.clone()),
        ..Default::default()
    });
    let supervisor = gated_supervisor(store.clone(), dir.path());
    let handle = supervisor.spawn(marker_request(&marker)).await.unwrap();
    assert_eq!(handle.wait().await.unwrap().code, Some(0));
    assert!(marker.exists());
    assert_eq!(
        states_of(&store.log),
        vec![ExecState::Prepared, ExecState::Spawned, ExecState::Exited]
    );
    let records = store.log.lock().unwrap();
    assert_eq!(
        records[0].launch_manifest_ref,
        records[2].launch_manifest_ref
    );
    assert_eq!(records[1].identity, records[2].identity);
    assert_eq!(records[1].group_reference, records[2].group_reference);
    assert_eq!(records[1].group_identity, records[2].group_identity);
    if cfg!(target_os = "linux")
        && std::env::var("IYAGI_CGROUP_REQUIRE_DELEGATION").as_deref() == Ok("1")
    {
        assert!(
            records[1].group_identity.is_some(),
            "gate released without native recovery proof"
        );
    }
    assert_eq!(records[1].started_at, records[2].started_at);
    assert_eq!(supervisor.ledger().active_count(), 0);
}
#[tokio::test]
async fn gated_prepare_failure_cannot_start_target() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("never");
    let store = Arc::new(FaultStore {
        fail_prepare: AtomicBool::new(true),
        ..Default::default()
    });
    let supervisor = gated_supervisor(store.clone(), dir.path());
    assert!(supervisor.spawn(marker_request(&marker)).await.is_err());
    assert!(!marker.exists());
    assert!(store.log.lock().unwrap().is_empty());
    assert_eq!(supervisor.ledger().active_count(), 0);
}
#[tokio::test]
async fn gated_ownership_failure_aborts_helper_and_confirms_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("never");
    let store = Arc::new(FaultStore {
        fail_spawn: AtomicBool::new(true),
        marker: Some(marker.clone()),
        ..Default::default()
    });
    let supervisor = gated_supervisor(store.clone(), dir.path());
    assert!(supervisor.spawn(marker_request(&marker)).await.is_err());
    assert!(!marker.exists());
    assert_eq!(
        states_of(&store.log),
        vec![ExecState::Prepared, ExecState::Exited]
    );
    let records = store.log.lock().unwrap();
    let identity = records.last().unwrap().identity.as_ref().unwrap();
    assert!(term_platform::process_identity(identity.pid).is_none_or(|p| !p.same_process(identity)));
    assert_eq!(supervisor.ledger().active_count(), 0);
}
#[tokio::test]
async fn gated_completion_outage_retains_reservation_until_retry_commits() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore {
        fail_exit: AtomicBool::new(true),
        ..Default::default()
    });
    let supervisor = gated_supervisor(store.clone(), dir.path());
    let handle = supervisor
        .spawn(marker_request(&dir.path().join("ran")))
        .await
        .unwrap();
    eventually(|| store.exit_attempts.load(Ordering::Acquire) > 0).await;
    assert!(supervisor.ledger().is_active(handle.exec_id()));
    assert_eq!(handle.inspect(), ExecProbe::Running);
    assert_eq!(
        states_of(&store.log),
        vec![ExecState::Prepared, ExecState::Spawned]
    );
    let native_path = {
        let records = store.log.lock().unwrap();
        records[1]
            .group_identity
            .as_ref()
            .and(records[1].group_reference.clone())
            .map(PathBuf::from)
    };
    if let Some(path) = &native_path {
        assert!(path.exists(), "uncommitted exit lost its recovery evidence");
    }
    store.fail_exit.store(false, Ordering::Release);
    handle.wait().await.unwrap();
    if let Some(path) = &native_path {
        eventually(|| !path.exists()).await;
    }
    assert_eq!(supervisor.ledger().active_count(), 0);
    assert_eq!(
        states_of(&store.log)
            .iter()
            .filter(|s| **s == ExecState::Exited)
            .count(),
        1
    );
}
#[tokio::test]
async fn gated_dropped_handle_still_cleans_up_owned_child() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let supervisor = gated_supervisor(store.clone(), dir.path());
    let mut request = marker_request(&dir.path().join("unused"));
    request.argv = vec!["exit".into(), "--delay-ms".into(), "30000".into()];
    let handle = supervisor.spawn(request).await.unwrap();
    let identity = handle.identity().unwrap().clone();
    drop(handle);
    eventually(|| supervisor.ledger().active_count() == 0).await;
    assert!(
        term_platform::process_identity(identity.pid).is_none_or(|p| !p.same_process(&identity))
    );
    assert_eq!(states_of(&store.log).last(), Some(&ExecState::Exited));
}
#[tokio::test]
async fn gated_native_group_keeps_descendants_owned_after_root_exit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let supervisor = gated_supervisor(store.clone(), dir.path());
    let mut request = marker_request(&dir.path().join("unused"));
    request.argv = vec![
        "tree",
        "--children",
        "2",
        "--depth",
        "1",
        "--hold-ms",
        "10000",
        "--root-early-exit-ms",
        "1500",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let handle = supervisor.spawn(request).await.unwrap();
    // Separate owned process is a bystander to this cancellation.
    let mut bystander = std::process::Command::new(fixture_bin())
        .args(["exit", "--delay-ms", "30000"])
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(handle.inspect(), ExecProbe::Running);
    assert!(supervisor.ledger().is_active(handle.exec_id()));
    let started = Instant::now();
    handle
        .stop(Duration::from_millis(100), Duration::from_millis(100))
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "descendants were not terminated promptly"
    );
    assert!(
        bystander.try_wait().unwrap().is_none(),
        "unrelated child was killed"
    );
    bystander.kill().unwrap();
    bystander.wait().unwrap();
    assert_eq!(supervisor.ledger().active_count(), 0);
}

#[tokio::test]
async fn supervised_claude_detects_eof_and_delays_result_until_durable_completion() {
    use iyagi_termd_lib::agent_runtime::claude::{claude_binding, ClaudePrintAdapter};
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore {
        fail_exit: AtomicBool::new(true),
        ..Default::default()
    });
    let supervisor = Arc::new(gated_supervisor(store.clone(), dir.path()));
    let adapter = ClaudePrintAdapter::supervised(
        supervisor.clone(),
        tokio::runtime::Handle::current(),
        dir.path().join("configs"),
    );
    let mut run = run_start();
    run.binding = claude_binding(
        fixture_bin().to_str().unwrap(),
        term_contracts::mission::types::AuthRoute::ApiKey,
    );
    let id = run.run_id.clone();
    let mut stream = adapter.subscribe();
    adapter.start(run).unwrap();
    eventually(|| store.exit_attempts.load(Ordering::Acquire) > 0).await;
    // Exercise the source's former two-second timeout during a store outage.
    tokio::time::sleep(Duration::from_millis(2200)).await;
    while let Some(event) = stream.try_next() {
        assert!(
            !matches!(
                event,
                AdapterEvent::Result { .. } | AdapterEvent::Failed { .. }
            ),
            "storage outage emitted terminal result: {event:?}"
        );
    }
    assert_eq!(supervisor.ledger().active_count(), 1);
    store.fail_exit.store(false, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "Claude never finished after stdout EOF"
        );
        match stream.next_timeout(Duration::from_millis(100)).await {
            Some(AdapterEvent::Result { .. }) => break,
            Some(AdapterEvent::Failed { code, message, .. }) => {
                panic!("unexpected failure {code:?}: {message}")
            }
            _ => {}
        }
    }
    assert!(matches!(
        adapter.close(&id),
        iyagi_termd_lib::agent_runtime::CancelReceipt::Confirmed { .. }
    ));
    assert_eq!(supervisor.ledger().active_count(), 0);
}
#[tokio::test]
async fn supervised_claude_close_does_not_claim_confirmation_during_store_outage() {
    use iyagi_termd_lib::agent_runtime::claude::{claude_binding, ClaudePrintAdapter};
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore {
        fail_exit: AtomicBool::new(true),
        ..Default::default()
    });
    let supervisor = Arc::new(gated_supervisor(store.clone(), dir.path()));
    let adapter = ClaudePrintAdapter::supervised(
        supervisor.clone(),
        tokio::runtime::Handle::current(),
        dir.path().join("configs"),
    );
    let mut run = run_start();
    run.binding = claude_binding(
        fixture_bin().to_str().unwrap(),
        term_contracts::mission::types::AuthRoute::ApiKey,
    );
    run.prompt_stdin = "hold".into();
    let id = run.run_id.clone();
    let mut stream = adapter.subscribe();
    adapter.start(run).unwrap();
    assert!(matches!(
        stream.next_timeout(Duration::from_secs(10)).await,
        Some(AdapterEvent::Started { .. })
    ));
    // close is a synchronous port and must not block this current-thread
    // runtime while its pipe pumps need to consume EOF.
    let closing = adapter.clone();
    let closing_id = id.clone();
    let receipt = tokio::task::spawn_blocking(move || closing.close(&closing_id))
        .await
        .unwrap();
    assert_eq!(
        receipt,
        iyagi_termd_lib::agent_runtime::CancelReceipt::Accepted
    );
    assert_eq!(supervisor.ledger().active_count(), 1);
    store.fail_exit.store(false, Ordering::Release);
    eventually(|| supervisor.ledger().active_count() == 0).await;
    assert!(matches!(
        adapter.close(&id),
        iyagi_termd_lib::agent_runtime::CancelReceipt::Confirmed { .. }
    ));
}

#[tokio::test]
async fn gated_stop_honors_interrupt_grace_before_native_termination() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let supervisor = gated_supervisor(store, dir.path());
    let script = FakeScript {
        steps: vec![FakeStep::Started {
            session_id: None,
            turn_id: None,
        }],
        ignore_interrupt: true,
        exit_late_ms: 30000,
    };
    let handle = supervisor
        .spawn(spawn_request(
            &fixture_bin(),
            &script,
            RequestOpts::default(),
        ))
        .await
        .unwrap();
    eventually(|| handle.take_output().contains("started")).await;
    let started = Instant::now();
    handle
        .stop(Duration::from_millis(300), Duration::from_millis(500))
        .await
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(280),
        "native termination skipped interrupt grace"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(supervisor.ledger().active_count(), 0);
}

fn codex_start(prompt: &str) -> RunStart {
    let mut start = run_start();
    start.binding.runtime = term_contracts::mission::types::RuntimeKind::Codex;
    start.binding.auth_route = term_contracts::mission::types::AuthRoute::Subscription;
    start.binding.program = fixture_bin().to_string_lossy().into_owned();
    start.binding.provider_id = "openai".into();
    start.binding.model_id = "fixture-model".into();
    start.binding.capabilities.steer.supported = true;
    start.prompt_stdin = prompt.into();
    start
}
async fn next_codex_terminal(
    stream: &mut iyagi_termd_lib::agent_runtime::EventStream,
) -> AdapterEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "Codex terminal event timed out");
        if let Some(event) = stream.next_timeout(Duration::from_millis(100)).await {
            if event.is_terminal() {
                return event;
            }
        }
    }
}
#[tokio::test]
async fn supervised_codex_owns_duplex_process_and_waits_for_durable_cleanup() {
    use iyagi_termd_lib::agent_runtime::{codex::CodexAdapter, CancelReceipt, RunProbe};
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore {
        fail_exit: AtomicBool::new(true),
        ..Default::default()
    });
    let supervisor = Arc::new(gated_supervisor(store.clone(), dir.path()));
    let adapter = CodexAdapter::supervised(supervisor.clone(), tokio::runtime::Handle::current());
    let start = codex_start("fixture-report");
    let id = start.run_id.clone();
    let mut events = adapter.subscribe();
    adapter.start(start.clone()).unwrap();
    assert!(
        adapter.start(start).is_err(),
        "duplicate start created a second process"
    );
    let result = next_codex_terminal(&mut events).await;
    assert!(matches!(result, AdapterEvent::Result { .. }), "{result:?}");
    eventually(|| store.exit_attempts.load(Ordering::Acquire) > 0).await;
    assert_eq!(supervisor.ledger().active_count(), 1);
    assert_eq!(
        adapter.inspect(&id),
        RunProbe::Running,
        "protocol completion is not OS/store completion"
    );
    assert_eq!(adapter.close(&id), CancelReceipt::Accepted);
    store.fail_exit.store(false, Ordering::Release);
    eventually(|| supervisor.ledger().active_count() == 0).await;
    assert!(matches!(adapter.inspect(&id), RunProbe::Finished { .. }));
    assert!(matches!(
        adapter.close(&id),
        CancelReceipt::Confirmed { .. }
    ));
    let records = store.log.lock().unwrap();
    assert_eq!(records[0].state, ExecState::Prepared);
    assert_eq!(records[1].state, ExecState::Spawned);
    assert_eq!(records.last().unwrap().identity, records[1].identity);
    assert_eq!(records.last().unwrap().state, ExecState::Exited);
}

#[tokio::test]
async fn authenticated_codex_uses_a_private_key_and_releases_config_after_durable_cleanup() {
    use iyagi_termd_lib::{
        agent_runtime::codex::CodexAdapter,
        connections::{ConnectionPreset, ConnectionStore},
    };
    use term_contracts::mission::types::{AuthRoute, ProviderResult};
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore {
        fail_exit: AtomicBool::new(true),
        ..Default::default()
    });
    struct Restore(Arc<FaultStore>);
    impl Drop for Restore {
        fn drop(&mut self) {
            self.0.fail_exit.store(false, Ordering::Release);
        }
    }
    let _restore = Restore(store.clone());
    let supervisor = Arc::new(gated_supervisor(store.clone(), dir.path()));
    let connections = Arc::new(ConnectionStore::with_credentials(
        dir.path().join("connections"),
        credentials::MemoryCredentials::new(),
    ));
    let key = "fake-codex-\"quoted\"-test-key";
    let info = connections
        .create(
            ConnectionPreset::CodexApi,
            zeroize::Zeroizing::new(key.into()),
        )
        .unwrap();
    let config_root = dir.path().join("configs");
    let adapter = CodexAdapter::authenticated(
        supervisor.clone(),
        tokio::runtime::Handle::current(),
        Some(connections.clone()),
        config_root.clone(),
    );
    let mut start = codex_start("fixture-auth-echo");
    start.binding.auth_route = AuthRoute::ApiKey;
    start.binding.credential_ref = Some(info.credential_ref);
    start.binding.endpoint_ref = Some(info.endpoint_ref.clone());
    let id = start.run_id.clone();
    let mut events = adapter.subscribe();
    adapter.start(start.clone()).unwrap();
    let result = next_codex_terminal(&mut events).await;
    match result {
        AdapterEvent::Result {
            result: ProviderResult::Report { report_text, .. },
            ..
        } => {
            assert!(report_text.contains("[redacted]"), "{report_text}");
            assert!(!report_text.contains(key));
        }
        other => panic!("{other:?}"),
    }
    eventually(|| store.exit_attempts.load(Ordering::Acquire) > 0).await;
    let private = std::fs::read_dir(&config_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(
        private.join("codex").is_dir(),
        "cleanup must wait for the durable exit"
    );
    assert!(!private.join("codex/auth.json").exists());
    assert_eq!(supervisor.ledger().active_count(), 1);
    store.fail_exit.store(false, Ordering::Release);
    eventually(|| supervisor.ledger().active_count() == 0 && !private.exists()).await;
    assert!(matches!(
        adapter.close(&id),
        iyagi_termd_lib::agent_runtime::CancelReceipt::Confirmed { .. }
    ));
    let count = store.log.lock().unwrap().len();
    connections.revoke(&info.endpoint_ref).unwrap();
    start.run_id = Id::generate();
    assert_eq!(
        adapter.start(start).err().unwrap().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        store.log.lock().unwrap().len(),
        count,
        "revoked credentials must be rejected before preparing an Exec"
    );
    assert_eq!(std::fs::read_dir(config_root).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit installed Codex metadata smoke; requires IYAGI_CODEX_BIN and IYAGI_CODEX_VERSION; no model requests"]
async fn installed_codex_ephemeral_auth_metadata_uses_the_production_peer() {
    use iyagi_termd_lib::{
        agent_runtime::codex::{PeerEvent, ProtocolPeer, SupervisedPeer},
        connections::{ConnectionPreset, ConnectionStore},
    };
    use term_contracts::mission::types::AuthRoute;
    let program = std::env::var("IYAGI_CODEX_BIN").expect("explicit installed Codex path");
    let version = std::env::var("IYAGI_CODEX_VERSION").expect("explicit expected version");
    assert!(Path::new(&program).is_absolute());
    let output = std::process::Command::new(&program)
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("codex-cli {version}")
    );
    let dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(gated_supervisor(
        Arc::new(FaultStore::default()),
        dir.path(),
    ));
    let connections = ConnectionStore::with_credentials(
        dir.path().join("connections"),
        credentials::MemoryCredentials::new(),
    );
    let info = connections
        .create(
            ConnectionPreset::CodexApi,
            zeroize::Zeroizing::new("sk-fake-local-metadata-only".into()),
        )
        .unwrap();
    let mut start = codex_start("this prompt must never be sent");
    start.workspace = Some(dir.path().into());
    start.binding.program = program;
    start.binding.auth_route = AuthRoute::ApiKey;
    start.binding.credential_ref = Some(info.credential_ref);
    start.binding.endpoint_ref = Some(info.endpoint_ref);
    let peer = SupervisedPeer::spawn_authenticated(
        &start,
        &supervisor,
        &tokio::runtime::Handle::current(),
        Some(&connections),
        &dir.path().join("configs"),
    )
    .unwrap();
    // Close and reap even when metadata assertions or a receive timeout fail.
    struct Close(Arc<SupervisedPeer>);
    impl Drop for Close {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let _close = Close(peer.clone());
    let driver = peer.clone();
    let cwd = dir.path().to_path_buf();
    let mut worker = tokio::task::spawn_blocking(move || {
        let request = |id: u64, method: &str, params: serde_json::Value| {
            driver
                .send(&serde_json::json!({"id":id,"method":method,"params":params}))
                .unwrap();
            loop {
                match driver.recv() {
                    PeerEvent::Message(value) if value["id"] == id => {
                        assert!(
                            value.get("error").is_none(),
                            "metadata request failed: {method}"
                        );
                        break value["result"].clone();
                    }
                    PeerEvent::Message(_) => {}
                    event => panic!("metadata stream ended: {event:?}"),
                }
            }
        };
        let scope = driver.auth_scope().unwrap();
        let initialized = request(
            1,
            "initialize",
            serde_json::json!({"clientInfo":{"name":"iyagi-metadata-smoke","version":"0.1.0"},"capabilities":{}}),
        );
        assert!(scope.verify_initialize(&initialized));
        let home = PathBuf::from(initialized["codexHome"].as_str().unwrap());
        driver
            .send(&serde_json::json!({"method":"initialized","params":{}}))
            .unwrap();
        let config = request(
            2,
            "config/read",
            serde_json::json!({"includeLayers":false,"cwd":cwd}),
        );
        assert!(scope.verify_config(&config["config"]));
        let key = driver.take_api_key().unwrap();
        assert!(driver.take_api_key().is_none());
        let login = request(
            3,
            "account/login/start",
            serde_json::json!({"type":"apiKey","apiKey":key.as_str()}),
        );
        assert_eq!(login["type"], "apiKey");
        let account = request(4, "account/read", serde_json::json!({}));
        assert_eq!(account["account"]["type"], "apiKey");
        assert_eq!(account["requiresOpenaiAuth"], true);
        assert!(!home.join("auth.json").exists());
        home
    });
    let outcome = tokio::time::timeout(Duration::from_secs(20), &mut worker).await;
    peer.close();
    eventually(|| supervisor.ledger().active_count() == 0 && peer.cleanup_confirmed()).await;
    if outcome.is_err() {
        let _ = worker.await;
    }
    let home = outcome.expect("metadata timeout").expect("metadata driver");
    eventually(|| !home.exists()).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_codex_approval_and_steer_use_the_open_stdin_channel() {
    use iyagi_termd_lib::agent_runtime::{codex::CodexAdapter, DeliveryReceipt};
    for mode in ["fixture-approval", "fixture-hold"] {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FaultStore::default());
        let supervisor = Arc::new(gated_supervisor(store, dir.path()));
        let adapter =
            CodexAdapter::supervised(supervisor.clone(), tokio::runtime::Handle::current());
        let start = codex_start(mode);
        let id = start.run_id.clone();
        let mut events = adapter.subscribe();
        adapter.start(start).unwrap();
        loop {
            let event = events.next_timeout(Duration::from_secs(10)).await.unwrap();
            if matches!(
                event,
                AdapterEvent::ApprovalRequested { .. } | AdapterEvent::Activity { .. }
            ) {
                break;
            }
            assert!(
                matches!(
                    event,
                    AdapterEvent::Started { .. } | AdapterEvent::ModelObserved { .. }
                ),
                "unexpected {event:?}"
            );
        }
        let sender = adapter.clone();
        let run_id = id.clone();
        let receipt = tokio::task::spawn_blocking(move || {
            if mode == "fixture-approval" {
                sender.answer(&run_id, "fixture-approval", "approve")
            } else {
                sender.send_message(&run_id, "follow-up received over stdin")
            }
        })
        .await
        .unwrap();
        assert!(
            matches!(receipt, DeliveryReceipt::Delivered { .. }),
            "{receipt:?}"
        );
        if mode == "fixture-approval" {
            assert!(
                matches!(
                    adapter.answer(&id, "fixture-approval", "approve"),
                    DeliveryReceipt::Rejected { .. }
                ),
                "approval replay was resent"
            );
        }
        let result = next_codex_terminal(&mut events).await;
        assert!(matches!(result, AdapterEvent::Result { .. }), "{result:?}");
        eventually(|| supervisor.ledger().active_count() == 0).await;
        let _ = adapter.close(&id);
    }
}
#[tokio::test]
async fn supervised_codex_interrupt_and_corrupt_output_always_clean_up() {
    use iyagi_termd_lib::agent_runtime::{codex::CodexAdapter, CancelReceipt};
    for mode in ["fixture-hold", "fixture-disconnect", "fixture-overcap"] {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FaultStore::default());
        let supervisor = Arc::new(gated_supervisor(store, dir.path()));
        let adapter =
            CodexAdapter::supervised(supervisor.clone(), tokio::runtime::Handle::current());
        let start = codex_start(mode);
        let id = start.run_id.clone();
        let mut events = adapter.subscribe();
        adapter.start(start).unwrap();
        if mode == "fixture-hold" {
            loop {
                let event = events.next_timeout(Duration::from_secs(10)).await.unwrap();
                if matches!(event, AdapterEvent::Activity { .. }) {
                    break;
                }
            }
            let stopping = adapter.clone();
            let stopping_id = id.clone();
            assert_eq!(
                tokio::task::spawn_blocking(move || stopping.interrupt(&stopping_id))
                    .await
                    .unwrap(),
                CancelReceipt::Accepted
            );
        }
        if mode != "fixture-hold" {
            let result = next_codex_terminal(&mut events).await;
            assert!(
                !matches!(result, AdapterEvent::Result { .. }),
                "invalid success: {result:?}"
            );
        }
        eventually(|| supervisor.ledger().active_count() == 0).await;
        assert!(matches!(
            adapter.close(&id),
            CancelReceipt::Confirmed { .. }
        ));
    }
}
#[tokio::test]
async fn supervised_codex_adapter_drop_cleans_a_quiet_owned_process() {
    use iyagi_termd_lib::agent_runtime::codex::CodexAdapter;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let supervisor = Arc::new(gated_supervisor(store, dir.path()));
    let adapter = CodexAdapter::supervised(supervisor.clone(), tokio::runtime::Handle::current());
    let mut events = adapter.subscribe();
    adapter.start(codex_start("fixture-hold")).unwrap();
    loop {
        let event = events.next_timeout(Duration::from_secs(10)).await.unwrap();
        if matches!(event, AdapterEvent::Activity { .. }) {
            break;
        }
    }
    drop(adapter);
    eventually(|| supervisor.ledger().active_count() == 0).await;
}

#[tokio::test]
async fn interactive_exec_serializes_concurrent_frames_and_closes_on_eof() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let supervisor = gated_supervisor(store, dir.path());
    let collected = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let sink = collected.clone();
    let mut request = marker_request(&dir.path().join("unused"));
    request.argv = vec!["echo".into()];
    request.sink = Arc::new(move |kind, line| {
        if kind == StreamKind::Stdout {
            sink.lock().unwrap().push(line.to_vec());
        }
    });
    let handle = supervisor
        .spawn_interactive_on(request, &tokio::runtime::Handle::current())
        .unwrap();
    let input = handle.input().unwrap();
    let mut writers = Vec::new();
    for n in 0..8 {
        let pipe = input.clone();
        writers.push(tokio::task::spawn_blocking(move || {
            let frame = format!("{n}:{}\n", "한글".repeat(2000));
            pipe.write_blocking(frame.as_bytes()).unwrap();
        }));
    }
    for writer in writers {
        writer.await.unwrap();
    }
    input.close();
    assert_eq!(handle.wait().await.unwrap().code, Some(0));
    let lines = collected.lock().unwrap();
    assert_eq!(lines.len(), 8);
    let mut ids = std::collections::HashSet::new();
    for line in lines.iter() {
        let line = std::str::from_utf8(line).unwrap();
        let (id, body) = line.split_once(':').unwrap();
        ids.insert(id.parse::<u8>().unwrap());
        assert_eq!(
            body,
            format!("{}\n", "한글".repeat(2000)),
            "concurrent stdin frames interleaved"
        );
    }
    assert_eq!(ids.len(), 8);
}
#[tokio::test]
async fn interactive_write_timeout_is_unknown_and_cannot_be_retried_on_that_channel() {
    use iyagi_termd_lib::exec::input::InputError;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let supervisor = gated_supervisor(store, dir.path());
    let mut request = marker_request(&dir.path().join("unused"));
    request.argv = vec!["exit".into(), "--delay-ms".into(), "30000".into()];
    let handle = supervisor
        .spawn_interactive_on(request, &tokio::runtime::Handle::current())
        .unwrap();
    let input = handle.input().unwrap();
    let writing = input.clone();
    let writer = tokio::task::spawn_blocking(move || {
        writing.write_blocking(&vec![b'x'; iyagi_termd_lib::exec::MAX_LINE_BYTES])
    });
    assert_eq!(writer.await.unwrap(), Err(InputError::Unconfirmed));
    assert_eq!(
        input.write_blocking(b"do not resend\n"),
        Err(InputError::Closed)
    );
    assert_eq!(
        supervisor.ledger().active_count(),
        1,
        "failed input cannot prove target termination"
    );
    handle
        .stop(Duration::from_millis(100), Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(supervisor.ledger().active_count(), 0);
}

#[test]
fn interactive_cleanup_survives_runtime_shutdown_before_pumps_are_polled() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let supervisor = gated_supervisor(store, dir.path());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut request = marker_request(&dir.path().join("unused"));
    request.argv = vec!["echo".into()];
    let handle = supervisor
        .spawn_interactive_on(request, runtime.handle())
        .unwrap();
    // No block_on was called: queued async pumps have not been polled.
    runtime.shutdown_timeout(Duration::from_millis(100));
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let stopping = handle.clone();
    std::thread::spawn(move || {
        let result = stopping.stop_blocking(Duration::from_millis(100), Duration::from_millis(100));
        let _ = sender.send(result);
    });
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("cleanup waited forever for an unpolled pump")
        .unwrap();
    assert_eq!(supervisor.ledger().active_count(), 0);
    assert!(
        matches!(handle.output_verdict(), OutputVerdict::Invalid { .. }),
        "dropped output cannot be complete evidence"
    );
}
#[tokio::test]
async fn oversized_codex_frame_is_rejected_before_any_bytes_are_written() {
    use iyagi_termd_lib::agent_runtime::codex::{PeerEvent, ProtocolPeer, SupervisedPeer};
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let supervisor = Arc::new(gated_supervisor(store, dir.path()));
    let peer = SupervisedPeer::spawn(
        &codex_start("unused"),
        &supervisor,
        &tokio::runtime::Handle::current(),
    )
    .unwrap();
    let sending = peer.clone();
    let result = tokio::task::spawn_blocking(move || {
        assert!(sending
            .send(
                &serde_json::json!({"oversized":"x".repeat(iyagi_termd_lib::exec::MAX_LINE_BYTES)})
            )
            .is_err());
        sending
            .send(&serde_json::json!({"id":1,"method":"initialize","params":{}}))
            .unwrap();
        sending.recv()
    })
    .await
    .unwrap();
    assert!(
        matches!(result,PeerEvent::Message(ref value) if value["id"]==1 && value["result"]["userAgent"]=="iyagi-test-fixture"),
        "partial oversized bytes corrupted the stream: {result:?}"
    );
    peer.close();
    eventually(|| supervisor.ledger().active_count() == 0).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_gate_clears_inherited_environment_before_applying_explicit_values() {
    let store = Arc::new(FaultStore::default());
    let work = tempfile::tempdir().unwrap();
    let supervisor = gated_supervisor(store, work.path());
    let mut request = marker_request(&work.path().join("unused"));
    request.program = "/usr/bin/env".into();
    request.argv.clear();
    request.env_clear = true;
    request.env_overrides = [("IYAGI_EXPLICIT_ONLY".into(), "isolated-value".into())].into();
    let output = Arc::new(Mutex::new(Vec::new()));
    let sink = output.clone();
    request.sink = Arc::new(move |stream, bytes| {
        if stream == StreamKind::Stdout {
            sink.lock().unwrap().extend_from_slice(bytes);
        }
    });
    let exec = supervisor.spawn(request).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), exec.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.lock().unwrap().clone()).unwrap(),
        "IYAGI_EXPLICIT_ONLY=isolated-value\n"
    );
    assert_eq!(supervisor.ledger().active_count(), 0);
}
