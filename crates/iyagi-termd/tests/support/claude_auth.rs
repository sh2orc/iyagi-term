use super::{
    credentials, eventually, fixture_bin, gated_supervisor, next_codex_terminal as next_terminal,
    run_start, FaultStore,
};
use iyagi_termd_lib::{
    agent_runtime::{
        claude::ClaudePrintAdapter, AdapterEvent, AgentAdapter, CancelReceipt, RunStart,
    },
    connections::{ConnectionPreset, ConnectionStore},
    exec::ExecSupervisor,
};
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use term_contracts::mission::{
    types::{Id, ProviderResult, RuntimeKind},
    MissionErrorCode,
};

const KEY: &str = "fake-claude-\"quoted\"-key";
struct Fixture {
    _dir: Arc<tempfile::TempDir>,
    workspace: PathBuf,
    configs: PathBuf,
    store: Arc<FaultStore>,
    supervisor: Arc<ExecSupervisor>,
    connections: Arc<ConnectionStore>,
    adapter: Arc<ClaudePrintAdapter>,
    start: RunStart,
}
impl Fixture {
    fn new(preset: ConnectionPreset, mode: &str) -> Self {
        Self::with_key(preset, mode, KEY)
    }
    fn with_key(preset: ConnectionPreset, mode: &str, key: &str) -> Self {
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let workspace = dir.path().join("work");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(
            workspace.join(".iyagi-claude-fixture.json"),
            serde_json::json!({"mode":mode}).to_string(),
        )
        .unwrap();
        let configs = dir.path().join("configs");
        let store = Arc::new(FaultStore::default());
        let supervisor = Arc::new(gated_supervisor(store.clone(), dir.path()));
        let connections = Arc::new(ConnectionStore::with_credentials(
            dir.path().join("connections"),
            credentials::MemoryCredentials::new(),
        ));
        let info = connections
            .create(preset, zeroize::Zeroizing::new(key.into()))
            .unwrap();
        let adapter = ClaudePrintAdapter::authenticated(
            supervisor.clone(),
            tokio::runtime::Handle::current(),
            configs.clone(),
            None,
            Some(connections.clone()),
        );
        let mut start = run_start();
        start.binding.runtime = RuntimeKind::Claude;
        start.binding.program = fixture_bin().to_string_lossy().into_owned();
        start.binding.provider_id = info.provider_id;
        start.binding.model_id = "fixture-model".into();
        start.binding.auth_route = info.auth_route;
        start.binding.credential_ref = Some(info.credential_ref);
        start.binding.endpoint_ref = Some(info.endpoint_ref);
        start.workspace = Some(workspace.clone());
        start.prompt_stdin = "auth-echo".into();
        Self {
            _dir: dir,
            workspace,
            configs,
            store,
            supervisor,
            connections,
            adapter,
            start,
        }
    }
    fn config_count(&self) -> usize {
        std::fs::read_dir(&self.configs)
            .map(|d| d.count())
            .unwrap_or(0)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.store.fail_exit.store(false, Ordering::Release);
    }
}

#[tokio::test]
async fn api_and_subscription_tokens_use_isolated_environments_and_durable_cleanup() {
    for preset in [
        ConnectionPreset::ClaudeApi,
        ConnectionPreset::ClaudeSubscription,
        ConnectionPreset::ClaudeZaiCoding,
    ] {
        let fixture = Fixture::new(preset, "");
        fixture.store.fail_exit.store(true, Ordering::Release);
        let mut events = fixture.adapter.subscribe();
        fixture.adapter.start(fixture.start.clone()).unwrap();
        assert!(
            fixture.adapter.start(fixture.start.clone()).is_err(),
            "same Run launched twice"
        );
        eventually(|| {
            fixture
                .workspace
                .join(".iyagi-claude-prompt-received")
                .exists()
        })
        .await;
        eventually(|| fixture.store.exit_attempts.load(Ordering::Acquire) > 0).await;
        assert_eq!(fixture.config_count(), 1);
        assert_eq!(fixture.supervisor.ledger().active_count(), 1);
        let observation: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.workspace.join(".iyagi-claude-observation.json")).unwrap(),
        )
        .unwrap();
        assert!(!observation.to_string().contains(KEY));
        let expected = match preset {
            ConnectionPreset::ClaudeApi => "ANTHROPIC_API_KEY",
            ConnectionPreset::ClaudeZaiCoding => "ANTHROPIC_AUTH_TOKEN",
            _ => "CLAUDE_CODE_OAUTH_TOKEN",
        };
        let keys = observation["env_keys"].as_array().unwrap();
        for name in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ] {
            assert_eq!(keys.contains(&serde_json::json!(name)), name == expected);
        }
        assert_eq!(
            observation["base_url"],
            if matches!(preset, ConnectionPreset::ClaudeZaiCoding) {
                "https://api.z.ai/api/anthropic"
            } else {
                "https://api.anthropic.com"
            }
        );
        assert!(
            PathBuf::from(observation["config"].as_str().unwrap()).starts_with(&fixture.configs)
        );
        while let Some(event) = events.try_next() {
            assert!(
                !matches!(event, AdapterEvent::Result { .. }),
                "result escaped while cleanup persistence failed"
            );
            assert!(!format!("{event:?}").contains(KEY));
        }
        fixture.store.fail_exit.store(false, Ordering::Release);
        match next_terminal(&mut events).await {
            AdapterEvent::Result {
                result: ProviderResult::Report { report_text, .. },
                ..
            } => assert_eq!(report_text, "fixture credential echo: [redacted]"),
            other => panic!("{other:?}"),
        }
        eventually(|| {
            fixture.config_count() == 0 && fixture.supervisor.ledger().active_count() == 0
        })
        .await;
        assert!(matches!(
            fixture.adapter.close(&fixture.start.run_id),
            CancelReceipt::Confirmed { .. }
        ));
        let records = fixture.store.log.lock().unwrap().len();
        fixture
            .connections
            .revoke(fixture.start.binding.endpoint_ref.as_ref().unwrap())
            .unwrap();
        let mut start = fixture.start.clone();
        start.run_id = Id::generate();
        assert_eq!(
            fixture.adapter.start(start).err().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(fixture.store.log.lock().unwrap().len(), records);
    }
}

#[tokio::test]
async fn auth_or_permission_mismatch_never_sends_the_prompt() {
    for (mode, expected) in [
        ("auth_mismatch", MissionErrorCode::AuthRequired),
        ("permission_mismatch", MissionErrorCode::PolicyDenied),
        ("wrong_id", MissionErrorCode::ResultInvalid),
    ] {
        let fixture = Fixture::new(ConnectionPreset::ClaudeApi, mode);
        let mut events = fixture.adapter.subscribe();
        fixture.adapter.start(fixture.start.clone()).unwrap();
        match next_terminal(&mut events).await {
            AdapterEvent::Failed { code, .. } => assert_eq!(code, expected),
            other => panic!("{mode}: {other:?}"),
        }
        assert!(!fixture
            .workspace
            .join(".iyagi-claude-prompt-received")
            .exists());
        eventually(|| {
            fixture.config_count() == 0 && fixture.supervisor.ledger().active_count() == 0
        })
        .await;
        assert!(matches!(
            fixture.adapter.close(&fixture.start.run_id),
            CancelReceipt::Confirmed { .. }
        ));
    }
}

#[tokio::test]
async fn initialization_transport_failure_proves_no_task_frame_after_durable_cleanup() {
    let fixture = Fixture::new(ConnectionPreset::ClaudeApi, "exit_before_init_reply");
    fixture.store.fail_exit.store(true, Ordering::Release);
    let mut events = fixture.adapter.subscribe();
    fixture.adapter.start(fixture.start.clone()).unwrap();
    eventually(|| {
        fixture
            .workspace
            .join(".iyagi-claude-init-received")
            .is_file()
    })
    .await;
    assert!(!fixture
        .workspace
        .join(".iyagi-claude-prompt-received")
        .exists());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        events.try_next().is_none(),
        "failure must wait for durable cleanup"
    );
    fixture.store.fail_exit.store(false, Ordering::Release);
    let event = events.next_timeout(Duration::from_secs(5)).await.unwrap();
    assert!(matches!(
        event,
        AdapterEvent::FailedBeforeSubmission {
            code: MissionErrorCode::ProviderUnavailable,
            ..
        }
    ));
    assert!(!fixture
        .workspace
        .join(".iyagi-claude-prompt-received")
        .exists());
    eventually(|| fixture.config_count() == 0 && fixture.supervisor.ledger().active_count() == 0)
        .await;
    assert!(matches!(
        fixture.adapter.close(&fixture.start.run_id),
        CancelReceipt::Confirmed { .. }
    ));
}

#[tokio::test]
async fn cancellation_during_auth_initialization_reaps_without_sending_a_task() {
    let fixture = Fixture::new(ConnectionPreset::ClaudeSubscription, "hold_init");
    fixture.adapter.start(fixture.start.clone()).unwrap();
    eventually(|| {
        fixture
            .workspace
            .join(".iyagi-claude-init-received")
            .exists()
    })
    .await;
    let adapter = fixture.adapter.clone();
    let id = fixture.start.run_id.clone();
    let _ = tokio::task::spawn_blocking(move || adapter.interrupt(&id))
        .await
        .unwrap();
    eventually(|| fixture.supervisor.ledger().active_count() == 0 && fixture.config_count() == 0)
        .await;
    assert!(!fixture
        .workspace
        .join(".iyagi-claude-prompt-received")
        .exists());
    assert!(matches!(
        fixture.adapter.close(&fixture.start.run_id),
        CancelReceipt::Confirmed { .. }
    ));
    assert!(fixture
        .adapter
        .subscribe()
        .next_timeout(Duration::from_millis(1))
        .await
        .is_none());
}

#[tokio::test]
async fn different_provider_connections_remain_isolated_while_running_together() {
    let anthropic = Fixture::with_key(
        ConnectionPreset::ClaudeApi,
        "hold_report",
        "fake-anthropic-only-key",
    );
    let zai = Fixture::with_key(
        ConnectionPreset::ClaudeZaiCoding,
        "hold_report",
        "fake-zai-only-key",
    );
    let mut anthropic_events = anthropic.adapter.subscribe();
    let mut zai_events = zai.adapter.subscribe();
    for fixture in [&anthropic, &zai] {
        fixture.adapter.start(fixture.start.clone()).unwrap();
    }
    for fixture in [&anthropic, &zai] {
        eventually(|| {
            fixture
                .workspace
                .join(".iyagi-claude-observation.json")
                .exists()
        })
        .await;
        assert_eq!(fixture.supervisor.ledger().active_count(), 1);
        assert_eq!(fixture.config_count(), 1);
    }
    let observation = |fixture: &Fixture| -> serde_json::Value {
        serde_json::from_slice(
            &std::fs::read(fixture.workspace.join(".iyagi-claude-observation.json")).unwrap(),
        )
        .unwrap()
    };
    let first = observation(&anthropic);
    let second = observation(&zai);
    assert_ne!(first["config"], second["config"]);
    assert_eq!(first["base_url"], "https://api.anthropic.com");
    assert_eq!(second["base_url"], "https://api.z.ai/api/anthropic");
    for fixture in [&anthropic, &zai] {
        std::fs::write(fixture.workspace.join(".iyagi-claude-release"), b"release").unwrap();
    }
    for events in [&mut anthropic_events, &mut zai_events] {
        match next_terminal(events).await {
            AdapterEvent::Result {
                result: ProviderResult::Report { report_text, .. },
                ..
            } => {
                // Each process echoes its actual environment key. Only its
                // own registered key is redacted by that process's supervisor.
                assert_eq!(report_text, "fixture credential echo: [redacted]");
            }
            other => panic!("{other:?}"),
        }
    }
    for fixture in [&anthropic, &zai] {
        eventually(|| {
            fixture.config_count() == 0 && fixture.supervisor.ledger().active_count() == 0
        })
        .await;
    }
}

#[tokio::test]
async fn dropping_adapter_during_auth_initialization_reaps_its_child_and_private_home() {
    let fixture = Fixture::new(ConnectionPreset::ClaudeApi, "hold_init");
    fixture.adapter.start(fixture.start.clone()).unwrap();
    eventually(|| {
        fixture
            .workspace
            .join(".iyagi-claude-init-received")
            .exists()
    })
    .await;
    let dir = fixture._dir.clone();
    let supervisor = fixture.supervisor.clone();
    let configs = fixture.configs.clone();
    let workspace = fixture.workspace.clone();
    drop(fixture);
    eventually(|| {
        supervisor.ledger().active_count() == 0 && std::fs::read_dir(&configs).unwrap().count() == 0
    })
    .await;
    assert!(!workspace.join(".iyagi-claude-prompt-received").exists());
    drop(dir);
}

#[tokio::test]
async fn authenticated_success_requires_a_structured_provider_result() {
    let fixture = Fixture::new(ConnectionPreset::ClaudeApi, "no_structured");
    let mut events = fixture.adapter.subscribe();
    fixture.adapter.start(fixture.start.clone()).unwrap();
    match next_terminal(&mut events).await {
        AdapterEvent::InvalidResult { code, .. } => {
            assert_eq!(code, MissionErrorCode::ResultInvalid)
        }
        other => panic!("{other:?}"),
    }
    eventually(|| fixture.config_count() == 0 && fixture.supervisor.ledger().active_count() == 0)
        .await;
}
