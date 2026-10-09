use super::*;
use crate::connections::{ConnectionPreset, CredentialStore};
use std::collections::HashMap;
use term_contracts::mission::types::Id;
use zeroize::Zeroizing;

#[derive(Default)]
struct Memory(Mutex<HashMap<String, Zeroizing<String>>>);
impl CredentialStore for Memory {
    fn put(&self, account: &str, value: &str) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(account.into(), Zeroizing::new(value.into()));
        Ok(())
    }
    fn get(&self, account: &str) -> std::io::Result<Zeroizing<String>> {
        self.0
            .lock()
            .unwrap()
            .get(account)
            .cloned()
            .ok_or_else(|| permission_denied("missing test key"))
    }
    fn delete(&self, account: &str) -> std::io::Result<()> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}

#[test]
fn managed_subscription_metadata_cannot_be_an_api_profile_or_token() {
    let scope = AuthScope {
        source: Source::ManagedSubscription,
        permission: "plan",
    };
    for account in [
        serde_json::json!({"apiProvider":"firstParty","apiKeySource":"/login managed key","subscriptionType":"max"}),
        serde_json::json!({"apiProvider":"gateway","subscriptionType":"max"}),
        serde_json::json!({"apiProvider":"firstParty","tokenSource":"CLAUDE_CODE_OAUTH_TOKEN","subscriptionType":"max"}),
        serde_json::json!({"apiProvider":"firstParty","tokenSource":"none"}),
        serde_json::json!({"apiProvider":"firstParty"}),
    ] {
        assert!(!scope.verify_account(&serde_json::json!({"account":account})));
    }
    assert!(scope.verify_account(
        &serde_json::json!({"account":{"apiProvider":"firstParty","subscriptionType":"max"}})
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_after_authentication_does_not_send_the_prompt() {
    let fixture = std::env::var_os("IYAGI_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(if cfg!(windows) {
                    "term-fixture.exe"
                } else {
                    "term-fixture"
                })
        });
    assert!(
        fixture.is_file(),
        "build term-fixture before running native protocol tests"
    );
    initialize_metadata(
        &fixture.to_string_lossy(),
        ConnectionPreset::ClaudeApi,
        true,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit installed Claude metadata smoke; requires IYAGI_CLAUDE_BIN and IYAGI_CLAUDE_VERSION; never sends a prompt"]
async fn installed_claude_authentication_initializes_without_a_model_request() {
    let program = std::env::var("IYAGI_CLAUDE_BIN").expect("explicit installed Claude path");
    let version = std::env::var("IYAGI_CLAUDE_VERSION").expect("explicit expected version");
    assert!(Path::new(&program).is_absolute());
    let output = std::process::Command::new(&program)
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("{version} (Claude Code)")
    );
    for preset in [
        ConnectionPreset::ClaudeApi,
        ConnectionPreset::ClaudeSubscription,
        ConnectionPreset::ClaudeZaiCoding,
    ] {
        initialize_metadata(&program, preset, false).await;
    }
}

async fn initialize_metadata(program: &str, preset: ConnectionPreset, cancel_after_init: bool) {
    use crate::agent_runtime::claude::{
        build_launch_plan, claude_binding, default_admission_config, healthy_host, noop_persist,
        spawn_exec_child,
    };
    use crate::exec::ExecSupervisor;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let store = ConnectionStore::with_credentials(
        dir.path().join("connections"),
        Arc::new(Memory::default()),
    );
    let info = store
        .create(preset, Zeroizing::new("fake-claude-metadata-only".into()))
        .unwrap();
    let mut binding = claude_binding(program, info.auth_route);
    binding.provider_id = info.provider_id;
    binding.credential_ref = Some(info.credential_ref);
    binding.endpoint_ref = Some(info.endpoint_ref);
    binding.model_id = "claude-sonnet-4-6".into();
    let run = RunStart {
        task_kind: None,
        mission_id: Id::generate(),
        owner_daemon_id: Id::generate(),
        run_id: Id::generate(),
        fencing_token: 1,
        binding,
        workspace_access: WorkspaceAccess::ReadOnly,
        allow_network: false,
        context_path: dir.path().into(),
        workspace: Some(dir.path().into()),
        prompt_stdin: "MUST NEVER BE SENT".into(),
    };
    let configs = dir.path().join("configs");
    let mut plan = build_launch_plan(&run, None, &configs).unwrap();
    configure(&run, &mut plan, Some(&store), None, &configs).unwrap();
    let supervisor = Arc::new(ExecSupervisor::new(
        default_admission_config(),
        noop_persist(),
        healthy_host(),
    ));
    let runtime = tokio::runtime::Handle::current();
    let worker_supervisor = supervisor.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut process =
            spawn_exec_child(&run, &plan, &worker_supervisor, &runtime, cancel.clone()).unwrap();
        // The installed smoke never calls next_line. The fixture also
        // exercises cancellation at the precise auth-to-prompt boundary.
        let initialized = process.source.initialize();
        if cancel_after_init {
            cancel.store(true, Ordering::Release);
            assert!(process.source.next_line().is_none());
        }
        drop(process);
        initialized
    })
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while supervisor.ledger().active_count() > 0 || std::fs::read_dir(&configs).unwrap().count() > 0
    {
        assert!(
            Instant::now() < deadline,
            "metadata process/directory did not clean up"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(outcome.is_ok(), "{preset:?}: {outcome:?}");
    assert!(!dir.path().join(".iyagi-claude-prompt-received").exists());
}

#[test]
fn zai_subscription_without_a_connection_launches_from_the_key_store() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    term_secrets::LocalSecretStore::new(&secrets)
        .write("zai-key-fixture")
        .unwrap();
    let binding = {
        let mut binding = crate::agent_runtime::claude::claude_binding(
            "/usr/local/bin/claude",
            AuthRoute::Subscription,
        );
        binding.provider_id = "zai-coding-plan".into();
        binding.model_id = "glm-5.3".into();
        binding.credential_ref = None;
        binding.endpoint_ref = None;
        binding
    };
    let run = RunStart {
        task_kind: None,
        mission_id: Id::generate(),
        owner_daemon_id: Id::generate(),
        run_id: Id::generate(),
        fencing_token: 1,
        binding,
        workspace_access: WorkspaceAccess::ReadOnly,
        allow_network: false,
        context_path: dir.path().into(),
        workspace: Some(dir.path().into()),
        prompt_stdin: "MUST NEVER BE SENT".into(),
    };
    let configs = dir.path().join("configs");
    // No connection store at all: the key comes from the daemon's own store,
    // the same source the `ccg` launch profile reads.
    let mut plan = super::super::build_launch_plan(&run, None, &configs).unwrap();
    configure(&run, &mut plan, None, Some(&secrets), &configs).unwrap();
    assert_eq!(
        plan.env.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str),
        Some("zai-key-fixture")
    );
    assert_eq!(
        plan.env.get("ANTHROPIC_BASE_URL").map(String::as_str),
        Some(crate::connections::CLAUDE_ZAI_BASE_URL)
    );
    // The terminal's `ccg` profile and a mission on this provider share one
    // tuning table, so the same key cannot run at two different timeouts.
    let (timeout, timeout_value) = crate::claude_provider::ZAI_API_TIMEOUT_MS;
    assert_eq!(
        plan.env.get(timeout).map(String::as_str),
        Some(timeout_value)
    );
    // The compaction window belongs to the model: this id does not declare the
    // 1M context, so forcing 1M on it would compact too late to recover.
    assert_eq!(
        plan.env
            .get(crate::claude_provider::ZAI_AUTO_COMPACT_WINDOW.0)
            .map(String::as_str),
        None
    );
    // The launch journal redacts the key the run carries.
    let redactor = plan
        .redactor
        .clone()
        .expect("keystore run carries a redactor");
    let mut line = "token=zai-key-fixture ok".to_owned();
    redactor.redact_plain(&mut line);
    assert_eq!(line, "token=[redacted] ok");
    // An unconfigured key store refuses the launch with its reason code
    // instead of the store's I/O detail.
    let empty = tempfile::tempdir().unwrap();
    let mut plan2 = super::super::build_launch_plan(&run, None, &configs).unwrap();
    let error = configure(&run, &mut plan2, None, Some(empty.path()), &configs)
        .expect_err("an unconfigured key store refuses the launch");
    assert!(error.to_string().contains("zai_key_missing"), "{error}");

    // An id that does declare the 1M context takes the window too — the pick
    // the Z.ai quick-setup card offers by default.
    let mut wide = run;
    wide.binding.model_id = "glm-5.3[1m]".into();
    let mut plan3 = super::super::build_launch_plan(&wide, None, &configs).unwrap();
    configure(&wide, &mut plan3, None, Some(&secrets), &configs).unwrap();
    let (window, window_value) = crate::claude_provider::ZAI_AUTO_COMPACT_WINDOW;
    assert_eq!(
        plan3.env.get(window).map(String::as_str),
        Some(window_value)
    );
    assert_eq!(
        plan3.env.get(timeout).map(String::as_str),
        Some(timeout_value)
    );
}
