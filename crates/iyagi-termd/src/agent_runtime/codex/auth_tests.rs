use super::*;
use crate::agent_runtime::codex::{CodexAdapter, PeerError, PeerEvent, ProtocolPeer};
use crate::agent_runtime::{fake::fake_binding, AdapterEvent, AgentAdapter, WorkspaceAccess};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::Duration;
use term_contracts::mission::{types::Id, MissionErrorCode};

const KEY: &str = "fake-codex-auth-test-key";

fn run(route: AuthRoute) -> RunStart {
    let mut binding = fake_binding();
    binding.runtime = RuntimeKind::Codex;
    binding.provider_id = "openai".into();
    binding.auth_route = route;
    RunStart {
        task_kind: None,
        mission_id: Id::generate(),
        owner_daemon_id: Id::generate(),
        run_id: Id::generate(),
        fencing_token: 1,
        binding,
        workspace_access: WorkspaceAccess::ReadOnly,
        allow_network: false,
        context_path: std::env::temp_dir(),
        workspace: None,
        prompt_stdin: "private task prompt".into(),
    }
}

/// Faults change one server response, so the actual engine decides how far
/// the handshake may proceed and whether a key or task prompt may be sent.
struct AuthPeer {
    scope: AuthScope,
    fault: &'static str,
    queue: Mutex<VecDeque<Value>>,
    sent: Mutex<Vec<String>>,
    took_key: AtomicBool,
    closed: AtomicBool,
}
impl ProtocolPeer for AuthPeer {
    fn auth_scope(&self) -> Option<AuthScope> {
        Some(self.scope.clone())
    }
    fn take_api_key(&self) -> Option<Zeroizing<String>> {
        if self.scope.ephemeral {
            assert!(!self.took_key.swap(true, Ordering::AcqRel));
            Some(Zeroizing::new(KEY.into()))
        } else {
            None
        }
    }
    fn send(&self, message: &Value) -> Result<(), PeerError> {
        let method = message["method"].as_str().unwrap();
        self.sent.lock().unwrap().push(method.into());
        let mut config = json!({
            "model_provider":"openai", "model_providers":{},
            "openai_base_url":if self.scope.ephemeral {API_URL} else {CHATGPT_MODEL_URL}, "chatgpt_base_url":CHATGPT_URL,
            "cli_auth_credentials_store":"ephemeral",
            "mcp_servers":{},
            "features":{"apps":false,"plugins":false,"hooks":false,"multi_agent":false,"multi_agent_v2":false,"shell_snapshot":false,"browser_use":false,"computer_use":false,"in_app_browser":false},
            "notify":[],"shell_environment_policy":{"experimental_use_profile":false},"allow_login_shell":false,
            "forced_login_method": if self.scope.ephemeral {json!("api")} else {Value::Null}
        });
        match self.fault {
            "destination" => config["openai_base_url"] = json!("https://wrong.invalid/v1"),
            "oauth_destination" => config["chatgpt_base_url"] = json!("https://wrong.invalid"),
            "store" => config["cli_auth_credentials_store"] = json!("file"),
            "custom_provider" => {
                config["model_providers"] = json!({"openai":{"env_key":"OTHER_KEY"}})
            }
            "forced_api" => config["forced_login_method"] = json!("api"),
            "mcp" => config["mcp_servers"] = json!({"unconfined":{"command":"untrusted"}}),
            "hooks" => config["features"]["hooks"] = json!(true),
            "plugins" => config["features"]["plugins"] = json!(true),
            "apps" => config["features"]["apps"] = json!(true),
            "multi_agent" => config["features"]["multi_agent"] = json!(true),
            "shell_snapshot" => config["features"]["shell_snapshot"] = json!(true),
            "notify" => config["notify"] = json!(["untrusted"]),
            "profile" => {
                config["shell_environment_policy"]["experimental_use_profile"] = json!(true)
            }
            "browser" => config["features"]["browser_use"] = json!(true),
            "login_shell" => config["allow_login_shell"] = json!(true),
            "subscription_api_endpoint" => config["openai_base_url"] = json!(API_URL),
            _ => {}
        }
        let result = match method {
            "initialize" => {
                json!({"userAgent":"fixture", "codexHome":if self.fault=="home" {PathBuf::from("/wrong-home")} else {self.scope.home.clone()}})
            }
            "initialized" => return Ok(()),
            "config/read" => json!({"config":config}),
            "account/login/start" => {
                assert_eq!(message["params"]["apiKey"], KEY);
                if self.fault == "login_error" {
                    self.queue
                        .lock()
                        .unwrap()
                        .push_back(json!({"id":message["id"],"error":{"message":KEY}}));
                    return Ok(());
                }
                // Notifications during the initial login precede the
                // authoritative account/read; later changes must fail.
                self.queue
                    .lock()
                    .unwrap()
                    .push_back(json!({"method":"account/updated","params":{"authMode":null}}));
                json!({"type":"apiKey"})
            }
            "account/read" => {
                json!({"account":{"type":if self.fault=="account" {"wrong-auth"} else if self.scope.ephemeral {"apiKey"} else {"chatgpt"}},"requiresOpenaiAuth":self.fault!="no_auth"})
            }
            "model/list" => json!({"data":[{"id":"fake-model","model":"fake-model"}]}),
            "thread/start" => {
                if self.fault == "mcp" {
                    assert_eq!(
                        message["params"]["config"]["mcp_servers"]["unconfined"]["enabled"],
                        false
                    );
                }
                assert_eq!(message["params"]["config"]["web_search"], "disabled");
                json!({"thread":{"id":"auth-thread"},"model":match self.fault {"model_missing"=>Value::Null,"model_changed"=>json!("other-model"),_=>json!("fake-model")},"modelProvider":if self.fault=="thread_provider" {"custom"} else {"openai"}})
            }
            "mcpServerStatus/list" => {
                if self.fault == "mcp" {
                    json!({"data":[{"runtimeStatus":"connected","tools":{}}],"nextCursor":null})
                } else {
                    json!({"data":[],"nextCursor":null})
                }
            }
            "turn/start" => json!({"turn":{"id":"auth-turn","status":"inProgress","items":[]}}),
            _ => panic!("unexpected request {method}"),
        };
        let mut queue = self.queue.lock().unwrap();
        queue.push_back(json!({"id":message["id"],"result":result}));
        if method == "turn/start" {
            queue.push_back(json!({"method":"account/updated","params":{"authMode":if self.fault=="drift" {Value::Null} else if self.scope.ephemeral {json!("apikey")} else {json!("chatgpt")}}}));
            let report = json!({"kind":"report","report_text":"success","knowledge":[]});
            queue.push_back(json!({"method":"turn/completed","params":{"threadId":"auth-thread","turn":{"id":"auth-turn","status":"completed","items":[{"type":"agentMessage","id":"final","phase":"final_answer","text":report.to_string()}]}}}));
        }
        Ok(())
    }
    fn recv(&self) -> PeerEvent {
        self.queue
            .lock()
            .unwrap()
            .pop_front()
            .map(PeerEvent::Message)
            .unwrap_or(PeerEvent::Eof)
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

#[tokio::test]
async fn auth_guards_block_keys_or_prompts_at_the_correct_boundary() {
    for (route, fault, expected, last, key_sent) in [
        (
            AuthRoute::Subscription,
            "model_missing",
            Some(MissionErrorCode::ModelUnavailable),
            "thread/start",
            false,
        ),
        (
            AuthRoute::Subscription,
            "model_changed",
            Some(MissionErrorCode::ModelUnavailable),
            "thread/start",
            false,
        ),
        (
            AuthRoute::ApiKey,
            "home",
            Some(MissionErrorCode::PolicyDenied),
            "initialize",
            false,
        ),
        (
            AuthRoute::ApiKey,
            "destination",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::ApiKey,
            "oauth_destination",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::ApiKey,
            "store",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::ApiKey,
            "custom_provider",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::ApiKey,
            "login_error",
            Some(MissionErrorCode::AuthRequired),
            "account/login/start",
            true,
        ),
        (
            AuthRoute::ApiKey,
            "account",
            Some(MissionErrorCode::AuthRequired),
            "account/read",
            true,
        ),
        (
            AuthRoute::ApiKey,
            "no_auth",
            Some(MissionErrorCode::AuthRequired),
            "account/read",
            true,
        ),
        (
            AuthRoute::ApiKey,
            "thread_provider",
            Some(MissionErrorCode::PolicyDenied),
            "thread/start",
            true,
        ),
        (
            AuthRoute::ApiKey,
            "drift",
            Some(MissionErrorCode::AuthRequired),
            "turn/start",
            true,
        ),
        (
            AuthRoute::Subscription,
            "account",
            Some(MissionErrorCode::AuthRequired),
            "account/read",
            false,
        ),
        (
            AuthRoute::Subscription,
            "forced_api",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::Subscription,
            "drift",
            Some(MissionErrorCode::AuthRequired),
            "turn/start",
            false,
        ),
        (
            AuthRoute::Subscription,
            "mcp",
            Some(MissionErrorCode::PolicyDenied),
            "mcpServerStatus/list",
            false,
        ),
        (
            AuthRoute::Subscription,
            "hooks",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::Subscription,
            "plugins",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::Subscription,
            "apps",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::Subscription,
            "multi_agent",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::Subscription,
            "shell_snapshot",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::Subscription,
            "notify",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::Subscription,
            "profile",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::Subscription,
            "browser",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::Subscription,
            "login_shell",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (
            AuthRoute::Subscription,
            "subscription_api_endpoint",
            Some(MissionErrorCode::PolicyDenied),
            "config/read",
            false,
        ),
        (AuthRoute::Subscription, "ok", None, "turn/start", false),
        (AuthRoute::ApiKey, "ok", None, "turn/start", true),
    ] {
        let home = tempfile::tempdir().unwrap();
        let peer = Arc::new(AuthPeer {
            scope: AuthScope {
                home: home.path().into(),
                ephemeral: route == AuthRoute::ApiKey,
            },
            fault,
            queue: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
            took_key: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        });
        let factory_peer = peer.clone();
        let adapter = CodexAdapter::with_peer_factory(Arc::new(move |_| Ok(factory_peer.clone())));
        let mut stream = adapter.subscribe();
        adapter.start(run(route)).unwrap();
        let terminal = loop {
            let event = stream
                .next_timeout(Duration::from_secs(5))
                .await
                .expect("terminal event");
            assert!(
                !format!("{event:?}").contains(KEY),
                "upstream key in public event"
            );
            if !matches!(
                event,
                AdapterEvent::Started { .. }
                    | AdapterEvent::Activity { .. }
                    | AdapterEvent::ModelObserved { .. }
            ) {
                break event;
            }
        };
        match (expected, terminal) {
            (Some(expected), AdapterEvent::Failed { code, .. }) => {
                assert_eq!(code, expected, "{fault}")
            }
            (None, AdapterEvent::Result { .. }) => {}
            (expected, event) => panic!("{fault}: expected {expected:?}, got {event:?}"),
        }
        assert_eq!(
            peer.sent.lock().unwrap().last().map(String::as_str),
            Some(last),
            "{fault}"
        );
        assert_eq!(peer.took_key.load(Ordering::Acquire), key_sent, "{fault}");
        for _ in 0..100 {
            if peer.closed.load(Ordering::Acquire) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            peer.closed.load(Ordering::Acquire),
            "{fault}: peer was not cleaned up"
        );
    }
}

#[test]
fn unsupported_auth_and_subscription_api_refs_cannot_prepare_a_launch() {
    let dir = tempfile::tempdir().unwrap();
    for route in [AuthRoute::Local, AuthRoute::Custom] {
        assert_eq!(
            AuthLaunch::prepare(&run(route), None, dir.path())
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    let mut start = run(AuthRoute::Subscription);
    start.binding.credential_ref = Some(format!("keyring:{}", Id::generate()));
    assert_eq!(
        AuthLaunch::prepare(&start, None, dir.path())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// A workload run must stay off the user's shared codex background daemon:
/// when the installed CLI accepts `--no-daemon`, the prepared argv carries it
/// in front of the subcommand, so force-stopping the run can never take the
/// shared daemon (and with it every interactive codex terminal) down.
#[test]
#[cfg(unix)]
fn prepared_argv_isolates_the_run_from_the_shared_daemon() {
    fn script(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    // Answers the capability probe; anything else is irrelevant here.
    let supported = dir.path().join("codex");
    script(&supported, "printf 'codex-cli 0.159.0\\n'");
    let mut start = run(AuthRoute::Subscription);
    start.binding.program = supported.to_string_lossy().into();
    let launch = AuthLaunch::prepare(&start, None, dir.path()).unwrap();
    assert_eq!(launch.argv.first().map(String::as_str), Some("--no-daemon"));
    assert_eq!(launch.argv.get(1).map(String::as_str), Some("app-server"));

    // A CLI that rejects the flag keeps today's argv (older builds have no
    // shared daemon to avoid).
    let rejects = dir.path().join("codex-old");
    script(
        &rejects,
        "if [ \"$1\" = '--no-daemon' ]; then exit 2; fi\nprintf 'codex-cli 0.153.4\\n'",
    );
    start.binding.program = rejects.to_string_lossy().into();
    let launch = AuthLaunch::prepare(&start, None, dir.path()).unwrap();
    assert_eq!(launch.argv.first().map(String::as_str), Some("app-server"));
}
