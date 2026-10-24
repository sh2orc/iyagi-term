//! 압력 완화 P2 — 스케줄링 양보(spec `08-pressure-relief.md` §2).
//!
//! 두 부분으로 나뉜다.
//!
//! * [`ReliefController`]: 순수 결정 코어. 시계도 OS도 모르고, 압력 level·
//!   면제 집합(포커스·보이는 세션)·살아 있는 워크로드 목록을 받아
//!   [`ReliefOp`] 목록만 낸다. 덕분에 §0의 불변 조건(포커스·보호 면제,
//!   수동 > 자동, 되돌릴 수 있음)을 단위 시험으로 통째로 검증할 수 있다.
//! * [`apply`]: 그 결정을 실제 OS에 거는 어댑터. 관리 워크로드는 그룹의
//!   `set_scheduling`으로, 직접 셸은 그룹이 없으므로(02-runner §3) §1.2의
//!   관측 트리에 per-pid 진입점을 쓴다.
//!
//! 불변 조건(§0):
//!
//! 1. 자동은 거부하지 않고 양보만 한다 — 프로세스를 정지·종료하지 않는다.
//! 2. 포커스·화면에 보이는 세션과 보호 표시 세션은 자동 대상이 아니다.
//! 3. 수동이 자동보다 우선한다: 수동 양보는 자동 복원되지 않고, 압력 중
//!    수동 복원한 세션은 NORMAL로 돌아갈 때까지, 독점 중 수동 복원한 세션은
//!    독점이 끝날 때까지 다시 양보되지 않는다.
//! 4. 모든 양보는 되돌릴 수 있고 데몬 종료 경로에서 해제된다.
//!
//! 양보 사유는 둘이다(기록이 사유를 기억해 해제 규칙을 고른다).
//!
//! * **독점**(호스트 압력과 무관): 가드 정책의 `cpu_cores_limit`을
//!   `sustain_ms` 창의 표본 [`HOG_ENTER_FRACTION`] 이상에서 넘겼다. 해제는
//!   한도의 [`HOG_QUIET_FRACTION`] 미만이 [`HOG_QUIET_MS`] 이어지고 최소
//!   유지 시간이 지났을 때다. 양보된 작업은 측정값이 떨어진다(Apple
//!   Silicon의 background 등급은 효율 코어로만 돈다) — 한 틱의 하락으로
//!   풀면 20초 뒤 다시 양보되는 진동이 됐다. 풀린 직후 다시 독점이면 유지
//!   시간을 두 배로 늘린다.
//! * **압력**: CPU WARNING에서는 낮은 우선순위(2) 워크로드만, CRITICAL에서는
//!   면제가 아닌 RUNNING 전부를 양보시킨다(P2-1). NORMAL 회복 뒤
//!   `relief_release_interval` 간격으로 하나씩 되돌리되, 되돌린 직후 압력이
//!   다시 오면 간격을 두 배로 늘린다(전부 양보 ↔ 하나씩 복원 진동 완화).

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::Arc;

use term_contracts::ids::{SessionId, U64String, WorkloadId};
use term_contracts::metrics::PressureLevel;
use term_contracts::session::ReliefAction;
use term_contracts::snapshot::{ReliefPolicy, ReliefState};
use term_contracts::state::WorkloadState;
use term_platform::group::{scheduling, SchedulingOutcome, SchedulingTier};

use crate::state::DaemonState;

/// 독점 진입: 지속 창 안 표본 중 이 비율 이상이 한도를 넘겼다.
pub const HOG_ENTER_FRACTION: f64 = 0.8;
/// 독점 해제: 한도의 이 비율 미만이어야 "조용하다".
pub const HOG_QUIET_FRACTION: f64 = 0.5;
/// 독점 해제: 조용한 상태가 이만큼 이어져야 한다.
pub const HOG_QUIET_MS: u64 = 30_000;
/// 독점 양보의 최소 유지 시간(재양보마다 두 배, 상한 [`HOG_MAX_HOLD_MS`]).
pub const HOG_MIN_HOLD_MS: u64 = 60_000;
/// 독점 양보 유지 시간의 상한.
pub const HOG_MAX_HOLD_MS: u64 = 10 * 60_000;
/// 독점 해제 뒤 이 안에 다시 독점이면 유지 시간을 늘린다.
const HOG_REYIELD_WINDOW_MS: u64 = 120_000;
/// 압력 복원 직후 이 안에 압력이 재발하면 복원 간격을 늘린다.
const PRESSURE_REENTRY_WINDOW_MS: u64 = 60_000;
/// 복원 간격의 상한.
const MAX_RELEASE_INTERVAL_MS: u64 = 30_000;
/// NORMAL이 이만큼 이어지면 늘어난 복원 간격을 기본으로 되돌린다.
const STABLE_NORMAL_MS: u64 = 5 * 60_000;
/// 실패한 조작의 재시도 간격 — 매 틱 같은 OS 호출(과 트리 스캔)을 반복하지
/// 않고, 실패한 하나가 순차 복원 슬롯을 막지 않게 한다.
const RETRY_BACKOFF_MS: u64 = 10_000;
/// 부분 적용된 양보의 재시도 간격(권한이 없는 setuid 자식 등은 매번
/// 실패하므로 매 틱 다시 훑지 않는다).
const PARTIAL_RETRY_MS: u64 = 30_000;

/// 한 워크로드에 대해 정책이 지시하는 OS 조작.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReliefOp {
    /// 스케줄링 우선순위를 background로 내린다.
    Yield { workload_id: WorkloadId },
    /// 원래 우선순위로 되돌린다.
    Restore { workload_id: WorkloadId },
}

impl ReliefOp {
    pub fn workload_id(&self) -> &WorkloadId {
        match self {
            ReliefOp::Yield { workload_id } | ReliefOp::Restore { workload_id } => workload_id,
        }
    }

    pub fn tier(&self) -> SchedulingTier {
        match self {
            ReliefOp::Yield { .. } => SchedulingTier::Background,
            ReliefOp::Restore { .. } => SchedulingTier::Normal,
        }
    }
}

/// 정책 입력 한 건: 지금 레지스트리에 살아 있는 워크로드.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveWorkload {
    pub workload_id: WorkloadId,
    pub session_id: SessionId,
    pub state: WorkloadState,
    /// 스케줄링 우선순위(0 높음 ~ 2 낮음).
    pub priority: u8,
    /// 에이전트가 "busy"라고 보고하는 중이다(가드의 정적 희생 선택이 쓴다).
    pub busy: bool,
    /// 이 틱의 귀속 논리 코어(측정·추정만; `None`은 미측정).
    pub cpu_cores: Option<f64>,
}

/// 낮은 우선순위 — CPU WARNING에서도 양보 대상이다.
pub const LOW_PRIORITY: u8 = 2;

/// 자동 양보의 사유(해제 규칙이 다르다).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum YieldReason {
    Hog,
    Pressure,
}

/// 워크로드 하나의 완화 기록.
#[derive(Debug, Clone, Default, PartialEq)]
struct Record {
    relief: ReliefState,
    /// 자동 양보의 사유(수동·미양보면 None).
    reason: Option<YieldReason>,
    /// 계획이 낸 Yield의 사유 — `record`가 성공을 기록할 때 옮겨 담는다.
    pending_reason: Option<YieldReason>,
    /// 최근 독점 판정 창의 표본(시각, 한도 초과 여부).
    cpu_samples: VecDeque<(u64, bool)>,
    /// 한도의 [`HOG_QUIET_FRACTION`] 미만이 시작된 시각.
    quiet_since_ms: Option<u64>,
    /// 독점 양보의 지금 유지 시간.
    hog_hold_ms: u64,
    /// 마지막 독점 해제 시각.
    last_hog_release_ms: Option<u64>,
    /// 사용자가 독점 중에 직접 복원했다 — 독점이 끝날 때까지 자동 독점
    /// 양보 대상이 아니다(§0-3).
    hog_override: bool,
    /// 사용자가 직접 양보시켰다 — 자동 복원 대상이 아니다(§0-3).
    manual_yield: bool,
    /// 사용자가 보호 표시했다(해제할 때까지 유지).
    protected_sticky: bool,
    /// 압력이 남아 있는 동안 수동 복원한 세션을 다시 양보시키지 않는
    /// 일시 보호 — NORMAL 회복에서 자동으로 풀린다.
    protected_until_normal: bool,
    /// 이 워크로드의 적용 경로가 `Unsupported`를 돌려줬다 — 다시 시도하지
    /// 않는다. 데몬 전체가 아니라 워크로드 단위인 이유: 위임 cgroup Linux는
    /// 관리 워크로드의 `cpu.weight`를 지원하지만 그룹이 없는 직접 셸의
    /// per-pid 경로는 지원하지 않을 수 있다(§0-4). 하나가 지원되지 않는다고
    /// 나머지의 완화까지 꺼서는 안 된다.
    unsupported: bool,
    /// 실패·부분 적용 뒤 이 시각까지는 같은 조작을 다시 계획하지 않는다.
    retry_after_ms: Option<u64>,
}

impl Record {
    /// 지금 자동 완화 대상에서 빠져 있는가(UI가 배지로 보여 주는 값).
    fn protected(&self) -> bool {
        self.protected_sticky || self.protected_until_normal
    }

    fn since_ms(&self) -> Option<u64> {
        match &self.relief {
            ReliefState::Yielded { since_ms, .. } => Some(since_ms.get()),
            ReliefState::None => None,
        }
    }

    fn backing_off(&self, now_ms: u64) -> bool {
        self.retry_after_ms.is_some_and(|until| now_ms < until)
    }

    /// 지속 창 안 표본의 [`HOG_ENTER_FRACTION`] 이상이 한도를 넘겼고, 창이
    /// 다 찼다.
    fn hot(&self, now_ms: u64, sustain_ms: u64) -> bool {
        let Some((first, _)) = self.cpu_samples.front() else {
            return false;
        };
        if now_ms.saturating_sub(*first) < sustain_ms.max(1) {
            return false;
        }
        let over = self.cpu_samples.iter().filter(|(_, over)| *over).count();
        over as f64 >= self.cpu_samples.len() as f64 * HOG_ENTER_FRACTION
    }

    fn quiet_long_enough(&self, now_ms: u64) -> bool {
        self.quiet_since_ms
            .is_some_and(|since| now_ms.saturating_sub(since) >= HOG_QUIET_MS)
    }
}

/// 양보 정책의 결정 코어. `DaemonState`가 `Mutex`로 하나만 소유한다.
pub struct ReliefController {
    records: HashMap<WorkloadId, Record>,
    policy: ReliefPolicy,
    /// 마지막 순차 복원 시각(단조 ms) — `None`이면 아직 한 번도 안 풀었다.
    last_release_ms: Option<u64>,
    /// `timing_ms.relief_release_interval`.
    release_interval_ms: u64,
    /// 지금의 복원 간격(압력 재발 때마다 두 배, 안정되면 기본으로).
    current_release_interval_ms: u64,
    /// 지난 틱의 CPU 압력.
    last_level: PressureLevel,
    /// NORMAL이 시작된 시각.
    normal_since_ms: Option<u64>,
    /// 종료 경로가 시작됐다 — 더 이상 양보를 내지 않는다.
    closing: bool,
}

impl ReliefController {
    pub fn new(policy: ReliefPolicy, release_interval_ms: u64) -> Self {
        let release_interval_ms = release_interval_ms.max(1);
        Self {
            records: HashMap::new(),
            policy,
            last_release_ms: None,
            release_interval_ms,
            current_release_interval_ms: release_interval_ms,
            last_level: PressureLevel::Normal,
            normal_since_ms: None,
            closing: false,
        }
    }

    pub fn policy(&self) -> ReliefPolicy {
        self.policy
    }

    /// 정책 교체(`relief.set_policy`). 자동 양보를 끄는 것만으로는 이미
    /// 양보 중인 세션이 복원되지 않는다 — NORMAL 회복 경로나 수동 복원이
    /// 되돌린다. 실제로 바뀌었으면 true.
    pub fn set_policy(&mut self, policy: ReliefPolicy) -> bool {
        let changed = self.policy != policy;
        self.policy = policy;
        changed
    }

    /// 스냅샷 보고용 `(relief, protected)`.
    pub fn view(&self, workload_id: &WorkloadId) -> (ReliefState, bool) {
        match self.records.get(workload_id) {
            Some(record) => (record.relief.clone(), record.protected()),
            None => (ReliefState::None, false),
        }
    }

    /// 이 워크로드의 적용 경로가 `Unsupported`로 판명되어 더 이상 시도하지
    /// 않는가(진단·시험용).
    pub fn is_unsupported(&self, workload_id: &WorkloadId) -> bool {
        self.records
            .get(workload_id)
            .is_some_and(|record| record.unsupported)
    }

    /// 사용자가 보호한 워크로드인가(sticky 또는 압력 중 수동 복원). 가드가
    /// 자동 정지 대상에서 빼는 데 쓴다.
    pub fn is_protected(&self, workload_id: &WorkloadId) -> bool {
        self.records
            .get(workload_id)
            .is_some_and(|record| record.protected())
    }

    /// 지금의 순차 복원 간격(진단·시험용).
    pub fn current_release_interval_ms(&self) -> u64 {
        self.current_release_interval_ms
    }

    /// 압력 단계 전이에 따른 복원 간격 조정.
    fn track_level(&mut self, now_ms: u64, cpu_level: PressureLevel) {
        let normal = cpu_level == PressureLevel::Normal;
        if !normal && self.last_level == PressureLevel::Normal {
            // 압력 재진입: 직전에 하나를 되돌린 지 얼마 안 됐다면 되돌리기가
            // 압력을 다시 부른 것이다 — 다음 회복에서는 더 천천히 푼다.
            if self
                .last_release_ms
                .is_some_and(|last| now_ms.saturating_sub(last) <= PRESSURE_REENTRY_WINDOW_MS)
            {
                self.current_release_interval_ms = self
                    .current_release_interval_ms
                    .saturating_mul(2)
                    .min(MAX_RELEASE_INTERVAL_MS.max(self.release_interval_ms));
            }
        }
        if normal {
            let since = *self.normal_since_ms.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= STABLE_NORMAL_MS {
                self.current_release_interval_ms = self.release_interval_ms;
            }
        } else {
            self.normal_since_ms = None;
        }
        self.last_level = cpu_level;
    }

    /// 한 틱의 계획. 부수효과는 기록 정리(죽은 워크로드 제거, NORMAL에서
    /// 일시 보호 해제, 독점 표본, 순차 복원 타이머)뿐이고 OS는 건드리지 않는다.
    ///
    /// `exempt`는 포커스된 세션과 화면에 보이는 세션 전부다. `cpu_limit_cores`
    /// ·`cpu_sustain_ms`는 가드 정책의 독점 한도와 지속 창이다.
    #[allow(clippy::too_many_arguments)]
    pub fn plan(
        &mut self,
        now_ms: u64,
        cpu_level: PressureLevel,
        exempt: &[SessionId],
        live: &[LiveWorkload],
        capability_supported: bool,
        cpu_limit_cores: f64,
        cpu_sustain_ms: u64,
    ) -> Vec<ReliefOp> {
        // 더 이상 살아 있지 않은 워크로드의 기록은 버린다 — 프로세스가 이미
        // 없으므로 복원 호출도 하지 않는다.
        let live_ids: HashSet<&str> = live.iter().map(|w| w.workload_id.as_str()).collect();
        self.records.retain(|id, _| live_ids.contains(id.as_str()));
        if self.closing {
            return Vec::new();
        }
        self.track_level(now_ms, cpu_level);

        let normal = cpu_level == PressureLevel::Normal;
        if normal {
            // 압력이 풀렸다: "압력 중 수동 복원" 일시 보호는 여기서 끝난다.
            for record in self.records.values_mut() {
                record.protected_until_normal = false;
            }
        }

        let exempt: HashSet<&str> = exempt.iter().map(SessionId::as_str).collect();
        // 계획은 결정적이어야 한다(스냅샷·시험 재현성): workload_id 순.
        let mut ordered: Vec<&LiveWorkload> = live.iter().collect();
        ordered.sort_by(|a, b| a.workload_id.as_str().cmp(b.workload_id.as_str()));

        // 0) 독점 표본: 이 틱의 초과 여부와 조용함을 기록한다. 면제 세션은
        //    세지 않는다(보고 있는 pane은 느리게 두지 않는다).
        let limit = if cpu_limit_cores.is_finite() && cpu_limit_cores > 0.0 {
            cpu_limit_cores
        } else {
            f64::INFINITY
        };
        let sustain = cpu_sustain_ms.max(1);
        for workload in &ordered {
            let record = self
                .records
                .entry(workload.workload_id.clone())
                .or_default();
            if exempt.contains(workload.session_id.as_str()) {
                record.cpu_samples.clear();
                record.quiet_since_ms = None;
                continue;
            }
            let over = workload.cpu_cores.is_some_and(|cores| cores > limit);
            record.cpu_samples.push_back((now_ms, over));
            while record
                .cpu_samples
                .front()
                .is_some_and(|(at, _)| now_ms.saturating_sub(*at) > sustain)
            {
                record.cpu_samples.pop_front();
            }
            let quiet = workload
                .cpu_cores
                .is_some_and(|cores| cores < limit * HOG_QUIET_FRACTION);
            if quiet {
                record.quiet_since_ms.get_or_insert(now_ms);
            } else {
                record.quiet_since_ms = None;
            }
            // 독점 중 수동 복원의 면제는 독점이 끝나면(충분히 조용하면) 풀린다.
            if record.hog_override && record.quiet_long_enough(now_ms) {
                record.hog_override = false;
            }
        }

        let mut ops = Vec::new();
        // 1) 보이게 된 자동 양보는 압력 level과 무관하게 즉시 복원한다
        //    (§0-2: 보고 있는 pane은 절대 느리게 두지 않는다).
        for workload in &ordered {
            let Some(record) = self.records.get(&workload.workload_id) else {
                continue;
            };
            if exempt.contains(workload.session_id.as_str())
                && record.relief.is_yielded()
                && !record.manual_yield
                && !record.backing_off(now_ms)
            {
                ops.push(ReliefOp::Restore {
                    workload_id: workload.workload_id.clone(),
                });
            }
        }

        // 압력 규칙이 지금 이 워크로드를 양보시키려 하는가(P2-1).
        let pressure_wants = |workload: &LiveWorkload| match cpu_level {
            PressureLevel::Normal => false,
            PressureLevel::Warning => workload.priority >= LOW_PRIORITY,
            PressureLevel::Critical => true,
        };
        let auto = capability_supported && self.policy.auto_yield;

        // 2) 자동 양보: 독점(모든 압력 단계) 또는 압력 규칙. 수동·보호·미지원·
        //    백오프 중인 것은 건너뛰고, 이미 양보된 것은 부분 적용일 때만 다시
        //    시도한다.
        if auto {
            let planned: HashSet<String> = ops
                .iter()
                .map(|op| op.workload_id().as_str().to_string())
                .collect();
            for workload in &ordered {
                if workload.state != WorkloadState::Running
                    || planned.contains(workload.workload_id.as_str())
                    || exempt.contains(workload.session_id.as_str())
                {
                    continue;
                }
                let Some(record) = self.records.get_mut(&workload.workload_id) else {
                    continue;
                };
                if record.unsupported || record.manual_yield || record.backing_off(now_ms) {
                    continue;
                }
                let hog = record.hot(now_ms, sustain) && !record.hog_override;
                let pressure = pressure_wants(workload) && !record.protected();
                let reason = if hog && !record.protected() {
                    Some(YieldReason::Hog)
                } else if pressure {
                    Some(YieldReason::Pressure)
                } else {
                    None
                };
                let Some(reason) = reason else { continue };
                let apply = match &record.relief {
                    ReliefState::None => true,
                    ReliefState::Yielded { partial, .. } => *partial,
                };
                if apply {
                    record.pending_reason = Some(reason);
                    ops.push(ReliefOp::Yield {
                        workload_id: workload.workload_id.clone(),
                    });
                } else if reason == YieldReason::Hog && record.reason == Some(YieldReason::Pressure)
                {
                    // 압력으로 양보 중이던 것이 독점이 됐다 — 해제 규칙을 바꾼다.
                    record.reason = Some(YieldReason::Hog);
                    record.hog_hold_ms = record.hog_hold_ms.max(HOG_MIN_HOLD_MS);
                }
            }
        }

        // 3) 순차 복원: 해제 조건을 채운 자동 양보를 가장 오래된 것부터 하나씩,
        //    지금의 복원 간격으로. 압력 사유는 NORMAL에서만, 독점 사유는 조용해진
        //    뒤 최소 유지 시간이 지나면(압력 규칙이 다시 양보시킬 것이면 그대로).
        if self.release_due(now_ms) {
            let planned: HashSet<&str> = ops.iter().map(|op| op.workload_id().as_str()).collect();
            let next = ordered
                .iter()
                .filter(|w| !planned.contains(w.workload_id.as_str()))
                .filter_map(|w| {
                    let record = self.records.get(&w.workload_id)?;
                    if record.manual_yield || record.backing_off(now_ms) {
                        return None;
                    }
                    let since = record.since_ms()?;
                    let releasable = match record.reason {
                        // 사유를 모르는 자동 양보(예전 기록)는 압력처럼 다룬다.
                        Some(YieldReason::Pressure) | None => {
                            normal && (record.hog_override || !record.hot(now_ms, sustain))
                        }
                        Some(YieldReason::Hog) => {
                            (record.quiet_long_enough(now_ms) || record.hog_override)
                                && now_ms.saturating_sub(since) >= record.hog_hold_ms
                                && !(pressure_wants(w) && auto && !record.protected())
                        }
                    };
                    releasable.then(|| (since, w.workload_id.clone()))
                })
                .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.as_str().cmp(b.1.as_str())));
            if let Some((_, workload_id)) = next {
                self.last_release_ms = Some(now_ms);
                ops.push(ReliefOp::Restore { workload_id });
            }
        }
        ops
    }

    /// 수동 조작(`session.relief`). 수동은 항상 자동보다 우선한다(§0-3).
    pub fn manual(
        &mut self,
        workload_id: &WorkloadId,
        action: ReliefAction,
        cpu_level: PressureLevel,
    ) -> Vec<ReliefOp> {
        let record = self.records.entry(workload_id.clone()).or_default();
        record.retry_after_ms = None;
        match action {
            ReliefAction::Yield => {
                record.manual_yield = true;
                match &mut record.relief {
                    // 이미 양보 중이면 소유권만 수동으로 넘긴다 — 같은 정책을
                    // 다시 걸 필요가 없고 `since_ms`도 그대로 둔다.
                    ReliefState::Yielded { manual, .. } => {
                        *manual = true;
                        record.reason = None;
                        Vec::new()
                    }
                    ReliefState::None => vec![ReliefOp::Yield {
                        workload_id: workload_id.clone(),
                    }],
                }
            }
            ReliefAction::Restore => {
                record.manual_yield = false;
                // 압력이 남아 있는데 사용자가 복원했다면 다음 틱에 다시
                // 양보시키지 않는다 — NORMAL 회복에서 자동으로 풀린다.
                record.protected_until_normal = cpu_level != PressureLevel::Normal;
                // 독점 중이었어도 마찬가지 — 독점이 끝날 때까지 면제.
                record.hog_override = true;
                record.cpu_samples.clear();
                if record.relief.is_yielded() {
                    vec![ReliefOp::Restore {
                        workload_id: workload_id.clone(),
                    }]
                } else {
                    Vec::new()
                }
            }
            ReliefAction::Protect => {
                record.protected_sticky = true;
                record.manual_yield = false;
                if record.relief.is_yielded() {
                    vec![ReliefOp::Restore {
                        workload_id: workload_id.clone(),
                    }]
                } else {
                    Vec::new()
                }
            }
            ReliefAction::Unprotect => {
                record.protected_sticky = false;
                record.protected_until_normal = false;
                Vec::new()
            }
        }
    }

    /// 종료 경로: 양보 중인 모든 워크로드를 되돌린다(§0-4). 기록은 즉시
    /// 비워서 같은 호출이 두 번 나가지 않게 하고, 이 뒤로는 계획을 내지
    /// 않는다(텔레메트리 틱과의 경합).
    pub fn shutdown_restore_ops(&mut self) -> Vec<ReliefOp> {
        self.closing = true;
        let mut ops: Vec<ReliefOp> = self
            .records
            .iter()
            .filter(|(_, record)| record.relief.is_yielded())
            .map(|(workload_id, _)| ReliefOp::Restore {
                workload_id: workload_id.clone(),
            })
            .collect();
        ops.sort_by(|a, b| a.workload_id().as_str().cmp(b.workload_id().as_str()));
        for record in self.records.values_mut() {
            record.relief = ReliefState::None;
        }
        ops
    }

    /// 지금 양보 중인 워크로드(크래시 복구 기록·진단용).
    pub fn yielded_ids(&self) -> Vec<WorkloadId> {
        let mut ids: Vec<WorkloadId> = self
            .records
            .iter()
            .filter(|(_, record)| record.relief.is_yielded())
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids
    }

    /// 한 조작의 결과를 기록한다. 상태가 실제로 바뀌었으면 true
    /// (호출자가 그때만 revision을 올린다).
    pub fn record(
        &mut self,
        op: &ReliefOp,
        outcome: &io::Result<SchedulingOutcome>,
        now_ms: u64,
    ) -> bool {
        let workload_id = op.workload_id().clone();
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                let record = self.records.entry(workload_id.clone()).or_default();
                record.pending_reason = None;
                if error.kind() == io::ErrorKind::Unsupported {
                    // 이 워크로드의 경로로는 되돌릴 수 없다 — 다시 시도하지
                    // 않는다(§0-4). 다른 워크로드는 그대로 관리한다.
                    record.unsupported = true;
                } else {
                    // 일시 실패: 매 틱 다시 밀지 않고, 순차 복원의 슬롯을
                    // 혼자 막지 않게 잠시 비킨다.
                    record.retry_after_ms = Some(now_ms + RETRY_BACKOFF_MS);
                }
                // 실패는 이전 상태를 그대로 둔다 — 걸리지 않은 양보를
                // "양보 중"으로 광고하지 않는다.
                tracing::debug!(%workload_id, ?op, %error, "relief op failed");
                return false;
            }
        };
        let record = self.records.entry(workload_id).or_default();
        record.retry_after_ms = outcome.is_partial().then_some(now_ms + PARTIAL_RETRY_MS);
        let next = match op {
            ReliefOp::Yield { .. } => {
                let reason = record.pending_reason.take();
                if !record.manual_yield {
                    if reason == Some(YieldReason::Hog) && !record.relief.is_yielded() {
                        // 풀린 지 얼마 안 돼 다시 독점이면 더 오래 붙잡는다.
                        let reyield = record.last_hog_release_ms.is_some_and(|last| {
                            now_ms.saturating_sub(last) <= HOG_REYIELD_WINDOW_MS
                        });
                        record.hog_hold_ms = if reyield {
                            record
                                .hog_hold_ms
                                .max(HOG_MIN_HOLD_MS)
                                .saturating_mul(2)
                                .min(HOG_MAX_HOLD_MS)
                        } else {
                            HOG_MIN_HOLD_MS
                        };
                    }
                    if reason.is_some() && !record.relief.is_yielded() {
                        record.reason = reason;
                    }
                } else {
                    record.reason = None;
                }
                // 부분 적용을 다시 시도한 경우 `since_ms`는 처음 걸린 시각을
                // 유지한다(순차 복원 순서의 기준).
                let since_ms = record.since_ms().unwrap_or(now_ms).min(U64String::MAX);
                ReliefState::Yielded {
                    since_ms: U64String::new(since_ms)
                        .unwrap_or_else(|_| U64String::new(0).expect("0 is in range")),
                    manual: record.manual_yield,
                    partial: outcome.is_partial(),
                }
            }
            ReliefOp::Restore { .. } => {
                if record.reason == Some(YieldReason::Hog) {
                    record.last_hog_release_ms = Some(now_ms);
                }
                record.reason = None;
                record.cpu_samples.clear();
                ReliefState::None
            }
        };
        let changed = record.relief != next;
        record.relief = next;
        changed
    }

    fn release_due(&self, now_ms: u64) -> bool {
        match self.last_release_ms {
            None => true,
            Some(last) => now_ms.saturating_sub(last) >= self.current_release_interval_ms,
        }
    }
}

/// 결정 하나를 OS에 건다.
///
/// 관리 워크로드는 그룹 백엔드가 자기 멤버를 검증해 적용하고, 직접 셸은
/// 그룹이 없으므로(02-runner §3) §1.2의 관측 트리를 지금 다시 훑어 검증된
/// 신원에만 per-pid 호출을 건다. 셸 트리의 닻도 스폰 때 기록한 루트 신원
/// (F1)이라 pid가 재사용됐으면 트리 자체가 비고, 어느 쪽이든 신원이
/// 어긋난 pid는 건너뛰고 세어질 뿐 절대 건드리지 않는다.
///
/// 블로킹 호출이므로 async executor에서 부르면 안 된다(`ResourcePlatform`
/// 계약): 텔레메트리 스레드나 `spawn_blocking` 워커에서만 호출한다.
pub fn apply(state: &Arc<DaemonState>, op: &ReliefOp) -> io::Result<SchedulingOutcome> {
    let workload_id = op.workload_id();
    let entry = state.workload_entry(workload_id).ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "workload is no longer registered")
    })?;
    // 레지스트리 락을 쥔 채로 플랫폼을 부르지 않는다(state.rs 락 규율).
    let group = {
        let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        guard.group.clone()
    };
    let yielding = matches!(op, ReliefOp::Yield { .. });
    if let Some(group) = group {
        let outcome = state.platform.set_scheduling(&group, op.tier());
        if let Ok(result) = &outcome {
            if yielding && result.applied > 0 {
                let members = state.platform.member_identities(&group).unwrap_or_default();
                let weighted = (group.kind == term_contracts::workload::GroupKind::Cgroup)
                    .then(|| group.reference.clone());
                state
                    .governance_ledger
                    .note_yielded(workload_id, members, weighted);
            } else if !yielding {
                state.governance_ledger.note_restored(workload_id);
            }
        }
        return outcome;
    }
    if !scheduling::per_process_supported() {
        // 트리를 훑기 전에 멈춘다: per-pid 경로가 없는 플랫폼(Linux)에서
        // 그룹 없는 워크로드는 애초에 대상이 아니다(§0-4).
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no resource group and no reversible per-process scheduling here",
        ));
    }
    // F1: 셸 트리의 닻은 스폰 때 찍어 둔 루트 신원이어야 한다(관측 경로와
    // 같은 닻). 맨 pid로 훑으면 셸이 죽고 pid가 재사용됐을 때 무고한
    // 프로세스의 트리가 완화 대상이 된다. 신원이 없는 셸(스폰 직후 종료해
    // 못 찍은 경우)은 완화 대상이 아니다.
    let Some(root) = state.shell_identity(workload_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "workload has neither a resource group nor a verified shell root",
        ));
    };
    let identities = crate::telemetry_loop::scan_shell_tree_verified(state, workload_id, &root);
    let outcome = scheduling::set_process_scheduling(&identities, op.tier());
    if let Ok(result) = &outcome {
        if yielding && result.applied > 0 {
            state
                .governance_ledger
                .note_yielded(workload_id, identities, None);
        } else if !yielding {
            state.governance_ledger.note_restored(workload_id);
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: u64 = 3_000;

    fn controller(auto_yield: bool) -> ReliefController {
        ReliefController::new(ReliefPolicy { auto_yield }, INTERVAL)
    }

    /// 독점 한도(코어)·지속 창 — 가드 정책의 값.
    const LIMIT: f64 = 6.0;
    const SUSTAIN: u64 = 20_000;

    /// CPU 독점 없이(측정값 없음) 도는 `plan` — 기존 시험들의 기본 경로.
    fn plan_no_hog(
        c: &mut ReliefController,
        now: u64,
        level: PressureLevel,
        focused: &[SessionId],
        live: &[LiveWorkload],
        supported: bool,
    ) -> Vec<ReliefOp> {
        c.plan(now, level, focused, live, supported, LIMIT, SUSTAIN)
    }

    fn workload(state: WorkloadState) -> LiveWorkload {
        LiveWorkload {
            workload_id: WorkloadId::generate(),
            session_id: SessionId::generate(),
            state,
            priority: 1,
            busy: false,
            cpu_cores: None,
        }
    }

    fn low_priority() -> LiveWorkload {
        let mut w = running();
        w.priority = LOW_PRIORITY;
        w
    }

    /// 이 코어 수를 쓰는 워크로드 한 개짜리 목록.
    fn using(w: &LiveWorkload, cores: f64) -> Vec<LiveWorkload> {
        let mut w = w.clone();
        w.cpu_cores = Some(cores);
        vec![w]
    }

    fn hog_plan(c: &mut ReliefController, now: u64, w: &LiveWorkload, cores: f64) -> Vec<ReliefOp> {
        c.plan(
            now,
            PressureLevel::Normal,
            &[],
            &using(w, cores),
            true,
            LIMIT,
            SUSTAIN,
        )
    }

    fn running() -> LiveWorkload {
        workload(WorkloadState::Running)
    }

    fn ok() -> io::Result<SchedulingOutcome> {
        Ok(SchedulingOutcome {
            applied: 2,
            failed: 0,
            skipped_reused: 0,
            skipped_denied: 0,
        })
    }

    fn partial() -> io::Result<SchedulingOutcome> {
        Ok(SchedulingOutcome {
            applied: 1,
            failed: 1,
            skipped_reused: 0,
            skipped_denied: 0,
        })
    }

    /// 계획을 그대로 적용해 기록까지 반영한다(성공 가정).
    fn settle(c: &mut ReliefController, ops: &[ReliefOp], now: u64) {
        for op in ops {
            c.record(op, &ok(), now);
        }
    }

    fn yielded_ids(ops: &[ReliefOp]) -> Vec<&str> {
        ops.iter()
            .filter_map(|op| match op {
                ReliefOp::Yield { workload_id } => Some(workload_id.as_str()),
                _ => None,
            })
            .collect()
    }

    fn restored_ids(ops: &[ReliefOp]) -> Vec<&str> {
        ops.iter()
            .filter_map(|op| match op {
                ReliefOp::Restore { workload_id } => Some(workload_id.as_str()),
                _ => None,
            })
            .collect()
    }

    /// §2: CRITICAL이면 포커스가 아닌 RUNNING 세션 **전부**를 한 번에
    /// 양보시킨다. 포커스 세션은 절대 대상이 아니다(§0-3).
    #[test]
    fn critical_yields_every_unfocused_running_workload_at_once() {
        let mut c = controller(true);
        let a = running();
        let b = running();
        let focused = running();
        let live = vec![a.clone(), b.clone(), focused.clone()];

        let ops = plan_no_hog(
            &mut c,
            1_000,
            PressureLevel::Critical,
            std::slice::from_ref(&focused.session_id),
            &live,
            true,
        );
        let mut got = yielded_ids(&ops);
        got.sort();
        let mut want = vec![a.workload_id.as_str(), b.workload_id.as_str()];
        want.sort();
        assert_eq!(got, want, "비포커스 RUNNING 전부, 포커스는 제외");
        settle(&mut c, &ops, 1_000);

        // 멱등: 이미 양보 중인 세션은 다시 계획되지 않는다.
        assert!(plan_no_hog(
            &mut c,
            2_000,
            PressureLevel::Critical,
            std::slice::from_ref(&focused.session_id),
            &live,
            true
        )
        .is_empty());
        assert!(matches!(
            c.view(&a.workload_id).0,
            ReliefState::Yielded {
                manual: false,
                partial: false,
                ..
            }
        ));
        assert_eq!(c.view(&focused.workload_id).0, ReliefState::None);
    }

    /// RUNNING이 아닌 워크로드(대기열·종료 중)는 손대지 않는다.
    #[test]
    fn only_running_workloads_are_yielded() {
        let mut c = controller(true);
        let queued = workload(WorkloadState::Queued);
        let run = running();
        let ops = plan_no_hog(
            &mut c,
            1_000,
            PressureLevel::Critical,
            &[],
            &[queued.clone(), run.clone()],
            true,
        );
        assert_eq!(yielded_ids(&ops), vec![run.workload_id.as_str()]);
    }

    /// 보호 표시(§0-3)는 압력과 무관하게 자동 대상에서 제외한다.
    #[test]
    fn protected_workloads_are_never_auto_yielded() {
        let mut c = controller(true);
        let guarded = running();
        let other = running();
        let live = vec![guarded.clone(), other.clone()];
        assert!(c
            .manual(
                &guarded.workload_id,
                ReliefAction::Protect,
                PressureLevel::Normal
            )
            .is_empty());
        assert!(c.view(&guarded.workload_id).1, "protected로 보고된다");

        let ops = plan_no_hog(&mut c, 1_000, PressureLevel::Critical, &[], &live, true);
        assert_eq!(yielded_ids(&ops), vec![other.workload_id.as_str()]);
        settle(&mut c, &ops, 1_000);

        // 보호를 풀면 다음 틱에 다시 대상이 된다.
        c.manual(
            &guarded.workload_id,
            ReliefAction::Unprotect,
            PressureLevel::Critical,
        );
        let ops = plan_no_hog(&mut c, 2_000, PressureLevel::Critical, &[], &live, true);
        assert_eq!(yielded_ids(&ops), vec![guarded.workload_id.as_str()]);
    }

    /// 보호를 걸면 이미 양보 중이던 세션도 즉시 복원된다.
    #[test]
    fn protect_restores_an_already_yielded_workload() {
        let mut c = controller(true);
        let w = running();
        let live = vec![w.clone()];
        let ops = plan_no_hog(&mut c, 1_000, PressureLevel::Critical, &[], &live, true);
        settle(&mut c, &ops, 1_000);
        assert!(c.view(&w.workload_id).0.is_yielded());

        let ops = c.manual(
            &w.workload_id,
            ReliefAction::Protect,
            PressureLevel::Critical,
        );
        assert_eq!(restored_ids(&ops), vec![w.workload_id.as_str()]);
        settle(&mut c, &ops, 1_100);
        assert_eq!(c.view(&w.workload_id).0, ReliefState::None);
        // 그리고 압력이 남아 있어도 다시 걸리지 않는다.
        assert!(plan_no_hog(&mut c, 2_000, PressureLevel::Critical, &[], &live, true).is_empty());
    }

    /// §2: NORMAL 회복 뒤에는 한 번에 하나씩 `relief_release_interval`
    /// 간격으로만 복원한다.
    #[test]
    fn normal_recovery_restores_one_at_a_time_on_the_release_interval() {
        let mut c = controller(true);
        let a = running();
        let b = running();
        let live = vec![a.clone(), b.clone()];
        let ops = plan_no_hog(&mut c, 1_000, PressureLevel::Critical, &[], &live, true);
        assert_eq!(ops.len(), 2);
        settle(&mut c, &ops, 1_000);

        // 첫 NORMAL 틱: 정확히 하나.
        let first = plan_no_hog(&mut c, 5_000, PressureLevel::Normal, &[], &live, true);
        assert_eq!(first.len(), 1, "한 번에 하나 (got {first:?})");
        settle(&mut c, &first, 5_000);

        // 간격 전에는 아무것도 풀지 않는다.
        assert!(plan_no_hog(
            &mut c,
            5_000 + INTERVAL - 1,
            PressureLevel::Normal,
            &[],
            &live,
            true
        )
        .is_empty());

        let second = plan_no_hog(
            &mut c,
            5_000 + INTERVAL,
            PressureLevel::Normal,
            &[],
            &live,
            true,
        );
        assert_eq!(second.len(), 1);
        settle(&mut c, &second, 5_000 + INTERVAL);
        assert_eq!(c.view(&a.workload_id).0, ReliefState::None);
        assert_eq!(c.view(&b.workload_id).0, ReliefState::None);
        // 더 풀 것이 없으면 조용하다.
        assert!(plan_no_hog(&mut c, 20_000, PressureLevel::Normal, &[], &live, true).is_empty());
    }

    /// 순차 복원은 가장 오래 양보한 것부터다.
    #[test]
    fn the_oldest_yield_is_released_first() {
        let mut c = controller(true);
        let old = running();
        let recent = running();
        let live = vec![old.clone(), recent.clone()];

        let ops = plan_no_hog(
            &mut c,
            1_000,
            PressureLevel::Critical,
            &[],
            std::slice::from_ref(&old),
            true,
        );
        settle(&mut c, &ops, 1_000);
        let ops = plan_no_hog(&mut c, 4_000, PressureLevel::Critical, &[], &live, true);
        settle(&mut c, &ops, 4_000);

        let first = plan_no_hog(&mut c, 9_000, PressureLevel::Normal, &[], &live, true);
        assert_eq!(restored_ids(&first), vec![old.workload_id.as_str()]);
    }

    /// §0-3: 포커스를 얻으면 압력 level과 무관하게 즉시 복원된다.
    #[test]
    fn gaining_focus_restores_immediately_even_under_pressure() {
        let mut c = controller(true);
        let w = running();
        let live = vec![w.clone()];
        let ops = plan_no_hog(&mut c, 1_000, PressureLevel::Critical, &[], &live, true);
        settle(&mut c, &ops, 1_000);
        assert!(c.view(&w.workload_id).0.is_yielded());

        let ops = plan_no_hog(
            &mut c,
            1_500,
            PressureLevel::Critical,
            std::slice::from_ref(&w.session_id),
            &live,
            true,
        );
        assert_eq!(restored_ids(&ops), vec![w.workload_id.as_str()]);
        settle(&mut c, &ops, 1_500);
        assert_eq!(c.view(&w.workload_id).0, ReliefState::None);
        // 그리고 포커스가 있는 한 다시 걸리지 않는다.
        assert!(plan_no_hog(
            &mut c,
            2_500,
            PressureLevel::Critical,
            std::slice::from_ref(&w.session_id),
            &live,
            true
        )
        .is_empty());
    }

    /// 수동 양보는 자동 복원되지 않는다(§0-2: 수동 > 자동).
    #[test]
    fn a_manual_yield_is_never_restored_by_the_recovery_path() {
        let mut c = controller(true);
        let w = running();
        let live = vec![w.clone()];
        let ops = c.manual(&w.workload_id, ReliefAction::Yield, PressureLevel::Normal);
        assert_eq!(yielded_ids(&ops), vec![w.workload_id.as_str()]);
        settle(&mut c, &ops, 1_000);
        assert!(matches!(
            c.view(&w.workload_id).0,
            ReliefState::Yielded { manual: true, .. }
        ));

        // NORMAL 회복도, 포커스도 수동 양보를 건드리지 않는다.
        assert!(plan_no_hog(&mut c, 9_000, PressureLevel::Normal, &[], &live, true).is_empty());
        assert!(plan_no_hog(
            &mut c,
            12_000,
            PressureLevel::Normal,
            std::slice::from_ref(&w.session_id),
            &live,
            true
        )
        .is_empty());
        // 사용자가 직접 복원할 때만 풀린다.
        let ops = c.manual(&w.workload_id, ReliefAction::Restore, PressureLevel::Normal);
        assert_eq!(restored_ids(&ops), vec![w.workload_id.as_str()]);
        settle(&mut c, &ops, 13_000);
        assert_eq!(c.view(&w.workload_id).0, ReliefState::None);
    }

    /// 자동 양보 중인 세션을 수동 양보로 바꾸면 OS 호출 없이 소유권만 넘어간다.
    #[test]
    fn manual_yield_takes_over_an_existing_auto_yield_without_a_new_os_call() {
        let mut c = controller(true);
        let w = running();
        let ops = plan_no_hog(
            &mut c,
            1_000,
            PressureLevel::Critical,
            &[],
            std::slice::from_ref(&w),
            true,
        );
        settle(&mut c, &ops, 1_000);

        let ops = c.manual(&w.workload_id, ReliefAction::Yield, PressureLevel::Critical);
        assert!(ops.is_empty(), "이미 걸린 정책을 다시 걸지 않는다");
        match c.view(&w.workload_id).0 {
            ReliefState::Yielded {
                manual, since_ms, ..
            } => {
                assert!(manual);
                assert_eq!(since_ms.get(), 1_000, "since_ms는 처음 걸린 시각 그대로");
            }
            other => panic!("still yielded expected, got {other:?}"),
        }
    }

    /// 압력 중 수동 복원한 세션은 NORMAL로 돌아갈 때까지 다시 양보되지
    /// 않고, NORMAL을 지나면 다시 대상이 된다.
    #[test]
    fn a_manual_restore_under_pressure_protects_until_normal_then_becomes_eligible() {
        let mut c = controller(true);
        let w = running();
        let live = vec![w.clone()];
        let ops = plan_no_hog(&mut c, 1_000, PressureLevel::Critical, &[], &live, true);
        settle(&mut c, &ops, 1_000);

        let ops = c.manual(
            &w.workload_id,
            ReliefAction::Restore,
            PressureLevel::Critical,
        );
        assert_eq!(restored_ids(&ops), vec![w.workload_id.as_str()]);
        settle(&mut c, &ops, 2_000);
        let (relief, protected) = c.view(&w.workload_id);
        assert_eq!(relief, ReliefState::None);
        assert!(protected, "압력이 풀릴 때까지 보호된다");

        // 압력이 남아 있는 동안은 계속 면제.
        assert!(plan_no_hog(&mut c, 3_000, PressureLevel::Critical, &[], &live, true).is_empty());
        assert!(plan_no_hog(&mut c, 4_000, PressureLevel::Critical, &[], &live, true).is_empty());

        // NORMAL을 한 번 지나면 일시 보호가 풀린다.
        assert!(plan_no_hog(&mut c, 5_000, PressureLevel::Normal, &[], &live, true).is_empty());
        assert!(!c.view(&w.workload_id).1, "NORMAL에서 보호가 풀린다");
        let ops = plan_no_hog(&mut c, 6_000, PressureLevel::Critical, &[], &live, true);
        assert_eq!(yielded_ids(&ops), vec![w.workload_id.as_str()]);
    }

    /// 압력이 없을 때의 수동 복원은 일시 보호를 만들지 않는다.
    #[test]
    fn a_manual_restore_at_normal_does_not_protect() {
        let mut c = controller(true);
        let w = running();
        c.manual(&w.workload_id, ReliefAction::Yield, PressureLevel::Normal);
        let ops = c.manual(&w.workload_id, ReliefAction::Restore, PressureLevel::Normal);
        assert!(ops.is_empty(), "걸린 적이 없으면 되돌릴 것도 없다");
        assert!(!c.view(&w.workload_id).1);
    }

    /// capability가 없으면 아무것도 계획하지 않는다(§0-4).
    #[test]
    fn an_unsupported_platform_plans_nothing() {
        let mut c = controller(true);
        let w = running();
        assert!(plan_no_hog(
            &mut c,
            1_000,
            PressureLevel::Critical,
            &[],
            std::slice::from_ref(&w),
            false
        )
        .is_empty());

        // capability는 지원한다고 했지만 실제 호출이 Unsupported를 돌려주면
        // **그 워크로드만** 더 시도하지 않는다 — 위임 cgroup Linux에서
        // 그룹 없는 직접 셸 하나가 나머지 완화까지 꺼서는 안 된다.
        let mut c = controller(true);
        let other = running();
        let live = vec![w.clone(), other.clone()];
        let ops = plan_no_hog(&mut c, 1_000, PressureLevel::Critical, &[], &live, true);
        assert_eq!(ops.len(), 2);
        let err: io::Result<SchedulingOutcome> =
            Err(io::Error::new(io::ErrorKind::Unsupported, "no"));
        for op in &ops {
            if op.workload_id() == &w.workload_id {
                assert!(!c.record(op, &err, 1_000), "실패는 상태를 바꾸지 않는다");
            } else {
                c.record(op, &ok(), 1_000);
            }
        }
        assert_eq!(c.view(&w.workload_id).0, ReliefState::None);
        assert!(c.is_unsupported(&w.workload_id));
        assert!(!c.is_unsupported(&other.workload_id));
        assert!(
            c.view(&other.workload_id).0.is_yielded(),
            "나머지 워크로드는 그대로 양보된다"
        );
        assert!(plan_no_hog(&mut c, 2_000, PressureLevel::Critical, &[], &live, true).is_empty());
    }

    /// 자동 양보 스위치가 꺼져 있으면 자동 계획은 없지만 수동은 그대로 된다.
    #[test]
    fn auto_yield_off_disables_automatic_planning_only() {
        let mut c = controller(false);
        let w = running();
        let live = vec![w.clone()];
        assert!(plan_no_hog(&mut c, 1_000, PressureLevel::Critical, &[], &live, true).is_empty());

        let ops = c.manual(&w.workload_id, ReliefAction::Yield, PressureLevel::Critical);
        assert_eq!(yielded_ids(&ops), vec![w.workload_id.as_str()]);
        settle(&mut c, &ops, 1_000);

        // 정책을 끄는 것만으로는 이미 걸린 양보가 풀리지 않는다.
        assert!(c.set_policy(ReliefPolicy { auto_yield: true }));
        assert!(!c.set_policy(ReliefPolicy { auto_yield: true }), "멱등");
    }

    /// 부분 적용은 `partial`로 보고되고 압력이 남아 있는 동안 다시 시도된다.
    #[test]
    fn a_partial_yield_is_reported_and_retried_while_pressure_persists() {
        let mut c = controller(true);
        let w = running();
        let live = vec![w.clone()];
        let ops = plan_no_hog(&mut c, 1_000, PressureLevel::Critical, &[], &live, true);
        assert!(c.record(&ops[0], &partial(), 1_000));
        assert!(matches!(
            c.view(&w.workload_id).0,
            ReliefState::Yielded { partial: true, .. }
        ));

        // 매 틱 다시 훑지 않는다 — 재시도 간격 뒤에 한 번.
        assert!(plan_no_hog(&mut c, 2_000, PressureLevel::Critical, &[], &live, true).is_empty());
        let retry = plan_no_hog(&mut c, 31_000, PressureLevel::Critical, &[], &live, true);
        assert_eq!(yielded_ids(&retry), vec![w.workload_id.as_str()]);
        assert!(c.record(&retry[0], &ok(), 31_000), "이제 완전히 걸렸다");
        match c.view(&w.workload_id).0 {
            ReliefState::Yielded {
                partial, since_ms, ..
            } => {
                assert!(!partial);
                assert_eq!(since_ms.get(), 1_000, "since_ms는 처음 걸린 시각");
            }
            other => panic!("yielded expected, got {other:?}"),
        }
        // 완전히 걸린 뒤에는 더 시도하지 않는다.
        assert!(plan_no_hog(&mut c, 62_000, PressureLevel::Critical, &[], &live, true).is_empty());
    }

    /// 사라진 워크로드의 기록은 버린다 — 유령에게 OS 호출을 보내지 않는다.
    #[test]
    fn records_of_dead_workloads_are_dropped_without_an_os_call() {
        let mut c = controller(true);
        let gone = running();
        let alive = running();
        let ops = plan_no_hog(
            &mut c,
            1_000,
            PressureLevel::Critical,
            &[],
            &[gone.clone(), alive.clone()],
            true,
        );
        settle(&mut c, &ops, 1_000);
        assert!(c.view(&gone.workload_id).0.is_yielded());

        // `gone`이 목록에서 빠진다: 복원 op 없이 기록만 사라진다.
        let ops = plan_no_hog(
            &mut c,
            9_000,
            PressureLevel::Normal,
            &[],
            std::slice::from_ref(&alive),
            true,
        );
        assert_eq!(restored_ids(&ops), vec![alive.workload_id.as_str()]);
        assert_eq!(c.view(&gone.workload_id).0, ReliefState::None);
        assert!(!c.view(&gone.workload_id).1);
    }

    /// 종료 경로는 양보 중인 모든 것을(수동 포함) 되돌린다(§0-4).
    #[test]
    fn shutdown_restores_everything_yielded() {
        let mut c = controller(true);
        let auto = running();
        let manual = running();
        let ops = plan_no_hog(
            &mut c,
            1_000,
            PressureLevel::Critical,
            &[],
            &[auto.clone(), manual.clone()],
            true,
        );
        settle(&mut c, &ops, 1_000);
        c.manual(
            &manual.workload_id,
            ReliefAction::Yield,
            PressureLevel::Critical,
        );

        let mut ops = restored_ids(&c.shutdown_restore_ops())
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        ops.sort();
        let mut want = vec![
            auto.workload_id.as_str().to_string(),
            manual.workload_id.as_str().to_string(),
        ];
        want.sort();
        assert_eq!(ops, want);
        assert_eq!(c.view(&auto.workload_id).0, ReliefState::None);
    }

    /// CPU 독점(가드 정책의 `cpu_cores_limit` 초과 지속)은 호스트 압력과
    /// 무관하게 — NORMAL에서도 — 양보 대상이 된다. 지속 창이 차기 전에는
    /// 아무것도 하지 않는다.
    #[test]
    fn cpu_hog_is_yielded_at_normal_after_sustain() {
        let mut c = controller(true);
        let w = running();
        assert!(hog_plan(&mut c, 0, &w, 8.0).is_empty());
        assert!(hog_plan(&mut c, 19_999, &w, 8.0).is_empty());
        let ops = hog_plan(&mut c, 20_000, &w, 8.0);
        assert_eq!(yielded_ids(&ops), vec![w.workload_id.as_str()]);
    }

    /// 포커스된 세션의 CPU 독점은 끝까지 양보하지 않는다(§0-2).
    #[test]
    fn cpu_hog_focused_is_never_yielded() {
        let mut c = controller(true);
        let w = running();
        for now in [0, 20_000, 120_000] {
            assert!(c
                .plan(
                    now,
                    PressureLevel::Normal,
                    std::slice::from_ref(&w.session_id),
                    &using(&w, 8.0),
                    true,
                    LIMIT,
                    SUSTAIN,
                )
                .is_empty());
        }
    }

    /// 보호(수동 Protect)·수동 양보된 독점은 자동 양보 대상이 아니다.
    #[test]
    fn cpu_hog_protected_or_manual_is_skipped() {
        let mut c = controller(true);
        let w = running();
        assert!(c
            .manual(&w.workload_id, ReliefAction::Protect, PressureLevel::Normal)
            .is_empty());
        for now in [0, 20_000, 120_000] {
            assert!(hog_plan(&mut c, now, &w, 8.0).is_empty());
        }

        // 수동 양보로 이미 걸려 있으면 그대로 둔다(새 op 없음).
        let mut c = controller(true);
        let w = running();
        let ops = c.manual(&w.workload_id, ReliefAction::Yield, PressureLevel::Normal);
        settle(&mut c, &ops, 1_000);
        for now in [0, 20_000, 120_000] {
            assert!(hog_plan(&mut c, now, &w, 8.0).is_empty());
        }
        assert!(c.view(&w.workload_id).0.is_yielded());
    }

    /// 양보된 독점은 측정값이 내려가도(효율 코어로 밀려 한도 아래가 돼도)
    /// 바로 풀리지 않는다 — 한도의 절반 미만이 30초 이어지고 최소 유지
    /// 시간이 지나야 복원된다.
    #[test]
    fn cpu_hog_holds_through_a_measured_drop_and_restores_once_quiet() {
        let mut c = controller(true);
        let w = running();
        assert!(hog_plan(&mut c, 0, &w, 8.0).is_empty());
        let ops = hog_plan(&mut c, 20_000, &w, 8.0);
        settle(&mut c, &ops, 20_000);
        assert!(c.view(&w.workload_id).0.is_yielded());
        // 양보 뒤 4코어(한도 아래지만 절반 이상): 계속 붙잡는다.
        for now in (21_000..=200_000).step_by(1_000) {
            assert!(hog_plan(&mut c, now, &w, 4.0).is_empty(), "t={now}");
        }
        // 조용해졌다(1코어): 30초 뒤에 복원.
        assert!(hog_plan(&mut c, 201_000, &w, 1.0).is_empty());
        assert!(hog_plan(&mut c, 230_999, &w, 1.0).is_empty());
        let ops = hog_plan(&mut c, 231_000, &w, 1.0);
        assert_eq!(restored_ids(&ops), vec![w.workload_id.as_str()]);
    }

    /// 조용해도 최소 유지 시간 전에는 풀지 않는다.
    #[test]
    fn cpu_hog_respects_the_minimum_hold() {
        let mut c = controller(true);
        let w = running();
        hog_plan(&mut c, 0, &w, 8.0);
        let ops = hog_plan(&mut c, 20_000, &w, 8.0);
        settle(&mut c, &ops, 20_000);
        // 곧바로 조용해져도 양보 후 60초 전에는 그대로.
        for now in (21_000..80_000).step_by(1_000) {
            assert!(hog_plan(&mut c, now, &w, 0.5).is_empty(), "t={now}");
        }
        let ops = hog_plan(&mut c, 80_000, &w, 0.5);
        assert_eq!(restored_ids(&ops), vec![w.workload_id.as_str()]);
    }

    /// 풀린 직후 다시 독점이면 유지 시간을 두 배로 늘린다.
    #[test]
    fn a_reyield_soon_after_release_doubles_the_hold() {
        let mut c = controller(true);
        let w = running();
        hog_plan(&mut c, 0, &w, 8.0);
        let ops = hog_plan(&mut c, 20_000, &w, 8.0);
        settle(&mut c, &ops, 20_000);
        for now in (21_000..80_000).step_by(1_000) {
            hog_plan(&mut c, now, &w, 0.5);
        }
        let ops = hog_plan(&mut c, 80_000, &w, 0.5);
        settle(&mut c, &ops, 80_000);
        // 다시 독점(풀린 지 2분 안).
        for now in (81_000..101_000).step_by(1_000) {
            assert!(hog_plan(&mut c, now, &w, 8.0).is_empty(), "t={now}");
        }
        let ops = hog_plan(&mut c, 101_000, &w, 8.0);
        assert_eq!(yielded_ids(&ops), vec![w.workload_id.as_str()]);
        settle(&mut c, &ops, 101_000);
        // 이번에는 120초를 붙잡는다.
        for now in (102_000..221_000).step_by(1_000) {
            assert!(hog_plan(&mut c, now, &w, 0.5).is_empty(), "t={now}");
        }
        let ops = hog_plan(&mut c, 221_000, &w, 0.5);
        assert_eq!(restored_ids(&ops), vec![w.workload_id.as_str()]);
    }

    /// 1초 틱에서 한 번 한도 아래로 내려간다고 독점 판정이 처음부터 다시
    /// 시작되지 않는다(링크 단계 같은 단일 스레드 구간).
    #[test]
    fn a_single_dip_does_not_reset_hog_detection() {
        let mut c = controller(true);
        let w = running();
        for now in (0..20_000).step_by(1_000) {
            let cores = if now == 10_000 { 1.0 } else { 8.0 };
            assert!(hog_plan(&mut c, now, &w, cores).is_empty(), "t={now}");
        }
        let ops = hog_plan(&mut c, 20_000, &w, 8.0);
        assert_eq!(yielded_ids(&ops), vec![w.workload_id.as_str()]);
    }

    /// 창의 대부분이 한도 아래였다면 독점이 아니다.
    #[test]
    fn cpu_hog_needs_most_of_the_window_over() {
        let mut c = controller(true);
        let w = running();
        assert!(hog_plan(&mut c, 0, &w, 8.0).is_empty());
        assert!(hog_plan(&mut c, 10_000, &w, 1.0).is_empty());
        assert!(hog_plan(&mut c, 10_001, &w, 8.0).is_empty());
        assert!(hog_plan(&mut c, 30_000, &w, 8.0).is_empty());
        let ops = hog_plan(&mut c, 30_001, &w, 8.0);
        assert_eq!(yielded_ids(&ops), vec![w.workload_id.as_str()]);
    }

    /// 독점 중 사용자가 직접 복원하면 독점이 끝날 때까지 다시 양보하지 않는다.
    #[test]
    fn a_manual_restore_of_a_hog_is_respected_until_it_quiets() {
        let mut c = controller(true);
        let w = running();
        hog_plan(&mut c, 0, &w, 8.0);
        let ops = hog_plan(&mut c, 20_000, &w, 8.0);
        settle(&mut c, &ops, 20_000);
        let ops = c.manual(&w.workload_id, ReliefAction::Restore, PressureLevel::Normal);
        assert_eq!(restored_ids(&ops), vec![w.workload_id.as_str()]);
        settle(&mut c, &ops, 21_000);
        for now in (22_000..=200_000).step_by(1_000) {
            assert!(hog_plan(&mut c, now, &w, 8.0).is_empty(), "t={now}");
        }
        // 독점이 끝나고(30초 조용) 다시 독점이 되면 그때는 다시 대상이다
        // (창의 80%가 다시 초과로 찰 때).
        for now in (201_000..=231_000).step_by(1_000) {
            assert!(hog_plan(&mut c, now, &w, 0.5).is_empty(), "t={now}");
        }
        let again = (232_000..=260_000)
            .step_by(1_000)
            .find(|now| !hog_plan(&mut c, *now, &w, 8.0).is_empty());
        assert!(
            again.is_some_and(|now| (240_000..=252_000).contains(&now)),
            "독점이 다시 차면 양보: {again:?}"
        );
    }

    /// P2-1: WARNING에서는 낮은 우선순위만 양보시키고, CRITICAL에서 나머지도.
    #[test]
    fn warning_yields_only_low_priority_and_critical_yields_all() {
        let mut c = controller(true);
        let normal = running();
        let low = low_priority();
        let live = vec![normal.clone(), low.clone()];
        let ops = plan_no_hog(&mut c, 1_000, PressureLevel::Warning, &[], &live, true);
        assert_eq!(yielded_ids(&ops), vec![low.workload_id.as_str()]);
        settle(&mut c, &ops, 1_000);
        let ops = plan_no_hog(&mut c, 2_000, PressureLevel::Critical, &[], &live, true);
        assert_eq!(yielded_ids(&ops), vec![normal.workload_id.as_str()]);
    }

    /// 되돌린 직후 압력이 다시 오면 다음 회복의 복원 간격이 늘어난다.
    #[test]
    fn pressure_reentry_slows_the_release_interval() {
        let mut c = controller(true);
        let a = running();
        let b = running();
        let live = vec![a.clone(), b.clone()];
        let ops = plan_no_hog(&mut c, 0, PressureLevel::Critical, &[], &live, true);
        settle(&mut c, &ops, 0);
        let ops = plan_no_hog(&mut c, 10_000, PressureLevel::Normal, &[], &live, true);
        assert_eq!(ops.len(), 1);
        settle(&mut c, &ops, 10_000);
        assert_eq!(c.current_release_interval_ms(), INTERVAL);
        // 복원 직후 압력 재발 → 간격 두 배.
        let ops = plan_no_hog(&mut c, 12_000, PressureLevel::Critical, &[], &live, true);
        settle(&mut c, &ops, 12_000);
        assert_eq!(c.current_release_interval_ms(), 2 * INTERVAL);
        // 다음 회복: 첫 복원 뒤 기본 간격으로는 두 번째가 나오지 않는다.
        let ops = plan_no_hog(&mut c, 20_000, PressureLevel::Normal, &[], &live, true);
        assert_eq!(ops.len(), 1);
        settle(&mut c, &ops, 20_000);
        assert!(plan_no_hog(
            &mut c,
            20_000 + INTERVAL,
            PressureLevel::Normal,
            &[],
            &live,
            true
        )
        .is_empty());
        assert_eq!(
            plan_no_hog(
                &mut c,
                20_000 + 2 * INTERVAL,
                PressureLevel::Normal,
                &[],
                &live,
                true
            )
            .len(),
            1
        );
        // NORMAL이 5분 이어지면 간격이 기본으로 돌아온다.
        plan_no_hog(
            &mut c,
            20_000 + STABLE_NORMAL_MS + 1_000,
            PressureLevel::Normal,
            &[],
            &live,
            true,
        );
        assert_eq!(c.current_release_interval_ms(), INTERVAL);
    }

    /// 실패한 복원은 백오프 동안 비켜서 다른 복원을 막지 않는다.
    #[test]
    fn a_failed_restore_backs_off_without_blocking_the_queue() {
        let mut c = controller(true);
        let old = running();
        let recent = running();
        let ops = plan_no_hog(
            &mut c,
            0,
            PressureLevel::Critical,
            &[],
            std::slice::from_ref(&old),
            true,
        );
        settle(&mut c, &ops, 0);
        let live = vec![old.clone(), recent.clone()];
        let ops = plan_no_hog(&mut c, 1_000, PressureLevel::Critical, &[], &live, true);
        settle(&mut c, &ops, 1_000);
        // 가장 오래된 것의 복원이 실패한다.
        let ops = plan_no_hog(&mut c, 5_000, PressureLevel::Normal, &[], &live, true);
        assert_eq!(restored_ids(&ops), vec![old.workload_id.as_str()]);
        let eperm = Err(io::Error::new(io::ErrorKind::PermissionDenied, "EPERM"));
        assert!(!c.record(&ops[0], &eperm, 5_000));
        // 다음 슬롯은 다른 것에게.
        let ops = plan_no_hog(
            &mut c,
            5_000 + INTERVAL,
            PressureLevel::Normal,
            &[],
            &live,
            true,
        );
        assert_eq!(restored_ids(&ops), vec![recent.workload_id.as_str()]);
    }

    /// 종료 경로가 시작되면 새 양보를 내지 않는다.
    #[test]
    fn shutdown_stops_new_yields() {
        let mut c = controller(true);
        let w = running();
        assert!(c.shutdown_restore_ops().is_empty());
        assert!(plan_no_hog(
            &mut c,
            1_000,
            PressureLevel::Critical,
            &[],
            std::slice::from_ref(&w),
            true
        )
        .is_empty());
    }
}
