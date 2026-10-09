//! O09 integration tests: Claude Code print adapter against recorded
//! stream-json transcripts (docs/orchestration/07-tickets.md O09,
//! contract docs/orchestration/03-adapters.md §4).
//!
//! Recorded-stream offline tests come first by design: the real CLI is
//! never spawned here (`PrintStreamSource` seam), and every protocol
//! shape is played back from versioned fixtures under
//! `src/agent_runtime/claude/fixtures/streams/`. The adapter is imported
//! from the lib (wired since O09 landed).

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use claude_mod::{
    claude_binding, parse_cli_version, ClaudeAdapterConfig, ClaudePrintAdapter, PrintProcess,
    RecordedSource, Recording, BASELINE_ARGV, EVIDENCE_CLI_VERSION, FIXTURE_HELP,
    FIXTURE_PRINT_HELP, FIXTURE_VERSION, NO_COMPATIBILITY_EVIDENCE,
};
use iyagi_termd_lib::agent_runtime::claude as claude_mod;
use iyagi_termd_lib::agent_runtime::{
    AdapterEvent, AgentAdapter, CancelReceipt, CancelRejected, DeliveryReceipt, QueuedReason,
    RunProbe, RunStart,
};
use term_contracts::mission::types::{AuthRoute, Id};
use term_contracts::mission::MissionErrorCode;

const SUCCESS: &str =
    include_str!("../src/agent_runtime/claude/fixtures/streams/success.iyagi.jsonl");
const AUTH_FAILURE: &str =
    include_str!("../src/agent_runtime/claude/fixtures/streams/auth-failure.iyagi.jsonl");
const EXIT0_NO_FINAL: &str =
    include_str!("../src/agent_runtime/claude/fixtures/streams/exit0-no-final.iyagi.jsonl");
const DISCONNECT: &str =
    include_str!("../src/agent_runtime/claude/fixtures/streams/disconnect.iyagi.jsonl");
const UNKNOWN_KINDS: &str =
    include_str!("../src/agent_runtime/claude/fixtures/streams/unknown-kinds.iyagi.jsonl");
const USAGE_NULL: &str =
    include_str!("../src/agent_runtime/claude/fixtures/streams/usage-null.iyagi.jsonl");
const INTERRUPTED: &str =
    include_str!("../src/agent_runtime/claude/fixtures/streams/interrupted.iyagi.jsonl");

fn recording(text: &str) -> Recording {
    Recording::parse(text).expect("recording fixture parses")
}

/// Adapter whose source factory pops the queued recordings in order —
/// the real CLI is never spawned.
fn recorded_adapter(recordings: Vec<Recording>, config_root: &Path) -> Arc<ClaudePrintAdapter> {
    let queue = Arc::new(Mutex::new(VecDeque::from(recordings)));
    let config = ClaudeAdapterConfig {
        config_root: config_root.to_path_buf(),
        secrets_root: None,
        source_factory: Arc::new(move |_run, _plan, cancel| {
            let next = queue
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .pop_front()
                .ok_or_else(|| std::io::Error::other("no recording queued"));
            Ok(PrintProcess {
                source: Box::new(RecordedSource::new(next?, cancel)),
                exec: None,
            })
        }),
        version_probe: Arc::new(|_| Some("2.1.263 (Claude Code)".to_string())),
    };
    ClaudePrintAdapter::with_config(config)
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
        binding: claude_binding("C:\\claude\\claude.exe", AuthRoute::ApiKey),
        context_path: std::env::temp_dir(),
        workspace: None,
        prompt_stdin: "do the recorded thing".to_string(),
    }
}

async fn wait_finished(adapter: &Arc<ClaudePrintAdapter>, run_id: &Id) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if adapter.inspect(run_id) == (RunProbe::Finished { exit: None }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("run did not reach Finished within 10s");
}

#[tokio::test]
async fn rejected_rate_limit_emits_a_window_without_terminating_the_current_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut sample = recording(SUCCESS);
    let reset = iyagi_termd_lib::agent_runtime::rate_limits::unix_millis() / 1000 + 60;
    for status in ["rejected", "allowed_warning", "allowed"] {
        sample.lines.insert(1,serde_json::to_vec(&serde_json::json!({"type":"rate_limit_event","rate_limit_info":{"status":status,"resetsAt":reset,"overageStatus":"rejected"}})).unwrap());
    }
    let adapter = recorded_adapter(vec![sample], dir.path());
    let mut stream = adapter.subscribe();
    let run = run_start();
    let id = run.run_id.clone();
    adapter.start(run).unwrap();
    let mut windows = vec![];
    loop {
        match stream
            .next_timeout(Duration::from_secs(5))
            .await
            .expect("provider event")
        {
            AdapterEvent::RateLimited { observation, .. } => windows.push(observation),
            AdapterEvent::Result { .. } => break,
            AdapterEvent::Failed { code, .. } => panic!("limit notice terminated run: {code:?}"),
            _ => {}
        }
    }
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].resets_at_unix_ms.get(), reset * 1000);
    wait_finished(&adapter, &id).await;
}

// ---- 1. success: deltas → envelope with usage → exit 0 ----------------------

#[tokio::test]
async fn success_stream_normalizes_started_activity_usage_result() {
    let config_root = tempfile::tempdir().expect("config root");
    // Pace the recording: without a per-line delay the reader thread can
    // drain the whole SUCCESS stream (and set `finished`) between our
    // Started await and send_message, flipping the receipt to NextRun.
    // Pacing keeps the run deterministically active for the steer
    // assertion — the same mechanism interrupt tests rely on.
    let mut paced = recording(SUCCESS);
    paced.line_delay_ms = 250;
    let adapter = recorded_adapter(vec![paced], config_root.path());
    let mut stream = adapter.subscribe();
    let run = run_start();
    let run_id = run.run_id.clone();
    adapter.start(run).expect("start");

    // Started carries the provider session id from system/init —
    // structured metadata only, never screen-scraped strings.
    match stream
        .next_timeout(Duration::from_secs(10))
        .await
        .expect("started")
    {
        AdapterEvent::Started {
            provider_session_id,
            provider_turn_id,
            ..
        } => {
            assert_eq!(provider_session_id.as_deref(), Some("claude-sess-0001"));
            assert_eq!(provider_turn_id, None);
        }
        other => panic!("expected Started, got {other:?}"),
    }
    // Print mode cannot steer the active turn: the receipt says so with a
    // reason, queued semantics stay engine-side.
    assert_eq!(
        adapter.send_message(&run_id, "please steer"),
        DeliveryReceipt::Queued {
            reason: QueuedReason::SteerUnsupported
        }
    );

    let mut chunks = Vec::new();
    let next = stream
        .next_timeout(Duration::from_secs(10))
        .await
        .expect("delta 1");
    let AdapterEvent::Activity { chunk, .. } = next else {
        panic!("expected Activity, got {next:?}")
    };
    chunks.push(chunk);
    let next = stream
        .next_timeout(Duration::from_secs(10))
        .await
        .expect("delta 2");
    let AdapterEvent::Activity { chunk, .. } = next else {
        panic!("expected Activity, got {next:?}")
    };
    chunks.push(chunk);
    assert_eq!(chunks, vec!["Hello ", "from claude print."]);

    // Usage arrives before the terminal event (nulls would stay null).
    match stream
        .next_timeout(Duration::from_secs(10))
        .await
        .expect("usage")
    {
        AdapterEvent::Usage {
            input_tokens,
            output_tokens,
            cost_usd_micros,
            ..
        } => {
            assert_eq!(input_tokens, Some(128));
            assert_eq!(output_tokens, Some(64));
            assert_eq!(cost_usd_micros, Some(2_500), "0.0025 USD → 2500 micros");
        }
        other => panic!("expected Usage, got {other:?}"),
    }

    match stream
        .next_timeout(Duration::from_secs(10))
        .await
        .expect("result")
    {
        AdapterEvent::Result { result, .. } => match result {
            term_contracts::mission::types::ProviderResult::Report { report_text, .. } => {
                assert_eq!(report_text, "Hello from claude print.");
            }
            other => panic!("expected Report, got {other:?}"),
        },
        other => panic!("expected Result, got {other:?}"),
    }
    // Post-terminal stream is quiet; the run reports Finished.
    assert!(stream.try_next().is_none());
    wait_finished(&adapter, &run_id).await;
    let stats = adapter.run_stats(&run_id).expect("stats recorded");
    assert_eq!(stats.unknown_event_kinds, 0);
    assert_eq!(stats.malformed_lines, 0);
    assert_eq!(stats.stream_event_deltas, 2);
    assert_eq!(
        adapter.answer(&run_id, "req-1", "yes"),
        DeliveryReceipt::Rejected {
            reason: "print mode has no approval reply channel (permission prompts are denied)"
        }
    );
    assert_eq!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { exit: None }
    );
}

// ---- 2. E19: exit 0 without a final result → RESULT_INVALID ------------------

#[tokio::test]
async fn exit_zero_without_final_envelope_is_result_invalid() {
    let config_root = tempfile::tempdir().expect("config root");
    let adapter = recorded_adapter(vec![recording(EXIT0_NO_FINAL)], config_root.path());
    let mut stream = adapter.subscribe();
    let run = run_start();
    adapter.start(run).expect("start");
    match stream
        .next_timeout(Duration::from_secs(10))
        .await
        .expect("started")
    {
        AdapterEvent::Started { .. } => {}
        other => panic!("expected Started, got {other:?}"),
    }
    loop {
        let event = stream
            .next_timeout(Duration::from_secs(10))
            .await
            .expect("terminal event");
        match event {
            AdapterEvent::Failed { code, message, .. } => {
                assert_eq!(code, MissionErrorCode::ResultInvalid, "{message}");
                assert!(message.contains("exit 0"), "E19 wording: {message}");
                return;
            }
            AdapterEvent::Activity { .. } => {}
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}

// ---- 3. auth failure before any envelope → AUTH_REQUIRED --------------------

#[tokio::test]
async fn structured_auth_retry_failure_maps_to_auth_required() {
    let config_root = tempfile::tempdir().expect("config root");
    let adapter = recorded_adapter(vec![recording(AUTH_FAILURE)], config_root.path());
    let mut stream = adapter.subscribe();
    let run = run_start();
    adapter.start(run).expect("start");
    loop {
        let event = stream
            .next_timeout(Duration::from_secs(10))
            .await
            .expect("event");
        match event {
            AdapterEvent::Failed { code, message, .. } => {
                assert_eq!(code, MissionErrorCode::AuthRequired, "{message}");
                assert!(
                    message.contains("authentication_failed"),
                    "structured slug surfaces in diagnostics: {message}"
                );
                return;
            }
            AdapterEvent::Started { .. } | AdapterEvent::Activity { .. } => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
}

// ---- 4. mid-stream disconnect (exit unobservable) → Disconnected ------------

#[tokio::test]
async fn mid_stream_disconnect_without_exit_is_disconnected_not_success() {
    let config_root = tempfile::tempdir().expect("config root");
    let adapter = recorded_adapter(vec![recording(DISCONNECT)], config_root.path());
    let mut stream = adapter.subscribe();
    adapter.start(run_start()).expect("start");
    loop {
        let event = stream
            .next_timeout(Duration::from_secs(10))
            .await
            .expect("event");
        match event {
            AdapterEvent::Disconnected { .. } => return,
            AdapterEvent::Started { .. } | AdapterEvent::Activity { .. } => {}
            other => panic!("transport disconnect must not be a result: {other:?}"),
        }
    }
}

// ---- 5. unknown event kinds are counted, never fatal ------------------------

#[tokio::test]
async fn unknown_event_kinds_and_malformed_lines_are_counted_not_fatal() {
    let config_root = tempfile::tempdir().expect("config root");
    let adapter = recorded_adapter(vec![recording(UNKNOWN_KINDS)], config_root.path());
    let mut stream = adapter.subscribe();
    let run = run_start();
    let run_id = run.run_id.clone();
    adapter.start(run).expect("start");
    wait_finished(&adapter, &run_id).await;
    let stats = adapter.run_stats(&run_id).expect("stats");
    assert_eq!(stats.unknown_event_kinds, 2, "future kinds skipped");
    assert_eq!(stats.malformed_lines, 1, "non-JSON line skipped");
    assert!(stats.lines >= 5);
    // The stream still terminated in a Result: tolerance is never fatal.
    let mut saw_result = false;
    while let Some(event) = stream.try_next() {
        match event {
            AdapterEvent::Result { result, .. } => {
                saw_result = true;
                assert!(matches!(
                    result,
                    term_contracts::mission::types::ProviderResult::Report { .. }
                ));
            }
            AdapterEvent::Usage { .. } | AdapterEvent::Started { .. } => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(saw_result, "run succeeded despite unknown kinds");
    assert_eq!(
        adapter.run_stats(&run_id).map(|s| s.unknown_event_kinds),
        Some(2)
    );
}

// ---- 6. interrupted run is never a result -----------------------------------

#[tokio::test]
async fn interrupted_run_never_emits_result_and_close_confirms() {
    let config_root = tempfile::tempdir().expect("config root");
    let adapter = recorded_adapter(vec![recording(INTERRUPTED)], config_root.path());
    let mut stream = adapter.subscribe();
    let run = run_start();
    let run_id = run.run_id.clone();
    adapter.start(run).expect("start");

    match stream
        .next_timeout(Duration::from_secs(10))
        .await
        .expect("started")
    {
        AdapterEvent::Started { .. } => {}
        other => panic!("expected Started, got {other:?}"),
    }
    // First delta observed → the recording still holds several paced
    // lines, so the interrupt deterministically lands mid-stream.
    match stream
        .next_timeout(Duration::from_secs(10))
        .await
        .expect("delta")
    {
        AdapterEvent::Activity { .. } => {}
        other => panic!("expected Activity, got {other:?}"),
    }
    assert_eq!(adapter.interrupt(&run_id), CancelReceipt::Accepted);
    // Interrupt requested: further messages queue for the next run.
    assert_eq!(
        adapter.send_message(&run_id, "late body"),
        DeliveryReceipt::Queued {
            reason: QueuedReason::NextRun
        }
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut failed = false;
    while Instant::now() < deadline {
        let Some(event) = stream.next_timeout(Duration::from_secs(5)).await else {
            continue;
        };
        match event {
            AdapterEvent::Failed { code, message, .. } => {
                assert_eq!(code, MissionErrorCode::OutcomeUnknown, "{message}");
                assert!(message.contains("interrupted"), "{message}");
                failed = true;
                break;
            }
            AdapterEvent::Result { .. } => panic!("interrupted run must not produce a Result"),
            AdapterEvent::Activity { .. } => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(
        failed,
        "interrupted run reached Failed (not a normal result)"
    );
    // Interrupting again is rejected — the run is already terminal.
    assert_eq!(
        adapter.interrupt(&run_id),
        CancelReceipt::Rejected {
            reason: CancelRejected::AlreadyTerminal
        }
    );
    wait_finished(&adapter, &run_id).await;
    assert_eq!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { exit: None }
    );
}

// ---- 7. usage null stays null (never zero) ----------------------------------

#[tokio::test]
async fn usage_null_is_preserved_as_none_never_zero() {
    let config_root = tempfile::tempdir().expect("config root");
    let adapter = recorded_adapter(vec![recording(USAGE_NULL)], config_root.path());
    let mut stream = adapter.subscribe();
    adapter.start(run_start()).expect("start");
    let mut usage: Option<(Option<u64>, Option<u64>, Option<u64>)> = None;
    let mut result_seen = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !result_seen {
        let Some(event) = stream.next_timeout(Duration::from_secs(5)).await else {
            continue;
        };
        match event {
            AdapterEvent::Usage {
                input_tokens,
                output_tokens,
                cost_usd_micros,
                ..
            } => usage = Some((input_tokens, output_tokens, cost_usd_micros)),
            AdapterEvent::Result { .. } => result_seen = true,
            AdapterEvent::Started { .. } => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(result_seen, "run succeeded with null usage");
    let (input_tokens, output_tokens, cost) =
        usage.expect("usage event emitted even when the envelope usage is null");
    assert_eq!(input_tokens, None, "null input_tokens never becomes 0");
    assert_eq!(output_tokens, None, "null output_tokens never becomes 0");
    assert_eq!(cost, None, "null total_cost_usd never becomes 0");
}

// ---- 8. argv/env isolation, evidence lock, resume ownership, probe ----------

#[tokio::test]
async fn argv_env_and_capability_claims_stay_evidence_bound() {
    let config_root = tempfile::tempdir().expect("config root");
    let adapter = recorded_adapter(
        vec![recording(SUCCESS), recording(SUCCESS)],
        config_root.path(),
    );
    let mut stream = adapter.subscribe();

    // Run 1 records the provider session id through Started.
    let run1 = run_start();
    let run1_id = run1.run_id.clone();
    adapter.start(run1).expect("start run 1");
    match stream
        .next_timeout(Duration::from_secs(10))
        .await
        .expect("started 1")
    {
        AdapterEvent::Started {
            provider_session_id,
            ..
        } => assert_eq!(provider_session_id.as_deref(), Some("claude-sess-0001")),
        other => panic!("expected Started, got {other:?}"),
    }
    wait_finished(&adapter, &run1_id).await;

    // Resume only a session this adapter recorded (03 §4 item 5).
    let run2 = run_start();
    adapter
        .start_with_resume(run2, Some("claude-sess-0001"))
        .expect("resume recorded session");
    let unknown = run_start();
    assert!(
        adapter
            .start_with_resume(unknown, Some("claude-sess-not-recorded"))
            .is_err(),
        "resume of an unrecorded session must be refused"
    );

    let observations = adapter.spawn_observations();
    assert_eq!(observations.len(), 2);
    let first = &observations[0];
    assert_eq!(
        first.argv,
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompts",
            "none",
            "--model",
            "claude-test-model",
        ],
        "baseline argv exactly as contracted"
    );
    // Prompt travels over stdin, never as an argv string.
    assert!(first.stdin_bytes > 0);
    assert!(!first.argv.iter().any(|a| a.contains("recorded thing")));
    // Per-run isolated config: CLAUDE_CONFIG_DIR + child-only HOME-family
    // override (env keys only are observed — values never persisted).
    assert!(first.env_keys.contains(&"CLAUDE_CONFIG_DIR".to_string()));
    assert!(
        first.env_keys.contains(&"USERPROFILE".to_string())
            || first.env_keys.contains(&"HOME".to_string()),
        "HOME-family fallback for the child only"
    );
    assert!(first
        .config_dir
        .to_string_lossy()
        .contains("claude-config-"));
    // Resume argv is evidenced in the captured help.
    assert!(observations[1].argv.contains(&"--resume".to_string()));
    assert!(observations[1]
        .argv
        .contains(&"claude-sess-0001".to_string()));

    // All-permission bypass flags never appear (03 §4 item 6).
    for observation in &observations {
        for arg in &observation.argv {
            for bad in [
                "dangerously-skip-permissions",
                "bypassPermissions",
                "fallback-model",
                "--bare",
            ] {
                assert!(
                    !arg.contains(bad),
                    "forbidden argv fragment '{bad}' in {arg}"
                );
            }
        }
    }

    // Evidence lock: every baseline argv item is documented in the
    // captured --help of the verified CLI build.
    for item in BASELINE_ARGV {
        assert!(
            FIXTURE_HELP.contains(item),
            "baseline argv item {item} missing from captured --help"
        );
    }
    for documented in ["--permission-prompts", "--resume", "--effort", "--model"] {
        assert!(FIXTURE_HELP.contains(documented));
    }
    assert!(FIXTURE_PRINT_HELP.contains("--print"));
    assert!(FIXTURE_VERSION.trim().contains(EVIDENCE_CLI_VERSION));

    // Effort flows from the binding; option-shaped values are rejected.
    let mut effort_run = run_start();
    effort_run.binding.effort = Some("high".into());
    let plan = claude_mod::build_launch_plan(&effort_run, None, config_root.path())
        .expect("plan with effort");
    let argv = plan.argv;
    let pos = argv
        .iter()
        .position(|a| a == "--effort")
        .expect("--effort present");
    assert_eq!(argv[pos + 1], "high");
    let mut hostile = run_start();
    hostile.binding.model_id = "--bare".into();
    assert!(claude_mod::build_launch_plan(&hostile, None, config_root.path()).is_err());
    assert!(claude_mod::build_launch_plan(&run_start(), Some("-x"), config_root.path()).is_err());

    // probe(): program exists + version parses + binding-only auth route;
    // capability claims are honest (all false, reasons recorded).
    let dir = tempfile::tempdir().expect("probe dir");
    let program = dir.path().join("claude.exe");
    std::fs::write(&program, b"stub").expect("stub program");
    let binding = claude_binding(&program.to_string_lossy(), AuthRoute::ApiKey);
    let report = adapter.probe(&binding);
    assert!(report.program_exists);
    assert_eq!(report.version_raw.as_deref(), Some("2.1.263 (Claude Code)"));
    assert_eq!(
        report.version,
        Some(parse_cli_version("2.1.263 (Claude Code)").expect("parses"))
    );
    assert_eq!(report.auth_route, AuthRoute::ApiKey);
    assert_eq!(report.provider_id, "anthropic");
    let caps = report.capabilities;
    // This API-key binding cannot inherit the managed-subscription evidence,
    // even on the exact Windows-tested CLI version.
    let proven_here = false;
    assert_eq!(caps.events.supported, proven_here);
    assert_eq!(caps.resume.supported, proven_here);
    assert_eq!(caps.usage.supported, proven_here);
    assert_eq!(caps.structured_result.supported, proven_here);
    assert_eq!(
        caps.steer.reason_code.as_deref(),
        Some(if proven_here {
            "print_mode_no_active_turn_steer"
        } else {
            NO_COMPATIBILITY_EVIDENCE
        })
    );
    assert_eq!(
        caps.approval_reply.reason_code.as_deref(),
        Some(if proven_here {
            "print_mode_no_approval_reply"
        } else {
            NO_COMPATIBILITY_EVIDENCE
        })
    );
    assert!(!caps.cancel.supported, "print interrupt not live-exercised");
    assert!(!caps.read_only.supported);
    assert!(!caps.scoped_write.supported);
    assert!(!caps.model_listing.supported);
    // An unmatched version resets to fully unclaimed.
    let mut other = binding.clone();
    other.program = dir
        .path()
        .join("claude-other.exe")
        .to_string_lossy()
        .into_owned();
    std::fs::write(&other.program, b"stub").expect("stub program");
    let adapter_reset = ClaudePrintAdapter::with_config(ClaudeAdapterConfig {
        config_root: dir.path().to_path_buf(),
        secrets_root: None,
        source_factory: Arc::new(|_run, _plan, _cancel| {
            Err(std::io::Error::other("not used by probe"))
        }),
        version_probe: Arc::new(|_p| Some("2.2.0 (Claude Code)".to_string())),
    });
    let reset = adapter_reset.probe(&other);
    assert_eq!(
        reset.capabilities.structured_result.reason_code.as_deref(),
        Some(NO_COMPATIBILITY_EVIDENCE),
        "unmatched version resets evidence"
    );
    // Anthropic vs Z.ai-style routes stay separated by the binding's
    // auth_route metadata alone — no credential values are touched.
    let zai_style = claude_binding(&program.to_string_lossy(), AuthRoute::Custom);
    assert_ne!(adapter.probe(&zai_style).auth_route, report.auth_route);
}
