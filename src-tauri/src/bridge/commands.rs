//! Tauri command surface of the bridge (command table in done/I04-bridge.md).
//!
//! Every argument is validated before it reaches the daemon connection:
//! ids must be UUID v4 strings, epochs printable bounded strings, the RPC
//! method must be in the allowlist (transport-owned `hello` and data-only
//! `session.ack` excluded), and tokens are never logged or echoed.

use serde_json::Value;
use tauri::ipc::Channel;
use tauri::State;
use uuid::Uuid;

use term_contracts::error::{ErrorCode, RpcError};
use term_contracts::ids::{SessionId, U64String};
use term_contracts::mission::rpc::methods as mission_methods;
use term_contracts::rpc::{methods, HelloResult, RpcRequest};
use term_contracts::session::SessionAck;

use super::connection::BridgeError;
use super::shell_profiles::{self, ShellProfilesError};
use super::state::BridgeState;
use super::system::{self, CliCandidate};

/// Frontend-callable RPC methods. `hello` is bridge-owned (always the first
/// frame, 01 §3) and `session.ack` is data-connection-only (`bridge_ack`).
/// `agent_session.report` is deliberately absent: only the hook CLI's own
/// connection reports captures — the UI just lists and forgets them.
/// The O1 mission surface (O04) rides the same control connection; the
/// daemon answers unimplemented methods with CAPABILITY_UNSUPPORTED.
pub fn is_allowed_method(method: &str) -> bool {
    matches!(
        method,
        methods::SYSTEM_SNAPSHOT
            | methods::WORKLOAD_LAUNCH
            | methods::WORKLOAD_CANCEL
            | methods::WORKLOAD_REPRIORITIZE
            | methods::WORKLOAD_UPDATE_POLICY
            | methods::WORKLOAD_PROCESSES
            | methods::SESSION_ATTACH
            | methods::SESSION_DETACH
            | methods::SESSION_INPUT
            | methods::SESSION_RESIZE
            | methods::SESSION_TAKE_CONTROL
            | methods::SESSION_FOCUS
            | methods::SESSION_RELIEF
            | methods::RELIEF_SET_POLICY
            | methods::WORKLOAD_SUSPEND
            | methods::WORKLOAD_RESUME
            | methods::GUARD_SET_POLICY
            | methods::SESSION_SEARCH
            | methods::INTERVENTION_LIST
            | methods::AGENT_SESSION_LIST
            | methods::AGENT_SESSION_FORGET
            | methods::RETENTION_SET_LIMIT
            | methods::DAEMON_SHUTDOWN
            | mission_methods::MISSION_CREATE
            | mission_methods::MISSION_LIST
            | mission_methods::MISSION_SNAPSHOT
            | mission_methods::MISSION_EVENTS
            | mission_methods::MISSION_CONTROL
            | mission_methods::MISSION_ACCEPT
            | mission_methods::MISSION_MESSAGE
            | mission_methods::MISSION_PLAN_APPLY
            | mission_methods::MISSION_TASK_CONTROL
            | mission_methods::MISSION_POLICY_UPDATE
            | mission_methods::MISSION_FINDING_RESOLVE
            | mission_methods::MISSION_DECISION_ANSWER
            | mission_methods::MISSION_REQUEST_GET
            | mission_methods::MISSION_ACTIVITY
            | mission_methods::BINDING_LIST
            | mission_methods::BINDING_SAVE
            | mission_methods::BINDING_PROBE
            | mission_methods::RUNTIME_DETECT
            | mission_methods::REPOSITORY_INSPECT
            | mission_methods::TEMPLATE_LIST
            | mission_methods::TEMPLATE_SAVE
            | mission_methods::VERIFICATION_LIST
            | mission_methods::VERIFICATION_SAVE
            | mission_methods::ARTIFACT_BEGIN
            | mission_methods::ARTIFACT_WRITE
            | mission_methods::ARTIFACT_COMMIT
            | mission_methods::ARTIFACT_READ
            | mission_methods::MISSION_RUN_ATTEST_EXITED
            | mission_methods::WORKSPACE_USAGE
            | mission_methods::WORKSPACE_CLEANUP
    )
}

fn invalid_argument(message: &str) -> RpcError {
    RpcError::new(ErrorCode::InvalidArgument, message)
}

fn validate_rpc_id(id: &str) -> Result<(), RpcError> {
    // Graphic ASCII only (0x21..=0x7e): no spaces, controls, or non-ASCII.
    let printable =
        !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| (0x21..0x7f).contains(&b));
    if printable {
        Ok(())
    } else {
        Err(invalid_argument(
            "id must be 1..=128 printable ASCII characters",
        ))
    }
}

fn validate_epoch(epoch: &str) -> Result<(), RpcError> {
    let printable =
        !epoch.is_empty() && epoch.len() <= 128 && epoch.bytes().all(|b| (0x21..0x7f).contains(&b));
    if printable {
        Ok(())
    } else {
        Err(invalid_argument(
            "epoch must be 1..=128 printable ASCII characters",
        ))
    }
}

/// Connect the control connection: resolve the data dir, spawn the daemon if
/// its runtime files are missing, retry until ready, then hello. Returns the
/// daemon capabilities and the one-shot data token (consumed by
/// `bridge_open_data` within its 5 s TTL — return promptly).
#[tauri::command]
pub async fn bridge_connect(
    state: State<'_, BridgeState>,
    data_dir: Option<String>,
) -> Result<HelloResult, RpcError> {
    let mut inner = state.inner.lock().await;
    inner
        .connect(data_dir.as_deref())
        .await
        .map_err(BridgeError::into_rpc)
}

/// Open the data connection using the single-use data token minted by
/// `bridge_connect`. Idempotent while the data connection is alive.
#[tauri::command]
pub async fn bridge_open_data(state: State<'_, BridgeState>) -> Result<(), RpcError> {
    let mut inner = state.inner.lock().await;
    inner.open_data().await.map_err(BridgeError::into_rpc)
}

/// One RPC over the control connection: id-matched response, 5 s timeout →
/// `DAEMON_UNAVAILABLE` (retryable). Holds no lock while awaiting.
#[tauri::command]
pub async fn bridge_rpc(
    state: State<'_, BridgeState>,
    id: String,
    method: String,
    params: Value,
) -> Result<Value, RpcError> {
    validate_rpc_id(&id)?;
    if !is_allowed_method(&method) {
        return Err(invalid_argument("method is not allowed through the bridge"));
    }
    let conn = {
        let inner = state.inner.lock().await;
        inner
            .control()
            .ok_or_else(|| RpcError::new(ErrorCode::DaemonUnavailable, "bridge is not connected"))?
    };
    let result = match conn.call(&id, &method, params).await {
        Ok(result) => result,
        // O1/future error codes travel verbatim to the webview (the mission
        // client parses `details` off them — 01 §7).
        Err(BridgeError::RpcRaw(raw)) => {
            return Err(serde_json::from_value(raw)
                .unwrap_or_else(|_| RpcError::new(ErrorCode::ProtocolMismatch, "raw rpc error")));
        }
        Err(other) => return Err(other.into_rpc()),
    };
    if method == methods::SYSTEM_SNAPSHOT {
        if let Some(rev) = result.get("revision").and_then(|v| v.as_u64()) {
            state
                .revision
                .fetch_max(rev, std::sync::atomic::Ordering::SeqCst);
        }
    }
    Ok(result)
}

/// Route `session.output` frames of one session to a frontend channel for
/// `view_id`. Re-subscribing a view replaces its channel (never appends).
/// Channel delivery is NOT an ACK (01 §3) — ACKs come back via `bridge_ack`.
#[tauri::command]
pub async fn bridge_subscribe_session(
    state: State<'_, BridgeState>,
    session_id: String,
    view_id: String,
    on_output: Channel<Value>,
) -> Result<(), RpcError> {
    let inner = state.inner.lock().await;
    if inner.data().is_none() {
        return Err(RpcError::new(
            ErrorCode::InvalidState,
            "data connection is not open",
        ));
    }
    inner
        .subscribe_session(&session_id, &view_id, on_output)
        .map_err(BridgeError::into_rpc)
}

/// Release the output channel of one view (pane closed/retried/disposed).
/// Always succeeds — unknown ids are a no-op.
#[tauri::command]
pub async fn bridge_unsubscribe_session(
    state: State<'_, BridgeState>,
    session_id: String,
    view_id: String,
) -> Result<(), RpcError> {
    let inner = state.inner.lock().await;
    inner.unsubscribe_session(&session_id, &view_id);
    Ok(())
}

/// Subscribe to control-connection events (workload.changed etc.).
#[tauri::command]
pub async fn bridge_subscribe_events(
    state: State<'_, BridgeState>,
    on_event: Channel<Value>,
) -> Result<(), RpcError> {
    let inner = state.inner.lock().await;
    inner.push_event_channel(on_event);
    Ok(())
}

/// Send `session.ack` on the data connection (정상 처리 시 별도 응답 없음).
#[tauri::command]
pub async fn bridge_ack(
    state: State<'_, BridgeState>,
    session_id: String,
    epoch: String,
    through_seq: String,
) -> Result<(), RpcError> {
    let session = SessionId::parse(&session_id)
        .map_err(|_| invalid_argument("session_id must be a UUID v4"))?;
    let through = U64String::parse(&through_seq)
        .map_err(|_| invalid_argument("through_seq must be a decimal U64String"))?;
    validate_epoch(&epoch)?;
    let ack = SessionAck {
        session_id: session,
        epoch,
        through_seq: through,
    };
    let conn = {
        let inner = state.inner.lock().await;
        inner.data().ok_or_else(|| {
            RpcError::new(ErrorCode::DaemonUnavailable, "data connection is not open")
        })?
    };
    let request = RpcRequest::new(
        Uuid::new_v4().to_string(),
        methods::SESSION_ACK,
        serde_json::to_value(&ack).expect("session ack serializes"),
    );
    conn.send_only(request).await.map_err(BridgeError::into_rpc)
}

/// Close both connections. The daemon itself stays alive (02 §1-5).
#[tauri::command]
pub async fn bridge_disconnect(state: State<'_, BridgeState>) -> Result<(), RpcError> {
    let mut inner = state.inner.lock().await;
    inner.shutdown();
    Ok(())
}

/// Cheap revision getter for UI staleness checks (01 §4: UI discards older
/// snapshots). Lock-free — never blocks connection work.
#[tauri::command]
pub fn bridge_revision(state: State<'_, BridgeState>) -> u64 {
    state.revision.load(std::sync::atomic::Ordering::SeqCst)
}

/// Read-only liveness probe used by the frontend watchdog. This distinguishes
/// a silently closed data stream from a healthy control RPC connection.
#[tauri::command]
pub async fn bridge_connection_status(state: State<'_, BridgeState>) -> Result<Value, RpcError> {
    let inner = state.inner.lock().await;
    let (control_alive, data_alive) = inner.connection_status();
    Ok(serde_json::json!({
        "control_alive": control_alive,
        "data_alive": data_alive,
        // Build/version handshake: the connected daemon predates this app
        // build. Drives the non-blocking "outdated — restart" banner.
        "daemon_outdated": inner.daemon_outdated(),
    }))
}

/// Scan PATH and common install dirs for the supported CLIs (04 §5).
#[tauri::command]
pub async fn system_list_clis() -> Result<Vec<CliCandidate>, RpcError> {
    // PATH and every node-version dir are walked synchronously: keep the
    // scan off the async workers.
    tokio::task::spawn_blocking(system::scan_clis)
        .await
        .map_err(|e| RpcError::new(ErrorCode::InvalidState, format!("cli scan failed: {e}")))
}

/// Locate a bare program name (e.g. `node`) on PATH — the interpreter for
/// an npm shim's `cli.js` (04 §5). Paths are refused so this never probes
/// arbitrary locations; unknown → `null`, never a guess.
#[tauri::command]
pub async fn system_locate_program(name: String) -> Result<Option<String>, RpcError> {
    if !system::is_bare_program_name(&name) {
        return Err(invalid_argument("program name must be a bare file name"));
    }
    tokio::task::spawn_blocking(move || system::locate_program(&name))
        .await
        .map_err(|e| {
            RpcError::new(
                ErrorCode::InvalidState,
                format!("program lookup failed: {e}"),
            )
        })
}

/// Detect shells for the profile picker (PowerShell/pwsh/CMD/WSL distros).
#[tauri::command]
pub async fn system_list_shells() -> Result<Vec<system::DetectedShell>, RpcError> {
    // `wsl -l -q` can take seconds (cold WSL service) — blocking pool, and
    // the probe itself kills the child after its 2 s deadline.
    tokio::task::spawn_blocking(system::detect_shells)
        .await
        .map_err(|e| {
            RpcError::new(
                ErrorCode::InvalidState,
                format!("shell detection failed: {e}"),
            )
        })
}

/// Resolve the Git branch for a terminal's current working directory.
#[tauri::command]
pub async fn system_git_branch(cwd: String) -> Result<Option<system::GitBranch>, RpcError> {
    system::git_branch(&cwd)
        .await
        .map_err(BridgeError::into_rpc)
}

/// Verified version query: fixed argv, 2 s timeout, 8 KiB output cap.
#[tauri::command]
pub async fn system_query_version(
    program: String,
    arg: String,
) -> Result<Option<String>, RpcError> {
    system::query_version(&program, &arg)
        .await
        .map_err(BridgeError::into_rpc)
}

/// Read subscription allowance from each provider. A provider failure is
/// represented in that provider's row instead of failing the whole panel.
/// Two data roots, each matching its writer: the Claude fallback cache is
/// written by the `claude-usage` hook, whose command line carries no
/// `--data-dir` (platform default, the same root `claude_usage_call` keeps
/// the status-line backup under), while the Z.ai key store is read back by
/// the daemon under the root it was spawned with (`effective_data_dir`, the
/// same resolution the `zai_api_key_*` commands write through).
#[tauri::command]
pub async fn subscription_usage_refresh(
    state: State<'_, BridgeState>,
) -> Result<Vec<crate::bridge::subscriptions::ProviderUsage>, RpcError> {
    let hook_data_dir =
        crate::bridge::daemon_manager::default_data_dir().map_err(BridgeError::into_rpc)?;
    let secrets_data_dir = state.effective_data_dir().map_err(BridgeError::into_rpc)?;
    Ok(crate::bridge::subscriptions::refresh(&hook_data_dir, &secrets_data_dir).await)
}

/// Data root of the Z.ai key store: the dir the daemon was started with, so
/// the key saved from Settings is the one `claude_provider` routing resolves
/// at launch. A resolution failure surfaces as the generic
/// `credential_store_error` code — never a path or OS detail.
fn zai_secret_data_dir(state: &BridgeState) -> Result<std::path::PathBuf, RpcError> {
    state
        .effective_data_dir()
        .map_err(|_| invalid_argument("credential_store_error"))
}

#[tauri::command]
pub async fn zai_api_key_status(
    state: State<'_, BridgeState>,
) -> Result<crate::bridge::subscriptions::KeyStatus, RpcError> {
    let data_dir = zai_secret_data_dir(&state)?;
    tokio::task::spawn_blocking(move || crate::bridge::subscriptions::key_status(&data_dir))
        .await
        .map_err(|_| invalid_argument("credential_store_error"))?
        .map_err(invalid_argument)
}

#[tauri::command]
pub async fn zai_api_key_set(
    state: State<'_, BridgeState>,
    api_key: String,
) -> Result<crate::bridge::subscriptions::KeyStatus, RpcError> {
    let data_dir = zai_secret_data_dir(&state)?;
    tokio::task::spawn_blocking(move || {
        crate::bridge::subscriptions::set_zai_key(&data_dir, &api_key)
    })
    .await
    .map_err(|_| invalid_argument("credential_store_error"))?
    .map_err(invalid_argument)?;
    // The atomic encrypted-file write is authoritative; no OS keychain call
    // or additional read is needed to acknowledge a completed save.
    Ok(crate::bridge::subscriptions::KeyStatus { configured: true })
}

#[tauri::command]
pub async fn zai_api_key_remove(
    state: State<'_, BridgeState>,
) -> Result<crate::bridge::subscriptions::KeyStatus, RpcError> {
    let data_dir = zai_secret_data_dir(&state)?;
    tokio::task::spawn_blocking(move || crate::bridge::subscriptions::remove_zai_key(&data_dir))
        .await
        .map_err(|_| invalid_argument("credential_store_error"))?
        .map_err(invalid_argument)?;
    Ok(crate::bridge::subscriptions::KeyStatus { configured: false })
}

#[tauri::command]
pub fn claude_usage_status() -> Result<crate::bridge::claude_usage::Status, RpcError> {
    claude_usage_call(|path, data_dir, command| {
        let _ = data_dir;
        crate::bridge::claude_usage::status(path, command)
    })
}

#[tauri::command]
pub fn claude_usage_apply() -> Result<crate::bridge::claude_usage::Status, RpcError> {
    claude_usage_call(crate::bridge::claude_usage::apply)
}

#[tauri::command]
pub fn claude_usage_remove() -> Result<crate::bridge::claude_usage::Status, RpcError> {
    claude_usage_call(crate::bridge::claude_usage::remove)
}

fn claude_usage_call(
    op: fn(
        &std::path::Path,
        &std::path::Path,
        &str,
    ) -> Result<crate::bridge::claude_usage::Status, crate::bridge::claude_usage::Error>,
) -> Result<crate::bridge::claude_usage::Status, RpcError> {
    let settings = crate::bridge::claude_hooks::claude_settings_path()
        .map_err(|error| hooks_rpc_error(&error))?;
    // 일부러 default_data_dir(): settings.json에 적히는 `claude-usage` 훅 명령은
    // `--data-dir`를 싣지 않으므로 데몬 바이너리는 원본 statusLine 백업을
    // 플랫폼 기본 루트(`config/claude-statusline-original.json`)에서 찾는다.
    // 백업도 같은 곳에 써야 복원이 맞물린다 — `effective_data_dir`가 아니다.
    let data_dir =
        crate::bridge::daemon_manager::default_data_dir().map_err(BridgeError::into_rpc)?;
    // hook 명령과 같은 해석(인용 + 상대경로 절대화 + PATH 폴백)을 쓴다.
    let binary = hook_binary_token(crate::bridge::daemon_manager::hook_daemon_binary());
    let command = format!("{binary} claude-usage");
    op(&settings, &data_dir, &command).map_err(|error| {
        RpcError::new(
            ErrorCode::InvalidState,
            format!("claude usage integration: {error}"),
        )
    })
}

fn shell_quote(value: &str) -> String {
    #[cfg(unix)]
    {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
    #[cfg(windows)]
    {
        format!("\"{}\"", value.replace('"', ""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_allowlist_excludes_hello_and_session_ack() {
        assert!(!is_allowed_method("hello"));
        assert!(!is_allowed_method("session.ack"));
        assert!(is_allowed_method("system.snapshot"));
        assert!(is_allowed_method("workload.launch"));
        assert!(is_allowed_method("session.attach"));
        assert!(is_allowed_method("session.input"));
        assert!(is_allowed_method("session.resize"));
        assert!(is_allowed_method("session.take_control"));
        // 창이 보고 있는 세션 보고(08 §1): 없으면 압력 완화가 보이는 pane을
        // 건드리게 된다 — mock 경로는 이 구멍을 가려 준다.
        assert!(is_allowed_method(methods::SESSION_FOCUS));
        assert!(is_allowed_method("session.focus"));
        // 압력 완화 수동 조작(08 §2): 없으면 UI의 양보/복원/보호 버튼이
        // 브리지에서 조용히 막힌다.
        assert!(is_allowed_method(methods::SESSION_RELIEF));
        assert!(is_allowed_method("session.relief"));
        assert!(is_allowed_method(methods::RELIEF_SET_POLICY));
        assert!(is_allowed_method(methods::WORKLOAD_SUSPEND));
        assert!(is_allowed_method(methods::WORKLOAD_RESUME));
        assert!(is_allowed_method(methods::GUARD_SET_POLICY));
        assert!(is_allowed_method("relief.set_policy"));
        assert!(is_allowed_method("retention.set_limit"));
        // Both are called by the real client (notification restore, journal
        // search); the mock path hides a missing allowlist entry.
        assert!(is_allowed_method("session.search"));
        assert!(is_allowed_method("intervention.list"));
        // Agent session capture/resume: the UI lists and forgets captures, so
        // both must pass the bridge. Without them every `bridge_rpc` call for
        // the resume picker failed and the feature was silently dead.
        assert!(is_allowed_method(methods::AGENT_SESSION_LIST));
        assert!(is_allowed_method(methods::AGENT_SESSION_FORGET));
        assert!(is_allowed_method("agent_session.list"));
        assert!(is_allowed_method("agent_session.forget"));
        // Reports come from the hook CLI's own connection, never the UI.
        assert!(!is_allowed_method(methods::AGENT_SESSION_REPORT));
        // O1 mission surface (O04): the UI drives missions through the same
        // control connection — a missing entry silently kills the whole
        // feature exactly like the agent-session gap above.
        for method in [
            mission_methods::MISSION_CREATE,
            mission_methods::MISSION_LIST,
            mission_methods::MISSION_SNAPSHOT,
            mission_methods::MISSION_EVENTS,
            mission_methods::MISSION_CONTROL,
            mission_methods::MISSION_ACCEPT,
            mission_methods::MISSION_MESSAGE,
            mission_methods::MISSION_PLAN_APPLY,
            mission_methods::MISSION_TASK_CONTROL,
            mission_methods::MISSION_POLICY_UPDATE,
            mission_methods::MISSION_FINDING_RESOLVE,
            mission_methods::MISSION_DECISION_ANSWER,
            mission_methods::MISSION_REQUEST_GET,
            mission_methods::MISSION_ACTIVITY,
            mission_methods::BINDING_LIST,
            mission_methods::BINDING_SAVE,
            mission_methods::BINDING_PROBE,
            mission_methods::RUNTIME_DETECT,
            mission_methods::REPOSITORY_INSPECT,
            mission_methods::TEMPLATE_LIST,
            mission_methods::TEMPLATE_SAVE,
            mission_methods::VERIFICATION_LIST,
            mission_methods::VERIFICATION_SAVE,
            mission_methods::ARTIFACT_BEGIN,
            mission_methods::ARTIFACT_WRITE,
            mission_methods::ARTIFACT_COMMIT,
            mission_methods::ARTIFACT_READ,
            mission_methods::MISSION_RUN_ATTEST_EXITED,
            mission_methods::WORKSPACE_USAGE,
            mission_methods::WORKSPACE_CLEANUP,
        ] {
            assert!(
                is_allowed_method(method),
                "mission method blocked: {method}"
            );
        }
        // Near-misses still land outside the allowlist.
        assert!(!is_allowed_method("mission.create "));
        assert!(!is_allowed_method("artifact.begin/extra"));
        assert!(!is_allowed_method("arbitrary.method"));
        assert!(!is_allowed_method(""));
    }

    /// 설치본은 공백이 든 경로에 있다 — 인용하지 않으면 셸이 명령을
    /// `/Applications/IYAGI`로 읽어 훅이 아예 실행되지 않는다.
    #[test]
    fn hook_command_quotes_a_path_with_spaces_and_keeps_the_agent_suffix() {
        let installed = if cfg!(windows) {
            "C:\\Program Files\\IYAGI\\iyagi-termd.exe"
        } else {
            "/Applications/IYAGI.app/Contents/MacOS/iyagi-termd"
        };
        let binary = hook_binary_token(Some(std::path::PathBuf::from(installed)));
        #[cfg(unix)]
        assert_eq!(
            binary,
            "'/Applications/IYAGI.app/Contents/MacOS/iyagi-termd'"
        );
        #[cfg(windows)]
        assert_eq!(binary, "\"C:\\Program Files\\IYAGI\\iyagi-termd.exe\"");

        let claude = hook_command_from(&binary, None);
        let codex = hook_command_from(&binary, Some("codex"));
        assert_eq!(claude, format!("{binary} hook"));
        assert_eq!(codex, format!("{binary} hook --agent codex"));

        // 그리고 그 명령을 hooks_json 판정자가 되돌려 읽을 수 있어야 한다
        // (등록/감지/제거가 같은 문자열 위에서 맞물리는 지점).
        use crate::bridge::hooks_json::is_managed_command;
        assert!(is_managed_command(&claude, None));
        assert!(!is_managed_command(&claude, Some("codex")));
        assert!(is_managed_command(&codex, Some("codex")));
        assert!(!is_managed_command(&codex, None));
    }

    /// 절대화할 수 없는 상대경로(데몬을 못 찾은 dev 후보)는 PATH 폴백.
    #[test]
    fn hook_command_falls_back_to_path_form_for_unresolvable_paths() {
        let missing = std::path::PathBuf::from("target/debug/iyagi-termd-does-not-exist");
        assert_eq!(
            hook_binary_token(Some(missing)),
            crate::bridge::daemon_manager::daemon_binary_name()
        );
        assert_eq!(
            hook_binary_token(None),
            crate::bridge::daemon_manager::daemon_binary_name()
        );
        assert_eq!(hook_command_from("iyagi-termd", None), "iyagi-termd hook");
    }

    /// 절대경로는 심볼릭 링크를 따라가지 않고 그대로(인용만) 쓴다 —
    /// `.app` 번들 경로가 canonicalize로 바뀌면 사용자가 보는 경로와
    /// 등록된 경로가 달라진다. 상대경로만 절대화 대상이다.
    #[test]
    fn hook_command_keeps_an_absolute_path_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("iyagi-termd");
        std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
        let token = hook_binary_token(Some(binary.clone()));
        assert!(
            token.contains(binary.to_string_lossy().as_ref()),
            "absolute candidate must survive verbatim: {token}"
        );
        let command = hook_command_from(&token, None);
        use crate::bridge::hooks_json::is_managed_command;
        assert!(is_managed_command(&command, None));
    }

    #[test]
    fn rpc_id_validation_rejects_bad_shapes() {
        assert!(validate_rpc_id("0a1b2c3d-uuid").is_ok());
        assert!(validate_rpc_id("").is_err());
        assert!(validate_rpc_id(&"x".repeat(129)).is_err());
        assert!(validate_rpc_id("has space").is_err());
        assert!(validate_rpc_id("has\nnewline").is_err());
        assert!(validate_rpc_id("라인").is_err());
    }

    #[test]
    fn epoch_validation_rejects_bad_shapes() {
        assert!(validate_epoch("epoch-1").is_ok());
        assert!(validate_epoch("").is_err());
        assert!(validate_epoch(&"e".repeat(129)).is_err());
        assert!(validate_epoch("tab\tepoch").is_err());
    }
}

/// Claude Code hooks 연동 상태(W1-5): 현재 내용 + 적용 제안. 파일은
/// 건드리지 않는다 — 동의(diff 확인)는 프론트에서 이뤄진다.
#[tauri::command]
pub fn claude_hooks_status() -> Result<serde_json::Value, RpcError> {
    claude_hooks_call(crate::bridge::claude_hooks::status)
}

/// 동의 후 적용(백업 + atomic 쓰기, 중복 주입 금지).
#[tauri::command]
pub fn claude_hooks_apply() -> Result<serde_json::Value, RpcError> {
    claude_hooks_call(crate::bridge::claude_hooks::apply)
}

/// 우리가 넣은 훅만 제거(사용자 hook은 불가침).
#[tauri::command]
pub fn claude_hooks_remove() -> Result<serde_json::Value, RpcError> {
    claude_hooks_call(crate::bridge::claude_hooks::remove)
}

/// Codex hooks 연동 상태(SessionStart/SessionEnd): 현재 내용 + 적용 제안.
/// 파일은 건드리지 않는다 — 동의(diff 확인)는 프론트에서 이뤄진다.
#[tauri::command]
pub fn codex_hooks_status() -> Result<serde_json::Value, RpcError> {
    codex_hooks_call(crate::bridge::codex_hooks::status)
}

/// 동의 후 적용(백업 + atomic 쓰기, 이미 등록된 이벤트는 건드리지 않음).
#[tauri::command]
pub fn codex_hooks_apply() -> Result<serde_json::Value, RpcError> {
    codex_hooks_call(crate::bridge::codex_hooks::apply)
}

/// 우리가 넣은 훅만 제거(사용자 hook은 불가침).
#[tauri::command]
pub fn codex_hooks_remove() -> Result<serde_json::Value, RpcError> {
    codex_hooks_call(crate::bridge::codex_hooks::remove)
}

/// hook이 부를 명령: 데몬 바이너리를 절대경로로(없으면 PATH 폴백).
/// `agent`가 있으면 등록 주체를 구분하는 `--agent <name>`을 덧붙인다
/// (Codex 등 — 데몬이 hook 소스를 안다, `iyagi-termd hook --agent codex`).
fn hook_command(agent: Option<&str>) -> String {
    // Linux AppImage: the stable per-user copy, never the transient mount
    // (daemon_manager::hook_daemon_binary).
    let binary = hook_binary_token(crate::bridge::daemon_manager::hook_daemon_binary());
    hook_command_from(&binary, agent)
}

/// 명령 조립(순수): 이미 셸용으로 인용된 바이너리 토큰 + 서브커맨드.
fn hook_command_from(binary: &str, agent: Option<&str>) -> String {
    match agent {
        Some(name) => format!("{binary} hook --agent {name}"),
        None => format!("{binary} hook"),
    }
}

/// 데몬 바이너리를 hook 명령에 박을 셸 토큰으로 만든다.
///
/// Claude/Codex는 hook `command`를 **셸을 거쳐** 실행한다. 설치본 경로에는
/// 공백이 있으므로(`/Applications/IYAGI.app/Contents/MacOS/iyagi-termd`)
/// 반드시 인용해야 한다 — 인용이 없으면 설치 빌드에서 훅이 아예 실행되지
/// 않았다. 또 `daemon_manager`의 후보에는 상대경로(`target/debug/iyagi-termd`)
/// 가 있는데, hook은 CLI의 작업 디렉터리에서 돌기 때문에 상대경로는 의미가
/// 없다 — `canonicalize`로 절대화하고, 그마저 실패하면 PATH에 의존하는
/// 맨몸 `iyagi-termd`로 폴백한다.
fn hook_binary_token(located: Option<std::path::PathBuf>) -> String {
    match absolute_daemon_binary(located) {
        Some(path) => shell_quote(&path.to_string_lossy()),
        None => crate::bridge::daemon_manager::daemon_binary_name().to_string(),
    }
}

/// 위 인용 규칙의 앞단(절대경로 해석)만 떼어 낸 것. 셸 토큰이 아니라 **날
/// 경로**가 필요한 곳(`shell_profiles`의 zsh 스크립트는 경로를 겹따옴표 안에
/// 직접 박고, 못 찾으면 PATH 폴백 대신 설치 자체를 막는다)이 같은 해석을
/// 공유한다.
fn absolute_daemon_binary(located: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    let path = located?;
    if path.is_absolute() {
        Some(path)
    } else {
        std::fs::canonicalize(&path).ok()
    }
}

/// 세 명령의 공통 형태: 경로·명령 해결 → 순수 로직 → 직렬화.
fn claude_hooks_call(
    op: fn(
        &std::path::Path,
        &str,
    ) -> Result<
        crate::bridge::claude_hooks::HooksStatus,
        crate::bridge::claude_hooks::HooksError,
    >,
) -> Result<serde_json::Value, RpcError> {
    let path =
        crate::bridge::claude_hooks::claude_settings_path().map_err(|e| hooks_rpc_error(&e))?;
    let command = hook_command(None);
    let status = op(&path, &command).map_err(|e| hooks_rpc_error(&e))?;
    serde_json::to_value(status)
        .map_err(|e| RpcError::new(ErrorCode::InvalidState, format!("serialize: {e}")))
}

/// Claude와 같은 형태(공용 `hooks_json` 위에 얹힌 Codex 어댑터) — 경로
/// 해석과 `--agent codex` 명령만 다르다.
fn codex_hooks_call(
    op: fn(
        &std::path::Path,
        &str,
    ) -> Result<
        crate::bridge::codex_hooks::HooksStatus,
        crate::bridge::codex_hooks::HooksError,
    >,
) -> Result<serde_json::Value, RpcError> {
    let path = crate::bridge::codex_hooks::codex_hooks_path().map_err(|e| hooks_rpc_error(&e))?;
    let command = hook_command(Some("codex"));
    let status = op(&path, &command).map_err(|e| hooks_rpc_error(&e))?;
    serde_json::to_value(status)
        .map_err(|e| RpcError::new(ErrorCode::InvalidState, format!("serialize: {e}")))
}

/// Claude·Codex 공용 오류 매핑 — 두 모듈의 `HooksError`는 `hooks_json`의
/// 재수출이라 실제로는 같은 타입이다.
fn hooks_rpc_error(e: &crate::bridge::hooks_json::HooksError) -> RpcError {
    use crate::bridge::hooks_json::HooksError as E;
    let code = match e {
        E::NoHome => ErrorCode::InvalidArgument,
        E::Read(_) | E::InvalidJson(_) => ErrorCode::InvalidArgument,
        E::Write(_) => ErrorCode::InvalidState,
    };
    RpcError::new(code, format!("{}: {e}", e.code()))
}

/// zsh 런치 프로필(`ccd`/`ccg`) 상태: 설치 여부 + 적용될 블록·스크립트
/// 미리보기 + 이미 존재하는 정의(충돌). 파일은 건드리지 않는다 — 동의는
/// 프론트에서 미리보기를 보고 이뤄진다(hooks 연동과 같은 계약).
///
/// `main_model`은 미리보기용이라 모르는 값이면 조용히 기본 모델로 되돌린다
/// (`apply`는 같은 값을 거절한다 — 데몬이 받지 않을 모델을 rc에 박지 않는다).
#[tauri::command]
pub async fn shell_profiles_status(
    state: State<'_, BridgeState>,
    main_model: Option<String>,
) -> Result<shell_profiles::ShellProfilesStatus, RpcError> {
    let data_dir = state.effective_data_dir().map_err(BridgeError::into_rpc)?;
    let binary = shell_profiles_binary();
    let model = shell_profiles::sanitize_main_model(main_model);
    // rc 스캔과 (설치 전) `zsh -ic` 탐지는 최대 3초까지 막힌다 — 워커 스레드로.
    tokio::task::spawn_blocking(move || {
        let home = shell_profiles::home_dir();
        shell_profiles::status(home.as_deref(), &data_dir, binary.as_deref(), &model)
    })
    .await
    .map_err(|_| shell_profiles_task_error())
}

/// 동의 후 적용: 스크립트를 쓰고 `~/.zshrc`에 마커 블록을 넣는다(백업 +
/// atomic 쓰기). 이미 `ccd`/`ccg`가 있으면 거절한다 — 단 `replace_existing`
/// (사용자가 "교체"를 고름)이면 그 줄은 남긴 채 블록을 rc 끝으로 옮겨 가린다.
#[tauri::command]
pub async fn shell_profiles_apply(
    state: State<'_, BridgeState>,
    main_model: String,
    replace_existing: Option<bool>,
) -> Result<shell_profiles::ShellProfilesStatus, RpcError> {
    let replace_existing = replace_existing.unwrap_or(false);
    let data_dir = state.effective_data_dir().map_err(BridgeError::into_rpc)?;
    let binary = shell_profiles_binary();
    tokio::task::spawn_blocking(move || {
        let home = shell_profiles::home_dir().ok_or(ShellProfilesError::NoHome)?;
        shell_profiles::apply(
            &home,
            &data_dir,
            binary.as_deref(),
            &main_model,
            replace_existing,
        )
    })
    .await
    .map_err(|_| shell_profiles_task_error())?
    .map_err(|error| shell_profiles_rpc_error(&error))
}

/// 우리 블록과 스크립트만 제거(사용자가 직접 쓴 정의는 불가침).
///
/// `main_model`은 반환되는 상태(`ShellProfilesStatus.main_model`)에만
/// 쓰인다 — 제거 자체는 모델과 무관하지만, 이 값이 없으면 응답이 항상
/// `DEFAULT_MAIN_MODEL`로 되돌아가 사용자가 고른 모델과 어긋난 상태를
/// 돌려주게 된다. `shell_profiles_status`와 같은 sanitize 규칙을 쓴다.
#[tauri::command]
pub async fn shell_profiles_remove(
    state: State<'_, BridgeState>,
    main_model: Option<String>,
) -> Result<shell_profiles::ShellProfilesStatus, RpcError> {
    let data_dir = state.effective_data_dir().map_err(BridgeError::into_rpc)?;
    let binary = shell_profiles_binary();
    let model = shell_profiles::sanitize_main_model(main_model);
    tokio::task::spawn_blocking(move || {
        let home = shell_profiles::home_dir().ok_or(ShellProfilesError::NoHome)?;
        shell_profiles::remove(&home, &data_dir, binary.as_deref(), &model)
    })
    .await
    .map_err(|_| shell_profiles_task_error())?
    .map_err(|error| shell_profiles_rpc_error(&error))
}

/// 스크립트에 박을 데몬 경로. hook 명령과 **같은** 해석을 쓴다: 무조건
/// 사용자별 안정 복사본을 만들어 그 경로를 박는다(`hook_daemon_binary`) —
/// 개발 빌드의 repo 경로는 이사하는 순간 죽고, AppImage의 마운트는 실행마다
/// 바뀐다. 앱 기동 때 사본이 갱신되므로 repo를 옮겨도 이 스크립트는 살아
/// 있다. 상대경로는 절대화(`absolute_daemon_binary`)한다. 이 스크립트는
/// `~/.zshrc`가 셸마다 source하므로, 실행마다 바뀌는 경로를 박으면 다음
/// 로그인부터 `ccd`/`ccg`가 죽는다.
fn shell_profiles_binary() -> Option<std::path::PathBuf> {
    absolute_daemon_binary(crate::bridge::daemon_manager::hook_daemon_binary())
}

/// 워커 스레드가 죽은 경우(패닉·런타임 종료). 경로나 OS 세부는 싣지 않는다.
fn shell_profiles_task_error() -> RpcError {
    RpcError::new(ErrorCode::InvalidState, "shell_profiles: task failed")
}

/// `shell_profiles` 오류 → RPC 오류. 충돌은 목록을 `details`에 그대로 실어
/// UI가 "어느 파일 몇 번째 줄"을 보여 줄 수 있게 한다(경로 외 비밀은 없다).
fn shell_profiles_rpc_error(error: &ShellProfilesError) -> RpcError {
    use crate::bridge::shell_profiles::ShellProfilesError as E;
    let code = match error {
        E::NoHome | E::InvalidModel => ErrorCode::InvalidArgument,
        E::Unsupported(_) | E::Read(_) | E::Write(_) | E::Conflict(_) | E::Unreplaceable(_) => {
            ErrorCode::InvalidState
        }
    };
    let rpc = RpcError::new(code, format!("{}: {error}", error.code()));
    match error {
        E::Conflict(items) | E::Unreplaceable(items) => {
            let details = serde_json::json!({ "code": error.code(), "conflicts": items });
            rpc.with_details(details)
        }
        E::Unsupported(reason) => {
            let details = serde_json::json!({ "code": error.code(), "reason": reason });
            rpc.with_details(details)
        }
        _ => rpc,
    }
}
