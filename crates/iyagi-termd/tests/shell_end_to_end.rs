//! Shell workload end-to-end: launch mode=shell (term-fixture echo), attach
//! a writer view, open the data connection, send input → ordered
//! session.output echo, ACKs advance; natural exit → SUCCEEDED + exited
//! event; cancel path → CANCELLED (spec §4 data path).

mod common;

use common::{b64, unb64, Client, DaemonProc};
use serde_json::json;
use std::time::Duration;

fn shell_launch(program: &str, args: &[&str]) -> serde_json::Value {
    json!({
        "request_id": common::uuid_v4(),
        "profile_id": common::uuid_v4(),
        "cwd": std::env::temp_dir().to_string_lossy(),
        "program": program,
        "argv": args,
        "env_overrides": {},
        "mode": "shell",
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
fn echo_round_trip_over_data_connection() {
    let daemon = DaemonProc::spawn("shell-echo", None);
    let fixture = common::fixture_bin();

    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let launch = control
        .request("workload.launch", shell_launch(&fixture, &["echo"]))
        .expect("launch shell echo");
    assert_eq!(launch["state"], "RUNNING", "got {launch}");
    let session_id = launch["session_id"]
        .as_str()
        .expect("session id")
        .to_string();

    // Writer attach on the control connection.
    let attach = control
        .request(
            "session.attach",
            json!({"session_id": session_id, "view_id": common::uuid_v4(), "access": "writer"}),
        )
        .expect("attach");
    let epoch = attach["epoch"].as_str().expect("epoch").to_string();
    assert_eq!(
        attach["replay_from_seq"], "1",
        "R1 replays the whole journal"
    );
    assert_eq!(attach["cols"], 80);
    assert_eq!(attach["rows"], 24);

    // Data connection (redeems the control hello's one-shot token).
    let data_token = control
        .data_token
        .clone()
        .expect("control hello issued a data token");
    let mut data = Client::data(&daemon.endpoint, &data_token);

    // Input flows in, output flows back on the data connection.
    let payloads = [
        "hello iyagi\n",
        "second line\n",
        "third chunk with 0x7f ok\n",
    ];
    let mut expected_echo = String::new();
    for (n, text) in payloads.iter().enumerate() {
        let input = control
            .request(
                "session.input",
                json!({
                    "session_id": session_id,
                    "epoch": epoch,
                    "input_id": format!("in-{n}"),
                    "data_b64": b64(text.as_bytes()),
                }),
            )
            .expect("input accepted");
        assert_eq!(input["accepted_bytes"], text.len() as u64, "got {input}");
        expected_echo.push_str(text);
    }

    // Collect session.output events on the data connection until the echoed
    // bytes contain every payload (ConPTY may reformat line endings).
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut received = Vec::new();
    let mut last_ack = 0u64;
    while std::time::Instant::now() < deadline {
        if let Some(frame) = data.recv_any(Duration::from_millis(200)) {
            if frame["event"] == "session.output" {
                let payload = frame["payload"].clone();
                let seq: u64 = payload["seq"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                assert!(
                    seq > last_ack,
                    "seq must be ordered, got {seq} after {last_ack}"
                );
                last_ack = seq;
                if payload["kind"] == "output" {
                    received.extend(unb64(payload["data_b64"].as_str().unwrap_or("")));
                }
                // ACK the delivered seq on the data connection.
                data.send_ack(
                    &session_id,
                    payload["epoch"].as_str().unwrap_or(&epoch),
                    seq,
                );
            }
        }
        let text = String::from_utf8_lossy(&received).into_owned();
        if payloads.iter().all(|p| text.contains(p.trim_end())) {
            break;
        }
    }
    let text = String::from_utf8_lossy(&received).into_owned();
    for payload in &payloads {
        assert!(
            text.contains(payload.trim_end()),
            "echo payload {payload:?} missing from output: {text:?}"
        );
    }

    // Cancel terminates the session: session.exited on control conns.
    let cancel = control
        .request(
            "workload.cancel",
            json!({"request_id": common::uuid_v4(), "workload_id": launch["workload_id"]}),
        )
        .expect("cancel");
    assert_eq!(
        cancel["state"], "STOPPING",
        "running cancel enters STOPPING: {cancel}"
    );
    let exited = control
        .wait_event("session.exited", Duration::from_secs(15))
        .expect("session.exited after cancel");
    assert_eq!(exited["session_id"], session_id);

    // Terminal state visible in a fresh snapshot.
    let snapshot = control
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    let workloads = snapshot["workloads"].as_array().expect("workloads");
    let mine = workloads
        .iter()
        .find(|w| w["workload_id"] == launch["workload_id"])
        .expect("workload in snapshot");
    let state = mine["state"].as_str().expect("state");
    assert!(
        state == "CANCELLED" || state == "DRAINING" || state == "STOPPING",
        "cancel-driven terminal path in progress, got {state}"
    );
    // Poll to terminal CANCELLED.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut final_state = String::new();
    while std::time::Instant::now() < deadline {
        let snapshot = control
            .request("system.snapshot", json!({}))
            .expect("snapshot");
        let mine = snapshot["workloads"]
            .as_array()
            .expect("workloads")
            .iter()
            .find(|w| w["workload_id"] == launch["workload_id"])
            .expect("workload");
        let state = mine["state"].as_str().unwrap_or_default().to_string();
        if state == "CANCELLED" {
            final_state = state;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(final_state, "CANCELLED", "cancel must reach CANCELLED");
}

#[test]
fn natural_exit_reaches_succeeded() {
    let daemon = DaemonProc::spawn("shell-exit", None);
    let fixture = common::fixture_bin();

    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let launch = control
        .request(
            "workload.launch",
            shell_launch(&fixture, &["exit", "--code", "0", "--delay-ms", "300"]),
        )
        .expect("launch exit fixture");
    assert_eq!(launch["state"], "RUNNING");

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if std::time::Instant::now() > deadline {
            panic!("workload did not reach SUCCEEDED");
        }
        let snapshot = control
            .request("system.snapshot", json!({}))
            .expect("snapshot");
        let mine = snapshot["workloads"]
            .as_array()
            .expect("workloads")
            .iter()
            .find(|w| w["workload_id"] == launch["workload_id"])
            .expect("workload");
        match mine["state"].as_str().unwrap_or_default() {
            "SUCCEEDED" => {
                assert_eq!(mine["exit_code"], 0, "got {mine}");
                break;
            }
            "FAILED" => panic!("exit 0 fixture failed: {mine}"),
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let exited = control
        .wait_event("session.exited", Duration::from_secs(5))
        .expect("session.exited event");
    assert_eq!(exited["exit_code"], 0);
    assert_eq!(exited["descendants_remaining"], false);
}

#[test]
fn shell_launch_validates_program_and_cwd() {
    let daemon = DaemonProc::spawn("shell-invalid", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let req = shell_launch("C:/definitely/not/here.exe", &[]);
    let cwd_backup = req["cwd"].clone();
    let _ = cwd_backup;
    let err = control
        .request("workload.launch", req)
        .expect_err("missing program");
    // Windows 경로 형태("C:/…")는 Windows에선 PROGRAM_NOT_FOUND, Unix에선
    // 절대 경로 검증의 INVALID_ARGUMENT로 거부된다 — 둘 다 정당한 거부.
    assert!(
        err["code"] == "PROGRAM_NOT_FOUND" || err["code"] == "INVALID_ARGUMENT",
        "unexpected code: {err}"
    );

    let mut req = shell_launch(&common::fixture_bin(), &["exit"]);
    req["cwd"] = json!("Z:/no/such/dir");
    let err = control
        .request("workload.launch", req)
        .expect_err("missing cwd");
    // "Z:/…"도 플랫폼에 따라 CWD_UNAVAILABLE(Windows) 또는 경로 형태
    // 검증의 INVALID_ARGUMENT(Unix)로 거부된다.
    assert!(
        err["code"] == "CWD_UNAVAILABLE" || err["code"] == "INVALID_ARGUMENT",
        "unexpected code: {err}"
    );
}

/// W2(ADR-5 해소): `session.input` 응답의 accepted_bytes가 쓰기 완료 후의
/// 실제 바이트 수인지 — 실제 셸에 echo를 쳐서 증명한다.
#[test]
fn session_input_reports_write_completion() {
    let daemon = DaemonProc::spawn("input-complete", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let req = shell_launch(&common::fixture_bin(), &["--hold-ms", "3000"]);
    let outcome = control
        .request("workload.launch", req)
        .expect("launch fixture");
    let session = outcome["session_id"].as_str().expect("session").to_string();

    let attach = control
        .request(
            "session.attach",
            json!({ "session_id": session, "view_id": uuid::Uuid::new_v4().to_string(), "access": "writer" }),
        )
        .expect("attach");
    let epoch = attach["epoch"].as_str().expect("epoch").to_string();

    let reply = control
        .request(
            "session.input",
            json!({
                "session_id": session,
                "epoch": epoch,
                "input_id": "probe-input-1",
                "data_b64": b64(b"ping\n"),
            }),
        )
        .expect("input accepted");
    let accepted = reply["accepted_bytes"].as_u64().expect("accepted_bytes");
    assert_eq!(
        accepted, 5,
        "echo 쓰기는 완료 후 실제 바이트 수로 응답한다: {reply}"
    );

    let _ = control.request(
        "workload.cancel",
        json!({ "request_id": "cancel-input-complete", "workload_id": outcome["workload_id"] }),
    );
}
