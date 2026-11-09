//! Kimi Code membership quotas and Kimi Open Platform balance.
//! The two platforms use different credentials and must never share a key slot.

use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderValue, AUTHORIZATION};
use serde_json::Value;
use term_secrets::LocalSecretStore;

use crate::subscriptions::{KeyStatus, ProviderBalance, ProviderUsage, UsageWindow};

const KIMI_CODE_USAGE_URL: &str = "https://api.kimi.com/coding/v1/usages";
const KIMI_OPEN_BALANCE_URL: &str = "https://api.moonshot.ai/v1/users/me/balance";
const BODY_CAP: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KimiPlatform {
    Code,
    OpenPlatform,
}

impl KimiPlatform {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "kimi-code" => Ok(Self::Code),
            "kimi-open-platform" => Ok(Self::OpenPlatform),
            _ => Err("invalid_kimi_platform"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Code => "kimi-code",
            Self::OpenPlatform => "kimi-open-platform",
        }
    }

    fn provider_name(self) -> &'static str {
        match self {
            Self::Code => "Kimi Code",
            Self::OpenPlatform => "Kimi Open",
        }
    }

    fn store(self, data_dir: &Path) -> LocalSecretStore {
        match self {
            Self::Code => LocalSecretStore::kimi_code(data_dir),
            Self::OpenPlatform => LocalSecretStore::kimi_open_platform(data_dir),
        }
    }
}

pub async fn refresh(data_dir: &Path) -> [ProviderUsage; 2] {
    let (code, open) = tokio::join!(
        read_platform(data_dir, KimiPlatform::Code),
        read_platform(data_dir, KimiPlatform::OpenPlatform)
    );
    [code, open]
}

pub fn key_status(data_dir: &Path, platform: KimiPlatform) -> Result<KeyStatus, &'static str> {
    platform
        .store(data_dir)
        .read()
        .map(|key| KeyStatus {
            configured: key.is_some(),
        })
        .map_err(|_| "credential_store_error")
}

pub fn set_key(data_dir: &Path, platform: KimiPlatform, key: &str) -> Result<(), &'static str> {
    validate_key(key)?;
    platform.store(data_dir).write(key.trim())
}

pub fn remove_key(data_dir: &Path, platform: KimiPlatform) -> Result<(), &'static str> {
    platform.store(data_dir).remove()
}

fn validate_key(key: &str) -> Result<(), &'static str> {
    let trimmed = key.trim();
    if trimmed.len() < 8 || trimmed.len() > 1024 || trimmed.chars().any(char::is_control) {
        Err("api_key_invalid")
    } else {
        Ok(())
    }
}

async fn read_platform(data_dir: &Path, platform: KimiPlatform) -> ProviderUsage {
    let store = platform.store(data_dir);
    let stored = match tokio::task::spawn_blocking(move || store.read()).await {
        Ok(stored) => stored,
        Err(_) => return error(platform, "credential_store_error"),
    };
    let key = match stored {
        Ok(Some(key)) => key,
        Ok(None) => {
            let mut usage = unavailable(platform, "not_configured");
            usage.status = "notConfigured";
            return usage;
        }
        Err(_) => return error(platform, "credential_store_error"),
    };
    let Ok(mut auth) = HeaderValue::from_str(&key) else {
        return error(platform, "api_key_invalid");
    };
    auth.set_sensitive(true);
    let Some(client) = client() else {
        return error(platform, "client_error");
    };
    let url = match platform {
        KimiPlatform::Code => KIMI_CODE_USAGE_URL,
        KimiPlatform::OpenPlatform => KIMI_OPEN_BALANCE_URL,
    };
    let response = match client.get(url).header(AUTHORIZATION, auth).send().await {
        Ok(response) => response,
        Err(_) => return error(platform, "network_error"),
    };
    if matches!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        return error(platform, "auth_failed");
    }
    if !response.status().is_success() {
        return error(platform, "provider_error");
    }
    let bytes = match read_body_capped(response).await {
        Ok(bytes) => bytes,
        Err(_) => return error(platform, "response_invalid"),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return error(platform, "response_invalid"),
    };
    let parsed = match platform {
        KimiPlatform::Code => parse_code_usage(&value),
        KimiPlatform::OpenPlatform => parse_open_balance(&value),
    };
    parsed.unwrap_or_else(|| unavailable(platform, "no_data"))
}

fn unavailable(platform: KimiPlatform, reason: &'static str) -> ProviderUsage {
    ProviderUsage {
        provider: platform.as_str(),
        display_name: platform.provider_name(),
        status: "unavailable",
        reason_code: Some(reason),
        detail: None,
        plan: None,
        windows: Vec::new(),
        balance: None,
        observed_at: None,
    }
}

fn error(platform: KimiPlatform, reason: &'static str) -> ProviderUsage {
    let mut usage = unavailable(platform, reason);
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
        provider: "kimi-code",
        display_name: "Kimi Code",
        status: "ready",
        reason_code: None,
        detail: None,
        plan,
        windows,
        balance: None,
        observed_at: Some(unix_now()),
    })
}

fn parse_open_balance(value: &Value) -> Option<ProviderUsage> {
    let data = value.get("data")?;
    let available = number(data.get("available_balance"))?;
    let cash = number(data.get("cash_balance")).unwrap_or(0.0);
    let voucher = number(data.get("voucher_balance")).unwrap_or(0.0);
    Some(ProviderUsage {
        provider: "kimi-open-platform",
        display_name: "Kimi Open",
        status: "ready",
        reason_code: None,
        detail: None,
        plan: Some("Pay as you go".to_string()),
        windows: Vec::new(),
        balance: Some(ProviderBalance {
            available,
            cash,
            voucher,
            currency: "USD",
        }),
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
    fn parses_open_platform_balance_without_inventing_a_percentage() {
        let usage = parse_open_balance(&json!({"data":{
            "available_balance":49.58894,"voucher_balance":46.58893,"cash_balance":3.00001
        }}))
        .unwrap();
        assert!(usage.windows.is_empty());
        let balance = usage.balance.unwrap();
        assert_eq!(balance.available, 49.58894);
        assert_eq!(balance.currency, "USD");
    }

    #[test]
    fn key_slots_are_independent_and_share_the_existing_master_key() {
        let root = tempfile::tempdir().unwrap();
        set_key(root.path(), KimiPlatform::Code, "kimi-code-secret").unwrap();
        set_key(root.path(), KimiPlatform::OpenPlatform, "kimi-open-secret").unwrap();
        assert!(
            key_status(root.path(), KimiPlatform::Code)
                .unwrap()
                .configured
        );
        assert!(
            key_status(root.path(), KimiPlatform::OpenPlatform)
                .unwrap()
                .configured
        );
        remove_key(root.path(), KimiPlatform::Code).unwrap();
        assert!(
            !key_status(root.path(), KimiPlatform::Code)
                .unwrap()
                .configured
        );
        assert!(
            key_status(root.path(), KimiPlatform::OpenPlatform)
                .unwrap()
                .configured
        );
        assert_eq!(
            KimiPlatform::OpenPlatform
                .store(root.path())
                .read()
                .unwrap()
                .as_deref(),
            Some("kimi-open-secret")
        );
    }
}
