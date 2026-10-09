//! Claude Code status-line bridge. Persists only subscription rate-limit fields
//! and, per session id, the current model display name + effort level (the
//! daemon's agent watch reflects `/model`·`/effort` from it within a tick —
//! the status line re-runs ~70 ms after either command). Transcript paths, cwd
//! and prompt data are deliberately dropped.

use std::io::{Read, Write};
use std::path::Path;

use serde_json::{json, Value};

const STDIN_LIMIT: usize = 256 * 1024;

pub fn run(data_dir: &Path) -> i32 {
    let mut input = String::new();
    if let Err(error) = std::io::stdin()
        .take(STDIN_LIMIT as u64)
        .read_to_string(&mut input)
    {
        eprintln!("iyagi-termd claude-usage: stdin read: {error}");
        return 0;
    }

    if let Ok(payload) = serde_json::from_str::<Value>(&input) {
        if let Some(sanitized) = sanitize(&payload) {
            if let Err(error) = write_cache(data_dir, &sanitized) {
                eprintln!("iyagi-termd claude-usage: cache write: {error}");
            }
        }
        if let Err(error) = write_model_cache(data_dir, &payload) {
            eprintln!("iyagi-termd claude-usage: model cache write: {error}");
        }
    }
    run_original_statusline(data_dir, input.as_bytes());
    0
}

fn sanitize(payload: &Value) -> Option<Value> {
    let limits = payload.get("rate_limits")?;
    let mut out = serde_json::Map::new();
    for name in ["five_hour", "seven_day"] {
        let Some(window) = limits.get(name) else {
            continue;
        };
        let Some(used) = window.get("used_percentage").and_then(Value::as_f64) else {
            continue;
        };
        if !used.is_finite() {
            continue;
        }
        out.insert(
            name.to_string(),
            json!({
                "usedPercentage": used.clamp(0.0, 100.0),
                "resetsAt": sanitized_reset(window.get("resets_at")),
            }),
        );
    }
    if out.is_empty() {
        return None;
    }
    Some(json!({
        "observedAt": unix_now(),
        "rateLimits": out,
    }))
}

fn sanitized_reset(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) => Value::String(text.clone()),
        Some(Value::Number(number)) if number.as_u64().is_some() => Value::Number(number.clone()),
        _ => Value::Null,
    }
}

fn write_cache(data_dir: &Path, value: &Value) -> std::io::Result<()> {
    let dir = data_dir.join("data");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let path = dir.join("subscription-claude.json");
    let tmp = dir.join("subscription-claude.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(tmp, path)
}

/// 세션별 모델 캐시가 이만큼 쌓이면 오래된 것부터 정리한다.
const MODEL_CACHE_PRUNE_AT: usize = 256;
/// 정리 대상: 이 기간 동안 갱신되지 않은 세션 캐시.
const MODEL_CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);

/// 세션별 모델·effort 스냅숏(`data/claude-model/<session>.json`) — 표시 이름과
/// effort 수준만 담는다. 내용이 같으면 다시 쓰지 않는다: 데몬은 수정 시각을
/// 변경 신호로 쓴다.
fn write_model_cache(data_dir: &Path, payload: &Value) -> std::io::Result<()> {
    let Some((session_id, observation)) = crate::agent_model::claude_statusline_model(payload)
    else {
        return Ok(());
    };
    if !term_contracts::agent_session::valid_session_id(&session_id) {
        return Ok(());
    }
    let path = crate::agent_model::statusline_cache_path(data_dir, &session_id);
    if let Some((_, previous)) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| crate::agent_model::parse_statusline_cache(&text))
    {
        if previous == observation {
            return Ok(());
        }
    }
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    prune_model_cache(dir);
    let value = crate::agent_model::statusline_cache_value(&observation, unix_now_ms());
    let tmp = dir.join(format!("{session_id}.tmp"));
    {
        let mut file = std::fs::File::create(&tmp)?;
        serde_json::to_writer(&mut file, &value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(tmp, path)
}

fn prune_model_cache(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let entries: Vec<_> = entries.flatten().collect();
    if entries.len() < MODEL_CACHE_PRUNE_AT {
        return;
    }
    let now = std::time::SystemTime::now();
    for entry in entries {
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > MODEL_CACHE_MAX_AGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn run_original_statusline(data_dir: &Path, input: &[u8]) {
    let backup = data_dir.join("config/claude-statusline-original.json");
    let Ok(text) = std::fs::read_to_string(backup) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let Some(command) = value.get("command").and_then(Value::as_str) else {
        return;
    };
    if command.contains("claude-usage") {
        return;
    }

    #[cfg(unix)]
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn();
    #[cfg(windows)]
    let mut child = std::process::Command::new("cmd")
        .args(["/D", "/S", "/C", command])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn();

    if let Ok(ref mut child) = child {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input);
        }
        let _ = child.wait();
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_subscription_windows() {
        let value = json!({
            "session_id": "secret-session",
            "cwd": "/private/repo",
            "rate_limits": {
                "five_hour": { "used_percentage": 27.5, "resets_at": "2026-09-09T12:00:00Z" },
                "seven_day": { "used_percentage": 101, "resets_at": null }
            }
        });
        let clean = sanitize(&value).unwrap();
        assert_eq!(clean["rateLimits"]["five_hour"]["usedPercentage"], 27.5);
        assert_eq!(clean["rateLimits"]["seven_day"]["usedPercentage"], 100.0);
        assert!(clean.get("session_id").is_none());
        assert!(!clean.to_string().contains("private"));
    }

    #[test]
    fn ignores_payload_without_rate_limits() {
        assert!(sanitize(&json!({"cwd": "/repo"})).is_none());
    }
}

#[cfg(test)]
mod model_cache_tests {
    use super::*;

    const SESSION: &str = "6d0d2c32-3575-4e13-a11d-c9dad4cb85e1";

    #[test]
    fn statusline_model_is_cached_per_session_without_rewriting_unchanged_snapshots() {
        let dir = std::env::temp_dir().join(format!("claude-usage-model-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let payload = json!({
            "session_id": SESSION,
            "model": { "id": "claude-opus-5[1m]", "display_name": "Opus 5 (1M context)" },
            "effort": { "level": "xhigh" },
            "transcript_path": "/private/transcript.jsonl",
            "cwd": "/private/cwd",
        });
        write_model_cache(&dir, &payload).unwrap();
        let path = crate::agent_model::statusline_cache_path(&dir, SESSION);
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("Opus 5 (1M context)") && first.contains("xhigh"));
        assert!(
            !first.contains("/private"),
            "경로·프롬프트는 저장하지 않는다"
        );

        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_model_cache(&dir, &payload).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified,
            "같은 스냅숏은 다시 쓰지 않는다 — 수정 시각이 곧 변경 신호다"
        );

        let switched = json!({
            "session_id": SESSION,
            "model": { "id": "claude-sonnet-5", "display_name": "Sonnet 5" },
            "effort": { "level": "high" },
        });
        write_model_cache(&dir, &switched).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("Sonnet 5"));

        // 세션 id가 경로를 벗어나려 하면 아무것도 쓰지 않는다.
        let hostile = json!({ "session_id": "../../escape", "model": { "id": "x" } });
        write_model_cache(&dir, &hostile).unwrap();
        assert!(!dir.join("escape.json").exists());
        assert!(!dir.join("data").join("escape.json").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
