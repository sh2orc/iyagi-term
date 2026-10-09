//! 세션 시작의 예외 조건을 실제 데몬·PTY로 띄워 보는 시험(04-ui §2-4, 01 §4).
//!
//! 데몬은 열 수 없는 요청을 사유와 함께 거절해 UI가 대체 경로·대체 셸로
//! 다시 열 수 있게 하고, 공간·자원 때문에 거절하던 경우는 받아들이는 쪽으로
//! 버틴다:
//! - 쓸 수 없는 시작 경로: `reason_code`(사라짐·디렉터리 아님·권한 없음)를
//!   싣고, 그 상위 경로로는 열린다.
//! - `env`로 감싼 셸이 지워졌으면 127로 끝나는 PTY 대신 PROGRAM_NOT_FOUND.
//! - 전역 저널 예산이 찼으면 새 셸을 열기 전에 끝난 세션의 저널부터 비운다.
//! - 호스트가 받을 수 없는 관리 실행 예약은 거절 대신 받을 수 있는 최대로 줄인다.

mod common;

use common::{launch_request, wait_workload_state, Client, DaemonProc};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

fn shell_at(cwd: &Path, program: &str, argv: &[&str]) -> Value {
    let mut request = launch_request("shell", argv, "2147483648");
    request["cwd"] = json!(cwd.to_string_lossy());
    request["program"] = json!(program);
    request
}

fn reason(err: &Value) -> &str {
    err["details"]["reason_code"].as_str().unwrap_or_default()
}

#[test]
fn unusable_cwd_is_rejected_with_a_reason_and_the_parent_still_opens() {
    let daemon = DaemonProc::spawn("launch-cwd", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let fixture = common::fixture_bin();
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    std::fs::create_dir(&project).unwrap();

    // 지워진 worktree: 사유 cwd_missing. UI는 상위 경로로 다시 연다.
    let gone = project.join("wt").join("feature");
    let err = control
        .request(
            "workload.launch",
            shell_at(&gone, &fixture, &["--hold-ms", "2000"]),
        )
        .expect_err("missing cwd");
    assert_eq!(err["code"], "CWD_UNAVAILABLE", "{err}");
    assert_eq!(reason(&err), "cwd_missing", "{err}");
    let opened = control
        .request(
            "workload.launch",
            shell_at(&project, &fixture, &["--hold-ms", "2000"]),
        )
        .expect("parent opens");
    assert_eq!(opened["state"], "RUNNING", "{opened}");

    // 파일을 경로로 받으면 cwd_not_directory.
    let file = project.join("notes.txt");
    std::fs::write(&file, b"x").unwrap();
    let err = control
        .request(
            "workload.launch",
            shell_at(&file, &fixture, &["--hold-ms", "10"]),
        )
        .expect_err("file cwd");
    assert_eq!(reason(&err), "cwd_not_directory", "{err}");

    // 들어갈 수 없는 디렉터리: 예전에는 경로 검사를 통과한 뒤 자식의 chdir이
    // 실패해 SPAWN_FAILED로 보였다 — 이제 경로 문제로 거절돼 대체 경로를 탄다.
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        use std::os::unix::fs::PermissionsExt as _;
        let locked = project.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
        let result = control.request(
            "workload.launch",
            shell_at(&locked, &fixture, &["--hold-ms", "10"]),
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        let err = result.expect_err("locked cwd");
        assert_eq!(err["code"], "CWD_UNAVAILABLE", "{err}");
        assert_eq!(reason(&err), "cwd_permission_denied", "{err}");
    }
}

#[cfg(unix)]
#[test]
fn env_wrapped_missing_shell_is_program_not_found_instead_of_a_dead_pane() {
    let daemon = DaemonProc::spawn("launch-env-shell", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let cwd = std::env::temp_dir();

    // UI가 보내는 모양 그대로: env -u … <셸> -l. 셸이 지워졌다.
    let err = control
        .request(
            "workload.launch",
            shell_at(
                &cwd,
                "/usr/bin/env",
                &["-u", "NO_COLOR", "/opt/gone/bin/fish", "-l"],
            ),
        )
        .expect_err("missing wrapped shell");
    assert_eq!(err["code"], "PROGRAM_NOT_FOUND", "{err}");
    assert_eq!(reason(&err), "wrapped_program_missing", "{err}");
    assert_eq!(err["details"]["program"], "/opt/gone/bin/fish", "{err}");

    // 대체 셸(UI의 다음 후보)은 그대로 열린다.
    let fixture = common::fixture_bin();
    let opened = control
        .request(
            "workload.launch",
            shell_at(
                &cwd,
                "/usr/bin/env",
                &["-u", "NO_COLOR", &fixture, "--hold-ms", "2000"],
            ),
        )
        .expect("wrapped existing program opens");
    assert_eq!(opened["state"], "RUNNING", "{opened}");
}

/// 저널 디렉터리에서 한 세션의 런(활성 파일 + 닫힌 세그먼트) 바이트 합.
fn journal_run_bytes(journals: &Path, session: &str) -> u64 {
    std::fs::read_dir(journals)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(session))
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

#[test]
fn a_full_journal_budget_is_relieved_before_a_new_shell_opens() {
    const BUDGET: u64 = 64 * 1024;
    let daemon = DaemonProc::spawn(
        "launch-journal-full",
        Some(json!({"limits": {
            "journal_global_bytes": BUDGET,
            "journal_session_bytes": BUDGET,
            "journal_segment_bytes": 4096,
        }})),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let journals = daemon.data_dir.join("data/journals");

    // A: 전역 예산을 거의 다 채우고 끝난다(롤링이 자기 세그먼트만 지운다).
    let first = control
        .request(
            "workload.launch",
            launch_request(
                "shell",
                &[
                    "flood", "--bytes", "1048576", "--chunk", "1024", "--seed", "3",
                ],
                "1048576",
            ),
        )
        .expect("launch A");
    let first_session = first["session_id"].as_str().unwrap().to_string();
    wait_workload_state(
        &mut control,
        &first["workload_id"],
        &["SUCCEEDED", "FAILED"],
        Duration::from_secs(30),
    );
    let held = journal_run_bytes(&journals, &first_session);
    assert!(
        held * 10 >= BUDGET * 9,
        "A should hold >= 90% of the budget, held {held}"
    );

    // B: 열기 전에 끝난 A의 저널이 비워져 B의 출력이 온전히 기록된다. 예전에는
    // 남은 틈(< 한 세그먼트)만 쓸 수 있어 B의 기록이 몇 KiB에서 멈췄다.
    let second = control
        .request(
            "workload.launch",
            launch_request(
                "shell",
                &[
                    "flood",
                    "--bytes",
                    "24576",
                    "--chunk",
                    "1024",
                    "--seed",
                    "4",
                    "--hold-ms",
                    "3000",
                ],
                "1048576",
            ),
        )
        .expect("launch B with a full journal budget");
    assert_eq!(second["state"], "RUNNING", "{second}");
    let second_session = second["session_id"].as_str().unwrap().to_string();
    assert_eq!(
        journal_run_bytes(&journals, &first_session),
        0,
        "A's finished journal was evicted"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let recorded = journal_run_bytes(&journals, &second_session);
        if recorded >= 24 * 1024 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "B's output stalled at {recorded} bytes"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let summary = common::snapshot_workload(&mut control, &second["workload_id"]).expect("B");
    assert_ne!(summary["last_error_code"], "JOURNAL_LIMIT", "{summary}");
}

#[test]
fn oversized_managed_request_is_fitted_to_the_host_not_refused() {
    let daemon = DaemonProc::spawn("launch-managed-fit", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    // 1 PiB 예약·1024 CPU 슬롯: 어떤 호스트도 받을 수 없는 크기다.
    let mut request = launch_request("managed", &["--hold-ms", "500"], "1125899906842624");
    request["policy"]["cpu_slots"] = json!(1024);
    let outcome = control
        .request("workload.launch", request)
        .expect("fitted instead of RESOURCE_UNSCHEDULABLE");
    let state = outcome["state"].as_str().unwrap_or_default();
    assert!(
        ["QUEUED", "STARTING", "RUNNING"].contains(&state),
        "{outcome}"
    );
    let policy = &outcome["effective_policy"];
    let reserved: u64 = policy["reservation_bytes"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(reserved > 0 && reserved < 1125899906842624, "{policy}");
    let slots = policy["cpu_slots"].as_u64().unwrap();
    assert!((1..1024).contains(&slots), "{policy}");

    // 대기열에 남았어도 그 사유는 "받을 수 없음"이 아니다.
    if let Some(summary) = common::snapshot_workload(&mut control, &outcome["workload_id"]) {
        assert_ne!(
            summary["queue_reason"], "RESOURCE_UNSCHEDULABLE",
            "{summary}"
        );
    }
    let _ = control.request(
        "workload.cancel",
        json!({"request_id": common::uuid_v4(), "workload_id": outcome["workload_id"], "force": true}),
    );
}

#[test]
fn oversized_managed_request_starts_with_the_headroom_the_host_has() {
    // 예산을 메모리 전체로 넓힌 호스트(relaxed)에서 1 PiB 요청: 예산 전체로 맞추면 여유
    // 검사가 "가용 메모리 ≥ 전체 메모리"를 요구해 대기열에서 영영 나오지 못한다. 지금의
    // 여유만큼으로 맞춰 곧바로 시작해야 한다.
    let daemon = DaemonProc::spawn(
        "launch-managed-fit-headroom",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let outcome = control
        .request(
            "workload.launch",
            launch_request("managed", &["echo"], "1125899906842624"),
        )
        .expect("fitted instead of RESOURCE_UNSCHEDULABLE");
    common::ensure_running(&mut control, &outcome, Duration::from_secs(15));
    let summary =
        common::snapshot_workload(&mut control, &outcome["workload_id"]).expect("summary");
    let reserved: u64 = summary["reservation_bytes"]
        .as_str()
        .expect("reservation_bytes")
        .parse()
        .unwrap();
    assert!(reserved < 1125899906842624, "{summary}");
    let _ = control.request(
        "workload.cancel",
        json!({"request_id": common::uuid_v4(), "workload_id": outcome["workload_id"], "force": true}),
    );
}

#[test]
fn managed_launch_right_after_startup_waits_for_telemetry_instead_of_refusing() {
    let daemon = DaemonProc::spawn(
        "launch-managed-early",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    // 기동 직후의 관리 실행은 받아들여져 곧 시작한다. 첫 호스트 표본 전이면
    // WAIT_TELEMETRY로 잠깐 기다린다 — 표본이 없는 호스트를 "0 바이트"로 보고
    // RESOURCE_UNSCHEDULABLE로 거절하지 않는다는 판정 자체는 term-core의
    // `unknown_host_total_is_not_a_zero_budget`가 고정한다(여기서는 표본이
    // 이미 와 있을 수 있어 그 창을 결정적으로 만들 수 없다).
    let outcome = control
        .request(
            "workload.launch",
            launch_request("managed", &["--hold-ms", "500"], "268435456"),
        )
        .expect("early managed launch is accepted");
    common::ensure_running(&mut control, &outcome, Duration::from_secs(15));
}
