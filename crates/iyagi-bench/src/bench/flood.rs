//! Benchmarks 3+4 — flood steady-state memory and slow-consumer
//! resilience (spec §5 "30분 flood에서 앱 메모리 증가가 steady state 이후
//! 계속 선형으로 증가하지 않음" + B16):
//!
//! * A shell workload runs `term-fixture flood` (deterministic 16 MiB
//!   pattern chunk-loop, 16 KiB write chunks) while a linked data
//!   connection ACKs every delivered record. Fresh flood launches replace
//!   finished ones so output flow is continuous for the window.
//! * Daemon RSS is sampled every 2 s; the slope after warm-up must show no
//!   continuing linear growth.
//! * Slow-consumer phase: ACKs stop for 5 s during the flood; control-path
//!   `system.snapshot` p95 must stay under 1 s; after ACKing resumes, the
//!   output stream must continue.
//!
//! Journal caps are raised via `IYAGI_TEST_CONFIG` for this daemon so a
//! 30 s+5 s continuous flood is not ended by the default 2 GiB *lifetime*
//! journal budget (retention cleanup is out of R1 scope — recorded as a
//! benchmark deviation in done/I12).

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{cancel_and_wait, launch_and_attach, note, workload_state, Ctx, LiveSession};
use crate::daemon::{self, DaemonProc};
use crate::report::{
    dist_from, status, CpuSamplePoint, FloodMemoryResult, SlowConsumerResult,
    TARGET_FLOOD_SLOPE_MIB_PER_MIN, TARGET_SNAPSHOT_P95_MS,
};
use crate::samplers::ProcSampler;
use crate::stats::linear_regression;
use crate::wire::Conn;

/// 16 MiB pattern × multiplier per flood launch.
fn per_launch_bytes(quick: bool) -> u64 {
    let pattern_mib: u64 = 16;
    pattern_mib * (if quick { 8 } else { 16 }) * 1024 * 1024
}

fn overrides() -> Value {
    // Session 15 GiB / global 100 GiB: effectively no cap within one bench
    // run; the default 128 MiB session / 2 GiB lifetime budgets would end a
    // continuous flood long before the window closes.
    json!({
        "limits": {
            "journal_session_bytes": 16_106_127_360u64,
            "journal_global_bytes": 107_374_182_400u64
        }
    })
}

/// Bookkeeping + ACK driving for the data connection.
struct Pump {
    last_seq: u64,
    bytes_total: u64,
    last_output_at: Instant,
    last_ack_key: Option<(String, String)>,
}

impl Pump {
    fn new() -> Pump {
        Pump {
            last_seq: 0,
            bytes_total: 0,
            last_output_at: Instant::now(),
            last_ack_key: None,
        }
    }

    /// Drain pending `session.output` frames; ACK the high-water seq (with
    /// the session/epoch the daemon itself reported) only when `acking`.
    /// Keeps draining while frames keep arriving (1 ms quiet gap ends a
    /// pass) so the harness never throttles the flood itself.
    fn drain(&mut self, data: &mut Conn, acking: bool) -> usize {
        const MAX_FRAMES_PER_PASS: usize = 512;
        let mut frames = 0usize;
        loop {
            if frames >= MAX_FRAMES_PER_PASS {
                break;
            }
            let Some(frame) = data.try_recv_frame(Duration::from_millis(1)) else {
                break;
            };
            if frame.value.get("event").and_then(|v| v.as_str()) != Some("session.output") {
                continue;
            }
            frames += 1;
            let payload = &frame.value["payload"];
            let seq: u64 = payload["seq"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if seq > self.last_seq {
                self.last_seq = seq;
            }
            if let (Some(session), Some(epoch)) =
                (payload["session_id"].as_str(), payload["epoch"].as_str())
            {
                self.last_ack_key = Some((session.to_string(), epoch.to_string()));
            }
            if payload["kind"].as_str() == Some("output") {
                let raw_len = payload["raw_len"].as_u64().unwrap_or(0);
                self.bytes_total += raw_len;
                if raw_len > 0 {
                    self.last_output_at = Instant::now();
                }
            }
        }
        if acking && self.last_seq > 0 {
            if let Some((session, epoch)) = &self.last_ack_key {
                data.send_ack(session, epoch, self.last_seq);
            }
        }
        frames
    }
}

pub fn run(ctx: &Ctx) -> Result<(FloodMemoryResult, SlowConsumerResult), String> {
    let warmup_s = if ctx.quick { 4 } else { 8 };
    let window_s = if ctx.quick { 12 } else { 30 };
    let pause_s = if ctx.quick { 3 } else { 5 };
    note(format!(
        "flood: warmup {warmup_s}s + RSS window {window_s}s + slow-consumer pause {pause_s}s, {} build",
        ctx.profile
    ));

    let mut daemon: DaemonProc = DaemonProc::spawn(&ctx.daemon_bin, "flood", Some(overrides()))?;
    let outcome = measure(ctx, &mut daemon, warmup_s, window_s, pause_s);
    match &outcome {
        Ok((mem, slow)) => note(format!(
            "flood: slope={:.2} MiB/min (r2={:.3}) RSS {:.1}->{:.1} MiB, output {:.1} MiB, launches={} -> {} | slow-consumer snapshot p95={:.1}ms resumed={} -> {}",
            mem.slope_mib_per_min,
            mem.slope_r2,
            mem.rss_start_bytes as f64 / 1048576.0,
            mem.rss_end_bytes as f64 / 1048576.0,
            mem.output_bytes_total as f64 / 1048576.0,
            mem.flood_launches,
            mem.status,
            slow.snapshot_dist.p95_ms,
            slow.output_resumed,
            slow.status,
        )),
        Err(err) => {
            note(format!("flood FAILED: {err}"));
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
    daemon: &mut DaemonProc,
    warmup_s: u64,
    window_s: u64,
    pause_s: u64,
) -> Result<(FloodMemoryResult, SlowConsumerResult), String> {
    let mut control = Conn::control(&daemon.endpoint, &daemon.token)?;
    let data_token = control
        .data_token
        .clone()
        .ok_or("control hello issued no data token")?;
    let mut data = Conn::data(&daemon.endpoint, &data_token)?;

    let cwd = daemon.data_dir.clone();
    let bytes = per_launch_bytes(ctx.quick);
    let mut launches: usize = 1;
    let mut stalled_relaunches: usize = 0;
    let mut current = start_flood(&mut control, ctx, &cwd, bytes)?;

    let mut pump = Pump::new();
    let mut sampler = ProcSampler::new(daemon.pid);
    let _ = sampler.refresh(); // CPU baseline
    let mut samples: Vec<CpuSamplePoint> = Vec::new();

    let started = Instant::now();
    let window_end = started + Duration::from_secs(warmup_s + window_s);
    let mut next_rss_sample = started;

    // Phase A: warm-up + RSS measurement window (ACKs on). The loop only
    // idles (relaunch checks + sleep) when a drain pass came back empty, so
    // the harness never throttles the flood with fixed sleeps.
    while Instant::now() < window_end {
        let frames = pump.drain(&mut data, true);
        control.drain_events();
        if Instant::now() >= next_rss_sample {
            if let Some(reading) = sampler.refresh() {
                samples.push(CpuSamplePoint {
                    t_s: started.elapsed().as_secs_f64(),
                    cpu_cores: reading.cpu_percent as f64 / 100.0,
                    rss_bytes: reading.rss_bytes,
                });
            }
            next_rss_sample += Duration::from_secs(2);
        }
        if frames == 0 {
            if needs_relaunch(&mut control, &current, &pump, daemon)? {
                let _ =
                    cancel_and_wait(&mut control, &current.workload_id, Duration::from_secs(20));
                current = start_flood(&mut control, ctx, &cwd, bytes)?;
                launches += 1;
                stalled_relaunches += 1;
                pump = Pump::new();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // Slope over post-warm-up samples only.
    let rss_samples: Vec<CpuSamplePoint> = samples
        .iter()
        .filter(|s| s.t_s >= warmup_s as f64)
        .cloned()
        .collect();
    let xs: Vec<f64> = rss_samples
        .iter()
        .map(|s| s.t_s - warmup_s as f64)
        .collect();
    let ys: Vec<f64> = rss_samples.iter().map(|s| s.rss_bytes as f64).collect();
    let (slope_bytes_per_s, slope_r2) = match linear_regression(&xs, &ys) {
        Some(fit) => (fit.slope, fit.r2),
        None => (0.0, 0.0),
    };
    let slope_mib_per_min = slope_bytes_per_s * 60.0 / 1048576.0;
    let rss_start = rss_samples.first().map(|s| s.rss_bytes).unwrap_or(0);
    let rss_end = rss_samples.last().map(|s| s.rss_bytes).unwrap_or(0);

    // Phase B: slow consumer — stop ACKing, keep draining, measure the
    // control path (`system.snapshot`) response latency.
    let seq_at_pause = pump.last_seq;
    let bytes_at_pause = pump.bytes_total;
    let mut snapshot_latencies_ms: Vec<f64> = Vec::new();
    let pause_started = Instant::now();
    while pause_started.elapsed() < Duration::from_secs(pause_s) {
        pump.drain(&mut data, false);
        control.drain_events();
        let (id, t0) = control
            .request_timed("system.snapshot", json!({}))
            .map_err(|e| format!("snapshot write during pause: {e}"))?;
        match control.wait_response(&id, Duration::from_secs(5)) {
            Some(Ok(_)) => {
                snapshot_latencies_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
            Some(Err(e)) => return Err(format!("snapshot error during pause: {e}")),
            None => return Err("system.snapshot unanswered during flood pause (5s)".into()),
        }
    }

    // Phase C: resume ACKs; output must continue (new bytes within 5 s).
    if let Some((session, epoch)) = pump.last_ack_key.clone() {
        data.send_ack(&session, &epoch, pump.last_seq);
    }
    let resume_deadline = Instant::now() + Duration::from_secs(5);
    let mut output_resumed = false;
    while Instant::now() < resume_deadline {
        let moved = pump.bytes_total > bytes_at_pause || pump.last_seq > seq_at_pause;
        if moved {
            output_resumed = true;
            break;
        }
        pump.drain(&mut data, true);
        std::thread::sleep(Duration::from_millis(20));
    }

    let _ = cancel_and_wait(&mut control, &current.workload_id, Duration::from_secs(20));

    let mem_pass = slope_mib_per_min.abs() <= TARGET_FLOOD_SLOPE_MIB_PER_MIN;
    let memory = FloodMemoryResult {
        warmup_seconds: warmup_s as f64,
        measure_seconds: window_s as f64,
        samples,
        slope_bytes_per_s,
        slope_mib_per_min,
        slope_r2,
        rss_start_bytes: rss_start,
        rss_end_bytes: rss_end,
        output_bytes_total: pump.bytes_total,
        flood_launches: launches,
        config_overrides: overrides(),
        target_slope_mib_per_min: TARGET_FLOOD_SLOPE_MIB_PER_MIN,
        status: status(ctx.profile, mem_pass),
        notes: vec![
            format!("verdict threshold |slope| <= {TARGET_FLOOD_SLOPE_MIB_PER_MIN} MiB/min over the {window_s}s post-warm-up window (a 100 MiB/30min leak == 3.3 MiB/min stays visible)"),
            "journal caps raised via IYAGI_TEST_CONFIG (session 15 GiB / global 100 GiB): the default 2 GiB lifetime journal budget would end a continuous flood before the window closes; retention cleanup is out of R1 scope".into(),
            format!("flood launches: {launches} total ({stalled_relaunches} after stall/finish); {seq_at_pause} records delivered pre-pause"),
        ],
    };

    let dist = dist_from(&snapshot_latencies_ms);
    let slow_pass = dist.p95_ms < TARGET_SNAPSHOT_P95_MS && output_resumed;
    let slow = SlowConsumerResult {
        pause_seconds: pause_s as f64,
        snapshot_dist: dist,
        output_resumed,
        target_p95_ms: TARGET_SNAPSHOT_P95_MS,
        status: status(ctx.profile, slow_pass),
        notes: vec![
            "ACKs stopped while the flood continued; the data connection kept draining but sent no credits (per-view 256 KiB high-water stops delivery, journal keeps buffering)".into(),
            format!("bytes delivered pre-pause {:.1} MiB; resume verified by continued delivery within 5s", bytes_at_pause as f64 / 1048576.0),
        ],
    };
    Ok((memory, slow))
}

/// Launch a fresh flood workload and attach a writer view.
fn start_flood(
    control: &mut Conn,
    ctx: &Ctx,
    cwd: &Path,
    bytes: u64,
) -> Result<LiveSession, String> {
    let bytes_text = bytes.to_string();
    let chunk_text = (16 * 1024).to_string();
    let args = [
        "flood",
        "--bytes",
        &bytes_text,
        "--chunk",
        &chunk_text,
        "--seed",
        "7",
    ];
    launch_and_attach(control, ctx, &args, cwd, "shell")
}

/// Relaunch when the current flood finished or stalled.
fn needs_relaunch(
    control: &mut Conn,
    current: &LiveSession,
    pump: &Pump,
    daemon: &mut DaemonProc,
) -> Result<bool, String> {
    if !daemon.alive() {
        return Err(format!(
            "daemon died during flood. stderr tail:\n{}",
            daemon.stderr_tail()
        ));
    }
    let quiet_for = pump.last_output_at.elapsed();
    if quiet_for < Duration::from_secs(4) {
        return Ok(false);
    }
    let state = workload_state(control, &current.workload_id)?;
    if matches!(
        state.as_str(),
        "SUCCEEDED" | "FAILED" | "CANCELLED" | "INTERRUPTED"
    ) {
        return Ok(true);
    }
    // Still RUNNING but silent for 8 s (blocked writer / stall): replace.
    Ok(quiet_for >= Duration::from_secs(8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_launch_bytes_is_16mib_pattern_multiple() {
        assert_eq!(per_launch_bytes(true), 8 * 16 * 1024 * 1024);
        assert_eq!(per_launch_bytes(false), 16 * 16 * 1024 * 1024);
    }

    #[test]
    fn overrides_raise_journal_caps() {
        let v = overrides();
        assert!(
            v["limits"]["journal_session_bytes"].as_u64().unwrap() > 128 * 1024 * 1024,
            "session cap must exceed the 128 MiB default"
        );
        assert!(
            v["limits"]["journal_global_bytes"].as_u64().unwrap() > 2 * 1024 * 1024 * 1024,
            "global cap must exceed the 2 GiB default"
        );
    }
}
