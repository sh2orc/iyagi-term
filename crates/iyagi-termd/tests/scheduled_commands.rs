//! `schedule.*` — 예약 명령(지정 시각에 세션 PTY에 one-shot 입력).
//!
//! 계약:
//! * 만료된 예약은 데몬 루프가 actor 경로로 쓰고 `schedule.fired`(outcome
//!   `fired`)를 방송한다 — view/epoch 소유권과 무관하다,
//! * 세션이 먼저 끝나면 finalize 훅이 pending을 `skipped("session-exited")`로,
//! * 취소는 pending일 때만,
//! * 과거 시각·없는 세션·빈 명령은 생성 단계에서 거절,
//! * 클레임된 `firing` 행은 두 번 발사되지 않는다(스토리지 클레임 원자성).

mod common;

use common::{launch_request, Client, DaemonProc};
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SETTLE: Duration = Duration::from_secs(30);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("wall clock after epoch")
        .as_millis() as u64
}

/// 살아 있는 echo 픽스처 셸 하나를 띄워 세션 id를 돌려준다. stdin을
/// 기다리므로 시험 내내 RUNNING이다.
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

fn schedule(control: &mut Client, session: &str, command: &str, fire_at_ms: u64) -> Value {
    control
        .request(
            "schedule.create",
            json!({ "session_id": session, "command": command, "fire_at_ms": fire_at_ms }),
        )
        .expect("schedule.create")
}

#[test]
fn due_command_is_written_and_broadcast() {
    let daemon = DaemonProc::spawn("schedule-fire", Some(common::relaxed_admission(json!({}))));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let (_workload, session) = live_shell(&mut control);
    // 만료까지 데몬 틱(500ms) 여유를 두되 시험을 느리게 하지 않는 창.
    let created = schedule(&mut control, &session, "cargo test", now_ms() + 1_500);

    let schedule_id = created["schedule_id"]
        .as_str()
        .expect("schedule id")
        .to_string();
    // wait_event는 프레임이 아니라 payload를 돌려준다.
    let event = control
        .wait_event("schedule.fired", SETTLE)
        .expect("fired event");
    assert_eq!(event["schedule_id"], json!(schedule_id));
    assert_eq!(event["outcome"], "fired", "payload: {event}");
    assert_eq!(event["session_id"], json!(session));
    assert_eq!(event["command"], "cargo test");

    let list = control.request("schedule.list", json!({})).expect("list");
    let row = list["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|row| row["schedule_id"] == json!(schedule_id))
        .expect("row present");
    assert_eq!(row["status"], "fired");
    assert_eq!(row["attempts"], 1, "한 번의 클레임으로 끝난다: {row}");
    assert!(
        row["fired_at_ms"].as_u64().expect("fired_at_ms") >= row["fire_at_ms"].as_u64().unwrap()
    );
}

#[test]
fn cancel_only_works_while_pending() {
    let daemon = DaemonProc::spawn(
        "schedule-cancel",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let (_workload, session) = live_shell(&mut control);
    // 아직 만료되지 않은 미래 시각.
    let created = schedule(&mut control, &session, "echo hi", now_ms() + 60_000);
    let schedule_id = created["schedule_id"]
        .as_str()
        .expect("schedule id")
        .to_string();

    let cancelled = control
        .request("schedule.cancel", json!({ "schedule_id": schedule_id }))
        .expect("cancel");
    assert_eq!(cancelled["cancelled"], true);
    // 두 번째 취소는 조용히 false.
    let again = control
        .request("schedule.cancel", json!({ "schedule_id": schedule_id }))
        .expect("cancel again");
    assert_eq!(again["cancelled"], false);

    // 시간이 흘러도(스윕 주기보다 멀리서 종료) 취소된 예약은 발사하지 않는다.
    let list = control.request("schedule.list", json!({})).expect("list");
    let row = list["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|row| row["schedule_id"] == json!(schedule_id))
        .expect("row present");
    assert_eq!(row["status"], "cancelled");
    assert!(
        control
            .wait_event("schedule.fired", Duration::from_secs(2))
            .is_none(),
        "취소된 예약은 이벤트를 내지 않는다"
    );
}

#[test]
fn session_exit_skips_pending_schedules() {
    let daemon = DaemonProc::spawn("schedule-exit", Some(common::relaxed_admission(json!({}))));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let (workload, session) = live_shell(&mut control);
    schedule(&mut control, &session, "echo late", now_ms() + 60_000);

    // 세션이 먼저 끝난다 — finalize 훅이 pending을 건너뛴다.
    control
        .request(
            "workload.cancel",
            json!({ "workload_id": workload, "force": true }),
        )
        .expect("cancel workload");
    let event = control
        .wait_event("schedule.fired", SETTLE)
        .expect("skip event");
    assert_eq!(event["outcome"], "skipped", "payload: {event}");
    assert_eq!(event["reason_code"], "session-exited");

    let list = control
        .request("schedule.list", json!({ "session_id": session }))
        .expect("list");
    let row = list["items"]
        .as_array()
        .expect("items")
        .first()
        .expect("row");
    assert_eq!(row["status"], "skipped");
    assert_eq!(row["reason_code"], "session-exited");
}

#[test]
fn create_validates_command_time_and_session() {
    let daemon = DaemonProc::spawn(
        "schedule-create-validation",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let (_workload, session) = live_shell(&mut control);
    let now = now_ms();

    // 과거 시각.
    let err = control
        .request(
            "schedule.create",
            json!({ "session_id": session, "command": "ls", "fire_at_ms": now - 1 }),
        )
        .expect_err("past fire time rejected");
    assert_eq!(err["code"], "INVALID_ARGUMENT", "{err}");
    // 1년 horizon 밖.
    let err = control
        .request("schedule.create", json!({ "session_id": session, "command": "ls", "fire_at_ms": now + 366 * 24 * 3600 * 1000 }))
        .expect_err("far future rejected");
    assert_eq!(err["code"], "INVALID_ARGUMENT", "{err}");
    // 빈 명령.
    let err = control
        .request(
            "schedule.create",
            json!({ "session_id": session, "command": "", "fire_at_ms": now + 60_000 }),
        )
        .expect_err("empty command rejected");
    assert_eq!(err["code"], "INVALID_ARGUMENT", "{err}");
    // 모르는 세션.
    let err = control
        .request(
            "schedule.create",
            json!({
                "session_id": "00000000-0000-4000-8000-000000000000",
                "command": "ls",
                "fire_at_ms": now + 60_000,
            }),
        )
        .expect_err("unknown session rejected");
    assert_eq!(err["code"], "INVALID_ARGUMENT", "{err}");
}
