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
use term_contracts::metrics::{MetricQuality, UsageCoverage};
use term_contracts::rpc::RpcEventKind;

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
    let mut shell_providers: HashMap<WorkloadId, PidProvider> = HashMap::new();
    loop {
        if *shutdown.borrow() {
            return;
        }
        tick(&state, &mut shell_providers);
        std::thread::sleep(state.config.telemetry_interval());
    }
}

fn tick(state: &Arc<DaemonState>, shell_providers: &mut HashMap<WorkloadId, PidProvider>) {
    let now = state.now_ms();
    let sample = {
        let mut telemetry = state.telemetry.lock().unwrap_or_else(|p| p.into_inner());
        telemetry.poll_host(now)
    };

    // Pressure classification with hysteresis (term-core).
    let level = {
        let mut pressure = state.pressure.lock().unwrap_or_else(|p| p.into_inner());
        pressure.update(sample.total_bytes().unwrap_or(0), sample.available_bytes())
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
    run_guard(state, now, cpu_level, level);

    // resource.snapshot to every control connection (1 s cadence).
    state.broadcast_control(
        RpcEventKind::ResourceSnapshot,
        serde_json::json!({
            "revision": state.revision(),
            "host": sample,
            "pressure": level,
            "cpu_pressure": cpu_level,
        }),
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
    let live = state.live_workloads_for_relief();
    let focused = state.focused_session_ids();
    let supported = state.scheduling_yield_supported;
    // 가드 정책의 CPU 한도를 넘긴 워크로드 — 정지 대신 양보 대상이다(가드는
    // CPU 초과를 더 이상 정지 사유로 다루지 않는다). guard 락을 잡고 푼 뒤에
    // relief 락을 잡는다(두 락을 동시에 쥐지 않는다 — 기존 교착 규율).
    let (cpu_limit, sustain_ms) = {
        let guard = state.guard.lock().unwrap_or_else(|p| p.into_inner());
        let policy = guard.policy();
        (
            f64::from(policy.cpu_cores_limit.max(1)),
            policy.sustain_ms.get(),
        )
    };
    let usage = state
        .usage_cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let cpu_over: Vec<WorkloadId> = live
        .iter()
        .filter(|w| {
            usage.get(&w.workload_id).is_some_and(|u| {
                matches!(
                    u.cpu_cores.quality,
                    MetricQuality::Measured | MetricQuality::Estimated
                ) && u.cpu_cores.value.is_some_and(|c| c > cpu_limit)
            })
        })
        .map(|w| w.workload_id.clone())
        .collect();
    let ops = {
        let mut relief = state.relief.lock().unwrap_or_else(|p| p.into_inner());
        relief.plan(
            now, cpu_level, &focused, &live, supported, &cpu_over, sustain_ms,
        )
    };
    if ops.is_empty() {
        return;
    }
    let mut changed = false;
    for op in ops {
        let outcome = crate::relief::apply(state, &op);
        let mut relief = state.relief.lock().unwrap_or_else(|p| p.into_inner());
        changed |= relief.record(&op, &outcome, now);
    }
    if changed {
        // 스냅샷이 새 완화 상태를 들고 가게 한다(UI 배지).
        state.bump_revision();
    }
}

/// 한 틱의 자원 가드(08 §5): 계획 → (락 해제) → 적용 → 기록. 완화와 같은
/// 락 규율을 따른다.
fn run_guard(
    state: &Arc<DaemonState>,
    now: u64,
    cpu_level: term_contracts::metrics::PressureLevel,
    mem_level: term_contracts::metrics::PressureLevel,
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
            let measured = |q: term_contracts::metrics::MetricQuality| {
                matches!(
                    q,
                    term_contracts::metrics::MetricQuality::Measured
                        | term_contracts::metrics::MetricQuality::Estimated
                )
            };
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
            }
        })
        .collect();
    let focused = state.focused_session_ids();
    // 자동 재개 판정용 호스트 CPU 사용률(회복 구간 이하인지).
    let host_cpu_percent = state
        .host
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .0
        .as_ref()
        .and_then(measured_cpu_cores)
        .map(|cores| cores * 100.0 / f64::from(state.logical_cpus.max(1)));
    let ops = {
        let mut guard = state.guard.lock().unwrap_or_else(|p| p.into_inner());
        guard.plan(now, cpu_level, mem_level, &focused, &live, host_cpu_percent)
    };
    if ops.is_empty() {
        return;
    }
    let mut changed = false;
    for op in ops {
        let outcome = crate::guard::apply(state, &op);
        let mut guard = state.guard.lock().unwrap_or_else(|p| p.into_inner());
        changed |= guard.record(&op, &outcome, now);
    }
    if changed {
        state.bump_revision();
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
        // Inventory: the group's verifiable member identities.
        let members = state.platform.member_identities(&group).unwrap_or_default();
        let platform = Arc::clone(&state.platform);
        let provider: Arc<dyn Fn() -> Vec<term_contracts::ids::ProcessIdentity> + Send + Sync> =
            Arc::new(move || platform.member_identities(&group).unwrap_or_default());

        let usage = {
            let mut telemetry = state.telemetry.lock().unwrap_or_else(|p| p.into_inner());
            telemetry.track_workload(workload_id.clone(), provider);
            telemetry.poll_workload(&workload_id, now, UsageCoverage::Group)
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
        let _ = members;
        state.ledger.update_resident(&workload_id, resident);
        state
            .usage_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(workload_id, usage);
    }
}

/// Observe direct shell sessions: they have no resource group (02-runner §3),
/// so their process set is the PTY root pid's tree. Usage lands in the
/// snapshot's `usage` column exactly like a managed workload's, but it NEVER
/// feeds the reservation ledger — shell consumption is not part of the
/// managed reservation sum (spec `03-resources.md` §3).
///
/// F1: 관측의 닻은 맨 루트 pid가 아니라 스폰 때 찍어 둔 루트 신원
/// ([`DaemonState::shell_identity`])이다. 닻 신원과 살아 있는 pid의 신원이
/// 어긋나면(루트 종료 뒤 pid 재사용) 그 워크로드를 관측에서 뺀다 — 그룹
/// 백엔드가 그룹을 잃으면 멈추는 것과 같은 규칙이다.
fn refresh_shells(
    state: &Arc<DaemonState>,
    now: u64,
    providers: &mut HashMap<WorkloadId, PidProvider>,
) {
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
    // 감시 집합에서 빠진 워크로드의 공급자는 버린다(샘플러 쪽 등록 해제는
    // 종료 경로가 `untrack_workload`로 이미 한다).
    let live: std::collections::HashSet<&WorkloadId> = anchored.iter().map(|(id, _)| id).collect();
    providers.retain(|id, _| live.contains(id));

    let rescan_ms = state.config.process_inventory_interval_ms();
    for (workload_id, root) in &anchored {
        // F1: 틱마다 닻을 재검증한다 — 루트가 이미 죽었거나 pid가 재사용됐으면
        // 관측에서 내려뜨린다(위의 레지스트리 락은 이미 놓았고, 신원 검증은
        // sysctl/proc 읽기 한 번이다).
        if !shell_anchor_alive(root) {
            {
                let mut telemetry = state.telemetry.lock().unwrap_or_else(|p| p.into_inner());
                telemetry.untrack_workload(workload_id);
            }
            state
                .usage_cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(workload_id);
            providers.remove(workload_id);
            continue;
        }
        let provider = providers
            .entry(workload_id.clone())
            .or_insert_with(|| shell_pid_provider(root.clone(), rescan_ms))
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

/// F1: 셸 루트 닻이 아직 원래 프로세스인가. 죽음·재사용·조회 불가는 모두
/// 거짓이다(01 §1: `None`은 "일치"가 아니라 "검증 불가"다).
fn shell_anchor_alive(root: &ProcessIdentity) -> bool {
    term_platform::identity::process_identity(root.pid).is_some_and(|live| live.same_process(root))
}

/// pid 공급자: 셸 루트의 프로세스 트리를 `timing_ms.process_inventory`
/// (2 s)마다 한 번만 다시 훑고, 그 사이 폴(1 s)에는 캐시를 돌려준다.
/// 신원을 확인할 수 없는 pid는 건너뛴다 — 추측한 신원으로는 아무것도
/// 계량하지 않는다(01 §1의 pid 재사용 방어).
///
/// F1: 캐시를 돌려주기 **전에** 닻 신원을 재검증한다 — 루트가 죽은 뒤 pid가
/// 재사용되면 캐시에 담긴 옛 트리가 무고한 프로세스에 묶일 수 있어서다.
/// 닻이 죽으면 캐시를 비우고 빈 목록을 돌려준다.
fn shell_pid_provider(root: ProcessIdentity, rescan_ms: u64) -> PidProvider {
    let cache: Mutex<Option<(std::time::Instant, Vec<ProcessIdentity>)>> = Mutex::new(None);
    Arc::new(move || {
        let mut guard = cache.lock().unwrap_or_else(|p| p.into_inner());
        if !shell_anchor_alive(&root) {
            *guard = None;
            return Vec::new();
        }
        if let Some((scanned_at, identities)) = guard.as_ref() {
            if (scanned_at.elapsed().as_millis() as u64) < rescan_ms {
                return identities.clone();
            }
        }
        let identities = scan_shell_tree_verified(&root);
        *guard = Some((std::time::Instant::now(), identities.clone()));
        identities
    })
}

/// F1: 닻 신원을 먼저 검증한 뒤 루트 pid와 관찰된 모든 자손의 검증된 신원을
/// 돌려준다. 루트 pid가 이미 다른 프로세스의 것이면(재사용) 빈 목록이다.
/// 완화 적용(08 §2)도 같은 관측을 쓴다 — 셸에는 그룹이 없으므로 이 목록이
/// 곧 멤버십이다.
pub(crate) fn scan_shell_tree_verified(root: &ProcessIdentity) -> Vec<ProcessIdentity> {
    if !shell_anchor_alive(root) {
        return Vec::new();
    }
    term_platform::proc_scan::scan_process_trees(&[root.pid])
        .remove(&root.pid)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|brief| term_platform::identity::process_identity(brief.pid))
        .collect()
}

/// 맨 루트 pid로 트리를 훑는 표시 전용 열거.
///
/// F1 경고: 닻 신원 검증이 없다 — 재사용된 루트 pid를 물 수 있으므로 소유권
/// 판정·스케줄링·계량에 쓰지 않는다(그 경로는 [`scan_shell_tree_verified`]).
/// 남은 호출자는 dispatch의 `workload.processes` 나열뿐이다.
pub(crate) fn scan_shell_tree(root_pid: u32) -> Vec<ProcessIdentity> {
    term_platform::proc_scan::scan_process_trees(&[root_pid])
        .remove(&root_pid)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|brief| term_platform::identity::process_identity(brief.pid))
        .collect()
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
    #[test]
    fn verified_scan_follows_a_live_anchor_and_rejects_a_recycled_pid() {
        let mut child = spawn_shell_like_child();
        let root = term_platform::identity::process_identity(child.id()).expect("child identity");

        let members = scan_shell_tree_verified(&root);
        assert!(
            members.iter().any(|m| m.same_process(&root)),
            "루트는 자기 트리의 멤버다"
        );
        assert!(shell_anchor_alive(&root));

        // 같은 pid에 다른 start token = 재사용 상황. 무고한 프로세스에게
        // 이 워크로드의 관측·완화가 묶여서는 안 된다.
        let mut recycled = root.clone();
        recycled.start_token = format!("{}-recycled", root.start_token);
        assert!(scan_shell_tree_verified(&recycled).is_empty());
        assert!(!shell_anchor_alive(&recycled));

        let _ = child.kill();
        let _ = child.wait();
    }

    /// F1: 닻이 죽으면 재스캔 창(2 s 캐시)이 열려 있어도 캐시를 못 믿는다.
    #[test]
    fn provider_cache_is_invalidated_when_the_anchor_dies() {
        let mut child = spawn_shell_like_child();
        let root = term_platform::identity::process_identity(child.id()).expect("child identity");
        let provider = shell_pid_provider(root.clone(), 60_000);
        let first = provider();
        assert!(first.iter().any(|m| m.same_process(&root)));

        let _ = child.kill();
        let _ = child.wait();
        // 캐시 만료(60 s)는 훨씬 뒤지만 닻 검증이 캐시를 무효화한다.
        assert!(provider().is_empty());
    }
}
