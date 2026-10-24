//! 예약 명령(scheduled command) 계약: 사용자가 특정 세션에 지정 시각에
//! 입력될 명령을 데몬에 맡긴다. 데몬이 예약표를 소유하므로 창을 닫아도
//! 예약은 살아 있고(`session.input`의 view/epoch 검증과 무관하게 actor
//! 경로로 직접 쓴다), 한 번 실행하면 끝난다.
//!
//! 계약 원칙:
//! * 명령 텍스트는 사용자가 직접 입력한 값이다 — 잘라 쓰지 않고 상한을
//!   넘으면 통째로 거절한다(intervention 채널 관례와 같다).
//! * 시각은 unix epoch ms(`u64`)로만 다닌다. 로컬 시각 해석은 UI 몫이다.
//! * 상태 전이는 `pending → firing → fired|failed|skipped`와 pending에서만
//!   갈 수 있는 `cancelled`뿐이다. 취소와 발사가 같은 행을 두고 다투지
//!   않도록 클레임(`pending→firing`)이 원자적으로 일어난다.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{ScheduleId, SessionId};

/// 예약 상태. 저장 스키마의 CHECK 목록과 문자열이 정확히 일치해야 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledCommandStatus {
    /// 발사 대기.
    Pending,
    /// 루프가 클레임했고 쓰기 중(또는 재시도 대기). 재시도는 pending으로
    /// 되돌아가지 않고 이 상태에서 `fire_at_ms`를 앞당겨 재시도한다.
    Firing,
    /// PTY에 썼다.
    Fired,
    /// 재시도 상한 안에 쓰지 못했다.
    Failed,
    /// 대상 세션·가드 등의 사정으로 실행하지 않았다(`reason_code` 참조).
    Skipped,
    /// 사용자가 pending 상태에서 취소했다.
    Cancelled,
}

/// `schedule.fired` 이벤트의 실행 결과. 상태 머신의 종착점 세 개만
/// 이벤트로 흐른다(pending/firing/cancelled은 이벤트가 없다).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleOutcome {
    Fired,
    Failed,
    Skipped,
}

/// `schedule.fired` 페이로드. UI는 이것으로 토스트·알림 센터를 채운다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleFiredPayload {
    pub schedule_id: ScheduleId,
    pub session_id: SessionId,
    pub outcome: ScheduleOutcome,
    /// skipped/failed의 이유(`session-gone`·`guard-suspended`·
    /// `session-exited`·`write-failed`·`daemon-restart`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// 실행한(또는 실패한) 명령 텍스트 원문.
    pub command: String,
    pub fired_at_ms: u64,
}

/// `schedule.create` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleCreateParams {
    pub session_id: SessionId,
    /// PTY에 타이핑될 명령. `\n`은 발사 시 `\r`로 변환되고, 끝에 실행
    /// Enter가 붙는다.
    pub command: String,
    /// 발사 시각(unix epoch ms). 생성 시각보다 미래여야 한다.
    pub fire_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleCreateResult {
    pub schedule_id: ScheduleId,
}

/// `schedule.list` params — 세션 좁힘 없이 최근 전체 목록.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// 저장된 예약 한 건. 목록 응답의 항목이기도 하다(명령 텍스트는 응답
/// 프레임 예산 안에 들어가도록 데몬이 잘라낼 수 있다 — 원문은 데몬이
/// 갖고 있다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduledCommand {
    pub schedule_id: ScheduleId,
    pub session_id: SessionId,
    pub command: String,
    pub fire_at_ms: u64,
    pub status: ScheduledCommandStatus,
    /// 지금까지의 쓰기 시도 횟수.
    pub attempts: u32,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleListResult {
    pub items: Vec<ScheduledCommand>,
}

/// `schedule.cancel` params — pending만 취소된다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleCancelParams {
    pub schedule_id: ScheduleId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleCancelResult {
    /// 이미 발사했거나 종착점에 다다른 예약이면 false(에러가 아니다).
    pub cancelled: bool,
}

/// 필드 상한·재시도 정책. 데몬과 UI가 같은 값을 쓴다.
pub mod limits {
    /// PTY 입력 청크 상한(4 KiB)에서 변환 여유를 뺀 명령 원문 상한.
    pub const COMMAND_MAX_BYTES: usize = 4_000;
    /// 발사 시각의 최대 horizon(1년). 멀리 있는 예약은 지원하지 않는다.
    pub const MAX_HORIZON_MS: u64 = 365 * 24 * 60 * 60 * 1_000;
    /// 쓰기 재시도 상한(입력 큐가 차 있거나 PTY가 멈춰 있을 때). 재시도는
    /// 이 횟수로만 묶인다 — 시도마다 새 input_id로 완전히 새 쓰기다.
    pub const MAX_ATTEMPTS: u32 = 3;
    /// 재시도 간격.
    pub const RETRY_DELAY_MS: u64 = 2_000;
    /// `schedule.list` 응답 프레임의 바이트 예산(agent_session.list 관례).
    pub const LIST_BUDGET_BYTES: usize = 48 * 1024;
    /// `schedule.list`가 저장소에서 가져오는 최대 건수.
    pub const LIST_ROW_LIMIT: i64 = 200;
}

impl ScheduleCreateParams {
    /// 검증: 명령은 1..=COMMAND_MAX_BYTES 바이트에 NUL이 없어야 하고,
    /// 발사 시각은 (now, now + 1년] 안에 있어야 한다. 잘라내지 않는다.
    pub fn validate(&self, now_ms: u64) -> Result<(), &'static str> {
        use limits::*;
        if self.command.is_empty() {
            return Err("command must not be empty");
        }
        if self.command.len() > COMMAND_MAX_BYTES {
            return Err("command exceeds 4000 bytes");
        }
        if self.command.contains('\0') {
            return Err("command must not contain NUL");
        }
        if self.fire_at_ms <= now_ms {
            return Err("fire_at_ms must be in the future");
        }
        if self.fire_at_ms.saturating_sub(now_ms) > MAX_HORIZON_MS {
            return Err("fire_at_ms exceeds the 1-year horizon");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(command: &str, fire_at_ms: u64) -> ScheduleCreateParams {
        ScheduleCreateParams {
            session_id: SessionId::generate(),
            command: command.into(),
            fire_at_ms,
        }
    }

    #[test]
    fn create_params_round_trip() {
        let p = params("cargo test\r\n", 1_800_000_000_000);
        let json = serde_json::to_string(&p).unwrap();
        let back: ScheduleCreateParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        assert!(json.contains("\"fire_at_ms\":1800000000000"));
    }

    #[test]
    fn validate_rejects_empty_oversize_nul_and_past_or_far_times() {
        let now = 1_700_000_000_000u64;
        assert!(params("", now + 60_000).validate(now).is_err());
        assert!(params(&"x".repeat(4_001), now + 60_000)
            .validate(now)
            .is_err());
        assert!(params("a\0b", now + 60_000).validate(now).is_err());
        assert!(params("ls", now).validate(now).is_err());
        assert!(params("ls", now - 1).validate(now).is_err());
        assert!(params("ls", now + limits::MAX_HORIZON_MS + 1)
            .validate(now)
            .is_err());
        assert!(params("ls", now + 60_000).validate(now).is_ok());
    }

    #[test]
    fn status_and_outcome_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&ScheduledCommandStatus::Skipped).unwrap(),
            "\"skipped\""
        );
        assert_eq!(
            serde_json::to_string(&ScheduleOutcome::Fired).unwrap(),
            "\"fired\""
        );
    }
}
