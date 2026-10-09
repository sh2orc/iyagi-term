//! Regression for the shared-daemon incident: every codex process the daemon
//! spawns for itself (local probes, model catalog, mission runs) must stay off
//! the user's shared codex background daemon (`~/.codex/app-server-control/`).
//! A workload that bootstraps or joins that daemon ties it to the workload's
//! process tree, and force-stopping the workload can wedge or destroy it —
//! after which every interactive codex terminal on the machine dies instantly.
//!
//! These cases run against script CLIs, so they assert the spawned argv (the
//! one thing a real CLI would see), not any particular handshake outcome.
#![cfg(unix)]
use iyagi_termd_lib::agent_runtime::{
    codex, detection::DetectionEnv, fake::fake_binding, local_probe,
};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use term_contracts::mission::types::{AuthRoute, RuntimeKind};

/// A CLI that records every invocation's argv, answers the version and
/// capability probes, and exits immediately for anything else.
fn recording_cli(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("codex");
    std::fs::write(
        &path,
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' \"$*\" >> \"$0.argv\"\n",
            "case \"$*\" in\n",
            "  *--version) printf 'codex-cli 0.159.0\\n'; exit 0;;\n",
            "esac\n",
            "exit 0\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// Every recorded app-server invocation, one argv per line.
fn recorded_spawn_argv(cli: &Path) -> Vec<String> {
    std::fs::read_to_string(cli.with_extension("argv"))
        .unwrap()
        .lines()
        .filter(|line| line.contains("app-server"))
        .map(str::to_owned)
        .collect()
}

fn binding_for(cli: &Path) -> term_contracts::mission::types::Binding {
    let mut binding = fake_binding();
    binding.runtime = RuntimeKind::Codex;
    binding.provider_id = "openai".into();
    binding.auth_route = AuthRoute::Subscription;
    binding.program = cli.to_string_lossy().into();
    binding
}

#[test]
fn local_probe_spawns_app_server_with_the_isolation_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let cli = recording_cli(dir.path());
    let binding = binding_for(&cli);
    // The probe reports a failed handshake (the script exits); only the argv
    // it was launched with matters here.
    let _report = local_probe::run(
        &binding,
        binding.program.as_str(),
        "0.159.0",
        &DetectionEnv::default(),
        local_probe::BUDGET,
    );
    let spawns = recorded_spawn_argv(&cli);
    assert!(!spawns.is_empty(), "the local probe spawned app-server");
    for argv in &spawns {
        assert_eq!(
            argv, "--no-daemon app-server",
            "app-server must be launched daemon-isolated"
        );
    }
}

#[test]
fn model_catalog_spawn_carries_the_isolation_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let cli = recording_cli(dir.path());
    // The listing fails its handshake against a script CLI; the launch argv is
    // still recorded and must carry the prefix.
    let _ = codex::list_models(&cli, dir.path(), Duration::from_secs(10));
    let spawns = recorded_spawn_argv(&cli);
    assert!(!spawns.is_empty(), "the catalog listing spawned app-server");
    for argv in &spawns {
        assert_eq!(
            argv, "--no-daemon app-server",
            "model catalog must be launched daemon-isolated"
        );
    }
}
