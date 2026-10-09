use iyagi_termd_lib::agent_runtime::fake::fake_binding;
use iyagi_termd_lib::connections::{ConnectionPreset, ConnectionStore, SecretRedactor};
use serde_json::json;
use term_contracts::mission::types::{AuthRoute, Id, RuntimeKind};
use zeroize::Zeroizing;

#[path = "support/credentials.rs"]
mod credentials;

const KEY: &str = "fake-key-for-local-tests-only";

#[test]
fn nested_json_strings_scrub_escaped_secrets_without_changing_clean_output() {
    use iyagi_termd_lib::exec::output::Redactor;
    let key = "fake-codex-\"quoted\"-test-key";
    let redactor = SecretRedactor::new([key.into()]);
    let report = json!({"report_text":format!("fixture credential echo: {key}")});
    let mut line = format!("{}\n", json!({"params":{"text":report.to_string()}}));
    redactor.redact(&mut line);
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    let nested: serde_json::Value =
        serde_json::from_str(value["params"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(nested["report_text"], "fixture credential echo: [redacted]");
    let clean = "{ \"params\": { \"text\": \"normal output\" } }\n";
    let mut line = clean.to_string();
    redactor.redact(&mut line);
    assert_eq!(line, clean);
}

#[test]
fn codex_api_connections_cannot_be_reused_by_another_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        ConnectionStore::with_credentials(dir.path().into(), credentials::MemoryCredentials::new());
    for preset in [ConnectionPreset::CodexApi, ConnectionPreset::OpenaiApi] {
        let info = store.create(preset, Zeroizing::new(KEY.into())).unwrap();
        let mut binding = fake_binding();
        binding.runtime = info.runtime;
        binding.provider_id = info.provider_id;
        binding.auth_route = info.auth_route;
        binding.credential_ref = Some(info.credential_ref);
        binding.endpoint_ref = Some(info.endpoint_ref);
        if binding.runtime == RuntimeKind::Codex {
            assert_eq!(
                store
                    .resolve_codex(&binding)
                    .unwrap()
                    .into_api_key()
                    .as_str(),
                KEY
            );
            assert!(store.resolve_opencode(&binding).is_err());
            binding.runtime = RuntimeKind::Opencode;
            assert!(store.resolve_opencode(&binding).is_err());
        } else {
            assert!(store.resolve_codex(&binding).is_err());
            binding.runtime = RuntimeKind::Codex;
            assert!(store.resolve_codex(&binding).is_err());
        }
    }
}

#[test]
fn claude_connections_are_bound_to_runtime_provider_and_auth_route() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        ConnectionStore::with_credentials(dir.path().into(), credentials::MemoryCredentials::new());
    for preset in [
        ConnectionPreset::ClaudeApi,
        ConnectionPreset::ClaudeSubscription,
        ConnectionPreset::ClaudeZaiCoding,
    ] {
        let info = store.create(preset, Zeroizing::new(KEY.into())).unwrap();
        let mut binding = fake_binding();
        binding.runtime = info.runtime;
        binding.provider_id = info.provider_id;
        binding.auth_route = info.auth_route;
        binding.credential_ref = Some(info.credential_ref);
        binding.endpoint_ref = Some(info.endpoint_ref);
        assert_eq!(
            store
                .resolve_claude(&binding)
                .unwrap()
                .into_api_key()
                .as_str(),
            KEY
        );
        assert!(store.resolve_codex(&binding).is_err());
        assert!(store.resolve_opencode(&binding).is_err());
        let mut wrong = binding.clone();
        wrong.runtime = RuntimeKind::Opencode;
        assert!(store.resolve_opencode(&wrong).is_err());
        let mut wrong = binding.clone();
        wrong.auth_route = if binding.auth_route == AuthRoute::ApiKey {
            AuthRoute::Subscription
        } else {
            AuthRoute::ApiKey
        };
        assert!(store.resolve_claude(&wrong).is_err());
        let mut wrong = binding.clone();
        wrong.provider_id = if binding.provider_id == "anthropic" {
            "zai-coding-plan"
        } else {
            "anthropic"
        }
        .into();
        assert!(store.resolve_claude(&wrong).is_err());
    }
}

#[test]
#[ignore = "explicit host OS credential-store smoke; creates and removes only its own random test entry"]
fn host_credential_store_resolves_and_revokes_an_owned_test_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConnectionStore::production(dir.path()).unwrap();
    let info = store
        .create(ConnectionPreset::ZaiCoding, Zeroizing::new(KEY.into()))
        .unwrap();
    let mut binding = fake_binding();
    binding.runtime = RuntimeKind::Opencode;
    binding.provider_id = info.provider_id;
    binding.auth_route = info.auth_route;
    binding.credential_ref = Some(info.credential_ref);
    binding.endpoint_ref = Some(info.endpoint_ref.clone());
    // Revoke before asserting, including after a failed read, so the test
    // does not retain a credential just because an assertion fails.
    let resolved = store.resolve_opencode(&binding);
    let cleanup = store.revoke(&info.endpoint_ref);
    cleanup.unwrap();
    assert_eq!(
        resolved.unwrap().environment()["IYAGI_PROVIDER_API_KEY"],
        KEY
    );
    assert!(store.resolve_opencode(&binding).is_err());
}

#[test]
fn connection_refs_resolve_without_persisting_keys_and_revoke_keeps_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let memory = credentials::MemoryCredentials::new();
    let store = ConnectionStore::with_credentials(dir.path().into(), memory.clone());
    for preset in [
        ConnectionPreset::OpenaiApi,
        ConnectionPreset::AnthropicApi,
        ConnectionPreset::ZaiApi,
        ConnectionPreset::ZaiCoding,
    ] {
        let info = store.create(preset, Zeroizing::new(KEY.into())).unwrap();
        let mut binding = fake_binding();
        binding.runtime = RuntimeKind::Opencode;
        binding.provider_id = info.provider_id.clone();
        binding.auth_route = info.auth_route;
        binding.endpoint_ref = Some(info.endpoint_ref.clone());
        binding.credential_ref = Some(info.credential_ref.clone());
        let resolved = store.resolve_opencode(&binding).unwrap();
        let env = resolved.environment();
        assert_eq!(env["IYAGI_PROVIDER_API_KEY"], KEY);
        assert!(!env["OPENCODE_CONFIG_CONTENT"].contains(KEY));
        let file = std::fs::read(dir.path().join(format!("{}.json", info.endpoint_ref))).unwrap();
        assert!(!String::from_utf8(file).unwrap().contains(KEY));
        let reopened = ConnectionStore::with_credentials(dir.path().into(), memory.clone());
        assert!(reopened.resolve_opencode(&binding).is_ok());
        store.revoke(&info.endpoint_ref).unwrap();
        assert!(reopened.resolve_opencode(&binding).is_err());
        assert_eq!(reopened.info(&info.endpoint_ref).unwrap(), info);
    }
    assert_eq!(store.list().unwrap().len(), 4);
}

#[test]
fn credential_is_bound_to_the_complete_destination_not_just_a_reference() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        ConnectionStore::with_credentials(dir.path().into(), credentials::MemoryCredentials::new());
    let mut info = store
        .create(ConnectionPreset::ZaiCoding, Zeroizing::new(KEY.into()))
        .unwrap();
    let mut binding = fake_binding();
    binding.runtime = RuntimeKind::Opencode;
    binding.provider_id = info.provider_id.clone();
    binding.auth_route = info.auth_route;
    binding.endpoint_ref = Some(info.endpoint_ref.clone());
    binding.credential_ref = Some(info.credential_ref.clone());
    assert!(store.resolve_opencode(&binding).is_ok());
    binding.auth_route = AuthRoute::ApiKey;
    assert!(store.resolve_opencode(&binding).is_err());
    // Even a valid, supported replacement endpoint cannot reuse the key.
    info.auth_route = AuthRoute::ApiKey;
    info.provider_id = "zai".into();
    info.base_url = "https://api.z.ai/api/paas/v4".into();
    binding.provider_id = info.provider_id.clone();
    std::fs::write(
        dir.path().join(format!("{}.json", info.endpoint_ref)),
        serde_json::to_vec(&info).unwrap(),
    )
    .unwrap();
    assert_eq!(
        store.resolve_opencode(&binding).err().unwrap().to_string(),
        "credential destination mismatch"
    );
    binding.credential_ref = Some(format!("keyring:{}", Id::generate()));
    assert!(store.resolve_opencode(&binding).is_err());
}

#[test]
fn endpoint_files_and_errors_are_bounded_and_symlinks_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        ConnectionStore::with_credentials(dir.path().into(), credentials::MemoryCredentials::new());
    let id = Id::generate();
    let path = dir.path().join(format!("{id}.json"));
    for bytes in [KEY.as_bytes().to_vec(), vec![b'x'; 8193]] {
        std::fs::write(&path, bytes).unwrap();
        let error = store.info(&id).unwrap_err().to_string();
        assert!(!error.contains(KEY));
    }
    #[cfg(unix)]
    {
        std::fs::remove_file(&path).unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"{}").unwrap();
        std::os::unix::fs::symlink(target, &path).unwrap();
        assert!(store.info(&id).is_err());
    }
    for key in ["", "short", "key with spaces", "key-with-newline\n"] {
        assert!(store
            .create(ConnectionPreset::ZaiCoding, Zeroizing::new(key.into()))
            .is_err());
    }
}

#[test]
fn effective_config_must_match_the_key_endpoint_and_only_the_selected_provider() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        ConnectionStore::with_credentials(dir.path().into(), credentials::MemoryCredentials::new());
    let info = store
        .create(ConnectionPreset::ZaiApi, Zeroizing::new(KEY.into()))
        .unwrap();
    let mut binding = fake_binding();
    binding.runtime = RuntimeKind::Opencode;
    binding.provider_id = info.provider_id.clone();
    binding.auth_route = info.auth_route;
    binding.credential_ref = Some(info.credential_ref);
    binding.endpoint_ref = Some(info.endpoint_ref);
    let resolved = store.resolve_opencode(&binding).unwrap();
    let good = json!({"provider":{"zai":{"options":{"baseURL":info.base_url,"apiKey":KEY}}}});
    assert!(resolved.verify_config(&good).is_ok());
    for patch in [
        json!({"baseURL":"https://example.invalid","apiKey":KEY}),
        json!({"baseURL":info.base_url,"apiKey":"ambient-key"}),
    ] {
        let mut bad = good.clone();
        bad["provider"]["zai"]["options"] = patch;
        assert!(resolved.verify_config(&bad).is_err());
    }
    let mut bad = good.clone();
    bad["provider"]["zai"]["npm"] = json!("untrusted-package");
    assert!(resolved.verify_config(&bad).is_err());
    let mut bad = good;
    bad["mcp"] = json!({"unexpected":{"command":["executable"]}});
    assert!(resolved.verify_config(&bad).is_err());
}

#[test]
fn redacts_nested_protocol_values_keys_and_json_escaped_keys() {
    use iyagi_termd_lib::exec::output::Redactor;
    let key = "fake-quote-\"-slash-\\";
    let redactor = SecretRedactor::new([key.into()]);
    let mut text = serde_json::to_string(&json!({key:[format!("echo {key}")]})).unwrap();
    redactor.redact(&mut text);
    assert!(!text.contains("fake-quote"));
    let mut value = json!({key:[format!("echo {key}")]});
    redactor.redact_json(&mut value);
    assert_eq!(value, json!({"[redacted]":["echo [redacted]"]}));
}

#[test]
fn cli_never_accepts_an_api_key_argument_or_echoes_invalid_stdin() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_iyagi-termd"))
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "connection",
            "add",
            "--preset",
            "zai-coding",
            "--key-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"invalid key on stdin\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("invalid key on stdin"));
    let help = Command::new(env!("CARGO_BIN_EXE_iyagi-termd"))
        .args(["connection", "add", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("--key-stdin"));
    assert!(!text.contains("--api-key"));
}
