//! Actual OpenAPI-shaped exchanges; no provider calls or credentials.
use iyagi_termd_lib::agent_runtime::opencode::{OpencodeAdapter, OpencodeTransport};
use iyagi_termd_lib::agent_runtime::{
    AdapterEvent, CancelReceipt, DeliveryReceipt, RunProbe, WorkspaceAccess,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use term_contracts::mission::types::{AuthRoute, Binding, Id, RuntimeCapabilities, Support};
use term_contracts::mission::MissionErrorCode;

#[path = "support/credentials.rs"]
mod credentials;

struct RecordedTransport {
    posts: Mutex<Vec<(String, Value)>>,
    scripted_posts: HashMap<String, Value>,
    saved: Mutex<Value>,
    events: Vec<Value>,
    event_calls: AtomicUsize,
    closes: AtomicUsize,
}
impl RecordedTransport {
    fn new(events: Vec<Value>, saved: Value) -> Self {
        Self {
            posts: Mutex::new(vec![]),
            scripted_posts: HashMap::from([
                ("/session".into(), json!({"id":"ses_owned"})),
                ("/session/ses_owned/prompt_async".into(), Value::Null),
                ("/session/ses_owned/abort".into(), json!(true)),
                ("/permission/per_owned/reply".into(), json!(true)),
            ]),
            saved: Mutex::new(saved),
            events,
            event_calls: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
        }
    }
    fn posted(&self, route: &str) -> Option<Value> {
        self.posts
            .lock()
            .unwrap()
            .iter()
            .find(|(r, _)| r == route)
            .map(|(_, v)| v.clone())
    }
    fn correlate(&self, value: &Value) -> Value {
        let parent = self
            .posted("/session/ses_owned/prompt_async")
            .and_then(|p| p["messageID"].as_str().map(str::to_owned))
            .unwrap_or_default();
        fn rewrite(v: &Value, parent: &str) -> Value {
            match v {
                Value::String(s) if s == "__parent__" => json!(parent),
                Value::Array(a) => a.iter().map(|v| rewrite(v, parent)).collect(),
                Value::Object(o) => Value::Object(
                    o.iter()
                        .map(|(k, v)| (k.clone(), rewrite(v, parent)))
                        .collect(),
                ),
                _ => v.clone(),
            }
        }
        rewrite(value, &parent)
    }
}
impl OpencodeTransport for RecordedTransport {
    fn post(&self, route: &str, body: &Value) -> Result<Value, String> {
        if route.ends_with("/prompt_async") {
            assert_eq!(
                self.event_calls.load(Ordering::SeqCst),
                1,
                "subscribe before POST"
            );
        }
        self.posts
            .lock()
            .unwrap()
            .push((route.into(), body.clone()));
        self.scripted_posts
            .get(route)
            .cloned()
            .ok_or_else(|| "scripted transport failure".into())
    }
    fn get(&self, route: &str) -> Result<Value, String> {
        assert!(route.starts_with("/session/ses_owned/message/"));
        Ok(self.correlate(&self.saved.lock().unwrap()))
    }
    fn event_stream(
        &self,
        _route: &str,
        on_event: &mut dyn FnMut(Value) -> bool,
    ) -> Result<(), String> {
        self.event_calls.fetch_add(1, Ordering::SeqCst);
        for e in &self.events {
            if !on_event(self.correlate(e)) {
                break;
            }
        }
        Ok(())
    }
    fn close(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}
fn binding() -> Binding {
    let yes = Support {
        supported: true,
        reason_code: None,
    };
    Binding {
        id: Id::generate(),
        revision: term_contracts::ids::U64String::parse("1").unwrap(),
        label: "opencode fixture".into(),
        runtime: term_contracts::mission::types::RuntimeKind::Opencode,
        program: "opencode".into(),
        runtime_version: Some("1.18.26".into()),
        provider_id: "zai-coding".into(),
        model_id: "glm-5.3".into(),
        effort: None,
        auth_route: AuthRoute::Subscription,
        credential_ref: None,
        endpoint_ref: None,
        capabilities: RuntimeCapabilities {
            structured_result: yes.clone(),
            events: yes.clone(),
            cancel: yes.clone(),
            resume: yes.clone(),
            steer: Support {
                supported: false,
                reason_code: Some("http_surface_no_steer".into()),
            },
            approval_reply: yes.clone(),
            read_only: yes.clone(),
            scoped_write: yes.clone(),
            model_listing: yes.clone(),
            usage: yes.clone(),
            native_terminal_attach: Support {
                supported: false,
                reason_code: Some("server_process".into()),
            },
        },
        checked_at: None,
        enabled: true,
        experimental_version: None,
        local_evidence: None,
        estimated_run_cost_usd_micros: None,
        resource_policy: term_contracts::mission::types::ResourcePolicy {
            reservation_bytes: term_contracts::ids::U64String::parse("2147483648").unwrap(),
            cpu_slots: 1,
            enforcement: term_contracts::launch::Enforcement::Observe,
            memory_max_bytes: None,
            cpu_max_cores: None,
            pids_max: None,
        },
    }
}

fn run_start() -> iyagi_termd_lib::agent_runtime::RunStart {
    iyagi_termd_lib::agent_runtime::RunStart {
        task_kind: None,
        mission_id: Id::generate(),
        owner_daemon_id: Id::generate(),
        workspace_access: iyagi_termd_lib::agent_runtime::WorkspaceAccess::ReadOnly,
        allow_network: false,
        run_id: Id::generate(),
        fencing_token: 7,
        binding: binding(),
        context_path: std::env::temp_dir(),
        workspace: None,
        prompt_stdin: "로그인 기능을 구현해라.".into(),
    }
}

fn assistant(id: &str, finish: Option<&str>) -> Value {
    let mut info = json!({"id":id,"sessionID":"ses_owned","role":"assistant","parentID":"__parent__",
        "providerID":"zai-coding","modelID":"glm-5.3","mode":"build","agent":"build","path":{"cwd":"/workspace","root":"/workspace"},
        "time":{"created":1},"cost":0.00012,"tokens":{"input":120,"output":45,"reasoning":0,"cache":{"read":0,"write":0}}});
    if let Some(finish) = finish {
        info["time"]["completed"] = json!(2);
        info["finish"] = json!(finish);
    }
    info
}
fn report() -> Value {
    json!({"kind":"report","report_text":"완료","knowledge":[]})
}
fn saved() -> Value {
    let mut info = assistant("msg_final", Some("stop"));
    info["structured"] = report();
    json!({"info":info,"parts":[]})
}
fn event(kind: &str, props: Value) -> Value {
    json!({"id":"evt_recorded","type":kind,"properties":props})
}
fn connected() -> Value {
    event("server.connected", json!({}))
}
fn update(info: Value) -> Value {
    event(
        "message.updated",
        json!({"sessionID":"ses_owned","info":info}),
    )
}
fn idle() -> Value {
    event(
        "session.status",
        json!({"sessionID":"ses_owned","status":{"type":"idle"}}),
    )
}
fn part(id: &str, text: &str) -> Value {
    event(
        "message.part.updated",
        json!({"sessionID":"ses_owned","time":1,"part":{"id":id,"sessionID":"ses_owned","messageID":"msg_final","type":"text","text":text}}),
    )
}
fn execute(
    events: Vec<Value>,
    saved: Value,
) -> (
    Arc<RecordedTransport>,
    OpencodeAdapter,
    Id,
    Vec<AdapterEvent>,
) {
    let transport = Arc::new(RecordedTransport::new(events, saved));
    let start = run_start();
    let adapter = OpencodeAdapter::with_transport(start.binding.clone(), transport.clone());
    let mut output = vec![];
    adapter.start(&start, |e| output.push(e)).unwrap();
    let _ = adapter.drive_turn(&start.run_id, "prompt", |e| output.push(e));
    (transport, adapter, start.run_id, output)
}

#[test]
fn session_and_prompt_have_their_distinct_explicit_model_contracts() {
    let (t, _, _, events) = execute(
        vec![
            connected(),
            connected(),
            idle(),
            update(assistant("msg_final", Some("stop"))),
            idle(),
        ],
        saved(),
    );
    assert_eq!(
        t.posted("/session").unwrap()["model"],
        json!({"providerID":"zai-coding","id":"glm-5.3"})
    );
    let body = t.posted("/session/ses_owned/prompt_async").unwrap();
    assert_eq!(
        body["model"],
        json!({"providerID":"zai-coding","modelID":"glm-5.3"})
    );
    assert_eq!(body["parts"], json!([{"type":"text","text":"prompt"}]));
    assert!(body.get("prompt").is_none());
    assert_eq!(body["format"]["type"], "json_schema");
    assert_eq!(body["format"]["retryCount"], 0);
    assert!(body["format"]["schema"].is_object());
    assert_eq!(
        t.posts
            .lock()
            .unwrap()
            .iter()
            .filter(|(r, _)| r.ends_with("prompt_async"))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::Result { .. }))
            .count(),
        1
    );
}

#[test]
fn permissions_derive_from_task_access_instead_of_capability_claims() {
    for write in [false, true] {
        for network in [false, true] {
            let mut start = run_start();
            start.workspace_access = if write {
                WorkspaceAccess::Write
            } else {
                WorkspaceAccess::ReadOnly
            };
            start.allow_network = network;
            start.binding.capabilities.read_only.supported = false;
            start.binding.capabilities.scoped_write.supported = false;
            let t = Arc::new(RecordedTransport::new(vec![], Value::Null));
            OpencodeAdapter::with_transport(start.binding.clone(), t.clone())
                .start(&start, |_| {})
                .unwrap();
            let rules = t.posted("/session").unwrap()["permission"]
                .as_array()
                .unwrap()
                .clone();
            let action = |tool: &str| {
                rules
                    .iter()
                    .rev()
                    .find(|v| v["permission"] == tool || v["permission"] == "*")
                    .unwrap()["action"]
                    .as_str()
                    .unwrap()
            };
            assert_eq!(action("edit"), if write { "allow" } else { "deny" });
            assert_eq!(action("webfetch"), if network { "allow" } else { "deny" });
            for tool in ["bash", "task", "external_directory", "some_custom_tool"] {
                assert_eq!(action(tool), "deny");
            }
        }
    }
}

#[test]
fn sse_correlates_session_parent_and_parts_without_utf8_slicing_panics() {
    let mut foreign = part("prt_foreign", "leak");
    foreign["properties"]["sessionID"] = json!("ses_other");
    let mut user = part("prt_user", "user text");
    user["properties"]["part"]["messageID"] = json!("msg_user");
    let mut stale = assistant("msg_stale", Some("stop"));
    stale["parentID"] = json!("msg_old");
    let events = vec![
        connected(),
        update(stale),
        foreign,
        user,
        update(assistant("msg_final", None)),
        part("prt_a", "a"),
        part("prt_a", "안녕"),
        part("prt_a", "안녕하세요"),
        part("prt_b", "둘"),
        event(
            "message.part.delta",
            json!({"sessionID":"ses_owned","messageID":"msg_final","partID":"prt_b","field":"text","delta":"째"}),
        ),
        part("prt_b", "둘째"),
        update(assistant("msg_final", Some("stop"))),
        idle(),
        part("prt_a", "late"),
    ];
    let (_, _, _, out) = execute(events, saved());
    let chunks: Vec<_> = out
        .iter()
        .filter_map(|e| {
            if let AdapterEvent::Activity { chunk, .. } = e {
                Some(chunk.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(chunks, vec!["a", "안녕", "하세요", "둘", "째"]);
    assert_eq!(
        out.iter()
            .filter(|e| matches!(e, AdapterEvent::Result { .. }))
            .count(),
        1
    );
}

#[test]
fn only_persisted_correlated_final_structured_evidence_can_succeed() {
    let sequence = || vec![connected(), update(saved()["info"].clone()), idle()];
    for change in [
        "missing",
        "malformed",
        "parent",
        "session",
        "id",
        "unfinished",
        "model",
    ] {
        let mut evidence = saved();
        match change {
            "missing" => {
                evidence["info"]
                    .as_object_mut()
                    .unwrap()
                    .remove("structured");
            }
            "malformed" => {
                evidence["info"]["structured"] =
                    json!({"kind":"report","report_text":"ok","knowledge":[],"fabricated":true})
            }
            "parent" => evidence["info"]["parentID"] = json!("msg_foreign"),
            "session" => evidence["info"]["sessionID"] = json!("ses_foreign"),
            "id" => evidence["info"]["id"] = json!("msg_other"),
            "unfinished" => {
                evidence["info"]["time"]
                    .as_object_mut()
                    .unwrap()
                    .remove("completed");
            }
            "model" => evidence["info"]["modelID"] = json!("default"),
            _ => unreachable!(),
        }
        let (_, _, _, out) = execute(sequence(), evidence);
        assert!(
            !out.iter().any(|e| matches!(e, AdapterEvent::Result { .. })),
            "{change}"
        );
        assert_eq!(
            out.iter().filter(|e| e.is_terminal()).count(),
            1,
            "{change}"
        );
        assert_eq!(
            out.iter()
                .any(|e| matches!(e, AdapterEvent::InvalidResult { .. })),
            matches!(change, "missing" | "malformed"),
            "only correlated completed answer failures carry correction evidence: {change}"
        );
    }
}

#[test]
fn disconnect_never_resubmits_and_idle_or_http_success_never_proves_exit() {
    for events in [
        vec![],
        vec![connected(), idle()],
        vec![
            connected(),
            update(assistant("msg_tool", Some("tool-calls"))),
            idle(),
        ],
    ] {
        let (t, a, id, out) = execute(events, json!({}));
        assert!(matches!(
            out.last(),
            Some(AdapterEvent::Disconnected { .. })
        ));
        assert_eq!(a.inspect(&id), RunProbe::Unknown);
        let posts = t.posts.lock().unwrap().len();
        assert_eq!(
            a.drive_turn(&id, "retry", |_| panic!("unexpected event"))
                .unwrap_err()
                .code,
            MissionErrorCode::InvalidState
        );
        assert_eq!(t.posts.lock().unwrap().len(), posts);
    }
}

#[test]
fn ambiguous_session_and_prompt_posts_are_not_retried() {
    for route in ["/session", "/session/ses_owned/prompt_async"] {
        let mut recorded = RecordedTransport::new(vec![connected(), connected()], saved());
        recorded.scripted_posts.remove(route);
        let t = Arc::new(recorded);
        let start = run_start();
        let a = OpencodeAdapter::with_transport(start.binding.clone(), t.clone());
        let mut events = vec![];
        let began = a.start(&start, |e| events.push(e));
        if began.is_ok() {
            let _ = a.drive_turn(&start.run_id, "first", |e| events.push(e));
        }
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AdapterEvent::Disconnected { .. }))
                .count(),
            1
        );
        assert!(a.start(&start, |_| {}).is_err());
        assert!(a.drive_turn(&start.run_id, "retry", |_| {}).is_err());
        assert_eq!(
            t.posts
                .lock()
                .unwrap()
                .iter()
                .filter(|(r, _)| r == route)
                .count(),
            1
        );
    }
}

#[test]
fn usage_sums_assistant_snapshots_once_and_preserves_unknowns() {
    let tool = assistant("msg_tool", Some("tool-calls"));
    let final_info = assistant("msg_final", Some("stop"));
    let sequence = vec![
        connected(),
        update(tool.clone()),
        update(tool),
        update(final_info.clone()),
        update(final_info),
        idle(),
    ];
    let (_, a, id, out) = execute(sequence.clone(), saved());
    assert_eq!(a.usage(&id), (Some(240), Some(90), Some(240)));
    assert!(out.iter().any(|e| matches!(
        e,
        AdapterEvent::Usage {
            input_tokens: Some(240),
            ..
        }
    )));
    let mut unknown = saved();
    unknown["info"]["tokens"] = json!({});
    unknown["info"]["cost"] = Value::Null;
    let (_, a, id, _) = execute(sequence, unknown);
    assert_eq!(a.usage(&id), (None, None, None));
}

#[test]
fn approval_answers_are_owned_single_use_and_never_always() {
    let approval = event(
        "permission.asked",
        json!({"id":"per_owned","sessionID":"ses_owned","permission":"edit","patterns":["a.rs"],"metadata":{},"always":[],"tool":{"messageID":"msg_final","callID":"call_1"}}),
    );
    let start = run_start();
    let t = Arc::new(RecordedTransport::new(
        vec![
            connected(),
            update(assistant("msg_final", None)),
            approval.clone(),
            approval,
            update(assistant("msg_final", Some("stop"))),
            idle(),
        ],
        saved(),
    ));
    let a = OpencodeAdapter::with_transport(start.binding.clone(), t.clone());
    a.start(&start, |_| {}).unwrap();
    let mut asks = 0;
    a.drive_turn(&start.run_id, "prompt", |e| {
        if let AdapterEvent::ApprovalRequested {
            provider_request_id,
            ..
        } = e
        {
            asks += 1;
            if asks == 1 {
                assert!(matches!(
                    a.answer(&Id::generate(), &provider_request_id, "once"),
                    DeliveryReceipt::Rejected { .. }
                ));
                assert!(matches!(
                    a.answer(&start.run_id, &provider_request_id, "always"),
                    DeliveryReceipt::Rejected { .. }
                ));
                assert!(matches!(
                    a.answer(&start.run_id, &provider_request_id, "approve"),
                    DeliveryReceipt::Delivered { .. }
                ));
                assert!(matches!(
                    a.answer(&start.run_id, &provider_request_id, "approve"),
                    DeliveryReceipt::Rejected { .. }
                ));
            }
        }
    })
    .unwrap();
    assert_eq!(asks, 1);
    assert_eq!(
        t.posted("/permission/per_owned/reply").unwrap(),
        json!({"reply":"once"})
    );
    assert!(matches!(
        a.answer(&start.run_id, "per_owned", "once"),
        DeliveryReceipt::Rejected { .. }
    ));
}

#[test]
fn provider_errors_are_normalized_and_raw_secrets_never_enter_events() {
    for (name, status, code) in [
        ("ProviderAuthError", 401, MissionErrorCode::AuthRequired),
        ("APIError", 429, MissionErrorCode::ProviderRateLimited),
        ("StructuredOutputError", 0, MissionErrorCode::ResultInvalid),
    ] {
        let (_, _, _, out) = execute(
            vec![
                connected(),
                event(
                    "session.error",
                    json!({"sessionID":"ses_owned","error":{"name":name,"data":{"message":"secret-key-value","responseBody":"secret-key-value","statusCode":status,"responseHeaders":{"Retry-After":"12","authorization":"secret-key-value"}}}}),
                ),
            ],
            Value::Null,
        );
        assert!(matches!(out.last(),Some(AdapterEvent::Failed{code:c,..}) if *c==code));
        assert!(!format!("{out:?}").contains("secret-key-value"));
        let limits: Vec<_> = out
            .iter()
            .filter_map(|e| {
                if let AdapterEvent::RateLimited { observation, .. } = e {
                    Some(observation)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(limits.len(), usize::from(status == 429));
        if let Some(limit) = limits.first() {
            assert_eq!(
                limit.resets_at_unix_ms.get() - limit.observed_at_unix_ms.get(),
                12_000
            );
        }
    }
}

#[test]
fn observed_model_mismatch_cannot_fall_back_to_default() {
    let mut info = assistant("msg_final", Some("stop"));
    info["providerID"] = json!("zai-general");
    let (_, _, _, out) = execute(vec![connected(), update(info)], saved());
    assert!(matches!(
        out.last(),
        Some(AdapterEvent::Failed {
            code: MissionErrorCode::ModelUnavailable,
            ..
        })
    ));
}

#[test]
fn abort_receipt_requires_true_and_never_claims_process_termination() {
    for value in [json!(true), json!(false), json!({})] {
        let mut t = RecordedTransport::new(vec![], Value::Null);
        t.scripted_posts
            .insert("/session/ses_owned/abort".into(), value.clone());
        let start = run_start();
        let a = OpencodeAdapter::with_transport(start.binding.clone(), Arc::new(t));
        a.start(&start, |_| {}).unwrap();
        assert_eq!(
            matches!(a.interrupt(&start.run_id), CancelReceipt::Accepted),
            value == true
        );
        assert_eq!(a.inspect(&start.run_id), RunProbe::Unknown);
        assert!(a.drive_turn(&start.run_id, "cancelled", |_| {}).is_err());
    }
}

#[test]
fn unknown_run_and_unsupported_steer_have_honest_receipts() {
    let t = Arc::new(RecordedTransport::new(vec![], Value::Null));
    let a = OpencodeAdapter::with_transport(binding(), t);
    assert!(matches!(
        a.interrupt(&Id::generate()),
        CancelReceipt::Rejected { .. }
    ));
    assert!(matches!(
        a.send_message(&Id::generate(), "message"),
        DeliveryReceipt::Queued { .. }
    ));
}

#[test]
fn probe_metadata_matches_live_evidence_registry() {
    // No recorded version → fully unclaimed (03 §6).
    let mut bare = binding();
    bare.runtime_version = None;
    let unclaimed = OpencodeAdapter::probe_metadata(&bare);
    assert!(!unclaimed.supported);
    assert_eq!(
        unclaimed.reason_code.as_deref(),
        Some("no_compatibility_evidence")
    );
    assert_eq!(unclaimed.provider_id, "zai-coding");
    assert_eq!(unclaimed.model_id, "glm-5.3");

    // Each OS needs its own recorded version. There is no macOS evidence;
    // a Windows version string cannot grant support on macOS.
    let mut live = binding();
    live.provider_id = "zai-coding-plan".into();
    live.credential_ref = Some(format!("keyring:{}", Id::generate()));
    live.endpoint_ref = Some(Id::generate());
    live.runtime_version = Some(
        if cfg!(target_os = "linux") {
            "1.18.30"
        } else {
            "1.18.26"
        }
        .into(),
    );
    let probe = OpencodeAdapter::probe_metadata(&live);
    let proven_here = cfg!(any(target_os = "windows", target_os = "linux"));
    assert_eq!(probe.supported, proven_here);
    assert_eq!(
        probe.reason_code.as_deref(),
        if proven_here {
            None
        } else {
            Some("no_compatibility_evidence")
        }
    );

    // An unknown newer version resets evidence to unclaimed.
    let mut newer = binding();
    newer.runtime_version = Some("1.19.0".into());
    let reset = OpencodeAdapter::probe_metadata(&newer);
    assert!(!reset.supported);
    assert_eq!(
        reset.reason_code.as_deref(),
        Some("no_compatibility_evidence")
    );
}

#[test]
fn binding_probe_reports_installation_without_paid_calls() {
    // Direct service-level probe: the real daemon path (mission_rpc probe
    // test covers the RPC surface); here we check the version capture logic
    // against the actually-installed opencode CLI. The CLI is optional
    // (CI runners and containers don't ship it) — skip, don't fail, when
    // it is absent; the probe contract is "version only, never a paid
    // call", which an absent binary cannot violate.
    let mut command = std::process::Command::new("opencode");
    command.arg("--version");
    #[cfg(windows)]
    {
        // npm-style shims are .cmd — must go through cmd.exe (and the
        // CVE-2024-24576 escaping rules) to be executable at all.
        command = {
            let mut wrapped = std::process::Command::new("cmd");
            wrapped.arg("/C").arg("opencode").arg("--version");
            wrapped
        };
    }
    let Ok(result) = command.output() else {
        eprintln!("skipping: opencode CLI not installed on this host");
        return;
    };
    let stdout = String::from_utf8_lossy(&result.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).trim().to_string();
    let text = if stdout.is_empty() { stderr } else { stdout };
    assert!(text.starts_with("1."), "unexpected version string {text:?}");
}

fn execution_context() -> (
    Arc<tokio::runtime::Runtime>,
    Arc<iyagi_termd_lib::exec::ExecSupervisor>,
    Arc<Mutex<Vec<term_contracts::mission::types::ExecRecord>>>,
) {
    use iyagi_termd_lib::exec::ExecSupervisor;
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap(),
    );
    let records = Arc::new(Mutex::new(vec![]));
    let sink = records.clone();
    let config = term_core::AdmissionConfig {
        logical_cpus: 8,
        managed_concurrency: 2,
        telemetry_stale_ms: 3000,
        host_reserve_min_bytes: 2 << 30,
        host_reserve_percent: 15,
        managed_budget_percent: 50,
    };
    let host = term_core::AdmissionHost {
        total_bytes: 16 << 30,
        available_bytes: Some(10 << 30),
        sample_age_ms: 0,
        reconciliation_required: false,
        pressure: term_contracts::metrics::PressureLevel::Normal,
    };
    let supervisor = Arc::new(ExecSupervisor::new(
        config,
        Arc::new(move |r| sink.lock().unwrap().push(r)),
        host,
    ));
    (runtime, supervisor, records)
}

fn fixture_start(workspace: &std::path::Path) -> iyagi_termd_lib::agent_runtime::RunStart {
    let mut start = run_start();
    let binary = std::env::var_os("IYAGI_FIXTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_BIN_EXE_iyagi-termd"))
                .parent()
                .unwrap()
                .join(if cfg!(windows) {
                    "term-fixture.exe"
                } else {
                    "term-fixture"
                })
        });
    assert!(
        binary.is_file(),
        "build term-fixture before running process tests"
    );
    start.binding.program = binary.to_str().unwrap().into();
    start.binding.runtime_version = Some("fixture".into());
    start.workspace = Some(workspace.to_owned());
    start
}

fn await_closed(adapter: &dyn iyagi_termd_lib::agent_runtime::AgentAdapter, id: &Id) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !matches!(adapter.close(id), CancelReceipt::Confirmed { .. }) {
        assert!(
            std::time::Instant::now() < deadline,
            "owned server cleanup timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn real_http_sse_child_completes_typed_result_and_releases_only_after_cleanup() {
    use iyagi_termd_lib::agent_runtime::opencode::{
        runtime::OpenCodeRuntimeAdapter, server, LiveServerPlan,
    };
    use iyagi_termd_lib::agent_runtime::AgentAdapter;
    let (runtime, supervisor, records) = execution_context();
    let work = tempfile::tempdir().unwrap();
    let start = fixture_start(work.path());
    let rt = runtime.clone();
    let sup = supervisor.clone();
    let adapter = OpenCodeRuntimeAdapter::with_factory(Arc::new(move |run, cancel| {
        let plan = LiveServerPlan::for_binding(&run.binding, run.workspace.clone().unwrap());
        server::spawn(run, &plan, &sup, rt.handle(), Default::default(), cancel)
    }));
    let mut stream = adapter.subscribe();
    adapter.start(start.clone()).unwrap();
    assert!(adapter.start(start.clone()).is_err());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut events = vec![];
    loop {
        if let Some(event) = stream.try_next() {
            assert_eq!(event.fencing_token(), 7);
            assert_eq!(event.run_id(), &start.run_id);
            let terminal = matches!(
                event,
                AdapterEvent::Result { .. }
                    | AdapterEvent::Failed { .. }
                    | AdapterEvent::Disconnected { .. }
            );
            events.push(event);
            if terminal {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no final event: {events:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    await_closed(adapter.as_ref(), &start.run_id);
    assert!(
        matches!(events.last(), Some(AdapterEvent::Result { .. })),
        "{events:?}"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e,AdapterEvent::Activity{chunk,..}if chunk=="작업 중")));
    assert_eq!(supervisor.ledger().active_count(), 0);
    let rows = records.lock().unwrap();
    assert!(rows.iter().all(|r| r.mission_id == start.mission_id
        && r.run_id == start.run_id
        && r.owner_daemon_id == start.owner_daemon_id));
    assert_eq!(
        rows.iter().map(|r| r.state).collect::<Vec<_>>(),
        vec![
            term_contracts::mission::types::ExecState::Prepared,
            term_contracts::mission::types::ExecState::Spawned,
            term_contracts::mission::types::ExecState::Exited
        ]
    );
    assert!(rows[1].identity.is_some());
}

#[test]
fn real_http_child_cancel_during_sse_confirms_cleanup_without_a_result() {
    use iyagi_termd_lib::agent_runtime::opencode::{
        runtime::OpenCodeRuntimeAdapter, server, LiveServerPlan,
    };
    use iyagi_termd_lib::agent_runtime::AgentAdapter;
    let (runtime, supervisor, _) = execution_context();
    let work = tempfile::tempdir().unwrap();
    let mut start = fixture_start(work.path());
    start.prompt_stdin = "hold".into();
    let rt = runtime.clone();
    let sup = supervisor.clone();
    let adapter = OpenCodeRuntimeAdapter::with_factory(Arc::new(move |run, cancel| {
        server::spawn(
            run,
            &LiveServerPlan::for_binding(&run.binding, run.workspace.clone().unwrap()),
            &sup,
            rt.handle(),
            Default::default(),
            cancel,
        )
    }));
    let mut stream = adapter.subscribe();
    adapter.start(start.clone()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(event) = stream.try_next() {
            assert!(!matches!(
                event,
                AdapterEvent::Result { .. } | AdapterEvent::Failed { .. }
            ));
            if matches!(event, AdapterEvent::Activity { .. }) {
                break;
            }
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(supervisor.ledger().active_count(), 1);
    assert!(matches!(
        adapter.interrupt(&start.run_id),
        CancelReceipt::Accepted
    ));
    await_closed(adapter.as_ref(), &start.run_id);
    while let Some(event) = stream.try_next() {
        assert!(!matches!(event, AdapterEvent::Result { .. }));
    }
    assert_eq!(supervisor.ledger().active_count(), 0);
}

#[test]
fn startup_timeout_reaps_the_owned_server_before_returning_error() {
    use iyagi_termd_lib::agent_runtime::opencode::{server, LiveServerPlan};
    let (runtime, supervisor, records) = execution_context();
    let work = tempfile::tempdir().unwrap();
    let start = fixture_start(work.path());
    let mut plan = LiveServerPlan::for_binding(&start.binding, work.path().into());
    plan.startup_timeout = std::time::Duration::from_millis(50);
    let env = std::collections::BTreeMap::from([(
        "IYAGI_FIXTURE_SERVER_START_DELAY_MS".into(),
        "3000".into(),
    )]);
    let result = server::spawn(
        &start,
        &plan,
        &supervisor,
        runtime.handle(),
        env,
        &std::sync::atomic::AtomicBool::new(false),
    );
    assert!(result.is_err());
    assert_eq!(supervisor.ledger().active_count(), 0);
    assert_eq!(
        records.lock().unwrap().last().unwrap().state,
        term_contracts::mission::types::ExecState::Exited
    );
}

#[test]
fn cancellation_can_interrupt_the_factory_before_a_session_exists() {
    use iyagi_termd_lib::agent_runtime::opencode::runtime::OpenCodeRuntimeAdapter;
    use iyagi_termd_lib::agent_runtime::AgentAdapter;
    let adapter = OpenCodeRuntimeAdapter::with_factory(Arc::new(|_, cancel| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !cancel.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err(std::io::Error::other("cancelled"))
    }));
    let start = run_start();
    adapter.start(start.clone()).unwrap();
    assert!(matches!(
        adapter.interrupt(&start.run_id),
        CancelReceipt::Accepted
    ));
    await_closed(adapter.as_ref(), &start.run_id);
}

#[test]
fn version_drift_closes_the_child_before_returning_a_failed_probe() {
    use iyagi_termd_lib::agent_runtime::opencode::{server, LiveServerPlan};
    let (runtime, supervisor, _) = execution_context();
    let work = tempfile::tempdir().unwrap();
    let mut start = fixture_start(work.path());
    start.binding.runtime_version = Some("unexpected-version".into());
    let plan = LiveServerPlan::for_binding(&start.binding, work.path().into());
    assert!(server::spawn(
        &start,
        &plan,
        &supervisor,
        runtime.handle(),
        Default::default(),
        &std::sync::atomic::AtomicBool::new(false)
    )
    .is_err());
    assert_eq!(supervisor.ledger().active_count(), 0);
}

#[test]
fn missing_executable_releases_the_exec_reservation_and_records_no_launch() {
    use iyagi_termd_lib::agent_runtime::opencode::{server, LiveServerPlan};
    let (runtime, supervisor, records) = execution_context();
    let work = tempfile::tempdir().unwrap();
    let mut start = fixture_start(work.path());
    start.binding.program = work
        .path()
        .join("missing-opencode")
        .to_string_lossy()
        .into_owned();
    let plan = LiveServerPlan::for_binding(&start.binding, work.path().into());
    assert!(server::spawn(
        &start,
        &plan,
        &supervisor,
        runtime.handle(),
        Default::default(),
        &std::sync::atomic::AtomicBool::new(false)
    )
    .is_err());
    assert_eq!(supervisor.ledger().active_count(), 0);
    let records = records.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0].state,
        term_contracts::mission::types::ExecState::Prepared
    );
    assert_eq!(
        records[1].state,
        term_contracts::mission::types::ExecState::Exited
    );
    assert!(records[1].identity.is_none());
}

#[test]
#[ignore = "requires IYAGI_OPENCODE_SMOKE_PROGRAM and IYAGI_OPENCODE_SMOKE_VERSION; metadata only, no inference"]
fn installed_opencode_authenticated_health_openapi_and_cleanup() {
    use iyagi_termd_lib::agent_runtime::opencode::{server, LiveServerPlan};
    let (runtime, supervisor, _) = execution_context();
    let work = tempfile::tempdir().unwrap();
    let mut start = run_start();
    start.binding.program =
        std::env::var("IYAGI_OPENCODE_SMOKE_PROGRAM").expect("explicit installed CLI path");
    start.binding.runtime_version = Some(
        std::env::var("IYAGI_OPENCODE_SMOKE_VERSION").expect("explicit installed CLI version"),
    );
    start.binding.provider_id = "iyagi-metadata-only".into();
    start.binding.model_id = "no-inference".into();
    start.workspace = Some(work.path().into());
    let plan = LiveServerPlan::for_binding(&start.binding, work.path().into());
    let mut env = std::collections::BTreeMap::new();
    for key in [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "OPENCODE_CONFIG_DIR",
    ] {
        let path = work.path().join(key.to_ascii_lowercase());
        std::fs::create_dir(&path).unwrap();
        env.insert(key.into(), path.to_string_lossy().into_owned());
    }
    for key in [
        "OPENCODE_DISABLE_MODELS_FETCH",
        "OPENCODE_DISABLE_DEFAULT_PLUGINS",
        "OPENCODE_DISABLE_CLAUDE_CODE",
    ] {
        env.insert(key.into(), "true".into());
    }
    let transport = server::spawn(
        &start,
        &plan,
        &supervisor,
        runtime.handle(),
        env,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .expect("owned installed server starts");
    let spec = transport.get("/doc");
    transport.close();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !matches!(transport.process_probe(), RunProbe::Finished { .. }) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let spec = spec.expect("authenticated OpenAPI read");
    assert!(spec["openapi"]
        .as_str()
        .is_some_and(|v| v.starts_with("3.1.")));
    assert_eq!(
        spec["paths"]["/session/{sessionID}/prompt_async"]["post"]["operationId"],
        "session.prompt_async"
    );
    assert_eq!(supervisor.ledger().active_count(), 0);
}

#[test]
fn permission_reply_transport_loss_reports_unknown_and_is_never_reposted() {
    let start = run_start();
    let approval = event(
        "permission.asked",
        json!({"id":"per_owned","sessionID":"ses_owned","permission":"edit","patterns":["a"],"metadata":{},"always":[]}),
    );
    let mut t = RecordedTransport::new(vec![connected(), approval.clone(), approval], Value::Null);
    t.scripted_posts.remove("/permission/per_owned/reply");
    let t = Arc::new(t);
    let a = OpencodeAdapter::with_transport(start.binding.clone(), t.clone());
    a.start(&start, |_| {}).unwrap();
    let mut asks = 0;
    let _ = a.drive_turn(&start.run_id, "prompt", |event| {
        if let AdapterEvent::ApprovalRequested {
            provider_request_id,
            ..
        } = event
        {
            asks += 1;
            assert!(matches!(
                a.answer(&start.run_id, &provider_request_id, "accept"),
                DeliveryReceipt::Unknown { .. }
            ));
            assert!(matches!(
                a.answer(&start.run_id, &provider_request_id, "accept"),
                DeliveryReceipt::Rejected { .. }
            ));
        }
    });
    assert_eq!(asks, 1);
    assert_eq!(
        t.posts
            .lock()
            .unwrap()
            .iter()
            .filter(|(route, _)| route == "/permission/per_owned/reply")
            .count(),
        1
    );
}

#[test]
fn late_updates_of_an_older_assistant_do_not_replace_the_final_turn() {
    let old = assistant("msg_tool", Some("tool-calls"));
    let (_, a, id, events) = execute(
        vec![
            connected(),
            update(old.clone()),
            update(assistant("msg_final", Some("stop"))),
            update(old),
            idle(),
        ],
        saved(),
    );
    assert_eq!(a.provider_ids(&id).1.as_deref(), Some("msg_final"));
    assert!(matches!(events.last(), Some(AdapterEvent::Result { .. })));
}

#[test]
fn production_factory_resolves_distinct_connections_and_scrubs_provider_echoes() {
    use iyagi_termd_lib::agent_runtime::opencode::runtime::OpenCodeRuntimeAdapter;
    use iyagi_termd_lib::agent_runtime::AgentAdapter;
    use iyagi_termd_lib::connections::{ConnectionPreset, ConnectionStore};
    let (runtime, supervisor, records) = execution_context();
    let work = tempfile::tempdir().unwrap();
    let connection_dir = work.path().join("connections");
    let config_root = work.path().join("runtime");
    let store = Arc::new(ConnectionStore::with_credentials(
        connection_dir,
        credentials::MemoryCredentials::new(),
    ));
    let adapter = OpenCodeRuntimeAdapter::supervised(
        supervisor.clone(),
        runtime.handle().clone(),
        store.clone(),
        config_root.clone(),
    );
    let mut events = adapter.subscribe();
    let mut starts = vec![];
    for (preset, key) in [
        (ConnectionPreset::ZaiApi, "fixture-general-key-123"),
        (ConnectionPreset::ZaiCoding, "fixture-coding-key-456"),
    ] {
        let info = store
            .create(preset, zeroize::Zeroizing::new(key.into()))
            .unwrap();
        let mut start = fixture_start(work.path());
        start.binding.provider_id = info.provider_id;
        start.binding.auth_route = info.auth_route;
        start.binding.credential_ref = Some(info.credential_ref);
        start.binding.endpoint_ref = Some(info.endpoint_ref);
        start.prompt_stdin = "auth-echo".into();
        adapter.start(start.clone()).unwrap();
        starts.push(start);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut completed = std::collections::HashSet::new();
    while completed.len() != starts.len() {
        if let Some(event) = events.try_next() {
            let printed = format!("{event:?}");
            assert!(
                !printed.contains("fixture-general-key-123")
                    && !printed.contains("fixture-coding-key-456")
            );
            match event {
                AdapterEvent::Result { run_id, result, .. } => {
                    assert!(serde_json::to_string(&result)
                        .unwrap()
                        .contains("[redacted]"));
                    completed.insert(run_id);
                }
                AdapterEvent::Failed { .. } | AdapterEvent::Disconnected { .. } => {
                    panic!("{printed}")
                }
                _ => {}
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "connection runs did not complete"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    for start in &starts {
        await_closed(adapter.as_ref(), &start.run_id);
    }
    drop(adapter);
    while std::fs::read_dir(&config_root).unwrap().next().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "private runtime directory retained after cleanup"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(supervisor.ledger().active_count(), 0);
    assert_eq!(
        records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.state == term_contracts::mission::types::ExecState::Prepared)
            .count(),
        2
    );
}

#[test]
fn revoked_connection_fails_with_auth_required_before_creating_an_exec() {
    use iyagi_termd_lib::agent_runtime::opencode::runtime::OpenCodeRuntimeAdapter;
    use iyagi_termd_lib::agent_runtime::AgentAdapter;
    use iyagi_termd_lib::connections::{ConnectionPreset, ConnectionStore};
    let (runtime, supervisor, records) = execution_context();
    let work = tempfile::tempdir().unwrap();
    let store = Arc::new(ConnectionStore::with_credentials(
        work.path().join("connections"),
        credentials::MemoryCredentials::new(),
    ));
    let info = store
        .create(
            ConnectionPreset::ZaiApi,
            zeroize::Zeroizing::new("fixture-test-key".into()),
        )
        .unwrap();
    store.revoke(&info.endpoint_ref).unwrap();
    let mut start = fixture_start(work.path());
    start.binding.provider_id = info.provider_id;
    start.binding.auth_route = info.auth_route;
    start.binding.credential_ref = Some(info.credential_ref);
    start.binding.endpoint_ref = Some(info.endpoint_ref);
    let adapter = OpenCodeRuntimeAdapter::supervised(
        supervisor.clone(),
        runtime.handle().clone(),
        store,
        work.path().join("runtime"),
    );
    let mut events = adapter.subscribe();
    adapter.start(start.clone()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(event) = events.try_next() {
            assert!(
                matches!(
                    event,
                    AdapterEvent::Failed {
                        code: MissionErrorCode::AuthRequired,
                        ..
                    }
                ),
                "{event:?}"
            );
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    await_closed(adapter.as_ref(), &start.run_id);
    assert!(records.lock().unwrap().is_empty());
    assert_eq!(supervisor.ledger().active_count(), 0);
    assert!(!work.path().join("runtime").exists());
}

#[test]
#[ignore = "explicit installed OpenCode metadata probe, no inference or real keys"]
fn installed_opencode_verifies_resolved_endpoint_and_credential_configuration() {
    use iyagi_termd_lib::agent_runtime::opencode::server;
    use iyagi_termd_lib::connections::{ConnectionPreset, ConnectionStore};
    let (runtime, supervisor, _) = execution_context();
    let work = tempfile::tempdir().unwrap();
    let store = ConnectionStore::with_credentials(
        work.path().join("connections"),
        credentials::MemoryCredentials::new(),
    );
    let info = store
        .create(
            ConnectionPreset::ZaiCoding,
            zeroize::Zeroizing::new("iyagi-fake-key-metadata-only".into()),
        )
        .unwrap();
    let mut start = run_start();
    start.binding.program =
        std::env::var("IYAGI_OPENCODE_SMOKE_PROGRAM").expect("installed CLI path");
    start.binding.runtime_version =
        Some(std::env::var("IYAGI_OPENCODE_SMOKE_VERSION").expect("installed version"));
    start.binding.provider_id = info.provider_id;
    start.binding.auth_route = info.auth_route;
    start.binding.credential_ref = Some(info.credential_ref);
    start.binding.endpoint_ref = Some(info.endpoint_ref);
    start.binding.model_id = "no-inference".into();
    start.workspace = Some(work.path().into());
    // A project configuration must not override the isolated connection.
    std::fs::write(
        work.path().join("opencode.json"),
        r#"{"provider":{"zai-coding-plan":{"options":{"baseURL":"https://invalid.example"}}}}"#,
    )
    .unwrap();
    let transport = server::spawn_resolved(
        &start,
        &supervisor,
        runtime.handle(),
        store.resolve_opencode(&start.binding).unwrap(),
        &work.path().join("runtime"),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert!(transport
        .get("/config")
        .unwrap()
        .to_string()
        .contains("[redacted]"));
    transport.close();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !matches!(transport.process_probe(), RunProbe::Finished { .. }) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(supervisor.ledger().active_count(), 0);
}
