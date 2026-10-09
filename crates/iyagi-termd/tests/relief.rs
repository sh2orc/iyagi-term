//! 압력 완화 P2 — 스케줄링 양보(spec `08-pressure-relief.md` §2).
//!
//! 계약:
//!
//! * CPU 압력이 WARNING 이상이면 포커스가 아닌 RUNNING 세션 전부가
//!   `relief.kind = "YIELDED"`가 되고, 포커스 세션은 `NONE`으로 남는다(§0-3),
//! * 그 양보는 진짜로 OS에 걸린다 — 관측된 트리의 pid마다 background 정책이
//!   읽힌다(§9: "적용 → 해제 → 원상 확인"을 한 시험 안에서),
//! * `session.relief restore`는 즉시 되돌리고, 압력이 남아 있는 동안
//!   다시 양보되지 않는다(수동 > 자동, §0-2),
//! * 포커스를 얻으면 압력과 무관하게 바로 복원된다,
//! * `relief.set_policy { auto_yield: false }`면 자동 양보는 일어나지 않는다.
//!
//! 압력은 호스트 부하에 맡길 수 없으므로 `IYAGI_TEST_CONFIG`의
//! `cpu_pressure.warning_used_percent = 0`으로 강제한다(측정만 되면 WARNING).

mod common;

use common::{launch_request, snapshot_workload, Client, DaemonProc};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// 텔레메트리 1 s 틱 + 2-sample 악화 + 완화 적용 — 넉넉히 기다린다.
const SETTLE: Duration = Duration::from_secs(20);
/// "일어나지 않아야 한다"를 확인하는 관찰 창.
const QUIET: Duration = Duration::from_secs(5);

/// 항상 WARNING으로 분류되는 압력 설정 + 느슨한 admission.
fn always_warning() -> Value {
    common::relaxed_admission(json!({
        "cpu_pressure": { "warning_used_percent": 0, "critical_used_percent": 200 },
    }))
}

/// 살아 있는 셸 하나(`echo` 픽스처는 stdin을 기다린다).
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

fn relief_kind(control: &mut Client, workload_id: &Value) -> String {
    snapshot_workload(control, workload_id)
        .and_then(|s| s["relief"]["kind"].as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("workload {workload_id} missing from the snapshot"))
}

fn protected(control: &mut Client, workload_id: &Value) -> bool {
    snapshot_workload(control, workload_id)
        .and_then(|s| s["protected"].as_bool())
        .unwrap_or_else(|| panic!("workload {workload_id} missing `protected`"))
}

fn wait_relief(control: &mut Client, workload_id: &Value, want: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = relief_kind(control, workload_id);
        if last == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    panic!(
        "workload {workload_id} relief never became {want:?} within {timeout:?}; last: {last:?}"
    );
}

/// `window` 동안 완화 상태가 `want`에서 벗어나지 않는지 확인한다.
fn stays(control: &mut Client, workload_id: &Value, want: &str, window: Duration) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        let now = relief_kind(control, workload_id);
        assert_eq!(now, want, "workload {workload_id} changed to {now:?}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// 이 워크로드가 지금 관측하고 있는 프로세스들의 pid(직접 셸은 PTY 루트
/// 트리, 관리 워크로드는 그룹 멤버).
fn observed_pids(control: &mut Client, workload_id: &Value) -> Vec<u32> {
    let page = control
        .request(
            "workload.processes",
            json!({ "workload_id": workload_id, "limit": 100 }),
        )
        .expect("workload.processes");
    page["processes"]
        .as_array()
        .expect("processes array")
        .iter()
        .filter_map(|p| p["identity"]["pid"].as_u64().map(|v| v as u32))
        .collect()
}

/// 시험 프로세스에서 직접 읽는 OS 상태(같은 uid이므로 읽을 수 있다).
/// macOS는 darwin background 정책 비트, Windows는 우선순위 클래스다.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn os_backgrounded(pid: u32) -> Option<bool> {
    use term_platform::group::scheduling::observed_tier;
    use term_platform::group::SchedulingTier;
    observed_tier(pid).map(|tier| tier == SchedulingTier::Background)
}

/// 관측된 pid 전부가 기대한 tier인지 — 최소 하나는 읽혀야 한다.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn assert_os_tier(control: &mut Client, workload_id: &Value, background: bool) {
    let pids = observed_pids(control, workload_id);
    assert!(
        !pids.is_empty(),
        "직접 셸도 관측 트리를 보고한다(08 §1.2): {workload_id}"
    );
    let mut read = 0usize;
    for pid in &pids {
        let Some(is_bg) = os_backgrounded(*pid) else {
            continue; // 그 사이 떠난 자손
        };
        read += 1;
        assert_eq!(
            is_bg, background,
            "pid {pid} of {workload_id}: background={is_bg}, expected {background}"
        );
    }
    assert!(read >= 1, "최소 한 프로세스의 정책은 읽혀야 한다: {pids:?}");
}

/// 남은 셸을 반드시 거둔다: 데몬은 SIGKILL로 내려가므로(테스트 하네스)
/// 종료 경로의 자동 복원이 돌지 않는다.
fn cleanup(control: &mut Client, workloads: &[&Value]) {
    for workload_id in workloads {
        let _ = control.request("workload.cancel", json!({ "workload_id": workload_id }));
    }
}

/// §2: WARNING이면 비포커스 세션이 양보되고, 포커스 세션은 그대로다.
/// 그리고 그 양보는 실제로 OS에 걸려 있다.
#[test]
fn pressure_yields_the_unfocused_session_and_spares_the_focused_one() {
    let daemon = DaemonProc::spawn("relief-yield", Some(always_warning()));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let (focused_w, focused_s) = live_shell(&mut control);
    let (background_w, _background_s) = live_shell(&mut control);
    control
        .request("session.focus", json!({ "session_id": focused_s }))
        .expect("focus");

    wait_relief(&mut control, &background_w, "YIELDED", SETTLE);
    assert_eq!(
        relief_kind(&mut control, &focused_w),
        "NONE",
        "보고 있는 pane은 절대 양보 대상이 아니다(§0-3)"
    );

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        assert_os_tier(&mut control, &background_w, true);
        assert_os_tier(&mut control, &focused_w, false);
    }

    // 상태는 안정적이다 — 매 틱 양보/복원을 반복하지 않는다.
    stays(
        &mut control,
        &background_w,
        "YIELDED",
        Duration::from_secs(3),
    );
    stays(&mut control, &focused_w, "NONE", Duration::from_secs(1));

    cleanup(&mut control, &[&focused_w, &background_w]);
}

/// §0-2: 수동 복원은 즉시 되돌리고, 압력이 남아 있는 동안 다시 양보되지
/// 않는다(`protected`). OS 상태도 원상으로 돌아간다(§9).
#[test]
fn a_manual_restore_undoes_the_yield_and_is_not_re_yielded_under_pressure() {
    let daemon = DaemonProc::spawn("relief-manual-restore", Some(always_warning()));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let (workload, session) = live_shell(&mut control);
    wait_relief(&mut control, &workload, "YIELDED", SETTLE);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    assert_os_tier(&mut control, &workload, true);

    let result = control
        .request(
            "session.relief",
            json!({ "session_id": session, "action": "restore" }),
        )
        .expect("manual restore");
    assert_eq!(result["relief"]["kind"], "NONE", "{result}");
    assert_eq!(
        result["protected"], true,
        "압력 중 수동 복원은 NORMAL까지 보호된다: {result}"
    );
    assert!(
        protected(&mut control, &workload),
        "스냅샷에도 보호가 보인다"
    );

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    assert_os_tier(&mut control, &workload, false);

    // 압력은 그대로지만 다시 걸리지 않는다.
    stays(&mut control, &workload, "NONE", QUIET);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    assert_os_tier(&mut control, &workload, false);

    cleanup(&mut control, &[&workload]);
}

/// §0-3: 포커스를 얻으면 압력이 남아 있어도 즉시 복원된다.
#[test]
fn focusing_a_yielded_session_restores_it() {
    let daemon = DaemonProc::spawn("relief-focus-restore", Some(always_warning()));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let (keep, keep_session) = live_shell(&mut control);
    let (target, target_session) = live_shell(&mut control);
    control
        .request("session.focus", json!({ "session_id": keep_session }))
        .expect("focus first");
    wait_relief(&mut control, &target, "YIELDED", SETTLE);

    // 포커스를 옮긴다: 이전 포커스는 양보 대상이 되고, 새 포커스는 풀린다.
    control
        .request("session.focus", json!({ "session_id": target_session }))
        .expect("move focus");
    wait_relief(&mut control, &target, "NONE", Duration::from_secs(8));
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    assert_os_tier(&mut control, &target, false);
    wait_relief(&mut control, &keep, "YIELDED", SETTLE);

    cleanup(&mut control, &[&keep, &target]);
}

/// 수동 양보·보호 표시(`session.relief`)는 압력과 무관하게 동작한다.
#[test]
fn manual_yield_and_protect_work_without_any_pressure() {
    // 자동은 꺼 두고(압력 기본값 그대로) 수동 경로만 본다.
    let daemon = DaemonProc::spawn(
        "relief-manual",
        Some(common::relaxed_admission(
            json!({ "relief": { "auto_yield": false } }),
        )),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let (workload, session) = live_shell(&mut control);

    let result = control
        .request(
            "session.relief",
            json!({ "session_id": session, "action": "yield" }),
        )
        .expect("manual yield");
    assert_eq!(result["relief"]["kind"], "YIELDED", "{result}");
    assert_eq!(result["relief"]["manual"], true, "{result}");
    assert_eq!(relief_kind(&mut control, &workload), "YIELDED");
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    assert_os_tier(&mut control, &workload, true);

    // 보호 표시는 양보 중인 세션도 되돌린다.
    let result = control
        .request(
            "session.relief",
            json!({ "session_id": session, "action": "protect" }),
        )
        .expect("protect");
    assert_eq!(result["relief"]["kind"], "NONE", "{result}");
    assert_eq!(result["protected"], true, "{result}");
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    assert_os_tier(&mut control, &workload, false);

    let result = control
        .request(
            "session.relief",
            json!({ "session_id": session, "action": "unprotect" }),
        )
        .expect("unprotect");
    assert_eq!(result["protected"], false, "{result}");

    // 모르는 세션은 정직하게 거절한다.
    let err = control
        .request(
            "session.relief",
            json!({ "session_id": common::uuid_v4(), "action": "yield" }),
        )
        .expect_err("unknown session refused");
    assert_eq!(err["code"], "INVALID_ARGUMENT");
    let err = control
        .request(
            "session.relief",
            json!({ "session_id": session, "action": "hibernate" }),
        )
        .expect_err("P2가 모르는 액션은 거절한다");
    assert_eq!(err["code"], "INVALID_ARGUMENT");

    cleanup(&mut control, &[&workload]);
}

/// `relief.set_policy { auto_yield: false }`면 압력이 있어도 자동 양보가
/// 일어나지 않는다. 스냅샷의 `relief_policy`가 현재 값을 그대로 광고한다.
#[test]
fn turning_auto_yield_off_stops_automatic_yields() {
    let daemon = DaemonProc::spawn("relief-policy", Some(always_warning()));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let snapshot = control
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    assert_eq!(
        snapshot["relief_policy"]["auto_yield"], true,
        "기본값은 켜짐(defaults.json relief.auto_yield)"
    );
    assert_eq!(
        snapshot["capabilities"]["scheduling_yield"]["support"], "supported",
        "이 플랫폼은 양보를 적용하고 되돌릴 수 있다: {}",
        snapshot["capabilities"]
    );

    let policy = control
        .request("relief.set_policy", json!({ "auto_yield": false }))
        .expect("set policy");
    assert_eq!(policy["auto_yield"], false, "{policy}");
    let snapshot = control
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    assert_eq!(snapshot["relief_policy"]["auto_yield"], false);

    // 정책을 끈 뒤에 뜬 세션은 압력이 있어도 끝까지 NONE이다.
    let (workload, _session) = live_shell(&mut control);
    stays(&mut control, &workload, "NONE", QUIET);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    assert_os_tier(&mut control, &workload, false);

    cleanup(&mut control, &[&workload]);
}
