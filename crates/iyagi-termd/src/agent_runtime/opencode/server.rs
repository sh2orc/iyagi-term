//! Run-owned server launch through the shared Exec supervisor. The caller
//! supplies admission, persistent Exec observation, and a live Tokio runtime;
//! this module never creates an untracked Child or a private fake host ledger.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use super::http::HttpTransport;
use super::{LiveServerPlan, OpencodeTransport};
use crate::agent_runtime::{RunProbe, RunStart};
use crate::exec::{ExecHandle, ExecProbe, ExecSupervisor, FnRedactor, SpawnRequest, StreamKind};
use serde_json::json;

#[derive(Default)]
struct Isolation {
    connection: Option<crate::connections::ResolvedConnection>,
    directory: Option<Arc<tempfile::TempDir>>,
}

/// Production launch: no inherited provider auth or user/project config.
/// The private directory stays owned by cleanup until the process and its
/// durable completion are both confirmed.
pub fn spawn_resolved(
    start: &RunStart,
    supervisor: &ExecSupervisor,
    runtime: &tokio::runtime::Handle,
    connection: crate::connections::ResolvedConnection,
    root: &std::path::Path,
    cancelled: &AtomicBool,
) -> std::io::Result<Arc<dyn OpencodeTransport>> {
    let workspace = start
        .workspace
        .as_ref()
        .ok_or_else(|| std::io::Error::other("OpenCode requires an owned workspace"))?;
    if start.binding.runtime_version.is_none() {
        return Err(std::io::Error::other(
            "OpenCode binding must be probed before launch",
        ));
    }
    crate::connections::private_directory(root)?;
    let private = Arc::new(tempfile::Builder::new().prefix("run-").tempdir_in(root)?);
    let mut environment = connection.environment();
    // Deliberately small host allowlist. HOME/XDG/temp and every provider
    // credential/config variable are replaced, never inherited.
    for name in [
        "PATH",
        "LANG",
        "LC_ALL",
        "TZ",
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
    ] {
        if let Ok(value) = std::env::var(name) {
            environment.insert(name.into(), value);
        }
    }
    for (name, sub) in [
        ("HOME", "home"),
        ("USERPROFILE", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("TMPDIR", "tmp"),
        ("TMP", "tmp"),
        ("TEMP", "tmp"),
    ] {
        let path = private.path().join(sub);
        crate::connections::private_directory(&path)?;
        environment.insert(name.into(), path.to_string_lossy().into_owned());
    }
    environment.insert("OPENCODE_DISABLE_PROJECT_CONFIG".into(), "true".into());
    let plan = LiveServerPlan::for_binding(&start.binding, workspace.clone());
    spawn_inner(
        start,
        &plan,
        supervisor,
        runtime,
        environment,
        cancelled,
        Isolation {
            connection: Some(connection),
            directory: Some(private),
        },
    )
}

/// Secret values arrive only as daemon-resolved environment overrides. The
/// caller must resolve a binding's auth/endpoint refs before invoking this
/// function; they must never become command-line arguments or log messages.
pub fn spawn(
    start: &RunStart,
    plan: &LiveServerPlan,
    supervisor: &ExecSupervisor,
    runtime: &tokio::runtime::Handle,
    environment: BTreeMap<String, String>,
    cancelled: &AtomicBool,
) -> std::io::Result<Arc<dyn OpencodeTransport>> {
    spawn_inner(
        start,
        plan,
        supervisor,
        runtime,
        environment,
        cancelled,
        Isolation::default(),
    )
}

fn spawn_inner(
    start: &RunStart,
    plan: &LiveServerPlan,
    supervisor: &ExecSupervisor,
    runtime: &tokio::runtime::Handle,
    mut environment: BTreeMap<String, String>,
    cancelled: &AtomicBool,
    isolation: Isolation,
) -> std::io::Result<Arc<dyn OpencodeTransport>> {
    if cancelled.load(Ordering::Acquire) {
        return Err(std::io::Error::other("OpenCode launch cancelled"));
    }
    let password = format!(
        "{}{}",
        term_contracts::mission::types::Id::generate(),
        term_contracts::mission::types::Id::generate()
    );
    environment.insert("OPENCODE_SERVER_USERNAME".into(), "opencode".into());
    environment.insert("OPENCODE_SERVER_PASSWORD".into(), password.clone());
    environment.insert("OPENCODE_DISABLE_AUTOUPDATE".into(), "true".into());
    environment.insert("OPENCODE_DISABLE_PRUNE".into(), "true".into());
    environment.insert("OPENCODE_DISABLE_LSP_DOWNLOAD".into(), "true".into());
    // Repository and user instruction files must not reach an automated run
    // (10 §5): no CLAUDE.md fallback and no Claude Code/external skill scans.
    environment.insert("OPENCODE_DISABLE_CLAUDE_CODE".into(), "true".into());
    environment.insert("OPENCODE_DISABLE_EXTERNAL_SKILLS".into(), "true".into());
    // This is a dedicated run, not a user's interactive session. Inline
    // settings disable unrelated automatic work without altering global files.
    let mut config: serde_json::Value = match environment.get("OPENCODE_CONFIG_CONTENT") {
        Some(raw) => serde_json::from_str(raw)
            .map_err(|_| std::io::Error::other("invalid resolved OpenCode configuration"))?,
        None => json!({}),
    };
    let object = config.as_object_mut().ok_or_else(|| {
        std::io::Error::other("resolved OpenCode configuration must be an object")
    })?;
    object.insert(
        "model".into(),
        json!(format!(
            "{}/{}",
            start.binding.provider_id, start.binding.model_id
        )),
    );
    object.insert(
        "enabled_providers".into(),
        json!([start.binding.provider_id]),
    );
    object.insert("autoupdate".into(), json!(false));
    object.insert("share".into(), json!("disabled"));
    object.insert("snapshot".into(), json!(false));
    object.insert("formatter".into(), json!(false));
    object.insert("lsp".into(), json!(false));
    environment.insert("OPENCODE_CONFIG_CONTENT".into(), config.to_string());
    let (port_tx, port_rx) = mpsc::sync_channel(1);
    let private = isolation.directory.clone();
    let sink = Arc::new(move |stream, bytes: &[u8]| {
        // Also covers launch/readiness failure before a transport exists.
        let _private = &private;
        if stream != StreamKind::Stdout {
            return;
        }
        let line = crate::agent_model::strip_ansi(&String::from_utf8_lossy(bytes));
        if let Some(address) = listening_address(&line) {
            let _ = port_tx.try_send(address);
        }
    });
    let hidden = password.clone();
    let provider_redactor = isolation.connection.as_ref().map(|c| c.redactor());
    use crate::exec::output::Redactor;
    use base64::Engine;
    let hidden_basic =
        base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"));
    let redactor = Arc::new(FnRedactor(move |line: &mut String| {
        *line = line.replace(&hidden, "[redacted]");
        *line = line.replace(&hidden_basic, "[redacted]");
        if let Some(redactor) = &provider_redactor {
            redactor.redact(line);
        }
    }));
    let exec = supervisor
        .spawn_on(
            SpawnRequest {
                exec_id: term_contracts::mission::types::Id::generate(),
                mission_id: start.mission_id.clone(),
                run_id: start.run_id.clone(),
                owner_daemon_id: start.owner_daemon_id.clone(),
                program: plan.program.clone().into(),
                argv: plan.argv.clone(),
                cwd: plan.cwd.clone(),
                env_overrides: environment,
                env_clear: isolation.connection.is_some(),
                stdin: None,
                resource_policy: start.binding.resource_policy.clone(),
                spool_bytes: 64 * 1024,
                redactor: Some(redactor),
                sink,
                validate_path: None,
            },
            runtime,
        )
        .map_err(|_| std::io::Error::other("OpenCode server launch was rejected"))?;
    let connected = connect(
        &exec,
        &port_rx,
        &password,
        runtime,
        plan.startup_timeout,
        cancelled,
        start.binding.runtime_version.as_deref(),
        &isolation,
        start,
    );
    if connected.is_err() {
        // This function runs on the adapter's worker thread. The supplied
        // runtime continues pumping pipes during confirmed cleanup.
        exec.stop_blocking(Duration::from_millis(100), Duration::from_millis(100))
            .map_err(|_| std::io::Error::other("OpenCode startup cleanup is unconfirmed"))?;
    }
    connected
}

#[allow(clippy::too_many_arguments)]
fn connect(
    exec: &ExecHandle,
    ports: &mpsc::Receiver<SocketAddr>,
    password: &str,
    runtime: &tokio::runtime::Handle,
    timeout: Duration,
    cancelled: &AtomicBool,
    expected_version: Option<&str>,
    isolation: &Isolation,
    start: &RunStart,
) -> std::io::Result<Arc<dyn OpencodeTransport>> {
    let deadline = Instant::now() + timeout;
    let address = loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(std::io::Error::other("OpenCode launch cancelled"));
        }
        if !matches!(exec.inspect(), ExecProbe::Running) {
            return Err(std::io::Error::other(
                "OpenCode server exited before readiness",
            ));
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "OpenCode server readiness timed out",
            ));
        }
        match ports.recv_timeout(Duration::from_millis(25)) {
            Ok(address) => break address,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(std::io::Error::other("OpenCode startup output ended"))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    let process = exec.clone();
    let stopped = Arc::new(AtomicBool::new(false));
    let cleanup_complete = stopped.clone();
    let spawn_runtime = runtime.clone();
    let private = isolation.directory.clone();
    let stop = Arc::new(move || {
        let process = process.clone();
        let complete = cleanup_complete.clone();
        let private = private.clone();
        spawn_runtime.spawn(async move {
            let _private = private;
            loop {
                if process
                    .stop(Duration::from_millis(100), Duration::from_millis(100))
                    .await
                    .is_ok()
                {
                    complete.store(true, Ordering::Release);
                    break;
                }
                // close is single-use at the HTTP boundary. The owning
                // worker must retry storage completion without reissuing a
                // provider request or abandoning the reservation.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
    });
    let process = exec.clone();
    let probe = Arc::new(move || {
        if !stopped.load(Ordering::Acquire) {
            return RunProbe::Running;
        }
        match process.inspect() {
            ExecProbe::Finished { exit } => RunProbe::Finished { exit },
            _ => RunProbe::Unknown,
        }
    });
    let mut transport =
        HttpTransport::new(address, password, stop, probe).map_err(std::io::Error::other)?;
    transport
        .verify_authentication()
        .map_err(std::io::Error::other)?;
    let health = transport
        .get("/global/health")
        .map_err(|_| std::io::Error::other("OpenCode server health check failed"))?;
    if health["healthy"] != true || health["version"].as_str().is_none() {
        transport.close();
        return Err(std::io::Error::other(
            "OpenCode server health response is invalid",
        ));
    }
    if expected_version.is_some_and(|version| health["version"].as_str() != Some(version)) {
        transport.close();
        return Err(std::io::Error::other(
            "OpenCode server version changed since binding probe",
        ));
    }
    if cancelled.load(Ordering::Acquire) {
        transport.close();
        return Err(std::io::Error::other("OpenCode launch cancelled"));
    }
    if let Some(connection) = &isolation.connection {
        let config = transport.get("/config").map_err(|_| {
            std::io::Error::other("OpenCode effective configuration could not be verified")
        })?;
        connection.verify_config(&config)?;
        if config["model"]
            != json!(format!(
                "{}/{}",
                start.binding.provider_id, start.binding.model_id
            ))
            || config["enabled_providers"] != json!([start.binding.provider_id])
        {
            return Err(std::io::Error::other(
                "OpenCode effective model configuration differs from the binding",
            ));
        }
    }
    use base64::Engine;
    let secrets = vec![
        password.to_owned(),
        base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}")),
    ];
    let redactor = match &isolation.connection {
        Some(connection) => connection.redactor_with(secrets),
        None => Arc::new(crate::connections::SecretRedactor::new(secrets)),
    };
    transport = transport.with_redactor(redactor);
    Ok(Arc::new(transport))
}

fn listening_address(line: &str) -> Option<SocketAddr> {
    let address: SocketAddr = line
        .trim()
        .strip_prefix("opencode server listening on http://")?
        .parse()
        .ok()?;
    (address.ip() == Ipv4Addr::LOCALHOST && address.port() != 0).then_some(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_accepts_only_the_literal_owned_loopback_announcement() {
        assert!(
            listening_address("opencode server listening on http://127.0.0.1:34567\n").is_some()
        );
        for line in [
            "http://127.0.0.1:34567",
            "opencode server listening on http://0.0.0.0:34567",
            "opencode server listening on http://127.0.0.1:0",
            "opencode server listening on http://127.0.0.1:34567/path",
            "opencode server listening on http://user:pass@127.0.0.1:34567",
        ] {
            assert!(listening_address(line).is_none());
        }
    }
}
