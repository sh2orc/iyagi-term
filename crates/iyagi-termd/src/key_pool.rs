//! API 키 회전 엔진 — 09-key-pool-rotation §3.
//!
//! opencodex의 계정 회전 상태머신을 **API 키 축**으로 축소한 것이다:
//! 같은 뼈대(세션 친화성 · 쿨다운 · 격리 · 사전 스트림 1회 페일오버)를
//! 쓰되, 구독 창이 없으므로 사용량 백분율 점수·프로브 리스 같은 것은 없다.
//! 쿨다운 해제는 시각 경과뿐 — API 키는 유기적 2xx로 회복을 증명할 필요가
//! 없다(09 §3.3).
//!
//! 순수 로직만 담는다(디스크 I/O·시계 생성 없음). 시계는 `now_ms` 주입,
//! 저장소 읽기는 [`Gateway`](crate::gateway::Gateway)가 키 조회 시점마다
//! 한다 — 추가·삭제가 즉시 반영되고 이 모듈은 상태(회전·건강)만 소유한다.

use std::collections::BTreeMap;

use term_contracts::gateway::{GatewayPoolStrategy, PoolKeyStatus, PoolKeyViewState};

/// 쿨다운 기본값(429에 `Retry-After`가 없을 때).
const DEFAULT_COOLDOWN_MS: u64 = 60_000;
/// 쿨다운 상한 — 제공자가 아무리 긴 값을 줘도 이 이상은 잠그지 않는다.
const MAX_COOLDOWN_MS: u64 = 24 * 60 * 60_000;
/// 연속 전송 실패(5xx·연결 오류)로 쿨다운에 들어가는 문턱.
const TRANSPORT_FAILURE_THRESHOLD: u32 = 3;
/// 전송 실패 쿨다운(소프트 회피).
const TRANSPORT_COOLDOWN_MS: u64 = 30_000;

/// 키 하나의 회전 상태. 비밀은 여기 없다 — id만 다룬다(08 §5 불변).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolKeyState {
    pub id: String,
    pub label: String,
    pub cooldown_until_ms: Option<u64>,
    pub quarantined: bool,
    pub consecutive_failures: u32,
    pub requests: u64,
    pub failures: u64,
    pub last_used_epoch_ms: Option<u64>,
}

impl PoolKeyState {
    fn available(&self, now_ms: u64) -> bool {
        !self.quarantined_or_cooling(now_ms)
    }

    fn quarantined_or_cooling(&self, now_ms: u64) -> bool {
        self.quarantined || self.cooldown_until_ms.is_some_and(|until| now_ms < until)
    }
}

/// 회전 엔진. 키 목록은 [`Self::sync`]로 저장소 스냅숏과 맞춘다 — 사라진
/// 키의 상태는 버리고(격리 플래그도 함께), 새 키는 `unused`로 들어온다.
pub struct KeyPool {
    strategy: GatewayPoolStrategy,
    /// 등록순 id 목록(선택 순서의 진실 원본).
    order: Vec<String>,
    states: BTreeMap<String, PoolKeyState>,
    /// round_robin 커서(order 인덱스). 성공한 발급에만 전진한다.
    rr_cursor: usize,
}

impl KeyPool {
    pub fn new(strategy: GatewayPoolStrategy) -> Self {
        KeyPool {
            strategy,
            order: Vec::new(),
            states: BTreeMap::new(),
            rr_cursor: 0,
        }
    }

    pub fn strategy(&self) -> GatewayPoolStrategy {
        self.strategy
    }

    pub fn set_strategy(&mut self, strategy: GatewayPoolStrategy) {
        self.strategy = strategy;
    }

    /// 저장소 스냅숏(`(id, label)` 등록순)과 상태를 맞춘다.
    pub fn sync(&mut self, snapshot: &[(String, String)]) {
        let live: std::collections::BTreeSet<&str> =
            snapshot.iter().map(|(id, _)| id.as_str()).collect();
        self.states.retain(|id, _| live.contains(id.as_str()));
        for (id, label) in snapshot {
            self.states
                .entry(id.clone())
                .or_insert_with(|| PoolKeyState {
                    id: id.clone(),
                    label: label.clone(),
                    cooldown_until_ms: None,
                    quarantined: false,
                    consecutive_failures: 0,
                    requests: 0,
                    failures: 0,
                    last_used_epoch_ms: None,
                });
        }
        self.order = snapshot.iter().map(|(id, _)| id.clone()).collect();
        self.rr_cursor = self.rr_cursor.min(self.order.len().saturating_sub(1));
    }

    /// 진행 중 세션이 묶은 키를 검증한다: 풀에 있고 사용 가능하면 그대로,
    /// 없어졌거나(삭제) 쓸 수 없으면(쿨다운·격리) [`Self::bind_new`]으로
    /// 갈아탄다. 친화성은 "가능한 한 유지"다(09 §3.2).
    pub fn rebind(&mut self, bound: Option<&str>, now_ms: u64) -> Option<String> {
        if let Some(id) = bound {
            if self
                .states
                .get(id)
                .is_some_and(|state| state.available(now_ms))
            {
                return Some(id.to_string());
            }
        }
        self.bind_new(now_ms)
    }

    /// 새 세션 발급용 키 선택. 사용 가능한 키가 없으면 `None`(호출자는
    /// 단일 키 경로로 폴백한다).
    pub fn bind_new(&mut self, now_ms: u64) -> Option<String> {
        if self.order.is_empty() {
            return None;
        }
        match self.strategy {
            GatewayPoolStrategy::FillFirst => self
                .order
                .iter()
                .find(|id| self.states[*id].available(now_ms))
                .cloned(),
            GatewayPoolStrategy::RoundRobin => {
                // 커서부터 순환하며 첫 사용 가능 키. 커서는 이 발급이
                // 성공적으로 쓰일 때(record_success)만 다음으로 전진한다.
                for step in 0..self.order.len() {
                    let index = (self.rr_cursor + step) % self.order.len();
                    let id = &self.order[index];
                    if self.states[id].available(now_ms) {
                        return Some(id.clone());
                    }
                }
                None
            }
        }
    }

    /// 발급이 실제로 쓰였다(업스트림 호출이 나갔다). round_robin 커서를
    /// 해당 키 다음으로 전진시키고 연속 실패를 지운다.
    pub fn record_attempt(&mut self, id: &str, now_epoch_ms: u64) {
        if let Some(state) = self.states.get_mut(id) {
            state.requests += 1;
            state.consecutive_failures = 0;
            state.last_used_epoch_ms = Some(now_epoch_ms);
        }
        if self.strategy == GatewayPoolStrategy::RoundRobin {
            if let Some(index) = self.order.iter().position(|k| k == id) {
                self.rr_cursor = (index + 1) % self.order.len();
            }
        }
    }

    /// 2xx 완결 — 상태는 `record_attempt`가 이미 정리했다(성공 = 시도).
    pub fn record_success(&mut self, id: &str) {
        if let Some(state) = self.states.get_mut(id) {
            state.cooldown_until_ms = None;
        }
    }

    /// 429/402(사전 스트림). `retry_after_ms`는 제공자가 준 값 그대로(있으면).
    pub fn record_rate_limited(&mut self, id: &str, retry_after_ms: Option<u64>, now_ms: u64) {
        let cooldown = retry_after_ms
            .unwrap_or(DEFAULT_COOLDOWN_MS)
            .min(MAX_COOLDOWN_MS);
        if let Some(state) = self.states.get_mut(id) {
            state.cooldown_until_ms = Some(now_ms + cooldown);
            state.failures += 1;
            state.consecutive_failures += 1;
        }
    }

    /// 401/403(사전 스트림) — 격리. 사람이 키를 다시 등록해야 풀린다
    /// (`sync`에서 항목이 사라졌다 다시 생기면 새 상태로 시작한다).
    pub fn record_auth_failure(&mut self, id: &str) {
        if let Some(state) = self.states.get_mut(id) {
            state.quarantined = true;
            state.failures += 1;
            state.consecutive_failures += 1;
        }
    }

    /// 5xx·전송 오류. 3회 연속이면 짧은 쿨다운(소프트 회피).
    pub fn record_transport_failure(&mut self, id: &str, now_ms: u64) {
        if let Some(state) = self.states.get_mut(id) {
            state.failures += 1;
            state.consecutive_failures += 1;
            if state.consecutive_failures >= TRANSPORT_FAILURE_THRESHOLD {
                state.cooldown_until_ms = Some(now_ms + TRANSPORT_COOLDOWN_MS);
                state.consecutive_failures = 0;
            }
        }
    }

    /// 상태 표면(`gateway.status`용, 비밀 없음).
    pub fn view(&self, now_ms: u64) -> Vec<PoolKeyStatus> {
        self.order
            .iter()
            .filter_map(|id| self.states.get(id))
            .map(|state| PoolKeyStatus {
                id: state.id.clone(),
                label: state.label.clone(),
                state: if state.quarantined {
                    PoolKeyViewState::Quarantined
                } else if state.cooldown_until_ms.is_some_and(|until| now_ms < until) {
                    PoolKeyViewState::Cooling
                } else if state.requests == 0 {
                    PoolKeyViewState::Unused
                } else {
                    PoolKeyViewState::Active
                },
                cooldown_remaining_ms: state
                    .cooldown_until_ms
                    .and_then(|until| until.checked_sub(now_ms)),
                requests: state.requests,
                failures: state.failures,
                last_used_epoch_ms: state.last_used_epoch_ms,
            })
            .collect()
    }

    /// 테스트·상태 조회용: 키 수.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(keys: &[&str], strategy: GatewayPoolStrategy) -> KeyPool {
        let mut pool = KeyPool::new(strategy);
        let snapshot: Vec<(String, String)> = keys
            .iter()
            .map(|k| (k.to_string(), format!("label-{k}")))
            .collect();
        pool.sync(&snapshot);
        pool
    }

    #[test]
    fn round_robin_rotates_on_successful_attempts_only() {
        let mut engine = pool(&["a", "b", "c"], GatewayPoolStrategy::RoundRobin);
        assert_eq!(engine.bind_new(0).as_deref(), Some("a"));
        engine.record_attempt("a", 0);
        assert_eq!(engine.bind_new(0).as_deref(), Some("b"));
        engine.record_attempt("b", 0);
        // 발급만 되고 시도로 기록되지 않으면 커서는 그대로 — 같은 키가 나온다.
        assert_eq!(engine.bind_new(0).as_deref(), Some("c"));
        assert_eq!(engine.bind_new(0).as_deref(), Some("c"));
        engine.record_attempt("c", 0);
        // 순환.
        assert_eq!(engine.bind_new(0).as_deref(), Some("a"));
    }

    #[test]
    fn fill_first_sticks_until_threshold() {
        let mut engine = pool(&["a", "b"], GatewayPoolStrategy::FillFirst);
        assert_eq!(engine.bind_new(0).as_deref(), Some("a"));
        assert_eq!(engine.bind_new(0).as_deref(), Some("a"));
        // a가 쿨다운에 들어가면 다음으로.
        engine.record_rate_limited("a", None, 0);
        assert_eq!(engine.bind_new(1_000).as_deref(), Some("b"));
        // 쿨다운이 지나면 a로 돌아온다(우선순위 순서).
        assert_eq!(engine.bind_new(61_000).as_deref(), Some("a"));
    }

    #[test]
    fn affinity_survives_while_key_is_usable_and_rebinds_when_not() {
        let mut engine = pool(&["a", "b"], GatewayPoolStrategy::RoundRobin);
        engine.record_attempt("a", 0);
        // 묶인 키가 사용 가능하면 그대로.
        assert_eq!(engine.rebind(Some("a"), 5_000).as_deref(), Some("a"));
        // 쿨다운에 들어가면 갈아탄다(다음 사용 가능 키).
        engine.record_rate_limited("a", None, 10_000);
        assert_eq!(engine.rebind(Some("a"), 11_000).as_deref(), Some("b"));
        // 삭제된 키는 sync에서 상태째 사라진다.
        engine.sync(&[("b".to_string(), "label-b".to_string())]);
        assert_eq!(engine.rebind(Some("a"), 12_000).as_deref(), Some("b"));
        assert_eq!(engine.len(), 1);
    }

    #[test]
    fn rate_limit_cooldown_honors_retry_after_and_caps_it() {
        let mut engine = pool(&["a"], GatewayPoolStrategy::RoundRobin);
        engine.record_rate_limited("a", Some(120_000), 0);
        assert_eq!(
            engine.bind_new(60_000),
            None,
            "Retry-After 창 안에서는 사용 불가"
        );
        assert!(engine.bind_new(120_000).is_some(), "만료되면 복귀");
        // 상한 24h — 말도 안 되게 긴 값도 그 이상으로 잠그지 않는다.
        engine.record_rate_limited("a", Some(u64::MAX / 2), 200_000);
        assert!(engine.bind_new(200_000 + MAX_COOLDOWN_MS + 1).is_some());
    }

    #[test]
    fn auth_failure_quarantines_until_re_registration() {
        let mut engine = pool(&["a", "b"], GatewayPoolStrategy::RoundRobin);
        engine.record_auth_failure("a");
        assert_eq!(
            engine.bind_new(10_000_000).as_deref(),
            Some("b"),
            "격리는 시간으로 안 풀린다"
        );
        // 재등록(sync에서 사라졌다 새 id로 들어오는 것과 같은 효과).
        engine.record_success("a");
        assert!(
            engine.states.get("a").unwrap().quarantined,
            "성공으로도 안 풀린다"
        );
    }

    #[test]
    fn transport_failures_escalate_to_soft_cooldown() {
        let mut engine = pool(&["a"], GatewayPoolStrategy::RoundRobin);
        engine.record_transport_failure("a", 0);
        engine.record_transport_failure("a", 1);
        assert!(engine.bind_new(2).is_some(), "2회까지는 남아 있다");
        engine.record_transport_failure("a", 3);
        assert_eq!(engine.bind_new(4), None, "3회 연속이면 30s 소프트 회피");
        assert!(engine.bind_new(31_000).is_some());
        // 시도 성공은 연속 실패를 지운다.
        engine.record_attempt("a", 40_000);
        for n in 0..2 {
            engine.record_transport_failure("a", 41_000 + n);
        }
        assert!(engine.bind_new(50_000).is_some(), "카운터는 리셋됐다");
    }

    #[test]
    fn view_reports_state_without_secrets() {
        let mut engine = pool(&["a", "b"], GatewayPoolStrategy::RoundRobin);
        engine.record_attempt("a", 1_000);
        engine.record_rate_limited("b", Some(5_000), 2_000);
        let view = engine.view(3_000);
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].state, PoolKeyViewState::Active);
        assert_eq!(view[0].requests, 1);
        assert_eq!(view[1].state, PoolKeyViewState::Cooling);
        assert_eq!(view[1].cooldown_remaining_ms, Some(4_000));
        // fresh 풀의 첫 키는 unused.
        let fresh = pool(&["x"], GatewayPoolStrategy::RoundRobin);
        assert_eq!(fresh.view(0)[0].state, PoolKeyViewState::Unused);
    }

    #[test]
    fn sync_drops_removed_keys_and_keeps_their_survivors() {
        let mut engine = pool(&["a", "b"], GatewayPoolStrategy::RoundRobin);
        engine.record_attempt("a", 0);
        engine.record_auth_failure("b");
        // b가 지워지고 c가 들어온다.
        engine.sync(&[
            ("a".to_string(), "label-a".to_string()),
            ("c".to_string(), "label-c".to_string()),
        ]);
        assert_eq!(engine.len(), 2);
        let view = engine.view(0);
        assert_eq!(view[0].state, PoolKeyViewState::Active);
        assert_eq!(view[1].state, PoolKeyViewState::Unused);
    }
}
