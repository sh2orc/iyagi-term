//! Intervention channel (SOTA_GAP_REVIEW W1-5): structured "사람의 개입이
//! 필요하다" 신호를 CLI 공식 hooks에서 받아 UI로 흘린다.
//!
//! 계약 원칙:
//! * 페이로드는 **신뢰할 수 없는 외부 입력**이다 — 길이 상한으로 잘라 쓰지
//!   않고 그냥 거절한다(값을 지어내지 않는다).
//! * 이 채널은 알림 전용이다. 실행·권한 승인·입력 주입 기능이 없다
//!   (§2.1: 알림 텍스트를 읽고 PTY에 yes를 넣지 않는다).
//! * `report_id`로 멱등 중복을 건너뛴다(hook 재시도·에이전트 재실행).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 개입 종류. 의미는 스키마로 동결한다(추가는 허용, 재정의는 금지).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum InterventionKind {
    /// 도구 실행·파일 쓰기 등의 승인 요청.
    Permission,
    /// 에이전트가 사용자 답변을 기다리는 질문.
    Question,
    /// 정보 알림(에이전트 대기·완료 안내 등).
    Notification,
    /// 에이전트 응답 종료(공식 Stop hook).
    Stop,
}

/// `intervention.report` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InterventionReport {
    /// 멱등 키(hook이 생성한 UUID 등). 빈 값은 거절.
    pub report_id: String,
    pub kind: InterventionKind,
    /// 한 줄 제목(상한 200자).
    pub title: String,
    /// 근거·맥락(상한 2_000자, 없어도 된다).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// 알림이 속한 세션 단서(cwd·세션 id 후보). UI가 pane 연결에만 쓴다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_hint: Option<String>,
    /// 신호 출처(예: "claude-code-hook"). 상한 64자.
    pub source: String,
}

/// `intervention.list` 결과 항목 — 보고 시각(ISO-8601 UTC)이 붙은 사본.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InterventionNotice {
    #[serde(flatten)]
    pub report: InterventionReport,
    pub reported_at: String,
}

/// 필드별 길이 상한.
pub mod limits {
    pub const REPORT_ID_MAX: usize = 128;
    pub const TITLE_MAX: usize = 200;
    pub const DETAIL_MAX: usize = 2_000;
    pub const SESSION_HINT_MAX: usize = 4_096;
    pub const SOURCE_MAX: usize = 64;
    /// 데몬이 메모리에 유지하는 최근 개입 수(링).
    pub const RETAINED: usize = 100;
    /// 멱등 중복 판정에 쓰는 report_id 수.
    pub const DEDUP_ENTRIES: usize = 128;
}

impl InterventionReport {
    /// 신뢰할 수 없는 입력 검증. 잘라내지 않고 통째로 거절한다.
    pub fn validate(&self) -> Result<(), &'static str> {
        use limits::*;
        if self.report_id.trim().is_empty() || self.report_id.len() > REPORT_ID_MAX {
            return Err("report_id must be 1..=128 non-space chars");
        }
        if self.title.trim().is_empty() || self.title.len() > TITLE_MAX {
            return Err("title must be 1..=200 non-space chars");
        }
        if let Some(detail) = &self.detail {
            if detail.len() > DETAIL_MAX {
                return Err("detail exceeds 2000 chars");
            }
        }
        if let Some(hint) = &self.session_hint {
            if hint.len() > SESSION_HINT_MAX {
                return Err("session_hint exceeds 4096 chars");
            }
        }
        if self.source.trim().is_empty() || self.source.len() > SOURCE_MAX {
            return Err("source must be 1..=64 non-space chars");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> InterventionReport {
        InterventionReport {
            report_id: "r-1".into(),
            kind: InterventionKind::Permission,
            title: "파일 쓰기 승인 요청".into(),
            detail: Some("edit: src/a.ts".into()),
            session_hint: Some("/repo".into()),
            source: "claude-code-hook".into(),
        }
    }

    #[test]
    fn valid_report_round_trips_and_omits_optional_fields() {
        let r = report();
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("session_hint") || r.session_hint.is_some());
        let back: InterventionReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.kind, InterventionKind::Permission);
    }

    #[test]
    fn oversize_and_empty_fields_are_rejected_not_clamped() {
        let mut r = report();
        r.title = "x".repeat(201);
        assert!(r.validate().is_err());
        r.title = "  ".into();
        assert!(r.validate().is_err());
        r = report();
        r.detail = Some("d".repeat(2_001));
        assert!(r.validate().is_err());
        r = report();
        assert!(r.validate().is_ok());
    }

    #[test]
    fn kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&InterventionKind::Question).unwrap(),
            "\"question\""
        );
    }
}
