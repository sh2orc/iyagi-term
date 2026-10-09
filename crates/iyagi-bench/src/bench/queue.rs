//! Benchmark 5 — queue wait under fixed concurrency=1 (spec §5 수집 항목
//! "queue wait p50/p95"): one long-running managed echo workload holds the
//! single slot, four managed echo workloads queue behind it; each wait is
//! measured from the instant its launch response (state=QUEUED) arrives to
//! the instant its `workload.changed` RUNNING broadcast arrives (client
//! arrival timestamps — the daemon stamps no wall-clock on transitions).
//!
//! Workloads drain sequentially: each is cancelled once RUNNING so the next
//! is admitted; the measured wait therefore includes the predecessor's
//! cancel drain and the ~250 ms scheduler tick — that is the real drain a
//! user sees with concurrency=1.

use std::time::Duration;

use serde_json::json;

use super::{cancel_and_wait, note, wait_workload_state_event, Ctx};
use crate::daemon::{self, DaemonProc};
use crate::report::{dist_from, measured_status, QueueWaitResult};
use crate::wire::{uuid_v4, Conn};

const DEPTH: usize = 4;

pub fn run(ctx: &Ctx) -> Result<QueueWaitResult, String> {
    note(format!(
        "queue_wait: concurrency=1, {DEPTH} queued managed echo workloads, {} build",
        ctx.profile
    ));

    let mut daemon: DaemonProc = DaemonProc::spawn(
        &ctx.daemon_bin,
        "queue",
        Some(json!({"limits": {"managed_concurrency": 1}})),
    )?;
    let outcome = measure(ctx, &mut daemon);
    match &outcome {
        Ok(result) => note(format!(
            "queue_wait: waits {:?} p50={:.0}ms p95={:.0}ms -> {}",
            result.waits_ms, result.dist.p50_ms, result.dist.p95_ms, result.status
        )),
        Err(err) => {
            note(format!("queue_wait FAILED: {err}"));
            note(format!("daemon stderr tail:\n{}", daemon.stderr_tail()));
        }
    }
    let _ = daemon::shutdown(&mut daemon, None);
    if !ctx.keep_data && outcome.is_ok() {
        daemon::cleanup_data_dir(&daemon.data_dir);
    }
    outcome
}

fn managed_launch(ctx: &Ctx, cwd: &std::path::Path) -> serde_json::Value {
    json!({
        "request_id": uuid_v4(),
        "profile_id": uuid_v4(),
        "cwd": cwd.to_string_lossy(),
        "program": ctx.fixture_bin.to_string_lossy(),
        "argv": ["echo"],
        "env_overrides": {},
        "mode": "managed",
        "cols": 80,
        "rows": 24,
        "priority": 1,
        "policy": {
            "reservation_bytes": "2147483648",
            "cpu_slots": 1,
            "enforcement": "observe",
            "memory_max_bytes": null,
            "cpu_max_cores": null,
            "pids_max": null,
        },
    })
}

fn measure(ctx: &Ctx, daemon: &mut DaemonProc) -> Result<QueueWaitResult, String> {
    let mut control = Conn::control(&daemon.endpoint, &daemon.token)?;
    let cwd = daemon.data_dir.clone();

    // Slot holder: first managed launch runs immediately.
    let holder_reply = control
        .request("workload.launch", managed_launch(ctx, &cwd))
        .map_err(|e| format!("holder launch failed: {e}"))?;
    if holder_reply["state"] != "RUNNING" {
        return Err(format!("holder not RUNNING: {holder_reply}"));
    }
    let holder = holder_reply["workload_id"]
        .as_str()
        .ok_or("holder missing workload_id")?
        .to_string();

    // Queue DEPTH more; record each QUEUED-response arrival instant.
    let mut queued: Vec<(String, std::time::Instant)> = Vec::new();
    for _ in 0..DEPTH {
        let (id, _t) = control
            .request_timed("workload.launch", managed_launch(ctx, &cwd))
            .map_err(|e| format!("queued launch write: {e}"))?;
        let reply = control
            .wait_response(&id, Duration::from_secs(15))
            .ok_or("queued launch response timeout")?
            .map_err(|e| format!("queued launch rejected: {e}"))?;
        if reply["state"] != "QUEUED" {
            return Err(format!("expected QUEUED, got {reply}"));
        }
        let workload_id = reply["workload_id"]
            .as_str()
            .ok_or("queued result missing workload_id")?
            .to_string();
        queued.push((workload_id, std::time::Instant::now()));
    }

    // Observe the published wait reason (scheduler stamps it within ~250 ms).
    let mut queue_reason = "unknown".to_string();
    let reason_deadline = std::time::Instant::now() + Duration::from_secs(3);
    while queue_reason == "unknown" && std::time::Instant::now() < reason_deadline {
        control.drain_events();
        if let Some(event) = control.pop_event("queue.changed") {
            if let Some(list) = event.value["payload"]["queue"].as_array() {
                if let Some(reason) = list.iter().find_map(|entry| entry["wait_reason"].as_str()) {
                    queue_reason = reason.to_string();
                }
            }
        }
        if queue_reason == "unknown" {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // Free the single slot so the queue starts draining: the first queued
    // workload's wait therefore includes the holder's cancel drain.
    cancel_and_wait(&mut control, &holder, Duration::from_secs(30))?;

    // Drain sequentially: RUNNING -> measure wait -> cancel -> next.
    let mut waits_ms: Vec<f64> = Vec::with_capacity(DEPTH);
    for (workload_id, t_queued) in queued {
        let t_running = wait_workload_state_event(
            &mut control,
            &workload_id,
            "RUNNING",
            Duration::from_secs(60),
        )?;
        waits_ms.push(t_running.duration_since(t_queued).as_secs_f64() * 1000.0);
        let state = cancel_and_wait(&mut control, &workload_id, Duration::from_secs(30))?;
        if state != "CANCELLED" {
            return Err(format!("queued workload ended {state}, expected CANCELLED"));
        }
    }

    Ok(QueueWaitResult {
        concurrency: 1,
        queued_depth: DEPTH,
        waits_ms: waits_ms.clone(),
        dist: dist_from(&waits_ms),
        queue_reason,
        status: measured_status(ctx.profile),
        notes: vec![
            "wait = QUEUED-response arrival -> workload.changed(RUNNING) arrival, both client-side (the daemon stamps no wall-clock on state transitions)".into(),
            "sequential drain: each wait includes the predecessor's cancel drain and the ~250 ms scheduler tick; managed launches also pay the gated-helper round trip".into(),
            "no numeric target in the spec for queue wait — reported for the baseline table".into(),
        ],
    })
}
