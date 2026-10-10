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
//! 1. 포커스된 세션은 절대 자동으로 정지하지 않는다 — 보고 있는 터미널을
//!    얼리는 대신 경고(`GuardReason`)만 올린다(원클릭 정지는 UI가 제공).
//! 2. 수동 정지는 자동 재개 대상이 아니다.
//! 3. 호스트 메모리 CRITICAL에서는 rss가 가장 큰 비포커스 독점 업부터
//!    양보의 릴리스 간격과 같은 속도로 하나씩 정지한다.
//! 4. 모든 정지는 되돌릴 수 있고 데몬 종료 경로에서 재개된다.
//! 5. 자동 정지된 세션에 포커스가 오면 다음 틱에 재개한다 — 사용자가
//!    돌아와 보는 터미널이 말없이 얼어 있지 않게(입력이 버려지고 메모리는
//!    그대로 잡힌 "좀비" pane). 포커스 중에는 초과 지속 시간도 세지 않으므로
//!    포커스가 떠나도 `sustain_ms`를 다시 채워야 정지된다. 수동 정지는
//!    예외(불변 2). 호스트 메모리 CRITICAL 정지도 예외다 — 사유가 살아 있는
//!    동안에는 포커스로 풀지 않는다(SIGSTOP은 메모리를 돌려주지 않으므로).
//! 6. 호스트 메모리 CRITICAL 정지는 [`PRESSURE_SUSPEND_MIN_RSS`] 이상인
//!    업만 고른다 — SIGSTOP은 메모리를 돌려주지 않아 압력이 그대로 남고,
//!    하한이 없으면 놀고 있는 셸까지 3초에 하나씩 모두 얼렸다.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;

use term_contracts::ids::{SessionId, U64String, WorkloadId};
use term_contracts::metrics::PressureLevel;
use term_contracts::snapshot::{GuardPolicy, GuardReason, GuardState};
use term_contracts::state::WorkloadState;
use term_platform::group::SchedulingOutcome;

use crate::state::DaemonState;

/// 메모리 압력 정지의 하한: 이보다 작은 업은 얼려 봐야 압력 해소에
/// 기여하지 못한다(불변 6).
pub const PRESSURE_SUSPEND_MIN_RSS: u64 = 512 * 1024 * 1024;

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
}

/// 가드의 결정 코어. `DaemonState`가 `Mutex`로 하나만 소유한다.
pub struct GuardController {
    records: HashMap<WorkloadId, Record>,
    policy: GuardPolicy,
    /// 마지막 호스트 압력 정지 시각(속도 제한: 양보의 릴리스 간격과 같은
    /// 3초에 하나).
    last_pressure_suspend_ms: Option<u64>,
    /// 마지막 메모리 회복 재개 시각(3초에 하나 — 한꺼번에 풀면 RSS가 한 번에
    /// 돌아와 다시 CRITICAL로 떨어진다).
    last_pressure_resume_ms: Option<u64>,
}

impl GuardController {
    pub fn new(policy: GuardPolicy) -> Self {
        Self {
            records: HashMap::new(),
            policy,
            last_pressure_suspend_ms: None,
            last_pressure_resume_ms: None,
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

    /// 스냅샷 보고용 `(guard, warning)`.
    pub fn view(&self, workload_id: &WorkloadId) -> (GuardState, Option<GuardReason>) {
        match self.records.get(workload_id) {
            Some(record) => (record.state.clone(), None),
            None => (GuardState::None, None),
        }
    }

    /// 죽은 워크로드의 기록을 버린다.
    fn retain_live(&mut self, live: &[LiveUsage]) {
        let ids: HashSet<&str> = live.iter().map(|w| w.workload_id.as_str()).collect();
        self.records.retain(|id, _| ids.contains(id.as_str()));
    }

    /// 한 틱의 계획. 부수효과는 기록 정리뿐이고 OS를 건드리지 않는다.
    pub fn plan(
        &mut self,
        now_ms: u64,
        cpu_level: PressureLevel,
        mem_level: PressureLevel,
        focused: &[SessionId],
        live: &[LiveUsage],
        cpu_recovery_used_percent: Option<f64>,
    ) -> Vec<GuardOp> {
        self.retain_live(live);
        let focused: HashSet<&str> = focused.iter().map(SessionId::as_str).collect();
        // 결정은 결정적이어야 한다(시험 재현성): workload_id 순.
        let mut ordered: Vec<&LiveUsage> = live.iter().collect();
        ordered.sort_by(|a, b| a.workload_id.as_str().cmp(b.workload_id.as_str()));

        let mut ops = Vec::new();
        let sustain = self.policy.sustain_ms.get().max(1);
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

            if focused.contains(workload.session_id.as_str()) {
                // 불변 1·5: 보고 있는 세션은 얼리지 않고, 자동 정지돼 있었다면
                // 되살린다. 초과 지속 시간은 포커스 동안 세지 않는다. 실패한
                // 재개(Unsupported·EPERM)를 매 틱 다시 밀지 않는다 — 백오프와
                // unsupported 가드가 없으면 OS 호출+로그가 무한히 반복된다.
                record.over_since_ms = None;
                if matches!(&record.state, GuardState::Suspended { manual: false, .. })
                    && !record.unsupported
                    && now_ms >= record.resume_retry_after_ms.unwrap_or(0)
                    // 예외: 호스트 메모리 CRITICAL 정지는 그 사유가 살아 있는
                    // 동안 포커스로도 풀지 않는다 — SIGSTOP은 메모리를 돌려주지
                    // 않으므로 해동이 곧 재정지 진동이 된다(재정지는 3초에 1개
                    // 제한). 배지의 수동 재개 버튼과 메모리 회복이 푼다.
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

            if over {
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
                // 호스트 CPU가 회복 구간이면 스스로 돌아온다. 여기도 같은
                // 백오프·unsupported 가드를 지난다(조건이 한번 참이면 매 틱
                // 참으로 남으므로 실패 시 무한 재시도가 된다).
                if self.policy.auto_resume {
                    let eligible = match &record.state {
                        GuardState::Suspended {
                            since_ms, manual, ..
                        } => {
                            let quiet_for = now_ms.saturating_sub(since_ms.get());
                            let host_recovered = cpu_level == PressureLevel::Normal
                                || cpu_recovery_used_percent.is_some_and(|used| used < 60.0);
                            !*manual && quiet_for >= 10 * 60_000 && host_recovered
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

        // 호스트 메모리 CRITICAL: rss가 가장 큰 비포커스·미정지 업부터
        // 하나씩(릴리스 간격과 같은 속도 제한은 호출자가 now로 판정해
        // record.last_pressure_suspend_ms에 기록한다).
        if mem_level == PressureLevel::Critical && self.policy.auto_suspend {
            let due = self
                .last_pressure_suspend_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= 3_000);
            if due {
                let target = ordered
                    .iter()
                    .filter(|w| w.state == WorkloadState::Running)
                    .filter(|w| !focused.contains(w.session_id.as_str()))
                    .filter(|w| {
                        self.records
                            .get(&w.workload_id)
                            .is_none_or(|r| !r.state.is_suspended() && !r.unsupported)
                    })
                    .filter_map(|w| w.resident_bytes.map(|r| (r, w.workload_id.clone())))
                    .filter(|(rss, _)| *rss >= PRESSURE_SUSPEND_MIN_RSS)
                    .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.as_str().cmp(b.1.as_str())));
                if let Some((_, id)) = target {
                    self.last_pressure_suspend_ms = Some(now_ms);
                    ops.push(GuardOp::Suspend {
                        workload_id: id,
                        reason: GuardReason::HostMemoryPressure,
                    });
                }
            }
        }

        // 호스트 메모리 NORMAL: 메모리 사유(호스트 압력·개별 한도)로 자동 정지된
        // 워크로드를 3초에 하나씩, 가장 나중에 얼린 것부터 되살린다. 가장 먼저
        // 얼린 것이 RSS가 가장 크므로 역순이 재악화 위험이 작다. 수동 정지는 손대지
        // 않는다. 포커스 재개는 위 루프가 이미 처리했다(planned에 있으면 건너뛴다).
        // 진동하지 않는다: NORMAL에서는 개별 한도 초과가 정지를 만들지 않으므로
        // 되살린 대상이 같은 틱에 다시 얼리지 않는다.
        if mem_level == PressureLevel::Normal && self.policy.auto_suspend {
            let due = self
                .last_pressure_resume_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= 3_000);
            if due {
                let planned: HashSet<&str> =
                    ops.iter().map(|op| op.workload_id().as_str()).collect();
                let target = ordered
                    .iter()
                    .filter(|w| !planned.contains(w.workload_id.as_str()))
                    .filter_map(|w| {
                        let record = self.records.get(&w.workload_id)?;
                        if record.unsupported || now_ms < record.resume_retry_after_ms.unwrap_or(0)
                        {
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

    /// 수동 조작(`workload.suspend`/`workload.resume`). 수동 정지는 자동
    /// 재개 대상이 아니다(불변 2).
    pub fn manual(&mut self, workload_id: &WorkloadId, suspend: bool) -> Vec<GuardOp> {
        let record = self.records.entry(workload_id.clone()).or_default();
        let already = record.state.is_suspended();
        if suspend && !already {
            record.state = GuardState::Suspended {
                since_ms: U64String::new(0).expect("0 in range"),
                reason: GuardReason::Manual,
                manual: true,
                partial: false,
            };
            record.over_since_ms = None;
            vec![GuardOp::Suspend {
                workload_id: workload_id.clone(),
                reason: GuardReason::Manual,
            }]
        } else if !suspend && already {
            record.state = GuardState::None;
            record.over_since_ms = None;
            vec![GuardOp::Resume {
                workload_id: workload_id.clone(),
            }]
        } else {
            Vec::new()
        }
    }

    /// 종료 경로: 정지 중인 모든 워크로드를 재개한다(불변 4).
    pub fn shutdown_resume_ops(&mut self) -> Vec<GuardOp> {
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
                if error.kind() == io::ErrorKind::Unsupported {
                    self.records
                        .entry(workload_id.clone())
                        .or_default()
                        .unsupported = true;
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
                let manual = matches!(&record.state, GuardState::Suspended { manual: true, .. });
                let since_ms = match &record.state {
                    GuardState::Suspended { since_ms, .. } => since_ms.clone(),
                    GuardState::None => U64String::new(now_ms)
                        .unwrap_or_else(|_| U64String::new(0).expect("0 in range")),
                };
                GuardState::Suspended {
                    since_ms,
                    reason: *reason,
                    manual,
                    partial: outcome.is_partial(),
                }
            }
            GuardOp::Resume { .. } => GuardState::None,
        };
        let changed = record.state != next;
        record.state = next;
        changed
    }
}

/// 결정 하나를 OS에 건다(양보의 [`crate::relief::apply`]와 같은 구조).
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
        return if suspend {
            state.platform.suspend_owned(&group)
        } else {
            state.platform.resume_owned(&group)
        };
    }
    // 그룹 없는 직접 셸: 관측 트리를 다시 훑아 검증된 신원에만 건다.
    let Some(root) = state.shell_identity(workload_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "workload has neither a resource group nor a verified shell root",
        ));
    };
    let identities = crate::telemetry_loop::scan_shell_tree_verified(&root);
    term_platform::group::scheduling::set_process_suspend(&identities, suspend)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> GuardPolicy {
        GuardPolicy::default()
    }

    fn usage(cpu: Option<f64>, rss: Option<u64>) -> LiveUsage {
        LiveUsage {
            workload_id: WorkloadId::generate(),
            session_id: SessionId::generate(),
            state: WorkloadState::Running,
            cpu_cores: cpu,
            resident_bytes: rss,
        }
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

    #[test]
    fn host_memory_critical_suspends_the_largest_unfocused_hog_one_at_a_time() {
        let mut guard = GuardController::new(policy());
        let small = usage(Some(0.5), Some(512 * 1024 * 1024));
        let big = usage(Some(0.1), Some(6 * 1024 * 1024 * 1024));
        let live = vec![small.clone(), big.clone()];
        let ops = guard.plan(
            1_000,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &[],
            &live,
            None,
        );
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].workload_id(), &big.workload_id);
        // 속도 제한: 같은 창에는 두 번째 정지가 없다.
        let ops = guard.plan(
            2_000,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &[],
            &live,
            None,
        );
        assert!(ops.is_empty());
    }

    #[test]
    fn host_memory_critical_ignores_workloads_below_the_rss_floor() {
        let mut guard = GuardController::new(policy());
        let idle_shell = usage(Some(0.0), Some(40 * 1024 * 1024));
        let below = usage(Some(0.0), Some(PRESSURE_SUSPEND_MIN_RSS - 1));
        let live = vec![idle_shell, below];
        let ops = guard.plan(
            1_000,
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

    /// Applies `ops` as if the OS calls all succeeded.
    fn apply_ok(guard: &mut GuardController, ops: &[GuardOp], now_ms: u64) {
        for op in ops {
            guard.record(
                op,
                &Ok(SchedulingOutcome {
                    applied: 1,
                    failed: 0,
                    skipped_reused: 0,
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
        // 호스트 메모리 CRITICAL 경로로 얼린다(rss 하한 이상의 비포커스 독점).
        let ops = guard.plan(
            1_000,
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
        // CRITICAL 경로로 t=0, 3_000, 6_000에 하나씩 얼린다.
        let ops = guard.plan(
            0,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &[],
            std::slice::from_ref(&a),
            None,
        );
        apply_ok(&mut guard, &ops, 0);
        let ops = guard.plan(
            3_000,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &[],
            &[a.clone(), b.clone()],
            None,
        );
        apply_ok(&mut guard, &ops, 3_000);
        let ops = guard.plan(
            6_000,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &[],
            &[a.clone(), b.clone(), c.clone()],
            None,
        );
        apply_ok(&mut guard, &ops, 6_000);
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
        let ops = guard.plan(
            1_000,
            PressureLevel::Normal,
            PressureLevel::Critical,
            &[],
            std::slice::from_ref(&w),
            None,
        );
        apply_ok(&mut guard, &ops, 1_000);

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
}
