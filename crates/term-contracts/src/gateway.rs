//! 로컬 관측 게이트웨이 계약(`gateway.status` / `gateway.set_policy`).
//!
//! 데몬(`iyagi-termd`)이 라우팅된 Claude Code pane의 업스트릭 트래픽을
//! 루프백 프록시로 감쌀 때 주고받는 형태다. 토큰·키 같은 비밀은 어디에도
//! 없다 — 상태는 워크로드/세션 id와 사용량만 싣는다.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{SessionId, WorkloadId};

/// 키 풀 회전 전략(09 §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum GatewayPoolStrategy {
    /// 새 세션 발급마다 순서대로 순환(기본).
    RoundRobin,
    /// 순서상 첫 사용 가능 키에 붙어 있다가 임계에 걸리면 다음으로.
    FillFirst,
}

/// `gateway.set_policy` params. 전부 생략 가능하고, 지정한 것만 바뀐다.
/// * `enabled` — 관측 라우팅 on/off(새 라우팅만 영향).
/// * `blocked` — 킬 스위치(true면 모든 요청 403).
/// * `pool_strategy` — API 키 풀 회전 전략.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GatewayPolicyParams {
    pub enabled: Option<bool>,
    pub blocked: Option<bool>,
    pub pool_strategy: Option<GatewayPoolStrategy>,
}

/// `gateway.set_policy` 결과(적용된 정책 전체).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GatewayPolicy {
    pub enabled: bool,
    pub blocked: bool,
    pub pool_strategy: GatewayPoolStrategy,
}

/// 풀 키 하나의 요약 상태(비밀 없음 — id·라벨·상태만).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PoolKeyStatus {
    pub id: String,
    pub label: String,
    pub state: PoolKeyViewState,
    /// 쿨다운 남은 밀리초(쿨다운 중이 아니면 null).
    pub cooldown_remaining_ms: Option<u64>,
    /// 이 키가 답한 요청 수.
    pub requests: u64,
    pub failures: u64,
    pub last_used_epoch_ms: Option<u64>,
}

/// [`PoolKeyStatus::state`]의 값.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum PoolKeyViewState {
    /// 등록됐지만 아직 한 번도 안 쓰였다.
    Unused,
    /// 사용 중(최근 시도 있음, 제한 없음).
    Active,
    /// 429/전송 실패로 잠시 제외(시각 경과로 복귀).
    Cooling,
    /// 401/403 — 사람이 키를 다시 등록해야 한다.
    Quarantined,
}

/// 토큰 사용량. `requests`는 응답이 완결된 요청 수, `failures`는 그중
/// 전송 실패·업스트림 오류(4xx/5xx)인 수.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GatewayUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub requests: u64,
    pub failures: u64,
}

/// 게이트웨이를 경유 중인 세션 하나(워크로드당 하나).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GatewaySessionUsage {
    pub workload_id: WorkloadId,
    pub session_id: SessionId,
    pub usage: GatewayUsage,
    /// 이 세션이 마지막으로 쓴 모델(응답에서 관측; 아직 없으면 null).
    pub model: Option<String>,
    pub started_epoch_ms: u64,
    pub last_seen_epoch_ms: u64,
}

/// `gateway.status` 결과.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GatewayStatus {
    /// 관측 라우팅 사용 여부(사용자 설정).
    pub enabled: bool,
    /// 서버가 실제로 수신 중인가(바인드 실패·미기동이면 false).
    pub listening: bool,
    pub port: u16,
    /// 킬 스위치(모든 요청 차단).
    pub blocked: bool,
    /// 데몬 수명 전체 누적(세션 정리와 무관).
    pub totals: GatewayUsage,
    /// 현재 장부에 남아 있는 세션별 사용량(id 정렬).
    pub sessions: Vec<GatewaySessionUsage>,
    /// API 키 풀 상태(09 §3.4, 비밀 없음). 풀이 비면 빈 목록.
    pub pool: Vec<PoolKeyStatus>,
    /// 현재 회전 전략.
    pub pool_strategy: GatewayPoolStrategy,
}

/// 게이트웨이가 발급하는 세션 토큰의 접두사. 비밀 값 자체는 상태·로그에
/// 절대 나오지 않고, 로그 판별 등에 쓰이는 형식 마커만 공유한다.
pub const GATEWAY_TOKEN_PREFIX: &str = "iyagi-gw-";
