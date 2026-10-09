//! B31 (spec `06-verification.md` §3): idle daemon exit.
//!
//! With `idle_daemon_exit` lowered to 2 s: after the last client disconnects
//! and no workloads remain, the daemon process exits by itself (logs and
//! data files preserved); while a managed workload is still running, the
//! daemon stays alive past the idle window.

mod common;

use common::{launch_request, wait_workload_state, Client, DaemonProc};
use serde_json::json;
use std::time::Duration;

#[test]
fn b31_idle_daemon_exits_after_last_client_and_workload() {
    let mut daemon = DaemonProc::spawn(
        "b31-idle",
        Some(common::relaxed_admission(
            json!({"timing_ms": {"idle_daemon_exit": 2000}}),
        )),
    );
    {
        let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
        let launch = client
            .request(
                "workload.launch",
                launch_request(
                    "shell",
                    &["exit", "--code", "0", "--delay-ms", "300"],
                    "1048576",
                ),
            )
            .expect("short workload");
        assert_eq!(
            launch["state"], "RUNNING",
            "shell launches are never queued: {launch}"
        );
        let done = wait_workload_state(
            &mut client,
            &launch["workload_id"],
            &["SUCCEEDED"],
            Duration::from_secs(15),
        );
        assert_eq!(done["state"], "SUCCEEDED");
        // Drop the only client (control connection closes).
    }

    // No clients, no active workloads: the daemon exits by itself within the
    // idle window + polling slack.
    let status = common::wait_exit(&mut daemon.child, Duration::from_secs(12))
        .expect("daemon must self-exit when idle");
    assert!(status.success(), "clean exit expected, got {status}");

    // Logs/data preserved under the data dir.
    assert!(daemon.data_dir.join("data").is_dir(), "data dir preserved");
    assert!(
        daemon.data_dir.join("data/journals").is_dir(),
        "journals dir preserved"
    );
    assert!(
        common::count_entries(&daemon.data_dir.join("runtime")) > 0,
        "runtime artifacts (token/endpoint) preserved"
    );
}

#[test]
fn b31_active_workload_keeps_the_daemon_alive() {
    let mut daemon = DaemonProc::spawn(
        "b31-active",
        Some(common::relaxed_admission(
            json!({"timing_ms": {"idle_daemon_exit": 2000}}),
        )),
    );
    let workload_id = {
        let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
        // Long-hold managed workload (no descendants: depth 0).
        let launch = client
            .request(
                "workload.launch",
                launch_request(
                    "managed",
                    &[
                        "tree",
                        "--children",
                        "1",
                        "--depth",
                        "0",
                        "--hold-ms",
                        "12000",
                    ],
                    "1048576",
                ),
            )
            .expect("long workload");
        common::ensure_running(&mut client, &launch, Duration::from_secs(15));
        launch["workload_id"].clone()
    }
    .clone();

    // Client count is now 0, but the workload holds the daemon alive past
    // the 2 s idle window.
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        daemon.child.try_wait().expect("try_wait").is_none(),
        "daemon must stay alive while a workload runs"
    );

    // Reconnect: the workload survived the full detach.
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let summary = common::snapshot_workload(&mut client, &workload_id).expect("workload");
    assert_eq!(summary["state"], "RUNNING", "got {summary}");

    // Cleanup through the cancel path.
    let _ = client.request(
        "workload.cancel",
        json!({"request_id": common::uuid_v4(), "workload_id": workload_id}),
    );
    wait_workload_state(
        &mut client,
        &workload_id,
        &["CANCELLED", "SUCCEEDED"],
        Duration::from_secs(20),
    );
}
