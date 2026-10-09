//! Scheduler pump (250 ms tick): `WorkloadQueue::pick_next` with a live
//! admission closure that reserves atomically, then starts admitted
//! workloads; bypassed heads keep their wait reasons for the UI
//! (spec `03-resources.md` §3 — head-of-line bypass).

use std::sync::Arc;

use term_contracts::ids::WorkloadId;
use term_contracts::launch::LaunchMode;
use term_contracts::snapshot::QueueReason;
use term_contracts::state::WorkloadState;
use term_core::AdmissionRequest;

use crate::orchestrator;
use crate::state::DaemonState;

/// Run the scheduler loop on a dedicated thread until shutdown.
/// Supervised loop body (`supervisor::spawn_supervised`).
pub fn run(state: Arc<DaemonState>) {
    // 종료 신호 수신기는 루프 밖에서 한 번만 만든다(틱마다
    // `watch::Receiver`를 새로 할당하던 낭비를 없앤다).
    let shutdown = state.shutdown.subscribe();
    loop {
        if *shutdown.borrow() {
            return;
        }
        tick(&state);
        std::thread::sleep(state.config.scheduler_interval);
    }
}

fn tick(state: &Arc<DaemonState>) {
    let host = state.admission_host();
    let mut queue_changed = false;

    let outcome = state.queue.pick_next(|candidate| {
        let Some(entry) = state.workload_entry(candidate.workload_id) else {
            // Unknown entry: drop it from the queue as unadmissible forever.
            return QueueReason::ResourceUnschedulable;
        };
        let (policy, mode) = {
            let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
            (guard.policy.clone(), guard.mode)
        };
        if mode != LaunchMode::Managed {
            return QueueReason::ResourceUnschedulable;
        }
        let request = AdmissionRequest {
            reservation_bytes: policy.reservation_bytes.get(),
            cpu_slots: policy.cpu_slots,
        };
        match state
            .ledger
            .try_admit_and_reserve(&host, candidate.workload_id.clone(), request)
        {
            Ok(_) => QueueReason::Admit,
            Err(term_core::CoreError::AdmissionDenied { reason }) => reason,
            Err(_) => QueueReason::WaitTelemetry,
        }
    });

    for skipped in &outcome.skipped {
        // Record the wait reason (storage + registry) when it changed.
        let changed = state
            .workload_entry(&skipped.workload_id)
            .map(|entry| {
                let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
                let different = guard.queue_reason != Some(skipped.reason);
                guard.queue_reason = Some(skipped.reason);
                different
            })
            .unwrap_or(false);
        if changed {
            let _ = state
                .storage
                .set_queue_reason(&skipped.workload_id, Some(skipped.reason));
            queue_changed = true;
        }
    }

    if let Some(picked) = outcome.picked {
        queue_changed = true;
        let _ = state
            .storage
            .set_queue_reason(&picked.workload_id, Some(QueueReason::Admit));
        start_one(Arc::clone(state), picked.workload_id.clone());
    }

    if queue_changed {
        state.broadcast_queue_changed();
    }
}

/// Start one admitted workload on its own thread (the pipeline blocks on
/// PTY/gate IO).
fn start_one(state: Arc<DaemonState>, workload_id: WorkloadId) {
    // Verify it is still QUEUED before handing it to the pipeline.
    let valid = state
        .workload_entry(&workload_id)
        .map(|entry| entry.lock().unwrap_or_else(|p| p.into_inner()).state == WorkloadState::Queued)
        .unwrap_or(false);
    if !valid {
        state.ledger.release(&workload_id);
        return;
    }
    // Spawn failure must degrade, not abort the daemon: release the
    // reservation and leave the workload queued — the next scheduler pass
    // (or a fresh queue snapshot) retries it once threads are available.
    let worker_state = Arc::clone(&state);
    let worker_workload = workload_id.clone();
    let spawn_result = std::thread::Builder::new()
        .name(format!("launch-{workload_id}"))
        .spawn(move || orchestrator::start_admitted(worker_state, worker_workload));
    if let Err(error) = spawn_result {
        tracing::error!(
            workload = %workload_id,
            %error,
            "launch worker spawn failed — workload stays queued"
        );
        state.ledger.release(&workload_id);
    }
}
