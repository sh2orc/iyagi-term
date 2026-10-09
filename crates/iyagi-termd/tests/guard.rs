//! 자원 가드(08 §5) — 일시정지/재개 통합 시험.
//!
//! 계약:
//!
//! * `workload.suspend`는 관리 워크로드를 **진짜로** 정지시킨다 — 귀속
//!   cpu_cores가 0으로 떨어지는 것으로 증명한다(제품 자체 관측이 곧 시험
//!   프로브다),
//! * `workload.resume`은 다시 달리게 한다(사용량이 돌아온다),
//! * 수동 정지는 스냅샷에 `guard.kind = "SUSPENDED"`(`manual`)로 보인다.
//!
//! 이 백엔드가 일시정지를 지원하지 않으면(Windows R1) 건너뛴다 — 그때
//! 가드는 dormant여야 하고 이 시험은 침묵한다.

mod common;

use common::{launch_request, snapshot_workload, Client, DaemonProc};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// 귀속 사용량이 텔레메트리(1 s)를 타고 흐르는 여유.
const SETTLE: Duration = Duration::from_secs(20);

fn cpu_cores(control: &mut Client, workload_id: &Value) -> Option<f64> {
    snapshot_workload(control, workload_id).and_then(|s| {
        s["usage"]["cpu_cores"]["value"]
            .as_f64()
            .or_else(|| s["usage"]["cpu_cores"].as_f64())
    })
}

fn wait_cores(
    control: &mut Client,
    workload_id: &Value,
    want_busy: bool,
    timeout: Duration,
) -> f64 {
    let deadline = Instant::now() + timeout;
    let mut last = 0.0;
    while Instant::now() < deadline {
        if let Some(cores) = cpu_cores(control, workload_id) {
            last = cores;
            let ok = if want_busy { cores > 0.5 } else { cores < 0.2 };
            if ok {
                return cores;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    panic!(
        "workload {workload_id} cpu_cores never settled to {} within {timeout:?}; last: {last}",
        if want_busy { "busy" } else { "idle" }
    );
}

fn guard_kind(control: &mut Client, workload_id: &Value) -> String {
    snapshot_workload(control, workload_id)
        .and_then(|s| s["guard"]["kind"].as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("workload {workload_id} missing `guard`"))
}

#[test]
fn manual_suspend_stops_the_process_tree_and_resume_continues_it() {
    let daemon = DaemonProc::spawn("guard-manual", Some(common::relaxed_admission(json!({}))));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    // 이 백엔드가 일시정지를 지원하는가? 아니면 침묵히 건너뛴다.
    let caps = control
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    if caps["capabilities"]["suspend_resume"]["support"].as_str() != Some("supported") {
        eprintln!(
            "SKIP: suspend_resume unsupported ({:?})",
            caps["capabilities"]["suspend_resume"]
        );
        return;
    }

    let launch = control
        .request(
            "workload.launch",
            launch_request(
                "managed",
                &["cpu", "--workers", "1", "--duration-ms", "120000"],
                "1048576",
            ),
        )
        .expect("launch");
    let workload_id = launch["workload_id"].clone();
    // admission이 여유로워도 RUNNING 전환을 기다린다.
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline {
        if let Some(s) = snapshot_workload(&mut control, &workload_id) {
            if s["state"] == "RUNNING" {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    // 달리고 있다는 증거.
    wait_cores(&mut control, &workload_id, true, SETTLE);

    // 정지 → 사용량이 0으로 떨어진다.
    let suspended = control
        .request(
            "workload.suspend",
            json!({"request_id": common::uuid_v4(), "workload_id": workload_id}),
        )
        .expect("suspend rpc");
    assert_eq!(suspended["guard"]["kind"], "SUSPENDED");
    assert_eq!(suspended["guard"]["manual"], true);
    wait_cores(&mut control, &workload_id, false, SETTLE);
    assert_eq!(guard_kind(&mut control, &workload_id), "SUSPENDED");

    // 재개 → 다시 달린다.
    let resumed = control
        .request(
            "workload.resume",
            json!({"request_id": common::uuid_v4(), "workload_id": workload_id}),
        )
        .expect("resume rpc");
    assert_eq!(resumed["guard"]["kind"], "NONE");
    wait_cores(&mut control, &workload_id, true, SETTLE);

    let _ = control.request(
        "workload.cancel",
        json!({"request_id": common::uuid_v4(), "workload_id": workload_id, "force": true}),
    );
}
