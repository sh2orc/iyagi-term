//! Session limit (defaults 32; test override 3): the 4th concurrent
//! workload is rejected with SESSION_LIMIT while existing ones keep running
//! (spec §7 table).

mod common;

use common::Client;
use serde_json::json;
use std::time::Duration;

fn shell_request(args: Vec<String>) -> serde_json::Value {
    json!({
        "request_id": common::uuid_v4(),
        "profile_id": common::uuid_v4(),
        "cwd": std::env::temp_dir().to_string_lossy(),
        "program": common::fixture_bin(),
        "argv": args,
        "env_overrides": {},
        "mode": "shell",
        "cols": 80, "rows": 24, "priority": 1,
        "policy": {
            "reservation_bytes": "2147483648", "cpu_slots": 1,
            "enforcement": "observe",
            "memory_max_bytes": null, "cpu_max_cores": null, "pids_max": null,
        },
    })
}

#[test]
fn fourth_launch_hits_session_limit() {
    let daemon =
        common::DaemonProc::spawn("session-limit", Some(json!({"limits": {"sessions": 3}})));
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);

    for n in 0..3 {
        let launch = client
            .request("workload.launch", shell_request(vec!["echo".into()]))
            .unwrap_or_else(|e| panic!("launch {n} failed: {e}"));
        assert_eq!(launch["state"], "RUNNING", "launch {n}: {launch}");
    }

    let err = client
        .request("workload.launch", shell_request(vec!["echo".into()]))
        .expect_err("4th launch must hit the limit");
    assert_eq!(err["code"], "SESSION_LIMIT", "got {err}");

    // Existing executions are unaffected (they stay RUNNING).
    let snapshot = client
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    let running = snapshot["workloads"]
        .as_array()
        .expect("workloads")
        .iter()
        .filter(|w| w["state"] == "RUNNING")
        .count();
    assert_eq!(running, 3, "existing workloads keep running");
    let _ = Duration::from_millis(1);
}
