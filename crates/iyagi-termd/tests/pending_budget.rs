//! 연결당 pending 완료 예산(ipc.rs, 02-runner §6) — `limits.max_pending_inputs`
//! 오버라이드로 낮춰 잰다(기본 64를 실제로 채우려면 세션 65개가 필요하므로).
//!
//! 세 가지 계약:
//! 1. 입력 완료가 예산에 찼을 때 들어온 `session.input`은 dispatch 전에
//!    `BUSY`("too many pending session requests")로 거절된다.
//! 2. resize 예산은 입력 예산과 독립이다 — 입력이 진행 중이어도
//!    `session.resize`는 통과한다(막힌 입력이 resize를 굶기지 않는다).
//! 3. 완료가 빠지면 카운터도 빠져, 다음 입력은 다시 예산 게이트를 지나간다.
//!
//! "입력 완료가 진행 중" 상태를 만드는 장치: `session.input`의 쓰기는
//! blocking pool에서 완료를 기다리므로 요청을 보낸 뒤 잠시(풀 왕복 ~수백 µs,
//! tty가 막힌 플랫폼에선 750 ms) 진행 중으로 남는다. macOS는 읽지 않는 pty에도
//! MB 단위를 흡수해 스톨이 나지 않으므로, 예산(1)을 넘는 두 번째 입력을
//! 연속 발사하고 경쟁에서 지면(완료가 먼저 끝나면) 곧바로 다시 시도한다 —
//! 풀 지연이 프레임 처리보다 긴 것이 압도적이라 수 회 안에 관측된다.
//! 교차하는 응답은 wait_fired가 임시 보관하므로(common 참조) 회수 순서는
//! 자유롭다.

mod common;

use serde_json::json;
use std::time::Duration;

use common::{b64, uuid_v4, Client, DaemonProc};

fn shell_launch(args: &[&str]) -> serde_json::Value {
    json!({
        "request_id": common::uuid_v4(),
        "profile_id": common::uuid_v4(),
        "cwd": std::env::temp_dir().to_string_lossy(),
        "program": common::fixture_bin(),
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

struct LiveSession {
    session: String,
    epoch: String,
    workload: serde_json::Value,
}

fn launch_and_attach(control: &mut Client, tag: &str) -> LiveSession {
    let outcome = control
        .request(
            "workload.launch",
            shell_launch(&["memory", "--mib", "1", "--hold-ms", "60000"]),
        )
        .unwrap_or_else(|e| panic!("launch {tag}: {e}"));
    let session = outcome["session_id"].as_str().expect("session").to_string();
    let view = uuid_v4();
    let attach = control
        .request(
            "session.attach",
            json!({ "session_id": session, "view_id": view, "access": "writer" }),
        )
        .expect("attach");
    let epoch = attach["epoch"].as_str().expect("epoch").to_string();
    LiveSession {
        session,
        epoch,
        workload: outcome["workload_id"].clone(),
    }
}

fn input_params(live: &LiveSession, input_id: &str) -> serde_json::Value {
    json!({
        "session_id": live.session,
        "epoch": live.epoch,
        "input_id": input_id,
        "data_b64": b64(b"budget-probe"),
    })
}

/// 진행 중인 완료 하나에 겹쳐 보낸 입력이 예산 `BUSY`로 거절되는지 관측할
/// 때까지 반복한다. 세션 둘은 writer당 한 건 규칙(한 view의 동시 입력 1개)을
/// 피하려는 것 — 서로 다른 세션이면 두 입력이 연속해서 진행 중이 될 수 있다.
fn observe_budget_refusal(
    control: &mut Client,
    a: &LiveSession,
    b: &LiveSession,
) -> serde_json::Value {
    for attempt in 0..50 {
        let first = control.next_request_id();
        control.fire(
            "session.input",
            input_params(a, &format!("cap-a-{attempt}")),
        );
        let overlap = control.next_request_id();
        control.fire(
            "session.input",
            input_params(b, &format!("cap-b-{attempt}")),
        );
        let overlap_reply = control
            .wait_fired(&overlap, Duration::from_secs(10))
            .expect("overlap input response");
        let _ = control
            .wait_fired(&first, Duration::from_secs(10))
            .expect("first input response");
        if let Err(err) = overlap_reply {
            assert_eq!(err["code"], "BUSY", "attempt {attempt}: got {err}");
            assert!(
                err["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("too many pending")),
                "must be the budget refusal, got {err}"
            );
            // 거절은 dispatch 전이었다는 표식 — 클라이언트가 안심하고 같은
            // 입력을 다시 보내는 근거(02-runner §6).
            assert_eq!(err["details"]["reason_code"], "PENDING_BUDGET", "got {err}");
            assert!(
                err["details"]["retry_after_ms"]
                    .as_u64()
                    .is_some_and(|ms| ms > 0),
                "must carry a retry hint, got {err}"
            );
            return err;
        }
        // 경쟁에서 졌다(첫 완료가 두 번째 프레임보다 먼저 끝났다) — 다시 시도.
    }
    panic!("input budget refusal never observed in 50 attempts");
}

#[test]
fn input_budget_caps_rejects_before_dispatch_and_recovers() {
    let daemon = DaemonProc::spawn(
        "pending-budget",
        Some(json!({ "limits": { "max_pending_inputs": 1 } })),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let a = launch_and_attach(&mut control, "a");
    let b = launch_and_attach(&mut control, "b");

    // 1) 진행 중인 입력 위에 겹친 입력은 dispatch 전에 Busy로 돌아온다.
    let err = observe_budget_refusal(&mut control, &a, &b);
    assert_eq!(err["code"], "BUSY");

    // 2) resize 예산은 독립 — 입력 진행 중에도 resize는 예산 게이트에 걸리지
    //    않는다(resize 카운터만 센다).
    let first = control.next_request_id();
    control.fire("session.input", input_params(&a, "sep-input"));
    let resize_id = control.next_request_id();
    control.fire(
        "session.resize",
        json!({
            "session_id": a.session,
            "epoch": a.epoch,
            "resize_id": "rs-budget",
            "cols": 100,
            "rows": 30,
        }),
    );
    let resize = control
        .wait_fired(&resize_id, Duration::from_secs(5))
        .expect("resize response")
        .expect("resize must not eat the input budget");
    assert!(resize["applied_seq"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    let _ = control
        .wait_fired(&first, Duration::from_secs(10))
        .expect("input response");

    // 3) 완료가 빠지면 카운터도 빠진다 — 다음 입력은 예산 게이트를 지나간다.
    control
        .request("session.input", input_params(&a, "after-drain"))
        .expect("input after drain must pass the budget gate");

    let _ = control.request(
        "workload.cancel",
        json!({ "request_id": "cancel-a", "workload_id": a.workload }),
    );
    let _ = control.request(
        "workload.cancel",
        json!({ "request_id": "cancel-b", "workload_id": b.workload }),
    );
}
