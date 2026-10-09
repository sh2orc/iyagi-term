//! B26 (spec `06-verification.md` §3): host telemetry stale for >3 s.
//!
//! With `telemetry_interval` raised to 6 s, a launch issued after the seed
//! sample has aged past the 3 s staleness bound stays QUEUED with reason
//! WAIT_TELEMETRY (new managed launches wait; existing workloads keep
//! running), and admission proceeds once a fresh sample lands (ticks at
//! ~6 s from daemon start).

mod common;

use common::{launch_request, Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
fn b26_stale_telemetry_queues_new_launches_but_keeps_running_work() {
    // Timing anchor: the seed sample is taken during daemon startup, and the
    // telemetry loop re-samples every 6 s. Workload 2 must be launched with
    // the sample older than 3 s but before the 6 s refresh.
    let t0 = Instant::now();
    let daemon = DaemonProc::spawn(
        "b26-stale",
        Some(common::relaxed_admission(
            json!({"timing_ms": {"telemetry": 6000}}),
        )),
    );
    assert!(
        t0.elapsed() < Duration::from_millis(1500),
        "daemon startup took {:?}; the staleness window is not reliably testable",
        t0.elapsed()
    );
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);

    // Workload 1 launches against the fresh seed sample: RUNNING.
    let first = client
        .request(
            "workload.launch",
            launch_request("managed", &["echo"], "1048576"),
        )
        .expect("first launch");
    common::ensure_running(&mut client, &first, Duration::from_millis(2500));
    assert_eq!(first["state"], "RUNNING", "fresh telemetry admits: {first}");

    // Sleep into the stale window (seed is >3 s old, next tick at ~6 s).
    while Instant::now() < t0 + Duration::from_millis(4200) {
        std::thread::sleep(Duration::from_millis(50));
    }

    // Workload 2 must be QUEUED (WAIT_TELEMETRY), not started.
    let second = client
        .request(
            "workload.launch",
            launch_request("managed", &["echo"], "1048576"),
        )
        .expect("second launch");
    assert_eq!(
        second["state"], "QUEUED",
        "stale telemetry must hold new managed launches: {second}"
    );
    assert_ne!(second["workload_id"], first["workload_id"]);

    // The scheduler stamps the reason within a tick or two.
    let deadline = Instant::now() + Duration::from_secs(5);
    let reason = loop {
        let summary = common::snapshot_workload(&mut client, &second["workload_id"])
            .expect("queued workload");
        if let Some(reason) = summary["queue_reason"].as_str() {
            break reason.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "queue_reason must surface for the waiting workload: {summary}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(reason, "WAIT_TELEMETRY", "got reason {reason:?}");

    // The existing workload is unaffected by telemetry staleness.
    let mine = common::snapshot_workload(&mut client, &first["workload_id"]).expect("first");
    assert_eq!(
        mine["state"], "RUNNING",
        "running work is unaffected: {mine}"
    );

    // Once the 6 s telemetry tick lands a fresh sample, admission proceeds.
    let started = common::wait_workload_state(
        &mut client,
        &second["workload_id"],
        &["RUNNING"],
        Duration::from_secs(8),
    );
    assert_eq!(started["state"], "RUNNING", "got {started}");
    let mine = common::snapshot_workload(&mut client, &first["workload_id"]).expect("first");
    assert_eq!(mine["state"], "RUNNING");

    // Cleanup.
    for id in [first["workload_id"].clone(), second["workload_id"].clone()] {
        let _ = client.request(
            "workload.cancel",
            json!({"request_id": common::uuid_v4(), "workload_id": id}),
        );
    }
}
