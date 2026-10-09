//! Benchmark 1 — input→echo latency (spec §5 "foreground echo p95 ≤50 ms,
//! p99 ≤100 ms"): N inputs to a shell workload running `term-fixture echo`,
//! measured t(send `session.input`) → t(receipt of the output frame whose
//! bytes contain this input's unique marker), over the data connection.
//!
//! Replay is excluded by construction: the writer view is attached once
//! before any input, the warm-up drains all startup output, and each timed
//! iteration waits only for its own unique marker.

use std::time::{Duration, Instant};

use serde_json::json;

use super::{cancel_and_wait, launch_and_attach, note, Ctx};
use crate::daemon::{self, DaemonProc};
use crate::report::{
    dist_from, status, LatencyResult, Percentiles, TARGET_ECHO_P95_MS, TARGET_ECHO_P99_MS,
};
use crate::wire::{b64, unb64, Conn};

/// Rolling received-bytes window for marker matching (markers are ~20 B).
struct EchoWindow {
    buf: Vec<u8>,
    cap: usize,
}

impl EchoWindow {
    fn new(cap: usize) -> Self {
        EchoWindow {
            buf: Vec::with_capacity(cap),
            cap,
        }
    }
    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() > self.cap {
            let cut = self.buf.len() - self.cap;
            self.buf.drain(..cut);
        }
    }
    fn contains(&self, marker: &str) -> bool {
        let m = marker.as_bytes();
        !m.is_empty() && self.buf.windows(m.len()).any(|w| w == m)
    }
}

/// One measured input round: send → RPC accepted → marker seen on data.
fn echo_round(
    control: &mut Conn,
    data: &mut Conn,
    session_id: &str,
    epoch: &str,
    marker: &str,
    window: &mut EchoWindow,
) -> Result<f64, String> {
    let text = format!("{marker}\n");
    let (id, t_send) = control
        .request_timed(
            "session.input",
            json!({
                "session_id": session_id,
                "epoch": epoch,
                "input_id": format!("in-{marker}"),
                "data_b64": b64(text.as_bytes()),
            }),
        )
        .map_err(|e| format!("input write: {e}"))?;
    let reply = control
        .wait_response(&id, Duration::from_secs(5))
        .ok_or("no session.input response within 5s")?;
    reply.map_err(|e| format!("session.input rejected: {e}"))?;

    // Wait for the marker on the data connection, ACKing every record.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_seq: u64 = 0;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("echo marker {marker:?} not seen within 5s"));
        }
        let Some(frame) = data.try_recv_frame(remaining.min(Duration::from_millis(100))) else {
            continue; // recv timeout inside the overall deadline — keep waiting
        };
        if frame.value.get("event").and_then(|v| v.as_str()) != Some("session.output") {
            continue;
        }
        let payload = frame.value["payload"].clone();
        let seq: u64 = payload["seq"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if seq > last_seq {
            last_seq = seq;
            if let Some(ep) = payload["epoch"].as_str() {
                data.send_ack(session_id, ep, seq);
            }
        }
        if payload["kind"].as_str() == Some("output") {
            if let Some(text) = payload["data_b64"].as_str() {
                window.push(&unb64(text));
            }
        }
        if window.contains(marker) {
            return Ok((frame.at - t_send).as_secs_f64() * 1000.0);
        }
    }
}

pub fn run(ctx: &Ctx) -> Result<LatencyResult, String> {
    let inputs = if ctx.quick { 50 } else { 200 };
    let warmup = if ctx.quick { 10 } else { 20 };
    note(format!(
        "latency: {inputs} inputs (+{warmup} warmup) over loopback, {} build",
        ctx.profile
    ));

    let mut daemon: DaemonProc = DaemonProc::spawn(&ctx.daemon_bin, "latency", None)?;
    let outcome = measure(ctx, &daemon, inputs, warmup);
    match &outcome {
        Ok(result) => note(format!(
            "latency: p50={:.1}ms p95={:.1}ms p99={:.1}ms -> {}",
            result.dist.p50_ms, result.dist.p95_ms, result.dist.p99_ms, result.status
        )),
        Err(err) => {
            note(format!("latency FAILED: {err}"));
            note(format!("daemon stderr tail:\n{}", daemon.stderr_tail()));
        }
    }
    let _ = daemon::shutdown(&mut daemon, None);
    if !ctx.keep_data && outcome.is_ok() {
        daemon::cleanup_data_dir(&daemon.data_dir);
    }
    outcome
}

fn measure(
    ctx: &Ctx,
    daemon: &DaemonProc,
    inputs: usize,
    warmup: usize,
) -> Result<LatencyResult, String> {
    let mut control = Conn::control(&daemon.endpoint, &daemon.token)?;
    let cwd = daemon.data_dir.clone();
    let live = launch_and_attach(&mut control, ctx, &["echo"], &cwd, "shell")?;

    let data_token = control
        .data_token
        .clone()
        .ok_or("control hello issued no data token")?;
    let mut data = Conn::data(&daemon.endpoint, &data_token)?;

    let mut window = EchoWindow::new(8192);
    let mut samples_ms: Vec<f64> = Vec::with_capacity(inputs);
    for i in 0..(warmup + inputs) {
        let marker = format!("bm{:05}x{}", i, uuid_short());
        let ms = echo_round(
            &mut control,
            &mut data,
            &live.session_id,
            &live.epoch,
            &marker,
            &mut window,
        )?;
        if i >= warmup {
            samples_ms.push(ms);
        }
    }

    let _ = cancel_and_wait(&mut control, &live.workload_id, Duration::from_secs(20));

    let dist: Percentiles = dist_from(&samples_ms);
    let pass = dist.p95_ms <= TARGET_ECHO_P95_MS && dist.p99_ms <= TARGET_ECHO_P99_MS;
    Ok(LatencyResult {
        inputs,
        warmup_inputs: warmup,
        dist,
        target_p95_ms: TARGET_ECHO_P95_MS,
        target_p99_ms: TARGET_ECHO_P99_MS,
        status: status(ctx.profile, pass),
        notes: vec![
            "t(send session.input) -> t(receipt of session.output frame containing the input's unique marker); timestamps taken at frame-arrival in the reader thread".into(),
            "no replay counted: single attach before warm-up, per-input unique marker, ACKs advance normally".into(),
            format!("spec targets are for release builds; this run measured the {} build", ctx.profile),
        ],
    })
}

fn uuid_short() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_window_matches_markers_split_across_pushes() {
        let mut w = EchoWindow::new(64);
        w.push(b"bm00001xAB12");
        w.push(b"34CD\nnext");
        assert!(w.contains("bm00001xAB1234CD"));
        assert!(!w.contains("bm00002"));
    }

    #[test]
    fn echo_window_trims_its_cap() {
        let mut w = EchoWindow::new(16);
        w.push(b"OLDMARKER-0123456789");
        w.push(b"tail");
        assert!(w.buf.len() <= 16);
        // Old content dropped once past the cap, recent bytes kept.
        assert!(!w.contains("OLDMARKER"));
        assert!(w.contains("tail"));
    }

    #[test]
    fn echo_window_rejects_empty_marker() {
        let mut w = EchoWindow::new(8);
        w.push(b"abc");
        assert!(!w.contains(""));
    }
}
