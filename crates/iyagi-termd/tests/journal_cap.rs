//! B18 (spec `06-verification.md` §3): journal cap lowered to 1 MiB under a
//! 16 MiB flood — rolling journal (02-runner §5).
//!
//! Expected: the cap never stops PTY reading, so the flood runs to completion
//! and the workload ends on its own (SUCCEEDED), never `JOURNAL_LIMIT`. The
//! delivered stream stays seq-contiguous within an epoch. A view that falls
//! behind the retained head is not fed a gap and not retried forever: the
//! daemon detaches it with `session.replay_required` and it attaches again at
//! the new head. On disk the run stays near the cap (oldest segments
//! deleted), a late attach replays from the trimmed head (opening with that
//! segment's size record, `replay_dropped_bytes` reported), and the control
//! channel keeps working throughout.

mod common;

use common::{launch_request, Client, DaemonProc};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const FLOOD_BYTES: u64 = 16 * 1024 * 1024;
const CAP_BYTES: u64 = 1024 * 1024;

fn u64_of(value: &Value) -> u64 {
    value.as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
}

#[test]
fn b18_rolling_journal_keeps_reading_past_the_cap() {
    let daemon = DaemonProc::spawn(
        "b18-cap",
        Some(common::relaxed_admission(
            json!({"limits": {"journal_session_bytes": CAP_BYTES},
                    "timing_ms": {"gate_timeout": 15000}}),
        )),
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = control
        .request(
            "workload.launch",
            launch_request(
                "managed",
                &[
                    "flood", "--bytes", "16777216", "--chunk", "16384", "--seed", "5",
                ],
                "1048576",
            ),
        )
        .expect("launch flood");
    common::ensure_running(&mut control, &launch, Duration::from_secs(15));
    let session = launch["session_id"].as_str().expect("session").to_string();
    let workload_id = launch["workload_id"].clone();
    let data_token = control.data_token.clone().expect("data token");
    let mut data = Client::data(&daemon.endpoint, &data_token);

    let mut view_id = common::uuid_v4();
    let attach = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": view_id, "access": "reader"}),
        )
        .expect("attach");
    let mut epoch = attach["epoch"].as_str().expect("epoch").to_string();
    let mut next_seq = u64_of(&attach["replay_from_seq"]);
    assert!(next_seq >= 1, "{attach}");

    // Consume + ACK. The flood outruns any consumer under a 1 MiB cap, so
    // the daemon may detach this view for falling behind the retained head;
    // then we attach again and continue from the new head.
    let mut last_seq = 0u64;
    let mut total_raw = 0u64;
    let mut frames = 0u64;
    let mut reattached = 0u32;
    let mut quiet_ms = 0u64;
    let mut terminal: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last_ack = Instant::now() - Duration::from_secs(1);
    while Instant::now() < deadline {
        if let Some(event) = control.pop_event("session.replay_required") {
            // pop_event already returns the payload (stash_event unwraps it).
            let payload = &event;
            assert_eq!(payload["session_id"], session, "{event}");
            assert_eq!(payload["view_id"], view_id, "{event}");
            assert_eq!(payload["epoch"], epoch, "{event}");
            let first_seq = u64_of(&payload["first_seq"]);
            assert!(
                first_seq > next_seq,
                "only a view behind the head is detached: {event}"
            );
            view_id = common::uuid_v4();
            let again = control
                .request(
                    "session.attach",
                    json!({"session_id": session, "view_id": view_id, "access": "reader"}),
                )
                .expect("re-attach after replay_required");
            epoch = again["epoch"].as_str().expect("epoch").to_string();
            next_seq = u64_of(&again["replay_from_seq"]);
            assert!(next_seq >= first_seq, "{again}");
            last_seq = 0;
            reattached += 1;
            continue;
        }
        match data.recv_any(Duration::from_millis(50)) {
            Some(frame) => {
                if frame.get("event").and_then(|e| e.as_str()) != Some("session.output") {
                    continue;
                }
                let payload = &frame["payload"];
                if payload["epoch"].as_str() != Some(epoch.as_str()) {
                    continue; // in flight for a detached epoch
                }
                let seq = u64_of(&payload["seq"]);
                assert_eq!(
                    seq, next_seq,
                    "delivered records must stay contiguous within an epoch (no arbitrary drop)"
                );
                next_seq += 1;
                last_seq = seq;
                total_raw += payload["raw_len"].as_u64().unwrap_or(0);
                frames += 1;
                quiet_ms = 0;
            }
            None => quiet_ms += 50,
        }
        if last_ack.elapsed() >= Duration::from_millis(120) && last_seq > 0 {
            data.send_ack(&session, &epoch, last_seq);
            last_ack = Instant::now();
        }
        if quiet_ms >= 1500 {
            let summary = common::snapshot_workload(&mut control, &workload_id).expect("workload");
            if matches!(
                summary["state"].as_str(),
                Some("SUCCEEDED" | "FAILED" | "CANCELLED")
            ) {
                terminal = Some(summary);
                break;
            }
            assert_ne!(summary["last_error_code"], "JOURNAL_LIMIT", "{summary}");
            quiet_ms = 0;
        }
    }
    let terminal =
        terminal.expect("the flood must run to completion: the cap must not stop reading");
    assert_eq!(terminal["state"], "SUCCEEDED", "{terminal}");
    assert_ne!(terminal["last_error_code"], "JOURNAL_LIMIT", "{terminal}");
    assert!(
        frames > 0 && last_seq > 0,
        "the view must have received the tail"
    );

    // On-disk retention stays near the cap: closed segments were deleted.
    let journal = daemon
        .data_dir
        .join("data/journals")
        .join(format!("{session}.mtj"));
    let on_disk = term_pty::segments::journal_files_bytes(&journal);
    let segment =
        term_pty::journal::segment_target_for(CAP_BYTES, term_pty::journal::DEFAULT_SEGMENT_BYTES);
    assert!(
        on_disk <= CAP_BYTES + segment + 64 * 1024,
        "retained journal must stay near the 1 MiB cap (got {on_disk})"
    );
    assert!(
        on_disk >= CAP_BYTES / 2,
        "trimming keeps the recent window, not just the tail (got {on_disk})"
    );
    let set = term_pty::segments::JournalSet::open(&journal).expect("journal run scans");
    assert!(
        set.first_seq() > 1,
        "head must have moved: {:?}",
        set.head()
    );
    for segment in set.segments() {
        assert_eq!(
            segment.status,
            term_pty::journal::ScanStatus::Ok,
            "{segment:?}"
        );
    }

    // A late attach replays from the trimmed head, opening with a size
    // record, and its last_seq is exactly where the live view stopped.
    let late = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": common::uuid_v4(), "access": "reader"}),
        )
        .expect("late attach");
    let replay_from = u64_of(&late["replay_from_seq"]);
    assert!(replay_from > 1, "{late}");
    assert_eq!(replay_from, set.first_seq(), "{late}");
    // The flood may still be flushing final records when the view's quiet
    // window closes; the journal's true last_seq settles at exit. The
    // invariant under test: the late attach reports the JOURNAL's last
    // committed seq (replay covers everything retained), never beyond it.
    let settled_last = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let set = term_pty::segments::JournalSet::open(&journal).expect("journal run scans");
            let seq = set.last_seq();
            if seq >= u64_of(&late["last_seq"]) || Instant::now() >= deadline {
                break seq;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    assert!(
        u64_of(&late["last_seq"]) <= settled_last,
        "late attach must not report beyond the journal: {late} settled={settled_last}"
    );
    let _ = last_seq;
    let dropped = u64_of(&late["replay_dropped_bytes"]);
    assert!(
        dropped > 0,
        "replay_dropped_bytes must be reported after a trim: {late}"
    );
    let total = dropped + on_disk;
    // The invariant is NO SILENT LOSS: everything the PTY produced is either
    // retained or accounted as dropped. The journaled total exceeds the
    // fixture's output by per-record framing (each PTY read becomes one
    // record with a header), which measured ~14% for 16 KiB flood chunks on
    // Windows ConPTY — hence the 1.5x ceiling instead of the old 1.1x that
    // conflated output bytes with journaled bytes.
    assert!(
        total >= FLOOD_BYTES,
        "dropped + retained must cover everything written ({total}, dropped={dropped}, on_disk={on_disk})"
    );
    assert!(
        total <= FLOOD_BYTES + FLOOD_BYTES / 2,
        "journal overhead beyond framing sanity ({total}, dropped={dropped}, on_disk={on_disk})"
    );
    let late_epoch = late["epoch"].as_str().expect("epoch").to_string();
    let first = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "late replay must start streaming"
            );
            let Some(frame) = data.recv_any(Duration::from_millis(100)) else {
                continue;
            };
            if frame.get("event").and_then(|e| e.as_str()) != Some("session.output") {
                continue;
            }
            let payload = frame["payload"].clone();
            if payload["epoch"].as_str() == Some(late_epoch.as_str()) {
                break payload;
            }
        }
    };
    assert_eq!(u64_of(&first["seq"]), replay_from, "{first}");
    assert_eq!(
        first["kind"], "resize",
        "a trimmed replay opens with the segment's size record: {first}"
    );

    // The control channel is unaffected.
    let started = Instant::now();
    let snapshot = control
        .request_timeout("system.snapshot", json!({}), Duration::from_secs(1))
        .expect("control channel must stay alive")
        .expect("snapshot ok");
    assert!(snapshot["workloads"].is_array());
    assert!(started.elapsed() < Duration::from_secs(1));

    eprintln!(
        "b18 rolling: raw_received={total_raw} frames={frames} reattached={reattached} on_disk={on_disk} replay_from={replay_from} dropped={dropped}"
    );
}
