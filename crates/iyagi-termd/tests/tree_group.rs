//! B08/B09 (spec `06-verification.md` §3): root process-group behavior.
//!
//! * B08 — the tree fixture's root exits 0 while owned descendants remain:
//!   the workload must not be misjudged as SUCCEEDED while the owned group
//!   still has members.
//! * B09 — cancelling one workload's group leaves an unrelated workload and
//!   its session (input → echo) untouched.
//!
//! KNOWN BUG (documented in done/round3-b-tests.md, `B08-WIN-CONPTY-DESC`):
//! on Windows the session actor's 2 s drain timeout closes the ConPTY
//! master; destroying the console KILLS the console-attached descendants
//! (spec 01 §5 requires them to survive until the user cancels), after
//! which the workload is marked SUCCEEDED at the root's exit code 0. The
//! fix belongs in `term-pty/src/actor.rs` (keep the session alive while
//! `owned_alive > 0`; surface root-exit while RUNNING), which is outside
//! this ticket's file scope — the full spec assertion is kept below under
//! `#[ignore]` with the bug id instead of being silently weakened.

mod common;

use common::{b64, launch_request, wait_workload_state, Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

/// Managed tree whose root leaves `children` sleepers behind after `early` ms.
fn tree_request(children: &str, depth: &str, hold_ms: &str, early_ms: &str) -> serde_json::Value {
    launch_request(
        "managed",
        &[
            "tree",
            "--children",
            children,
            "--depth",
            depth,
            "--hold-ms",
            hold_ms,
            "--root-early-exit-ms",
            early_ms,
        ],
        "1048576",
    )
}

fn processes_of(client: &mut Client, workload_id: &serde_json::Value) -> Vec<serde_json::Value> {
    let page = client
        .request(
            "workload.processes",
            json!({"workload_id": workload_id, "cursor": 0, "limit": 100}),
        )
        .expect("workload.processes");
    page["processes"].as_array().cloned().unwrap_or_default()
}

/// Poll until the owned group has at least `min` members (None on timeout).
#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS 게이트된 b08 두 건에서만 쓰인다
fn wait_members(
    client: &mut Client,
    workload_id: &serde_json::Value,
    min: usize,
    timeout: Duration,
) -> Option<usize> {
    let deadline = Instant::now() + timeout;
    loop {
        let count = processes_of(client, workload_id).len();
        if count >= min {
            return Some(count);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
// macOS: 고아 프로세스가 launchd로 재부모화되어 PPID 관측 트리가 뿌리
// 이탈을 놓친다 — 문서화된 관측 한계(term-platform macos_tree). 검증은
// Linux(cgroup)/Windows(Job)에서 유효하므로 여기선 건너뛴다.
#[cfg(not(target_os = "macos"))]
fn b08_root_exit_is_not_misjudged_while_the_group_has_members() {
    let daemon = DaemonProc::spawn("b08-tree", Some(common::relaxed_admission(json!({}))));
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = client
        .request("workload.launch", tree_request("3", "1", "20000", "800"))
        .expect("launch tree");
    let workload_id = launch["workload_id"].clone();
    common::ensure_running(&mut client, &launch, Duration::from_secs(15));

    // Wait until the root has exited and only the 3 sleepers remain in the
    // owned group (helper + root leave the group when they exit).
    let members = wait_members(&mut client, &workload_id, 3, Duration::from_secs(10))
        .expect("3 owned descendants become group members");

    // Spec B08 invariant that must hold TODAY: no SUCCEEDED while the owned
    // group still reports members. Watch the group until it empties.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_running_with_members = false;
    loop {
        let summary = common::snapshot_workload(&mut client, &workload_id)
            .expect("tree workload in snapshot");
        let state = summary["state"].as_str().unwrap_or_default().to_string();
        let member_count = processes_of(&mut client, &workload_id).len();
        assert!(
            !(state == "SUCCEEDED" && member_count > 0),
            "SUCCEEDED reported while {member_count} owned members remain: {summary}"
        );
        if member_count > 0 && state == "RUNNING" {
            saw_running_with_members = true;
        }
        if member_count == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "group never emptied: {summary}");
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(
        saw_running_with_members && members >= 3,
        "the workload must stay RUNNING while the root's descendants live"
    );

    // Without any cancel the workload settles into a terminal state
    // (see the bug note above for what that state currently is).
    let terminal = wait_workload_state(
        &mut client,
        &workload_id,
        &["SUCCEEDED", "FAILED", "CANCELLED"],
        Duration::from_secs(15),
    );
    eprintln!("b08 terminal state: {}", terminal["state"]);
}

/// The full spec assertion for B08: root exit 0 with live descendants keeps
/// the workload RUNNING with `root_exited=true` until the user cancels, and
/// only `workload.cancel` clears the group (→ CANCELLED).
#[test]
#[cfg(not(target_os = "macos"))]
fn b08_descendants_survive_until_cancel() {
    let daemon = DaemonProc::spawn("b08-full", Some(common::relaxed_admission(json!({}))));
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = client
        .request("workload.launch", tree_request("3", "1", "20000", "800"))
        .expect("launch tree");
    let workload_id = launch["workload_id"].clone();
    common::ensure_running(&mut client, &launch, Duration::from_secs(15));

    // RUNNING + root_exited=true, never SUCCEEDED (spec 01 §5).
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let summary = common::snapshot_workload(&mut client, &workload_id)
            .expect("tree workload in snapshot");
        assert_ne!(
            summary["state"], "SUCCEEDED",
            "root exit 0 with live descendants must not be success: {summary}"
        );
        if summary["state"] == "RUNNING" && summary["root_exited"] == true {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "never saw RUNNING+root_exited: {summary}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Descendants survive ≥3 s past the root exit.
    std::thread::sleep(Duration::from_secs(3));
    let members = processes_of(&mut client, &workload_id);
    assert!(
        members.len() >= 3,
        "owned descendants must survive until cancel: {members:?}"
    );

    // Cancel → STOPPING → CANCELLED once the owned group dies.
    let cancel = client
        .request(
            "workload.cancel",
            json!({"request_id": common::uuid_v4(), "workload_id": workload_id}),
        )
        .expect("cancel");
    assert_eq!(cancel["state"], "STOPPING", "got {cancel}");
    let terminal = wait_workload_state(
        &mut client,
        &workload_id,
        &["CANCELLED"],
        Duration::from_secs(25),
    );
    assert_eq!(terminal["state"], "CANCELLED", "got {terminal}");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if processes_of(&mut client, &workload_id).is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "group must be empty after CANCELLED"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn b09_cancel_kills_only_the_owned_group() {
    let daemon = DaemonProc::spawn("b09-isolation", Some(common::relaxed_admission(json!({}))));
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);

    // Victim: tree with lingering descendants. Bystander: managed echo.
    let victim = client
        .request("workload.launch", tree_request("2", "1", "30000", "500"))
        .expect("launch tree");
    let bystander = client
        .request(
            "workload.launch",
            launch_request("managed", &["echo"], "1048576"),
        )
        .expect("launch echo");
    common::ensure_running(&mut client, &victim, Duration::from_secs(15));
    common::ensure_running(&mut client, &bystander, Duration::from_secs(15));

    let bystander_session = bystander["session_id"]
        .as_str()
        .expect("session")
        .to_string();

    // Attach a writer to the bystander + open its data connection.
    let view_id = common::uuid_v4();
    let attach = client
        .request(
            "session.attach",
            json!({"session_id": bystander_session, "view_id": view_id, "access": "writer"}),
        )
        .expect("attach bystander");
    let epoch = attach["epoch"].as_str().expect("epoch").to_string();
    let data_token = client.data_token.clone().expect("data token");
    let mut data = Client::data(&daemon.endpoint, &data_token);

    fn echo_once(
        client: &mut Client,
        data: &mut Client,
        session: &str,
        epoch: &str,
        payload: &str,
    ) -> bool {
        let input = client
            .request(
                "session.input",
                json!({
                    "session_id": session,
                    "epoch": epoch,
                    "input_id": common::uuid_v4(),
                    "data_b64": b64(payload.as_bytes()),
                }),
            )
            .expect("input accepted");
        assert_eq!(input["accepted_bytes"], payload.len() as u64);
        let needle = payload.trim_end().as_bytes().to_vec();
        // Wait for the echo among session.output frames.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(frame) = data.recv_any(Duration::from_millis(300)) {
                if frame.get("event").and_then(|e| e.as_str()) == Some("session.output") {
                    if let Some(text) = frame["payload"]["data_b64"].as_str() {
                        let bytes = common::unb64(text);
                        if bytes.windows(needle.len()).any(|w| w == needle) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    // The bystander session echoes before the cancel.
    assert!(
        echo_once(
            &mut client,
            &mut data,
            &bystander_session,
            &epoch,
            "still-alive\n"
        ),
        "bystander must echo before the cancel"
    );

    // Cancel the victim only.
    let cancel = client
        .request(
            "workload.cancel",
            json!({"request_id": common::uuid_v4(), "workload_id": victim["workload_id"]}),
        )
        .expect("cancel victim");
    assert_eq!(cancel["state"], "STOPPING");
    let victim_terminal = wait_workload_state(
        &mut client,
        &victim["workload_id"],
        &["CANCELLED"],
        Duration::from_secs(25),
    );
    assert_eq!(victim_terminal["state"], "CANCELLED");

    // The bystander is untouched: still RUNNING and its session still echoes.
    let bystander_now =
        common::snapshot_workload(&mut client, &bystander["workload_id"]).expect("bystander");
    assert_eq!(bystander_now["state"], "RUNNING", "got {bystander_now}");
    assert!(
        echo_once(
            &mut client,
            &mut data,
            &bystander_session,
            &epoch,
            "after-cancel\n"
        ),
        "bystander session must keep echoing after the victim's cancel"
    );

    // Victim group empty; no unrelated processes were touched.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if processes_of(&mut client, &victim["workload_id"]).is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "victim group must be empty");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Cleanup.
    let _ = client.request(
        "workload.cancel",
        json!({"request_id": common::uuid_v4(), "workload_id": bystander["workload_id"]}),
    );
    wait_workload_state(
        &mut client,
        &bystander["workload_id"],
        &["CANCELLED"],
        Duration::from_secs(20),
    );
}

#[test]
fn force_cancel_skips_grace_and_preserves_other_workloads() {
    let daemon = DaemonProc::spawn(
        "force-cancel",
        Some(common::relaxed_admission(json!({
            "timing_ms": { "stop_grace": 30000 }
        }))),
    );
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let victim = client
        .request("workload.launch", tree_request("2", "1", "60000", "60000"))
        .expect("launch victim");
    let bystander = client
        .request("workload.launch", tree_request("0", "1", "60000", "60000"))
        .expect("launch bystander");
    common::ensure_running(&mut client, &victim, Duration::from_secs(15));
    common::ensure_running(&mut client, &bystander, Duration::from_secs(15));
    wait_members(
        &mut client,
        &victim["workload_id"],
        3,
        Duration::from_secs(10),
    )
    .expect("victim descendants are running");
    client
        .request(
            "workload.cancel",
            json!({
                "workload_id": victim["workload_id"], "force": true
            }),
        )
        .expect("force cancel");
    let terminal = wait_workload_state(
        &mut client,
        &victim["workload_id"],
        &["CANCELLED"],
        Duration::from_secs(8),
    );
    assert_eq!(terminal["state"], "CANCELLED");
    assert!(processes_of(&mut client, &victim["workload_id"]).is_empty());
    assert_eq!(
        common::snapshot_workload(&mut client, &bystander["workload_id"]).unwrap()["state"],
        "RUNNING"
    );
    client
        .request(
            "workload.cancel",
            json!({
                "workload_id": bystander["workload_id"], "force": true
            }),
        )
        .expect("clean up bystander");
}
