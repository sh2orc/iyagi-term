//! Queue behavior with managed_concurrency=1 (injected via IYAGI_TEST_CONFIG):
//! two managed launches → one RUNNING, one QUEUED with WAIT_CONCURRENCY +
//! `queue.changed` events; releasing the first lets the second start.

mod common;

use common::{Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

fn managed_request(args: Vec<String>) -> serde_json::Value {
    json!({
        "request_id": common::uuid_v4(),
        "profile_id": common::uuid_v4(),
        "cwd": std::env::temp_dir().to_string_lossy(),
        "program": common::fixture_bin(),
        "argv": args,
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

fn wait_for_state(
    client: &mut Client,
    workload_id: &serde_json::Value,
    want: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let snapshot = client
            .request("system.snapshot", json!({}))
            .expect("snapshot");
        if let Some(mine) = snapshot["workloads"]
            .as_array()
            .expect("workloads")
            .iter()
            .find(|w| w["workload_id"] == *workload_id)
            .cloned()
        {
            if mine["state"] == want {
                return mine;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("workload {workload_id} never reached {want}");
}

#[test]
fn concurrency_one_queues_the_second_launch() {
    let daemon = DaemonProc::spawn("queue", Some(json!({"limits": {"managed_concurrency": 1}})));

    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);

    // First: long-running echo → RUNNING and holds the single slot.
    let first = client
        .request("workload.launch", managed_request(vec!["echo".into()]))
        .expect("first launch");
    assert_eq!(first["state"], "RUNNING", "got {first}");

    // Second: immediately QUEUED with WAIT_CONCURRENCY.
    let second = client
        .request("workload.launch", managed_request(vec!["echo".into()]))
        .expect("second launch");
    assert_eq!(second["state"], "QUEUED", "got {second}");
    assert_ne!(second["workload_id"], first["workload_id"]);

    // The scheduler's first pass (250 ms tick) stamps WAIT_CONCURRENCY on
    // the queued workload; poll the snapshot until it shows up.
    let mine = {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = client
                .request("system.snapshot", json!({}))
                .expect("snapshot");
            let mine = snapshot["workloads"]
                .as_array()
                .expect("workloads")
                .iter()
                .find(|w| w["workload_id"] == second["workload_id"])
                .expect("queued workload in snapshot")
                .clone();
            if mine["queue_reason"] == "WAIT_CONCURRENCY" || std::time::Instant::now() >= deadline {
                break mine;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    assert_eq!(mine["state"], "QUEUED");
    assert_eq!(mine["queue_reason"], "WAIT_CONCURRENCY");

    // Reprioritize the queued workload (0 = highest), then release the slot
    // by cancelling the first: the scheduler admits the second.
    let reprio = client
        .request(
            "workload.reprioritize",
            json!({"workload_id": second["workload_id"], "priority": 0}),
        )
        .expect("reprioritize");
    assert!(reprio["queue"].as_array().is_some());

    let cancel = client
        .request(
            "workload.cancel",
            json!({"request_id": common::uuid_v4(), "workload_id": first["workload_id"]}),
        )
        .expect("cancel first");
    assert_eq!(cancel["state"], "STOPPING");

    wait_for_state(&mut client, &first["workload_id"], "CANCELLED");
    // The second starts once the reservation is released.
    wait_for_state(&mut client, &second["workload_id"], "RUNNING");

    // Running workloads cannot be reprioritized (INVALID_STATE).
    let err = client
        .request(
            "workload.reprioritize",
            json!({"workload_id": second["workload_id"], "priority": 2}),
        )
        .expect_err("running reprioritize");
    assert_eq!(err["code"], "INVALID_STATE");

    // update_policy is QUEUED-only as well.
    let err = client
        .request(
            "workload.update_policy",
            json!({"workload_id": second["workload_id"], "policy": {
                "reservation_bytes": "1073741824", "cpu_slots": 1, "enforcement": "observe",
                "memory_max_bytes": null, "cpu_max_cores": null, "pids_max": null}}),
        )
        .expect_err("running policy update");
    assert_eq!(err["code"], "INVALID_STATE");

    // Cleanup: cancel the second too.
    let _ = client.request(
        "workload.cancel",
        json!({"request_id": common::uuid_v4(), "workload_id": second["workload_id"]}),
    );
    wait_for_state(&mut client, &second["workload_id"], "CANCELLED");
}

#[test]
fn queue_wait_reasons_surface_in_events() {
    let daemon = DaemonProc::spawn(
        "queue-reasons",
        Some(json!({"limits": {"managed_concurrency": 1}})),
    );
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let first = client
        .request("workload.launch", managed_request(vec!["echo".into()]))
        .expect("first");
    assert_eq!(first["state"], "RUNNING");
    let second = client
        .request("workload.launch", managed_request(vec!["echo".into()]))
        .expect("second");
    assert_eq!(second["state"], "QUEUED");

    // The scheduler re-evaluates every 250 ms; a queue.changed carrying the
    // reason must have arrived within a few seconds.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_reason = false;
    while Instant::now() < deadline {
        // recv_any stashes incoming events for pop_event.
        let _ = client.recv_any(Duration::from_millis(300));
        if let Some(event) = client.pop_event("queue.changed") {
            if let Some(queue) = event["queue"].as_array() {
                if queue.iter().any(|e| e["wait_reason"] == "WAIT_CONCURRENCY") {
                    saw_reason = true;
                }
            }
        }
        if saw_reason {
            break;
        }
    }
    assert!(saw_reason, "scheduler must publish WAIT_CONCURRENCY");
}

/// 대기 중인 실행의 정책 수정도 실행과 같은 규칙이다: 이 호스트가 받을 수 없는 크기는
/// RESOURCE_UNSCHEDULABLE로 거절하지 않고 맞춰 받으며, 응답은 유효 정책이다.
#[test]
fn queued_policy_update_is_fitted_like_a_launch() {
    let daemon = DaemonProc::spawn(
        "queue-fit",
        Some(json!({"limits": {"managed_concurrency": 1}})),
    );
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let first = client
        .request("workload.launch", managed_request(vec!["echo".into()]))
        .expect("first launch");
    assert_eq!(first["state"], "RUNNING", "got {first}");
    let second = client
        .request("workload.launch", managed_request(vec!["echo".into()]))
        .expect("second launch");
    assert_eq!(second["state"], "QUEUED", "got {second}");

    let updated = client
        .request(
            "workload.update_policy",
            json!({"workload_id": second["workload_id"], "policy": {
                "reservation_bytes": "1125899906842624", "cpu_slots": 1024, "enforcement": "observe",
                "memory_max_bytes": null, "cpu_max_cores": null, "pids_max": null}}),
        )
        .expect("fitted instead of RESOURCE_UNSCHEDULABLE");
    let reserved: u64 = updated["policy"]["reservation_bytes"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(reserved > 0 && reserved < 1125899906842624, "{updated}");
    let slots = updated["policy"]["cpu_slots"].as_u64().unwrap();
    assert!((1..1024).contains(&slots), "{updated}");

    for workload in [&second, &first] {
        let _ = client.request(
            "workload.cancel",
            json!({"request_id": common::uuid_v4(), "workload_id": workload["workload_id"], "force": true}),
        );
    }
}
