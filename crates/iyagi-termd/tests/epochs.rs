//! Epoch and ownership discipline (spec §4/§6):
//! * input with an old epoch → STALE_EPOCH;
//! * resize from a reader view → NOT_INPUT_OWNER;
//! * take_control CAS swaps the writer, old writer demoted,
//!   `session.owner_changed` emitted;
//! * a second writer attach demotes the previous writer.

mod common;

use common::{b64, Client, DaemonProc};
use serde_json::json;
use std::time::Duration;

fn shell_launch_echo() -> serde_json::Value {
    json!({
        "request_id": common::uuid_v4(),
        "profile_id": common::uuid_v4(),
        "cwd": std::env::temp_dir().to_string_lossy(),
        "program": common::fixture_bin(),
        "argv": ["echo"],
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
fn epoch_and_owner_rules() {
    let daemon = DaemonProc::spawn("epochs", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = client
        .request("workload.launch", shell_launch_echo())
        .expect("launch");
    let session_id = launch["session_id"].as_str().expect("session").to_string();

    // Writer + reader views on the same control connection.
    let writer_view = common::uuid_v4();
    let reader_view = common::uuid_v4();
    let writer_attach = client
        .request(
            "session.attach",
            json!({"session_id": session_id, "view_id": writer_view, "access": "writer"}),
        )
        .expect("writer attach");
    let writer_epoch = writer_attach["epoch"].as_str().expect("epoch").to_string();

    let reader_attach = client
        .request(
            "session.attach",
            json!({"session_id": session_id, "view_id": reader_view, "access": "reader"}),
        )
        .expect("reader attach");
    let reader_epoch = reader_attach["epoch"].as_str().expect("epoch").to_string();

    // Old-epoch input (pre-attach epoch string) → STALE_EPOCH.
    let stale_epoch = common::uuid_v4(); // never issued
    let err = client
        .request(
            "session.input",
            json!({
                "session_id": session_id,
                "epoch": stale_epoch,
                "input_id": "stale-1",
                "data_b64": b64(b"x"),
            }),
        )
        .expect_err("stale epoch input");
    assert_eq!(err["code"], "STALE_EPOCH", "got {err}");

    // Reader resize → NOT_INPUT_OWNER.
    let err = client
        .request(
            "session.resize",
            json!({
                "session_id": session_id,
                "epoch": reader_epoch,
                "resize_id": "rs-1",
                "cols": 120,
                "rows": 40,
            }),
        )
        .expect_err("reader resize");
    assert_eq!(err["code"], "NOT_INPUT_OWNER", "got {err}");

    // Reader input → NOT_INPUT_OWNER too.
    let err = client
        .request(
            "session.input",
            json!({
                "session_id": session_id,
                "epoch": reader_epoch,
                "input_id": "rd-1",
                "data_b64": b64(b"x"),
            }),
        )
        .expect_err("reader input");
    assert_eq!(err["code"], "NOT_INPUT_OWNER");

    // Valid writer input still works.
    let ok = client
        .request(
            "session.input",
            json!({
                "session_id": session_id,
                "epoch": writer_epoch,
                "input_id": "wr-1",
                "data_b64": b64(b"y"),
            }),
        )
        .expect("writer input");
    assert_eq!(ok["accepted_bytes"], 1);

    // Writer resize works and reports an applied seq.
    let resize = client
        .request(
            "session.resize",
            json!({
                "session_id": session_id,
                "epoch": writer_epoch,
                "resize_id": "rs-2",
                "cols": 100,
                "rows": 35,
            }),
        )
        .expect("writer resize");
    assert!(resize["applied_seq"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    let applied = client
        .wait_event("session.resize_applied", Duration::from_secs(5))
        .expect("resize_applied event");
    assert_eq!(applied["cols"], 100);

    // take_control with the wrong expected owner → INVALID_STATE (CAS fail).
    let err = client
        .request(
            "session.take_control",
            json!({
                "session_id": session_id,
                "view_id": reader_view,
                "expected_owner": common::uuid_v4(),
            }),
        )
        .expect_err("bad CAS");
    assert_eq!(err["code"], "INVALID_STATE");

    // take_control CAS success: reader becomes writer, new epoch,
    // owner_changed event, old writer demoted.
    let takeover = client
        .request(
            "session.take_control",
            json!({
                "session_id": session_id,
                "view_id": reader_view,
                "expected_owner": writer_view,
            }),
        )
        .expect("take_control");
    let new_epoch = takeover["epoch"].as_str().expect("new epoch").to_string();
    assert_ne!(new_epoch, reader_epoch);
    let changed = client
        .wait_event("session.owner_changed", Duration::from_secs(5))
        .expect("owner_changed event");
    assert_eq!(changed["old_owner"], writer_view);
    assert_eq!(changed["new_owner"], reader_view);

    // Old writer input now fails ownership; new owner succeeds.
    let err = client
        .request(
            "session.input",
            json!({
                "session_id": session_id,
                "epoch": writer_epoch,
                "input_id": "wr-2",
                "data_b64": b64(b"z"),
            }),
        )
        .expect_err("demoted writer input");
    assert_eq!(err["code"], "NOT_INPUT_OWNER");
    let ok = client
        .request(
            "session.input",
            json!({
                "session_id": session_id,
                "epoch": new_epoch,
                "input_id": "rd-2",
                "data_b64": b64(b"w"),
            }),
        )
        .expect("new owner input");
    assert_eq!(ok["accepted_bytes"], 1);

    // Third attach exceeds the 2-view limit → INVALID_STATE.
    let err = client
        .request(
            "session.attach",
            json!({"session_id": session_id, "view_id": common::uuid_v4(), "access": "reader"}),
        )
        .expect_err("third view");
    assert_eq!(err["code"], "INVALID_STATE");

    // A new writer attach demotes the current writer + owner_changed.
    let new_writer_view = common::uuid_v4();
    // Detach one view to make room first.
    let detach = client
        .request(
            "session.detach",
            json!({"session_id": session_id, "view_id": writer_view}),
        )
        .expect("detach");
    assert_eq!(detach["detached"], true);
    let attach = client
        .request(
            "session.attach",
            json!({"session_id": session_id, "view_id": new_writer_view, "access": "writer"}),
        )
        .expect("new writer attach");
    assert_ne!(attach["epoch"], new_epoch);
    let changed = client
        .wait_event("session.owner_changed", Duration::from_secs(5))
        .expect("second owner_changed");
    assert_eq!(changed["old_owner"], reader_view);
    assert_eq!(changed["new_owner"], new_writer_view);

    // Data-role restriction: only session.ack on a data connection.
    let data_token = client.data_token.clone().expect("token");
    let mut data = Client::data(&daemon.endpoint, &data_token);
    let err = data
        .request_timeout("system.snapshot", json!({}), Duration::from_secs(5))
        .expect("data conn must answer (error)")
        .expect_err("snapshot on data conn");
    assert_eq!(err["code"], "INVALID_ARGUMENT");
}
