//! 스냅샷 복원 attach(`resume_from_seq`): UI가 이미 그린 화면 뒤의 레코드만
//! 재생한다. 요청한 seq가 보존된 저널 범위 밖이면 조용히 헤드부터 재생하고,
//! UI는 `replay_from_seq`가 요청값과 같을 때만 스냅샷을 쓴다.

mod common;

use common::{launch_request, wait_workload_state, Client, DaemonProc};
use serde_json::json;
use std::time::Duration;

const SETTLE: Duration = Duration::from_secs(30);

fn attach(
    control: &mut Client,
    session: &str,
    view: &str,
    resume: Option<u64>,
) -> serde_json::Value {
    let mut params = json!({"session_id": session, "view_id": view, "access": "reader"});
    if let Some(seq) = resume {
        params["resume_from_seq"] = json!(seq.to_string());
    }
    control.request("session.attach", params).expect("attach")
}

fn detach(control: &mut Client, session: &str, view: &str) {
    control
        .request(
            "session.detach",
            json!({"session_id": session, "view_id": view}),
        )
        .expect("detach");
}

#[test]
fn attach_resumes_after_a_snapshot_only_inside_the_retained_journal() {
    let daemon = DaemonProc::spawn("attach-resume", Some(common::relaxed_admission(json!({}))));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let launch = control
        .request(
            "workload.launch",
            launch_request(
                "shell",
                &["flood", "--bytes", "8192", "--chunk", "2048", "--seed", "3"],
                "1048576",
            ),
        )
        .expect("shell launch");
    let session = launch["session_id"].as_str().expect("session").to_string();
    wait_workload_state(&mut control, &launch["workload_id"], &["SUCCEEDED"], SETTLE);

    let view = common::uuid_v4();
    let full = attach(&mut control, &session, &view, None);
    assert_eq!(
        full["replay_from_seq"], "1",
        "스냅샷이 없으면 처음부터 재생한다"
    );
    let last_seq: u64 = full["last_seq"]
        .as_str()
        .expect("last_seq")
        .parse()
        .expect("u64");
    assert!(
        last_seq >= 3,
        "크기 레코드와 출력 레코드가 있다 (last_seq={last_seq})"
    );
    detach(&mut control, &session, &view);

    let view = common::uuid_v4();
    let resumed = attach(&mut control, &session, &view, Some(3));
    assert_eq!(
        resumed["replay_from_seq"], "3",
        "보존된 seq면 그 뒤만 재생한다"
    );
    assert_eq!(resumed["last_seq"], full["last_seq"]);
    assert!(resumed.get("replay_dropped_bytes").is_none());
    detach(&mut control, &session, &view);

    let view = common::uuid_v4();
    let caught_up = attach(&mut control, &session, &view, Some(last_seq + 1));
    assert_eq!(
        caught_up["replay_from_seq"],
        (last_seq + 1).to_string(),
        "스냅샷이 마지막 레코드까지 담았으면 재생할 것이 없다"
    );
    detach(&mut control, &session, &view);

    let view = common::uuid_v4();
    let ahead = attach(&mut control, &session, &view, Some(last_seq + 5));
    assert_eq!(
        ahead["replay_from_seq"], "1",
        "저널보다 앞선 스냅샷은 믿지 않고 처음부터 재생한다"
    );
    detach(&mut control, &session, &view);
}
