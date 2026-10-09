//! `retention.set_limit`: 상한은 살아 있는 저널 writer에 걸려야 한다.
//!
//! 예전엔 `SessionEntry.journal_limit`(아무도 읽지 않는 원자값)만 바꾸고
//! 성공을 답했다 — writer는 예전 cap에서 계속 거부했다(무효 no-op). 롤링
//! 저널(02-runner §5)에서 상한은 "보존 창"이다: 올리면 더 쌓이고, 내리면
//! 가장 오래된 세그먼트를 즉시 지운다. 종료한 세션의 상한 변경은 여전히
//! INVALID_STATE다(되살릴 writer가 없다).

mod common;

use common::{b64, launch_request, wait_workload_state, Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

const SETTLE: Duration = Duration::from_secs(30);

fn journal_path(daemon: &DaemonProc, session: &str) -> std::path::PathBuf {
    daemon
        .data_dir
        .join("data/journals")
        .join(format!("{session}.mtj"))
}

/// 짧은 줄을 계속 흘려(canonical tty의 줄 길이 한계를 피한다) 저널 런이
/// `min_bytes`를 넘을 때까지 기다린다. writer는 250 ms 주기로 flush한다.
fn feed_until(
    control: &mut Client,
    session: &str,
    epoch: &str,
    path: &std::path::Path,
    min_bytes: u64,
    tag: &str,
) -> (u64, usize) {
    let line = format!("{}\n", "x".repeat(100));
    let deadline = Instant::now() + SETTLE;
    let mut bytes = 0;
    let mut n = 0;
    while Instant::now() < deadline && bytes <= min_bytes {
        for _ in 0..10 {
            control
                .request(
                    "session.input",
                    json!({
                        "session_id": session,
                        "epoch": epoch,
                        "input_id": format!("{tag}-in-{n}"),
                        "data_b64": b64(line.as_bytes()),
                    }),
                )
                .expect("input accepted");
            n += 1;
        }
        std::thread::sleep(Duration::from_millis(300));
        bytes = term_pty::segments::journal_files_bytes(path);
    }
    (bytes, n)
}

/// 8 KiB cap → 1 MiB로 올린 뒤 echo로 흘리면 저널 런이 옛 cap을 넘어
/// 자라고(더 이상 잘리지 않는다) 워크로드는 RUNNING을 유지한다.
#[test]
fn raised_limit_applies_to_the_live_journal_writer() {
    let daemon = DaemonProc::spawn(
        "retention-limit",
        Some(common::relaxed_admission(
            json!({"limits": {"journal_session_bytes": 8192}}),
        )),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = control
        .request(
            "workload.launch",
            launch_request("shell", &["echo"], "1048576"),
        )
        .expect("echo shell launch");
    assert_eq!(launch["state"], "RUNNING");
    let workload_id = launch["workload_id"].clone();
    let session = launch["session_id"].as_str().expect("session").to_string();

    let attach = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": common::uuid_v4(), "access": "writer"}),
        )
        .expect("writer attach");
    let epoch = attach["epoch"].as_str().expect("epoch").to_string();

    let raised = control
        .request(
            "retention.set_limit",
            json!({"session_id": session, "max_bytes": 1_048_576}),
        )
        .expect("raising the cap on a live session succeeds");
    assert_eq!(raised["max_bytes"], 1_048_576);

    let path = journal_path(&daemon, &session);
    let (bytes, n) = feed_until(&mut control, &session, &epoch, &path, 8192, "limit");
    assert!(
        bytes > 8192,
        "저널 런이 옛 cap(8192)을 넘어 자라야 한다: bytes={bytes} after {n} lines"
    );
    let summary = common::snapshot_workload(&mut control, &workload_id).expect("workload");
    assert_eq!(summary["state"], "RUNNING", "{summary}");
    assert_ne!(
        summary["last_error_code"], "JOURNAL_LIMIT",
        "롤링 저널은 cap에 걸리지 않는다: {summary}"
    );

    control
        .request("workload.cancel", json!({"workload_id": workload_id}))
        .expect("cancel");
    wait_workload_state(&mut control, &workload_id, &["CANCELLED"], SETTLE);
}

/// 상한을 내리면 가장 오래된 세그먼트가 즉시 지워지고(보존 창 축소), 그
/// 뒤 attach는 잘린 헤드부터 재생한다고 알린다.
#[test]
fn lowered_limit_trims_the_live_journal_immediately() {
    let daemon = DaemonProc::spawn(
        "retention-limit-lower",
        Some(common::relaxed_admission(
            json!({"limits": {"journal_session_bytes": 1_048_576}}),
        )),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = control
        .request(
            "workload.launch",
            launch_request("shell", &["echo"], "1048576"),
        )
        .expect("echo shell launch");
    let workload_id = launch["workload_id"].clone();
    let session = launch["session_id"].as_str().expect("session").to_string();
    let attach = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": common::uuid_v4(), "access": "writer"}),
        )
        .expect("writer attach");
    let epoch = attach["epoch"].as_str().expect("epoch").to_string();

    let path = journal_path(&daemon, &session);
    let (bytes, n) = feed_until(&mut control, &session, &epoch, &path, 48 * 1024, "lower");
    assert!(
        bytes > 48 * 1024,
        "저널 런이 48 KiB를 넘어야 한다: bytes={bytes} after {n} lines"
    );
    assert_eq!(
        term_pty::segments::JournalSet::open(&path)
            .expect("run scans")
            .first_seq(),
        1,
        "1 MiB 상한 아래에서는 아직 잘리지 않는다"
    );

    let lowered = control
        .request(
            "retention.set_limit",
            json!({"session_id": session, "max_bytes": 16384}),
        )
        .expect("lowering the cap on a live session succeeds");
    assert_eq!(lowered["max_bytes"], 16384);

    // 즉시(다음 flush 안에) 보존 런이 새 상한 근처로 줄어든다. 부하가 큰
    // 시험 환경을 감안해 넉넉히 기다리되, 실패 문구에 파일 상태를 남긴다.
    let deadline = Instant::now() + Duration::from_secs(15);
    let (on_disk, set) = loop {
        let on_disk = term_pty::segments::journal_files_bytes(&path);
        if on_disk <= 16384 + 4096 {
            if let Ok(set) = term_pty::segments::JournalSet::open(&path) {
                break (on_disk, set);
            }
        }
        assert!(
            Instant::now() < deadline,
            "lowering the limit must trim the run right away (still {on_disk} bytes; files={:?})",
            term_pty::segments::journal_files(&path)
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        set.first_seq() > 1,
        "head must have moved: {:?}",
        set.head()
    );
    eprintln!("lowered: on_disk={on_disk} first_seq={}", set.first_seq());

    let summary = common::snapshot_workload(&mut control, &workload_id).expect("workload");
    assert_eq!(summary["state"], "RUNNING", "{summary}");
    assert_ne!(summary["last_error_code"], "JOURNAL_LIMIT", "{summary}");

    // A later attach starts at the trimmed head and reports what is gone.
    let reader = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": common::uuid_v4(), "access": "reader"}),
        )
        .expect("reader attach");
    let replay_from: u64 = reader["replay_from_seq"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .expect("replay_from_seq");
    assert_eq!(replay_from, set.first_seq(), "{reader}");
    let dropped: u64 = reader["replay_dropped_bytes"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .expect("replay_dropped_bytes after a trim");
    assert!(dropped > 0, "{reader}");

    control
        .request("workload.cancel", json!({"workload_id": workload_id}))
        .expect("cancel");
    wait_workload_state(&mut control, &workload_id, &["CANCELLED"], SETTLE);
}

/// 종료(actor finalized)한 세션의 상한 변경은 무효다 — 성공을 답하지 않는다.
#[test]
fn set_limit_on_a_finished_session_is_invalid_state() {
    let daemon = DaemonProc::spawn(
        "retention-limit-finished",
        Some(common::relaxed_admission(json!({}))),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = control
        .request(
            "workload.launch",
            launch_request("shell", &["exit", "--code", "0"], "1048576"),
        )
        .expect("shell launch");
    let session = launch["session_id"].as_str().expect("session").to_string();
    wait_workload_state(&mut control, &launch["workload_id"], &["SUCCEEDED"], SETTLE);

    let err = control
        .request(
            "retention.set_limit",
            json!({"session_id": session, "max_bytes": 1_048_576}),
        )
        .expect_err("a stopped journal cannot be raised");
    assert_eq!(err["code"], "INVALID_STATE", "{err}");
}
