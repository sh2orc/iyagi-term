//! Real IPC/PTY coverage for independent pane resizes and journal ordering.
mod common;

use common::{Client, DaemonProc, Wire};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

fn launch(client: &mut Client) -> (String, String, String) {
    let launched = client
        .request(
            "workload.launch",
            common::launch_request("shell", &["echo"], "1"),
        )
        .unwrap();
    let session = launched["session_id"].as_str().unwrap().to_owned();
    let attached = client
        .request(
            "session.attach",
            json!({
                "session_id": session, "view_id": common::uuid_v4(), "access": "writer",
            }),
        )
        .unwrap();
    (
        launched["workload_id"].as_str().unwrap().to_owned(),
        session,
        attached["epoch"].as_str().unwrap().to_owned(),
    )
}

#[test]
fn six_panes_resize_without_accumulating_waits_and_ack_real_records() {
    let daemon = DaemonProc::spawn("resize-latency", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let data = Client::data(&daemon.endpoint, control.data_token.as_ref().unwrap());
    let panes: Vec<_> = (0..6).map(|_| launch(&mut control)).collect();
    let mut expected = HashMap::new();
    let mut previous = HashMap::<String, u64>::new();

    // Include a no-op after every resize. The old daemon waited 600 ms for
    // each no-op and stalled every pane behind it on the shared connection.
    for (round, cols) in [100, 100, 120, 120, 80, 80].into_iter().enumerate() {
        let start = Instant::now();
        let mut requests = HashMap::new();
        for (_, session, epoch) in &panes {
            let id = control.next_request_id();
            let resize_id = format!("{round}-{session}");
            control.fire(
                "session.resize",
                json!({
                    "session_id": session, "epoch": epoch,
                    "resize_id": resize_id, "cols": cols, "rows": 30,
                }),
            );
            requests.insert(id, (session.clone(), resize_id));
        }
        let mut timings = Vec::new();
        while !requests.is_empty() {
            let remaining = Duration::from_secs(1).saturating_sub(start.elapsed());
            assert!(
                !remaining.is_zero(),
                "panes waited behind another resize: {timings:?}"
            );
            let frame = control
                .recv_any(remaining)
                .expect("resize response within one second");
            let Some((session, resize_id)) =
                frame["id"].as_str().and_then(|id| requests.remove(id))
            else {
                continue;
            };
            assert!(frame.get("error").is_none(), "{frame}");
            assert_eq!(frame["result"]["resize_id"], resize_id);
            let seq = frame["result"]["applied_seq"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap();
            if round % 2 == 1 {
                assert_eq!(
                    previous[&session], seq,
                    "unchanged size reuses its real record"
                );
            } else {
                assert!(seq > previous.get(&session).copied().unwrap_or(0));
                expected.insert((session.clone(), seq), cols);
            }
            previous.insert(session, seq);
            timings.push(start.elapsed().as_micros());
        }
        eprintln!("six-pane resize {cols} (round {round}), response microseconds: {timings:?}");
    }

    // Every successful reply must identify an actual ordered resize frame,
    // not an output seq fabricated to escape a timeout.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = HashMap::<String, u64>::new();
    while !expected.is_empty() {
        let frame = data
            .recv_any(deadline.saturating_duration_since(Instant::now()))
            .expect("journal delivery");
        if frame["event"] != "session.output" {
            continue;
        }
        let payload = &frame["payload"];
        let session = payload["session_id"].as_str().unwrap().to_owned();
        let seq = payload["seq"].as_str().unwrap().parse::<u64>().unwrap();
        assert_eq!(
            seq,
            seen.get(&session).copied().unwrap_or(0) + 1,
            "journal order for {session}"
        );
        seen.insert(session.clone(), seq);
        if let Some(cols) = expected.remove(&(session, seq)) {
            assert_eq!(payload["kind"], "resize");
            assert_eq!(payload["cols"], cols);
            assert_eq!(payload["rows"], 30);
        }
    }
    for (workload, _, _) in panes {
        control
            .request(
                "workload.cancel",
                json!({"workload_id": workload, "force": true}),
            )
            .unwrap();
    }
}

fn response(wire: &Wire, id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = wire
            .recv_frame(deadline.saturating_duration_since(Instant::now()))
            .expect("response");
        if frame["id"] == id {
            return frame;
        }
    }
}

#[test]
fn resize_events_do_not_cancel_partially_read_requests() {
    let daemon = DaemonProc::spawn("resize-fragment", None);
    let (mut writer, _) = Client::control(&daemon.endpoint, &daemon.token);
    let (workload, session, epoch) = launch(&mut writer);
    let mut observer = Wire::connect(&daemon.endpoint);
    observer
        .send_frame(
            &json!({"v": 1, "id": "hello", "method": "hello", "params": {
                "client_id": common::uuid_v4(), "token": daemon.token, "role": "control",
            }}),
        )
        .unwrap();
    assert!(response(&observer, "hello").get("result").is_some());

    for (index, split) in [2, 6].into_iter().enumerate() {
        let id = format!("fragment-{index}");
        let bytes = term_contracts::rpc::encode_frame(&json!({
            "v": 1, "id": id, "method": "system.snapshot", "params": {},
        }))
        .unwrap();
        observer.send_raw(&bytes[..split]);
        writer
            .request(
                "session.resize",
                json!({
                    "session_id": session, "epoch": epoch, "resize_id": id,
                    "cols": 100 + index, "rows": 30,
                }),
            )
            .unwrap();
        // Receiving the event proves select handled an outbound frame
        // while the inbound frame was still incomplete.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let frame = observer
                .recv_frame(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if frame["event"] == "session.resize_applied" {
                break;
            }
        }
        observer.send_raw(&bytes[split..]);
        assert!(response(&observer, &id).get("result").is_some());
    }
    writer
        .request(
            "workload.cancel",
            json!({"workload_id": workload, "force": true}),
        )
        .unwrap();
}
