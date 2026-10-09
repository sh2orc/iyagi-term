//! UTC ISO-8601 timestamps for TEXT columns.
//!
//! Format `YYYY-MM-DDTHH:MM:SS.sssZ` is fixed width, so lexicographic
//! `ORDER BY` matches chronological order, and it matches the rows the schema
//! file itself produces via `strftime('%Y-%m-%dT%H:%M:%fZ','now')`. Wall-clock
//! time is used only for display/ordering columns; the daemon's runtime
//! decisions stay on its monotonic clock (spec `01-contracts.md` §1).

use std::time::{SystemTime, UNIX_EPOCH};

/// Current wall clock as `YYYY-MM-DDTHH:MM:SS.sssZ`.
pub fn now_iso8601() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    iso8601_from_unix(dur.as_secs() as i64, dur.subsec_millis())
}

/// `days`일 전의 벽시계 시각. 보존 정리의 하한 비교에 쓴다 — 열이 고정폭
/// ISO-8601이라 문자열 `<` 비교가 곧 시간 비교다.
pub(crate) fn iso8601_days_ago(days: u32) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let cutoff = (now.as_secs() as i64).saturating_sub(i64::from(days).saturating_mul(86_400));
    iso8601_from_unix(cutoff, now.subsec_millis())
}

pub fn iso8601_from_unix(secs: i64, millis: u32) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 -> (y, m, d).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_formats() {
        assert_eq!(iso8601_from_unix(0, 0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn known_instants_format() {
        // Well-known epoch values (leap-year day included).
        assert_eq!(
            iso8601_from_unix(951_782_400, 0),
            "2000-02-29T00:00:00.000Z"
        );
        assert_eq!(
            iso8601_from_unix(1_700_000_000, 0),
            "2023-11-14T22:13:20.000Z"
        );
    }

    #[test]
    fn millis_are_zero_padded() {
        assert_eq!(iso8601_from_unix(1, 7), "1970-01-01T00:00:01.007Z");
    }

    #[test]
    fn days_ago_is_in_the_past_and_same_shape() {
        let now = now_iso8601();
        let today = iso8601_days_ago(0);
        let long_ago = iso8601_days_ago(90);
        assert_eq!(now.len(), long_ago.len());
        assert!(long_ago < today);
        // 0일 전은 지금과 같은 날짜다(밀리초 차이만).
        assert_eq!(today[..10], now[..10]);
        // 과도한 일수도 패닉 없이 하한으로 수렴한다.
        let _ = iso8601_days_ago(u32::MAX);
    }

    #[test]
    fn format_is_fixed_width_and_sortable() {
        let a = iso8601_from_unix(1, 2);
        let b = iso8601_from_unix(100_000, 999);
        assert_eq!(a.len(), b.len());
        assert!(a < b);
    }
}
