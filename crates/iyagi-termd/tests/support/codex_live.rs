//! Explicit real-provider compatibility check. Never runs in the offline suite.
use super::*;
use iyagi_termd_lib::agent_runtime::{
    codex::{auth::AuthScope, CodexAdapter, PeerError, PeerEvent, ProtocolPeer, SupervisedPeer},
    CancelReceipt, RunProbe, WorkspaceAccess,
};
use serde_json::json;
use term_contracts::mission::types::ProviderResult;

struct DiagnosticPeer {
    inner: Arc<SupervisedPeer>,
    diagnostics: Arc<Mutex<Vec<serde_json::Value>>>,
    workspace: PathBuf,
    file_items: Mutex<std::collections::HashSet<String>>,
    allowed_approvals: Arc<Mutex<std::collections::HashSet<String>>>,
    writable: bool,
    interrupt_request: Mutex<Option<serde_json::Value>>,
    start_turn: Mutex<Option<String>>,
}
impl ProtocolPeer for DiagnosticPeer {
    fn auth_scope(&self) -> Option<AuthScope> {
        self.inner.auth_scope()
    }
    fn take_api_key(&self) -> Option<zeroize::Zeroizing<String>> {
        self.inner.take_api_key()
    }
    fn send(&self, value: &serde_json::Value) -> Result<(), PeerError> {
        if value["method"] == "turn/interrupt" {
            *self.interrupt_request.lock().unwrap() = Some(value["id"].clone());
        }
        self.inner.send(value)
    }
    fn recv(&self) -> PeerEvent {
        let event = self.inner.recv();
        if let PeerEvent::Message(value) = &event {
            if let Some(id) = value["result"]["turn"]["id"].as_str() {
                *self.start_turn.lock().unwrap() = Some(id.into());
            }
            if self.interrupt_request.lock().unwrap().as_ref() == value.get("id")
                && value.get("id").is_some()
            {
                self.diagnostics.lock().unwrap().push(json!({"interrupt_reply":true,"accepted":value.get("error").is_none(),"error_code":value["error"]["code"]}));
            }
            if value["method"] == "turn/started" {
                self.diagnostics.lock().unwrap().push(json!({"turn_started":true,"matches_start_reply":self.start_turn.lock().unwrap().as_ref().map(|id| Some(id.as_str()) == value["params"]["turn"]["id"].as_str())}));
            }
            if value["method"] == "turn/completed" {
                self.diagnostics
                    .lock()
                    .unwrap()
                    .push(json!({"turn_completed":value["params"]["turn"]["status"]}));
            }
            let item = &value["params"]["item"];
            if self.writable && value["method"] == "item/started" && item["type"] == "fileChange" {
                let safe = item["changes"].as_array().is_some_and(|changes| {
                    changes.len() == 1
                        && changes.iter().all(|change| {
                            let Some(path) = change["path"].as_str() else {
                                return false;
                            };
                            let path = self.workspace.join(path);
                            change["kind"]["type"] == "add"
                                && path.file_name().is_some_and(|name| name == "allowed.txt")
                                && path.parent().and_then(|parent| parent.canonicalize().ok())
                                    == self.workspace.canonicalize().ok()
                        })
                });
                if safe {
                    self.file_items
                        .lock()
                        .unwrap()
                        .insert(item["id"].as_str().unwrap().into());
                }
            }
            if value["method"] == "item/fileChange/requestApproval"
                && value["params"]["grantRoot"].is_null()
                && value["params"]["itemId"]
                    .as_str()
                    .is_some_and(|id| self.file_items.lock().unwrap().contains(id))
            {
                let id = value["id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value["id"].to_string());
                self.allowed_approvals.lock().unwrap().insert(id);
            }
            if value["method"] == "error" {
                let message = value["params"]["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase();
                let categories: Vec<_> = [
                    "schema",
                    "oneof",
                    "additionalproperties",
                    "401",
                    "403",
                    "429",
                    "timeout",
                    "timed out",
                    "connect",
                    "unsupported",
                    "invalid",
                    "websocket",
                    "stream disconnected",
                ]
                .into_iter()
                .filter(|term| message.contains(term))
                .collect();
                let mut log = self.diagnostics.lock().unwrap();
                if log.len() < 32 {
                    log.push(json!({"will_retry":value["params"]["willRetry"],"categories":categories,"info":value["params"]["error"]["codexErrorInfo"]}));
                }
            }
        }
        event
    }
    fn close(&self) {
        self.inner.close();
    }
    fn cleanup_confirmed(&self) -> bool {
        self.inner.cleanup_confirmed()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real subscription inference; explicit IYAGI_CODEX_BIN/VERSION/MODEL and evidence output required"]
async fn installed_subscription_runs_and_cancels_through_production_adapter() {
    let program = std::env::var("IYAGI_CODEX_BIN").expect("explicit installed CLI path");
    let version = std::env::var("IYAGI_CODEX_VERSION").expect("explicit installed CLI version");
    let model = std::env::var("IYAGI_CODEX_MODEL").expect("explicit model; no fallback");
    let evidence = std::env::var("IYAGI_CODEX_EVIDENCE_OUT").expect("explicit evidence file");
    assert!(Path::new(&program).is_absolute());
    assert_eq!(
        iyagi_termd_lib::agent_runtime::installation::version(
            &program,
            term_contracts::mission::types::RuntimeKind::Codex
        )
        .unwrap(),
        version
    );
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let outside = directory.path().join("outside.txt");
    std::fs::write(&outside, b"original").unwrap();
    let context = workspace.join("context.json");
    std::fs::write(&context, b"{}").unwrap();
    let store = Arc::new(FaultStore::default());
    let supervisor = Arc::new(gated_supervisor(
        store.clone(),
        &directory.path().join("exec"),
    ));
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let allowed_approvals = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let factory_approvals = allowed_approvals.clone();
    let factory_diagnostics = diagnostics.clone();
    let factory_supervisor = supervisor.clone();
    let runtime = tokio::runtime::Handle::current();
    let config_root = directory.path().join("configs");
    let adapter = CodexAdapter::with_peer_factory(Arc::new(move |run| {
        let peer = SupervisedPeer::spawn_authenticated(
            run,
            &factory_supervisor,
            &runtime,
            None,
            &config_root,
        )
        .map_err(|_| "authenticated launch failed".to_string())?;
        Ok(Arc::new(DiagnosticPeer {
            inner: peer,
            diagnostics: factory_diagnostics.clone(),
            workspace: run.workspace.clone().unwrap(),
            writable: run.workspace_access == WorkspaceAccess::Write,
            file_items: Mutex::new(std::collections::HashSet::new()),
            allowed_approvals: factory_approvals.clone(),
            interrupt_request: Mutex::new(None),
            start_turn: Mutex::new(None),
        }) as Arc<dyn ProtocolPeer>)
    }));
    let mut cases = Vec::new();
    let mut failure = None;
    let selected_case = std::env::var("IYAGI_CODEX_LIVE_CASE").ok();
    for (name, access, prompt, cancel) in [
        ("read_only_result", WorkspaceAccess::ReadOnly, "This is a compatibility check. Do not call any tools or inspect any files. Return a report with report_text exactly compatibility-ok and knowledge an empty array.", false),
        ("workspace_write_result", WorkspaceAccess::Write, "This is a compatibility check in a disposable directory. Use apply_patch to create allowed.txt containing exactly compatibility-write followed by a newline. Do not modify any other file, invoke shell commands, or request elevated permissions. Return a report with report_text exactly compatibility-ok and knowledge an empty array.", false),
        ("cancel", WorkspaceAccess::ReadOnly, "This is an interrupt compatibility check. Without calling tools or inspecting files, compose a report containing the numbers from 1 to 5000, one per line.", true),
    ] {
        if selected_case.as_ref().is_some_and(|selected| selected != name) { continue; }
        let mut start = codex_start(prompt);
        start.binding.program = program.clone();
        start.binding.runtime_version = Some(version.clone());
        start.binding.model_id = model.clone();
        start.binding.effort = Some("low".into());
        start.workspace_access = access;
        start.workspace = Some(workspace.clone());
        start.context_path = context.clone();
        let id = start.run_id.clone();
        let mut events = adapter.subscribe();
        let mut started = false;
        let mut result_ok = false;
        let mut interrupted = false;
        let mut approvals = 0;
        let mut approved = 0;
        let mut event_count = 0;
        let start_result = adapter.start(start);
        if start_result.is_ok() {
            let deadline = Instant::now() + Duration::from_secs(90);
            while Instant::now() < deadline {
                if cancel && interrupted && matches!(adapter.inspect(&id), RunProbe::Finished { .. }) {
                    result_ok = true;
                    break;
                }
                let Some(event) = events.next_timeout(Duration::from_millis(100)).await else { continue };
                if event.run_id() != &id { continue; }
                event_count += 1;
                match event {
                    AdapterEvent::Started { .. } => {
                        started = true;
                        if cancel {
                            let driver = adapter.clone();
                            let run = id.clone();
                            interrupted = tokio::task::spawn_blocking(move || driver.interrupt(&run)).await.unwrap() == CancelReceipt::Accepted;
                        }
                    }
                    AdapterEvent::ApprovalRequested { provider_request_id, .. } => {
                        approvals += 1;
                        // Authorize only the exact owned-file addition verified from its
                        // correlated protocol item. Root grants and commands are denied.
                        let safe = allowed_approvals.lock().unwrap().remove(&provider_request_id);
                        let receipt = adapter.answer(&id, &provider_request_id, if safe {"accept"} else {"deny"});
                        if safe && matches!(receipt, iyagi_termd_lib::agent_runtime::DeliveryReceipt::Delivered { .. }) { approved += 1; }
                    }
                    AdapterEvent::Result { result: ProviderResult::Report { report_text, .. }, .. } => {
                        result_ok = !cancel && report_text == "compatibility-ok";
                        break;
                    }
                    event if event.is_terminal() => {
                        // The adapter emits safe normalized diagnostics; no raw account/CLI data.
                        failure = Some(format!("{name}: {event:?}"));
                        break;
                    }
                    _ => {}
                }
            }
        } else {
            failure = Some(format!("{name}: local start failed"));
        }
        let observed = adapter.model_observation(&id);
        adapter.close(&id);
        eventually(|| supervisor.ledger().active_count() == 0).await;
        let model_matches = observed.as_ref().is_some_and(|(requested, actual)| requested == &model && actual.as_ref() == Some(&model));
        let files_ok = std::fs::read(&outside).unwrap() == b"original"
            && if access == WorkspaceAccess::Write {
                std::fs::read(workspace.join("allowed.txt")).is_ok_and(|bytes| bytes == b"compatibility-write\n")
            } else { true };
        let cleanup = store.log.lock().unwrap().iter().any(|record| record.run_id == id && record.state == ExecState::Exited && record.ended_at.is_some());
        let provider_interrupt_confirmed = !cancel || {
            let diagnostics = diagnostics.lock().unwrap();
            diagnostics.iter().any(|event| event["interrupt_reply"] == true && event["accepted"] == true)
                && diagnostics.iter().any(|event| event["turn_completed"] == "interrupted")
        };
        let passed = started && result_ok && model_matches && files_ok && cleanup
            && approvals == approved && provider_interrupt_confirmed;
        cases.push(json!({"case":name,"passed":passed,"started":started,"result_ok":result_ok,"model_matches":model_matches,"files_ok":files_ok,"cleanup":cleanup,"approvals":approvals,"approved_owned_file_changes":approved,"events":event_count,"interrupt_accepted":interrupted}));
        if !passed { break; }
    }
    let passed = cases.len() == if selected_case.is_some() { 1 } else { 3 }
        && cases.iter().all(|case| case["passed"] == true);
    let report = json!({"format":1,"runtime":"codex","version":version,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"auth_route":"subscription","provider":"openai","model":model,"tested_at":term_storage::time::now_iso8601(),"scope":"production adapter + native Exec; real managed subscription inference","cases":cases,"passed":passed,"failure":failure,"diagnostics":*diagnostics.lock().unwrap()});
    std::fs::write(evidence, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    assert!(
        passed,
        "installed compatibility failed; see explicit evidence output"
    );
}
