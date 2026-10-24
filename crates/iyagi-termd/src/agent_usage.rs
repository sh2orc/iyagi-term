//! Claude Code 상태줄 `rate_limits` 파싱과 세션별 사용량 한도 캐시
//! (`data/claude-limits/<session>.json`). `agent_model`과 같은 분리 구조다:
//! 순수 파서·경로·캐시 직렬화만 여기 있고, 읽고 쓰는 쪽(상태줄 브리지
//! `claude_usage.rs`·와처 `model_watch.rs`)이 가져다 쓴다.
//!
//! 단위는 여기서 정규화해 끝낸다 — 캐시 파일과 [`AgentUsageLimits`]는
//! 항상 unix ms·반올림 u8 퍼센트다. CLI는 `resets_at`을 초 단위 숫자로
//! 주지만(공식 문서), 이전 관측에서 ms로 온 적이 있어 둘 다 견딘다.
//! 파서 자체는 시계를 모른다: `now_ms`를 받아 스냅숏에만 찍는다.

use std::path::{Path, PathBuf};

use serde_json::Value;
use term_contracts::snapshot::{AgentUsageLimitWindow, AgentUsageLimits};

/// 이 값부터는 초 단위로 볼 수 없다 — 이미 ms다(2001-09-09 이후).
const SECONDS_VS_MS_BOUNDARY: u64 = 1_000_000_000_000;

/// 상태줄 페이로드의 세션 id(Claude 자체 세션 — `claude --resume` 인자).
pub fn session_id_of(payload: &Value) -> Option<&str> {
    payload.get("session_id").and_then(Value::as_str)
}

/// 상태줄 페이로드의 `rate_limits`를 정규화된 스냅숏으로 바꾼다.
/// 어느 창도 건네지 않으면 `None` — 이것은 "한도 상태를 지운다"는 뜻으로
/// 쓰는 쪽(브리지)에서 무시하는 것과 다르게 취급해야 한다.
pub fn statusline_usage_limits(payload: &Value, now_ms: u64) -> Option<AgentUsageLimits> {
    let limits = payload.get("rate_limits")?;
    let five_hour = window(limits.get("five_hour"));
    let seven_day = window(limits.get("seven_day"));
    if five_hour.is_none() && seven_day.is_none() {
        return None;
    }
    Some(AgentUsageLimits {
        five_hour,
        seven_day,
        observed_at_unix_ms: now_ms,
    })
}

/// 창 하나. `used_percentage`가 숫자가 아니거나 유한하지 않으면 창 전체를
/// 버린다(값을 지어내지 않는다). `resets_at`이 없어도 창은 살린다 —
/// "100%인데 리셋 시각 모름"이 실재하는 관측이기 때문이다.
fn window(value: Option<&Value>) -> Option<AgentUsageLimitWindow> {
    let value = value?;
    let used = value.get("used_percentage").and_then(Value::as_f64)?;
    if !used.is_finite() {
        return None;
    }
    Some(AgentUsageLimitWindow {
        used_percent: used.clamp(0.0, 100.0).round() as u8,
        resets_at_unix_ms: resets_at_ms(value.get("resets_at")),
    })
}

/// `resets_at` 정규화: 초 단위 숫자는 ms로, ms는 그대로, ISO-8601 문자열은
/// 파싱, 그 외(0·null·다른 타입)는 없음. 0은 "모른다"는 뜻으로 쓰이므로
/// 거짓 카운트다운의 씨앗이 되지 않게 버린다.
fn resets_at_ms(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::Number(number)) => match number.as_u64()? {
            0 => None,
            seconds if seconds < SECONDS_VS_MS_BOUNDARY => seconds.checked_mul(1000),
            milliseconds => Some(milliseconds),
        },
        Some(Value::String(text)) => crate::agent_model::iso8601_utc_ms(text),
        _ => None,
    }
}

/// 세션별 한도 캐시 경로: `<data_dir>/data/claude-limits/<session>.json`.
pub fn limits_cache_path(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir
        .join("data")
        .join("claude-limits")
        .join(format!("{session_id}.json"))
}

/// 캐시 값은 계약 타입을 그대로 직렬화한다 — 포맷이 곧 타입이므로
/// 조립 코드가 스키마와 어긋날 여지가 없다.
pub fn parse_limits_cache(text: &str) -> Option<AgentUsageLimits> {
    serde_json::from_str(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW_MS: u64 = 1_790_000_000_000;

    #[test]
    fn normalizes_percentages_and_units() {
        let payload = json!({
            "session_id": "s",
            "rate_limits": {
                "five_hour": { "used_percentage": 27.5, "resets_at": 1_790_000_000u64 },
                "seven_day": { "used_percentage": 101, "resets_at": 1_790_000_000_000u64 }
            }
        });
        let limits = statusline_usage_limits(&payload, NOW_MS).unwrap();
        assert_eq!(
            limits.five_hour,
            Some(AgentUsageLimitWindow {
                used_percent: 28,
                resets_at_unix_ms: Some(1_790_000_000_000)
            })
        );
        assert_eq!(
            limits.seven_day,
            Some(AgentUsageLimitWindow {
                used_percent: 100,
                resets_at_unix_ms: Some(1_790_000_000_000)
            })
        );
        assert_eq!(limits.observed_at_unix_ms, NOW_MS);
    }

    #[test]
    fn accepts_iso_strings_and_treats_zero_and_junk_as_unknown() {
        let payload = json!({
            "rate_limits": {
                "five_hour": { "used_percentage": 100, "resets_at": "2026-09-09T12:00:00Z" },
                "seven_day": { "used_percentage": 40, "resets_at": 0 }
            }
        });
        let limits = statusline_usage_limits(&payload, NOW_MS).unwrap();
        assert_eq!(
            limits.five_hour.unwrap().resets_at_unix_ms,
            Some(1_788_955_200_000)
        );
        assert_eq!(limits.seven_day.unwrap().resets_at_unix_ms, None);

        for bad in [
            json!(null),
            json!("not a date"),
            json!(-5),
            json!("1_790_000_000"),
        ] {
            assert_eq!(resets_at_ms(Some(&bad)), None, "bad: {bad}");
        }
    }

    #[test]
    fn non_finite_or_missing_percentages_drop_the_window() {
        let payload = json!({
            "rate_limits": {
                "five_hour": { "used_percentage": null, "resets_at": 100 },
                "seven_day": {}
            }
        });
        assert!(statusline_usage_limits(&payload, NOW_MS).is_none());

        let only_good = json!({
            "rate_limits": { "seven_day": { "used_percentage": 50 } }
        });
        let limits = statusline_usage_limits(&only_good, NOW_MS).unwrap();
        assert!(limits.five_hour.is_none());
        assert_eq!(limits.seven_day.unwrap().used_percent, 50);
    }

    #[test]
    fn cache_path_is_session_scoped_and_parses_round_trip() {
        let path = limits_cache_path(Path::new("/data"), "abc");
        assert_eq!(path, Path::new("/data/data/claude-limits/abc.json"));
        let limits = statusline_usage_limits(
            &json!({ "rate_limits": { "five_hour": { "used_percentage": 100, "resets_at": 5 } } }),
            7,
        )
        .unwrap();
        let text = serde_json::to_string(&limits).unwrap();
        assert_eq!(parse_limits_cache(&text).unwrap(), limits);
        assert!(parse_limits_cache("not json").is_none());
    }
}
