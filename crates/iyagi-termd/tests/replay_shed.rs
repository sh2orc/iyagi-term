//! 재생 정체(silent stall) 회귀 시험: 완료 이벤트가 없는 프로토콜에서
//! 데몬이 조용히 멈추면 클라이언트의 "기록 재생 중…" 갇힘이 된다.
//!
//! * 데이터 연결이 죽은 뷰 — 컨트롤 연결은 살아 있어 다른 경로가 정리해
//!   주지 않는다. 연결 해체 지점이 뷰를 떼고 `session.replay_required`로
//!   다시 붙으라고 알리는지 확인한다.
//! * 읽을 수 없는 저널(손상) — 읽기 실패를 재시도만 하면 뷰는 남은 기록을
//!   영원히 받지 못한다. 5초 이상 실패가 이어지면 뷰를 떼고 다시 붙으라고
//!   알리는지 확인한다.

mod common;

use common::{launch_request, wait_workload_state, Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

const SETTLE: Duration = Duration::from_secs(30);

/// 1 MiB 플러드 + ACK 없음: 흐름 크레딧(256 KiB 고수위)이 뷰를 중간에
/// 멈춘다 — 커서가 끝에 못 미치므로 데이터 연결이 죽는 순간에도 "보낼 것이
/// 남은" 뷰가 된다.
#[test]
fn data_conn_death_sheds_pending_view_with_replay_required() {
    let daemon = DaemonProc::spawn(
        "replay-shed-dead-conn",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut control, hello) = Client::control(&daemon.endpoint, &daemon.token);
    let token_str = hello["data_token"].as_str().unwrap().to_string();
    let mut data = Client::data(&daemon.endpoint, &token_str);

    let launch = control
        .request(
            "workload.launch",
            launch_request(
                "shell",
                &[
                    "flood", "--bytes", "1048576", "--chunk", "1024", "--seed", "5",
                ],
                "536870912",
            ),
        )
        .expect("launch");
    let session = launch["session_id"].as_str().unwrap().to_string();
    wait_workload_state(&mut control, &launch["workload_id"], &["SUCCEEDED"], SETTLE);

    let view = common::uuid_v4();
    control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": view, "access": "reader"}),
        )
        .expect("attach");

    // 재생이 흐름 크레딧에 막혀 끝까지 못 갔다(보낼 레코드가 남아 있다).
    let blocked = control
        .wait_event("session.flow_blocked", Duration::from_secs(10))
        .expect("view is flow-blocked mid-replay (records still pending)");
    assert_eq!(blocked["view_id"].as_str(), Some(view.as_str()));

    // 컨트롤 연결은 살려 둔 채 데이터 연결만 닫는다(그냥 drop이면 읽기
    // 스레드의 복제 핸들이 소켓을 붙들어 데몬이 죽음을 못 본다 — kill).
    data.kill();
    drop(data);

    // 데이터 연결 해체 지점이 뷰를 떼고 다시 붙으라고 알린다 — 예전에는
    // 이 뷰를 펌프가 매 패스 조용히 건너뛰기만 했다.
    let replay = control
        .wait_event("session.replay_required", Duration::from_secs(10))
        .expect("replay_required after the data connection died");
    assert_eq!(replay["session_id"].as_str(), Some(session.as_str()));
    assert_eq!(replay["view_id"].as_str(), Some(view.as_str()));

    // 뷰는 실제로 레지스트리에서 사라졌다: 같은 뷰의 detach는 이제 실패한다.
    let err = control
        .request(
            "session.detach",
            json!({"session_id": session, "view_id": view}),
        )
        .expect_err("shed view is gone from the registry");
    assert_eq!(
        err["code"].as_str(),
        Some("INVALID_ARGUMENT"),
        "detach of a shed view must fail: {err}"
    );
}

/// 저널 중간을 손상시킨 뒤 attach: 첫 읽기부터 Corrupt로 실패하고, 5초의
/// 연속 실패 창이 차면 뷰가 떨어지며 `session.replay_required`가 간다.
/// (같은 view id/epoch의 재attach나 HeadTrimmed 경로가 아니어야 한다.)
#[test]
fn persistent_journal_read_failure_sheds_views_after_five_seconds() {
    let daemon = DaemonProc::spawn(
        "replay-shed-corrupt-journal",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut control, hello) = Client::control(&daemon.endpoint, &daemon.token);
    let token_str = hello["data_token"].as_str().unwrap().to_string();
    let data = Client::data(&daemon.endpoint, &token_str);

    let launch = control
        .request(
            "workload.launch",
            launch_request(
                "shell",
                &[
                    "flood", "--bytes", "262144", "--chunk", "1024", "--seed", "7",
                ],
                "536870912",
            ),
        )
        .expect("launch");
    let session = launch["session_id"].as_str().unwrap().to_string();
    wait_workload_state(&mut control, &launch["workload_id"], &["SUCCEEDED"], SETTLE);

    // 저널 중간(첫 스캔 창 안쪽)을 덮어 쓴다 — CRC/프레이밍이 깨져 매 패스
    // 읽기가 Corrupt로 실패한다. 종료 세션이므로 writer는 이미 파일을
    // 닫았다.
    let journal = daemon
        .data_dir
        .join("data/journals")
        .join(format!("{session}.mtj"));
    let size = std::fs::metadata(&journal).expect("journal on disk").len();
    assert!(size > 80_000, "flood journal must be sizable ({size})");
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&journal)
            .expect("open journal for corruption");
        file.seek(SeekFrom::Start(60_000)).expect("seek");
        file.write_all(&[0xa5u8; 8_192]).expect("corrupt records");
        file.flush().expect("flush corruption");
    }

    let view = common::uuid_v4();
    let attached_at = Instant::now();
    control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": view, "access": "reader"}),
        )
        .expect("attach (in-memory entry; disk scan happens in the pump)");

    // 읽기 실패가 5초 이어진 뒤에야 뷰가 떨어진다 — 그 전의 어떤 경로도
    // (HeadTrimmed 오분류 등) 이벤트를 내면 안 된다.
    let replay = control
        .wait_event("session.replay_required", Duration::from_secs(20))
        .expect("replay_required after persistent read failures");
    assert_eq!(replay["session_id"].as_str(), Some(session.as_str()));
    assert_eq!(replay["view_id"].as_str(), Some(view.as_str()));
    assert!(
        attached_at.elapsed() >= Duration::from_millis(4_500),
        "shed must wait out the 5s failure window (took {:?})",
        attached_at.elapsed()
    );

    // 데이터 연결은 살아 있다(이 시험의 떼어내기 원인은 읽기 실패뿐이다).
    assert!(
        !data.closed(Duration::from_millis(200)),
        "the data connection must stay up; only the journal is broken"
    );

    // 뷰는 실제로 레지스트리에서 사라졌다.
    let err = control
        .request(
            "session.detach",
            json!({"session_id": session, "view_id": view}),
        )
        .expect_err("shed view is gone from the registry");
    assert_eq!(err["code"].as_str(), Some("INVALID_ARGUMENT"));
}
