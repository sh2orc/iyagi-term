//! 압력 완화 P2 — 스케줄링 양보(spec `08-pressure-relief.md` §2).
//!
//! 두 부분으로 나뉜다.
//!
//! * [`ReliefController`]: 순수 결정 코어. 시계도 OS도 모르고, 압력 level·
//!   포커스 집합·살아 있는 워크로드 목록을 받아 [`ReliefOp`] 목록만 낸다.
//!   덕분에 §0의 불변 조건(포커스·보호 면제, 수동 > 자동, 되돌릴 수 있음)을
//!   단위 시험으로 통째로 검증할 수 있다.
//! * [`apply`]: 그 결정을 실제 OS에 거는 어댑터. 관리 워크로드는 그룹의
//!   `set_scheduling`으로, 직접 셸은 그룹이 없으므로(02-runner §3) §1.2의
//!   관측 트리에 per-pid 진입점을 쓴다.
//!
//! 불변 조건(§0):
//!
//! 1. 자동은 거부하지 않고 양보만 한다 — 프로세스를 정지·종료하지 않는다.
//! 2. 포커스된 세션과 보호 표시 세션은 자동 대상이 아니다.
//! 3. 수동이 자동보다 우선한다: 수동 양보는 자동 복원되지 않고, 압력 중
//!    수동 복원한 세션은 NORMAL로 돌아갈 때까지 다시 양보되지 않는다.
//! 4. 모든 양보는 되돌릴 수 있고 데몬 종료 경로에서 해제된다.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;

use term_contracts::ids::{SessionId, U64String, WorkloadId};
use term_contracts::metrics::PressureLevel;
use term_contracts::session::ReliefAction;
use term_contracts::snapshot::{ReliefPolicy, ReliefState};
use term_contracts::state::WorkloadState;
use term_platform::group::{scheduling, SchedulingOutcome, SchedulingTier};

use crate::state::DaemonState;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveWorkload {
    pub workload_id: WorkloadId,
    pub session_id: SessionId,
    pub state: WorkloadState,
}

/// 워크로드 하나의 완화 기록.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Record {
    relief: ReliefState,
    /// 사용자가 직접 양보시켰다 — 자동 복원 대상이 아니다(§0-2).
    manual_yield: bool,
    /// 사용자가 보호 표시했다(해제할 때까지 유지).
    protected_sticky: bool,
    /// 압력이 남아 있는 동안 수동 복원한 세션을 다시 양보시키지 않는
    /// 일시 보호 — NORMAL 회복에서 자동으로 풀린다.
    protected_until_normal: bool,
    /// 이 워크로드의 적용 경로가 `Unsupported`를 돌려줬다 — 다시 시도하지
    /// 않는다. 데몬 전체가 아니라 워크로드 단위인 이유: 위임 cgroup Linux는
    /// 관리 워크로드의 `cpu.weight`를 지원하지만 그룹이 없는 직접 셸의
    /// per-pid 경로는 지원하지 않는다(§0-4). 하나가 지원되지 않는다고
    /// 나머지의 완화까지 꺼서는 안 된다.
    unsupported: bool,
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
}

/// 양보 정책의 결정 코어. `DaemonState`가 `Mutex`로 하나만 소유한다.
pub struct ReliefController {
    records: HashMap<WorkloadId, Record>,
    policy: ReliefPolicy,
    /// 마지막 순차 복원 시각(단조 ms) — `None`이면 아직 한 번도 안 풀었다.
    last_release_ms: Option<u64>,
    /// `timing_ms.relief_release_interval`.
    release_interval_ms: u64,
}

impl ReliefController {
    pub fn new(policy: ReliefPolicy, release_interval_ms: u64) -> Self {
        Self {
            records: HashMap::new(),
            policy,
            last_release_ms: None,
            release_interval_ms: release_interval_ms.max(1),
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

    /// 한 틱의 계획. 부수효과는 기록 정리(죽은 워크로드 제거, NORMAL에서
    /// 일시 보호 해제, 순차 복원 타이머)뿐이고 OS는 건드리지 않는다.
    pub fn plan(
        &mut self,
        now_ms: u64,
        cpu_level: PressureLevel,
        focused: &[SessionId],
        live: &[LiveWorkload],
        capability_supported: bool,
    ) -> Vec<ReliefOp> {
        // 더 이상 살아 있지 않은 워크로드의 기록은 버린다 — 프로세스가 이미
        // 없으므로 복원 호출도 하지 않는다.
        let live_ids: HashSet<&str> = live.iter().map(|w| w.workload_id.as_str()).collect();
        self.records.retain(|id, _| live_ids.contains(id.as_str()));

        let normal = cpu_level == PressureLevel::Normal;
        if normal {
            // 압력이 풀렸다: "압력 중 수동 복원" 일시 보호는 여기서 끝난다.
            for record in self.records.values_mut() {
                record.protected_until_normal = false;
            }
        }

        let focused: HashSet<&str> = focused.iter().map(SessionId::as_str).collect();
        // 계획은 결정적이어야 한다(스냅샷·시험 재현성): workload_id 순.
        let mut ordered: Vec<&LiveWorkload> = live.iter().collect();
        ordered.sort_by(|a, b| a.workload_id.as_str().cmp(b.workload_id.as_str()));

        let mut ops = Vec::new();
        // 1) 포커스를 얻은 자동 양보는 압력 level과 무관하게 즉시 복원한다
        //    (§0-3: 보고 있는 pane은 절대 느리게 두지 않는다).
        for workload in &ordered {
            let Some(record) = self.records.get(&workload.workload_id) else {
                continue;
            };
            if focused.contains(workload.session_id.as_str())
                && record.relief.is_yielded()
                && !record.manual_yield
            {
                ops.push(ReliefOp::Restore {
                    workload_id: workload.workload_id.clone(),
                });
            }
        }
        let planned: HashSet<&str> = ops.iter().map(|op| op.workload_id().as_str()).collect();
        let planned: HashSet<String> = planned.into_iter().map(str::to_string).collect();

        if normal {
            // 2) NORMAL 회복: 자동 양보를 오래된 것부터 하나씩,
            //    `relief_release_interval` 간격으로 되돌린다(§2).
            if self.release_due(now_ms) {
                let next = ordered
                    .iter()
                    .filter(|w| !planned.contains(w.workload_id.as_str()))
                    .filter_map(|w| {
                        let record = self.records.get(&w.workload_id)?;
                        (!record.manual_yield)
                            .then(|| record.since_ms())
                            .flatten()
                            .map(|since| (since, w.workload_id.clone()))
                    })
                    .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.as_str().cmp(b.1.as_str())));
                if let Some((_, workload_id)) = next {
                    self.last_release_ms = Some(now_ms);
                    ops.push(ReliefOp::Restore { workload_id });
                }
            }
            return ops;
        }

        // 3) WARNING/CRITICAL: 대상 전부를 한 번에 양보시킨다(§2 — 우선순위는
        //    경합이 있을 때만 작동하므로 WARNING에서 켜도 손해가 없다).
        if !capability_supported || !self.policy.auto_yield {
            return ops;
        }
        for workload in ordered {
            if workload.state != WorkloadState::Running
                || planned.contains(workload.workload_id.as_str())
                || focused.contains(workload.session_id.as_str())
            {
                continue;
            }
            let record = self.records.get(&workload.workload_id);
            if record.is_some_and(|r| r.protected() || r.unsupported) {
                continue;
            }
            let apply = match record.map(|r| &r.relief) {
                None | Some(ReliefState::None) => true,
                // 일부 멤버만 걸린 양보는 압력이 남아 있는 동안 다시 시도한다
                // (그 사이 새로 생긴 자손도 이때 걸린다).
                Some(ReliefState::Yielded { partial, .. }) => *partial,
            };
            if apply {
                ops.push(ReliefOp::Yield {
                    workload_id: workload.workload_id.clone(),
                });
            }
        }
        ops
    }

    /// 수동 조작(`session.relief`). 수동은 항상 자동보다 우선한다(§0-2).
    pub fn manual(
        &mut self,
        workload_id: &WorkloadId,
        action: ReliefAction,
        cpu_level: PressureLevel,
    ) -> Vec<ReliefOp> {
        let record = self.records.entry(workload_id.clone()).or_default();
        match action {
            ReliefAction::Yield => {
                record.manual_yield = true;
                match &mut record.relief {
                    // 이미 양보 중이면 소유권만 수동으로 넘긴다 — 같은 정책을
                    // 다시 걸 필요가 없고 `since_ms`도 그대로 둔다.
                    ReliefState::Yielded { manual, .. } => {
                        *manual = true;
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
    /// 비워서 같은 호출이 두 번 나가지 않게 한다.
    pub fn shutdown_restore_ops(&mut self) -> Vec<ReliefOp> {
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
                if error.kind() == io::ErrorKind::Unsupported {
                    // 이 워크로드의 경로로는 되돌릴 수 없다 — 다시 시도하지
                    // 않는다(§0-4). 다른 워크로드는 그대로 관리한다.
                    self.records
                        .entry(workload_id.clone())
                        .or_default()
                        .unsupported = true;
                }
                // 실패는 이전 상태를 그대로 둔다 — 걸리지 않은 양보를
                // "양보 중"으로 광고하지 않는다.
                tracing::debug!(%workload_id, ?op, %error, "relief op failed");
                return false;
            }
        };
        let record = self.records.entry(workload_id).or_default();
        let next = match op {
            ReliefOp::Yield { .. } => {
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
            ReliefOp::Restore { .. } => ReliefState::None,
        };
        let changed = record.relief != next;
        record.relief = next;
        changed
    }

    fn release_due(&self, now_ms: u64) -> bool {
        match self.last_release_ms {
            None => true,
            Some(last) => now_ms.saturating_sub(last) >= self.release_interval_ms,
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
    if let Some(group) = group {
        return state.platform.set_scheduling(&group, op.tier());
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
    let identities = crate::telemetry_loop::scan_shell_tree_verified(&root);
    scheduling::set_process_scheduling(&identities, op.tier())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: u64 = 3_000;

    fn controller(auto_yield: bool) -> ReliefController {
        ReliefController::new(ReliefPolicy { auto_yield }, INTERVAL)
    }

    fn workload(state: WorkloadState) -> LiveWorkload {
        LiveWorkload {
            workload_id: WorkloadId::generate(),
            session_id: SessionId::generate(),
            state,
        }
    }

    fn running() -> LiveWorkload {
        workload(WorkloadState::Running)
    }

    fn ok() -> io::Result<SchedulingOutcome> {
        Ok(SchedulingOutcome {
            applied: 2,
            failed: 0,
            skipped_reused: 0,
        })
    }

    fn partial() -> io::Result<SchedulingOutcome> {
        Ok(SchedulingOutcome {
            applied: 1,
            failed: 1,
            skipped_reused: 0,
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

    /// §2: WARNING이면 포커스가 아닌 RUNNING 세션 **전부**를 한 번에
    /// 양보시킨다. 포커스 세션은 절대 대상이 아니다(§0-3).
    #[test]
    fn warning_yields_every_unfocused_running_workload_at_once() {
        let mut c = controller(true);
        let a = running();
        let b = running();
        let focused = running();
        let live = vec![a.clone(), b.clone(), focused.clone()];

        let ops = c.plan(
            1_000,
            PressureLevel::Warning,
            &[focused.session_id.clone()],
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
        assert!(c
            .plan(
                2_000,
                PressureLevel::Warning,
                &[focused.session_id.clone()],
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
        let ops = c.plan(
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

        let ops = c.plan(1_000, PressureLevel::Warning, &[], &live, true);
        assert_eq!(yielded_ids(&ops), vec![other.workload_id.as_str()]);
        settle(&mut c, &ops, 1_000);

        // 보호를 풀면 다음 틱에 다시 대상이 된다.
        c.manual(
            &guarded.workload_id,
            ReliefAction::Unprotect,
            PressureLevel::Warning,
        );
        let ops = c.plan(2_000, PressureLevel::Warning, &[], &live, true);
        assert_eq!(yielded_ids(&ops), vec![guarded.workload_id.as_str()]);
    }

    /// 보호를 걸면 이미 양보 중이던 세션도 즉시 복원된다.
    #[test]
    fn protect_restores_an_already_yielded_workload() {
        let mut c = controller(true);
        let w = running();
        let live = vec![w.clone()];
        let ops = c.plan(1_000, PressureLevel::Warning, &[], &live, true);
        settle(&mut c, &ops, 1_000);
        assert!(c.view(&w.workload_id).0.is_yielded());

        let ops = c.manual(
            &w.workload_id,
            ReliefAction::Protect,
            PressureLevel::Warning,
        );
        assert_eq!(restored_ids(&ops), vec![w.workload_id.as_str()]);
        settle(&mut c, &ops, 1_100);
        assert_eq!(c.view(&w.workload_id).0, ReliefState::None);
        // 그리고 압력이 남아 있어도 다시 걸리지 않는다.
        assert!(c
            .plan(2_000, PressureLevel::Warning, &[], &live, true)
            .is_empty());
    }

    /// §2: NORMAL 회복 뒤에는 한 번에 하나씩 `relief_release_interval`
    /// 간격으로만 복원한다.
    #[test]
    fn normal_recovery_restores_one_at_a_time_on_the_release_interval() {
        let mut c = controller(true);
        let a = running();
        let b = running();
        let live = vec![a.clone(), b.clone()];
        let ops = c.plan(1_000, PressureLevel::Warning, &[], &live, true);
        assert_eq!(ops.len(), 2);
        settle(&mut c, &ops, 1_000);

        // 첫 NORMAL 틱: 정확히 하나.
        let first = c.plan(5_000, PressureLevel::Normal, &[], &live, true);
        assert_eq!(first.len(), 1, "한 번에 하나 (got {first:?})");
        settle(&mut c, &first, 5_000);

        // 간격 전에는 아무것도 풀지 않는다.
        assert!(c
            .plan(
                5_000 + INTERVAL - 1,
                PressureLevel::Normal,
                &[],
                &live,
                true
            )
            .is_empty());

        let second = c.plan(5_000 + INTERVAL, PressureLevel::Normal, &[], &live, true);
        assert_eq!(second.len(), 1);
        settle(&mut c, &second, 5_000 + INTERVAL);
        assert_eq!(c.view(&a.workload_id).0, ReliefState::None);
        assert_eq!(c.view(&b.workload_id).0, ReliefState::None);
        // 더 풀 것이 없으면 조용하다.
        assert!(c
            .plan(20_000, PressureLevel::Normal, &[], &live, true)
            .is_empty());
    }

    /// 순차 복원은 가장 오래 양보한 것부터다.
    #[test]
    fn the_oldest_yield_is_released_first() {
        let mut c = controller(true);
        let old = running();
        let recent = running();
        let live = vec![old.clone(), recent.clone()];

        let ops = c.plan(1_000, PressureLevel::Warning, &[], &[old.clone()], true);
        settle(&mut c, &ops, 1_000);
        let ops = c.plan(4_000, PressureLevel::Warning, &[], &live, true);
        settle(&mut c, &ops, 4_000);

        let first = c.plan(9_000, PressureLevel::Normal, &[], &live, true);
        assert_eq!(restored_ids(&first), vec![old.workload_id.as_str()]);
    }

    /// §0-3: 포커스를 얻으면 압력 level과 무관하게 즉시 복원된다.
    #[test]
    fn gaining_focus_restores_immediately_even_under_pressure() {
        let mut c = controller(true);
        let w = running();
        let live = vec![w.clone()];
        let ops = c.plan(1_000, PressureLevel::Critical, &[], &live, true);
        settle(&mut c, &ops, 1_000);
        assert!(c.view(&w.workload_id).0.is_yielded());

        let ops = c.plan(
            1_500,
            PressureLevel::Critical,
            &[w.session_id.clone()],
            &live,
            true,
        );
        assert_eq!(restored_ids(&ops), vec![w.workload_id.as_str()]);
        settle(&mut c, &ops, 1_500);
        assert_eq!(c.view(&w.workload_id).0, ReliefState::None);
        // 그리고 포커스가 있는 한 다시 걸리지 않는다.
        assert!(c
            .plan(
                2_500,
                PressureLevel::Critical,
                &[w.session_id.clone()],
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
        assert!(c
            .plan(9_000, PressureLevel::Normal, &[], &live, true)
            .is_empty());
        assert!(c
            .plan(
                12_000,
                PressureLevel::Normal,
                &[w.session_id.clone()],
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
        let ops = c.plan(1_000, PressureLevel::Warning, &[], &[w.clone()], true);
        settle(&mut c, &ops, 1_000);

        let ops = c.manual(&w.workload_id, ReliefAction::Yield, PressureLevel::Warning);
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
        let ops = c.plan(1_000, PressureLevel::Warning, &[], &live, true);
        settle(&mut c, &ops, 1_000);

        let ops = c.manual(
            &w.workload_id,
            ReliefAction::Restore,
            PressureLevel::Warning,
        );
        assert_eq!(restored_ids(&ops), vec![w.workload_id.as_str()]);
        settle(&mut c, &ops, 2_000);
        let (relief, protected) = c.view(&w.workload_id);
        assert_eq!(relief, ReliefState::None);
        assert!(protected, "압력이 풀릴 때까지 보호된다");

        // 압력이 남아 있는 동안은 계속 면제.
        assert!(c
            .plan(3_000, PressureLevel::Warning, &[], &live, true)
            .is_empty());
        assert!(c
            .plan(4_000, PressureLevel::Critical, &[], &live, true)
            .is_empty());

        // NORMAL을 한 번 지나면 일시 보호가 풀린다.
        assert!(c
            .plan(5_000, PressureLevel::Normal, &[], &live, true)
            .is_empty());
        assert!(!c.view(&w.workload_id).1, "NORMAL에서 보호가 풀린다");
        let ops = c.plan(6_000, PressureLevel::Warning, &[], &live, true);
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
        assert!(c
            .plan(1_000, PressureLevel::Critical, &[], &[w.clone()], false)
            .is_empty());

        // capability는 지원한다고 했지만 실제 호출이 Unsupported를 돌려주면
        // **그 워크로드만** 더 시도하지 않는다 — 위임 cgroup Linux에서
        // 그룹 없는 직접 셸 하나가 나머지 완화까지 꺼서는 안 된다.
        let mut c = controller(true);
        let other = running();
        let live = vec![w.clone(), other.clone()];
        let ops = c.plan(1_000, PressureLevel::Warning, &[], &live, true);
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
        assert!(c
            .plan(2_000, PressureLevel::Warning, &[], &live, true)
            .is_empty());
    }

    /// 자동 양보 스위치가 꺼져 있으면 자동 계획은 없지만 수동은 그대로 된다.
    #[test]
    fn auto_yield_off_disables_automatic_planning_only() {
        let mut c = controller(false);
        let w = running();
        let live = vec![w.clone()];
        assert!(c
            .plan(1_000, PressureLevel::Critical, &[], &live, true)
            .is_empty());

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
        let ops = c.plan(1_000, PressureLevel::Warning, &[], &live, true);
        assert!(c.record(&ops[0], &partial(), 1_000));
        assert!(matches!(
            c.view(&w.workload_id).0,
            ReliefState::Yielded { partial: true, .. }
        ));

        let retry = c.plan(2_000, PressureLevel::Warning, &[], &live, true);
        assert_eq!(yielded_ids(&retry), vec![w.workload_id.as_str()]);
        assert!(c.record(&retry[0], &ok(), 2_000), "이제 완전히 걸렸다");
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
        assert!(c
            .plan(3_000, PressureLevel::Warning, &[], &live, true)
            .is_empty());
    }

    /// 사라진 워크로드의 기록은 버린다 — 유령에게 OS 호출을 보내지 않는다.
    #[test]
    fn records_of_dead_workloads_are_dropped_without_an_os_call() {
        let mut c = controller(true);
        let gone = running();
        let alive = running();
        let ops = c.plan(
            1_000,
            PressureLevel::Warning,
            &[],
            &[gone.clone(), alive.clone()],
            true,
        );
        settle(&mut c, &ops, 1_000);
        assert!(c.view(&gone.workload_id).0.is_yielded());

        // `gone`이 목록에서 빠진다: 복원 op 없이 기록만 사라진다.
        let ops = c.plan(9_000, PressureLevel::Normal, &[], &[alive.clone()], true);
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
        let ops = c.plan(
            1_000,
            PressureLevel::Warning,
            &[],
            &[auto.clone(), manual.clone()],
            true,
        );
        settle(&mut c, &ops, 1_000);
        c.manual(
            &manual.workload_id,
            ReliefAction::Yield,
            PressureLevel::Warning,
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
}
