//! Benchmark 2 — idle daemon CPU (spec §5 "idle daemon CPU 평균 ≤논리 코어
//! 0.02개"): K shell sessions running `term-fixture echo` (waiting on
//! stdin = idle), each with an attached writer view; sample the daemon
//! process's own CPU time via sysinfo over the window and report average
//! cores (sysinfo `cpu_usage` %, 100 % == one logical core) + daemon RSS.
//!
//! The daemon's own 1 s host-telemetry loop is part of the measurement —
//! that is the product's real idle cost.

use std::time::{Duration, Instant};

use super::{cancel_and_wait, launch_and_attach, note, Ctx};
use crate::daemon::{self, DaemonProc};
use crate::report::{status, CpuSamplePoint, IdleCpuResult, TARGET_IDLE_CORES};
use crate::samplers::ProcSampler;
use crate::wire::Conn;

pub fn run(ctx: &Ctx) -> Result<IdleCpuResult, String> {
    // K=8 per the ticket; only the window shrinks in quick mode.
    let sessions: usize = 8;
    let window_s = if ctx.quick { 10 } else { 30 };
    note(format!(
        "idle_cpu: {sessions} idle shell sessions, {window_s}s sampling window, {} build",
        ctx.profile
    ));

    let mut daemon: DaemonProc = DaemonProc::spawn(&ctx.daemon_bin, "idle", None)?;
    let outcome = measure(ctx, &mut daemon, sessions, window_s);
    match &outcome {
        Ok(result) => note(format!(
            "idle_cpu: avg={:.4} cores max={:.4} cores rss={:.1} MiB -> {}",
            result.avg_cores,
            result.max_cores,
            result.daemon_rss_avg_bytes as f64 / (1024.0 * 1024.0),
            result.status
        )),
        Err(err) => {
            note(format!("idle_cpu FAILED: {err}"));
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
    sessions: usize,
    window_s: u64,
) -> Result<IdleCpuResult, String> {
    let mut control = Conn::control(&daemon.endpoint, &daemon.token)?;
    let cwd = daemon.data_dir.clone();

    let mut live = Vec::new();
    for _ in 0..sessions {
        live.push(launch_and_attach(
            &mut control,
            ctx,
            &["echo"],
            &cwd,
            "shell",
        )?);
    }
    // A linked data connection per control hello (one here): idle sessions
    // produce no output, but the connection keeps the topology realistic.
    let data_token = control
        .data_token
        .clone()
        .ok_or("control hello issued no data token")?;
    let mut data = Conn::data(&daemon.endpoint, &data_token)?;

    // Settle: let launches, journal flushers and one telemetry pass land.
    std::thread::sleep(Duration::from_secs(2));

    let mut sampler = ProcSampler::new(daemon.pid);
    let mut samples: Vec<CpuSamplePoint> = Vec::new();
    let started = Instant::now();
    let mut discarded_baseline = false;
    while started.elapsed() < Duration::from_secs(window_s) {
        // First refresh after construction only re-anchors the CPU delta.
        if let Some(reading) = sampler.refresh() {
            if discarded_baseline {
                samples.push(CpuSamplePoint {
                    t_s: started.elapsed().as_secs_f64(),
                    cpu_cores: reading.cpu_percent as f64 / 100.0,
                    rss_bytes: reading.rss_bytes,
                });
            }
            discarded_baseline = true;
        } else {
            return Err("daemon process vanished during idle sampling".into());
        }
        // Keep both connections drained (no traffic expected).
        control.drain_events();
        while data.try_recv_frame(Duration::from_millis(0)).is_some() {}
        if !daemon.alive() {
            return Err(format!(
                "daemon exited during idle sampling. stderr tail:\n{}",
                daemon.stderr_tail()
            ));
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    // Teardown sessions.
    for session in &live {
        let _ = cancel_and_wait(&mut control, &session.workload_id, Duration::from_secs(20));
    }
    drop(data);

    // Report in cores via the documented conversion (100 % == one core).
    let percents: Vec<f32> = samples
        .iter()
        .map(|s| (s.cpu_cores * 100.0) as f32)
        .collect();
    let avg_cores = crate::stats::average_cores(&percents).unwrap_or(f64::MAX);
    let max_cores = samples.iter().map(|s| s.cpu_cores).fold(0.0, f64::max);
    let rss_avg = samples.iter().map(|s| s.rss_bytes).sum::<u64>() / samples.len().max(1) as u64;
    let rss_max = samples.iter().map(|s| s.rss_bytes).max().unwrap_or(0);
    let pass = avg_cores <= TARGET_IDLE_CORES;

    Ok(IdleCpuResult {
        sessions,
        sample_seconds: window_s as f64,
        samples,
        avg_cores,
        max_cores,
        daemon_rss_avg_bytes: rss_avg,
        daemon_rss_max_bytes: rss_max,
        target_avg_cores: TARGET_IDLE_CORES,
        status: status(ctx.profile, pass),
        notes: vec![
            "sysinfo process cpu_usage: 100% == one logical core; the daemon's own 1s host-telemetry loop is included (that is the product's idle cost)".into(),
            "sessions idle = term-fixture echo blocked on stdin; writer views attached, linked data connection open".into(),
            format!("spec target is for release builds; this run measured the {} build", ctx.profile),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_points_roundtrip_through_report_types() {
        let pts = [
            CpuSamplePoint {
                t_s: 0.0,
                cpu_cores: 0.001,
                rss_bytes: 10,
            },
            CpuSamplePoint {
                t_s: 1.0,
                cpu_cores: 0.002,
                rss_bytes: 11,
            },
        ];
        let avg = crate::stats::mean(&[0.001, 0.002]).unwrap();
        assert!((avg - 0.0015).abs() < 1e-9);
        let text = serde_json::to_string(&pts[0]).unwrap();
        let back: CpuSamplePoint = serde_json::from_str(&text).unwrap();
        assert_eq!(back, pts[0]);
    }
}
