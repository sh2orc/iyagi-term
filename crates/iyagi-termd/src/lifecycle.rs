//! Daemon lifecycle: idle-exit watcher and crash-reconciliation assessment
//! (spec `02-runner.md` §1, `01-contracts.md` §6).

use std::sync::Arc;
use std::time::{Duration, Instant};

use term_contracts::ids::WorkloadId;
use term_contracts::state::WorkloadState;
use term_platform::identity;

use crate::state::DaemonState;

/// Assess the reconciliation report produced by `Storage::open`: every
/// workload left non-terminal by a previous daemon became INTERRUPTED;
/// `reconciliation_required` is set when an ownership row names a process
/// that is still verifiably alive (full identity match on pid, start_token
/// and boot_id; a bare pid is never enough, spec §1/§6). New managed
/// admission is then conservative (WAIT_TELEMETRY via the admission input).
pub fn assess_reconciliation(
    state: &Arc<DaemonState>,
    report: Vec<term_storage::ReconciledWorkload>,
) {
    let mut survivors = 0;
    let mut interrupted = Vec::new();
    // 스토리지가 이력의 진실 원본이다: 세션 id와 기록된 사실(mode·policy·
    // priority·exit)은 거기서 가져온다. 무작위 session_id를 만들면 스냅샷·
    // `session.search`·멱등 재전송이 같은 워크로드에 다른 id를 답한다.
    let stored_sessions = state.storage.sessions().unwrap_or_default();
    let mut registry = state.workloads.lock().unwrap_or_else(|p| p.into_inner());
    for reconciled in &report {
        let record = state
            .storage
            .workload_record(&reconciled.workload_id)
            .ok()
            .flatten();
        let session_id = stored_sessions
            .iter()
            .find(|s| s.workload_id == reconciled.workload_id)
            .map(|s| s.id.clone())
            .unwrap_or_else(term_contracts::ids::SessionId::generate);
        let policy = record
            .as_ref()
            .map(|r| term_contracts::launch::LaunchPolicy {
                reservation_bytes: r.reservation_bytes.clone(),
                cpu_slots: r.cpu_slots,
                enforcement: r.enforcement,
                memory_max_bytes: r.memory_max_bytes.clone(),
                cpu_max_cores: r.cpu_max_cores,
                pids_max: r.pids_max,
            })
            .unwrap_or(term_contracts::launch::LaunchPolicy {
                reservation_bytes: term_contracts::U64String::new(2 << 30).expect("fits"),
                cpu_slots: 1,
                enforcement: term_contracts::launch::Enforcement::Observe,
                memory_max_bytes: None,
                cpu_max_cores: None,
                pids_max: None,
            });
        // Register the reconciled workload so snapshots report it.
        let entry = crate::state::WorkloadEntry {
            workload_id: reconciled.workload_id.clone(),
            session_id,
            request_id: None,
            mode: record
                .as_ref()
                .map(|r| r.mode)
                .unwrap_or(term_contracts::launch::LaunchMode::Managed),
            state: term_contracts::state::WorkloadState::Interrupted,
            title: format!("{} (interrupted)", reconciled.workload_id),
            cwd: String::new(),
            program: String::new(),
            requested_policy: policy.clone(),
            policy,
            priority: record
                .as_ref()
                .map(|r| r.priority)
                .unwrap_or(term_contracts::launch::Priority(1)),
            descriptor: None,
            queue_reason: None,
            cancel_requested: record.as_ref().is_some_and(|r| r.cancel_requested),
            root_exited: record.as_ref().is_some_and(|r| r.root_exited),
            exit_code: record.as_ref().and_then(|r| r.exit_code),
            last_error_code: Some("DAEMON_RESTART".to_string()),
            missing_capabilities: Vec::new(),
            group: None,
            actor: None,
            reservation_released: true,
            reserved: false,
            gate_exit_code: std::sync::Mutex::new(None),
            connection: term_contracts::state::TerminalConnection::Detached,
            shell_pid: None,
            agent: None,
            finalized: true,
            claude_provider: None,
            redactor: None,
        };
        registry.insert(
            reconciled.workload_id.clone(),
            Arc::new(std::sync::Mutex::new(entry)),
        );
        interrupted.push(reconciled.workload_id.clone());

        if let Some(ownership) = &reconciled.ownership {
            let alive = identity::process_identity(ownership.identity.pid)
                .is_some_and(|current| current.same_process(&ownership.identity));
            if alive {
                survivors += 1;
                tracing::warn!(
                    workload = %reconciled.workload_id,
                    pid = ownership.identity.pid,
                    "reconciliation: owned process still alive; managed admission will be conservative"
                );
            }
        }
    }
    drop(registry);
    // 재조정된 INTERRUPTED도 종료 상태다 — 종료 워크로드 보관 링을 똑같이
    // 탄다(오래된 데이터 디렉터리를 다시 열어도 레지스트리가 유계로 남는다).
    for workload_id in &interrupted {
        state.note_workload_terminal(workload_id);
    }
    if survivors > 0 {
        state
            .reconciliation_required
            .store(true, std::sync::atomic::Ordering::Release);
    }
    if !report.is_empty() {
        tracing::info!(
            interrupted = report.len(),
            survivors,
            "crash reconciliation applied (no auto-respawn)"
        );
    }
}

/// Idle-exit: no clients AND no active workloads for `idle_exit` → clean
/// shutdown (spec 02-runner §1.5). Log retention alone never counts.
/// Supervised loop body (`supervisor::spawn_supervised`).
pub fn run_idle_watcher(state: Arc<DaemonState>) {
    {
        let threshold = state.config.idle_exit();
        // 종료 신호 수신기는 루프 밖에서 한 번만 만든다.
        let shutdown = state.shutdown.subscribe();
        loop {
            if *shutdown.borrow() {
                return;
            }
            std::thread::sleep(Duration::from_secs(1).min(threshold));
            let idle_for = {
                let last = *state
                    .last_activity
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let now_active = Instant::now();
                now_active.duration_since(last)
            };
            let clients = state
                .connections
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .len();
            let active = state.active_workload_count();
            if clients == 0 && active == 0 && idle_for >= threshold {
                tracing::info!(
                    idle_ms = idle_for.as_millis() as u64,
                    "idle exit: no clients and no active workloads"
                );
                let _ = state.shutdown.send(true);
                return;
            }
        }
    }
}

/// Wait until every registered workload reaches a terminal state (bounded).
pub fn wait_for_terminal(state: &Arc<DaemonState>, timeout: Duration) {
    wait_while_any(state, timeout, |s| !s.is_terminal());
}

/// Shutdown that keeps workloads (restart, OS signal): only workloads already
/// on their way out (STOPPING/DRAINING) get a moment to finish. A running
/// shell never ends by itself here, so waiting for it would just hold the
/// singleton lock for the whole timeout whenever a terminal is open — and the
/// app's restart, waiting for that lock, would be late by as much.
pub fn wait_for_winding_down(state: &Arc<DaemonState>, timeout: Duration) {
    wait_while_any(state, timeout, |s| {
        matches!(s, WorkloadState::Stopping | WorkloadState::Draining)
    });
}

fn wait_while_any(
    state: &Arc<DaemonState>,
    timeout: Duration,
    pending: impl Fn(WorkloadState) -> bool,
) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let active_ids: Vec<WorkloadId> = state
            .workloads
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter_map(|e| {
                let guard = e.lock().unwrap_or_else(|p| p.into_inner());
                pending(guard.state).then_some(guard.workload_id.clone())
            })
            .collect();
        if active_ids.is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
