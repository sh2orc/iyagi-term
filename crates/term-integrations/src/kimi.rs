//! Kimi Code membership quota for the status-bar usage gauge.
//!
//! Two read-only credentials, tried in this order. Both draw from the same
//! membership quota (5-hour, weekly and monthly windows):
//!
//! 1. **The Kimi Code CLI login** (`$KIMI_CODE_HOME`, default `~/.kimi-code`):
//!    its OAuth access token, used only while it is still valid (about 15
//!    minutes after the CLI last refreshed it). It is never refreshed here:
//!    refreshing rotates the refresh token, and the CLI owns that rotation
//!    behind its own `oauth/<slot>.lock`.
//! 2. **The membership API key** stored in iyagi (`kimi-code.enc`). It does not
//!    expire, so the gauge keeps working while the CLI is idle.
//!
//! Kimi runs two deployments — global (`kimi.ai`) and mainland China
//! (`kimi.com`) — and a credential only works in its own. The CLI login names
//! its endpoint in `config.toml`; the key is tried in both, and the region
//! that accepted it is remembered for later reads. Kimi Open Platform
//! (pay-as-you-go) keys are not supported.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION};
use serde_json::Value;
use term_secrets::LocalSecretStore;
use zeroize::Zeroizing;

use crate::subscriptions::{KeyStatus, ProviderUsage, UsageWindow};

const PROVIDER: &str = "kimi-code";
const DISPLAY_NAME: &str = "Kimi Code";
const BODY_CAP: usize = 256 * 1024;
const CREDENTIAL_CAP: u64 = 64 * 1024;
const CONFIG_CAP: u64 = 1024 * 1024;
/// Treat an access token as expired this many seconds early, so a request
/// never leaves with a token that dies in flight.
const TOKEN_MARGIN_SECS: u64 = 30;
/// The provider the Kimi Code CLI writes for its own login.
const MANAGED_PROVIDER: &str = "managed:kimi-code";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KimiRegion {
    Global,
    MainlandCn,
}

impl KimiRegion {
    fn usage_url(self) -> &'static str {
        match self {
            Self::Global => "https://api.kimi.ai/coding/v1/usages",
            Self::MainlandCn => "https://api.kimi.com/coding/v1/usages",
        }
    }

    /// The official region a `base_url` belongs to. Anything else — a
    /// custom gateway, plain http — gets no credential from us.
    fn of_base_url(base_url: &str) -> Option<Self> {
        let rest = base_url.trim().strip_prefix("https://")?;
        let host = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
        match host.as_str() {
            "api.kimi.ai" => Some(Self::Global),
            "api.kimi.com" => Some(Self::MainlandCn),
            _ => None,
        }
    }
}

fn store(data_dir: &Path) -> LocalSecretStore {
    LocalSecretStore::kimi_code(data_dir)
}

pub fn key_status(data_dir: &Path) -> Result<KeyStatus, &'static str> {
    store(data_dir)
        .read()
        .map(|key| KeyStatus {
            configured: key.is_some(),
        })
        .map_err(|_| "credential_store_error")
}

pub fn set_key(data_dir: &Path, key: &str) -> Result<(), &'static str> {
    validate_key(key)?;
    store(data_dir).write(key.trim())?;
    // A new key may belong to the other region; the next usage read finds out.
    set_key_region(None);
    Ok(())
}

pub fn remove_key(data_dir: &Path) -> Result<(), &'static str> {
    store(data_dir).remove()?;
    set_key_region(None);
    Ok(())
}

fn validate_key(key: &str) -> Result<(), &'static str> {
    let trimmed = key.trim();
    if trimmed.len() < 8 || trimmed.len() > 1024 || trimmed.chars().any(char::is_control) {
        Err("api_key_invalid")
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------- CLI login

/// What the Kimi Code CLI login offers right now.
#[derive(Debug)]
enum CliLogin {
    /// No CLI home, no OAuth login, or a login stored somewhere we do not
    /// read (keyring storage, a custom endpoint).
    Missing,
    /// Logged in, but the access token has expired since the CLI last ran.
    Expired { region: KimiRegion },
    Valid {
        region: KimiRegion,
        access_token: Zeroizing<String>,
    },
}

fn kimi_code_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("KIMI_CODE_HOME").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".kimi-code"))
}

fn read_capped(path: &Path, cap: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > cap {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The OAuth slot and endpoint of the CLI's own login, from `config.toml`.
#[derive(Debug, Default, PartialEq, Eq)]
struct LoginRef {
    base_url: Option<String>,
    storage: Option<String>,
    key: Option<String>,
}

/// Reads `[providers."managed:kimi-code"]` and its `.oauth` table. A small
/// line scanner on purpose: only flat `key = "string"` pairs are needed.
fn login_ref(config: &str) -> LoginRef {
    let mut found = LoginRef::default();
    let mut table: Vec<String> = Vec::new();
    for raw in config.lines() {
        let line = strip_comment(raw).trim();
        if line.starts_with("[[") {
            table = vec!["[[array]]".to_string()];
            continue;
        }
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            table = header_path(header);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let in_provider =
            table.len() >= 2 && table[0] == "providers" && table[1] == MANAGED_PROVIDER;
        match (in_provider, table.len(), key) {
            (true, 2, "base_url") => found.base_url = toml_string(value),
            (true, 3, "storage") if table[2] == "oauth" => found.storage = toml_string(value),
            (true, 3, "key") if table[2] == "oauth" => found.key = toml_string(value),
            _ => {}
        }
    }
    found
}

fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..index],
            _ => {}
        }
    }
    line
}

fn header_path(header: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in header.trim().chars() {
        match ch {
            '"' => quoted = !quoted,
            '.' if !quoted => parts.push(std::mem::take(&mut current).trim().to_string()),
            _ => current.push(ch),
        }
    }
    parts.push(current.trim().to_string());
    parts
}

fn toml_string(value: &str) -> Option<String> {
    let value = value.trim();
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    if inner.contains('\\') || inner.contains('"') {
        return None;
    }
    Some(inner.to_string())
}

/// `oauth/<slot>` → `<home>/credentials/<slot>.json`, refusing anything that
/// could step outside that directory.
fn credential_path(home: &Path, key: &str) -> Option<PathBuf> {
    let slot = key.strip_prefix("oauth/")?;
    let valid = !slot.is_empty()
        && slot.len() <= 128
        && slot
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
    valid.then(|| home.join("credentials").join(format!("{slot}.json")))
}

fn read_cli_login(home: &Path, now: u64) -> CliLogin {
    let Some(config) = read_capped(&home.join("config.toml"), CONFIG_CAP) else {
        return CliLogin::Missing;
    };
    let login = login_ref(&config);
    let (Some(base_url), Some(key)) = (login.base_url, login.key) else {
        return CliLogin::Missing;
    };
    if login
        .storage
        .as_deref()
        .is_some_and(|storage| storage != "file")
    {
        return CliLogin::Missing;
    }
    let (Some(region), Some(path)) = (
        KimiRegion::of_base_url(&base_url),
        credential_path(home, &key),
    ) else {
        return CliLogin::Missing;
    };
    let Some(text) = read_capped(&path, CREDENTIAL_CAP).map(Zeroizing::new) else {
        return CliLogin::Missing;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return CliLogin::Missing;
    };
    let token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty());
    let expires_at = number(value.get("expires_at")).map(|epoch| {
        if epoch > 100_000_000_000.0 {
            (epoch / 1000.0) as u64
        } else {
            epoch as u64
        }
    });
    match (token, expires_at) {
        (Some(token), Some(expires_at)) if expires_at > now.saturating_add(TOKEN_MARGIN_SECS) => {
            CliLogin::Valid {
                region,
                access_token: Zeroizing::new(token.to_string()),
            }
        }
        (Some(_), _) => CliLogin::Expired { region },
        (None, _) => CliLogin::Missing,
    }
}

// ---------------------------------------------------------------- refresh

static RETIRED_CLEANUP: Once = Once::new();
/// The last successful reading, shown (with its own `observed_at`) while no
/// credential can read a fresh one — e.g. the CLI token expired and no key
/// is stored.
static LAST_READY: Mutex<Option<ProviderUsage>> = Mutex::new(None);
/// The region that last accepted the stored key, so later reads start there.
/// Kept in memory only: after a restart a wrong first guess costs one 401.
static KEY_REGION: Mutex<Option<KimiRegion>> = Mutex::new(None);

fn key_region() -> Option<KimiRegion> {
    KEY_REGION.lock().ok().and_then(|region| *region)
}

fn set_key_region(region: Option<KimiRegion>) {
    if let Ok(mut current) = KEY_REGION.lock() {
        *current = region;
    }
}

pub async fn refresh(data_dir: &Path) -> ProviderUsage {
    RETIRED_CLEANUP.call_once(|| term_secrets::remove_retired_slots(data_dir));
    let now = unix_now();
    let home = kimi_code_home();
    let dir = data_dir.to_path_buf();
    let read = tokio::task::spawn_blocking(move || {
        let login = home
            .as_deref()
            .map_or(CliLogin::Missing, |home| read_cli_login(home, now));
        let key = store(&dir).read_zeroizing();
        (login, key)
    })
    .await;
    let Ok((login, key)) = read else {
        return error("credential_store_error");
    };
    let usage = read_usage(login, key, key_region()).await;
    remember(usage)
}

async fn read_usage(
    login: CliLogin,
    key: Result<Option<Zeroizing<String>>, &'static str>,
    remembered: Option<KimiRegion>,
) -> ProviderUsage {
    // An unreadable key store must not hide a working CLI login; it is
    // reported only when nothing else can read the quota.
    let key_unreadable = key.is_err();
    let key = key.unwrap_or_default();
    let login_region = match &login {
        CliLogin::Valid { region, .. } | CliLogin::Expired { region } => Some(*region),
        CliLogin::Missing => None,
    };
    if let CliLogin::Valid {
        region,
        access_token,
    } = &login
    {
        match fetch(region.usage_url(), access_token).await {
            Ok(usage) => return usage,
            // A rejected login token is the CLI's problem; the key may still work.
            Err("auth_failed") => {}
            Err(reason) if key.is_none() => return error(reason),
            Err(_) => {}
        }
    }
    if let Some(key) = key {
        let mut regions = Vec::with_capacity(2);
        for region in [
            remembered,
            login_region,
            Some(KimiRegion::Global),
            Some(KimiRegion::MainlandCn),
        ]
        .into_iter()
        .flatten()
        {
            if !regions.contains(&region) {
                regions.push(region);
            }
        }
        for region in regions {
            match fetch(region.usage_url(), &key).await {
                Ok(usage) => {
                    set_key_region(Some(region));
                    return usage;
                }
                // The key belongs to the other deployment — try it there.
                Err("auth_failed") => continue,
                Err(reason) => return error(reason),
            }
        }
        // Neither deployment knows it: most often a pay-as-you-go Open
        // Platform key (`api.moonshot.ai`) saved where a membership key goes.
        return error("kimi_key_rejected");
    }
    if key_unreadable {
        return error("credential_store_error");
    }
    match login {
        CliLogin::Expired { .. } => {
            let mut usage = unavailable("kimi_login_stale");
            usage.status = "unavailable";
            usage
        }
        _ => {
            let mut usage = unavailable("not_configured");
            usage.status = "notConfigured";
            usage
        }
    }
}

/// Keep the latest ready reading; when only "the CLI token went stale" is
/// left to say, show that reading instead — its `observed_at` says how old.
fn remember(usage: ProviderUsage) -> ProviderUsage {
    let Ok(mut last) = LAST_READY.lock() else {
        return usage;
    };
    if usage.status == "ready" {
        *last = Some(usage.clone());
        return usage;
    }
    if usage.reason_code == Some("kimi_login_stale") {
        if let Some(previous) = last.as_ref() {
            return previous.clone();
        }
    }
    if usage.status == "notConfigured" {
        *last = None;
    }
    usage
}

async fn fetch(url: &str, credential: &str) -> Result<ProviderUsage, &'static str> {
    let Ok(mut auth) = HeaderValue::from_str(&format!("Bearer {}", credential.trim())) else {
        return Err("api_key_invalid");
    };
    auth.set_sensitive(true);
    let client = client().ok_or("client_error")?;
    let response = client
        .get(url)
        .header(AUTHORIZATION, auth)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "network_error")?;
    if matches!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        return Err("auth_failed");
    }
    if !response.status().is_success() {
        return Err("provider_error");
    }
    let bytes = read_body_capped(response)
        .await
        .map_err(|_| "response_invalid")?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "response_invalid")?;
    Ok(parse_code_usage(&value).unwrap_or_else(|| unavailable("no_data")))
}

fn unavailable(reason: &'static str) -> ProviderUsage {
    ProviderUsage {
        provider: PROVIDER,
        display_name: DISPLAY_NAME,
        status: "unavailable",
        reason_code: Some(reason),
        detail: None,
        plan: None,
        windows: Vec::new(),
        observed_at: None,
    }
}

fn error(reason: &'static str) -> ProviderUsage {
    let mut usage = unavailable(reason);
    usage.status = "error";
    usage
}

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn client() -> Option<reqwest::Client> {
    if let Some(client) = CLIENT.get() {
        return Some(client.clone());
    }
    let built = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;
    Some(CLIENT.get_or_init(|| built).clone())
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

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
}

fn reset_time(value: &Value, snake: bool) -> Option<u64> {
    let value = if snake {
        value.get("reset_time").or_else(|| value.get("reset_at"))
    } else {
        value.get("resetTime").or_else(|| value.get("reset_at"))
    }?;
    match value {
        Value::Number(_) => number(Some(value)).map(|epoch| {
            if epoch > 10_000_000_000.0 {
                (epoch / 1000.0) as u64
            } else {
                epoch as u64
            }
        }),
        Value::String(text) => text.parse().ok().or_else(|| {
            time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
                .ok()
                .and_then(|date| u64::try_from(date.unix_timestamp()).ok())
        }),
        _ => None,
    }
}

fn count_window(
    id: &str,
    label: String,
    duration: Option<u64>,
    detail: &Value,
) -> Option<UsageWindow> {
    let limit = number(detail.get("limit"))?;
    if limit <= 0.0 {
        return None;
    }
    let remaining = number(detail.get("remaining"));
    let used =
        number(detail.get("used")).or_else(|| remaining.map(|left| (limit - left).max(0.0)))?;
    Some(make_window(
        id.to_string(),
        label,
        used / limit * 100.0,
        duration,
        reset_time(detail, false),
        Some(used),
        Some(limit),
    ))
}

fn ratio_window(
    id: &str,
    label: &str,
    duration: Option<u64>,
    value: &Value,
) -> Option<UsageWindow> {
    let ratio = number(value.get("used_ratio"))?;
    if ratio < 0.0 {
        return None;
    }
    let percent = if ratio <= 1.0 { ratio * 100.0 } else { ratio };
    Some(make_window(
        id.to_string(),
        label.to_string(),
        percent,
        duration,
        reset_time(value, true),
        None,
        None,
    ))
}

fn parse_code_usage(value: &Value) -> Option<ProviderUsage> {
    let mut windows = Vec::new();
    if let Some(usage) = value.get("usage") {
        if let Some(window) = count_window("weekly", "7d".to_string(), Some(10_080), usage) {
            windows.push(window);
        }
    }
    if let Some(limits) = value.get("limits").and_then(Value::as_array) {
        for (index, entry) in limits.iter().enumerate() {
            let Some(detail) = entry.get("detail") else {
                continue;
            };
            let window = entry.get("window");
            let duration = window.and_then(|window| {
                let amount = number(window.get("duration"))? as u64;
                let unit = window
                    .get("timeUnit")
                    .and_then(Value::as_str)
                    .unwrap_or("MINUTE")
                    .trim_start_matches("TIME_UNIT_");
                let multiplier = match unit {
                    "SECOND" => 1.0 / 60.0,
                    "MINUTE" => 1.0,
                    "HOUR" => 60.0,
                    "DAY" => 1_440.0,
                    "WEEK" => 10_080.0,
                    _ => return None,
                };
                Some((amount as f64 * multiplier).round() as u64)
            });
            let id = if duration == Some(300) {
                "5h".to_string()
            } else {
                format!("limit-{index}")
            };
            if let Some(window) = count_window(&id, duration_label(duration), duration, detail) {
                windows.push(window);
            }
        }
    }
    if let Some(usages) = value.get("usages").and_then(Value::as_object) {
        for (key, id, label, duration) in [
            ("limit_5h", "5h", "5h", Some(300)),
            ("limit_week", "weekly", "7d", Some(10_080)),
            ("limit_7d", "weekly", "7d", Some(10_080)),
            ("limit_month_total", "monthly", "Monthly total", None),
        ] {
            if windows.iter().any(|window| window.id == id) {
                continue;
            }
            if let Some(window) = usages
                .get(key)
                .and_then(|value| ratio_window(id, label, duration, value))
            {
                windows.push(window);
            }
        }
    }
    if windows.iter().all(|window| window.id != "monthly") {
        if let Some(total) = value.get("totalQuota") {
            let exhausted = number(total.get("used")).is_some_and(|used| used > 0.0);
            if total.get("used").is_some() || total.get("remaining").is_some() {
                windows.push(make_window(
                    "monthly".to_string(),
                    "Monthly total".to_string(),
                    if exhausted { 100.0 } else { 0.0 },
                    None,
                    None,
                    None,
                    None,
                ));
            }
        }
    }
    if windows.is_empty() {
        return None;
    }
    let plan = value
        .pointer("/membership/level")
        .or_else(|| value.pointer("/membership/tier"))
        .or_else(|| value.get("level"))
        .or_else(|| value.get("plan"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(ProviderUsage {
        provider: PROVIDER,
        display_name: DISPLAY_NAME,
        status: "ready",
        reason_code: None,
        detail: None,
        plan,
        windows,
        observed_at: Some(unix_now()),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_legacy_weekly_and_count_windows() {
        let usage = parse_code_usage(&json!({
            "usage": {"limit":"2048","used":"214","remaining":"1834","resetTime":"2026-11-16T00:00:00Z"},
            "limits": [{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},
                "detail":{"limit":"200","used":"139","remaining":"61","resetTime":"2026-11-09T12:00:00Z"}}]
        }))
        .unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].id, "weekly");
        assert_eq!(usage.windows[1].id, "5h");
        assert_eq!(usage.windows[1].remaining_percent, 30.5);
        assert_eq!(usage.windows[1].used, Some(139.0));
    }

    #[test]
    fn parses_ratio_monthly_and_prefers_absolute_five_hour_counts() {
        let usage = parse_code_usage(&json!({
            "limits": [{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},
                "detail":{"limit":"100","used":"25","remaining":"75","resetTime":"2026-11-09T12:00:00Z"}}],
            "usages": {
                "limit_5h":{"used_ratio":0.9,"reset_time":"2026-11-09T12:00:00Z"},
                "limit_month_total":{"used_ratio":0.0795,"reset_time":"2026-12-09T00:00:00Z"},
                "limit_month_code":{"used_ratio":0.02,"reset_time":"2026-12-09T00:00:00Z"}
            }
        }))
        .unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].used_percent, 25.0);
        assert_eq!(usage.windows[1].id, "monthly");
        assert_eq!(usage.windows[1].used_percent, 7.95);
        assert!(!usage
            .windows
            .iter()
            .any(|window| window.id == "limit_month_code"));
    }

    #[test]
    fn key_round_trip_forgets_the_remembered_region() {
        let root = tempfile::tempdir().unwrap();
        assert!(!key_status(root.path()).unwrap().configured);
        set_key(root.path(), "kimi-code-secret").unwrap();
        assert!(key_status(root.path()).unwrap().configured);
        set_key_region(Some(KimiRegion::MainlandCn));
        set_key(root.path(), "kimi-code-secret-two").unwrap();
        assert_eq!(key_region(), None);
        set_key_region(Some(KimiRegion::Global));
        remove_key(root.path()).unwrap();
        assert!(!key_status(root.path()).unwrap().configured);
        assert_eq!(key_region(), None);
        assert_eq!(set_key(root.path(), "short"), Err("api_key_invalid"));
    }

    const CLI_CONFIG: &str = r#"default_model = "kimi-code/k3.1-pro"
[[hooks]]
event = "Stop"
command = "echo '[providers.x]' # not a table"
[providers."managed:kimi-code"]
type = "kimi"
api_key = ""
base_url = "https://api.kimi.ai/coding/v1" # global login
[providers."managed:kimi-code".oauth]
storage = "file"
key = "oauth/kimi-code-env-0e4f99c69cc27850"
oauth_host = "https://auth.kimi.ai"
[models."kimi-code/k3"]
provider = "managed:kimi-code"
base_url = "https://example.invalid"
"#;

    #[test]
    fn login_ref_reads_only_the_managed_provider_and_its_oauth_table() {
        assert_eq!(
            login_ref(CLI_CONFIG),
            LoginRef {
                base_url: Some("https://api.kimi.ai/coding/v1".into()),
                storage: Some("file".into()),
                key: Some("oauth/kimi-code-env-0e4f99c69cc27850".into()),
            }
        );
        assert_eq!(
            login_ref("[providers.other]\nbase_url = \"https://api.kimi.ai/coding/v1\"\n"),
            LoginRef::default()
        );
    }

    #[test]
    fn regions_come_only_from_official_https_hosts() {
        assert_eq!(
            KimiRegion::of_base_url("https://api.kimi.ai/coding/v1"),
            Some(KimiRegion::Global)
        );
        assert_eq!(
            KimiRegion::of_base_url("https://API.kimi.com/coding/v1/"),
            Some(KimiRegion::MainlandCn)
        );
        assert_eq!(
            KimiRegion::of_base_url("http://api.kimi.ai/coding/v1"),
            None
        );
        assert_eq!(
            KimiRegion::of_base_url("https://api.kimi.ai.evil.example/coding"),
            None
        );
        assert_eq!(
            KimiRegion::of_base_url("https://gateway.local/coding/v1"),
            None
        );
        assert_eq!(
            KimiRegion::Global.usage_url(),
            "https://api.kimi.ai/coding/v1/usages"
        );
        assert_eq!(
            KimiRegion::MainlandCn.usage_url(),
            "https://api.kimi.com/coding/v1/usages"
        );
    }

    #[test]
    fn credential_paths_stay_inside_the_credentials_directory() {
        let home = Path::new("/h/.kimi-code");
        assert_eq!(
            credential_path(home, "oauth/kimi-code").unwrap(),
            home.join("credentials").join("kimi-code.json")
        );
        for bad in [
            "kimi-code",
            "oauth/",
            "oauth/../x",
            "oauth/a/b",
            "oauth/a.b",
        ] {
            assert_eq!(credential_path(home, bad), None, "{bad}");
        }
    }

    fn cli_home(config: &str, credential: Option<serde_json::Value>) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), config).unwrap();
        if let Some(credential) = credential {
            std::fs::create_dir_all(home.path().join("credentials")).unwrap();
            std::fs::write(
                home.path()
                    .join("credentials")
                    .join("kimi-code-env-0e4f99c69cc27850.json"),
                credential.to_string(),
            )
            .unwrap();
        }
        home
    }

    #[test]
    fn cli_login_is_valid_only_before_expiry_and_never_from_other_storage() {
        let now = 1_800_000_000;
        let valid = cli_home(
            CLI_CONFIG,
            Some(
                json!({"access_token":"tok","refresh_token":"r","expires_at": now + 600,"expires_in":900}),
            ),
        );
        match read_cli_login(valid.path(), now) {
            CliLogin::Valid {
                region,
                access_token,
            } => {
                assert_eq!(region, KimiRegion::Global);
                assert_eq!(access_token.as_str(), "tok");
            }
            other => panic!("expected a valid login, got {other:?}"),
        }
        // Milliseconds are accepted too.
        let millis = cli_home(
            CLI_CONFIG,
            Some(json!({"access_token":"tok","expires_at": (now + 600) * 1000})),
        );
        assert!(matches!(
            read_cli_login(millis.path(), now),
            CliLogin::Valid { .. }
        ));

        let near = cli_home(
            CLI_CONFIG,
            Some(json!({"access_token":"tok","expires_at": now + 10})),
        );
        assert!(matches!(
            read_cli_login(near.path(), now),
            CliLogin::Expired {
                region: KimiRegion::Global
            }
        ));

        let missing_file = cli_home(CLI_CONFIG, None);
        assert!(matches!(
            read_cli_login(missing_file.path(), now),
            CliLogin::Missing
        ));

        let keyring = cli_home(
            &CLI_CONFIG.replace("storage = \"file\"", "storage = \"keyring\""),
            Some(json!({"access_token":"tok","expires_at": now + 600})),
        );
        assert!(matches!(
            read_cli_login(keyring.path(), now),
            CliLogin::Missing
        ));

        let custom = cli_home(
            &CLI_CONFIG.replace("https://api.kimi.ai/coding/v1", "https://gateway.local/v1"),
            Some(json!({"access_token":"tok","expires_at": now + 600})),
        );
        assert!(matches!(
            read_cli_login(custom.path(), now),
            CliLogin::Missing
        ));

        let no_home = tempfile::tempdir().unwrap();
        assert!(matches!(
            read_cli_login(no_home.path(), now),
            CliLogin::Missing
        ));
    }

    #[tokio::test]
    async fn without_a_key_the_status_says_setup_or_stale_login() {
        let missing = read_usage(CliLogin::Missing, Ok(None), None).await;
        assert_eq!(missing.status, "notConfigured");
        assert_eq!(missing.reason_code, Some("not_configured"));
        let stale = read_usage(
            CliLogin::Expired {
                region: KimiRegion::Global,
            },
            Ok(None),
            None,
        )
        .await;
        assert_eq!(stale.status, "unavailable");
        assert_eq!(stale.reason_code, Some("kimi_login_stale"));
    }

    #[test]
    fn a_stale_login_keeps_showing_the_last_reading() {
        let ready = parse_code_usage(&json!({"usages":{"limit_5h":{"used_ratio":0.25}}})).unwrap();
        assert_eq!(remember(ready.clone()).status, "ready");
        let shown = remember(unavailable("kimi_login_stale"));
        assert_eq!(shown.status, "ready");
        assert_eq!(shown.windows[0].used_percent, 25.0);
        // Other failures are reported as they are.
        assert_eq!(
            remember(error("network_error")).reason_code,
            Some("network_error")
        );
        let mut cleared = unavailable("not_configured");
        cleared.status = "notConfigured";
        remember(cleared);
        assert_eq!(
            remember(unavailable("kimi_login_stale")).reason_code,
            Some("kimi_login_stale")
        );
    }
}
