//! Subscription allowance readers for Codex, Claude Code and Z.ai Coding Plan.
//! Results are normalized and never include account identities or credentials.

use std::path::Path;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderValue, AUTHORIZATION};
use serde::Serialize;
use serde_json::{json, Value};
use term_secrets::LocalSecretStore;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const ZAI_QUOTA_URL: &str = "https://api.z.ai/api/monitor/usage/quota/limit";
const BODY_CAP: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsage {
    pub provider: &'static str,
    pub display_name: &'static str,
    pub status: &'static str,
    pub reason_code: Option<&'static str>,
    pub detail: Option<String>,
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub observed_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub id: String,
    pub label: String,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub window_duration_minutes: Option<u64>,
    pub resets_at: Option<u64>,
    pub used: Option<f64>,
    pub limit: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub configured: bool,
}

impl ProviderUsage {
    fn unavailable(provider: &'static str, name: &'static str, reason: &'static str) -> Self {
        Self {
            provider,
            display_name: name,
            status: "unavailable",
            reason_code: Some(reason),
            detail: None,
            plan: None,
            windows: Vec::new(),
            observed_at: None,
        }
    }

    fn error(provider: &'static str, name: &'static str, reason: &'static str) -> Self {
        let mut out = Self::unavailable(provider, name, reason);
        out.status = "error";
        out
    }
}

/// `refresh` runs every 60 s for the status bar. The CLI location scan
/// walks PATH and every node-version dir synchronously, so it is done on a
/// blocking thread and remembered for a while — install locations change on
/// the order of weeks, not minutes.
const CODEX_PATH_TTL: Duration = Duration::from_secs(10 * 60);
static CODEX_PATH: Mutex<Option<(Instant, Option<String>)>> = Mutex::new(None);
static CLAUDE_PATH: Mutex<Option<(Instant, Option<String>)>> = Mutex::new(None);

async fn agent_program(
    kind: &'static str,
    cache: &Mutex<Option<(Instant, Option<String>)>>,
) -> Option<String> {
    if let Some((at, path)) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
    {
        if at.elapsed() < CODEX_PATH_TTL {
            return path.clone();
        }
    }
    // Windows npm installs list `codex` (POSIX shim), `codex.cmd` and
    // `codex.exe`-less layouts side by side: take the form the bridge can
    // actually start, never the bare shim.
    let scanned = tokio::task::spawn_blocking(move || {
        super::system::spawnable_program(&super::system::scan_clis(), kind)
    })
    .await
    .unwrap_or(None);
    *cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((Instant::now(), scanned.clone()));
    scanned
}

/// Two data roots, each following its writer:
/// - `hook_data_dir`: where the `iyagi-termd claude-usage` status-line hook
///   writes the Claude fallback cache (`data/subscription-claude.json`). The
///   hook command line carries no `--data-dir`, so the daemon binary resolves
///   the platform default on its own (`daemon_manager::default_data_dir`),
///   whatever root the daemon process was spawned with.
/// - `secrets_data_dir`: the daemon's data root
///   (`BridgeState::effective_data_dir`). The Z.ai key store lives under it
///   because `iyagi-termd` opens `<root>/secrets` when it resolves
///   `claude_provider` at launch.
///
/// The two only differ while a `--data-dir` override is in play; the
/// frontend never passes one today, but the split keeps each reader pinned
/// to the dir its writer uses.
pub async fn refresh(hook_data_dir: &Path, secrets_data_dir: &Path) -> Vec<ProviderUsage> {
    let codex_path = agent_program("codex", &CODEX_PATH).await;
    let (codex, claude, zai) = tokio::join!(
        read_codex(codex_path),
        read_claude(hook_data_dir),
        read_zai(secrets_data_dir)
    );
    vec![codex, claude, zai]
}

async fn read_codex(program: Option<String>) -> ProviderUsage {
    let Some(program) = program else {
        return ProviderUsage::unavailable("codex", "Codex", "cli_not_found");
    };
    match tokio::time::timeout(Duration::from_secs(8), codex_transaction(&program)).await {
        Ok(Ok(usage)) => usage,
        Ok(Err(reason)) => ProviderUsage::error("codex", "Codex", reason),
        Err(_) => ProviderUsage::error("codex", "Codex", "timeout"),
    }
}

async fn codex_transaction(program: &str) -> Result<ProviderUsage, &'static str> {
    let mut command = tokio::process::Command::new(program);
    command
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // stdio is app-server's default transport; there is no --stdio flag.
    // A Dock-launched app can discover an nvm-installed Codex while its
    // inherited PATH cannot resolve the launcher's /usr/bin/env node.
    // Keep the selected installation's bin directory available to the child.
    if let Some(bin) = Path::new(program)
        .parent()
        .filter(|path| path.is_absolute())
    {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let paths = std::iter::once(bin.to_path_buf()).chain(std::env::split_paths(&inherited));
        if let Ok(path) = std::env::join_paths(paths) {
            command.env("PATH", path);
        }
    }
    // Runs every 60 s from a GUI-subsystem app: without CREATE_NO_WINDOW a
    // console would flash on each refresh (Windows).
    super::system::hide_console(&mut command);
    let mut child = command.spawn().map_err(|_| "cli_start_failed")?;
    let mut stdin = child.stdin.take().ok_or("cli_start_failed")?;
    let stdout = child.stdout.take().ok_or("cli_start_failed")?;
    let mut reader = BufReader::new(stdout);

    write_rpc(&mut stdin, json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{"clientInfo":{"name":"iyagi","title":"iyagi","version":env!("CARGO_PKG_VERSION")},"capabilities":null}
    })).await?;
    let _ = read_rpc(&mut reader, 1).await?;
    write_rpc(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    )
    .await?;
    write_rpc(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"account/read","params":{"refreshToken":false}}),
    )
    .await?;
    let account = read_rpc(&mut reader, 2).await?;
    let account_value = account.pointer("/result/account");
    let account_type = account_value
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str);
    if account_value.is_none()
        || account_value == Some(&Value::Null)
        || account_type == Some("apiKey")
    {
        let _ = child.kill().await;
        return Err("subscription_login_required");
    }
    let account_plan = account_value
        .and_then(|value| value.get("planType"))
        .and_then(Value::as_str);
    write_rpc(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"account/rateLimits/read","params":null}),
    )
    .await?;
    let response = read_rpc(&mut reader, 3).await?;
    let _ = child.kill().await;
    parse_codex(&response, account_plan).ok_or("no_data")
}

async fn write_rpc(
    stdin: &mut tokio::process::ChildStdin,
    value: Value,
) -> Result<(), &'static str> {
    let mut bytes = serde_json::to_vec(&value).map_err(|_| "protocol_error")?;
    bytes.push(b'\n');
    stdin
        .write_all(&bytes)
        .await
        .map_err(|_| "protocol_error")?;
    stdin.flush().await.map_err(|_| "protocol_error")
}

/// Reads one `\n`-terminated line, failing before more than `BODY_CAP`
/// bytes are buffered — `Lines::next_line` would allocate the whole line
/// first, so a misbehaving app-server could grow memory without bound.
async fn read_capped_line(
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> Result<Option<String>, &'static str> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let buf = reader.fill_buf().await.map_err(|_| "protocol_error")?;
        if buf.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = buf.iter().position(|&b| b == b'\n');
        let chunk = &buf[..newline.unwrap_or(buf.len())];
        if line.len() + chunk.len() > BODY_CAP {
            return Err("protocol_error");
        }
        line.extend_from_slice(chunk);
        let used = chunk.len() + usize::from(newline.is_some());
        reader.consume(used);
        if newline.is_some() {
            break;
        }
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| "protocol_error")
}

async fn read_rpc(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    id: u64,
) -> Result<Value, &'static str> {
    for _ in 0..128 {
        let line = read_capped_line(reader).await?.ok_or("protocol_error")?;
        let value: Value = serde_json::from_str(&line).map_err(|_| "protocol_error")?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            if value.get("error").is_some() {
                return Err("provider_error");
            }
            return Ok(value);
        }
    }
    Err("protocol_error")
}

fn parse_codex(response: &Value, account_plan: Option<&str>) -> Option<ProviderUsage> {
    let result = response.get("result")?;
    let plan = result
        .get("planType")
        .or_else(|| result.pointer("/rateLimits/planType"))
        .and_then(Value::as_str)
        .or(account_plan)
        .map(str::to_string);
    let mut snapshots = Vec::new();
    if let Some(map) = result
        .get("rateLimitsByLimitId")
        .and_then(Value::as_object)
        .filter(|map| !map.is_empty())
    {
        snapshots.extend(map.values());
    } else if let Some(snapshot) = result.get("rateLimits") {
        snapshots.push(snapshot);
    }
    let mut windows = Vec::new();
    for (snapshot_index, snapshot) in snapshots.into_iter().enumerate() {
        let limit_name = snapshot.get("limitName").and_then(Value::as_str);
        for (lane, key) in [("primary", "primary"), ("secondary", "secondary")] {
            let Some(window) = snapshot.get(key) else {
                continue;
            };
            let Some(used) = window.get("usedPercent").and_then(Value::as_f64) else {
                continue;
            };
            let duration = window.get("windowDurationMins").and_then(Value::as_u64);
            let base = duration_label(duration);
            let label = match limit_name {
                Some(name) if !name.is_empty() => format!("{name} · {base}"),
                _ => base,
            };
            windows.push(make_window(
                format!("{snapshot_index}-{lane}"),
                label,
                used,
                duration,
                window.get("resetsAt").and_then(Value::as_u64),
                None,
                None,
            ));
        }
    }
    if windows.is_empty() {
        return None;
    }
    Some(ProviderUsage {
        provider: "codex",
        display_name: "Codex",
        status: "ready",
        reason_code: None,
        detail: None,
        plan,
        windows,
        observed_at: Some(unix_now()),
    })
}

// Coalesce concurrent refreshes, including failures, so opening the popover cannot
// repeatedly launch a CLI or hammer the subscription endpoint. Store normalized data only.
static CLAUDE_USAGE: tokio::sync::Mutex<Option<(Instant, Result<ProviderUsage, &'static str>)>> =
    tokio::sync::Mutex::const_new(None);

async fn read_claude(data_dir: &Path) -> ProviderUsage {
    let mut cached = CLAUDE_USAGE.lock().await;
    let result = if let Some((_, result)) = cached
        .as_ref()
        .filter(|(at, _)| at.elapsed() < Duration::from_secs(60))
    {
        result.clone()
    } else {
        let result = match agent_program("claude", &CLAUDE_PATH).await {
            Some(program) => {
                match tokio::time::timeout(Duration::from_secs(8), claude_transaction(&program))
                    .await
                {
                    Ok(result) => result,
                    Err(_) => Err("timeout"),
                }
            }
            None => Err("cli_not_found"),
        };
        *cached = Some((Instant::now(), result.clone()));
        result
    };
    match result {
        Ok(usage) => usage,
        Err(reason) => {
            let fallback = read_claude_cache(data_dir);
            if fallback.status == "ready" {
                fallback
            } else {
                ProviderUsage::unavailable("claude", "Claude", reason)
            }
        }
    }
}

async fn claude_transaction(program: &str) -> Result<ProviderUsage, &'static str> {
    // No user message is sent: this control request reads subscription usage without
    // inference. Disable hooks/project MCPs and persistence in the helper process.
    let mut command = tokio::process::Command::new(program);
    command
        .args([
            "--print",
            "--verbose",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--no-session-persistence",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--settings",
            "{\"disableAllHooks\":true}",
            "--setting-sources",
            "user",
        ])
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    super::system::hide_console(&mut command);
    let mut child = command.spawn().map_err(|_| "cli_start_failed")?;
    let mut stdin = child.stdin.take().ok_or("cli_start_failed")?;
    let mut reader = BufReader::new(child.stdout.take().ok_or("cli_start_failed")?);
    write_rpc(
        &mut stdin,
        json!({
            "type": "control_request", "request_id": "iyagi-usage",
            "request": { "subtype": "get_usage" }
        }),
    )
    .await?;
    for _ in 0..128 {
        let line = read_capped_line(&mut reader)
            .await?
            .ok_or("protocol_error")?;
        let value: Value = serde_json::from_str(&line).map_err(|_| "protocol_error")?;
        if value.get("type").and_then(Value::as_str) != Some("control_response") {
            continue;
        }
        let Some(response) = value.get("response") else {
            continue;
        };
        if response.get("request_id").and_then(Value::as_str) != Some("iyagi-usage") {
            continue;
        }
        let _ = child.kill().await;
        if response.get("subtype").and_then(Value::as_str) != Some("success") {
            return Err("provider_error");
        }
        return response
            .get("response")
            .and_then(parse_claude_usage)
            .ok_or("no_data");
    }
    Err("protocol_error")
}

fn parse_claude_usage(value: &Value) -> Option<ProviderUsage> {
    let limits = value.get("rate_limits")?;
    let mut windows = Vec::new();
    for (id, label, duration) in [("five_hour", "5h", 300), ("seven_day", "7d", 10_080)] {
        if let Some(window) = limits
            .get(id)
            .and_then(|w| claude_window(w, id.into(), label.into(), duration))
        {
            windows.push(window);
        }
    }
    // Use the CLI's named projection; internal bucket names change between releases.
    // utilization is already a percent (e.g. 87), not a fraction.
    if let Some(scoped) = limits.get("model_scoped").and_then(Value::as_array) {
        for entry in scoped {
            let Some(name) = entry
                .get("display_name")
                .and_then(Value::as_str)
                .filter(|name| {
                    !name.trim().is_empty()
                        && name.len() <= 128
                        && !name.chars().any(char::is_control)
                })
            else {
                continue;
            };
            let id = format!("model-scoped:{name}");
            if windows.iter().any(|window| window.id == id) {
                continue;
            }
            if let Some(window) = claude_window(entry, id, format!("{name} · 7d"), 10_080) {
                windows.push(window);
            }
        }
    }
    if windows.is_empty() {
        return None;
    }
    Some(ProviderUsage {
        provider: "claude",
        display_name: "Claude",
        status: "ready",
        reason_code: None,
        detail: None,
        plan: value
            .get("subscription_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        windows,
        observed_at: Some(unix_now()),
    })
}

fn claude_window(value: &Value, id: String, label: String, duration: u64) -> Option<UsageWindow> {
    let used = value
        .get("utilization")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())?;
    let reset = value.get("resets_at").and_then(|v| {
        v.as_u64()
            .map(normalize_epoch)
            .or_else(|| v.as_str().and_then(parse_rfc3339_epoch))
    });
    Some(make_window(
        id,
        label,
        used,
        Some(duration),
        reset,
        None,
        None,
    ))
}

fn read_claude_cache(data_dir: &Path) -> ProviderUsage {
    let path = data_dir.join("data/subscription-claude.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return ProviderUsage::unavailable("claude", "Claude", "integration_required");
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return ProviderUsage::error("claude", "Claude", "cache_invalid");
    };
    let mut windows = Vec::new();
    for (id, label, duration) in [("five_hour", "5h", 300), ("seven_day", "7d", 10_080)] {
        let Some(window) = value.pointer(&format!("/rateLimits/{id}")) else {
            continue;
        };
        let Some(used) = window.get("usedPercentage").and_then(Value::as_f64) else {
            continue;
        };
        let resets_at = window.get("resetsAt").and_then(|value| {
            value
                .as_u64()
                .map(normalize_epoch)
                .or_else(|| value.as_str().and_then(parse_rfc3339_epoch))
        });
        windows.push(make_window(
            id.into(),
            label.into(),
            used,
            Some(duration),
            resets_at,
            None,
            None,
        ));
    }
    if windows.is_empty() {
        return ProviderUsage::unavailable("claude", "Claude", "no_data");
    }
    ProviderUsage {
        provider: "claude",
        display_name: "Claude",
        status: "ready",
        reason_code: None,
        detail: None,
        plan: None,
        windows,
        observed_at: value.get("observedAt").and_then(Value::as_u64),
    }
}

fn parse_rfc3339_epoch(value: &str) -> Option<u64> {
    value.parse().ok().or_else(|| {
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .ok()
            .and_then(|date| u64::try_from(date.unix_timestamp()).ok())
    })
}

/// One process-wide HTTP client. Building a `reqwest::Client` parses the
/// root certificate store and allocates a connection pool; doing that on
/// every 60 s poll fragmented the heap for as long as the app stayed open.
static ZAI_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn zai_client() -> Option<reqwest::Client> {
    if let Some(client) = ZAI_CLIENT.get() {
        return Some(client.clone());
    }
    let built = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;
    Some(ZAI_CLIENT.get_or_init(|| built).clone())
}

async fn read_zai(secrets_data_dir: &Path) -> ProviderUsage {
    let store = secret_store(secrets_data_dir);
    let stored = match tokio::task::spawn_blocking(move || store.read()).await {
        Ok(stored) => stored,
        Err(_) => return ProviderUsage::error("zai", "Z.ai", "credential_store_error"),
    };
    let key = match stored {
        Ok(Some(key)) => key,
        Ok(None) => {
            let mut out = ProviderUsage::unavailable("zai", "Z.ai", "not_configured");
            out.status = "notConfigured";
            return out;
        }
        Err(_) => return ProviderUsage::error("zai", "Z.ai", "credential_store_error"),
    };
    let Ok(mut auth) = HeaderValue::from_str(&key) else {
        return ProviderUsage::error("zai", "Z.ai", "api_key_invalid");
    };
    auth.set_sensitive(true);
    let Some(client) = zai_client() else {
        return ProviderUsage::error("zai", "Z.ai", "client_error");
    };
    let response = match client
        .get(ZAI_QUOTA_URL)
        .header(AUTHORIZATION, auth)
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return ProviderUsage::error("zai", "Z.ai", "network_error"),
    };
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        || response.status() == reqwest::StatusCode::FORBIDDEN
    {
        return ProviderUsage::error("zai", "Z.ai", "auth_failed");
    }
    if !response.status().is_success() {
        return ProviderUsage::error("zai", "Z.ai", "provider_error");
    }
    let bytes = match read_body_capped(response).await {
        Ok(bytes) => bytes,
        _ => return ProviderUsage::error("zai", "Z.ai", "response_invalid"),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return ProviderUsage::error("zai", "Z.ai", "response_invalid"),
    };
    parse_zai(&value).unwrap_or_else(|| ProviderUsage::unavailable("zai", "Z.ai", "no_data"))
}

async fn read_body_capped(mut response: reqwest::Response) -> Result<Vec<u8>, ()> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if body.len().saturating_add(chunk.len()) > BODY_CAP {
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn parse_zai(value: &Value) -> Option<ProviderUsage> {
    let root = value.get("data").unwrap_or(value);
    let limits = root.get("limits").and_then(Value::as_array)?;
    let mut windows = Vec::new();
    for (index, quota) in limits.iter().enumerate() {
        let kind = quota.get("type").and_then(Value::as_str).unwrap_or("QUOTA");
        let used_count = quota.get("currentValue").and_then(Value::as_f64);
        let limit_count = quota
            .get("usage")
            .and_then(Value::as_f64)
            .or_else(|| quota.get("limit").and_then(Value::as_f64));
        let percentage = quota.get("percentage").and_then(Value::as_f64).or_else(|| {
            match (used_count, limit_count) {
                (Some(used), Some(limit)) if limit > 0.0 => Some(used / limit * 100.0),
                _ => None,
            }
        });
        let Some(used_percent) = percentage else {
            continue;
        };
        let (label, duration) = zai_window_label(quota, kind);
        let reset = quota
            .get("nextResetTime")
            .and_then(Value::as_u64)
            .map(normalize_epoch);
        windows.push(make_window(
            format!("{}-{index}", kind.to_ascii_lowercase()),
            label,
            used_percent,
            duration,
            reset,
            used_count,
            limit_count,
        ));
    }
    if windows.is_empty() {
        return None;
    }
    Some(ProviderUsage {
        provider: "zai",
        display_name: "Z.ai",
        status: "ready",
        reason_code: None,
        detail: None,
        plan: root
            .get("level")
            .and_then(Value::as_str)
            .map(str::to_string),
        windows,
        observed_at: Some(unix_now()),
    })
}

fn zai_window_label(quota: &Value, kind: &str) -> (String, Option<u64>) {
    let unit = quota.get("unit").and_then(Value::as_u64);
    let number = quota.get("number").and_then(Value::as_u64);
    if kind == "TOKENS_LIMIT" || (unit == Some(3) && number == Some(5)) {
        ("5h".into(), Some(300))
    } else if unit == Some(6) && number == Some(1) {
        ("7d".into(), Some(10_080))
    } else if kind == "TIME_LIMIT" {
        ("MCP monthly".into(), None)
    } else if kind == "CREDIT_LIMIT" {
        ("Coding quota".into(), None)
    } else {
        (kind.replace('_', " ").to_ascii_lowercase(), None)
    }
}

fn make_window(
    id: String,
    label: String,
    used: f64,
    duration: Option<u64>,
    resets_at: Option<u64>,
    used_count: Option<f64>,
    limit_count: Option<f64>,
) -> UsageWindow {
    let used_percent = used.clamp(0.0, 100.0);
    UsageWindow {
        id,
        label,
        used_percent,
        remaining_percent: (100.0 - used_percent).max(0.0),
        window_duration_minutes: duration,
        resets_at,
        used: used_count,
        limit: limit_count,
    }
}

fn normalize_epoch(value: u64) -> u64 {
    if value > 10_000_000_000 {
        value / 1000
    } else {
        value
    }
}

fn duration_label(duration: Option<u64>) -> String {
    match duration {
        Some(300) => "5h".into(),
        Some(10_080) => "7d".into(),
        Some(minutes) if minutes % 1_440 == 0 => format!("{}d", minutes / 1_440),
        Some(minutes) if minutes % 60 == 0 => format!("{}h", minutes / 60),
        Some(minutes) => format!("{minutes}m"),
        None => "quota".into(),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `<data_dir>/secrets` — the same store `iyagi-termd` opens under its own
/// data root when it resolves `claude_provider` at launch. Callers pass the
/// bridge's effective data dir (never a freshly computed default) so the
/// key written from Settings is the key the daemon reads back.
fn secret_store(data_dir: &Path) -> LocalSecretStore {
    LocalSecretStore::new(data_dir)
}

pub fn get_zai_key(data_dir: &Path) -> Result<Option<String>, &'static str> {
    secret_store(data_dir).read()
}

pub fn set_zai_key(data_dir: &Path, key: &str) -> Result<(), &'static str> {
    validate_key(key)?;
    secret_store(data_dir).write(key.trim())
}

pub fn remove_zai_key(data_dir: &Path) -> Result<(), &'static str> {
    secret_store(data_dir).remove()
}

pub fn key_status(data_dir: &Path) -> Result<KeyStatus, &'static str> {
    get_zai_key(data_dir)
        .map(|key| KeyStatus {
            configured: key.is_some(),
        })
        .map_err(|_| "credential_store_error")
}

fn validate_key(key: &str) -> Result<(), &'static str> {
    let trimmed = key.trim();
    if trimmed.len() < 8 || trimmed.len() > 1024 || trimmed.chars().any(char::is_control) {
        Err("api_key_invalid")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_control_usage_keeps_fable_separate_and_drops_session_data() {
        let value = json!({
            "subscription_type": "max", "session": { "private": "discard" },
            "rate_limits": {
                "five_hour": { "utilization": 43, "resets_at": 2000000000 },
                "seven_day": { "utilization": 57, "resets_at": null },
                "model_scoped": [{ "display_name": "Fable", "utilization": 87,
                    "resets_at": "2026-09-14T01:00:00Z" }]
            }
        });
        let usage = parse_claude_usage(&value).unwrap();
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].remaining_percent, 57.0);
        let fable = &usage.windows[2];
        assert_eq!(fable.id, "model-scoped:Fable");
        assert_eq!(fable.label, "Fable · 7d");
        assert_eq!(fable.remaining_percent, 13.0);
        assert_eq!(fable.resets_at, parse_rfc3339_epoch("2026-09-14T01:00:00Z"));
        assert_eq!(usage.plan.as_deref(), Some("max"));
        assert!(!serde_json::to_string(&usage).unwrap().contains("private"));
    }

    #[test]
    fn claude_usage_handles_old_missing_and_invalid_model_windows() {
        assert!(parse_claude_usage(&json!({"rate_limits": null})).is_none());
        let value = json!({"rate_limits": {
            "five_hour": {"utilization": 25},
            "model_scoped": [
                {"display_name": "Fable", "utilization": null},
                {"display_name": "", "utilization": 20},
                {"display_name": "Fable", "utilization": 120},
                {"display_name": "Fable", "utilization": 5}
            ]
        }});
        let usage = parse_claude_usage(&value).unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[1].remaining_percent, 0.0);
        let old =
            parse_claude_usage(&json!({"rate_limits":{"five_hour":{"utilization":25}}})).unwrap();
        assert_eq!(old.windows.len(), 1);
    }

    #[tokio::test]
    #[ignore = "requires an installed, signed-in Claude CLI; sends only get_usage"]
    async fn claude_usage_native_smoke() {
        let program = agent_program("claude", &CLAUDE_PATH)
            .await
            .expect("Claude installed");
        let usage = tokio::time::timeout(Duration::from_secs(8), claude_transaction(&program))
            .await
            .expect("usage timeout")
            .expect("usage response");
        assert_eq!(usage.provider, "claude");
        assert!(!usage.windows.is_empty());
        assert!(usage
            .windows
            .iter()
            .all(|w| (0.0..=100.0).contains(&w.remaining_percent)));
    }

    #[test]
    fn parses_codex_windows_without_assuming_lane_duration() {
        let value = json!({"result":{"rateLimits":{"planType":"plus","primary":{"usedPercent":25,"windowDurationMins":300,"resetsAt":100},"secondary":{"usedPercent":60,"windowDurationMins":10080,"resetsAt":200}}}});
        let parsed = parse_codex(&value, None).unwrap();
        assert_eq!(parsed.windows[0].label, "5h");
        assert_eq!(parsed.windows[1].remaining_percent, 40.0);
    }

    #[test]
    fn parses_zai_legacy_and_credit_windows() {
        let value = json!({"data":{"level":"max","limits":[
            {"type":"TOKENS_LIMIT","percentage":20,"nextResetTime":2_000_000_000_000u64},
            {"type":"CREDIT_LIMIT","unit":6,"number":1,"usage":2000,"currentValue":500}
        ]}});
        let parsed = parse_zai(&value).unwrap();
        assert_eq!(parsed.windows[0].label, "5h");
        assert_eq!(parsed.windows[0].resets_at, Some(2_000_000_000));
        assert_eq!(parsed.windows[1].label, "7d");
        assert_eq!(parsed.windows[1].used_percent, 25.0);
    }

    #[test]
    fn rejects_multiline_or_tiny_keys() {
        assert!(validate_key("tiny").is_err());
        assert!(validate_key("valid-key\nsecond").is_err());
        assert!(validate_key("valid-key-123").is_ok());
    }

    #[test]
    fn zai_key_lives_under_the_given_data_dir_only() {
        let primary = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        assert!(!key_status(primary.path()).unwrap().configured);
        // Surrounding whitespace is trimmed before the store sees the key.
        set_zai_key(primary.path(), "  valid-key-123  ").unwrap();
        assert!(key_status(primary.path()).unwrap().configured);
        assert_eq!(
            get_zai_key(primary.path()).unwrap().as_deref(),
            Some("valid-key-123")
        );
        assert!(primary.path().join("secrets").join("zai.enc").is_file());
        // Another data root (a daemon started with a different `--data-dir`)
        // never sees it: the store is pinned to the dir it was given.
        assert!(!key_status(other.path()).unwrap().configured);
        assert!(!other.path().join("secrets").exists());
        remove_zai_key(primary.path()).unwrap();
        assert!(!key_status(primary.path()).unwrap().configured);
        // Rejected keys never reach the store.
        assert_eq!(set_zai_key(primary.path(), "tiny"), Err("api_key_invalid"));
        assert!(!primary.path().join("secrets").join("zai.enc").exists());
    }
}
