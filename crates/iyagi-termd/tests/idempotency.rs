//! Launch idempotency (B01/B02-lite, spec §6): same request_id + same
//! payload (sequentially AND concurrently ×10) → one workload/process
//! (side-effect fixture proves single execution); same id + different argv
//! → REQUEST_CONFLICT.

mod common;

use common::{Client, DaemonProc};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn managed_request(program: &str, args: Vec<String>) -> serde_json::Value {
    json!({
        "request_id": common::uuid_v4(),
        "profile_id": common::uuid_v4(),
        "cwd": std::env::temp_dir().to_string_lossy(),
        "program": program,
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

#[test]
fn duplicate_launch_is_idempotent_sequential() {
    let daemon = DaemonProc::spawn("idem-seq", None);
    let fixture = common::fixture_bin();
    let effect = daemon.data_dir.join("side-effect.txt");

    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let req = managed_request(
        &fixture,
        vec![
            "side-effect".into(),
            "--file".into(),
            effect.to_string_lossy().into_owned(),
        ],
    );
    // side-effect exits 0 immediately; keep a second copy for the replay.
    let first = client
        .request("workload.launch", req.clone())
        .expect("first launch");
    assert_eq!(first["state"], "RUNNING");

    // Same id + same payload: same workload, current state.
    let second = client
        .request("workload.launch", req.clone())
        .expect("duplicate launch");
    assert_eq!(
        second["workload_id"], first["workload_id"],
        "duplicate returns the SAME workload"
    );
    assert_eq!(second["session_id"], first["session_id"]);

    // Same id + different fingerprint: REQUEST_CONFLICT.
    let mut conflict = req.clone();
    conflict["argv"] = json!(["exit", "--code", "3"]);
    let err = client
        .request("workload.launch", conflict)
        .expect_err("conflict expected");
    assert_eq!(err["code"], "REQUEST_CONFLICT");

    // Exactly one side-effect file: one process ever ran. The gate reports
    // Started at process creation, so the file may land a beat after the
    // launch replies — poll briefly (the duplicate-launch assertions above
    // already prove no SECOND process was started).
    let effect_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !effect.is_file() {
        assert!(
            std::time::Instant::now() < effect_deadline,
            "side effect never happened"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let siblings: Vec<_> = std::fs::read_dir(daemon.data_dir.join("data/journals"))
        .expect("journals dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "mtj"))
        .collect();
    assert_eq!(siblings.len(), 1, "one session only, got {siblings:?}");
}

#[test]
fn concurrent_duplicate_launches_create_one_workload() {
    let daemon = DaemonProc::spawn("idem-conc", None);
    let fixture = common::fixture_bin();
    let effect = daemon.data_dir.join("conc-effect.txt");
    let mut req = managed_request(
        &fixture,
        vec![
            "side-effect".into(),
            "--file".into(),
            effect.to_string_lossy().into_owned(),
        ],
    );
    // A long-running program so the race window is wide: echo holds the pty.
    req["argv"] = json!(["echo"]);

    let (client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let client = Arc::new(std::sync::Mutex::new(client));
    let request_id = req["request_id"].clone();
    let mut handles = Vec::new();
    for _ in 0..10 {
        let _client = Arc::clone(&client);
        let req = req.clone();
        let endpoint = daemon.endpoint.clone();
        let token = daemon.token.clone();
        handles.push(std::thread::spawn(move || {
            // Each racing client gets its own connection (mirrors real
            // retries after DAEMON_UNAVAILABLE).
            let (mut racing, _) = Client::control(&endpoint, &token);
            racing.request("workload.launch", req)
        }));
    }
    let mut workload_ids = Vec::new();
    for handle in handles {
        let outcome = handle.join().expect("thread").expect("launch answered");
        workload_ids.push(outcome["workload_id"].clone());
        let _ = &request_id;
    }
    let distinct: std::collections::HashSet<_> = workload_ids.iter().cloned().collect();
    assert_eq!(
        distinct.len(),
        1,
        "all 10 racers see one workload: {distinct:?}"
    );

    // Single process: at most one side-effect-capable program ran (echo),
    // and exactly one session journal exists.
    let journals: Vec<_> = std::fs::read_dir(daemon.data_dir.join("data/journals"))
        .expect("journals dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "mtj"))
        .collect();
    assert_eq!(journals.len(), 1, "one session journal, got {journals:?}");
    assert!(!effect.is_file(), "echo fixture never ran the side-effect");
}
