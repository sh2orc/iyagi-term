//! 자원 가드(08 §5 — 강한 자동 제어).
//!
//! 한 워크로드의 귀속 메모리가 한도를 지속적으로 초과하면(호스트에 메모리
//! 압력이 있을 때만) **일시정지**한다. CPU 초과는 얼리지 않는다 — 빌드·
//! 테스트의 정상 동작이고, 경합은 완화(relief)가 background 등급으로
//! 양보해 나눠 쓴다. 정지는 되돌릴 수 있다: 검증된 트리에 SIGSTOP/
//! SIGCONT(위임 cgroup Linux는 freeze)를 걸고 사용자가 언제든 재개한다.
//!
//! [`GuardController`]는 [`crate::relief::ReliefController`]와 같은 구조다:
//! 순수 결정 코어(`plan`/`manual`)는 시계도 OS도 모르고, [`apply`]가 그
//! 결정을 OS에 건다. 불변 조건:
//!
//! 1. 보이는 세션(포커스·화면에 보이는 pane)과 보호 표시 세션은 절대 자동으로
//!    정지하지 않는다 — 그런 세션이 메모리 압력의 주범이면 다른 것을 대신
//!    얼리지 않고 경고(`guard_warning`)만 올린다.
//! 2. 수동 정지는 자동 재개 대상이 아니고, 사용자가 직접 재개한 세션은
//!    메모리가 NORMAL로 돌아올 때까지 다시 자동 정지하지 않는다.
//! 3. 호스트 메모리 CRITICAL에서는 **메모리가 늘고 있는** 비면제 워크로드를
//!    먼저 얼린다(SIGSTOP은 메모리를 돌려주지 않지만 증가는 멈춘다). 늘고
//!    있는 것이 없으면 에피소드가 [`STATIC_VICTIM_DELAY_MS`] 이어진 뒤에야
//!    가장 큰 하나만 얼린다. 정지 사이에는 [`PRESSURE_SUSPEND_INTERVAL_MS`]를
//!    두고(직전 정지의 효과를 볼 시간), 한 에피소드에
//!    [`PRESSURE_EPISODE_MAX_SUSPENDS`]개를 넘기지 않는다 — 예전에는 3초마다
//!    하나씩 상한 없이 얼려 30초 만에 열 개가 멈췄다.
//! 4. 모든 정지는 되돌릴 수 있고 데몬 종료 경로에서 재개된다. 종료가 시작되면
//!    ([`GuardController::shutdown_resume_ops`]) 새 정지를 내지 않는다.
//! 5. 자동 정지된 세션이 보이면 다음 틱에 재개한다 — 사용자가 돌아와 보는
//!    터미널이 말없이 얼어 있지 않게. 보이는 동안에는 초과 지속 시간도 세지
//!    않는다. 수동 정지는 예외(불변 2). 호스트 메모리 CRITICAL 정지도 예외다 —
//!    사유가 살아 있는 동안에는 보는 것만으로 풀지 않는다.
//! 6. 증가하지 않는 워크로드를 CRITICAL에서 얼릴 때는
//!    [`PRESSURE_SUSPEND_MIN_RSS`] 이상인 것만 고른다 — 놀고 있는 셸까지
//!    얼리지 않게.
//! 7. 메모리 회복(NORMAL) 재개는 정책의 `auto_suspend`와 무관하게 돈다(자동
//!    정지를 꺼도 이미 얼어 있는 것은 풀려야 한다). 정지 시점 RSS만큼 호스트
//!    여유가 없으면 기다린다(풀자마자 다시 압력으로 떨어지지 않게).

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::Arc;

use term_contracts::ids::{SessionId, U64String, WorkloadId};
use term_contracts::metrics::PressureLevel;
use term_contracts::snapshot::{GuardPolicy, GuardReason, GuardState};
use term_contracts::state::WorkloadState;
use term_platform::group::{scheduling, SchedulingOutcome};

use crate::state::DaemonState;

/// 증가하지 않는 워크로드를 메모리 압력으로 얼릴 때의 하한: 이보다 작은
/// 업은 얼려 봐야 압력 해소에 기여하지 못한다(불변 6).
pub const PRESSURE_SUSPEND_MIN_RSS: u64 = 512 * 1024 * 1024;

/// 호스트 압력 정지 사이의 최소 간격 — 직전 정지가 증가를 멈췄는지 볼 시간.
pub const PRESSURE_SUSPEND_INTERVAL_MS: u64 = 10_000;

/// 한 CRITICAL 에피소드에서 압력 사유로 얼리는 상한.
pub const PRESSURE_EPISODE_MAX_SUSPENDS: u32 = 3;

/// 메모리 증가를 재는 창.
pub const GROWTH_WINDOW_MS: u64 = 15_000;

/// 이만큼 늘어야 "늘고 있다"로 본다(창 안의 최저점 대비).
pub const GROWTH_MIN_BYTES: u64 = 64 * 1024 * 1024;

/// 늘고 있는 것이 없을 때 정적인 큰 워크로드를 얼리기 전, CRITICAL
/// 에피소드가 이어져야 하는 시간(증가 감지에 먼저 기회를 준다).
pub const STATIC_VICTIM_DELAY_MS: u64 = 10_000;

/// 한 에피소드에서 증가하지 않는(정적) 워크로드를 얼리는 상한.
const STATIC_VICTIMS_PER_EPISODE: u32 = 1;

/// 메모리 회복 재개 간격(3초에 하나 — 한꺼번에 풀면 RSS가 한 번에 돌아와
/// 다시 압력으로 떨어진다).
const PRESSURE_RESUME_INTERVAL_MS: u64 = 3_000;

/// 자동 Resume 재시도 간격. 실패한 재개를 매 틱 다시 밀지 않는다(포커스
/// 재개·자동 재개 공통). 사람이 기다리기엔 짧고, 가드 틱(1s)보다는 길다.
const RESUME_RETRY_BACKOFF_MS: u64 = 10_000;

/// 한 워크로드에 대해 가드가 지시하는 OS 조작.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardOp {
    Suspend {
        workload_id: WorkloadId,
        reason: GuardReason,
    },
    Resume {
        workload_id: WorkloadId,
    },
}

impl GuardOp {
    pub fn workload_id(&self) -> &WorkloadId {
        match self {
            GuardOp::Suspend { workload_id, .. } | GuardOp::Resume { workload_id } => workload_id,
        }
    }
}

/// 정책 입력 한 건: 살아 있는 워크로드와 그 귀속 사용량.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveUsage {
    pub workload_id: WorkloadId,
    pub session_id: SessionId,
    pub state: WorkloadState,
    /// 귀속 논리 코어(측정·추정만; `None`은 미측정).
    pub cpu_cores: Option<f64>,
    /// 귀속 상주 바이트(측정·추정만).
    pub resident_bytes: Option<u64>,
    /// 완화 쪽 보호 표시 — 자동 정지(개별 한도·호스트 압력) 대상에서 뺀다.
    pub protected: bool,
    /// 스케줄링 우선순위(0 높음 ~ 2 낮음). 정적 희생 선택에서 낮은 것이 먼저다.
    pub priority: u8,
    /// 에이전트가 "busy"라고 보고하는 중이다. 정적 희생 선택에서 일하는 중인
    /// 것은 뒤로 미룬다(얼려도 메모리는 그대로라 덜 해로운 쪽을 고른다).
    pub busy: bool,
}

/// 워크로드 하나의 가드 기록.
#[derive(Debug, Clone, Default, PartialEq)]
struct Record {
    state: GuardState,
    /// 한도 초과가 시작된 단조 시각(초과 중에만).
    over_since_ms: Option<u64>,
    /// 이 경로가 Unsupported로 판명났다 — 다시 시도하지 않는다.
    unsupported: bool,
    /// 자동 Resume push 뒤 이 시각까지는 다시 밀지 않는다. 성공한 재개는
    /// 리셋되지만, 실패한 재개(EPERM 등)는 상태가 Suspended에 남아 매 틱
    /// 같은 OS 호출을 반복하는 무한 재시도가 되는 것을 이 간격이 막는다.
    resume_retry_after_ms: Option<u64>,
    /// 최근 [`GROWTH_WINDOW_MS`]의 (시각, 상주 바이트) 표본 — 증가량 계산용.
    rss_history: VecDeque<(u64, u64)>,
    /// 마지막으로 관측한 상주 바이트.
    last_rss: Option<u64>,
    /// 정지에 성공했을 때의 상주 바이트(회복 재개의 여유 판정).
    rss_at_suspend: Option<u64>,
    /// 사용자가 직접 재개했다 — 메모리가 NORMAL로 돌아올 때까지 자동 정지
    /// 대상이 아니다(불변 2).
    manual_resume_exempt: bool,
    /// `manual(.., false)`가 낸 재개가 아직 기록되지 않았다(성공해야 위의
    /// 면제가 선다).
    pending_manual_resume: bool,
}

impl Record {
    /// 창 안의 최저점 대비 지금의 증가량(표본이 모자라면 0).
    fn growth(&self) -> u64 {
        let (Some(last), Some(min)) = (
            self.last_rss,
            self.rss_history.iter().map(|(_, rss)| *rss).min(),
        ) else {
            return 0;
        };
        last.saturating_sub(min)
    }
}

/// 한 번의 호스트 메모리 CRITICAL 구간.
#[derive(Debug, Clone, Default, PartialEq)]
struct PressureEpisode {
    started_ms: u64,
    suspends: u32,
    static_suspends: u32,
}

/// 가드의 결정 코어. `DaemonState`가 `Mutex`로 하나만 소유한다.
pub struct GuardController {
    records: HashMap<WorkloadId, Record>,
    policy: GuardPolicy,
    /// 마지막 호스트 압력 정지 시각(속도 제한).
    last_pressure_suspend_ms: Option<u64>,
    /// 마지막 메모리 회복 재개 시각.
    last_pressure_resume_ms: Option<u64>,
    /// 진행 중인 CRITICAL 에피소드(CRITICAL이 아니면 None).
    episode: Option<PressureEpisode>,
    /// 호스트 여유(가용 − 예비량 S). 회복 재개의 여유 판정에 쓴다. 모르면
    /// None(판정 없이 진행).
    headroom_bytes: Option<i64>,
    /// 지금 경고 중인 워크로드(포커스·보호된 압력 주범).
    warnings: HashMap<WorkloadId, GuardReason>,
    /// 지난 틱 이후 경고가 바뀐 워크로드 — 호출자가 `workload.changed`로 알린다.
    warning_changes: HashSet<WorkloadId>,
    /// 종료 경로가 시작됐다 — 더 이상 정지를 내지 않는다(불변 4).
    closing: bool,
}

impl GuardController {
    pub fn new(policy: GuardPolicy) -> Self {
        Self {
            records: HashMap::new(),
            policy,
            last_pressure_suspend_ms: None,
            last_pressure_resume_ms: None,
            episode: None,
            headroom_bytes: None,
            warnings: HashMap::new(),
            warning_changes: HashSet::new(),
            closing: false,
        }
    }

    pub fn policy(&self) -> GuardPolicy {
        self.policy.clone()
    }

    pub fn set_policy(&mut self, policy: GuardPolicy) -> bool {
        let changed = self.policy != policy;
        self.policy = policy;
        changed
    }

    /// 다음 `plan`이 쓸 호스트 여유(가용 − 예비량 S, 음수 가능). 모르면 None.
    pub fn set_host_headroom(&mut self, headroom_bytes: Option<i64>) {
        self.headroom_bytes = headroom_bytes;
    }

    /// 스냅샷 보고용 `(guard, warning)`.
    pub fn view(&self, workload_id: &WorkloadId) -> (GuardState, Option<GuardReason>) {
        let warning = self.warnings.get(workload_id).copied();
        match self.records.get(workload_id) {
            Some(record) => (record.state.clone(), warning),
            None => (GuardState::None, warning),
        }
    }

    /// 지난 호출 이후 경고가 바뀐 워크로드(호출자가 `workload.changed`로 알린다).
    pub fn take_warning_changes(&mut self) -> Vec<WorkloadId> {
        let mut ids: Vec<WorkloadId> = self.warning_changes.drain().collect();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids
    }

    /// 죽은 워크로드의 기록을 버린다.
    fn retain_live(&mut self, live: &[LiveUsage]) {
        let ids: HashSet<&str> = live.iter().map(|w| w.workload_id.as_str()).collect();
        self.records.retain(|id, _| ids.contains(id.as_str()));
        let gone: Vec<WorkloadId> = self
            .warnings
            .keys()
            .filter(|id| !ids.contains(id.as_str()))
            .cloned()
            .collect();
        for id in gone {
            self.warnings.remove(&id);
        }
    }

    fn set_warnings(&mut self, next: HashMap<WorkloadId, GuardReason>) {
        for (id, reason) in &next {
            if self.warnings.get(id) != Some(reason) {
                self.warning_changes.insert(id.clone());
            }
        }
        for id in self.warnings.keys() {
            if !next.contains_key(id) {
                self.warning_changes.insert(id.clone());
            }
        }
        self.warnings = next;
    }

    /// 한 틱의 계획. 부수효과는 기록 정리뿐이고 OS를 건드리지 않는다.
    ///
    /// `exempt`는 자동 정지에서 빠지는 세션 — 포커스된 세션과 화면에 보이는
    /// 세션 전부다(호출자가 합쳐서 넘긴다).
    pub fn plan(
        &mut self,
        now_ms: u64,
        cpu_level: PressureLevel,
        mem_level: PressureLevel,
        exempt: &[SessionId],
        live: &[LiveUsage],
        cpu_recovery_used_percent: Option<f64>,
    ) -> Vec<GuardOp> {
        self.retain_live(live);
        if self.closing {
            return Vec::new();
        }
        let exempt: HashSet<&str> = exempt.iter().map(SessionId::as_str).collect();
        // 결정은 결정적이어야 한다(시험 재현성): workload_id 순.
        let mut ordered: Vec<&LiveUsage> = live.iter().collect();
        ordered.sort_by(|a, b| a.workload_id.as_str().cmp(b.workload_id.as_str()));

        // 표본 갱신(증가량) + 메모리 NORMAL이면 수동 재개 면제 해제.
        for workload in &ordered {
            let record = self
                .records
                .entry(workload.workload_id.clone())
                .or_default();
            record.last_rss = workload.resident_bytes;
            if let Some(rss) = workload.resident_bytes {
                record.rss_history.push_back((now_ms, rss));
            }
            while record
                .rss_history
                .front()
                .is_some_and(|(at, _)| now_ms.saturating_sub(*at) > GROWTH_WINDOW_MS)
            {
                record.rss_history.pop_front();
            }
            if mem_level == PressureLevel::Normal {
                record.manual_resume_exempt = false;
            }
        }
        if mem_level != PressureLevel::Critical {
            self.episode = None;
        }

        let mut ops = Vec::new();
        // 메모리 초과의 지속 창 — CPU의 sustain_ms(양보 판정이 쓴다)와 별개다.
        let sustain = self.policy.rss_sustain_ms.get().max(1);
        let rss_limit = self.policy.rss_limit_bytes.get().max(1);

        for workload in &ordered {
            if workload.state != WorkloadState::Running {
                continue;
            }
            // CPU 초과는 정지 사유가 아니다(완화가 양보로 다룬다). 메모리
            // 초과도 호스트가 NORMAL이면 얼리지 않는다: 호스트에 여유가 있는데
            // 큰 워크로드 하나를 얼려 봐야 얻는 게 없고, SIGSTOP은 RSS를
            // 줄이지도 않는다.
            let over = mem_level != PressureLevel::Normal
                && workload.resident_bytes.is_some_and(|r| r > rss_limit);
            let reason = GuardReason::MemoryLimit;
            let id = &workload.workload_id;
            let record = self.records.entry(id.clone()).or_default();

            if exempt.contains(workload.session_id.as_str()) {
                // 불변 1·5: 보이는 세션은 얼리지 않고, 자동 정지돼 있었다면
                // 되살린다. 초과 지속 시간은 보이는 동안 세지 않는다. 실패한
                // 재개(Unsupported·EPERM)를 매 틱 다시 밀지 않는다 — 백오프와
                // unsupported 가드가 없으면 OS 호출+로그가 무한히 반복된다.
                record.over_since_ms = None;
                if matches!(&record.state, GuardState::Suspended { manual: false, .. })
                    && !record.unsupported
                    && now_ms >= record.resume_retry_after_ms.unwrap_or(0)
                    // 예외: 호스트 메모리 CRITICAL 정지는 그 사유가 살아 있는
                    // 동안 보는 것만으로 풀지 않는다 — SIGSTOP은 메모리를 돌려주지
                    // 않으므로 해동이 곧 재정지 진동이 된다. 배지의 수동 재개
                    // 버튼과 메모리 회복이 푼다.
                    && !(matches!(
                        &record.state,
                        GuardState::Suspended {
                            reason: GuardReason::HostMemoryPressure,
                            ..
                        }
                    ) && mem_level == PressureLevel::Critical)
                {
                    record.resume_retry_after_ms = Some(now_ms + RESUME_RETRY_BACKOFF_MS);
                    ops.push(GuardOp::Resume {
                        workload_id: id.clone(),
                    });
                }
                continue;
            }

            // 보호 표시·수동 재개 면제는 자동 정지 대상이 아니다 — 아래
            // else에서 over_since_ms가 None으로 유지된다.
            if over && !workload.protected && !record.manual_resume_exempt {
                record.over_since_ms.get_or_insert(now_ms);
                let over_for = now_ms.saturating_sub(record.over_since_ms.unwrap_or(now_ms));
                let already = record.state.is_suspended();
                if over_for >= sustain
                    && (!already
                        || matches!(&record.state, GuardState::Suspended { partial: true, .. }))
                    && self.policy.auto_suspend
                    && !record.unsupported
                {
                    ops.push(GuardOp::Suspend {
                        workload_id: id.clone(),
                        reason,
                    });
                }
            } else {
                record.over_since_ms = None;
                // 자동 재개(기본 off): 한도 아래로 충분히 오래 조용하고
                // 호스트가 회복 구간이면 스스로 돌아온다. 메모리 사유로 얼린
                // 것은 메모리도 NORMAL이어야 한다(CPU만 회복됐다고 풀면 곧장
                // 다시 압력이다). 여기도 같은 백오프·unsupported 가드를 지난다.
                if self.policy.auto_resume {
                    let eligible = match &record.state {
                        GuardState::Suspended {
                            since_ms,
                            manual,
                            reason,
                            ..
                        } => {
                            let quiet_for = now_ms.saturating_sub(since_ms.get());
                            let host_recovered = cpu_level == PressureLevel::Normal
                                || cpu_recovery_used_percent.is_some_and(|used| used < 60.0);
                            let memory_ok = !matches!(
                                reason,
                                GuardReason::MemoryLimit | GuardReason::HostMemoryPressure
                            ) || mem_level == PressureLevel::Normal;
                            !*manual && quiet_for >= 10 * 60_000 && host_recovered && memory_ok
                        }
                        GuardState::None => false,
                    };
                    if eligible
                        && !record.unsupported
                        && now_ms >= record.resume_retry_after_ms.unwrap_or(0)
                    {
                        record.resume_retry_after_ms = Some(now_ms + RESUME_RETRY_BACKOFF_MS);
                        ops.push(GuardOp::Resume {
                            workload_id: id.clone(),
                        });
                    }
                }
            }
        }

        // 호스트 메모리 CRITICAL: 늘고 있는 것부터(불변 3), 면제된 주범은
        // 경고만(불변 1).
        let mut warnings = HashMap::new();
        if mem_level == PressureLevel::Critical {
            let episode_started = self.episode.get_or_insert(PressureEpisode {
                started_ms: now_ms,
                ..PressureEpisode::default()
            });
            let episode_age = now_ms.saturating_sub(episode_started.started_ms);
            let running: Vec<&LiveUsage> = ordered
                .iter()
                .copied()
                .filter(|w| w.state == WorkloadState::Running)
                .collect();
            let growth_of = |w: &LiveUsage| -> u64 {
                self.records
                    .get(&w.workload_id)
                    .map(Record::growth)
                    .unwrap_or(0)
            };
            let is_exempt = |w: &LiveUsage| -> bool {
                exempt.contains(w.session_id.as_str())
                    || w.protected
                    || self
                        .records
                        .get(&w.workload_id)
                        .is_some_and(|r| r.manual_resume_exempt)
            };
            let suspended_or_unsupported = |w: &LiveUsage| -> bool {
                self.records
                    .get(&w.workload_id)
                    .is_some_and(|r| r.state.is_suspended() || r.unsupported)
            };
            // 주범: 창 안에서 가장 많이 늘어난 워크로드(면제 여부 무관).
            let culprit = running
                .iter()
                .copied()
                .filter(|w| !suspended_or_unsupported(w))
                .map(|w| (growth_of(w), w))
                .filter(|(growth, _)| *growth >= GROWTH_MIN_BYTES)
                .max_by(|a, b| {
                    a.0.cmp(&b.0)
                        .then_with(|| b.1.workload_id.as_str().cmp(a.1.workload_id.as_str()))
                })
                .map(|(_, w)| w);
            let planned: HashSet<&str> = ops.iter().map(|op| op.workload_id().as_str()).collect();
            if let Some(culprit) = culprit.filter(|w| is_exempt(w)) {
                // 보이는·보호된 세션이 압력을 만들고 있다: 다른 것을 얼려 봐야
                // 증가는 멈추지 않는다. 그 세션에 경고만 올린다.
                warnings.insert(culprit.workload_id.clone(), GuardReason::HostMemoryPressure);
            } else if self.policy.auto_suspend {
                let due = self
                    .last_pressure_suspend_ms
                    .is_none_or(|last| now_ms.saturating_sub(last) >= PRESSURE_SUSPEND_INTERVAL_MS);
                let episode = self.episode.clone().unwrap_or_default();
                if due && episode.suspends < PRESSURE_EPISODE_MAX_SUSPENDS {
                    let eligible = |w: &&LiveUsage| {
                        !is_exempt(w)
                            && !suspended_or_unsupported(w)
                            && !planned.contains(w.workload_id.as_str())
                    };
                    let growing = culprit.filter(eligible);
                    let target = match growing {
                        Some(w) => Some((w.workload_id.clone(), false)),
                        None if episode_age >= STATIC_VICTIM_DELAY_MS
                            && episode.static_suspends < STATIC_VICTIMS_PER_EPISODE =>
                        {
                            // 정적 희생: 낮은 우선순위 → 일하지 않는 것 → 큰 것.
                            running
                                .iter()
                                .copied()
                                .filter(eligible)
                                .filter_map(|w| w.resident_bytes.map(|rss| (rss, w)))
                                .filter(|(rss, _)| *rss >= PRESSURE_SUSPEND_MIN_RSS)
                                .max_by(|a, b| {
                                    a.1.priority
                                        .cmp(&b.1.priority)
                                        .then_with(|| b.1.busy.cmp(&a.1.busy))
                                        .then_with(|| a.0.cmp(&b.0))
                                        .then_with(|| {
                                            b.1.workload_id.as_str().cmp(a.1.workload_id.as_str())
                                        })
                                })
                                .map(|(_, w)| (w.workload_id.clone(), true))
                        }
                        None => None,
                    };
                    if let Some((id, is_static)) = target {
                        self.last_pressure_suspend_ms = Some(now_ms);
                        if let Some(episode) = self.episode.as_mut() {
                            episode.suspends += 1;
                            if is_static {
                                episode.static_suspends += 1;
                            }
                        }
                        ops.push(GuardOp::Suspend {
                            workload_id: id,
                            reason: GuardReason::HostMemoryPressure,
                        });
                    }
                }
            }
        }
        self.set_warnings(warnings);

        // 호스트 메모리 NORMAL: 메모리 사유(호스트 압력·개별 한도)로 자동 정지된
        // 워크로드를 3초에 하나씩, 가장 나중에 얼린 것부터 되살린다(불변 7 —
        // 정책의 auto_suspend와 무관). 수동 정지는 손대지 않는다. 보이는 세션
        // 재개는 위 루프가 이미 처리했다(planned에 있으면 건너뛴다). 정지
        // 시점 RSS만큼 호스트 여유가 없으면 이번에는 풀지 않는다.
        if mem_level == PressureLevel::Normal {
            let due = self
                .last_pressure_resume_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= PRESSURE_RESUME_INTERVAL_MS);
            if due {
                let planned: HashSet<&str> =
                    ops.iter().map(|op| op.workload_id().as_str()).collect();
                let headroom = self.headroom_bytes;
                let target = ordered
                    .iter()
                    .filter(|w| !planned.contains(w.workload_id.as_str()))
                    .filter_map(|w| {
                        let record = self.records.get(&w.workload_id)?;
                        if record.unsupported || now_ms < record.resume_retry_after_ms.unwrap_or(0)
                        {
                            return None;
                        }
                        let fits = match (headroom, record.rss_at_suspend) {
                            (Some(headroom), Some(rss)) => {
                                headroom >= i64::try_from(rss).unwrap_or(i64::MAX)
                            }
                            _ => true,
                        };
                        if !fits {
                            return None;
                        }
                        match &record.state {
                            GuardState::Suspended {
                                since_ms,
                                manual: false,
                                reason: GuardReason::HostMemoryPressure | GuardReason::MemoryLimit,
                                ..
                            } => Some((since_ms.get(), w.workload_id.clone())),
                            _ => None,
                        }
                    })
                    .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.as_str().cmp(b.1.as_str())));
                if let Some((_, id)) = target {
                    self.last_pressure_resume_ms = Some(now_ms);
                    if let Some(record) = self.records.get_mut(&id) {
                        record.resume_retry_after_ms = Some(now_ms + RESUME_RETRY_BACKOFF_MS);
                    }
                    ops.push(GuardOp::Resume { workload_id: id });
                }
            }
        }
        ops
    }

    /// 수동 조작(`workload.suspend`/`workload.resume`). 상태는 OS 조작이
    /// 성공했을 때 [`Self::record`]가 바꾼다 — 실패한 조작을 "정지됨"으로
    /// 광고하지 않는다. 수동 정지는 자동 재개 대상이 아니고(불변 2), 수동
    /// 재개는 메모리가 NORMAL로 돌아올 때까지 자동 정지를 막는다.
    pub fn manual(&mut self, workload_id: &WorkloadId, suspend: bool) -> Vec<GuardOp> {
        let record = self.records.entry(workload_id.clone()).or_default();
        let already = record.state.is_suspended();
        if suspend && !already {
            vec![GuardOp::Suspend {
                workload_id: workload_id.clone(),
                reason: GuardReason::Manual,
            }]
        } else if !suspend && already {
            record.pending_manual_resume = true;
            vec![GuardOp::Resume {
                workload_id: workload_id.clone(),
            }]
        } else {
            Vec::new()
        }
    }

    /// 종료 경로: 정지 중인 모든 워크로드를 재개한다(불변 4). 이 뒤로는
    /// `plan`이 아무것도 내지 않는다 — 텔레메트리 틱이 재개와 경합해 다시
    /// 얼리지 않게.
    pub fn shutdown_resume_ops(&mut self) -> Vec<GuardOp> {
        self.closing = true;
        let mut ops: Vec<GuardOp> = self
            .records
            .iter()
            .filter(|(_, record)| record.state.is_suspended())
            .map(|(workload_id, _)| GuardOp::Resume {
                workload_id: workload_id.clone(),
            })
            .collect();
        ops.sort_by(|a, b| a.workload_id().as_str().cmp(b.workload_id().as_str()));
        for record in self.records.values_mut() {
            record.state = GuardState::None;
        }
        ops
    }

    /// 지금 정지 중인 워크로드(크래시 복구 기록·진단용).
    pub fn suspended_ids(&self) -> Vec<WorkloadId> {
        let mut ids: Vec<WorkloadId> = self
            .records
            .iter()
            .filter(|(_, record)| record.state.is_suspended())
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids
    }

    /// 한 조작의 결과를 기록한다. 상태가 실제로 바뀌었으면 true.
    pub fn record(
        &mut self,
        op: &GuardOp,
        outcome: &io::Result<SchedulingOutcome>,
        now_ms: u64,
    ) -> bool {
        let workload_id = op.workload_id().clone();
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                let record = self.records.entry(workload_id.clone()).or_default();
                record.pending_manual_resume = false;
                if error.kind() == io::ErrorKind::Unsupported {
                    record.unsupported = true;
                }
                tracing::debug!(%workload_id, ?op, %error, "guard op failed");
                return false;
            }
        };
        let record = self.records.entry(workload_id).or_default();
        // 조작이 실제로 적용됐다 — 재시도 백오프는 실패에만 의미가 있다.
        record.resume_retry_after_ms = None;
        let next = match op {
            GuardOp::Suspend { reason, .. } => {
                let manual = *reason == GuardReason::Manual
                    || matches!(&record.state, GuardState::Suspended { manual: true, .. });
                let since_ms = match &record.state {
                    GuardState::Suspended { since_ms, .. } => since_ms.clone(),
                    GuardState::None => U64String::new(now_ms)
                        .unwrap_or_else(|_| U64String::new(0).expect("0 in range")),
                };
                if !record.state.is_suspended() {
                    record.rss_at_suspend = record.last_rss;
                }
                record.over_since_ms = None;
                record.manual_resume_exempt = false;
                GuardState::Suspended {
                    since_ms,
                    reason: *reason,
                    manual,
                    partial: outcome.is_partial(),
                }
            }
            GuardOp::Resume { .. } => {
                if std::mem::take(&mut record.pending_manual_resume) {
                    record.manual_resume_exempt = true;
                }
                record.over_since_ms = None;
                record.rss_at_suspend = None;
                GuardState::None
            }
        };
        let changed = record.state != next;
        record.state = next;
        changed
    }
}

/// 결정 하나를 OS에 건다(양보의 [`crate::relief::apply`]와 같은 구조). 성공한
/// 정지·재개는 크래시 안전 장부에 남긴다.
pub fn apply(state: &Arc<DaemonState>, op: &GuardOp) -> io::Result<SchedulingOutcome> {
    let suspend = matches!(op, GuardOp::Suspend { .. });
    let workload_id = op.workload_id();
    let entry = state.workload_entry(workload_id).ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "workload is no longer registered")
    })?;
    let group = {
        let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        guard.group.clone()
    };
    if let Some(group) = group {
        let outcome = if suspend {
            state.platform.suspend_owned(&group)
        } else {
            state.platform.resume_owned(&group)
        };
        if let Ok(result) = &outcome {
            if suspend && result.applied > 0 {
                let members = state.platform.member_identities(&group).unwrap_or_default();
                let frozen = (group.kind == term_contracts::workload::GroupKind::Cgroup)
                    .then(|| group.reference.clone());
                state
                    .governance_ledger
                    .note_suspended(workload_id, members, frozen);
            } else if !suspend {
                state.governance_ledger.note_resumed(workload_id);
            }
        }
        return outcome;
    }
    // 그룹 없는 직접 셸: 관측 트리를 다시 훑어 검증된 신원에만 건다.
    let Some(root) = state.shell_identity(workload_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "workload has neither a resource group nor a verified shell root",
        ));
    };
    let walk = || crate::telemetry_loop::scan_shell_tree_verified(state, workload_id, &root);
    if !suspend {
        // 순서(자식 먼저 깨우기)는 플랫폼이 트리 깊이로 정한다.
        let outcome = scheduling::set_process_suspend(&walk(), false);
        if outcome.is_ok() {
            state.governance_ledger.note_resumed(workload_id);
        }
        return outcome;
    }
    // 훑기와 정지 사이에 fork된 자식(빌드의 rustc 등)은 재훑기가 멈춘다.
    let (outcome, identities) = scheduling::suspend_walked(|| Ok(walk()))?;
    if outcome.applied > 0 {
        state
            .governance_ledger
            .note_suspended(workload_id, identities, None);
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> GuardPolicy {
        // 이 모듈의 기존 시험들은 20 s 창의 타이밍을 가정한다 — 메모리 창의
        // 새 기본(5 s)과 무관하게 유지한다(새 기본은 아래 독립 시험이 다룬다).
        GuardPolicy {
            rss_sustain_ms: U64String::new(20_000).expect("20 s in range"),
            ..GuardPolicy::default()
        }
    }

    fn usage(cpu: Option<f64>, rss: Option<u64>) -> LiveUsage {
        LiveUsage {
            workload_id: WorkloadId::generate(),
            session_id: SessionId::generate(),
            state: WorkloadState::Running,
            cpu_cores: cpu,
            resident_bytes: rss,
            protected: false,
            priority: 1,
            busy: false,
        }
    }

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    fn ok_outcome() -> io::Result<SchedulingOutcome> {
        Ok(SchedulingOutcome {
            applied: 1,
            failed: 0,
            skipped_reused: 0,
            skipped_denied: 0,
        })
    }

    /// 압력 사유 정지를 그 시각에 성공한 것으로 기록한다(회복 시험의 준비).
    fn record_pressure_suspend(guard: &mut GuardController, w: &LiveUsage, at: u64) {
        let op = GuardOp::Suspend {
            workload_id: w.workload_id.clone(),
            reason: GuardReason::HostMemoryPressure,
        };
        assert!(guard.record(&op, &ok_outcome(), at));
    }

    fn protected_usage(cpu: Option<f64>, rss: Option<u64>) -> LiveUsage {
        let mut w = usage(cpu, rss);
        w.protected = true;
        w
    }

    fn suspended(state: &GuardState) -> bool {
        state.is_suspended()
    }

    #[test]
    fn sustained_over_limit_suspends_only_after_sustain_and_never_focused() {
        let mut guard = GuardController::new(policy());
        let hog = usage(Some(0.1), Some(5 * 1024 * 1024 * 1024));
        let focused_hog = usage(Some(0.1), Some(5 * 1024 * 1024 * 1024));
        let live = vec![hog.clone(), focused_hog.clone()];
        let focused = vec![focused_hog.session_id.clone()];

        // 초과 직후에는 정지하지 않는다.
        assert!(guard
            .plan(
                1_000,
                PressureLevel::Normal,
                PressureLevel::Warning,
                &focused,
                &live,
                None
            )
            .is_empty());
        // 지속 시간이 지나면 비포커스만.
        let ops = guard.plan(
            21_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &focused,
            &live,
            None,
        );
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].workload_id(), &hog.workload_id);
    }

    /// CPU만 초과하면(호스트 메모리에 여유가 있으면) 절대 정지하지 않는다 —
    /// 경합은 완화가 양보로 다룬다.
    #[test]
    fn cpu_over_limit_alone_never_suspends() {
        let mut guard = GuardController::new(policy());
        let hog = usage(Some(8.0), Some(100 * 1024 * 1024));
        let live = vec![hog.clone()];
        for mem_level in [PressureLevel::Normal, PressureLevel::Warning] {
            guard.plan(1_000, PressureLevel::Normal, mem_level, &[], &live, None);
            let ops = guard.plan(120_000, PressureLevel::Normal, mem_level, &[], &live, None);
            assert!(ops.is_empty(), "CPU 초과만으로는 얼리지 않는다: {ops:?}");
        }
    }

    /// 같은 RSS 초과도 호스트 메모리가 NORMAL이면 얼리지 않고, WARNING
    /// 이상이어야 지속 시간 뒤에 정지한다.
    #[test]
    fn rss_over_limit_suspends_only_when_host_not_normal() {
        let w = usage(Some(0.1), Some(5 * 1024 * 1024 * 1024));
        let live = vec![w.clone()];

        let mut normal = GuardController::new(policy());
        normal.plan(
            1_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            &live,
            None,
        );
        let ops = normal.plan(
            120_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            &live,
            None,
        );
        assert!(ops.is_empty(), "호스트가 NORMAL이면 얼리지 않는다: {ops:?}");

        let mut warning = GuardController::new(policy());
        warning.plan(
            1_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        let ops = warning.plan(
            21_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        assert!(matches!(
            ops.as_slice(),
            [GuardOp::Suspend {
                reason: GuardReason::MemoryLimit,
                ..
            }]
        ));
    }

    /// 늘고 있는 것이 없으면 에피소드가 자리 잡은 뒤에야 가장 큰 정적 업
    /// 하나만 얼리고, 두 번째 정적 희생은 없다(불변 3).
    #[test]
    fn host_memory_critical_freezes_one_static_hog_only_after_the_episode_settles() {
        let mut guard = GuardController::new(policy());
        let small = usage(Some(0.5), Some(512 * MIB));
        let big = usage(Some(0.1), Some(6 * GIB));
        let live = vec![small.clone(), big.clone()];
        let critical = |guard: &mut GuardController, at: u64| {
            guard.plan(
                at,
                PressureLevel::Normal,
                PressureLevel::Critical,
                &[],
                &live,
                None,
            )
        };
        assert!(
            critical(&mut guard, 1_000).is_empty(),
            "증가 감지에 먼저 기회를 준다"
        );
        assert!(critical(&mut guard, 10_999).is_empty());
        let ops = critical(&mut guard, 11_000);
        assert!(
            matches!(ops.as_slice(), [GuardOp::Suspend { workload_id, reason: GuardReason::HostMemoryPressure }] if workload_id == &big.workload_id),
            "{ops:?}"
        );
        apply_ok(&mut guard, &ops, 11_000);
        // 같은 에피소드에서 두 번째 정적 희생은 없다(small도 하한 이상이지만).
        for at in [21_000, 31_000, 60_000] {
            assert!(critical(&mut guard, at).is_empty(), "t={at}");
        }
    }

    #[test]
    fn host_memory_critical_ignores_workloads_below_the_rss_floor() {
        let mut guard = GuardController::new(policy());
        let idle_shell = usage(Some(0.0), Some(40 * MIB));
        let below = usage(Some(0.0), Some(PRESSURE_SUSPEND_MIN_RSS - 1));
        let live = vec![idle_shell, below];
        for at in [1_000, 11_000, 21_000] {
            let ops = guard.plan(
                at,
                PressureLevel::Normal,
                PressureLevel::Critical,
                &[],
                &live,
                None,
            );
            assert!(
                ops.is_empty(),
                "small workloads must not be frozen: {ops:?}"
            );
        }
    }

    /// 늘고 있는 워크로드는 기다리지 않고 먼저 얼린다 — 더 큰 정적 업보다도.
    #[test]
    fn host_memory_critical_freezes_the_growing_workload_first() {
        let mut guard = GuardController::new(policy());
        let big = usage(Some(0.1), Some(6 * GIB));
        let mut grower = usage(Some(2.0), Some(GIB));
        let at = |guard: &mut GuardController, now: u64, live: &[LiveUsage]| {
            guard.plan(
                now,
                PressureLevel::Normal,
                PressureLevel::Critical,
                &[],
                live,
                None,
            )
        };
        assert!(at(&mut guard, 0, &[big.clone(), grower.clone()]).is_empty());
        grower.resident_bytes = Some(GIB + 200 * MIB);
        let ops = at(&mut guard, 1_000, &[big.clone(), grower.clone()]);
        assert!(
            matches!(ops.as_slice(), [GuardOp::Suspend { workload_id, .. }] if workload_id == &grower.workload_id),
            "{ops:?}"
        );
    }

    /// 주범이 보이는(또는 보호된) 세션이면 다른 것을 대신 얼리지 않고 그
    /// 세션에 경고만 올린다(불변 1).
    #[test]
    fn visible_culprit_gets_a_warning_and_nobody_else_is_frozen() {
        let mut guard = GuardController::new(policy());
        // 개별 한도(4 GiB) 아래의 정적 업 — 한도 규칙이 아니라 압력 희생
        // 선택만 본다.
        let big = usage(Some(0.1), Some(3 * GIB));
        let mut grower = usage(Some(2.0), Some(GIB));
        let visible = vec![grower.session_id.clone()];
        let plan = |guard: &mut GuardController, now: u64, grower: &LiveUsage| {
            guard.plan(
                now,
                PressureLevel::Normal,
                PressureLevel::Critical,
                &visible,
                &[big.clone(), grower.clone()],
                None,
            )
        };
        assert!(plan(&mut guard, 0, &grower).is_empty());
        for step in 1..=20u64 {
            grower.resident_bytes = Some(GIB + step * 100 * MIB);
            let ops = plan(&mut guard, step * 1_000, &grower);
            assert!(
                ops.is_empty(),
                "다른 워크로드를 대신 얼리지 않는다: t={step}s {ops:?}"
            );
        }
        assert_eq!(
            guard.view(&grower.workload_id).1,
            Some(GuardReason::HostMemoryPressure)
        );
        assert_eq!(guard.view(&big.workload_id).1, None);
        assert_eq!(
            guard.take_warning_changes(),
            vec![grower.workload_id.clone()]
        );
        assert!(
            guard.take_warning_changes().is_empty(),
            "변화는 한 번만 알린다"
        );
        // 압력이 풀리면 경고도 내려간다.
        guard.plan(
            30_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &visible,
            &[big.clone(), grower.clone()],
            None,
        );
        assert_eq!(guard.view(&grower.workload_id).1, None);
        assert_eq!(
            guard.take_warning_changes(),
            vec![grower.workload_id.clone()]
        );
    }

    /// 한 에피소드의 압력 정지는 간격을 두고 상한까지만. CRITICAL을 벗어나면
    /// 에피소드가 끝난다.
    #[test]
    fn pressure_episode_is_spaced_and_capped() {
        let mut guard = GuardController::new(policy());
        let mut growers: Vec<LiveUsage> = (0..4).map(|_| usage(Some(2.0), Some(GIB))).collect();
        let mut suspended_at = Vec::new();
        for step in 0..=40u64 {
            for w in &mut growers {
                if !guard.view(&w.workload_id).0.is_suspended() {
                    w.resident_bytes = Some(GIB + step * 100 * MIB);
                }
            }
            let ops = guard.plan(
                step * 1_000,
                PressureLevel::Normal,
                PressureLevel::Critical,
                &[],
                &growers,
                None,
            );
            if !ops.is_empty() {
                suspended_at.push(step * 1_000);
                apply_ok(&mut guard, &ops, step * 1_000);
            }
        }
        assert_eq!(
            suspended_at,
            vec![1_000, 11_000, 21_000],
            "10초 간격, 에피소드당 3개"
        );
        // CRITICAL을 벗어났다 다시 들어오면 새 에피소드다.
        guard.plan(
            41_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &growers,
            None,
        );
        let last = growers
            .iter_mut()
            .find(|w| !guard.view(&w.workload_id).0.is_suspended())
            .expect("one left running");
        last.resident_bytes = Some(10 * GIB);
        let ops = guard.plan(
            42_000,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &[],
            &growers,
            None,
        );
        assert!(
            matches!(ops.as_slice(), [GuardOp::Suspend { .. }]),
            "{ops:?}"
        );
    }

    /// Applies `ops` as if the OS calls all succeeded.
    fn apply_ok(guard: &mut GuardController, ops: &[GuardOp], now_ms: u64) {
        for op in ops {
            guard.record(
                op,
                &Ok(SchedulingOutcome {
                    applied: 1,
                    failed: 0,
                    skipped_reused: 0,
                    skipped_denied: 0,
                }),
                now_ms,
            );
        }
    }

    #[test]
    fn focus_resumes_an_auto_suspended_session_and_restarts_the_sustain_window() {
        let mut guard = GuardController::new(policy());
        let hog = usage(Some(0.1), Some(5 * 1024 * 1024 * 1024));
        let live = vec![hog.clone()];
        guard.plan(
            1_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        let ops = guard.plan(
            21_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        assert!(matches!(ops.as_slice(), [GuardOp::Suspend { .. }]));
        apply_ok(&mut guard, &ops, 21_000);
        assert!(suspended(&guard.view(&hog.workload_id).0));

        // The user comes back to the pane: next tick resumes it.
        let focused = vec![hog.session_id.clone()];
        let ops = guard.plan(
            22_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &focused,
            &live,
            None,
        );
        assert!(
            matches!(ops.as_slice(), [GuardOp::Resume { workload_id }] if workload_id == &hog.workload_id),
            "focus must resume an automatic suspension: {ops:?}"
        );
        apply_ok(&mut guard, &ops, 22_000);
        assert!(!suspended(&guard.view(&hog.workload_id).0));

        // Still over the limit while focused for a long time: never frozen.
        let ops = guard.plan(
            60_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &focused,
            &live,
            None,
        );
        assert!(ops.is_empty());
        // Focus leaves: the sustain window starts over instead of firing on
        // the very next tick.
        let ops = guard.plan(
            61_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        assert!(
            ops.is_empty(),
            "no instant re-suspend after focus leaves: {ops:?}"
        );
        let ops = guard.plan(
            81_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        assert!(matches!(ops.as_slice(), [GuardOp::Suspend { .. }]));
    }

    #[test]
    fn failed_focus_resume_backs_off_instead_of_retrying_every_tick() {
        let mut guard = GuardController::new(policy());
        let hog = usage(Some(0.1), Some(5 * 1024 * 1024 * 1024));
        let live = vec![hog.clone()];
        guard.plan(
            1_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        let ops = guard.plan(
            21_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        assert!(matches!(ops.as_slice(), [GuardOp::Suspend { .. }]));
        apply_ok(&mut guard, &ops, 21_000);

        // 포커스 재개가 EPERM으로 실패하면 상태는 Suspended에 남는다.
        let focused = vec![hog.session_id.clone()];
        let ops = guard.plan(
            22_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &focused,
            &live,
            None,
        );
        assert!(matches!(ops.as_slice(), [GuardOp::Resume { .. }]));
        let eperm = Err(io::Error::new(io::ErrorKind::PermissionDenied, "EPERM"));
        assert!(!guard.record(&ops[0], &eperm, 22_000));
        assert!(suspended(&guard.view(&hog.workload_id).0));

        // 백오프 안의 직후 틱들은 같은 OS 호출을 되풀이하지 않는다.
        let ops = guard.plan(
            23_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &focused,
            &live,
            None,
        );
        assert!(ops.is_empty(), "failed resume must back off: {ops:?}");

        // 간격이 지나면 다시 시도한다.
        let ops = guard.plan(
            33_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &focused,
            &live,
            None,
        );
        assert!(
            matches!(ops.as_slice(), [GuardOp::Resume { .. }]),
            "backoff elapsed must retry once: {ops:?}"
        );

        // Unsupported로 판명난 뒤로는 아예 시도하지 않는다(suspend 경로와 같은 가드).
        let unsupported = Err(io::Error::new(io::ErrorKind::Unsupported, "no freezer"));
        assert!(!guard.record(&ops[0], &unsupported, 33_000));
        let ops = guard.plan(
            60_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &focused,
            &live,
            None,
        );
        assert!(
            ops.is_empty(),
            "unsupported resume must never retry: {ops:?}"
        );
    }

    #[test]
    fn focus_does_not_thaw_a_host_memory_freeze_while_memory_is_still_critical() {
        let mut guard = GuardController::new(policy());
        let hog = usage(Some(0.1), Some(6 * 1024 * 1024 * 1024));
        let live = vec![hog.clone()];
        // 호스트 메모리 CRITICAL 경로로 얼린다(rss 하한 이상의 비포커스 정적 업 —
        // 에피소드가 자리 잡은 뒤).
        guard.plan(
            0,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &[],
            &live,
            None,
        );
        let ops = guard.plan(
            STATIC_VICTIM_DELAY_MS,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &[],
            &live,
            None,
        );
        assert!(matches!(ops.as_slice(), [GuardOp::Suspend { .. }]));
        apply_ok(&mut guard, &ops, 1_000);

        // 사용자가 그 pane을 봐도 메모리가 여전히 CRITICAL이면 해동하지 않는다.
        let focused = vec![hog.session_id.clone()];
        let ops = guard.plan(
            2_000,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &focused,
            &live,
            None,
        );
        assert!(
            ops.is_empty(),
            "pressure freeze must hold while CRITICAL: {ops:?}"
        );

        // 메모리가 회복되면 포커스 재개가 다음 틱에 돌아온다(불변 5).
        let ops = guard.plan(
            3_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &focused,
            &live,
            None,
        );
        assert!(
            matches!(ops.as_slice(), [GuardOp::Resume { workload_id }] if workload_id == &hog.workload_id),
            "recovered memory must allow focus resume: {ops:?}"
        );
        apply_ok(&mut guard, &ops, 3_000);
        assert!(!suspended(&guard.view(&hog.workload_id).0));
    }

    #[test]
    fn focus_does_not_resume_a_manual_suspension() {
        let mut guard = GuardController::new(policy());
        let w = usage(Some(0.1), None);
        let ops = guard.manual(&w.workload_id, true);
        apply_ok(&mut guard, &ops, 1_000);
        let ops = guard.plan(
            2_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            std::slice::from_ref(&w.session_id),
            std::slice::from_ref(&w),
            None,
        );
        assert!(
            ops.is_empty(),
            "manual suspend stays until resumed manually: {ops:?}"
        );
    }

    #[test]
    fn manual_suspend_resumes_only_manually_and_shutdown_resumes_all() {
        let mut guard = GuardController::new(policy());
        let w = usage(Some(0.1), None);
        let ops = guard.manual(&w.workload_id, true);
        assert_eq!(ops.len(), 1);
        apply_ok(&mut guard, &ops, 1_000);
        // 자동 재개 정책을 켜도 수동 정지는 재개되지 않는다.
        let mut p = policy();
        p.auto_resume = true;
        guard.set_policy(p);
        let live = vec![w.clone()];
        let ops = guard.plan(
            10 * 60_000 + 2_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            &live,
            None,
        );
        assert!(ops.is_empty(), "manual suspend must not auto-resume");
        let shutdown = guard.shutdown_resume_ops();
        assert_eq!(shutdown.len(), 1);
        assert!(matches!(shutdown[0], GuardOp::Resume { .. }));
    }

    #[test]
    fn record_tracks_state_and_partial() {
        let mut guard = GuardController::new(policy());
        let w = usage(Some(0.1), Some(5 * 1024 * 1024 * 1024));
        let op = GuardOp::Suspend {
            workload_id: w.workload_id.clone(),
            reason: GuardReason::MemoryLimit,
        };
        let partial_outcome = Ok(SchedulingOutcome {
            applied: 3,
            failed: 1,
            skipped_reused: 0,
            skipped_denied: 0,
        });
        assert!(guard.record(&op, &partial_outcome, 5_000));
        let (state, _) = guard.view(&w.workload_id);
        assert!(suspended(&state));
        if let GuardState::Suspended { partial, .. } = state {
            assert!(partial, "a failed member marks the pass partial");
        }
        let ok = Ok(SchedulingOutcome {
            applied: 4,
            failed: 0,
            skipped_reused: 0,
            skipped_denied: 0,
        });
        let resume = GuardOp::Resume {
            workload_id: w.workload_id.clone(),
        };
        assert!(guard.record(&resume, &ok, 6_000));
        let (state, _) = guard.view(&w.workload_id);
        assert!(!suspended(&state));
    }

    /// 메모리 회복(NORMAL)은 자동 정지된 것을 3초에 하나씩, 가장 나중에
    /// 얼린 것부터 되살린다.
    #[test]
    fn memory_recovery_resumes_one_every_3s_newest_first() {
        let mut guard = GuardController::new(policy());
        let a = usage(Some(0.1), Some(1024 * 1024 * 1024));
        let b = usage(Some(0.1), Some(2 * 1024 * 1024 * 1024));
        let c = usage(Some(0.1), Some(3 * 1024 * 1024 * 1024));
        // 압력 사유로 t=0, 3_000, 6_000에 하나씩 얼린 상태를 만든다.
        record_pressure_suspend(&mut guard, &a, 0);
        record_pressure_suspend(&mut guard, &b, 3_000);
        record_pressure_suspend(&mut guard, &c, 6_000);
        let live = vec![a.clone(), b.clone(), c.clone()];

        // NORMAL: 가장 나중에 얼린 c부터 하나만.
        let ops = guard.plan(
            10_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            &live,
            None,
        );
        assert!(matches!(
            ops.as_slice(),
            [GuardOp::Resume { workload_id }] if workload_id == &c.workload_id
        ));
        apply_ok(&mut guard, &ops, 10_000);

        // 3초 안에는 두 번째가 없다.
        let ops = guard.plan(
            12_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            &live,
            None,
        );
        assert!(ops.is_empty(), "속도 제한 안: {ops:?}");

        // 간격이 지나면 다음(더 먼저 얼린 b).
        let ops = guard.plan(
            13_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            &live,
            None,
        );
        assert!(matches!(
            ops.as_slice(),
            [GuardOp::Resume { workload_id }] if workload_id == &b.workload_id
        ));
    }

    /// 수동 정지는 메모리가 회복돼도 자동으로 되살리지 않는다(불변 2).
    #[test]
    fn memory_recovery_skips_manual_suspensions() {
        let mut guard = GuardController::new(policy());
        let w = usage(Some(0.1), Some(1024 * 1024 * 1024));
        let ops = guard.manual(&w.workload_id, true);
        apply_ok(&mut guard, &ops, 1_000);
        let ops = guard.plan(
            120_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        assert!(ops.is_empty(), "수동 정지는 손대지 않는다: {ops:?}");
    }

    /// WARNING에서는 회복 재개가 돌지 않는다 — NORMAL만이 회복이다.
    #[test]
    fn memory_recovery_does_nothing_at_warning() {
        let mut guard = GuardController::new(policy());
        let w = usage(Some(0.1), Some(5 * 1024 * 1024 * 1024));
        guard.plan(
            1_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        let ops = guard.plan(
            21_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        assert!(matches!(ops.as_slice(), [GuardOp::Suspend { .. }]));
        apply_ok(&mut guard, &ops, 21_000);
        let ops = guard.plan(
            60_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        assert!(ops.is_empty(), "WARNING에서는 재개하지 않는다: {ops:?}");
    }

    /// 재개가 실패한 워크로드는 백오프(10초) 안에 다시 재개하지 않는다.
    #[test]
    fn memory_recovery_respects_resume_backoff() {
        let mut guard = GuardController::new(policy());
        let w = usage(Some(0.1), Some(1024 * 1024 * 1024));
        record_pressure_suspend(&mut guard, &w, 1_000);

        // NORMAL에서 재개를 시도했지만 EPERM으로 실패한다.
        let ops = guard.plan(
            2_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        assert!(matches!(ops.as_slice(), [GuardOp::Resume { .. }]));
        let eperm = Err(io::Error::new(io::ErrorKind::PermissionDenied, "EPERM"));
        assert!(!guard.record(&ops[0], &eperm, 2_000));
        assert!(suspended(&guard.view(&w.workload_id).0));

        // 백오프 안의 틱들은 같은 OS 호출을 되풀이하지 않는다.
        let ops = guard.plan(
            11_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        assert!(ops.is_empty(), "백오프 안: {ops:?}");

        // 간격이 지나면 다시 시도한다.
        let ops = guard.plan(
            12_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        assert!(
            matches!(ops.as_slice(), [GuardOp::Resume { .. }]),
            "백오프가 지나면 재시도: {ops:?}"
        );
    }

    /// 보호 표시된 워크로드는 개별 메모리 한도를 넘어도 자동 정지하지 않는다.
    #[test]
    fn protected_workload_is_never_auto_suspended() {
        let mut guard = GuardController::new(policy());
        let w = protected_usage(Some(0.1), Some(5 * 1024 * 1024 * 1024));
        let live = vec![w.clone()];
        guard.plan(
            1_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        let ops = guard.plan(
            60_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        assert!(ops.is_empty(), "보호된 워크로드는 얼리지 않는다: {ops:?}");
    }

    /// CRITICAL 희생 선택에서도 보호된 것은 빠진다 — RSS 최대여도 두 번째가
    /// 희생된다.
    #[test]
    fn protected_workload_is_skipped_as_pressure_victim() {
        let mut guard = GuardController::new(policy());
        let protected = protected_usage(Some(0.1), Some(6 * 1024 * 1024 * 1024));
        let other = usage(Some(0.1), Some(2 * 1024 * 1024 * 1024));
        guard.plan(
            1_000,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &[],
            &[protected.clone(), other.clone()],
            None,
        );
        let ops = guard.plan(
            1_000 + STATIC_VICTIM_DELAY_MS,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &[],
            &[protected.clone(), other.clone()],
            None,
        );
        assert!(matches!(
            ops.as_slice(),
            [GuardOp::Suspend { workload_id, .. }] if workload_id == &other.workload_id
        ));
    }

    /// 메모리 지속 창은 CPU 창과 별개다 — 기본 정책(rss 5 s / cpu 20 s)에서
    /// RSS 초과는 CPU 창이 지나기 전에 이미 정지한다.
    #[test]
    fn rss_sustain_is_independent_from_cpu_sustain() {
        let mut guard = GuardController::new(GuardPolicy::default());
        let w = usage(Some(7.5), Some(5 * 1024 * 1024 * 1024));
        let live = vec![w.clone()];
        guard.plan(
            0,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        let ops = guard.plan(
            5_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        assert!(matches!(
            ops.as_slice(),
            [GuardOp::Suspend {
                reason: GuardReason::MemoryLimit,
                ..
            }]
        ));
    }

    /// 메모리 회복 재개는 자동 정지 정책과 무관하게 돈다(불변 7).
    #[test]
    fn memory_recovery_runs_even_with_auto_suspend_off() {
        let mut p = policy();
        p.auto_suspend = false;
        let mut guard = GuardController::new(p);
        let w = usage(Some(0.1), Some(GIB));
        record_pressure_suspend(&mut guard, &w, 0);
        let ops = guard.plan(
            5_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        assert!(
            matches!(ops.as_slice(), [GuardOp::Resume { .. }]),
            "{ops:?}"
        );
    }

    /// 정지 시점 RSS만큼 호스트 여유가 없으면 회복 재개를 미룬다.
    #[test]
    fn memory_recovery_waits_for_headroom() {
        let mut guard = GuardController::new(policy());
        let w = usage(Some(0.1), Some(3 * GIB));
        // 관측(=last_rss 3 GiB) 뒤에 정지가 기록된다.
        guard.plan(
            0,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        record_pressure_suspend(&mut guard, &w, 0);
        guard.set_host_headroom(Some(GIB as i64));
        let ops = guard.plan(
            5_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        assert!(
            ops.is_empty(),
            "여유 1 GiB로는 3 GiB를 풀지 않는다: {ops:?}"
        );
        guard.set_host_headroom(Some(4 * GIB as i64));
        let ops = guard.plan(
            6_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        assert!(
            matches!(ops.as_slice(), [GuardOp::Resume { .. }]),
            "{ops:?}"
        );
    }

    /// 사용자가 직접 재개한 세션은 메모리가 NORMAL로 돌아올 때까지 다시
    /// 자동 정지하지 않는다(불변 2).
    #[test]
    fn manual_resume_exempts_until_memory_is_normal() {
        let mut guard = GuardController::new(policy());
        let hog = usage(Some(0.1), Some(5 * GIB));
        let live = vec![hog.clone()];
        let warn = |guard: &mut GuardController, at: u64| {
            guard.plan(
                at,
                PressureLevel::Normal,
                PressureLevel::Warning,
                &[],
                &live,
                None,
            )
        };
        warn(&mut guard, 0);
        let ops = warn(&mut guard, 20_000);
        assert!(matches!(ops.as_slice(), [GuardOp::Suspend { .. }]));
        apply_ok(&mut guard, &ops, 20_000);
        // 사용자가 재개한다.
        let ops = guard.manual(&hog.workload_id, false);
        apply_ok(&mut guard, &ops, 21_000);
        assert!(!guard.view(&hog.workload_id).0.is_suspended());
        // 압력이 남아 있어도 다시 얼리지 않는다.
        for at in [22_000, 45_000, 90_000] {
            assert!(warn(&mut guard, at).is_empty(), "t={at}");
        }
        // NORMAL을 한 번 지나면 면제가 끝나고, 다시 지속되면 정지한다.
        guard.plan(
            91_000,
            PressureLevel::Normal,
            PressureLevel::Normal,
            &[],
            &live,
            None,
        );
        assert!(warn(&mut guard, 92_000).is_empty());
        let ops = warn(&mut guard, 112_000);
        assert!(
            matches!(ops.as_slice(), [GuardOp::Suspend { .. }]),
            "{ops:?}"
        );
    }

    /// 실패한 수동 정지는 상태를 바꾸지 않는다 — 걸리지 않은 정지를
    /// "정지됨"으로 광고하면 입력·리사이즈가 거부된다.
    #[test]
    fn failed_manual_suspend_leaves_state_unchanged() {
        let mut guard = GuardController::new(policy());
        let w = usage(Some(0.1), None);
        let ops = guard.manual(&w.workload_id, true);
        let eperm = Err(io::Error::new(io::ErrorKind::PermissionDenied, "EPERM"));
        assert!(!guard.record(&ops[0], &eperm, 1_000));
        assert!(!guard.view(&w.workload_id).0.is_suspended());
        // 실패한 수동 재개도 면제를 세우지 않는다.
        let ops = guard.manual(&w.workload_id, true);
        apply_ok(&mut guard, &ops, 2_000);
        let ops = guard.manual(&w.workload_id, false);
        assert!(!guard.record(&ops[0], &eperm, 3_000));
        assert!(guard.view(&w.workload_id).0.is_suspended());
    }

    /// 메모리 사유 정지의 자동 재개(`auto_resume`)는 메모리도 NORMAL이어야 한다.
    #[test]
    fn auto_resume_of_a_memory_suspension_needs_memory_normal() {
        let mut p = policy();
        p.auto_resume = true;
        p.auto_suspend = false;
        let mut guard = GuardController::new(p);
        let w = usage(Some(0.0), Some(GIB));
        let op = GuardOp::Suspend {
            workload_id: w.workload_id.clone(),
            reason: GuardReason::MemoryLimit,
        };
        assert!(guard.record(&op, &ok_outcome(), 0));
        let ops = guard.plan(
            11 * 60_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            std::slice::from_ref(&w),
            Some(10.0),
        );
        assert!(ops.is_empty(), "메모리 WARNING에서는 풀지 않는다: {ops:?}");
    }

    /// 종료 경로가 시작되면 새 정지를 내지 않는다(텔레메트리 틱과의 경합).
    #[test]
    fn shutdown_stops_new_suspensions() {
        let mut guard = GuardController::new(policy());
        let hog = usage(Some(0.1), Some(5 * GIB));
        let live = vec![hog.clone()];
        guard.plan(
            0,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        assert!(guard.shutdown_resume_ops().is_empty());
        let ops = guard.plan(
            60_000,
            PressureLevel::Normal,
            PressureLevel::Warning,
            &[],
            &live,
            None,
        );
        assert!(ops.is_empty(), "{ops:?}");
    }
}
