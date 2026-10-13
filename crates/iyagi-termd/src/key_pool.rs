//! API 키 회전 엔진 — 09-key-pool-rotation §3.
//!
//! opencodex의 계정 회전 상태머신을 **API 키 축**으로 축소한 것이다:
//! 같은 뼈대(세션 친화성 · 쿨다운 · 격리 · 사전 스트림 1회 페일오버)를
//! 쓰되, 구독 창이 없으므로 사용량 백분율 점수·프로브 리스 같은 것은 없다.
//! 쿨다운 해제는 시각 경과뿐 — API 키는 유기적 2xx로 회복을 증명할 필요가
//! 없다(09 §3.3).
//!
//! 순수 로직만 담는다(디스크 I/O·시계 생성 없음). 시계는 `now_ms` 주입,
//! 저장소 읽기는 [`Gateway`](crate::gateway::Gateway)가 한다(5초 TTL 캐시)
//! — 이 모듈은 상태(회전·건강)만 소유한다.

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
    /// round_robin 커서(order 인덱스). 발급할 때 고른 키 다음으로 전진한다
    /// — 요청 전에 잇달아 열린 세션들도 서로 다른 키로 흩어진다.
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
    /// 갈아탄다. 친화성은 "가능한 한 유지"다(09 §3.2). `exclude`는 방금
    /// 실패한 키(사전 스트림 페일오버) — 묶인 키여도 고르지 않는다.
    pub fn rebind(
        &mut self,
        bound: Option<&str>,
        exclude: Option<&str>,
        now_ms: u64,
    ) -> Option<String> {
        if let Some(id) = bound.filter(|id| Some(*id) != exclude) {
            if self
                .states
                .get(id)
                .is_some_and(|state| state.available(now_ms))
            {
                return Some(id.to_string());
            }
        }
        self.bind_new_excluding(exclude, now_ms)
    }

    /// 새 세션 발급용 키 선택. 사용 가능한 키가 없으면 `None`(호출자는
    /// 단일 키 경로로 폴백한다).
    pub fn bind_new(&mut self, now_ms: u64) -> Option<String> {
        self.bind_new_excluding(None, now_ms)
    }

    fn bind_new_excluding(&mut self, exclude: Option<&str>, now_ms: u64) -> Option<String> {
        if self.order.is_empty() {
            return None;
        }
        let usable =
            |id: &String| Some(id.as_str()) != exclude && self.states[id].available(now_ms);
        match self.strategy {
            GatewayPoolStrategy::FillFirst => self.order.iter().find(|id| usable(id)).cloned(),
            GatewayPoolStrategy::RoundRobin => {
                // 커서부터 순환하며 첫 사용 가능 키. 고른 키 다음으로 커서를
                // 옮긴다 — 다음 발급은 다른 키부터 본다.
                let len = self.order.len();
                let index = (0..len)
                    .map(|step| (self.rr_cursor + step) % len)
                    .find(|index| usable(&self.order[*index]))?;
                self.rr_cursor = (index + 1) % len;
                Some(self.order[index].clone())
            }
        }
    }

    /// `id` 말고 지금 쓸 수 있는 키가 있는가(페일오버할 곳이 있는가).
    pub fn has_available_except(&self, id: &str, now_ms: u64) -> bool {
        self.order
            .iter()
            .any(|other| other != id && self.states[other].available(now_ms))
    }

    /// 풀 전원이 쓸 수 없을 때 가장 빨리 풀리는 쿨다운까지 남은 시간.
    /// 쿨다운 중인 키가 없으면(전원 격리) `None`.
    pub fn earliest_cooldown_remaining_ms(&self, now_ms: u64) -> Option<u64> {
        self.states
            .values()
            .filter(|state| !state.quarantined)
            .filter_map(|state| state.cooldown_until_ms?.checked_sub(now_ms))
            .filter(|remaining| *remaining > 0)
            .min()
    }

    /// 발급이 실제로 쓰였다(업스트림 호출이 나갔다) — 장부만 갱신한다.
    /// 연속 실패는 결과(`record_success`/`record_*_failure`)가 정한다.
    pub fn record_attempt(&mut self, id: &str, now_epoch_ms: u64) {
        if let Some(state) = self.states.get_mut(id) {
            state.requests += 1;
            state.last_used_epoch_ms = Some(now_epoch_ms);
        }
    }

    /// 2xx — 연속 실패를 지운다. 쿨다운은 건드리지 않는다: 쿨다운 해제는
    /// 시각 경과뿐이고(09 §3.3), 429 직전에 같은 키로 나간 병렬 요청의
    /// 성공이 막 걸린 쿨다운을 풀어 버리면 안 된다.
    pub fn record_success(&mut self, id: &str) {
        if let Some(state) = self.states.get_mut(id) {
            state.consecutive_failures = 0;
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

    /// 401(사전 스트림) — 격리. 사람이 키를 다시 등록해야 풀린다
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
    fn round_robin_spreads_back_to_back_sessions() {
        let mut engine = pool(&["a", "b", "c"], GatewayPoolStrategy::RoundRobin);
        // 요청이 나가기 전에 잇달아 열린 세션도 서로 다른 키로 흩어진다.
        assert_eq!(engine.bind_new(0).as_deref(), Some("a"));
        assert_eq!(engine.bind_new(0).as_deref(), Some("b"));
        assert_eq!(engine.bind_new(0).as_deref(), Some("c"));
        // 순환.
        assert_eq!(engine.bind_new(0).as_deref(), Some("a"));
        // 친화성 요청(묶인 키 유지)은 커서를 움직이지 않는다.
        engine.record_attempt("a", 0);
        assert_eq!(engine.rebind(Some("a"), None, 0).as_deref(), Some("a"));
        assert_eq!(engine.bind_new(0).as_deref(), Some("b"));
        // 쓸 수 없는 키는 건너뛴다.
        engine.record_rate_limited("c", None, 0);
        assert_eq!(engine.bind_new(1).as_deref(), Some("a"));
    }

    #[test]
    fn failover_excludes_the_key_that_just_failed() {
        let mut engine = pool(&["a", "b"], GatewayPoolStrategy::FillFirst);
        // 전송 오류 1회는 쿨다운이 아니다 — exclude 없이는 같은 키가 나온다.
        engine.record_transport_failure("a", 0);
        assert_eq!(engine.rebind(Some("a"), None, 1).as_deref(), Some("a"));
        assert_eq!(engine.rebind(Some("a"), Some("a"), 1).as_deref(), Some("b"));
        assert!(engine.has_available_except("a", 1));
        // 다른 키가 없으면 페일오버할 곳도 없다.
        let single = pool(&["a"], GatewayPoolStrategy::RoundRobin);
        assert!(!single.has_available_except("a", 0));
    }

    #[test]
    fn success_does_not_lift_a_fresh_cooldown() {
        let mut engine = pool(&["a"], GatewayPoolStrategy::RoundRobin);
        engine.record_rate_limited("a", Some(10_000), 0);
        // 429 직전에 같은 키로 나간 병렬 요청이 2xx로 끝났다.
        engine.record_success("a");
        assert_eq!(
            engine.bind_new(5_000),
            None,
            "쿨다운은 시각 경과로만 풀린다"
        );
        assert_eq!(engine.earliest_cooldown_remaining_ms(5_000), Some(5_000));
        assert!(engine.bind_new(10_000).is_some());
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
        assert_eq!(engine.rebind(Some("a"), None, 5_000).as_deref(), Some("a"));
        // 쿨다운에 들어가면 갈아탄다(다음 사용 가능 키).
        engine.record_rate_limited("a", None, 10_000);
        assert_eq!(engine.rebind(Some("a"), None, 11_000).as_deref(), Some("b"));
        // 삭제된 키는 sync에서 상태째 사라진다.
        engine.sync(&[("b".to_string(), "label-b".to_string())]);
        assert_eq!(engine.rebind(Some("a"), None, 12_000).as_deref(), Some("b"));
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
        // 성공 기록으로도 안 풀린다.
        engine.record_success("a");
        assert!(
            engine.states.get("a").unwrap().quarantined,
            "성공으로도 안 풀린다"
        );
        assert_eq!(
            engine.earliest_cooldown_remaining_ms(0),
            None,
            "격리는 쿨다운 힌트가 아니다"
        );
        // 재등록 = 삭제 후 새 id — sync가 새 상태로 들인다.
        engine.sync(&[
            ("b".to_string(), "label-b".to_string()),
            ("a2".to_string(), "label-a".to_string()),
        ]);
        assert_eq!(engine.view(0)[1].state, PoolKeyViewState::Unused);
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
        // 게이트웨이 순서 그대로: 시도 기록은 카운터를 지우지 않는다(5xx도
        // 시도다) — 2xx만 지운다.
        for n in 0..2 {
            engine.record_attempt("a", 40_000 + n);
            engine.record_transport_failure("a", 40_000 + n);
        }
        engine.record_attempt("a", 40_010);
        engine.record_success("a");
        engine.record_attempt("a", 40_020);
        engine.record_transport_failure("a", 40_020);
        assert!(engine.bind_new(40_030).is_some(), "2xx가 카운터를 리셋했다");
        engine.record_attempt("a", 40_040);
        engine.record_transport_failure("a", 40_040);
        engine.record_attempt("a", 40_050);
        engine.record_transport_failure("a", 40_050);
        assert_eq!(engine.bind_new(40_060), None, "리셋 뒤 3회 연속");
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
