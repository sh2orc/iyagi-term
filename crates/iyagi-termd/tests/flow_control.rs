//! B16/B17 (spec `06-verification.md` §3): output flow control over the wire.
//!
//! * B16 — a data connection that stops ACKing while a 16 MiB flood runs:
//!   the view blocks at its 256 KiB watermark and `session.flow_blocked` is
//!   announced; the daemon keeps answering `system.snapshot` (<1 s); the
//!   journal keeps moving (the journal is the buffer, not RAM); when ACKs
//!   resume, delivery continues from the journal cursor with no record loss
//!   (seq contiguous, totals, fixture hash marker).
//! * B17 — daemon-level ACK semantics: duplicate ACK ignored (no error),
//!   future ACK rejected as a protocol error (error frame on the data
//!   connection; the connection itself survives — frame-level violations
//!   are what close connections, spec §3), old-epoch ACK after a re-attach
//!   ignored even with a future seq.

mod common;

use common::{launch_request, Client, DaemonProc};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Fixture pattern replication (term-fixture flood: xorshift64* mapped onto
// '!'..'~' so the stream survives terminal transports).

struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn fill(&mut self, buf: &mut [u8]) {
        let mut i = 0;
        while i + 8 <= buf.len() {
            buf[i..i + 8].copy_from_slice(&self.next().to_le_bytes());
            i += 8;
        }
        if i < buf.len() {
            let bytes = self.next().to_le_bytes();
            let tail = buf.len() - i;
            buf[i..].copy_from_slice(&bytes[..tail]);
        }
    }
}

/// The exact byte pattern `term-fixture flood --bytes --chunk --seed` writes.
fn flood_pattern(bytes: u64, chunk: usize, seed: u64) -> Vec<u8> {
    let mut rng = XorShift64::new(seed);
    let mut buf = vec![0u8; chunk.min(bytes.max(1) as usize)];
    let mut out = Vec::with_capacity(bytes as usize);
    let mut remaining = bytes as usize;
    while remaining > 0 {
        let take = buf.len().min(remaining);
        rng.fill(&mut buf);
        for b in buf.iter_mut() {
            *b = b'!' + (*b % 94);
        }
        out.extend_from_slice(&buf[..take]);
        remaining -= take;
    }
    out
}

// ---------------------------------------------------------------------------
// session.output collector: seq contiguity + totals per epoch.

#[derive(Default)]
struct OutputCollector {
    /// (epoch, last_seq, per-epoch expected-next, total raw bytes, payload).
    epochs: Vec<(String, u64, u64, u64, Vec<u8>)>,
    frames: u64,
}

impl OutputCollector {
    fn push(&mut self, payload: &serde_json::Value) {
        let seq: u64 = payload["seq"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let epoch = payload["epoch"].as_str().unwrap_or_default().to_string();
        let is_output = payload["kind"].as_str() == Some("output");
        let data = common::unb64(payload["data_b64"].as_str().unwrap_or_default());
        let raw: u64 = payload["raw_len"].as_u64().unwrap_or(0);
        // Register the epoch on its first record (seq 1 = the initial size).
        if !self.epochs.iter().any(|(e, _, _, _, _)| *e == epoch) {
            assert_eq!(seq, 1, "first record of an epoch must be seq 1, got {seq}");
            self.epochs.push((epoch.clone(), 0, 1, 0, Vec::new()));
        }
        let entry = self
            .epochs
            .iter_mut()
            .find(|(e, _, _, _, _)| *e == epoch)
            .expect("registered above");
        assert_eq!(
            seq, entry.2,
            "seq must be contiguous (no drop/duplicate) in epoch {epoch}"
        );
        entry.1 = entry.1.max(seq);
        entry.2 = seq + 1;
        if is_output {
            self.frames += 1;
            entry.3 += raw;
            entry.4.extend_from_slice(&data);
        }
    }

    fn total_raw(&self) -> u64 {
        self.epochs.iter().map(|(_, _, _, raw, _)| *raw).sum()
    }

    /// All payloads of the FIRST epoch with CR/LF and ANSI escape sequences
    /// stripped: ConPTY wraps long lines by inserting CRLF plus explicit
    /// cursor positioning (e.g. `ESC[23;80H`); the wrapped text itself is
    /// preserved, so stripping control/escape runs reassembles it.
    fn stripped_stream(&self) -> Vec<u8> {
        let data: Vec<u8> = self
            .epochs
            .first()
            .map(|(_, _, _, _, data)| data.clone())
            .unwrap_or_default();
        common::strip_terminal_controls(&data)
    }
}

fn attach_reader(
    control: &mut Client,
    daemon: &DaemonProc,
    session: &str,
) -> (String, String, Client) {
    let view_id = common::uuid_v4();
    let attach = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": view_id, "access": "reader"}),
        )
        .expect("attach reader");
    let epoch = attach["epoch"].as_str().expect("epoch").to_string();
    let data_token = control.data_token.clone().expect("data token");
    let data = Client::data(&daemon.endpoint, &data_token);
    (view_id, epoch, data)
}

// ---------------------------------------------------------------------------
// B16

#[test]
fn b16_slow_consumer_blocks_view_not_the_daemon_or_journal() {
    const FLOOD_BYTES: u64 = 16 * 1024 * 1024;
    let daemon = DaemonProc::spawn("b16-slow", Some(common::relaxed_admission(json!({}))));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = control
        .request(
            "workload.launch",
            launch_request(
                "managed",
                &[
                    "flood", "--bytes", "16777216", "--chunk", "16384", "--seed", "7",
                ],
                "1048576",
            ),
        )
        .expect("launch flood");
    common::ensure_running(&mut control, &launch, Duration::from_secs(15));
    let session = launch["session_id"].as_str().expect("session").to_string();
    let workload_id = launch["workload_id"].clone();

    let (_view, epoch, mut data) = attach_reader(&mut control, &daemon, &session);
    let mut collector = OutputCollector::default();

    // -- Phase A: freeze ACKs ------------------------------------------------
    // Receive WITHOUT acking until the view crosses its 256 KiB watermark.
    let blocked = control
        .wait_event("session.flow_blocked", Duration::from_secs(20))
        .expect("session.flow_blocked must be announced for a frozen consumer");
    assert_eq!(blocked["session_id"], json!(session), "got {blocked}");

    // Daemon responsiveness throughout: every snapshot answers < 1 s.
    for _ in 0..3 {
        let started = Instant::now();
        let snapshot = control
            .request_timeout("system.snapshot", json!({}), Duration::from_secs(1))
            .expect("system.snapshot must answer while a consumer is frozen");
        assert!(snapshot.is_ok(), "snapshot failed: {:?}", snapshot.err());
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "snapshot took {elapsed:?}"
        );
    }

    // Drain whatever was already in flight during the frozen window.
    let frozen_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < frozen_deadline {
        match data.recv_any(Duration::from_millis(100)) {
            Some(frame) => {
                if frame.get("event").and_then(|e| e.as_str()) == Some("session.output") {
                    collector.push(&frame["payload"]);
                }
            }
            None => break,
        }
    }
    let frozen_bytes = collector.total_raw();
    assert!(
        frozen_bytes < 1024 * 1024,
        "a frozen view must not receive unbounded bytes (got {frozen_bytes})"
    );

    // The journal kept moving while the view was blocked. A fast reader can
    // finish the flood and rotate before this check, so include closed segments
    // as well as the active data/journals/<session>.mtj file.
    let journal = daemon
        .data_dir
        .join("data/journals")
        .join(format!("{session}.mtj"));
    // macOS debug 빌드의 fixture/PTY 처리량은 Windows 대비 느리다 —
    // 마감을 45초로 넉넉히(기대치 15 MiB는 그대로).
    let deadline = Instant::now() + Duration::from_secs(45);
    let journal_len = loop {
        let len = term_pty::segments::journal_files_bytes(&journal);
        if len >= 15 * 1024 * 1024 || Instant::now() >= deadline {
            break len;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(
        journal_len >= 15 * 1024 * 1024,
        "journal must keep accepting while the view is blocked (len {journal_len})"
    );

    // The workload itself finished: the journal was the buffer.
    let done = wait_state(
        &mut control,
        &workload_id,
        &["SUCCEEDED"],
        Duration::from_secs(20),
    );
    assert_eq!(done["state"], "SUCCEEDED", "got {done}");

    // -- Phase B: resume ACKs -------------------------------------------------
    // ACK everything received so far; delivery must continue from the stored
    // journal cursor with no record loss.
    let resume_started = Instant::now();
    let mut quiet_ms = 0u64;
    let mut last_ack = Instant::now() - Duration::from_secs(1);
    while Instant::now() < resume_started + Duration::from_secs(60) {
        match data.recv_any(Duration::from_millis(50)) {
            Some(frame) => {
                if frame.get("event").and_then(|e| e.as_str()) == Some("session.output") {
                    collector.push(&frame["payload"]);
                    quiet_ms = 0;
                }
            }
            None => quiet_ms += 50,
        }
        if last_ack.elapsed() >= Duration::from_millis(120) {
            let last = collector
                .epochs
                .first()
                .map(|(_, last, _, _, _)| *last)
                .unwrap_or(0);
            if last > 0 {
                data.send_ack(&session, &epoch, last);
                last_ack = Instant::now();
            }
        }
        // Complete: quiescent for a full second after the workload finished.
        if quiet_ms >= 1000 {
            break;
        }
    }

    // -- Assertions ------------------------------------------------------------
    let total = collector.total_raw();
    assert!(
        total >= FLOOD_BYTES,
        "all flood bytes must eventually be delivered (got {total})"
    );
    assert_eq!(collector.epochs.len(), 1, "single attach epoch expected");

    // The fixture's own integrity marker: sha256 over the deterministic
    // pattern, printed once the flood completes. ConPTY's re-encoded output
    // wraps long lines and DUPLICATES the character at the wrap seam
    // (cursor repositioned to the last column, then the continuation
    // re-prints the boundary char), so the check is order-preserving
    // subsequence matching with bounded inserted slack: every byte of the
    // marker must survive in order, allowing only conhost insertions.
    let expected_hash = {
        let pattern = flood_pattern(FLOOD_BYTES, 16384, 7);
        let digest = Sha256::digest(&pattern);
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let stripped = collector.stripped_stream();
    let marker = format!("term-fixture flood bytes={FLOOD_BYTES} sha256={expected_hash}");
    let marker_at = subsequence_match(&stripped, marker.as_bytes(), 48);
    assert!(
        marker_at.is_some(),
        "flood completion marker with the deterministic sha256 must survive delivery \
         (total raw {total}, stripped head: {:?})",
        String::from_utf8_lossy(&stripped[..stripped.len().min(120)])
    );
    // And the flood pattern itself: the stream must open with the pattern's
    // first bytes (same conhost-insertion tolerance).
    let pattern_head = flood_pattern(64 * 1024, 16384, 7);
    assert!(
        subsequence_match(&stripped, &pattern_head, 8 * 1024).is_some(),
        "delivered stream must open with the deterministic flood pattern"
    );
    eprintln!(
        "b16: frames={} total_raw={} journal_len={}",
        collector.frames, total, journal_len
    );
}

/// Order-preserving match of `needle` anywhere in `hay`, allowing at most
/// `slack` inserted bytes in total (ConPTY wrap-duplication artifacts).
/// Returns the end offset of the first bounded match.
fn subsequence_match(hay: &[u8], needle: &[u8], slack: usize) -> Option<usize> {
    let budget = needle.len() + slack;
    let mut start = 0usize;
    while start + needle.len() <= hay.len() {
        if hay[start] != needle[0] {
            start += 1;
            continue;
        }
        let mut j = start;
        let mut ok = true;
        for &b in needle {
            while j < hay.len() && hay[j] != b {
                j += 1;
            }
            if j >= hay.len() || j - start >= budget {
                ok = false;
                break;
            }
            j += 1;
        }
        if ok {
            return Some(j);
        }
        start += 1;
    }
    None
}

fn wait_state(
    client: &mut Client,
    workload_id: &serde_json::Value,
    wanted: &[&str],
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let summary = common::snapshot_workload(client, workload_id)
            .unwrap_or_else(|| panic!("workload vanished"));
        if let Some(state) = summary["state"].as_str() {
            if wanted.contains(&state) {
                return summary;
            }
        }
        assert!(
            Instant::now() < deadline,
            "never reached {wanted:?}: {summary}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// B17

#[test]
fn b17_ack_wire_semantics_duplicate_future_old_epoch() {
    let daemon = DaemonProc::spawn("b17-ack", Some(common::relaxed_admission(json!({}))));
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = control
        .request(
            "workload.launch",
            launch_request(
                "managed",
                &[
                    "flood", "--bytes", "4194304", "--chunk", "16384", "--seed", "3",
                ],
                "1048576",
            ),
        )
        .expect("launch flood");
    common::ensure_running(&mut control, &launch, Duration::from_secs(15));
    let session = launch["session_id"].as_str().expect("session").to_string();

    let (view_id, epoch, mut data) = attach_reader(&mut control, &daemon, &session);
    let mut collector = OutputCollector::default();

    // Gather ≥ 30 records.
    let deadline = Instant::now() + Duration::from_secs(10);
    while collector.frames < 30 && Instant::now() < deadline {
        if let Some(frame) = data.recv_any(Duration::from_millis(100)) {
            if frame.get("event").and_then(|e| e.as_str()) == Some("session.output") {
                collector.push(&frame["payload"]);
            }
        }
    }
    let last_seq = collector
        .epochs
        .first()
        .map(|(_, last, _, _, _)| *last)
        .unwrap_or(0);
    assert!(last_seq >= 30, "need ≥30 records before probing acks");

    // (1) Duplicate ACK: same through_seq twice → ignored, NO error frame,
    //     delivery continues.
    let dup_id_a = data.send_ack(&session, &epoch, last_seq);
    let dup_id_b = data.send_ack(&session, &epoch, last_seq);
    let mut frames_after_dup = 0;
    let dup_deadline = Instant::now() + Duration::from_millis(600);
    while Instant::now() < dup_deadline {
        if let Some(frame) = data.recv_any(Duration::from_millis(100)) {
            let is_response = frame.get("id").is_some();
            let id = frame.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            assert!(
                !(is_response
                    && (id == dup_id_a || id == dup_id_b)
                    && frame.get("error").is_some()),
                "duplicate ACK must be silently ignored, got {frame}"
            );
            if !is_response && frame.get("event").and_then(|e| e.as_str()) == Some("session.output")
            {
                collector.push(&frame["payload"]);
                frames_after_dup += 1;
            }
        }
    }

    // (2) Future ACK: through_seq beyond anything sent → protocol error as a
    //     response frame on the data connection; the connection survives.
    let future_id = data.send_ack(&session, &epoch, last_seq + 1_000_000);
    let mut future_error: Option<serde_json::Value> = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && future_error.is_none() {
        if let Some(frame) = data.recv_any(Duration::from_millis(100)) {
            if frame.get("id").and_then(|v| v.as_str()) == Some(future_id.as_str()) {
                future_error = frame.get("error").cloned();
            } else if frame.get("event").and_then(|e| e.as_str()) == Some("session.output") {
                collector.push(&frame["payload"]);
            }
        }
    }
    let error = future_error.expect("future ACK must produce a protocol error response");
    assert_eq!(error["code"], "INVALID_ARGUMENT", "got {error}");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|m| m.contains("protocol violation") || m.contains("beyond")),
        "error should name the flow protocol violation: {error}"
    );
    // The connection is NOT closed for an ACK protocol error (only frame
    // violations close connections, spec §3): it still carries output.
    assert!(
        !data.closed(Duration::from_millis(300)),
        "connection must survive"
    );

    // (3) Old-epoch ACK after re-attach: even with a future seq it is ignored
    //     (epoch check short-circuits before the ledger).
    let detach = control
        .request(
            "session.detach",
            json!({"session_id": session, "view_id": view_id}),
        )
        .expect("detach");
    assert_eq!(detach["detached"], true);
    let reattach = control
        .request(
            "session.attach",
            json!({"session_id": session, "view_id": view_id, "access": "reader"}),
        )
        .expect("re-attach");
    let new_epoch = reattach["epoch"].as_str().expect("new epoch").to_string();
    assert_ne!(new_epoch, epoch, "re-attach rotates the epoch");

    let stale_id = data.send_ack(&session, &epoch, last_seq + 5_000_000);
    let stale_deadline = Instant::now() + Duration::from_millis(800);
    let mut replay_frames = 0;
    while Instant::now() < stale_deadline {
        if let Some(frame) = data.recv_any(Duration::from_millis(100)) {
            if frame.get("id").and_then(|v| v.as_str()) == Some(stale_id.as_str()) {
                panic!("old-epoch ACK must be ignored without a response: {frame}");
            }
            if frame.get("event").and_then(|e| e.as_str()) == Some("session.output") {
                // Replay under the NEW epoch: seq restarts at 1.
                let payload = &frame["payload"];
                if payload["epoch"].as_str() == Some(new_epoch.as_str()) {
                    collector.push(payload);
                    replay_frames += 1;
                }
            }
        }
    }
    assert!(replay_frames > 0, "replay under the new epoch must flow");

    // Stream keeps working: ACK the new epoch and drain the replay fully.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last_ack = Instant::now() - Duration::from_secs(1);
    let mut quiet_ms = 0u64;
    while Instant::now() < deadline {
        match data.recv_any(Duration::from_millis(50)) {
            Some(frame) => {
                if frame.get("event").and_then(|e| e.as_str()) == Some("session.output") {
                    collector.push(&frame["payload"]);
                    quiet_ms = 0;
                }
            }
            None => quiet_ms += 50,
        }
        if last_ack.elapsed() >= Duration::from_millis(120) {
            let last = collector
                .epochs
                .iter()
                .find(|(e, _, _, _, _)| *e == new_epoch)
                .map(|(_, last, _, _, _)| *last)
                .unwrap_or(0);
            if last > 0 {
                data.send_ack(&session, &new_epoch, last);
                last_ack = Instant::now();
            }
        }
        let done =
            common::snapshot_workload(&mut control, &launch["workload_id"]).expect("workload");
        let replay_raw = collector
            .epochs
            .iter()
            .find(|(e, _, _, _, _)| *e == new_epoch)
            .map(|(_, _, _, raw, _)| *raw)
            .unwrap_or(0);
        if done["state"] == "SUCCEEDED" && replay_raw >= 4 * 1024 * 1024 && quiet_ms >= 800 {
            break;
        }
    }
    let replay_raw = collector
        .epochs
        .iter()
        .find(|(e, _, _, _, _)| *e == new_epoch)
        .map(|(_, _, _, raw, _)| *raw)
        .unwrap_or(0);
    assert!(
        replay_raw >= 4 * 1024 * 1024,
        "replayed epoch must deliver the full flood (got {replay_raw})"
    );
    let _ = frames_after_dup;
}
