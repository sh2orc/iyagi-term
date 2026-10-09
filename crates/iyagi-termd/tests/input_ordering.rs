//! `session.input`의 wire 순서 보장(02-runner §6): 검증은 연결 루프에서
//! 인라인으로, PTY 쓰기(≤750 ms)만 blocking pool에서 deferred로 돈다.
//!
//! 같은 컨트롤 연결에서 `session.input` **뒤에** `session.take_control`를
//! 보내면, 입력은 자신이 읽힌 시점의 소유권으로 검증을 지나야 한다. 옛
//! 구현(입력 전체를 pool로 미룸)에서는 take_control이 먼저 인라인 실행돼
//! 입력이 `NOT_INPUT_OWNER`로 거절됐고, 클라이언트는 화면 클리어 + 저널
//! 재생을 동반하는 전체 재접속으로 회복했다. tty 입력 큐가 찬 상태(읽지
//! 않는 프로그램)에서 재현하면 쓰기가 750 ms 상한까지 걸려 순서가 결정적이
//! 된다 — take_control은 밀리초 안에 끝나고, 입력 검증은 그보다 먼저
//! 일어나야 하므로.

mod common;

use serde_json::json;
use std::time::Duration;

use common::{b64, uuid_v4, Client, DaemonProc};

fn shell_launch(program: &str, args: &[&str]) -> serde_json::Value {
    json!({
        "request_id": common::uuid_v4(),
        "profile_id": common::uuid_v4(),
        "cwd": std::env::temp_dir().to_string_lossy(),
        "program": program,
        "argv": args,
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

fn attach_writer(control: &mut Client, session: &str, access: &str) -> (String, String) {
    let view = uuid_v4();
    let attach = control
        .request(
            "session.attach",
            json!({ "session_id": session, "view_id": view, "access": access }),
        )
        .expect("attach");
    let epoch = attach["epoch"].as_str().expect("epoch").to_string();
    (view, epoch)
}

#[test]
fn input_read_before_take_control_keeps_its_validation_order() {
    let daemon = DaemonProc::spawn("input-order", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    // memory fixture는 stdin을 전혀 읽지 않는다 — tty 입력 큐가 차면
    // writer는 750 ms 상한까지 WouldBlock을 재시도한다.
    let outcome = control
        .request(
            "workload.launch",
            shell_launch(
                &common::fixture_bin(),
                &["memory", "--mib", "1", "--hold-ms", "60000"],
            ),
        )
        .expect("launch memory fixture");
    let session = outcome["session_id"].as_str().expect("session").to_string();
    let (writer_view, writer_epoch) = attach_writer(&mut control, &session, "writer");
    let (reader_view, _reader_epoch) = attach_writer(&mut control, &session, "reader");

    // 큐 선채우기: 4 KiB 청크 하나면 어느 플랫폼의 tty 입력 큐(1–4 KiB)든
    // 찬다. 응답은 쓰기 완료 또는 750 ms 뒤 Queued — 어느 쪽이든 이 시점에
    // 큐가 차 있다.
    control
        .request(
            "session.input",
            json!({
                "session_id": session,
                "epoch": writer_epoch,
                "input_id": "order-prefill",
                "data_b64": b64(&vec![b'p'; 4096]),
            }),
        )
        .expect("prefill accepted (written or queued)");

    // 프로브 입력: 읽히는 즉시 인라인 검증을 지나고(소유권은 아직 writer),
    // 쓰기만 pool에서 750 ms를 채운다. 응답 id를 먼저 계산해 fire로 보낸다.
    let probe_id = control.next_request_id();
    control.fire(
        "session.input",
        json!({
            "session_id": session,
            "epoch": writer_epoch,
            "input_id": "order-probe",
            "data_b64": b64(&vec![b'x'; 4096]),
        }),
    );
    // 같은 연결로 곧바로 take_control — 인라인 실행이므로 밀리초 안에 끝난다.
    let takeover_id = control.next_request_id();
    control.fire(
        "session.take_control",
        json!({
            "session_id": session,
            "view_id": reader_view,
            "expected_owner": writer_view,
        }),
    );

    let takeover = control
        .wait_fired(&takeover_id, Duration::from_secs(5))
        .expect("takeover response");
    let takeover = takeover.expect("take_control must succeed");
    let new_epoch = takeover["epoch"].as_str().expect("new epoch");
    assert_ne!(new_epoch, writer_epoch);

    // 핵심 단언: 프로브 입력은 take_control보다 먼저 읽혔으므로 성공해야 한다.
    // 검증이 pool로 미뤄졌다면(회귀) NOT_INPUT_OWNER/STALE_EPOCH로 거절된다.
    let probe = control
        .wait_fired(&probe_id, Duration::from_secs(10))
        .expect("probe response");
    match probe {
        Ok(reply) => {
            let accepted = reply["accepted_bytes"].as_u64().unwrap_or(0);
            assert_eq!(
                accepted, 4096,
                "probe input accepted in wire order: {reply}"
            );
        }
        Err(err) => {
            panic!("input fired BEFORE take_control must not be rejected by the takeover: {err}")
        }
    }

    // 대조군: take_control 이후의 같은 epoch 입력은 거절된다 — 순서가 실제로
    // 뒤바뀜을 증명(회귀 테스트가 통과를 가장하지 않게).
    let err = control
        .request(
            "session.input",
            json!({
                "session_id": session,
                "epoch": writer_epoch,
                "input_id": "order-after",
                "data_b64": b64(b"z"),
            }),
        )
        .expect_err("demoted writer input");
    assert_eq!(err["code"], "NOT_INPUT_OWNER", "got {err}");

    let _ = control.request(
        "workload.cancel",
        json!({ "request_id": "cancel-input-order", "workload_id": outcome["workload_id"] }),
    );
}
