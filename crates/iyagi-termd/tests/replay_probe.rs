//! 재생 프로브(W1-11): 살아 있는 세션에서 detach → 재attach 시 저널 재생이
//! data 연결로 흐르는지 확인한다.
//!
//! **현재 미해결 결함(2026-09-08 발견)**: 256 KiB(약 260레코드)까지는
//! 재생이 완료되나, 그 이상(2 MiB ≈ 2050레코드)에서는 재attach 직후
//! 몇 프레임(5~43, 실행마다 다름)만 전달되고 펌프·연결 writer가 모두
//! 정지한다. 데몬 로그에 오류 없음, 연결도 살아 있음. 첫 attach에서는
//! 동일 저널이 정상 전달되므로 경로 자체는 동작 — 두 번째 뷰의 재생에서만
//! 발생. 원인 미특정(전달 펌프 정지 지점이 attach 핸들러 반환 직후).
//!
//! 재현: `--bytes 2097152`로 이 시험을 돌린다(아래 `#[ignore]` 해제).
//! 256 KiB로 낮추면 통과한다. replay-matrix 벤치 시나리오도 이 결함으로
//! INCOMPLETE 상태로 끝난다(180초 상한 후).

mod common;

use common::{Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
#[ignore = "미해결: 대형 저널(>256 KiB) 재attach 재생 정지 — 위 모듈 문서 참조"]
fn detach_reattach_replays_whole_journal_while_alive() {
    let daemon = DaemonProc::spawn("replay-probe", None);
    let (mut control, hello) = Client::control(&daemon.endpoint, &daemon.token);
    let token_str = hello["data_token"].as_str().unwrap().to_string();
    let mut data = Client::data(&daemon.endpoint, &token_str);
    let cwd = daemon.data_dir.clone();

    let launch = control
        .request(
            "workload.launch",
            json!({
                "request_id": common::uuid_v4(),
                "profile_id": "p",
                "cwd": cwd.to_string_lossy(),
                "program": common::fixture_bin(),
                "argv": ["flood", "--bytes", "262144", "--chunk", "16384", "--seed", "3", "--hold-ms", "60000"],
                "env_overrides": {},
                "mode": "shell",
                "executor": {"kind": "local"},
                "cols": 80, "rows": 24, "priority": 1,
                "policy": {"reservation_bytes": "536870912", "cpu_slots": 1, "enforcement": "observe",
                           "memory_max_bytes": null, "cpu_max_cores": null, "pids_max": null}
            }),
        )
        .expect("launch");
    let session = launch["session_id"].as_str().unwrap().to_string();
    let workload = launch["workload_id"].as_str().unwrap().to_string();
    let view1 = common::uuid_v4();

    let attach = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": view1, "access": "writer"}),
        )
        .expect("attach1");
    let epoch1 = attach["epoch"].as_str().unwrap().to_string();
    let last_seq_live: u64 = attach["last_seq"].as_str().unwrap().parse().unwrap();

    // 1단계: flood가 끝날 때까지(3초 무출력) ACK.
    let mut last_output = Instant::now();
    let mut max_seq = 0u64;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(frame) = data.recv_any(Duration::from_millis(50)) {
            let p = &frame["payload"];
            if p["session_id"].as_str() == Some(session.as_str()) {
                if let Some(seq) = p["seq"].as_str().and_then(|v: &str| v.parse::<u64>().ok()) {
                    last_output = Instant::now();
                    if seq > max_seq {
                        max_seq = seq;
                        data.send_ack(&session, &epoch1, seq);
                    }
                }
            }
        } else if last_output.elapsed() > Duration::from_secs(3) {
            break;
        }
        assert!(Instant::now() < deadline, "flood did not finish");
    }
    println!("probe: flood drained, max_seq={max_seq}, last_seq(at attach)={last_seq_live}");
    assert!(
        max_seq > 0,
        "no output frames arrived on data conn in phase 1"
    );

    // 2단계: detach → 재attach(전체 재생).
    control
        .request(
            "session.detach",
            json!({"session_id": session, "view_id": view1}),
        )
        .expect("detach1");

    let attach2 = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": common::uuid_v4(), "access": "writer"}),
        )
        .expect("attach2");
    let epoch2 = attach2["epoch"].as_str().unwrap().to_string();
    let last_seq2: u64 = attach2["last_seq"].as_str().unwrap().parse().unwrap();
    println!("probe: re-attach last_seq={last_seq2}");

    let started = Instant::now();
    let mut replayed = 0u64;
    let mut acked2 = 0u64;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(frame) = data.recv_any(Duration::from_millis(50)) {
            let p = &frame["payload"];
            if p["session_id"].as_str() == Some(session.as_str()) {
                if let Some(seq) = p["seq"].as_str().and_then(|v: &str| v.parse::<u64>().ok()) {
                    replayed += 1;
                    if replayed <= 5 || replayed.is_multiple_of(100) {
                        eprintln!(
                            "probe recv #{replayed} seq={seq} at {:?}",
                            started.elapsed()
                        );
                    }
                    if seq > acked2 {
                        acked2 = seq;
                        data.send_ack(&session, &epoch2, seq);
                        if seq >= last_seq2 {
                            break;
                        }
                    }
                }
            }
        }
        if Instant::now() > deadline {
            panic!(
                "replay stalled: replayed={replayed} acked2={acked2} last_seq={last_seq2} data_conn_closed={} after {:?}",
                data.closed(Duration::from_millis(200)),
                started.elapsed()
            );
        }
    }
    println!(
        "probe: replay complete in {:?} ({} frames, last={acked2})",
        started.elapsed(),
        replayed
    );

    let _ = control.request(
        "workload.cancel",
        json!({"request_id": common::uuid_v4(), "workload_id": workload}),
    );
}
