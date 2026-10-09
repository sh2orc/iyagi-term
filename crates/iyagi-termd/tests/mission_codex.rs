//! O08 Codex adapter offline tests: recorded app-server JSONL transcripts
//! (built strictly from the codex-cli 0.153.4 generated schemas in
//! `src/agent_runtime/codex/fixtures/`) drive the same adapter engine a
//! live `codex app-server` child would (03 §3). No network, no model calls,
//! no auth.
//!
use iyagi_termd_lib::agent_runtime::codex;
/// The adapter under test, imported from the lib (wired since O08 landed).
use iyagi_termd_lib::agent_runtime::{
    AdapterEvent, AgentAdapter, CancelReceipt, CancelRejected, DeliveryReceipt, EventStream,
    QueuedReason, RunProbe, RunStart,
};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codex::{CodexAdapter, RecordedPeer};
use term_contracts::ids::U64String;
use term_contracts::launch::{Enforcement, LaunchPolicy};
use term_contracts::mission::types::{
    AuthRoute, Binding, Id, ProviderResult, RuntimeCapabilities, RuntimeKind, Support,
};
use term_contracts::mission::MissionErrorCode;

/// Thread/turn ids the recorded transcripts use.
const TID: &str = "0192f0de-7c1a-7b2e-8c3d-1a2b3c4d5e6f";
const UID: &str = "0192f0de-7c1a-7b2e-8c3d-1a2b3c4d5e70";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/agent_runtime/codex/fixtures/streams")
        .join(name)
}

/// A codex binding shaped like the offline evidence: read-only sandbox
/// (scoped_write unevidenced), approvals on, steer toggleable.
fn binding(steer: bool) -> Binding {
    let yes = || Support {
        supported: true,
        reason_code: None,
    };
    let no = |reason: &str| Support {
        supported: false,
        reason_code: Some(reason.into()),
    };
    Binding {
        id: Id::generate(),
        revision: U64String::parse("1").expect("revision"),
        label: "codex recorded transcript".into(),
        runtime: RuntimeKind::Codex,
        program: "codex".into(),
        runtime_version: Some("0.153.4".into()),
        provider_id: "openai".into(),
        model_id: "gpt-5.1-codex".into(),
        effort: None,
        auth_route: AuthRoute::Subscription,
        credential_ref: None,
        endpoint_ref: None,
        capabilities: RuntimeCapabilities {
            structured_result: yes(),
            events: yes(),
            cancel: yes(),
            resume: no("only threads this adapter recorded"),
            steer: if steer {
                yes()
            } else {
                no("turn/steer not evidenced live")
            },
            approval_reply: yes(),
            read_only: yes(),
            scoped_write: no("sandbox enforcement not evidenced live"),
            model_listing: yes(),
            usage: yes(),
            native_terminal_attach: no("activity viewer is the O1 default"),
        },
        checked_at: None,
        enabled: true,
        experimental_version: None,
        local_evidence: None,
        estimated_run_cost_usd_micros: None,
        resource_policy: LaunchPolicy {
            reservation_bytes: U64String::parse("268435456").expect("bytes"),
            cpu_slots: 1,
            enforcement: Enforcement::Observe,
            memory_max_bytes: None,
            cpu_max_cores: None,
            pids_max: None,
        },
    }
}

fn run_start(b: Binding) -> RunStart {
    RunStart {
        task_kind: None,
        mission_id: Id::generate(),
        owner_daemon_id: Id::generate(),
        workspace_access: iyagi_termd_lib::agent_runtime::WorkspaceAccess::ReadOnly,
        allow_network: false,
        run_id: Id::generate(),
        fencing_token: 7,
        binding: b,
        context_path: std::env::temp_dir(),
        workspace: None,
        prompt_stdin: "로그인 기능을 구현해라.".into(),
    }
}

/// Adapter wired to a recorded transcript; keeps the created peer for
/// send-side assertions.
struct Recorded {
    adapter: Arc<CodexAdapter>,
    peers: Arc<Mutex<Vec<Arc<RecordedPeer>>>>,
}

impl Recorded {
    fn new(fixture_name: &str) -> Self {
        Self::with_tail(fixture_name, Vec::new())
    }

    fn with_tail(
        fixture_name: &str,
        tail: Vec<iyagi_termd_lib::agent_runtime::codex::TranscriptLine>,
    ) -> Self {
        let path = fixture(fixture_name);
        let peers = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&peers);
        let adapter = CodexAdapter::with_peer_factory(Arc::new(move |_| {
            let mut lines = iyagi_termd_lib::agent_runtime::codex::load_transcript(&path)?;
            lines.extend(tail.clone());
            let peer = RecordedPeer::new(lines);
            sink.lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(Arc::clone(&peer));
            Ok(peer)
        }));
        Recorded { adapter, peers }
    }

    fn peer(&self) -> Arc<RecordedPeer> {
        self.peers.lock().unwrap_or_else(|p| p.into_inner())[0].clone()
    }
}

async fn next_event(stream: &mut EventStream) -> AdapterEvent {
    loop {
        let event = stream
            .next_timeout(Duration::from_secs(5))
            .await
            .expect("event within 5s");
        if !matches!(event, AdapterEvent::ModelObserved { .. }) {
            return event;
        }
    }
}

/// Concatenate consecutive Activity chunks at the stream head; returns the
/// joined text and the first non-Activity event after them. The bounded
/// fan-out may stage and merge same-run display deltas, so only the text
/// and its position relative to other events are asserted.
async fn joined_activity(stream: &mut EventStream) -> (String, AdapterEvent) {
    let mut text = String::new();
    loop {
        match next_event(stream).await {
            AdapterEvent::Activity { chunk, .. } => text.push_str(&chunk),
            other => return (text, other),
        }
    }
}

#[tokio::test]
async fn transport_failure_is_retryable_only_before_attempting_the_task_frame() {
    use codex::{PeerError, PeerEvent, ProtocolPeer};
    struct FailingSend {
        inner: Arc<RecordedPeer>,
        method: &'static str,
    }
    impl ProtocolPeer for FailingSend {
        fn send(&self, message: &serde_json::Value) -> Result<(), PeerError> {
            self.inner.send(message)?;
            if message["method"] == self.method {
                Err(PeerError("fixture partial write".into()))
            } else {
                Ok(())
            }
        }
        fn recv(&self) -> PeerEvent {
            self.inner.recv()
        }
        fn close(&self) {
            self.inner.close();
        }
    }
    for method in ["initialize", "turn/start"] {
        let peer = Arc::new(FailingSend {
            inner: RecordedPeer::from_file(&fixture("handshake_success.jsonl")).unwrap(),
            method,
        });
        let selected = peer.clone();
        let adapter = CodexAdapter::with_peer_factory(Arc::new(move |_| Ok(selected.clone())));
        let mut stream = adapter.subscribe();
        let start = run_start(binding(false));
        let id = start.run_id.clone();
        adapter.start(start).unwrap();
        let event = next_event(&mut stream).await;
        if method == "initialize" {
            assert!(matches!(
                event,
                AdapterEvent::FailedBeforeSubmission {
                    code: MissionErrorCode::ProviderUnavailable,
                    ..
                }
            ));
            assert!(!peer.inner.sent_methods().iter().any(|m| m == "turn/start"));
        } else {
            assert!(matches!(event, AdapterEvent::Disconnected { .. }));
            assert_eq!(
                peer.inner
                    .sent_methods()
                    .iter()
                    .filter(|m| *m == "turn/start")
                    .count(),
                1
            );
        }
        assert!(matches!(
            adapter.close(&id),
            CancelReceipt::Confirmed { .. }
        ));
    }
}

fn wait_not_running(adapter: &CodexAdapter, run_id: &Id) {
    for _ in 0..500 {
        if !matches!(adapter.inspect(run_id), RunProbe::Running) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("run never left the Running state");
}

fn sent_with_method(peer: &RecordedPeer, method: &str) -> serde_json::Value {
    peer.sent_messages()
        .into_iter()
        .find(|m| m.get("method").and_then(|v| v.as_str()) == Some(method))
        .unwrap_or_else(|| panic!("no sent message for {method}"))
}

#[tokio::test]
async fn happy_path_full_sequence_and_explicit_thread_params() {
    let recorded = Recorded::new("handshake_success.jsonl");
    let adapter = &recorded.adapter;
    let mut stream = adapter.subscribe();
    let start = run_start(binding(false));
    let run_id = start.run_id.clone();
    adapter.start(start).expect("start");

    // Started carries the provider's exact ids (03 §1).
    match next_event(&mut stream).await {
        AdapterEvent::Started {
            provider_session_id,
            provider_turn_id,
            ..
        } => {
            assert_eq!(provider_session_id.as_deref(), Some(TID));
            assert_eq!(provider_turn_id.as_deref(), Some(UID));
        }
        other => panic!("expected Started, got {other:?}"),
    }
    // Both recorded deltas, in order. Chunk boundaries are not part of the
    // contract: the critical Started saturates the fan-out window, so the
    // deltas right behind it stage and merge into one Activity.
    let (text, next) = joined_activity(&mut stream).await;
    assert_eq!(text, "로그인 모듈 확인 중...{\"kind\": \"re");
    match next {
        AdapterEvent::Usage {
            input_tokens,
            output_tokens,
            cost_usd_micros,
            ..
        } => {
            assert_eq!(input_tokens, Some(1200));
            assert_eq!(output_tokens, Some(300));
            assert_eq!(cost_usd_micros, None, "cost never fabricated");
        }
        other => panic!("expected Usage, got {other:?}"),
    }
    match next_event(&mut stream).await {
        AdapterEvent::Result { result, .. } => match result {
            ProviderResult::Report { report_text, .. } => {
                assert!(report_text.contains("로그인 모듈 점검 완료"))
            }
            other => panic!("expected Report result, got {other:?}"),
        },
        other => panic!("expected Result, got {other:?}"),
    }

    // The handshake matched the recorded protocol exactly.
    let peer = recorded.peer();
    assert_eq!(
        peer.sent_methods(),
        vec![
            "initialize",
            "initialized",
            "account/read",
            "model/list",
            "thread/start",
            "turn/start"
        ]
    );
    assert!(peer.mismatches().is_empty());
    // 03 §3: thread/start pins model/cwd/approvalPolicy/sandbox explicitly.
    // A read-only run has nothing left to approve — the sandbox is the boundary
    // — so the policy is `never`; a per-command prompt there only stops the run.
    let thread_params = sent_with_method(&peer, "thread/start")["params"].clone();
    assert_eq!(thread_params["model"], "gpt-5.1-codex");
    assert_eq!(thread_params["approvalPolicy"], "never");
    assert_eq!(thread_params["sandbox"], "read-only");
    assert!(thread_params.get("cwd").and_then(|c| c.as_str()).is_some());
    // 03 §2: turn/start carries the dedicated ProviderResult outputSchema.
    let turn_params = sent_with_method(&peer, "turn/start")["params"].clone();
    assert_eq!(turn_params["threadId"], TID);
    assert_eq!(turn_params["input"][0]["text"], "로그인 기능을 구현해라.");
    let kinds: Vec<&str> = turn_params["outputSchema"]["properties"]["result"]["anyOf"]
        .as_array()
        .expect("nested anyOf")
        .iter()
        .map(|v| {
            v["properties"]["kind"]["enum"][0]
                .as_str()
                .expect("kind enum")
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["plan", "report", "patch", "review", "question", "blocked"]
    );

    // Terminal hygiene: receipts distinguish states; unknown answers reject.
    assert_eq!(adapter.inspect(&run_id), RunProbe::Finished { exit: None });
    assert_eq!(
        adapter.interrupt(&run_id),
        CancelReceipt::Rejected {
            reason: CancelRejected::AlreadyTerminal
        }
    );
    assert_eq!(
        adapter.answer(&run_id, "41", "accept"),
        DeliveryReceipt::Rejected {
            reason: "run already terminal"
        }
    );
    assert_eq!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { exit: None }
    );
    assert_eq!(adapter.inspect(&run_id), RunProbe::Absent);
}

#[tokio::test]
async fn account_validation_failure_is_auth_required() {
    let recorded = Recorded::new("auth_required.jsonl");
    let adapter = &recorded.adapter;
    let mut stream = adapter.subscribe();
    let start = run_start(binding(false));
    let run_id = start.run_id.clone();
    adapter.start(start).expect("start");

    match next_event(&mut stream).await {
        AdapterEvent::Failed { code, message, .. } => {
            assert_eq!(code, MissionErrorCode::AuthRequired);
            assert!(message.contains("account"), "message: {message}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    // No Started event, and the protocol never went past account/read.
    assert!(stream.try_next().is_none());
    assert_eq!(
        recorded.peer().sent_methods(),
        vec!["initialize", "initialized", "account/read"]
    );
    assert_eq!(adapter.inspect(&run_id), RunProbe::Finished { exit: None });
    assert_eq!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { exit: None }
    );
}

#[tokio::test]
async fn model_list_gap_is_model_unavailable() {
    let recorded = Recorded::new("model_unavailable.jsonl");
    let adapter = &recorded.adapter;
    let mut stream = adapter.subscribe();
    adapter.start(run_start(binding(false))).expect("start");

    match next_event(&mut stream).await {
        AdapterEvent::Failed { code, message, .. } => {
            assert_eq!(code, MissionErrorCode::ModelUnavailable);
            assert!(message.contains("gpt-5.1-codex"), "message: {message}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    // thread/start was never sent — validation precedes session creation.
    assert_eq!(
        recorded.peer().sent_methods(),
        vec!["initialize", "initialized", "account/read", "model/list"]
    );
}

#[tokio::test]
async fn approval_uses_the_exact_provider_request_id_exactly_once() {
    let recorded = Recorded::new("approval_flow.jsonl");
    let adapter = &recorded.adapter;
    let mut stream = adapter.subscribe();
    let start = run_start(binding(false));
    let run_id = start.run_id.clone();
    adapter.start(start).expect("start");

    let (request_id, question) = loop {
        match next_event(&mut stream).await {
            AdapterEvent::Started { .. } => continue,
            AdapterEvent::Activity { .. } => continue,
            AdapterEvent::ApprovalRequested {
                provider_request_id,
                question,
                ..
            } => break (provider_request_id, question),
            other => panic!("expected ApprovalRequested, got {other:?}"),
        }
    };
    // EXACT wire id (server request id 41), not a synthetic one.
    assert_eq!(request_id, "41");
    assert!(question.contains("cargo test"), "question: {question}");

    // Unknown request id is refused without consuming the pending one.
    assert_eq!(
        adapter.answer(&run_id, "999", "accept"),
        DeliveryReceipt::Rejected {
            reason: "unknown or obsolete approval request"
        }
    );
    assert_eq!(
        adapter.answer(&run_id, "41", "accept"),
        DeliveryReceipt::Delivered {
            provider_ref: Some("41".into())
        }
    );
    // The pending approval is consumed exactly once — obsolete afterwards.
    assert_eq!(
        adapter.answer(&run_id, "41", "accept"),
        DeliveryReceipt::Rejected {
            reason: "unknown or obsolete approval request"
        }
    );
    // The response echoes the provider's wire id with the mapped decision.
    let answered = recorded
        .peer()
        .sent_messages()
        .into_iter()
        .find(|m| m.get("id").and_then(|v| v.as_u64()) == Some(41))
        .expect("approval response sent");
    assert_eq!(answered["result"]["decision"], "accept");

    // The turn finishes after the answer (approval → answer → finish).
    match next_event(&mut stream).await {
        AdapterEvent::Activity { .. } => {}
        other => panic!("expected post-approval Activity, got {other:?}"),
    }
    match next_event(&mut stream).await {
        AdapterEvent::Usage { .. } => {}
        other => panic!("expected Usage, got {other:?}"),
    }
    match next_event(&mut stream).await {
        AdapterEvent::Result { .. } => {}
        other => panic!("expected Result, got {other:?}"),
    }
    assert!(matches!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { .. }
    ));
}

/// Regression: a file-change approval's question embeds the retained diff
/// evidence and threads run `approvalPolicy: untrusted`, so a 4 KiB question
/// cap answered nearly every real patch approval with `-32601` and never
/// raised a Decision. The recorded command approval is swapped for
/// schema-shaped synthetic test input (item/started evidence + request),
/// not a claim of another captured provider run.
#[tokio::test]
async fn file_change_approval_with_a_multi_kilobyte_diff_surfaces_as_a_decision() {
    use codex::TranscriptLine;
    let diff: String = (0..400)
        .map(|i| format!("+    let generated_line_{i} = {i};\n"))
        .collect();
    assert!(diff.len() > 10 * 1024);
    let started = serde_json::json!({
        "method": "item/started",
        "params": {
            "threadId": TID,
            "turnId": UID,
            "item": {
                "id": "item-patch-1",
                "type": "fileChange",
                "status": "inProgress",
                "changes": [{"path": "src/login.rs", "kind": {"type": "update"}, "diff": diff}]
            }
        }
    });
    let request = serde_json::json!({
        "id": 41,
        "method": "item/fileChange/requestApproval",
        "params": {
            "threadId": TID,
            "turnId": UID,
            "itemId": "item-patch-1",
            "reason": "apply the login patch",
            "grantRoot": null
        }
    });
    let mut lines = codex::load_transcript(&fixture("approval_flow.jsonl")).expect("fixture");
    let at = lines
        .iter()
        .position(|line| match line {
            TranscriptLine::S { msg } => msg["method"] == "item/commandExecution/requestApproval",
            _ => false,
        })
        .expect("recorded approval request");
    lines[at] = TranscriptLine::S { msg: request };
    lines.insert(at, TranscriptLine::S { msg: started });
    let peer = RecordedPeer::new(lines);
    let selected = Arc::clone(&peer);
    let adapter = CodexAdapter::with_peer_factory(Arc::new(move |_| Ok(selected.clone())));
    let mut stream = adapter.subscribe();
    let start = run_start(binding(false));
    let run_id = start.run_id.clone();
    adapter.start(start).expect("start");

    let (request_id, question) = loop {
        match next_event(&mut stream).await {
            AdapterEvent::Started { .. } => continue,
            AdapterEvent::Activity { .. } => continue,
            AdapterEvent::ApprovalRequested {
                provider_request_id,
                question,
                ..
            } => break (provider_request_id, question),
            other => panic!("expected ApprovalRequested, got {other:?}"),
        }
    };
    assert_eq!(request_id, "41");
    assert!(question.len() > 4 * 1024, "{} bytes", question.len());
    let details: serde_json::Value = serde_json::from_str(&question).expect("question JSON");
    assert_eq!(details["details_available"], true);
    assert_eq!(details["changes"][0]["diff"], diff.as_str());
    // Nothing was auto-rejected back to the provider.
    let sent = peer.sent_messages();
    assert!(sent.iter().all(|m| m.get("error").is_none()));
    assert_eq!(
        adapter.answer(&run_id, "41", "accept"),
        DeliveryReceipt::Delivered {
            provider_ref: Some("41".into())
        }
    );
    let answered = peer
        .sent_messages()
        .into_iter()
        .find(|m| m.get("id").and_then(|v| v.as_u64()) == Some(41))
        .expect("approval response sent");
    assert_eq!(answered["result"]["decision"], "accept");
    loop {
        match next_event(&mut stream).await {
            AdapterEvent::Activity { .. } | AdapterEvent::Usage { .. } => continue,
            AdapterEvent::Result { .. } => break,
            other => panic!("expected the approved turn to finish, got {other:?}"),
        }
    }
    assert!(matches!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { .. }
    ));
}

#[tokio::test]
async fn interrupt_is_accepted_then_terminal_without_a_final() {
    let recorded = Recorded::new("interrupted.jsonl");
    let adapter = &recorded.adapter;
    let mut stream = adapter.subscribe();
    let start = run_start(binding(false));
    let run_id = start.run_id.clone();
    adapter.start(start).expect("start");

    assert!(matches!(
        next_event(&mut stream).await,
        AdapterEvent::Started { .. }
    ));
    match next_event(&mut stream).await {
        AdapterEvent::Activity { .. } => {}
        other => panic!("expected pre-interrupt Activity, got {other:?}"),
    }
    // Accepted: the protocol interrupt is out; termination is NOT yet
    // confirmed (03 §1 — no process was reaped).
    assert_eq!(adapter.interrupt(&run_id), CancelReceipt::Accepted);
    let interrupt = sent_with_method(&recorded.peer(), "turn/interrupt");
    assert_eq!(interrupt["params"]["threadId"], TID);
    assert_eq!(interrupt["params"]["turnId"], UID);
    // turn/completed(interrupted) we requested emits no synthetic final.
    wait_not_running(adapter, &run_id);
    assert_eq!(adapter.inspect(&run_id), RunProbe::Finished { exit: None });
    assert_eq!(
        adapter.interrupt(&run_id),
        CancelReceipt::Rejected {
            reason: CancelRejected::AlreadyTerminal
        }
    );
    assert_eq!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { exit: None }
    );
}

#[tokio::test]
async fn early_interrupt_waits_for_the_matching_started_turn_and_sends_once() {
    use codex::{PeerError, PeerEvent, ProtocolPeer, TranscriptLine};
    use std::sync::Condvar;
    struct DelayedStart {
        peer: Arc<RecordedPeer>,
        release: Mutex<bool>,
        wake: Condvar,
    }
    impl ProtocolPeer for DelayedStart {
        fn send(&self, message: &serde_json::Value) -> Result<(), PeerError> {
            self.peer.send(message)
        }
        fn recv(&self) -> PeerEvent {
            let event = self.peer.recv();
            if matches!(&event, PeerEvent::Message(value) if value["method"] == "turn/started" && value["params"]["threadId"] == TID)
            {
                let ready = self.release.lock().unwrap();
                let (ready, timeout) = self
                    .wake
                    .wait_timeout_while(ready, Duration::from_secs(5), |ready| !*ready)
                    .unwrap();
                if timeout.timed_out() && !*ready {
                    return PeerEvent::ConnectionLost;
                }
            }
            event
        }
        fn close(&self) {
            *self.release.lock().unwrap() = true;
            self.wake.notify_all();
            self.peer.close();
        }
    }
    let mut lines = codex::load_transcript(&fixture("interrupted.jsonl")).unwrap();
    lines.retain(|line| !matches!(line, TranscriptLine::S { msg } if msg["method"] == "item/agentMessage/delta"));
    let index = lines
        .iter()
        .position(
            |line| matches!(line, TranscriptLine::S { msg } if msg["method"] == "turn/started"),
        )
        .unwrap();
    lines.insert(index, TranscriptLine::S { msg: serde_json::json!({"method":"turn/started","params":{"threadId":"other-thread","turn":{"id":"other-turn"}}}) });
    let peer = Arc::new(DelayedStart {
        peer: RecordedPeer::new(lines),
        release: Mutex::new(false),
        wake: Condvar::new(),
    });
    let factory = peer.clone();
    let adapter = CodexAdapter::with_peer_factory(Arc::new(move |_| Ok(factory.clone())));
    let mut events = adapter.subscribe();
    let start = run_start(binding(false));
    let id = start.run_id.clone();
    adapter.start(start).unwrap();
    assert!(matches!(
        next_event(&mut events).await,
        AdapterEvent::Started { .. }
    ));
    assert_eq!(adapter.interrupt(&id), CancelReceipt::Accepted);
    assert_eq!(adapter.interrupt(&id), CancelReceipt::Accepted);
    assert!(!peer
        .peer
        .sent_methods()
        .iter()
        .any(|method| method == "turn/interrupt"));
    *peer.release.lock().unwrap() = true;
    peer.wake.notify_all();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !matches!(adapter.inspect(&id), RunProbe::Finished { .. }) {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        peer.peer
            .sent_methods()
            .iter()
            .filter(|method| method.as_str() == "turn/interrupt")
            .count(),
        1
    );
    assert!(
        events.try_next().is_none(),
        "interruption cannot synthesize success"
    );
    adapter.close(&id);
}

#[tokio::test]
async fn midstream_disconnect_emits_disconnected() {
    let recorded = Recorded::new("disconnect_midstream.jsonl");
    let adapter = &recorded.adapter;
    let mut stream = adapter.subscribe();
    let start = run_start(binding(false));
    let run_id = start.run_id.clone();
    adapter.start(start).expect("start");

    assert!(matches!(
        next_event(&mut stream).await,
        AdapterEvent::Started { .. }
    ));
    assert!(matches!(
        next_event(&mut stream).await,
        AdapterEvent::Activity { .. }
    ));
    match next_event(&mut stream).await {
        AdapterEvent::Disconnected { .. } => {}
        other => panic!("expected Disconnected, got {other:?}"),
    }
    assert_eq!(adapter.inspect(&run_id), RunProbe::Finished { exit: None });
    assert!(matches!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { .. }
    ));
}

#[tokio::test]
async fn exit_zero_without_a_final_is_result_invalid() {
    let recorded = Recorded::new("exit_without_final.jsonl");
    let adapter = &recorded.adapter;
    let mut stream = adapter.subscribe();
    let start = run_start(binding(false));
    let run_id = start.run_id.clone();
    adapter.start(start).expect("start");

    assert!(matches!(
        next_event(&mut stream).await,
        AdapterEvent::Started { .. }
    ));
    assert!(matches!(
        next_event(&mut stream).await,
        AdapterEvent::Activity { .. }
    ));
    match next_event(&mut stream).await {
        AdapterEvent::Failed { code, message, .. } => {
            assert_eq!(code, MissionErrorCode::ResultInvalid);
            assert!(
                message.contains("without turn/completed"),
                "message: {message}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(matches!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { .. }
    ));
}

#[tokio::test]
async fn malformed_final_results_are_result_invalid() {
    for fixture_name in ["result_not_json.jsonl", "result_fabricated_id.jsonl"] {
        let recorded = Recorded::new(fixture_name);
        let adapter = &recorded.adapter;
        let mut stream = adapter.subscribe();
        let start = run_start(binding(false));
        let run_id = start.run_id.clone();
        adapter.start(start).expect("start");

        let mut failed = None;
        while let Some(event) = stream.next_timeout(Duration::from_secs(5)).await {
            if let AdapterEvent::InvalidResult {
                code,
                message,
                rejected_result,
                ..
            } = event
            {
                assert!(rejected_result.is_some(), "the rejected answer is retained");
                failed = Some((code, message));
                break;
            }
        }
        let (code, message) = failed.unwrap_or_else(|| panic!("{fixture_name}: no Failed event"));
        assert_eq!(code, MissionErrorCode::ResultInvalid, "{fixture_name}");
        assert!(
            message.contains("ProviderResult"),
            "{fixture_name}: {message}"
        );
        assert!(matches!(
            adapter.close(&run_id),
            CancelReceipt::Confirmed { .. }
        ));
    }
}

#[tokio::test]
async fn stale_fencing_token_events_are_counted_and_dropped() {
    let recorded = Recorded::new("handshake_success.jsonl");
    let adapter = &recorded.adapter;
    let mut stream = adapter.subscribe();
    let start = run_start(binding(false));
    let run_id = start.run_id.clone();
    adapter.start(start).expect("start");
    // A new actor took the run over after launch: token 7 is now stale
    // (E11). register() in start() pins the launch token; advance() must
    // come after it, and the stream drops queued stale events at read time.
    stream.gate().advance(&run_id, 8);

    std::thread::sleep(Duration::from_millis(300));
    assert!(
        stream.try_next().is_none(),
        "every event from the stale actor is dropped"
    );
    assert!(
        stream.gate().dropped_stale() >= 5,
        "dropped_stale = {}",
        stream.gate().dropped_stale()
    );
    assert!(matches!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { .. }
    ));
}

#[tokio::test]
async fn model_mismatch_at_start_or_reroute_records_and_rejects_a_silent_fallback() {
    for reroute_after_start in [false, true] {
        let peers = Arc::new(Mutex::new(Vec::new()));
        let sink = peers.clone();
        let adapter = CodexAdapter::with_peer_factory(Arc::new(move |_| {
            let mut lines = codex::load_transcript(&fixture("model_mismatch.jsonl"))?;
            // The retained transcript already mismatches at thread/start.
            // This synthetic variant first confirms the bound model, letting
            // the later captured reroute exercise the active-turn boundary.
            if reroute_after_start {
                for line in &mut lines {
                    if let codex::TranscriptLine::S { msg } = line {
                        if msg["id"] == 4 {
                            msg["result"]["model"] = serde_json::json!("gpt-5.1-codex");
                            msg["result"]["thread"]["model"] = serde_json::json!("gpt-5.1-codex");
                        }
                    }
                }
            }
            let peer = RecordedPeer::new(lines);
            sink.lock().unwrap().push(peer.clone());
            Ok(peer)
        }));
        let mut stream = adapter.subscribe();
        let start = run_start(binding(false));
        let run_id = start.run_id.clone();
        adapter.start(start).expect("start");
        let mut saw_failure = false;
        let mut observed = vec![];
        while let Some(event) = stream.next_timeout(Duration::from_secs(5)).await {
            match event {
                AdapterEvent::ModelObserved { model, .. } => observed.push(model),
                AdapterEvent::Failed {
                    code: MissionErrorCode::ModelUnavailable,
                    ..
                } => {
                    saw_failure = true;
                    break;
                }
                AdapterEvent::Result { .. } => panic!("mismatched result must not be accepted"),
                _ => {}
            }
        }
        assert!(saw_failure);
        assert_eq!(
            observed,
            if reroute_after_start {
                vec!["gpt-5.1-codex", "gpt-5.1-codex-mini"]
            } else {
                vec!["gpt-5.1-codex-mini"]
            }
        );
        assert_eq!(
            adapter.model_observation(&run_id),
            Some(("gpt-5.1-codex".into(), Some("gpt-5.1-codex-mini".into())))
        );
        assert_eq!(
            peers.lock().unwrap()[0]
                .sent_methods()
                .iter()
                .any(|method| method == "turn/start"),
            reroute_after_start
        );
        assert!(matches!(
            adapter.close(&run_id),
            CancelReceipt::Confirmed { .. }
        ));
    }
}

#[tokio::test]
async fn steer_receipts_and_queued_paths() {
    // (1) Unknown run rejects.
    let idle = Recorded::new("steer_midturn.jsonl");
    assert_eq!(
        idle.adapter.send_message(&Id::generate(), "추가 요청"),
        DeliveryReceipt::Rejected {
            reason: "unknown run"
        }
    );

    // (2) Steer into an open turn: Delivered with the exact turn pin.
    // The captured prefix has no ack. Append a schema-derived response as
    // synthetic test input, not as a claim of another captured provider run.
    let recorded = Recorded::with_tail(
        "steer_midturn.jsonl",
        vec![iyagi_termd_lib::agent_runtime::codex::TranscriptLine::S {
            msg: serde_json::json!({"id":6,"result":{"turnId":UID}}),
        }],
    );
    let adapter = &recorded.adapter;
    let mut stream = adapter.subscribe();
    let start = run_start(binding(true));
    let run_id = start.run_id.clone();
    adapter.start(start).expect("start");
    assert!(matches!(
        next_event(&mut stream).await,
        AdapterEvent::Started { .. }
    ));
    assert!(matches!(
        next_event(&mut stream).await,
        AdapterEvent::Activity { .. }
    ));
    assert_eq!(
        adapter.send_message(&run_id, "테스트도 함께 추가해라."),
        DeliveryReceipt::Delivered {
            provider_ref: Some(UID.into())
        }
    );
    let steer = sent_with_method(&recorded.peer(), "turn/steer");
    assert_eq!(steer["params"]["threadId"], TID);
    assert_eq!(steer["params"]["expectedTurnId"], UID);
    assert_eq!(
        steer["params"]["input"][0]["text"],
        "테스트도 함께 추가해라."
    );
    assert!(matches!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { .. }
    ));

    // (3) Binding without steer evidence: queued with the reason.
    let no_steer = Recorded::new("steer_midturn.jsonl");
    let adapter = &no_steer.adapter;
    let mut stream = adapter.subscribe();
    let start = run_start(binding(false));
    let run_id = start.run_id.clone();
    adapter.start(start).expect("start");
    assert!(matches!(
        next_event(&mut stream).await,
        AdapterEvent::Started { .. }
    ));
    assert!(matches!(
        next_event(&mut stream).await,
        AdapterEvent::Activity { .. }
    ));
    assert_eq!(
        adapter.send_message(&run_id, "body"),
        DeliveryReceipt::Queued {
            reason: QueuedReason::SteerUnsupported
        }
    );
    assert!(matches!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { .. }
    ));

    // (4) After a terminal: the body queues for the next run.
    let done = Recorded::new("handshake_success.jsonl");
    let adapter = &done.adapter;
    let mut stream = adapter.subscribe();
    let start = run_start(binding(true));
    let run_id = start.run_id.clone();
    adapter.start(start).expect("start");
    let mut terminal = false;
    while let Some(event) = stream.next_timeout(Duration::from_secs(5)).await {
        if matches!(
            event,
            AdapterEvent::Result { .. }
                | AdapterEvent::Failed { .. }
                | AdapterEvent::Disconnected { .. }
        ) {
            terminal = true;
            break;
        }
    }
    assert!(terminal);
    assert_eq!(
        adapter.send_message(&run_id, "body"),
        DeliveryReceipt::Queued {
            reason: QueuedReason::NextRun
        }
    );
    assert!(matches!(
        adapter.close(&run_id),
        CancelReceipt::Confirmed { .. }
    ));
}

#[tokio::test]
async fn steer_requires_the_expected_ack_and_never_retries_uncertain_delivery() {
    use iyagi_termd_lib::agent_runtime::codex::TranscriptLine;
    for (response, rejected) in [
        (
            serde_json::json!({"id":6,"error":{"code":-32000,"message":"fixture rejection"}}),
            true,
        ),
        (
            serde_json::json!({"id":6,"result":{"turnId":"another-turn"}}),
            false,
        ),
        (serde_json::json!({"id":999,"result":{"turnId":UID}}), false),
        (serde_json::json!({"id":6,"result":{}}), false),
    ] {
        let recorded = Recorded::with_tail(
            "steer_midturn.jsonl",
            vec![TranscriptLine::S { msg: response }],
        );
        let mut events = recorded.adapter.subscribe();
        let run = run_start(binding(true));
        let id = run.run_id.clone();
        recorded.adapter.start(run).unwrap();
        assert!(matches!(
            next_event(&mut events).await,
            AdapterEvent::Started { .. }
        ));
        assert!(matches!(
            next_event(&mut events).await,
            AdapterEvent::Activity { .. }
        ));
        let receipt = recorded.adapter.send_message(&id, "only once");
        if rejected {
            assert!(matches!(receipt, DeliveryReceipt::Rejected { .. }));
        } else {
            assert!(matches!(receipt, DeliveryReceipt::Unknown { .. }));
        }
        assert_eq!(
            recorded
                .peer()
                .sent_methods()
                .iter()
                .filter(|m| m.as_str() == "turn/steer")
                .count(),
            1
        );
        recorded.adapter.close(&id);
    }
}

#[tokio::test]
async fn requested_task_access_controls_sandbox_without_changing_capability_evidence() {
    use iyagi_termd_lib::agent_runtime::WorkspaceAccess;
    for (access, network) in [
        (WorkspaceAccess::ReadOnly, false),
        (WorkspaceAccess::Write, false),
        (WorkspaceAccess::Write, true),
    ] {
        let recorded = Recorded::new("handshake_success.jsonl");
        let mut stream = recorded.adapter.subscribe();
        let mut start = run_start(binding(false));
        assert!(!start.binding.capabilities.scoped_write.supported);
        start.workspace_access = access;
        start.allow_network = network;
        let directory = tempfile::tempdir().unwrap();
        start.workspace = Some(directory.path().into());
        let id = start.run_id.clone();
        recorded.adapter.start(start).unwrap();
        loop {
            match next_event(&mut stream).await {
                AdapterEvent::Result { .. } => break,
                AdapterEvent::Failed { code, message, .. } => {
                    panic!("unexpected failure {code:?}: {message}")
                }
                _ => {}
            }
        }
        let peer = recorded.peer();
        let thread = sent_with_method(&peer, "thread/start");
        let turn = sent_with_method(&peer, "turn/start");
        let policy = &turn["params"]["sandboxPolicy"];
        assert_eq!(policy["networkAccess"], network);
        if access == WorkspaceAccess::Write {
            assert_eq!(thread["params"]["sandbox"], "workspace-write");
            assert_eq!(policy["type"], "workspaceWrite");
            assert_eq!(
                policy["writableRoots"],
                serde_json::json!([directory.path()])
            );
            assert_eq!(policy["excludeSlashTmp"], true);
            assert_eq!(policy["excludeTmpdirEnvVar"], true);
        } else {
            assert_eq!(thread["params"]["sandbox"], "read-only");
            assert_eq!(policy["type"], "readOnly");
        }
        recorded.adapter.close(&id);
    }
}

#[test]
fn writer_without_an_absolute_existing_workspace_never_spawns_a_peer() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("file");
    std::fs::write(&file, b"owned fixture").unwrap();
    for workspace in [
        None,
        Some(PathBuf::from("relative")),
        Some(directory.path().join("missing")),
        Some(file),
    ] {
        let recorded = Recorded::new("handshake_success.jsonl");
        let mut start = run_start(binding(false));
        start.workspace_access = iyagi_termd_lib::agent_runtime::WorkspaceAccess::Write;
        start.workspace = workspace;
        assert_eq!(
            recorded.adapter.start(start).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(recorded.peers.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn unconfirmed_approval_write_is_unknown_and_is_never_sent_twice() {
    use codex::{PeerError, PeerEvent, ProtocolPeer};
    struct FailedApproval {
        inner: Arc<RecordedPeer>,
        attempts: std::sync::atomic::AtomicUsize,
    }
    impl ProtocolPeer for FailedApproval {
        fn send(&self, message: &serde_json::Value) -> Result<(), PeerError> {
            if message["id"] == 41 && message.get("result").is_some() {
                self.attempts
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                return Err(PeerError("partial write fixture".into()));
            }
            self.inner.send(message)
        }
        fn recv(&self) -> PeerEvent {
            self.inner.recv()
        }
        fn close(&self) {
            self.inner.close();
        }
    }
    let peer = Arc::new(FailedApproval {
        inner: RecordedPeer::from_file(&fixture("approval_flow.jsonl")).unwrap(),
        attempts: Default::default(),
    });
    let factory_peer = peer.clone();
    let adapter = CodexAdapter::with_peer_factory(Arc::new(move |_| Ok(factory_peer.clone())));
    let run = run_start(binding(false));
    let id = run.run_id.clone();
    let mut events = adapter.subscribe();
    adapter.start(run).unwrap();
    loop {
        if matches!(
            next_event(&mut events).await,
            AdapterEvent::ApprovalRequested { .. }
        ) {
            break;
        }
    }
    assert!(matches!(
        adapter.answer(&id, "41", "accept"),
        DeliveryReceipt::Unknown { .. }
    ));
    assert!(matches!(
        adapter.answer(&id, "41", "accept"),
        DeliveryReceipt::Rejected { .. }
    ));
    assert_eq!(peer.attempts.load(std::sync::atomic::Ordering::Acquire), 1);
    assert!(matches!(
        adapter.close(&id),
        CancelReceipt::Confirmed { .. }
    ));
}
