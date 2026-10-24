//! `auto_continue` — 네트워크 끊김 자동 continue(auto_continue.rs).
//!
//! 계약:
//! * 세션 출력에 API 연결 오류 문구가 나오면(에코 픽스처로 재현) 그 세션은
//!   무장하고 `auto_continue.changed {state:"armed"}`를 방송한다,
//! * 인터넷이 살아 있으면 회복 루프가 `continue`를 입력하고
//!   `{state:"fired"}`를 방송한다 — 에코가 돌려준 출력에서 확인한다.
//!   회복 단계는 기계가 온라인일 때만 단언한다(오프라인 CI에서도 감지
//!   계약은 완전히 검증된다).

mod common;

use serde_json::{json, Value};
use std::time::{Duration, Instant};

use common::{b64, uuid_v4, Client, DaemonProc};

const SETTLE: Duration = Duration::from_secs(30);

/// 살아 있는 echo 픽스처 셸 — 입력을 그대로 출력으로 되돀린다.
fn live_echo_shell(control: &mut Client) -> (Value, String) {
    let launch = control
        .request(
            "workload.launch",
            common::launch_request("shell", &["echo"], "1048576"),
        )
        .expect("shell launch");
    assert_eq!(launch["state"], "RUNNING", "shell launches never queue");
    let session = launch["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    (launch["workload_id"].clone(), session)
}

fn attach_writer(control: &mut Client, session: &str) -> (String, String) {
    let view = uuid_v4();
    let attach = control
        .request(
            "session.attach",
            json!({ "session_id": session, "view_id": view, "access": "writer" }),
        )
        .expect("attach");
    let epoch = attach["epoch"].as_str().expect("epoch").to_string();
    (view, epoch)
}

fn send_input(control: &mut Client, session: &str, epoch: &str, view: &str, text: &str) {
    control
        .request(
            "session.input",
            json!({
                "session_id": session,
                "epoch": epoch,
                "view_id": view,
                "input_id": uuid_v4(),
                "data_b64": b64(text.as_bytes()),
            }),
        )
        .expect("session.input");
}

/// 테스트 판정용 인터넷 확인(회복 단계는 온라인일 때만 단언한다).
fn internet_reachable() -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let Ok(addrs) = ("api.anthropic.com", 443).to_socket_addrs() else {
        return false;
    };
    for addr in addrs {
        if TcpStream::connect_timeout(&addr, Duration::from_secs(3)).is_ok() {
            return true;
        }
    }
    false
}

#[test]
fn api_error_phrase_arms_and_recovery_sends_continue() {
    let daemon = DaemonProc::spawn("auto-continue", Some(common::relaxed_admission(json!({}))));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let (_workload, session) = live_echo_shell(&mut control);
    let (view, epoch) = attach_writer(&mut control, &session);

    // 에코 픽스처는 입력을 출력으로 되돌린다 — 오류 문구를 입력하면 그
    // 문구가 출력에 나타나 감지기가 무장한다.
    send_input(
        &mut control,
        &session,
        &epoch,
        &view,
        "API Error: Can't reach the API server (ENOTFOUND)\r",
    );
    let armed = control
        .wait_event("auto_continue.changed", SETTLE)
        .expect("armed event");
    assert_eq!(armed["state"], "armed", "payload: {armed}");
    assert_eq!(armed["session_id"], json!(session));

    if !internet_reachable() {
        eprintln!(
            "offline machine: skipping the recovery assertion \
             (the detection contract above is fully verified)"
        );
        return;
    }

    // 회복 루프는 무장을 알아차린 뒤 즉시 프로브한다 — 온라인이면 곧 fired.
    let fired = control
        .wait_event("auto_continue.changed", SETTLE)
        .expect("fired event");
    assert_eq!(fired["state"], "fired", "payload: {fired}");
    assert_eq!(fired["session_id"], json!(session));

    // 에코가 `continue`를 돌려줬는지 저널에서 확인한다(첫 프로브 전에 이미
    // 온라인이면 무장→발사가 한 틱 안에 끝나 출력도 이미 저널에 있다).
    let deadline = Instant::now() + SETTLE;
    loop {
        let found = control
            .request(
                "session.search",
                json!({ "query": "continue", "session_id": session }),
            )
            .expect("session.search")["matches"]
            .as_array()
            .map(|matches| !matches.is_empty())
            .unwrap_or(false);
        if found {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "echoed `continue` never reached the journal"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}
