//! Shell-mode journal cap (02-runner §5, rolling journal): a 64 KiB cap
//! under a 16 MiB flood never stops PTY reading, never surfaces
//! `JOURNAL_LIMIT`, and never ends the session on its own — the oldest
//! segments are deleted instead and the on-disk run stays near the cap.
//!
//! Regression kept from the pre-rolling days: a journal error used to be
//! treated like EOF and started the drain window, which `kill()`ed a live
//! shell root two seconds after the cap. There is no journal error to
//! mis-handle any more, but the root must still be the one to decide when
//! the session ends.

mod common;

use common::{launch_request, wait_workload_state, Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

const CAP_BYTES: u64 = 65536;

#[test]
fn shell_journal_cap_rolls_instead_of_stopping() {
    let daemon = DaemonProc::spawn(
        "cap-shell",
        Some(common::relaxed_admission(
            json!({"limits": {"journal_session_bytes": CAP_BYTES}}),
        )),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = control
        .request(
            "workload.launch",
            launch_request(
                "shell",
                &[
                    "flood", "--bytes", "16777216", "--chunk", "16384", "--seed", "5",
                ],
                "1048576",
            ),
        )
        .expect("launch flood shell");
    common::ensure_running(&mut control, &launch, Duration::from_secs(15));
    let session = launch["session_id"].as_str().expect("session").to_string();
    let workload_id = launch["workload_id"].clone();
    let journal = daemon
        .data_dir
        .join("data/journals")
        .join(format!("{session}.mtj"));

    // Watch the journal roll: the head must move past seq 1 while the
    // workload keeps RUNNING (or finishes on its own) with no cap error.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut saw_trim = false;
    loop {
        let summary = common::snapshot_workload(&mut control, &workload_id).expect("workload");
        assert_ne!(
            summary["last_error_code"], "JOURNAL_LIMIT",
            "the cap must roll, not stop: {summary}"
        );
        if let Ok(set) = term_pty::segments::JournalSet::open(&journal) {
            if set.first_seq() > 1 {
                saw_trim = true;
            }
        }
        let state = summary["state"].as_str().unwrap_or("");
        if saw_trim && (state == "RUNNING" || state == "SUCCEEDED") {
            break;
        }
        if matches!(state, "SUCCEEDED" | "FAILED" | "CANCELLED") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "journal head must move under the flood: {summary}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        saw_trim,
        "a 16 MiB flood under a 64 KiB cap must trim the head"
    );

    // Past the old 2 s drain window the session is still what the root
    // decides: the cap itself never ends it.
    std::thread::sleep(Duration::from_millis(3000));
    let summary = common::snapshot_workload(&mut control, &workload_id)
        .expect("rolling shell workload stays listed");
    assert_ne!(summary["last_error_code"], "JOURNAL_LIMIT", "{summary}");
    assert!(
        matches!(summary["state"].as_str(), Some("RUNNING" | "SUCCEEDED")),
        "the cap must not fail or cancel a shell session: {summary}"
    );

    // Retention stays near the cap once the flood is done.
    let terminal = if summary["state"] == "RUNNING" {
        control
            .request(
                "workload.cancel",
                json!({"request_id": common::uuid_v4(), "workload_id": workload_id}),
            )
            .expect("cancel works");
        wait_workload_state(
            &mut control,
            &workload_id,
            &["CANCELLED", "SUCCEEDED"],
            Duration::from_secs(25),
        )
    } else {
        summary
    };
    assert_ne!(terminal["last_error_code"], "JOURNAL_LIMIT", "{terminal}");
    let on_disk = term_pty::segments::journal_files_bytes(&journal);
    let segment =
        term_pty::journal::segment_target_for(CAP_BYTES, term_pty::journal::DEFAULT_SEGMENT_BYTES);
    assert!(
        on_disk <= CAP_BYTES + segment + 32 * 1024,
        "on-disk run must stay near the 64 KiB cap (got {on_disk})"
    );
}
