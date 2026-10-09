//! `session.focus` — 창이 지금 보고 있는 세션 보고(spec
//! `08-pressure-relief.md` §1 관측 단계).
//!
//! 압력 완화는 보이는 pane을 절대 건드리지 않으므로 데몬이 포커스 집합을
//! 알아야 한다. 계약:
//!
//! * 보고한 세션은 결과와 `system.snapshot.focused_session_ids`에 나타난다,
//! * `session_id: null`은 "이 창은 아무것도 보고 있지 않다" — 자기 항목만 지운다,
//! * 모르는 세션(또는 이미 끝난 세션)은 `session not found`로 정직하게 거절한다,
//! * 컨트롤 연결이 닫히면 그 연결의 포커스만 사라진다(다른 창은 그대로),
//! * 포커스한 워크로드가 종료되면 데몬이 그 항목을 스스로 거둔다.

mod common;

use common::{launch_request, wait_workload_state, Client, DaemonProc};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const SETTLE: Duration = Duration::from_secs(30);

/// 살아 있는 셸 하나를 띄우고 `(workload_id, session_id)`를 돌려준다.
/// `echo` 픽스처는 stdin을 기다리므로 시험이 끝날 때까지 RUNNING이다.
fn live_shell(control: &mut Client) -> (Value, String) {
    let launch = control
        .request(
            "workload.launch",
            launch_request("shell", &["echo"], "1048576"),
        )
        .expect("shell launch");
    assert_eq!(launch["state"], "RUNNING", "shell launches never queue");
    let session = launch["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    (launch["workload_id"].clone(), session)
}

fn focused_ids(control: &mut Client) -> Vec<String> {
    let snapshot = control
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    snapshot["focused_session_ids"]
        .as_array()
        .expect("focused_session_ids is always present")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect()
}

/// 스냅샷의 포커스 집합이 `wanted`와 같아질 때까지 기다린다(연결 종료는
/// 데몬이 다음 브로드캐스트에서 알아채므로 즉시가 아니다).
fn wait_focused(control: &mut Client, wanted: &[&str], timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    let mut last = Vec::new();
    while Instant::now() < deadline {
        last = focused_ids(control);
        if last.iter().map(String::as_str).eq(wanted.iter().copied()) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("focused_session_ids never became {wanted:?} within {timeout:?}; last: {last:?}");
}

#[test]
fn focus_is_reported_in_the_result_and_the_snapshot() {
    let daemon = DaemonProc::spawn("session-focus", Some(common::relaxed_admission(json!({}))));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    // 아무도 보고하지 않았으면 집합은 비어 있다(없음은 0이 아니라 빈 목록).
    assert!(focused_ids(&mut control).is_empty());

    let (_workload, session) = live_shell(&mut control);
    let result = control
        .request("session.focus", json!({ "session_id": session }))
        .expect("focus");
    assert_eq!(
        result["focused_session_ids"],
        json!([session]),
        "결과는 데몬 전체 집합이다"
    );
    assert_eq!(focused_ids(&mut control), vec![session.clone()]);

    // 같은 보고를 반복해도 결과는 같다(멱등).
    let again = control
        .request("session.focus", json!({ "session_id": session }))
        .expect("focus again");
    assert_eq!(again["focused_session_ids"], json!([session]));

    // null = 아무것도 보고 있지 않다.
    let cleared = control
        .request("session.focus", json!({ "session_id": null }))
        .expect("clear focus");
    assert_eq!(cleared["focused_session_ids"], json!([]));
    assert!(focused_ids(&mut control).is_empty());

    // 파라미터 자체가 없어도 같은 뜻이다.
    let empty = control
        .request("session.focus", json!({}))
        .expect("clear focus with empty params");
    assert_eq!(empty["focused_session_ids"], json!([]));
}

#[test]
fn unknown_and_finished_sessions_are_refused() {
    let daemon = DaemonProc::spawn(
        "session-focus-unknown",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let err = control
        .request("session.focus", json!({ "session_id": common::uuid_v4() }))
        .expect_err("unknown session must be refused");
    assert_eq!(err["code"], "INVALID_ARGUMENT");

    let err = control
        .request("session.focus", json!({ "session_id": "not-a-uuid" }))
        .expect_err("malformed session id must be refused");
    assert_eq!(err["code"], "INVALID_ARGUMENT");

    // 끝난 세션은 재생용으로 남아 있어도 포커스 대상이 아니다.
    let launch = control
        .request(
            "workload.launch",
            launch_request("shell", &["exit", "--code", "0"], "1048576"),
        )
        .expect("short shell launch");
    let session = launch["session_id"].as_str().expect("session").to_string();
    wait_workload_state(&mut control, &launch["workload_id"], &["SUCCEEDED"], SETTLE);
    let err = control
        .request("session.focus", json!({ "session_id": session }))
        .expect_err("finished session must be refused");
    assert_eq!(err["code"], "INVALID_ARGUMENT");
    assert!(focused_ids(&mut control).is_empty());
}

#[test]
fn a_closed_window_drops_only_its_own_focus() {
    let daemon = DaemonProc::spawn(
        "session-focus-disconnect",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut first, _) = Client::control(&daemon.endpoint, &daemon.token);
    let (mut second, _) = Client::control(&daemon.endpoint, &daemon.token);

    let (_w1, s1) = live_shell(&mut first);
    let (_w2, s2) = live_shell(&mut second);
    first
        .request("session.focus", json!({ "session_id": s1 }))
        .expect("first focus");
    let both = second
        .request("session.focus", json!({ "session_id": s2 }))
        .expect("second focus");
    let both: Vec<String> = both["focused_session_ids"]
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    let mut expected = vec![s1.clone(), s2.clone()];
    expected.sort();
    assert_eq!(both, expected, "집합은 창마다 하나씩 모인다");

    // 둘째 창이 사라진다: 자기 항목만 사라지고 첫째 창의 포커스는 남는다.
    drop(second);
    wait_focused(&mut first, &[s1.as_str()], Duration::from_secs(15));
}

#[test]
fn finalizing_the_focused_workload_clears_the_focus() {
    let daemon = DaemonProc::spawn(
        "session-focus-finalize",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let (workload, session) = live_shell(&mut control);
    control
        .request("session.focus", json!({ "session_id": session }))
        .expect("focus");
    assert_eq!(focused_ids(&mut control), vec![session.clone()]);

    control
        .request("workload.cancel", json!({ "workload_id": workload }))
        .expect("cancel");
    wait_workload_state(
        &mut control,
        &workload,
        &["CANCELLED", "SUCCEEDED", "FAILED"],
        SETTLE,
    );
    wait_focused(&mut control, &[], Duration::from_secs(15));
}
