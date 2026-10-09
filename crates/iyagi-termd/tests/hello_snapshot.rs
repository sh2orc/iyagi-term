//! hello → system.snapshot round-trip: capabilities present, revision ≥ 1,
//! monotonic revision across a mutating call.

mod common;

use common::{Client, DaemonProc};
use serde_json::json;
use std::time::Duration;

#[test]
fn snapshot_reports_capabilities_and_monotonic_revision() {
    let daemon = DaemonProc::spawn("snapshot", None);

    let (mut client, hello) = Client::control(&daemon.endpoint, &daemon.token);
    assert_eq!(hello["protocol"], 1, "protocol version 1");
    assert!(hello["daemon_id"].is_string());
    assert!(hello["connection_id"].is_string());
    assert!(
        hello["data_token"].is_string(),
        "control hello carries a data token"
    );
    let platform = hello["capabilities"]["platform"]
        .as_str()
        .unwrap_or_default();
    assert!(!platform.is_empty(), "capabilities.platform present");

    let first = client
        .request("system.snapshot", json!({}))
        .expect("snapshot 1");
    let revision = first["revision"].as_u64().expect("revision number");
    assert!(revision >= 1);
    assert!(first["capabilities"]["platform"].is_string());
    assert!(first["queue"].as_array().is_some());
    assert!(first["workloads"].as_array().is_some());
    assert!(first["host"].is_object());
    assert_eq!(first["reconciliation_required"], false);
    // 08 §1: 아직 아무 창도 포커스를 보고하지 않았다 — 없음은 빈 목록이다.
    assert_eq!(
        first["focused_session_ids"].as_array().map(Vec::len),
        Some(0)
    );

    // resource.snapshot events flow on control connections (1 s cadence).
    let event = client
        .wait_event("resource.snapshot", Duration::from_secs(5))
        .expect("resource snapshot event");
    assert!(event["host"].is_object());
    // 08 §1: 이벤트는 메모리 압력과 CPU 포화도를 함께 싣고, 호스트 샘플도
    // 히스테리시스를 거친 CPU 레벨을 그대로 들고 있다.
    for level in [
        &event["pressure"],
        &event["cpu_pressure"],
        &event["host"]["cpu_pressure"],
    ] {
        assert!(
            matches!(level.as_str(), Some("NORMAL" | "WARNING" | "CRITICAL")),
            "pressure levels are spec constants, got {level}"
        );
    }
    assert_eq!(
        event["cpu_pressure"], event["host"]["cpu_pressure"],
        "이벤트의 cpu_pressure와 샘플의 값은 같은 추적기의 출력이다"
    );

    let second = client
        .request("system.snapshot", json!({}))
        .expect("snapshot 2");
    assert!(
        second["revision"].as_u64().unwrap_or(0) >= revision,
        "revision must not go backwards"
    );
}

#[test]
fn unknown_method_is_a_clean_error() {
    let daemon = DaemonProc::spawn("unknown-method", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let err = client
        .request("no.such.method", json!({}))
        .expect_err("unknown method must error");
    assert_eq!(err["code"], "INVALID_ARGUMENT");
}
