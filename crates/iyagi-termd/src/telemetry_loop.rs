//! Telemetry loop (1 s tick): host sample → pressure trackers (memory + CPU)
//! → `resource.snapshot` broadcast; per-workload usage refresh (sampler over
//! the group member identities) feeding the reservation ledger's resident
//! estimates and the snapshot usage column (spec `03-resources.md` §2–3).
//!
//! 직접 셸 워크로드에는 자원 그룹이 없으므로(02-runner §3) 관측은 PTY 루트
//! pid의 프로세스 트리를 훑어서 한다(spec `08-pressure-relief.md` §1,
//! `04-ui.md`: "direct shell workload는 session 단위 자원 관측이다").
//! 셸 사용량은 관리 예약 합에 절대 더하지 않는다(03 §3).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use term_contracts::ids::{ProcessIdentity, WorkloadId};
use term_contracts::metrics::{Metric, MetricQuality, UsageCoverage, WorkloadUsage};
use term_contracts::rpc::RpcEventKind;
use term_contracts::workload::GroupKind;

use crate::state::DaemonState;

/// pid 공급자(샘플러 계약: 공급자가 자기 재열거 주기를 소유한다).
type PidProvider = Arc<dyn Fn() -> Vec<ProcessIdentity> + Send + Sync>;

/// Supervised loop body (`supervisor::spawn_supervised`): re-runnable, all
/// per-run resources (shutdown receiver, shell providers) are created inside
/// so a restart after a panic starts from a clean state.
pub fn run(state: Arc<DaemonState>) {
    // 종료 신호 수신기는 루프 밖에서 한 번만 만든다(재시작마다 새로 만든다).
    let shutdown = state.shutdown.subscribe();
    // 셸 공급자는 워크로드마다 하나씩 살려 둔다 — 틱마다 새로 만들면
    // 2 s 재열거 캐시가 매번 버려져 매초 트리를 훉게 된다.
    let mut shell_providers = ShellObserver::new();
    loop {
        if *shutdown.borrow() {
            return;
        }
        tick(&state, &mut shell_providers);
        std::thread::sleep(state.config.telemetry_interval());
    }
}

fn tick(state: &Arc<DaemonState>, shell_providers: &mut ShellObserver) {
    let now = state.now_ms();
    let sample = {
        let mut telemetry = state.telemetry.lock().unwrap_or_else(|p| p.into_inner());
        telemetry.poll_host(now)
    };

    // Pressure classification with hysteresis (term-core). 커널 메모리 압력
    // 신호(P2-4)를 함께 넘긴다: critical은 즉시 CRITICAL이고 신호가 남아 있는
    // 동안 회복하지 않는다(예전에는 이미 CRITICAL이면 신호를 건너뛰어, macOS의
    // 넉넉한 가용 수치만 보고 10초 만에 NORMAL로 내려갔다가 다음 틱에 다시
    // CRITICAL이 되며 가드가 얼렸다 풀기를 반복했다). 지속된 warn은 WARNING.
    let level = {
        let kernel = term_platform::telemetry::kernel_memory_pressure();
        let mut pressure = state.pressure.lock().unwrap_or_else(|p| p.into_inner());
        pressure.update_with_kernel(
            sample.total_bytes().unwrap_or(0),
            sample.available_bytes(),
            kernel,
        )
    };
    // CPU 포화도도 같은 규칙으로(08 §1). 관측 불가(미측정/논리 코어 0)는
    // 0이 아니라 "모름"으로 넘겨 현재 레벨을 유지하게 한다.
    let cpu_level = {
        let mut cpu = state.cpu_pressure.lock().unwrap_or_else(|p| p.into_inner());
        cpu.update(measured_cpu_cores(&sample), sample.logical_cpu_count)
    };
    // 히스테리시스를 거친 값만 밖으로 나간다 — 샘플러가 채운 NORMAL을 덮는다.
    let mut sample = sample;
    sample.cpu_pressure = cpu_level;
    {
        let mut host = state.host.lock().unwrap_or_else(|p| p.into_inner());
        host.0 = Some(sample.clone());
        host.1 = now;
    }
    *state
        .pressure_level
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = level;
    *state
        .cpu_pressure_level
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = cpu_level;

    refresh_workloads(state, now);
    refresh_shells(state, now, shell_providers);
    // 압력 완화(08 §2)는 관측이 끝난 뒤에 돈다: 방금 갱신한 셸 트리와
    // 포커스 집합을 그대로 쓴다. 플랫폼 호출은 블로킹이므로 async
    // executor가 아니라 이 텔레메트리 스레드에서만 일어난다.
    run_relief(state, now, cpu_level);
    run_guard(state, now, cpu_level, level, &sample);
    crate::lifecycle::recheck_reconciliation(state);

    // resource.snapshot to every control connection (1 s cadence). 살아 있는
    // 워크로드의 최신 CPU/RSS도 싣는다 — UI 배지가 스냅샷을 다시 받지 않고도
    // 살아 있게(예전에는 이벤트가 없으면 배지가 마지막 값에 멈춰 있었다).
    let usage = {
        let live: std::collections::HashSet<WorkloadId> = state
            .live_workloads_for_relief()
            .into_iter()
            .map(|w| w.workload_id)
            .collect();
        let cache = state.usage_cache.lock().unwrap_or_else(|p| p.into_inner());
        let mut usage: Vec<term_contracts::metrics::WorkloadUsageBrief> = cache
            .iter()
            .filter(|(id, _)| live.contains(*id))
            .map(|(id, u)| term_contracts::metrics::WorkloadUsageBrief {
                workload_id: id.clone(),
                cpu_cores: u.cpu_cores.clone(),
                resident_bytes: u.resident_bytes.clone(),
            })
            .collect();
        usage.sort_by(|a, b| a.workload_id.as_str().cmp(b.workload_id.as_str()));
        usage
    };
    let event = term_contracts::metrics::ResourceSnapshotEvent {
        revision: state.revision(),
        host: sample,
        pressure: level,
        cpu_pressure: cpu_level,
        usage,
    };
    state.broadcast_control(
        RpcEventKind::ResourceSnapshot,
        serde_json::to_value(&event).unwrap_or(serde_json::Value::Null),
    );
}

/// 한 틱의 완화 정책: 계획 → (락 해제) → 적용 → 기록.
///
/// 컨트롤러 락을 쥔 채로 플랫폼이나 레지스트리를 만지지 않는다 —
/// 스냅샷 경로는 `workloads → relief` 순으로 잠그므로 반대 방향이 생기면
/// 교착이다.
fn run_relief(
    state: &Arc<DaemonState>,
    now: u64,
    cpu_level: term_contracts::metrics::PressureLevel,
) {
    let mut live = state.live_workloads_for_relief();
    let exempt = state.exempt_session_ids();
    let supported = state.scheduling_yield_supported;
    // 가드 정책의 CPU 한도와 지속 창 — 독점 양보의 기준이다(가드는 CPU
    // 초과를 정지 사유로 다루지 않는다). guard 락을 잡고 푼 뒤에 relief 락을
    // 잡는다(두 락을 동시에 쥐지 않는다 — 교착 규율).
    let (cpu_limit, sustain_ms) = {
        let guard = state.guard.lock().unwrap_or_else(|p| p.into_inner());
        let policy = guard.policy();
        (
            f64::from(policy.cpu_cores_limit.max(1)),
            policy.sustain_ms.get(),
        )
    };
    {
        let usage = state.usage_cache.lock().unwrap_or_else(|p| p.into_inner());
        for workload in &mut live {
            workload.cpu_cores = usage.get(&workload.workload_id).and_then(|u| {
                measured(u.cpu_cores.quality)
                    .then_some(u.cpu_cores.value)
                    .flatten()
            });
        }
    }
    let live_ids: std::collections::HashSet<&str> =
        live.iter().map(|w| w.workload_id.as_str()).collect();
    state.governance_ledger.retain(&live_ids);
    let ops = {
        let mut relief = state.relief.lock().unwrap_or_else(|p| p.into_inner());
        relief.plan(
            now, cpu_level, &exempt, &live, supported, cpu_limit, sustain_ms,
        )
    };
    let mut changed = Vec::new();
    for op in ops {
        // 종료 경로가 시작됐으면 새 양보는 걸지 않는다(복원과의 경합).
        if matches!(op, crate::relief::ReliefOp::Yield { .. })
            && state
                .governance_closing
                .load(std::sync::atomic::Ordering::Acquire)
        {
            continue;
        }
        let outcome = crate::relief::apply(state, &op);
        let mut relief = state.relief.lock().unwrap_or_else(|p| p.into_inner());
        if relief.record(&op, &outcome, now) {
            changed.push(op.workload_id().clone());
        }
    }
    // UI 배지가 자동 양보·복원을 보게 한다(revision도 함께 오른다).
    for workload_id in changed {
        state.workload_state_changed(&workload_id);
    }
}

/// 측정·추정 품질만 정책 입력으로 인정한다(관측 불가는 0이 아니다).
fn measured(quality: MetricQuality) -> bool {
    matches!(quality, MetricQuality::Measured | MetricQuality::Estimated)
}

/// 한 틱의 자원 가드(08 §5): 계획 → (락 해제) → 적용 → 기록. 완화와 같은
/// 락 규율을 따른다.
fn run_guard(
    state: &Arc<DaemonState>,
    now: u64,
    cpu_level: term_contracts::metrics::PressureLevel,
    mem_level: term_contracts::metrics::PressureLevel,
    sample: &term_contracts::metrics::HostSample,
) {
    if !state.suspend_resume_supported {
        return;
    }
    // 살아 있는 워크로드에 귀속 사용량을 붙인다(관리·셸 모두 usage_cache에
    // 있다). 측정·추정 품질만 가드 입력으로 인정한다.
    let usage = state
        .usage_cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    // 완화 쪽 보호 표시(sticky·압력 중 수동 복원)도 가드의 자동 정지 대상에서
    // 뺀다 — relief 락을 먼저 잡고 푼 뒤 guard 락을 잠근다(교착 규율).
    let live_raw = state.live_workloads_for_relief();
    let protected_ids: HashSet<WorkloadId> = {
        let relief = state.relief.lock().unwrap_or_else(|p| p.into_inner());
        live_raw
            .iter()
            .map(|w| w.workload_id.clone())
            .filter(|id| relief.is_protected(id))
            .collect()
    };
    let live: Vec<crate::guard::LiveUsage> = live_raw
        .into_iter()
        .map(|w| {
            let u = usage.get(&w.workload_id);
            let protected = protected_ids.contains(&w.workload_id);
            crate::guard::LiveUsage {
                workload_id: w.workload_id,
                session_id: w.session_id,
                state: w.state,
                cpu_cores: u.and_then(|u| {
                    measured(u.cpu_cores.quality)
                        .then_some(u.cpu_cores.value)
                        .flatten()
                }),
                resident_bytes: u
                    .and_then(|u| {
                        measured(u.resident_bytes.quality)
                            .then(|| u.resident_bytes.value.clone())
                            .flatten()
                    })
                    .map(|v| v.get()),
                protected,
                priority: w.priority,
                busy: w.busy,
            }
        })
        .collect();
    let exempt = state.exempt_session_ids();
    // 자동 재개 판정용 호스트 CPU 사용률(회복 구간 이하인지).
    let host_cpu_percent = measured_cpu_cores(sample)
        .map(|cores| cores * 100.0 / f64::from(state.logical_cpus.max(1)));
    // 회복 재개의 여유 판정: 가용 − 예비량 S(음수 가능). 모르면 None.
    let headroom = match (sample.total_bytes(), sample.available_bytes()) {
        (Some(total), Some(available)) => {
            let reserve = state
                .pressure
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .config()
                .host_reserve_bytes(total);
            Some(
                i64::try_from(available)
                    .unwrap_or(i64::MAX)
                    .saturating_sub(i64::try_from(reserve).unwrap_or(i64::MAX)),
            )
        }
        _ => None,
    };
    let (ops, warning_changes) = {
        let mut guard = state.guard.lock().unwrap_or_else(|p| p.into_inner());
        guard.set_host_headroom(headroom);
        let ops = guard.plan(now, cpu_level, mem_level, &exempt, &live, host_cpu_percent);
        (ops, guard.take_warning_changes())
    };
    let mut changed: Vec<WorkloadId> = warning_changes;
    for op in ops {
        // 종료 경로가 시작됐으면 새 정지는 걸지 않는다(재개와의 경합).
        if matches!(op, crate::guard::GuardOp::Suspend { .. })
            && state
                .governance_closing
                .load(std::sync::atomic::Ordering::Acquire)
        {
            continue;
        }
        let outcome = crate::guard::apply(state, &op);
        if let Err(error) = &outcome {
            tracing::debug!(workload = %op.workload_id(), %error, "guard op failed");
        } else {
            tracing::info!(workload = %op.workload_id(), ?op, "resource guard applied");
        }
        let mut guard = state.guard.lock().unwrap_or_else(|p| p.into_inner());
        if guard.record(&op, &outcome, now) {
            changed.push(op.workload_id().clone());
        }
    }
    changed.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    changed.dedup_by(|a, b| a.as_str() == b.as_str());
    // UI가 자동 정지·재개·경고를 보게 한다(revision도 함께 오른다).
    for workload_id in changed {
        state.workload_state_changed(&workload_id);
    }
}

/// Busy logical cores from a host sample, or `None` when the number is not
/// a measurement (spec `03-resources.md` §2: unavailable is never 0).
pub fn measured_cpu_cores(sample: &term_contracts::metrics::HostSample) -> Option<f64> {
    match sample.cpu_cores_used.quality {
        MetricQuality::Measured | MetricQuality::Estimated => sample.cpu_cores_used.value,
        MetricQuality::Unavailable => None,
    }
}

/// Refresh usage for running managed workloads and feed `M_i` (resident
/// estimates) into the reservation ledger.
fn refresh_workloads(state: &Arc<DaemonState>, now: u64) {
    let running: Vec<_> = {
        let registry = state.workloads.lock().unwrap_or_else(|p| p.into_inner());
        registry
            .values()
            .filter_map(|entry| {
                let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
                (guard.state == term_contracts::state::WorkloadState::Running
                    && guard.group.is_some())
                .then_some((
                    guard.workload_id.clone(),
                    guard.group.clone(),
                    guard.root_exited,
                    guard.actor.clone(),
                ))
            })
            .collect()
    };

    for (workload_id, group, root_exited, actor) in running {
        let Some(group) = group else { continue };
        // Mid-flight root exit (spec 02-runner §5): surface root_exited=true
        // while the workload stays RUNNING with owned descendants alive.
        if !root_exited && actor.as_ref().is_some_and(|a| actor_root_exited(a)) {
            if let Some(entry) = state.workload_entry(&workload_id) {
                let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
                if !guard.root_exited {
                    guard.root_exited = true;
                    let _ = state.storage.set_root_exited(&workload_id, true);
                    drop(guard);
                    state.workload_state_changed(&workload_id);
                }
            }
        }
        // 관측 트리(macOS 등)는 그룹 커널 계정이 아니라 관찰한 트리다 —
        // 커버리지를 그대로 밝힌다.
        let coverage = match group.kind {
            GroupKind::ObservedTree => UsageCoverage::ObservedTree,
            _ => UsageCoverage::Group,
        };
        let platform = Arc::clone(&state.platform);
        let provider: Arc<dyn Fn() -> Vec<term_contracts::ids::ProcessIdentity> + Send + Sync> =
            Arc::new(move || platform.member_identities(&group).unwrap_or_default());

        let per_pid = {
            let mut telemetry = state.telemetry.lock().unwrap_or_else(|p| p.into_inner());
            telemetry.track_workload(workload_id.clone(), provider);
            telemetry.poll_workload(&workload_id, now, coverage)
        };
        let usage = match (per_pid, sample_group_accounting(state, &workload_id, now)) {
            (Some(per_pid), Some(group)) => Some(merge_group_accounting(per_pid, group)),
            (per_pid, group) => per_pid.or(group),
        };
        let Some(usage) = usage else { continue };

        // Resident estimate for admission math (`M_i`); committed / accounted
        // values stay workload-usage-only (never summed, spec §2).
        let resident = usage
            .resident_bytes
            .value
            .as_ref()
            .map(|v| v.get())
            .or_else(|| usage.committed_bytes.value.as_ref().map(|v| v.get()));
        state.ledger.update_resident(&workload_id, resident);
        state
            .usage_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(workload_id, usage);
    }
}

/// 커널이 그룹 단위로 세는 백엔드(cgroup v2, Windows Job)의 계정. 관측
/// 트리(macOS 등)에는 그룹 계정이 없으므로 `None`.
///
/// 레지스트리가 쥔 핸들 자체로 샘플한다: 핸들을 복제하면 차분 기준(이전
/// 누적 CPU)도 복제돼 틱마다 같은 옛 기준과 비교하게 된다. 파일 몇 개
/// (Job은 질의 몇 번)를 읽는 동안만 그 워크로드의 잠금을 쥔다.
fn sample_group_accounting(
    state: &Arc<DaemonState>,
    workload_id: &WorkloadId,
    now: u64,
) -> Option<WorkloadUsage> {
    let entry = state.workload_entry(workload_id)?;
    let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
    let group = guard.group.as_ref()?;
    if !matches!(group.kind, GroupKind::Cgroup | GroupKind::Job) {
        return None;
    }
    match state.platform.sample_group(group, now) {
        Ok(usage) => Some(usage),
        Err(error) => {
            tracing::debug!(%workload_id, %error, "group accounting unavailable; per-pid only");
            None
        }
    }
}

/// 그룹 계정이 재는 값은 그쪽을 쓴다: CPU·IO는 끝난 자손까지 포함한 커널
/// 누적 카운터의 차분이고, 프로세스 수·accounted(cgroup `memory.current`)·
/// committed(Job 커밋)는 per-pid 합산이 모르는 값이다. 상주 메모리는 per-pid
/// 쪽이 먼저다 — cgroup의 RSS 합은 공유 페이지를 이중으로 세는 추정이고
/// per-pid는 footprint를 잰다. 어느 쪽이든 측정값이 없으면 다른 쪽을 쓴다.
fn merge_group_accounting(per_pid: WorkloadUsage, group: WorkloadUsage) -> WorkloadUsage {
    WorkloadUsage {
        workload_id: per_pid.workload_id,
        cpu_cores: pick_metric(group.cpu_cores, per_pid.cpu_cores),
        resident_bytes: pick_metric(per_pid.resident_bytes, group.resident_bytes),
        accounted_bytes: pick_metric(group.accounted_bytes, per_pid.accounted_bytes),
        committed_bytes: pick_metric(group.committed_bytes, per_pid.committed_bytes),
        read_bytes_per_sec: pick_metric(group.read_bytes_per_sec, per_pid.read_bytes_per_sec),
        write_bytes_per_sec: pick_metric(group.write_bytes_per_sec, per_pid.write_bytes_per_sec),
        network_rx_bytes_per_sec: pick_metric(
            group.network_rx_bytes_per_sec,
            per_pid.network_rx_bytes_per_sec,
        ),
        network_tx_bytes_per_sec: pick_metric(
            group.network_tx_bytes_per_sec,
            per_pid.network_tx_bytes_per_sec,
        ),
        process_count: pick_metric(group.process_count, per_pid.process_count),
        coverage: if group.coverage == UsageCoverage::Group {
            UsageCoverage::Group
        } else {
            per_pid.coverage
        },
    }
}

/// 측정값이 있는 쪽, 둘 다 없으면 값이 있는 `primary`, 그것도 없으면
/// `fallback`(사유를 싣고 간다).
fn pick_metric<T>(primary: Metric<T>, fallback: Metric<T>) -> Metric<T> {
    let measured = |m: &Metric<T>| m.value.is_some() && m.quality == MetricQuality::Measured;
    if measured(&primary) || (!measured(&fallback) && primary.value.is_some()) {
        primary
    } else {
        fallback
    }
}

/// 직접 셸 관측 상태(텔레메트리 스레드 소유).
pub(crate) struct ShellObserver {
    providers: HashMap<WorkloadId, PidProvider>,
    last_scan: Option<std::time::Instant>,
    /// env 태그 판정을 끝낸 고아((pid, 시작 토큰) → 워크로드). 프로세스마다
    /// 한 번만 env를 읽는다.
    orphan_tags: HashMap<(u32, String), Option<WorkloadId>>,
    /// 이보다 먼저 시작한 고아는 태그를 읽지 않는다(지금 셸들은 모두 이 데몬
    /// 뒤에 떴다 — 그 전 고아의 태그는 옛 워크로드의 것이다).
    started_epoch_ms: u64,
}

impl ShellObserver {
    pub(crate) fn new() -> Self {
        Self {
            providers: HashMap::new(),
            last_scan: None,
            orphan_tags: HashMap::new(),
            started_epoch_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }
}

/// Observe direct shell sessions: they have no resource group (02-runner §3),
/// so their process set is the PTY root pid's tree. Usage lands in the
/// snapshot's `usage` column exactly like a managed workload's, but it NEVER
/// feeds the reservation ledger — shell consumption is not part of the
/// managed reservation sum (spec `03-resources.md` §3).
///
/// F1: 관측의 닻은 맨 루트 pid가 아니라 스폰 때 찍어 둔 루트 신원이다.
///
/// 멤버는 `timing_ms.process_inventory`(2 s)마다 **한 번의 스캔**으로 모든
/// 셸을 함께 계산한다(예전에는 셸마다 따로 전체 프로세스 테이블을 훑어 pane
/// 수에 비례해 비용이 늘었다). 멤버 = 루트 트리 ∪ 예전에 확인된 멤버 중 아직
/// 같은 프로세스인 것(`cmd &`·nohup으로 부모가 바뀐 자손이 계량·양보·정지에서
/// 빠지지 않게) ∪ 이 워크로드의 env 태그(`IYAGI_WORKLOAD_ID`)가 붙은 고아.
fn refresh_shells(state: &Arc<DaemonState>, now: u64, observer: &mut ShellObserver) {
    let shells: Vec<(WorkloadId, u32)> = {
        let registry = state.workloads.lock().unwrap_or_else(|p| p.into_inner());
        registry
            .values()
            .filter_map(|entry| {
                let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
                let is_shell = guard.mode == term_contracts::launch::LaunchMode::Shell
                    && guard.state == term_contracts::state::WorkloadState::Running
                    && guard.group.is_none();
                if !is_shell {
                    return None;
                }
                // PTY 루트 pid가 없으면(아직 안 떴거나 못 읽었으면) 관측 대상이
                // 아니다 — 추측한 루트로 남의 트리를 계량하지 않는다.
                Some((guard.workload_id.clone(), guard.shell_pid?))
            })
            .collect()
    };
    // F1: 신원까지 기록된 셸만 닻이 있다. 신원이 없는 셸(스폰 직후 종료해
    // 못 찍은 경우)은 관측하지 않는다 — 맨 pid를 닻으로 대신 쓰면 재사용된
    // pid의 무고한 트리를 계량하게 된다(01 §1).
    let anchored: Vec<(WorkloadId, ProcessIdentity)> = shells
        .into_iter()
        .filter_map(|(workload_id, _root_pid)| {
            let identity = state.shell_identity(&workload_id)?;
            Some((workload_id, identity))
        })
        .collect();
    // 감시 집합에서 빠진 워크로드의 공급자·멤버는 버린다(샘플러 쪽 등록
    // 해제는 종료 경로가 `untrack_workload`로 이미 한다).
    let live: HashSet<&WorkloadId> = anchored.iter().map(|(id, _)| id).collect();
    observer.providers.retain(|id, _| live.contains(id));
    state
        .shell_members
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retain(|id, _| live.contains(id));

    // F1: 틱마다 닻을 재검증한다 — 루트가 이미 죽었거나 pid가 재사용됐으면
    // 관측에서 내려뜨린다(신원 검증은 sysctl/proc 읽기 한 번이다).
    let mut alive: Vec<(WorkloadId, ProcessIdentity)> = Vec::with_capacity(anchored.len());
    for (workload_id, root) in anchored {
        if shell_anchor_alive(&root) {
            alive.push((workload_id, root));
            continue;
        }
        {
            let mut telemetry = state.telemetry.lock().unwrap_or_else(|p| p.into_inner());
            telemetry.untrack_workload(&workload_id);
        }
        state
            .usage_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&workload_id);
        observer.providers.remove(&workload_id);
        state
            .shell_members
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&workload_id);
    }

    let rescan = std::time::Duration::from_millis(state.config.process_inventory_interval_ms());
    // 아직 한 번도 훑지 않은 셸(방금 뜬 셸)이 있으면 주기를 기다리지 않는다 —
    // 멤버가 없는 채로 폴하면 "모름"이 프로세스 0개·0바이트로 보고된다.
    let unseen = {
        let members = state
            .shell_members
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        alive.iter().any(|(id, _)| !members.contains_key(id))
    };
    if unseen || observer.last_scan.is_none_or(|at| at.elapsed() >= rescan) {
        observe_shell_members(state, &alive, observer);
        observer.last_scan = Some(std::time::Instant::now());
    }

    for (workload_id, root) in &alive {
        let provider = observer
            .providers
            .entry(workload_id.clone())
            .or_insert_with(|| {
                shell_member_provider(
                    Arc::clone(&state.shell_members),
                    workload_id.clone(),
                    root.clone(),
                )
            })
            .clone();
        let usage = {
            let mut telemetry = state.telemetry.lock().unwrap_or_else(|p| p.into_inner());
            telemetry.track_workload(workload_id.clone(), provider);
            telemetry.poll_workload(workload_id, now, UsageCoverage::ObservedTree)
        };
        let Some(usage) = usage else { continue };
        state
            .usage_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(workload_id.clone(), usage);
    }
}

/// 모든 셸의 멤버를 스냅샷 한 장으로 다시 계산해 `state.shell_members`에 둔다.
fn observe_shell_members(
    state: &Arc<DaemonState>,
    alive: &[(WorkloadId, ProcessIdentity)],
    observer: &mut ShellObserver,
) {
    let roots: Vec<u32> = alive.iter().map(|(_, root)| root.pid).collect();
    let snapshot =
        term_platform::native_proc::snapshot(term_platform::proc_scan::SHARED_SCAN_MAX_AGE)
            .and_then(|snapshot| {
                // 찍은 뒤에 뜬 셸이 있으면 한 번 새로 찍는다.
                if roots.iter().all(|pid| snapshot.get(*pid).is_some()) {
                    Some(snapshot)
                } else {
                    term_platform::native_proc::snapshot(std::time::Duration::ZERO)
                }
            });
    let Some(snapshot) = snapshot else {
        // 네이티브 목록이 없는 플랫폼(Windows): 루트 트리만.
        let mut trees = term_platform::proc_scan::scan_tree_identities(
            &roots,
            term_platform::proc_scan::SHARED_SCAN_MAX_AGE,
        );
        let next = alive
            .iter()
            .map(|(id, root)| (id.clone(), trees.remove(&root.pid).unwrap_or_default()))
            .collect();
        *state
            .shell_members
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = next;
        return;
    };
    let previous = state
        .shell_members
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let mut claimed: HashSet<u32> = HashSet::new();
    let mut next: HashMap<WorkloadId, Vec<ProcessIdentity>> = HashMap::new();
    for (workload_id, root) in alive {
        let known = previous.get(workload_id).map(Vec::as_slice).unwrap_or(&[]);
        let members = members_in(&snapshot, root, known);
        claimed.extend(members.iter().map(|m| m.pid));
        next.insert(workload_id.clone(), members);
    }
    // env 태그 고아: 데몬 뒤에 뜬, 같은 사용자의, 아직 누구의 멤버도 아닌
    // ppid=1 프로세스만 한 번씩 확인한다(이중 fork로 처음부터 트리 밖에서 뜬
    // 개발 서버·데몬).
    observer.orphan_tags.retain(|(pid, token), _| {
        snapshot
            .get(*pid)
            .is_some_and(|entry| &entry.start_token == token)
    });
    #[cfg(unix)]
    {
        // 안전성: 인자 없는 getuid 호출이다.
        let uid = unsafe { libc::getuid() };
        let mut tagged: Vec<(WorkloadId, u32)> = Vec::new();
        for entry in snapshot.entries() {
            if entry.ppid != 1
                || entry.uid != uid
                || entry.start_epoch_ms < observer.started_epoch_ms
                || claimed.contains(&entry.pid)
            {
                continue;
            }
            let owner = observer
                .orphan_tags
                .entry((entry.pid, entry.start_token.clone()))
                .or_insert_with(|| {
                    term_platform::native_proc::env_var(
                        entry.pid,
                        crate::orchestrator::ENV_WORKLOAD_ID,
                    )
                    .and_then(|value| WorkloadId::parse(&value).ok())
                })
                .clone();
            if let Some(owner) = owner.filter(|owner| next.contains_key(owner)) {
                tagged.push((owner, entry.pid));
            }
        }
        tagged.sort_by_key(|(_, pid)| *pid);
        for (owner, pid) in tagged {
            let extra: Vec<ProcessIdentity> = snapshot
                .descendants(pid)
                .into_iter()
                .filter(|pid| claimed.insert(*pid))
                .filter_map(|pid| snapshot.identity(pid))
                .collect();
            if let Some(members) = next.get_mut(&owner) {
                members.extend(extra);
            }
        }
    }
    *state
        .shell_members
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = next;
}

/// 스냅샷에서 한 셸의 멤버: 루트 트리 ∪ 예전 멤버 중 아직 같은 프로세스인
/// 것과 그 자손. 루트가 이미 다른 프로세스면(재사용) 빈 목록이다.
fn members_in(
    snapshot: &term_platform::native_proc::Snapshot,
    root: &ProcessIdentity,
    known: &[ProcessIdentity],
) -> Vec<ProcessIdentity> {
    if !snapshot.is_alive(root) {
        return Vec::new();
    }
    let mut pids = snapshot.descendants(root.pid);
    let mut seen: HashSet<u32> = pids.iter().copied().collect();
    for member in known {
        if seen.contains(&member.pid) || !snapshot.is_alive(member) {
            continue;
        }
        for pid in snapshot.descendants(member.pid) {
            if seen.insert(pid) {
                pids.push(pid);
            }
        }
    }
    pids.into_iter()
        .filter_map(|pid| snapshot.identity(pid))
        .collect()
}

/// F1: 셸 루트 닻이 아직 원래 프로세스인가. 죽음·재사용·조회 불가는 모두
/// 거짓이다(01 §1: `None`은 "일치"가 아니라 "검증 불가"다).
fn shell_anchor_alive(root: &ProcessIdentity) -> bool {
    term_platform::identity::process_identity(root.pid).is_some_and(|live| live.same_process(root))
}

/// pid 공급자: 관측 상태가 2초마다 갱신하는 멤버 목록을 돌려준다(폴은 1 s).
///
/// F1: 목록을 돌려주기 **전에** 닻 신원을 재검증한다 — 루트가 죽은 뒤 pid가
/// 재사용되면 예전 목록이 무고한 프로세스에 묶일 수 있어서다. 닻이 죽으면
/// 빈 목록을 돌려준다.
fn shell_member_provider(
    members: Arc<Mutex<HashMap<WorkloadId, Vec<ProcessIdentity>>>>,
    workload_id: WorkloadId,
    root: ProcessIdentity,
) -> PidProvider {
    Arc::new(move || {
        if !shell_anchor_alive(&root) {
            return Vec::new();
        }
        members
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&workload_id)
            .cloned()
            .unwrap_or_default()
    })
}

/// F1: 닻 신원을 먼저 검증한 뒤 지금 이 셸에 속한 프로세스의 검증된 신원을
/// 돌려준다 — 새로 찍은 스냅샷의 루트 트리와, 관측이 기억해 둔 멤버(부모가
/// 바뀐 자손·태그 고아) 중 아직 같은 프로세스인 것. 루트 pid가 이미 다른
/// 프로세스의 것이면(재사용) 빈 목록이다. 완화·가드 적용(08 §2·§5)이 쓴다 —
/// 셸에는 그룹이 없으므로 이 목록이 곧 멤버십이다.
pub(crate) fn scan_shell_tree_verified(
    state: &DaemonState,
    workload_id: &WorkloadId,
    root: &ProcessIdentity,
) -> Vec<ProcessIdentity> {
    if !shell_anchor_alive(root) {
        return Vec::new();
    }
    let known = state
        .shell_members
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(workload_id)
        .cloned()
        .unwrap_or_default();
    match term_platform::native_proc::snapshot(std::time::Duration::ZERO) {
        Some(snapshot) => members_in(&snapshot, root, &known),
        None => {
            term_platform::proc_scan::scan_tree_identities(&[root.pid], std::time::Duration::ZERO)
                .remove(&root.pid)
                .unwrap_or_default()
        }
    }
}

/// 맨 루트 pid로 트리를 훑는 표시 전용 열거.
///
/// F1 경고: 닻 신원 검증이 없다 — 재사용된 루트 pid를 물 수 있으므로 소유권
/// 판정·스케줄링·계량에 쓰지 않는다(그 경로는 [`scan_shell_tree_verified`]).
/// 남은 호출자는 dispatch의 `workload.processes` 나열뿐이다.
pub(crate) fn scan_shell_tree(root_pid: u32) -> Vec<ProcessIdentity> {
    term_platform::proc_scan::scan_tree_identities(
        &[root_pid],
        term_platform::proc_scan::SHARED_SCAN_MAX_AGE,
    )
    .remove(&root_pid)
    .unwrap_or_default()
}

/// Actor's mid-flight root-exit signal (None ⇒ actor already finalized).
fn actor_root_exited(actor: &term_pty::actor::SessionActorHandle) -> bool {
    actor.status().root_exited
}

#[cfg(test)]
mod shell_anchor_tests {
    use super::*;

    /// term-platform identity 테스트와 같은 방식의 단명하지 않은 자식.
    fn spawn_shell_like_child() -> std::process::Child {
        if cfg!(windows) {
            std::process::Command::new("ping")
                .args(["-n", "30", "127.0.0.1"])
                .spawn()
                .expect("spawn ping")
        } else {
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn sleep")
        }
    }

    /// F1: 살아 있는 닻은 자기 트리의 멤버로 관측되고, 같은 pid의 다른
    /// 신원(재사용)은 관측을 완전히 끊는다.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn verified_scan_follows_a_live_anchor_and_rejects_a_recycled_pid() {
        let mut child = spawn_shell_like_child();
        let root = term_platform::identity::process_identity(child.id()).expect("child identity");
        let snapshot =
            term_platform::native_proc::snapshot(std::time::Duration::ZERO).expect("native list");

        let members = members_in(&snapshot, &root, &[]);
        assert!(
            members.iter().any(|m| m.same_process(&root)),
            "루트는 자기 트리의 멤버다"
        );
        assert!(shell_anchor_alive(&root));

        // 같은 pid에 다른 start token = 재사용 상황. 무고한 프로세스에게
        // 이 워크로드의 관측·완화가 묶여서는 안 된다.
        let mut recycled = root.clone();
        recycled.start_token = format!("{}-recycled", root.start_token);
        assert!(members_in(&snapshot, &recycled, &[]).is_empty());
        assert!(!shell_anchor_alive(&recycled));

        let _ = child.kill();
        let _ = child.wait();
    }

    /// 부모가 바뀌어(셸이 먼저 끝나 고아가 됨) 루트 트리 밖으로 나간 예전
    /// 멤버도 같은 프로세스인 동안은 멤버로 남는다 — 계량·양보·정지에서
    /// 빠지지 않게. 죽은 예전 멤버는 빠진다.
    #[cfg(unix)]
    #[test]
    fn reparented_known_members_stay_observed_while_alive() {
        let mut root_child = spawn_shell_like_child();
        let root =
            term_platform::identity::process_identity(root_child.id()).expect("root identity");
        // `sh`가 sleep을 백그라운드로 띄우고 곧바로 끝난다 → sleep은 고아.
        let output = std::process::Command::new("sh")
            .args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"])
            .output()
            .expect("spawn orphan");
        let orphan_pid: u32 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("orphan pid");
        let orphan =
            term_platform::identity::process_identity(orphan_pid).expect("orphan identity");
        let snapshot =
            term_platform::native_proc::snapshot(std::time::Duration::ZERO).expect("native list");
        assert!(
            !snapshot.descendants(root.pid).contains(&orphan_pid),
            "고아는 루트 트리 밖이다"
        );
        let members = members_in(&snapshot, &root, std::slice::from_ref(&orphan));
        assert!(members.iter().any(|m| m.same_process(&orphan)));

        // 죽은 예전 멤버는 빠진다.
        unsafe { libc::kill(orphan_pid as libc::pid_t, libc::SIGKILL) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while term_platform::identity::process_identity(orphan_pid)
            .is_some_and(|live| live.same_process(&orphan))
        {
            assert!(std::time::Instant::now() < deadline, "orphan never died");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let snapshot =
            term_platform::native_proc::snapshot(std::time::Duration::ZERO).expect("native list");
        let members = members_in(&snapshot, &root, std::slice::from_ref(&orphan));
        assert!(!members.iter().any(|m| m.pid == orphan_pid));

        let _ = root_child.kill();
        let _ = root_child.wait();
    }

    /// F1: 닻이 죽으면 공급자는 기억된 멤버 목록을 더는 믿지 않는다.
    #[test]
    fn provider_stops_trusting_members_when_the_anchor_dies() {
        let mut child = spawn_shell_like_child();
        let root = term_platform::identity::process_identity(child.id()).expect("child identity");
        let workload_id = WorkloadId::generate();
        let members = Arc::new(Mutex::new(HashMap::from([(
            workload_id.clone(),
            vec![root.clone()],
        )])));
        let provider = shell_member_provider(Arc::clone(&members), workload_id, root.clone());
        assert!(provider().iter().any(|m| m.same_process(&root)));

        let _ = child.kill();
        let _ = child.wait();
        // 목록은 아직 그대로지만 닻 검증이 그것을 무효로 만든다.
        assert!(provider().is_empty());
    }
}

#[cfg(test)]
mod group_accounting_tests {
    use super::*;
    use term_contracts::ids::U64String;

    fn bytes(value: u64) -> U64String {
        U64String::new(value).expect("in range")
    }

    fn usage(coverage: UsageCoverage) -> WorkloadUsage {
        WorkloadUsage {
            workload_id: WorkloadId::generate(),
            cpu_cores: Metric::unavailable("t", "none"),
            resident_bytes: Metric::unavailable("t", "none"),
            accounted_bytes: Metric::unavailable("t", "none"),
            committed_bytes: Metric::unavailable("t", "none"),
            read_bytes_per_sec: Metric::unavailable("t", "none"),
            write_bytes_per_sec: Metric::unavailable("t", "none"),
            network_rx_bytes_per_sec: Metric::unavailable("t", "none"),
            network_tx_bytes_per_sec: Metric::unavailable("t", "none"),
            process_count: Metric::unavailable("t", "none"),
            coverage,
        }
    }

    /// CPU·프로세스 수·accounted는 커널 그룹 계정이 이기고(끝난 자손까지
    /// 센다), 상주 메모리는 per-pid footprint가 이긴다(cgroup RSS 합은
    /// 공유 페이지를 이중으로 센다). 첫 차분 샘플처럼 그룹 쪽 값이 없으면
    /// per-pid 값을 그대로 쓴다.
    #[test]
    fn group_counters_win_but_resident_memory_stays_per_pid() {
        let mut per_pid = usage(UsageCoverage::Group);
        per_pid.cpu_cores = Metric::measured("native", 0.5);
        per_pid.resident_bytes = Metric::measured("native.footprint", bytes(300));
        per_pid.read_bytes_per_sec = Metric::measured("native", 7.0);
        let id = per_pid.workload_id.clone();

        let mut group = usage(UsageCoverage::Group);
        group.cpu_cores = Metric::measured("cgroup.v2", 1.5);
        group.resident_bytes = Metric::estimated("proc.rss", bytes(900));
        group.accounted_bytes = Metric::measured("cgroup.v2", bytes(1200));
        group.process_count = Metric::measured("cgroup.v2", 4);
        group.read_bytes_per_sec = Metric::unavailable("cgroup.v2", "first differential sample");

        let merged = merge_group_accounting(per_pid, group);
        assert_eq!(merged.workload_id, id);
        assert_eq!(merged.cpu_cores.value, Some(1.5), "끝난 자손 포함 CPU");
        assert_eq!(
            merged.resident_bytes.value,
            Some(bytes(300)),
            "footprint 유지"
        );
        assert_eq!(merged.accounted_bytes.value, Some(bytes(1200)));
        assert_eq!(merged.process_count.value, Some(4));
        assert_eq!(
            merged.read_bytes_per_sec.value,
            Some(7.0),
            "그룹 첫 샘플: per-pid"
        );
        assert_eq!(merged.coverage, UsageCoverage::Group);
    }

    /// 측정값이 없으면 추정값이라도 있는 쪽, 둘 다 없으면 fallback의 사유.
    #[test]
    fn metric_pick_prefers_measured_then_any_value_then_the_fallback_reason() {
        let measured = || Metric::measured("m", 1u32);
        let estimated = || Metric::estimated("e", 2u32);
        let none = |why: &str| Metric::<u32>::unavailable("u", why);

        assert_eq!(pick_metric(measured(), estimated()).value, Some(1));
        assert_eq!(pick_metric(estimated(), measured()).value, Some(1));
        assert_eq!(pick_metric(estimated(), none("x")).value, Some(2));
        assert_eq!(pick_metric(none("x"), estimated()).value, Some(2));
        let both_missing = pick_metric(none("primary"), none("fallback"));
        assert_eq!(both_missing.reason.as_deref(), Some("fallback"));
    }
}
