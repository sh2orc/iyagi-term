//! Managed gated launch (02-runner §3): the gate-observer marker file is
//! NOT created until after launch returns RUNNING (the target only exists
//! post-RELEASE); cancel → terminal CANCELLED; the OS group ends empty.

mod common;

use common::{Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

fn managed_launch(program: &str, args: Vec<String>) -> serde_json::Value {
    json!({
        "request_id": common::uuid_v4(),
        "profile_id": common::uuid_v4(),
        "cwd": std::env::temp_dir().to_string_lossy(),
        "program": program,
        "argv": args,
        "env_overrides": {},
        "mode": "managed",
        "cols": 100,
        "rows": 30,
        "priority": 1,
        "policy": {
            "reservation_bytes": "2147483648",
            "cpu_slots": 1,
            "enforcement": "prefer",
            "memory_max_bytes": null,
            "cpu_max_cores": null,
            "pids_max": null,
        },
    })
}

#[test]
fn gate_observer_marker_appears_only_after_running() {
    let daemon = DaemonProc::spawn("gated", None);
    let fixture = common::fixture_bin();
    let marker = daemon.data_dir.join("gate-marker.txt");
    let marker_arg = marker.to_string_lossy().into_owned();

    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let launch = client
        .request(
            "workload.launch",
            managed_launch(
                &fixture,
                vec![
                    "gate-observer".to_string(),
                    "--marker".to_string(),
                    marker_arg,
                ],
            ),
        )
        .expect("managed launch");
    assert_eq!(launch["state"], "RUNNING", "got {launch}");
    assert!(launch["effective_policy"].is_object());
    assert_eq!(
        launch["missing_capabilities"].as_array().map(Vec::len),
        Some(0)
    );

    // The gate invariant: the target was created before we got here — the
    // marker proves the RELEASE actually happened; it must appear right
    // after the RUNNING response (helper spawns target synchronously).
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        marker.is_file(),
        "gate-observer marker must exist after RUNNING"
    );

    // workload.processes lists group identities (helper+target on Windows,
    // exec'd target on Unix) — argv/env excluded by shape.
    let processes = client
        .request(
            "workload.processes",
            json!({"workload_id": launch["workload_id"], "cursor": 0, "limit": 100}),
        )
        .expect("processes");
    let list = processes["processes"].as_array().expect("process array");
    assert!(!list.is_empty(), "running group has members");
    assert!(list[0]["identity"]["pid"].is_u64());
    assert!(list[0]["identity"]["start_token"].is_string());

    // Cancel: STOPPING now, CANCELLED shortly (job teardown).
    let cancel = client
        .request(
            "workload.cancel",
            json!({"request_id": common::uuid_v4(), "workload_id": launch["workload_id"]}),
        )
        .expect("cancel");
    assert_eq!(cancel["state"], "STOPPING");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut state = String::new();
    while Instant::now() < deadline {
        let snapshot = client
            .request("system.snapshot", json!({}))
            .expect("snapshot");
        let mine = snapshot["workloads"]
            .as_array()
            .expect("workloads")
            .iter()
            .find(|w| w["workload_id"] == launch["workload_id"])
            .expect("workload");
        state = mine["state"].as_str().unwrap_or_default().to_string();
        if state == "CANCELLED" || state == "FAILED" {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(state, "CANCELLED", "cancel must terminate the group");

    // Group empty: the processes page is empty after teardown.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut empty = false;
    while Instant::now() < deadline {
        let processes = client
            .request(
                "workload.processes",
                json!({"workload_id": launch["workload_id"], "cursor": 0, "limit": 100}),
            )
            .expect("processes");
        if processes["processes"].as_array().is_some_and(Vec::is_empty) {
            empty = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(empty, "group must be empty after terminal cancel");
}

#[test]
fn managed_require_with_unsupported_limit_is_rejected() {
    let daemon = DaemonProc::spawn("cap-require", None);
    let fixture = common::fixture_bin();

    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let mut req = managed_launch(&fixture, vec!["exit".into(), "--code".into(), "0".into()]);
    req["policy"]["enforcement"] = json!("require");
    req["policy"]["memory_max_bytes"] = json!("1073741824");

    // On Windows/Linux the job/cgroup memory limit is normally supported, so
    // the assertion is conditional on the platform capabilities. Anything
    // short of `supported` — unsupported OS, or a Linux session without a
    // delegated cgroup subtree (`permission_required`) — must refuse
    // `require` before anything is created (03 §5: require fails preflight;
    // only observe/prefer fall back to process-tree estimation).
    let hello_caps = client
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    let memory_support = hello_caps["capabilities"]["memory_limit_kind"]["support"]
        .as_str()
        .unwrap_or("unsupported");
    if memory_support != "supported" {
        let err = client
            .request("workload.launch", req)
            .expect_err("require must fail");
        assert_eq!(err["code"], "CAPABILITY_UNAVAILABLE");
        assert!(err["details"]["missing"].is_array());
    } else {
        let launch = client
            .request("workload.launch", req)
            .expect("require passes");
        assert_eq!(launch["state"], "RUNNING");
    }
}
