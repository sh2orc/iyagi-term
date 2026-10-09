//! 직접 셸 세션의 자원 관측(spec `08-pressure-relief.md` §1, `04-ui.md`:
//! "direct shell workload는 session 단위 자원 관측이다").
//!
//! 셸은 자원 그룹 없이 PTY로 바로 뜬다(02-runner §3) — 예전에는 그래서
//! 세션 pane의 자원 칸이 영영 비어 있었다. 이제 텔레메트리 루프가 PTY 루트
//! pid의 프로세스 트리를 훑어 `usage`를 채운다. 커버리지는 그룹이 아니라
//! `observed_tree`이며, 이 숫자는 관리 예약 합(03 §3)에 절대 더해지지 않는다.

mod common;

use common::{launch_request, snapshot_workload, Client, DaemonProc};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// 텔레메트리 틱은 1 s, 차분값은 두 번째 폴부터 — 넉넉히 기다린다.
const OBSERVE: Duration = Duration::from_secs(8);

fn wait_usage(control: &mut Client, workload_id: &Value, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    let mut last: Option<Value> = None;
    while Instant::now() < deadline {
        if let Some(summary) = snapshot_workload(control, workload_id) {
            if summary["usage"].is_object() {
                return summary["usage"].clone();
            }
            last = Some(summary);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("shell workload never reported usage within {timeout:?}; last summary: {last:?}");
}

#[test]
fn a_direct_shell_session_reports_observed_tree_usage() {
    let daemon = DaemonProc::spawn("shell-usage", Some(common::relaxed_admission(json!({}))));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    // stdin을 기다리는 픽스처: 관측하는 동안 살아 있다.
    let launch = control
        .request(
            "workload.launch",
            launch_request("shell", &["echo"], "1048576"),
        )
        .expect("shell launch");
    assert_eq!(launch["state"], "RUNNING");
    let workload_id = launch["workload_id"].clone();

    let usage = wait_usage(&mut control, &workload_id, OBSERVE);
    assert_eq!(
        usage["workload_id"], workload_id,
        "usage는 이 워크로드의 것이다"
    );
    assert_eq!(
        usage["coverage"], "observed_tree",
        "셸은 그룹이 없으므로 관찰된 트리 커버리지다: {usage}"
    );
    let process_count = usage["process_count"]["value"]
        .as_u64()
        .unwrap_or_else(|| panic!("process_count는 측정값이어야 한다: {usage}"));
    assert!(
        process_count >= 1,
        "최소한 PTY 루트 하나는 보인다 (count={process_count})"
    );
    assert_eq!(usage["process_count"]["quality"], "measured");
    // RSS는 관찰된 프로세스의 합(공유 페이지 중복 계산 때문에 estimated).
    assert!(
        usage["resident_bytes"]["value"].is_string(),
        "관찰된 프로세스가 있으면 RSS는 숫자다(문자열 u64): {usage}"
    );
}
