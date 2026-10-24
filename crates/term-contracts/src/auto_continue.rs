//! 네트워크 끊김 자동 continue 계약: 세션 출력에서 API 연결 오류 문구를
//! 감지하면 데몬이 주기적으로 인터넷을 확인하고, 회복되면 그 세션에
//! `continue`를 입력한다(사용자가 주문한 자동회복 — 개입 채널의
//! "출력을 읽고 yes를 넣지 않는다" 원칙은 신뢰할 수 없는 hook 알림에
//! 대한 것이고, 여기는 보내는 입력이 `continue` 한 단어로 고정된다).
//!
//! 상태 전이는 `armed → fired | disarmed`뿐이다. fired/disarmed 뒤 같은
//! 오류 문구가 다시 출력되면 새 감지로 다시 armed가 된다(오류 한 번당
//! continue 한 번).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{SessionId, WorkloadId};

/// 감지·회복 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AutoContinueState {
    /// 오류 문구를 출력에서 보았다 — 네트워크 회복을 기다리는 중.
    Armed,
    /// 네트워크가 회복돼 `continue`를 보냈다.
    Fired,
    /// 세션 종료·사용자 직접 입력 등으로 감시를 걷었다(`reason` 참조).
    Disarmed,
}

/// `auto_continue.changed` 페이로드.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AutoContinuePayload {
    pub session_id: SessionId,
    pub workload_id: WorkloadId,
    pub state: AutoContinueState,
    /// disarmed 사유: `session-gone`·`guard-suspended`·`user-input` 등.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 상태가 바뀐 시각(unix epoch ms).
    pub at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&AutoContinueState::Disarmed).unwrap(),
            "\"disarmed\""
        );
    }

    #[test]
    fn payload_round_trips() {
        let payload = AutoContinuePayload {
            session_id: SessionId::generate(),
            workload_id: WorkloadId::generate(),
            state: AutoContinueState::Fired,
            reason: None,
            at_ms: 1_800_000_000_000,
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(!json.contains("\"reason\""), "None은 생략한다: {json}");
        let back: AutoContinuePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, payload);
    }
}
