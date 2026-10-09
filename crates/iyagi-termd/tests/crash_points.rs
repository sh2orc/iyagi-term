//! B03/B33 (spec `06-verification.md` §3): daemon killed at launch-pipeline
//! points, restarted on the same data dir.
//!
//! * after the launch reply (intent + STARTING + RUNNING committed): the
//!   prior workload reports INTERRUPTED after restart, nothing re-executes
//!   (side-effect file count unchanged), a RESENT request id returns the old
//!   workload's INTERRUPTED state without re-creating anything (B33), and a
//!   NEW request id is served after reconciliation — admitted normally, or
//!   held with WAIT_TELEMETRY when reconciliation found verifiable survivors
//!   (the documented conservative gate).
//! * mid-launch (intent before/after — a race from outside): whatever
//!   committed, after restart there is no auto-respawn and no duplicate
//!   side-effect run.

mod common;

use common::{launch_request, Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

fn journal_count(data_dir: &std::path::Path) -> usize {
    std::fs::read_dir(data_dir.join("data/journals"))
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "mtj"))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn b03_b33_crash_after_running_then_restart_recovers_without_respawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.keep();
    let effect_dir = tempfile::tempdir().expect("effect dir");
    let effect_file = effect_dir.path().join("once.txt");

    // Stage 1: run to the interesting point, then hard-kill (taskkill /F).
    let mut first = DaemonProc::spawn_on(
        data_dir.clone(),
        "b03-1",
        Some(common::relaxed_admission(json!({}))),
    );
    let (bystander_id, long_id, long_request, long_request_id) = {
        let (mut client, _) = Client::control(&first.endpoint, &first.token);

        // A finished side-effect workload (marker created exactly once).
        let argv = [
            "side-effect".to_string(),
            "--file".to_string(),
            effect_file.to_string_lossy().into_owned(),
        ];
        let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
        let marker_launch = client
            .request(
                "workload.launch",
                launch_request("managed", &argv_ref, "1048576"),
            )
            .expect("marker launch");
        common::ensure_running(&mut client, &marker_launch, Duration::from_secs(15));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !effect_file.is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(effect_file.is_file(), "side-effect marker must exist");
        let marker_id = marker_launch["workload_id"].clone();
        let _ = common::wait_workload_state(
            &mut client,
            &marker_id,
            &["SUCCEEDED"],
            Duration::from_secs(15),
        );

        // A long-running workload that will be interrupted by the crash.
        // The whole request Value is kept for the byte-identical B33 resend.
        let long_request = launch_request(
            "managed",
            &[
                "tree",
                "--children",
                "1",
                "--depth",
                "0",
                "--hold-ms",
                "30000",
            ],
            "1048576",
        );
        let request_id = long_request["request_id"]
            .as_str()
            .expect("rid")
            .to_string();
        let long_launch = client
            .request("workload.launch", long_request.clone())
            .expect("long launch");
        common::ensure_running(&mut client, &long_launch, Duration::from_secs(15));
        (
            marker_id,
            long_launch["workload_id"].clone(),
            long_request,
            request_id,
        )
    };
    let _ = bystander_id;

    common::force_kill(&mut first.child);
    assert!(
        first.child.wait().is_ok(),
        "daemon process must be dead before restart"
    );

    // Stage 2: restart on the same data dir.
    let second = DaemonProc::spawn_on(
        data_dir.clone(),
        "b03-2",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut client, _) = Client::control(&second.endpoint, &second.token);

    // The RUNNING workload is INTERRUPTED (never auto-respawned).
    let long_summary =
        common::snapshot_workload(&mut client, &long_id).expect("interrupted workload");
    assert_eq!(long_summary["state"], "INTERRUPTED", "got {long_summary}");
    assert_eq!(long_summary["last_error_code"], "DAEMON_RESTART");

    // Nothing re-executed: the marker still exists exactly once, and no new
    // journals appeared for it.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        effect_file.is_file(),
        "the side-effect record must survive the crash"
    );
    assert_eq!(
        common::count_entries(effect_dir.path()),
        1,
        "the target must not re-run after restart"
    );
    let journals_after_restart = journal_count(&data_dir);

    // B33: RESEND the byte-identical request id → the old workload's
    // INTERRUPTED state, no CLI re-creation (journal count unchanged).
    assert_eq!(
        long_request["request_id"].as_str(),
        Some(long_request_id.as_str()),
        "resend must carry the exact original request"
    );
    let resent = client
        .request("workload.launch", long_request)
        .expect("resend must answer");
    assert_eq!(resent["workload_id"], long_id, "got {resent}");
    assert_eq!(resent["state"], "INTERRUPTED", "got {resent}");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        journal_count(&data_dir),
        journals_after_restart,
        "a resent request id must not re-create any CLI process"
    );

    // A NEW request id is served after reconciliation: either admitted, or
    // (if reconciliation found verifiable live survivors) held with
    // WAIT_TELEMETRY — both are spec-compliant conservative behaviors.
    let fresh = client
        .request(
            "workload.launch",
            launch_request("managed", &["echo"], "1048576"),
        )
        .expect("new request after restart");
    let fresh_id = fresh["workload_id"].clone();
    assert!(
        fresh["state"] == "RUNNING" || fresh["state"] == "QUEUED",
        "got {fresh}"
    );
    if fresh["state"] == "RUNNING" {
        // Admitted directly: reconciliation found no survivors.
        eprintln!("b03: fresh launch admitted immediately after restart");
    } else {
        let deadline = Instant::now() + Duration::from_secs(5);
        let reason = loop {
            let summary =
                common::snapshot_workload(&mut client, &fresh_id).expect("fresh workload");
            if let Some(reason) = summary["queue_reason"].as_str() {
                break reason.to_string();
            }
            assert!(
                Instant::now() < deadline,
                "held launch must show its wait reason: {summary}"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        assert_eq!(
            reason, "WAIT_TELEMETRY",
            "reconciliation gate must surface WAIT_TELEMETRY, got {reason}"
        );
        eprintln!("b03: reconciliation holds new launches (WAIT_TELEMETRY)");
    }

    // Cleanup the fresh workload.
    let _ = client.request(
        "workload.cancel",
        json!({"request_id": common::uuid_v4(), "workload_id": fresh_id}),
    );
    let _ = common::wait_workload_state(
        &mut client,
        &fresh_id,
        &["CANCELLED", "RUNNING", "SUCCEEDED"],
        Duration::from_secs(15),
    );
}

#[test]
fn b03_crash_mid_launch_leaves_no_respawn_and_no_duplicates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.keep();
    let effect_dir = tempfile::tempdir().expect("effect dir");
    let effect_file = effect_dir.path().join("race.txt");

    let mut daemon = DaemonProc::spawn_on(
        data_dir.clone(),
        "b03-race",
        Some(common::relaxed_admission(json!({}))),
    );
    {
        // Fire the launch and kill IMMEDIATELY: the crash lands somewhere in
        // the intent/STARTING/gate/RELEASE window (before/after the intent
        // commit — inherently racy from outside, and that is the point).
        let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
        let argv = [
            "side-effect".to_string(),
            "--file".to_string(),
            effect_file.to_string_lossy().into_owned(),
        ];
        let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
        client.fire(
            "workload.launch",
            launch_request("managed", &argv_ref, "1048576"),
        );
        std::thread::sleep(Duration::from_millis(40));
    }
    common::force_kill(&mut daemon.child);

    let restarted = DaemonProc::spawn_on(
        data_dir.clone(),
        "b03-race-2",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut client, _) = Client::control(&restarted.endpoint, &restarted.token);

    // Whatever committed: nothing auto-respawns (no live workload) and no
    // duplicate side-effect run appears over the next seconds.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = client
            .request("system.snapshot", json!({}))
            .expect("snapshot");
        let workloads = snapshot["workloads"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let live = workloads
            .iter()
            .filter(|w| {
                matches!(
                    w["state"].as_str(),
                    Some("QUEUED" | "STARTING" | "RUNNING" | "STOPPING" | "DRAINING")
                )
            })
            .count();
        assert_eq!(
            live, 0,
            "no auto-respawn after a mid-launch crash: {workloads:?}"
        );
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        common::count_entries(effect_dir.path()) <= 1,
        "at most one side-effect run even when the crash raced the pipeline"
    );
}
