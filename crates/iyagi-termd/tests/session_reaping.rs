//! 종료 세션 회수(메모리 누수 L1/L2/L9).
//!
//! 세션 레지스트리는 예전에 삽입 전용이었다 — 한 번 실행된 세션은
//! `SessionEntry`(저널 오프셋 맵·흐름 제어 상태·저널 쓰기 핸들)를 데몬이
//! 죽을 때까지 붙들었다. 이제는:
//!
//! * 종료한 세션은 보관 링([`FINALIZED_SESSIONS_RETAINED`]개)에 들어가
//!   그동안은 재attach·재생이 가능하고,
//! * 링에서 밀려나면 뷰가 없는 즉시 레지스트리에서 사라지며, 나중에
//!   attach하면 보관된 저널에서 읽기 전용 세션을 복원한다,
//! * 뷰가 아직 붙어 있으면 마지막 detach가 회수를 맡고,
//! * 저널 파일은 종료 시점에 finalize(flush)되어 그대로 남는다 — 삭제는
//!   retention의 몫이다(`journal_retention_days`).

mod common;

use common::{launch_request, wait_workload_state, Client, DaemonProc};
use serde_json::json;
use std::time::Duration;

/// `iyagi_termd_lib::state::FINALIZED_SESSIONS_RETAINED`와 같은 값 —
/// 이 시험이 그 계약을 고정한다.
const RETAINED: usize = 32;

/// 워크로드 하나가 종료 상태에 닿기까지 허용하는 시간.
const SETTLE: Duration = Duration::from_secs(30);

fn journal_path(daemon: &DaemonProc, session: &str) -> std::path::PathBuf {
    daemon
        .data_dir
        .join("data/journals")
        .join(format!("{session}.mtj"))
}

/// `iyagi_termd_lib::state::FINISHED_WORKLOADS_RETAINED`와 같은 값.
const FINISHED_RETAINED: usize = 64;

/// 셸 하나를 띄우고 종료(SUCCEEDED)까지 기다린 뒤 `(workload_id, session_id)`.
fn run_short_shell(control: &mut Client, argv: &[&str]) -> (String, String) {
    let launch = control
        .request("workload.launch", launch_request("shell", argv, "1048576"))
        .expect("shell launch");
    assert_eq!(launch["state"], "RUNNING", "shell launches never queue");
    let session = launch["session_id"].as_str().expect("session").to_string();
    let workload = launch["workload_id"]
        .as_str()
        .expect("workload")
        .to_string();
    wait_workload_state(control, &launch["workload_id"], &["SUCCEEDED"], SETTLE);
    (workload, session)
}

#[test]
fn finished_sessions_are_reaped_once_the_retained_ring_overflows() {
    let daemon = DaemonProc::spawn(
        "session-reaping",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    // (1) 출력이 있는 짧은 세션 하나. 종료 직후에도 저널은 온전해야 한다
    //     (종료 시점 finalize/flush — 그러지 않으면 마지막 버퍼가 영영
    //     디스크에 닿지 않고 fd도 열린 채 남는다).
    let (_, kept) = run_short_shell(
        &mut control,
        &["flood", "--bytes", "8192", "--chunk", "2048", "--seed", "9"],
    );
    let kept_journal = journal_path(&daemon, &kept);
    let scanned = term_pty::journal::JournalReader::open(&kept_journal)
        .expect("종료된 세션의 저널은 읽을 수 있어야 한다");
    assert_eq!(
        scanned.status(),
        term_pty::journal::ScanStatus::Ok,
        "종료 시점에 저널이 finalize(flush)된다 — 꼬리가 잘리지 않는다"
    );
    assert!(
        scanned.last_seq() >= 2,
        "초기 크기 + 출력 레코드가 디스크에 있다 (last_seq={})",
        scanned.last_seq()
    );

    // (2) 종료했지만 아직 보관 링 안이다: 재attach(재생)가 동작한다.
    let view_a = common::uuid_v4();
    let attach = control
        .request(
            "session.attach",
            json!({"session_id": kept, "view_id": view_a, "access": "reader"}),
        )
        .expect("보관 중인 종료 세션에는 붙을 수 있다");
    assert_eq!(attach["replay_from_seq"], "1", "R1은 처음부터 재생한다");

    // (3) 링을 넘칠 만큼 세션을 더 돌린다. `kept`은 링에서 밀려나지만
    //     뷰가 붙어 있으므로 아직 회수되지 않는다.
    let mut later = Vec::new();
    for _ in 0..(RETAINED + 2) {
        later.push(run_short_shell(&mut control, &["exit", "--code", "0"]).1);
    }

    // 뷰가 살아 있는 동안에는 레지스트리에 남아 있다(두 번째 뷰도 붙는다 —
    // 세션당 뷰 상한은 2다).
    let view_b = common::uuid_v4();
    control
        .request(
            "session.attach",
            json!({"session_id": kept, "view_id": view_b, "access": "reader"}),
        )
        .expect("뷰가 붙어 있는 종료 세션은 링 밖이어도 살아 있다");

    // (4) 마지막 뷰가 떠나는 순간 회수된다 — 그 전에는 아니다.
    control
        .request(
            "session.detach",
            json!({"session_id": kept, "view_id": view_a}),
        )
        .expect("detach 1");
    let view_c = common::uuid_v4();
    control
        .request(
            "session.attach",
            json!({"session_id": kept, "view_id": view_c, "access": "reader"}),
        )
        .expect("뷰가 하나라도 남아 있으면 회수되지 않는다");
    control
        .request(
            "session.detach",
            json!({"session_id": kept, "view_id": view_b}),
        )
        .expect("detach 2");
    control
        .request(
            "session.detach",
            json!({"session_id": kept, "view_id": view_c}),
        )
        .expect("detach 3 (마지막 뷰)");

    let restored = control
        .request(
            "session.attach",
            json!({"session_id": kept, "view_id": common::uuid_v4(), "access": "reader"}),
        )
        .expect("회수된 세션은 보관된 저널에서 복원한다");
    assert_eq!(restored["exited"], true);

    // (5) 보관 링 안의 최근 세션은 여전히 붙을 수 있다 — 회수는 무차별이
    //     아니라 "링에서 밀려난 것"만 대상으로 한다.
    let recent = later.last().expect("filler sessions").clone();
    control
        .request(
            "session.attach",
            json!({"session_id": recent, "view_id": common::uuid_v4(), "access": "reader"}),
        )
        .expect("최근 종료 세션은 대기열 서랍에서 붙을 수 있어야 한다");

    // (6) 회수는 저널을 지우지 않는다 — 삭제는 retention의 몫이다.
    assert!(
        kept_journal.is_file(),
        "회수된 세션의 저널 파일은 남는다(retention이 보존 기간 뒤에 지운다)"
    );
    let scanned = term_pty::journal::JournalReader::open(&kept_journal).expect("scan after reap");
    assert_eq!(scanned.status(), term_pty::journal::ScanStatus::Ok);

    // (7) 저널을 가진 세션이 33개 넘게 돌았어도 스냅샷은 계속 답한다.
    let snapshot = control
        .request("system.snapshot", json!({}))
        .expect("snapshot stays inside the frame budget");
    assert!(snapshot["workloads"].is_array());
}

/// L4: 종료 워크로드 레지스트리도 유계다. 스냅샷은 매번 레지스트리 전체를
/// 직렬화하므로(01 §2, 64 KiB 프레임 예산) 삽입 전용 맵은 수백 번 실행한
/// 뒤 `system.snapshot`을 영구 BUSY로 만들었다. 이력 조회는 스토리지가
/// 답한다 — 메모리 레지스트리는 살아 있는 것과 "최근"만 담는다.
#[test]
fn terminal_workloads_are_evicted_from_the_registry_ring() {
    let daemon = DaemonProc::spawn("workload-ring", Some(common::relaxed_admission(json!({}))));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let (oldest, _) = run_short_shell(&mut control, &["exit", "--code", "0"]);
    let mut newest = String::new();
    for _ in 0..(FINISHED_RETAINED + 2) {
        newest = run_short_shell(&mut control, &["exit", "--code", "0"]).0;
    }

    let snapshot = control
        .request("system.snapshot", json!({}))
        .expect("snapshot never grows past the frame budget");
    let list = snapshot["workloads"].as_array().expect("workloads array");
    assert!(
        list.len() <= FINISHED_RETAINED + 2,
        "레지스트리는 유계여야 한다(현재 {}건)",
        list.len()
    );
    assert!(
        !list.iter().any(|w| w["workload_id"] == oldest.as_str()),
        "가장 오래된 종료 워크로드는 축출된다"
    );
    assert!(
        list.iter().any(|w| w["workload_id"] == newest.as_str()),
        "최근 종료 워크로드는 남는다"
    );
}
