//! B05-class transport violations: bad token, oversized frame, invalid
//! JSON, slow hello — each closes ONLY that connection; the daemon survives
//! and serves the next valid client (spec `01-contracts.md` §3).

mod common;

use common::{uuid_v4, Client, DaemonProc, Wire};
use serde_json::json;
use std::time::Duration;

#[test]
fn bad_token_is_rejected_but_daemon_survives() {
    let daemon = DaemonProc::spawn("bad-token", None);

    let mut wire = Wire::connect(&daemon.endpoint);
    wire.send_frame(&json!({
        "v": 1,
        "id": "hello",
        "method": "hello",
        "params": {"client_id": uuid_v4(), "token": "definitely-not-the-token", "role": "control"},
    }))
    .expect("write bad-token hello");
    let frame = wire
        .recv_frame(Duration::from_secs(3))
        .expect("error response for bad token");
    let code = frame["error"]["code"].as_str().unwrap_or_default();
    assert_eq!(code, "INVALID_ARGUMENT", "got {frame}");
    assert!(
        wire.recv_frame(Duration::from_secs(3)).is_none(),
        "connection must close"
    );

    // Daemon alive: a valid hello + snapshot works right after.
    let (mut client, hello) = Client::control(&daemon.endpoint, &daemon.token);
    assert!(hello["capabilities"]["platform"].is_string());
    let snapshot = client
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    assert!(snapshot["revision"].as_u64().unwrap_or(0) >= 1);
}

#[test]
fn oversize_frame_closes_only_that_connection() {
    let daemon = DaemonProc::spawn("oversize", None);
    let endpoint = daemon.endpoint.clone();

    // Header claiming 70_000 bytes + a little junk.
    let mut wire = Wire::connect(&endpoint);
    let mut poison = 70_000u32.to_le_bytes().to_vec();
    poison.extend_from_slice(&[0u8; 16]);
    wire.send_raw(&poison);
    assert!(
        wire.recv_frame(Duration::from_secs(3)).is_none(),
        "connection must be closed without a reply"
    );

    let (mut client, _) = Client::control(&endpoint, &daemon.token);
    assert!(client.request("system.snapshot", json!({})).is_ok());
}

#[test]
fn invalid_json_closes_only_that_connection() {
    let daemon = DaemonProc::spawn("bad-json", None);
    let endpoint = daemon.endpoint.clone();

    let mut wire = Wire::connect(&endpoint);
    let body = b"{not valid json";
    let mut frame = (body.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(body);
    wire.send_raw(&frame);
    assert!(wire.recv_frame(Duration::from_secs(3)).is_none());

    // Also invalid UTF-8 inside a well-formed frame.
    let mut wire2 = Wire::connect(&endpoint);
    let junk = vec![0xFFu8, 0xFE, 0x00, 0x11];
    let mut frame2 = (junk.len() as u32).to_le_bytes().to_vec();
    frame2.extend_from_slice(&junk);
    wire2.send_raw(&frame2);
    assert!(wire2.recv_frame(Duration::from_secs(3)).is_none());

    let (mut client, _) = Client::control(&endpoint, &daemon.token);
    assert!(client.request("system.snapshot", json!({})).is_ok());
}

#[test]
fn slow_hello_times_out_and_closes() {
    let daemon = DaemonProc::spawn("slow-hello", None);
    let endpoint = daemon.endpoint.clone();

    // Connect, say nothing for 3s (> 2s hello window).
    let mut wire = Wire::connect(&endpoint);
    std::thread::sleep(Duration::from_millis(3_100));
    // The server should have closed us; a later write either fails or the
    // read side yields EOF.
    let body = br#"{"v":1,"id":"late","method":"system.snapshot","params":{}}"#;
    let mut frame = (body.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(body);
    let _ = wire.try_send_raw(&frame);
    assert!(
        wire.recv_frame(Duration::from_secs(3)).is_none(),
        "late hello must be ignored after the handshake timeout"
    );

    let (mut client, _) = Client::control(&endpoint, &daemon.token);
    assert!(client.request("system.snapshot", json!({})).is_ok());
}

#[test]
fn protocol_mismatch_gets_error_and_no_methods() {
    let daemon = DaemonProc::spawn("proto-mismatch", None);
    let endpoint = daemon.endpoint.clone();

    // (retry loop: the first pipe instance can be mid-disconnect on Windows)
    let mut frame = None;
    for _ in 0..3 {
        let mut wire = Wire::connect(&endpoint);
        if wire
            .send_frame(&json!({
                "v": 99,
                "id": "hello",
                "method": "hello",
                "params": {"client_id": uuid_v4(), "token": daemon.token, "role": "control"},
            }))
            .is_err()
        {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if let Some(response) = wire.recv_frame(Duration::from_secs(3)) {
            // No further methods on mismatch: connection closes.
            assert!(
                wire.recv_frame(Duration::from_secs(3)).is_none(),
                "connection must close after PROTOCOL_MISMATCH"
            );
            frame = Some(response);
            break;
        }
    }
    let frame = frame.expect("mismatch response");
    assert_eq!(frame["error"]["code"], "PROTOCOL_MISMATCH");
}
