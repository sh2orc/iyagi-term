//! B22 (spec `06-verification.md` §3): every view detached while the
//! process runs (query-emitting TUI included).
//!
//! "화면 연결 해제 · 프로세스 실행 중": detaching ALL views (and dropping the
//! connections) never kills the process — the workload stays RUNNING past
//! the idle window and re-attaching replays the exact prior output
//! (byte-compare of the `session.output` record stream). The R1 limitation
//! (a TUI may stall waiting for query responses while fully detached — the
//! daemon answers no terminal queries) is exercised by the tui fixture,
//! which drains responses on its own side thread.

mod common;

use common::{launch_request, wait_workload_state, Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

/// Collect the (seq, kind, data_b64, raw_len) stream for one attached view.
fn collect_records(
    data: &mut Client,
    epoch: &str,
    session: &str,
    quiet_ms_target: u64,
    max_wait: Duration,
) -> Vec<(u64, String, String, u64)> {
    let mut records = Vec::new();
    let mut quiet = 0u64;
    let mut last_ack = Instant::now() - Duration::from_secs(1);
    let deadline = Instant::now() + max_wait;
    while Instant::now() < deadline {
        match data.recv_any(Duration::from_millis(50)) {
            Some(frame) => {
                if frame.get("event").and_then(|e| e.as_str()) == Some("session.output") {
                    let payload = &frame["payload"];
                    if payload["epoch"].as_str() == Some(epoch) {
                        records.push((
                            payload["seq"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string()
                                .parse()
                                .unwrap_or(0),
                            payload["kind"].as_str().unwrap_or_default().to_string(),
                            payload["data_b64"].as_str().unwrap_or_default().to_string(),
                            payload["raw_len"].as_u64().unwrap_or(0),
                        ));
                        quiet = 0;
                    }
                }
            }
            None => quiet += 50,
        }
        let due_ack = last_ack.elapsed() >= Duration::from_millis(150);
        let last = records.last().map(|(seq, _, _, _)| *seq).unwrap_or(0);
        if due_ack && last > 0 {
            data.send_ack(session, epoch, last);
            last_ack = Instant::now();
        }
        if quiet >= quiet_ms_target && !records.is_empty() {
            break;
        }
    }
    records
}

#[test]
fn b22_full_detach_keeps_process_alive_and_replays_exactly() {
    let mut daemon = DaemonProc::spawn("b22-detach", Some(common::relaxed_admission(json!({}))));
    let session;
    let workload_id;
    let first_stream;
    {
        let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
        // A fixture that emits one stderr line up front and then holds for
        // 10 s: output exists to replay, and the process outlives the test.
        let launch = control
            .request(
                "workload.launch",
                launch_request(
                    "managed",
                    &["memory", "--mib", "4", "--hold-ms", "10000"],
                    "1048576",
                ),
            )
            .expect("launch memory");
        common::ensure_running(&mut control, &launch, Duration::from_secs(15));
        session = launch["session_id"].as_str().expect("session").to_string();
        workload_id = launch["workload_id"].clone();

        // Attach a writer, send nothing, collect what the process emitted.
        let view_id = common::uuid_v4();
        let attach = control
            .request(
                "session.attach",
                json!({"session_id": session, "view_id": view_id, "access": "writer"}),
            )
            .expect("attach");
        let epoch = attach["epoch"].as_str().expect("epoch").to_string();
        let data_token = control.data_token.clone().expect("data token");
        let mut data = Client::data(&daemon.endpoint, &data_token);
        first_stream = collect_records(&mut data, &epoch, &session, 800, Duration::from_secs(8));
        assert!(
            !first_stream.is_empty(),
            "expected the fixture's start line"
        );

        // Detach ALL views; then drop the data + control connections.
        let detach = control
            .request(
                "session.detach",
                json!({"session_id": session, "view_id": view_id}),
            )
            .expect("detach");
        assert_eq!(detach["detached"], true);
    }

    // ≥3 s with zero views attached: the process must survive.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        daemon.child.try_wait().expect("try_wait").is_none(),
        "daemon+process must survive a full view detach"
    );

    // Re-attach from a fresh connection and replay.
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let still = common::snapshot_workload(&mut control, &workload_id).expect("workload");
    assert_eq!(
        still["state"], "RUNNING",
        "process alive after full detach: {still}"
    );
    assert_eq!(
        still["connection"], "detached",
        "snapshot must show the detached connection state: {still}"
    );

    let view_id = common::uuid_v4();
    let attach = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": view_id, "access": "reader"}),
        )
        .expect("re-attach");
    assert_eq!(attach["replay_from_seq"], "1", "R1 replays from the start");
    let epoch2 = attach["epoch"].as_str().expect("epoch").to_string();
    assert!(!epoch2.is_empty(), "re-attach issues a fresh epoch");
    let data_token = control.data_token.clone().expect("data token");
    let mut data = Client::data(&daemon.endpoint, &data_token);
    let replay = collect_records(&mut data, &epoch2, &session, 800, Duration::from_secs(8));

    // Byte-compare the replayed record stream against the first attach
    // (seq, kind, data, size — epochs legitimately differ).
    assert_eq!(
        replay.len(),
        first_stream.len(),
        "replay must reproduce the same records"
    );
    for (index, (a, b)) in first_stream.iter().zip(replay.iter()).enumerate() {
        assert_eq!(a, b, "record {index} differs after replay");
    }

    // Cleanup.
    let _ = control.request(
        "workload.cancel",
        json!({"request_id": common::uuid_v4(), "workload_id": workload_id}),
    );
    wait_workload_state(
        &mut control,
        &workload_id,
        &["CANCELLED", "SUCCEEDED"],
        Duration::from_secs(20),
    );
}

#[test]
fn b22_tui_queries_flow_and_session_completes_while_detached() {
    let daemon = DaemonProc::spawn("b22-tui", Some(common::relaxed_admission(json!({}))));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    // tui emits device-attribute / cursor-position / size queries
    // (ESC[c, ESC[6n, ESC[18t) while running; its drain thread counts the
    // responses it received from stdin.
    let launch = control
        .request(
            "workload.launch",
            launch_request("managed", &["tui", "--seed", "1"], "1048576"),
        )
        .expect("launch tui");
    common::ensure_running(&mut control, &launch, Duration::from_secs(15));
    let session = launch["session_id"].as_str().expect("session").to_string();
    let workload_id = launch["workload_id"].clone();

    // Attach, observe live output, then DETACH every view while queries are
    // flowing. B22 (06-verification §3): the process must SURVIVE detached —
    // the daemon answers no terminal queries itself (02-runner §5: the R1
    // limitation that some TUIs may stall with no live view is by design;
    // this fixture uses timeouts so it completes regardless).
    let view_id = common::uuid_v4();
    let attach = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": view_id, "access": "writer"}),
        )
        .expect("attach");
    let epoch = attach["epoch"].as_str().expect("epoch").to_string();
    let data_token = control.data_token.clone().expect("data token");
    let mut data = Client::data(&daemon.endpoint, &data_token);

    let mut all_bytes = Vec::new();
    let mut last = 0u64;
    let mut last_ack = Instant::now() - Duration::from_secs(1);
    let live_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < live_deadline {
        if let Some(frame) = data.recv_any(Duration::from_millis(50)) {
            if frame.get("event").and_then(|e| e.as_str()) == Some("session.output") {
                let payload = &frame["payload"];
                all_bytes.extend_from_slice(&common::unb64(
                    payload["data_b64"].as_str().unwrap_or_default(),
                ));
                if let Some(seq) = payload["seq"].as_str().and_then(|s| s.parse::<u64>().ok()) {
                    last = last.max(seq);
                }
                if last_ack.elapsed() >= Duration::from_millis(150) && last > 0 {
                    data.send_ack(&session, &epoch, last);
                    last_ack = Instant::now();
                }
            }
        }
    }
    assert!(!all_bytes.is_empty(), "live output must flow before detach");

    // Detach the only view; queries now go unanswered.
    let _ = control.request(
        "session.detach",
        json!({"session_id": session, "view_id": view_id}),
    );
    std::thread::sleep(Duration::from_secs(3));
    let during = common::snapshot_workload(&mut control, &workload_id).expect("workload");
    assert!(
        matches!(during["state"].as_str(), Some("RUNNING" | "SUCCEEDED")),
        "process must survive with all views detached: {during}"
    );
    let after = common::snapshot_workload(&mut control, &workload_id).expect("workload");
    assert_eq!(after["connection"], "detached", "state must show detached");

    // Re-attach from a FRESH control+data connection pair (data tokens are
    // single-use) and drain to completion; the replayed stream carries
    // everything from seq 1 (R1 full-journal replay).
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let view2 = common::uuid_v4();
    let reattach = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": view2, "access": "writer"}),
        )
        .expect("re-attach");
    assert_eq!(
        reattach["replay_from_seq"], "1",
        "R1 replays from the start"
    );
    let epoch2 = reattach["epoch"].as_str().expect("epoch").to_string();
    let data_token2 = control.data_token.clone().expect("data token");
    let mut data = Client::data(&daemon.endpoint, &data_token2);

    let mut all_bytes = Vec::new();
    let mut last = 0u64;
    let mut last_ack = Instant::now() - Duration::from_secs(1);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut quiet = 0u64;
    let mut done = false;
    while Instant::now() < deadline {
        match data.recv_any(Duration::from_millis(50)) {
            Some(frame) => {
                if frame.get("event").and_then(|e| e.as_str()) == Some("session.output") {
                    let payload = &frame["payload"];
                    all_bytes.extend_from_slice(&common::unb64(
                        payload["data_b64"].as_str().unwrap_or_default(),
                    ));
                    if let Some(seq) = payload["seq"].as_str().and_then(|s| s.parse::<u64>().ok()) {
                        last = last.max(seq);
                    }
                    quiet = 0;
                }
            }
            None => quiet += 50,
        }
        if last_ack.elapsed() >= Duration::from_millis(150) && last > 0 {
            data.send_ack(&session, &epoch2, last);
            last_ack = Instant::now();
        }
        if !done {
            let summary = common::snapshot_workload(&mut control, &workload_id).expect("workload");
            done = summary["state"] == "SUCCEEDED";
        }
        if done && quiet >= 800 {
            break;
        }
    }
    assert!(
        done,
        "tui workload must complete (exit 0) despite the unanswered-query window"
    );

    // Seeded frames and the alternate-screen enter/leave round-trip
    // (화면 복원 contract) are present in the replayed+live stream.
    assert!(all_bytes.windows(12).any(|w| w == b"seed=1 frame"));
    assert!(all_bytes.windows(8).any(|w| w == b"[?1049h"));
    assert!(all_bytes.windows(8).any(|w| w == b"[?1049l"));

    // The fixture's own completion line reports how many query responses it
    // drained. With no live xterm answering, this harness legitimately
    // observes 0 — the documented R1 limitation in the flesh (the daemon
    // never answers terminal queries; a real attached xterm does, and that
    // path is exercised by the UI pipeline tests).
    let stripped_text =
        String::from_utf8_lossy(&common::strip_terminal_controls(&all_bytes)).into_owned();
    let marker = "term-fixture tui seed=1 query_responses=";
    let pos = stripped_text
        .find(marker)
        .unwrap_or_else(|| panic!("tui completion marker missing from the stream"));
    let count: String = stripped_text[pos + marker.len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let count: u64 = count.parse().unwrap_or(0);
    eprintln!("b22: query_responses observed by the fixture = {count} (0 = documented R1 detached-query limitation)");

    // Cleanup.
    let _ = control.request(
        "workload.cancel",
        json!({"request_id": common::uuid_v4(), "workload_id": workload_id}),
    );
    wait_workload_state(
        &mut control,
        &workload_id,
        &["CANCELLED", "SUCCEEDED"],
        Duration::from_secs(20),
    );
}
