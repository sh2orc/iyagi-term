//! Structured provider rejection windows. Never scrape messages or store headers.
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use term_contracts::{ids::U64String, mission::types::RateLimitObservation};

pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(U64String::MAX as u128) as u64
}

fn observation(now_ms: u64, resets_ms: u64) -> Option<RateLimitObservation> {
    let value = RateLimitObservation {
        observed_at_unix_ms: U64String::new(now_ms).ok()?,
        resets_at_unix_ms: U64String::new(resets_ms).ok()?,
    };
    term_core::mission::rate_limits::valid_observation(&value).then_some(value)
}

pub fn claude(value: &Value, now_ms: u64) -> Option<RateLimitObservation> {
    let info = &value["rate_limit_info"];
    if info["status"] != "rejected" {
        return None;
    }
    observation(now_ms, info["resetsAt"].as_u64()?.checked_mul(1000)?)
}

pub fn codex(value: &Value, now_ms: u64) -> Option<RateLimitObservation> {
    let limits = &value["rateLimits"];
    // An informational percentage, missing sparse field, or available credits
    // alone does not establish a rejection for this connection.
    let mut deadline = None;
    if limits["rateLimitReachedType"] == "rate_limit_reached" {
        for window in ["primary", "secondary"] {
            if limits[window]["usedPercent"]
                .as_u64()
                .is_some_and(|n| n >= 100)
            {
                if let Some(ms) = limits[window]["resetsAt"]
                    .as_u64()
                    .and_then(|n| n.checked_mul(1000))
                {
                    deadline = Some(deadline.map_or(ms, |old: u64| old.max(ms)));
                }
            }
        }
    }
    if limits["spendControlReached"] == true && limits["individualLimit"]["remainingPercent"] == 0 {
        if let Some(ms) = limits["individualLimit"]["resetsAt"]
            .as_u64()
            .and_then(|n| n.checked_mul(1000))
        {
            deadline = Some(deadline.map_or(ms, |old| old.max(ms)));
        }
    }
    observation(now_ms, deadline?)
}

pub fn opencode(error: &Value, now_ms: u64) -> Option<RateLimitObservation> {
    if error["name"] != "APIError" || error["data"]["statusCode"] != 429 {
        return None;
    }
    let headers = error["data"]["responseHeaders"].as_object()?;
    let values: Vec<_> = headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("retry-after"))
        .collect();
    if values.len() != 1 {
        return None;
    }
    let value = values[0].1.as_str()?.trim();
    if value.len() > 128 {
        return None;
    }
    let deadline = if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        now_ms.checked_add(value.parse::<u64>().ok()?.checked_mul(1000)?)?
    } else {
        httpdate::parse_http_date(value)
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis()
            .try_into()
            .ok()?
    };
    observation(now_ms, deadline)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_requires_rejection_and_a_valid_future_reset() {
        for status in ["allowed", "allowed_warning", "other"] {
            assert!(claude(&json!({"rate_limit_info":{"status":status,"resetsAt":20,"overageStatus":"rejected"}}),1000).is_none());
        }
        assert_eq!(
            claude(
                &json!({"rate_limit_info":{"status":"rejected","resetsAt":20}}),
                1000
            )
            .unwrap()
            .resets_at_unix_ms
            .get(),
            20_000
        );
        for reset in [
            Value::Null,
            json!(0),
            json!(-1),
            json!("20"),
            json!(u64::MAX),
        ] {
            assert!(claude(
                &json!({"rate_limit_info":{"status":"rejected","resetsAt":reset}}),
                1000
            )
            .is_none());
        }
    }

    #[test]
    fn codex_sparse_percentages_do_not_create_or_shorten_a_known_hold() {
        let mut value = json!({"rateLimits":{"primary":{"usedPercent":100,"resetsAt":10},"secondary":{"usedPercent":100,"resetsAt":20}}});
        assert!(codex(&value, 1000).is_none());
        value["rateLimits"]["rateLimitReachedType"] = json!("rate_limit_reached");
        assert_eq!(codex(&value, 1000).unwrap().resets_at_unix_ms.get(), 20_000);
        value["rateLimits"]["secondary"] = Value::Null;
        assert_eq!(codex(&value, 1000).unwrap().resets_at_unix_ms.get(), 10_000);
        assert!(codex(&value, 10_000).is_none());
        assert!(codex(&json!({"rateLimits":{"primary":null}}), 1000).is_none());
        let spending = json!({"rateLimits":{"spendControlReached":true,"individualLimit":{"remainingPercent":0,"resetsAt":30}}});
        assert_eq!(
            codex(&spending, 1000).unwrap().resets_at_unix_ms.get(),
            30_000
        );
    }

    #[test]
    fn retry_after_is_status_scoped_case_insensitive_and_accepts_http_dates() {
        let mut error = json!({"name":"APIError","data":{"statusCode":429,"responseHeaders":{"Retry-After":" 12 ","authorization":"fixture-secret"}}});
        assert_eq!(
            opencode(&error, 1000).unwrap().resets_at_unix_ms.get(),
            13_000
        );
        error["data"]["responseHeaders"]["Retry-After"] = json!("Thu, 01 Jan 1970 00:00:20 GMT");
        assert_eq!(
            opencode(&error, 1000).unwrap().resets_at_unix_ms.get(),
            20_000
        );
        for value in ["", "-2", "1.5", "tomorrow", "18446744073709551615"] {
            error["data"]["responseHeaders"]["Retry-After"] = json!(value);
            assert!(opencode(&error, 1000).is_none());
        }
        error["data"]["responseHeaders"]["Retry-After"] = json!("20");
        error["data"]["statusCode"] = json!(500);
        assert!(opencode(&error, 1000).is_none());
        error["data"]["statusCode"] = json!(429);
        error["data"]["responseHeaders"]["retry-after"] = json!("30");
        assert!(opencode(&error, 1000).is_none());
    }
}
