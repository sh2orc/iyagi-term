//! Result schema (JSON `scripts/bench/results/<ts>.json`) and the human
//! summary. Every section carries its target and a PASS/FAIL verdict; when
//! the harness runs a debug build the verdict is prefixed MEASURED-DEBUG
//! because the spec targets are for release builds.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "iyagi-bench/1";

/// Spec `06-verification.md` §5 initial targets.
pub const TARGET_ECHO_P95_MS: f64 = 50.0;
pub const TARGET_ECHO_P99_MS: f64 = 100.0;
pub const TARGET_IDLE_CORES: f64 = 0.02;
/// Chosen threshold for "no continuing linear growth" over a 30 s window:
/// a real leak of ≥ 100 MiB/30 min == 3.3 MiB/min stays visible, allocator
/// noise does not.
pub const TARGET_FLOOD_SLOPE_MIB_PER_MIN: f64 = 1.0;
pub const TARGET_SNAPSHOT_P95_MS: f64 = 1000.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostInfo {
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub cpu_count: usize,
    pub cpu_brand: String,
    pub memory_total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildInfo {
    /// "debug" or "release" — which target dir the daemon binary came from.
    pub profile: String,
    pub caveat: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Percentiles {
    pub n: usize,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples_ms: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LatencyResult {
    pub inputs: usize,
    pub warmup_inputs: usize,
    pub dist: Percentiles,
    pub target_p95_ms: f64,
    pub target_p99_ms: f64,
    pub status: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CpuSamplePoint {
    pub t_s: f64,
    pub cpu_cores: f64,
    pub rss_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdleCpuResult {
    pub sessions: usize,
    pub sample_seconds: f64,
    pub samples: Vec<CpuSamplePoint>,
    pub avg_cores: f64,
    pub max_cores: f64,
    pub daemon_rss_avg_bytes: u64,
    pub daemon_rss_max_bytes: u64,
    pub target_avg_cores: f64,
    pub status: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FloodMemoryResult {
    pub warmup_seconds: f64,
    pub measure_seconds: f64,
    pub samples: Vec<CpuSamplePoint>,
    pub slope_bytes_per_s: f64,
    pub slope_mib_per_min: f64,
    pub slope_r2: f64,
    pub rss_start_bytes: u64,
    pub rss_end_bytes: u64,
    pub output_bytes_total: u64,
    pub flood_launches: usize,
    pub config_overrides: serde_json::Value,
    pub target_slope_mib_per_min: f64,
    pub status: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SlowConsumerResult {
    pub pause_seconds: f64,
    pub snapshot_dist: Percentiles,
    pub output_resumed: bool,
    pub target_p95_ms: f64,
    pub status: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueueWaitResult {
    pub concurrency: u32,
    pub queued_depth: usize,
    pub waits_ms: Vec<f64>,
    pub dist: Percentiles,
    pub queue_reason: String,
    pub status: String,
    pub notes: Vec<String>,
}

/// W1-11 매트릭스(데몬 쪽 반쪽): N세션 × M MiB 재생 비용과 RSS 델타.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplayMatrixResult {
    pub sessions: u32,
    pub per_session_mib: u64,
    pub replay_p50_ms: f64,
    pub replay_p95_ms: f64,
    pub replay_max_ms: f64,
    /// 모든 세션이 last_seq까지 재생됐는가(관찰; 실패는 journal_limit 사건).
    pub all_sessions_through: bool,
    pub daemon_rss_delta_bytes: u64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchReport {
    pub schema: String,
    pub generated_utc: String,
    /// "quick" (CI smoke, ≤ ~60 s) or "full".
    pub mode: String,
    pub build: BuildInfo,
    pub host: HostInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency: Option<LatencyResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_cpu: Option<IdleCpuResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flood_memory: Option<FloodMemoryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slow_consumer: Option<SlowConsumerResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_wait: Option<QueueWaitResult>,
    pub replay_matrix: Option<ReplayMatrixResult>,
    /// Per-benchmark hard errors (daemon died, protocol violation, ...).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub errors: std::collections::BTreeMap<String, String>,
}

/// Verdict against a target, with the debug-build caveat applied.
pub fn status(profile: &str, pass: bool) -> String {
    if profile == "release" {
        if pass {
            "PASS".into()
        } else {
            "FAIL".into()
        }
    } else if pass {
        "MEASURED-DEBUG (meets target; release build pending)".into()
    } else {
        "MEASURED-DEBUG (would FAIL)".into()
    }
}

pub fn measured_status(profile: &str) -> String {
    if profile == "release" {
        "MEASURED".into()
    } else {
        "MEASURED-DEBUG".into()
    }
}

pub fn dist_from(samples: &[f64]) -> Percentiles {
    let pick = |p: f64| crate::stats::percentile(samples.to_vec(), p).unwrap_or(0.0);
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    Percentiles {
        n: samples.len(),
        mean_ms: crate::stats::mean(samples).unwrap_or(0.0),
        p50_ms: pick(50.0),
        p95_ms: pick(95.0),
        p99_ms: pick(99.0),
        min_ms: sorted.first().copied().unwrap_or(0.0),
        max_ms: sorted.last().copied().unwrap_or(0.0),
        samples_ms: samples.to_vec(),
    }
}

/// UTC "YYYY-MM-DDTHH:MM:SSZ" from the wall clock (no chrono dependency).
pub fn utc_now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_iso8601(secs)
}

pub fn format_iso8601(unix_secs: u64) -> String {
    let days = unix_secs / 86_400;
    let rem = unix_secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Compact timestamp for result filenames: "20260905T153000Z".
pub fn compact_timestamp() -> String {
    utc_now_iso8601()
        .replace(['-', ':'], "")
        .replace(".000Z", "Z")
}

/// Howard Hinnant's civil-from-days (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Human summary (stdout). Mirrors the JSON sections.
pub fn render_summary(report: &BenchReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "== iyagi-bench ==\nmode={} build={} generated={}\nhost: {} {} | arch={} | cpus={} | {} | RAM {:.1} GiB\n",
        report.mode,
        report.build.profile,
        report.generated_utc,
        report.host.os,
        report.host.os_version,
        report.host.arch,
        report.host.cpu_count,
        report.host.cpu_brand,
        gib(report.host.memory_total_bytes),
    ));
    if let Some(l) = &report.latency {
        out.push_str(&format!(
            "[input_echo_latency]  n={} (warmup {}) p50={:.1}ms p95={:.1}ms p99={:.1}ms max={:.1}ms -> {} (targets p95<={:.0}ms p99<={:.0}ms)\n",
            l.dist.n, l.warmup_inputs, l.dist.p50_ms, l.dist.p95_ms, l.dist.p99_ms, l.dist.max_ms,
            l.status, l.target_p95_ms, l.target_p99_ms,
        ));
    }
    if let Some(i) = &report.idle_cpu {
        out.push_str(&format!(
            "[idle_cpu]            sessions={} window={:.0}s avg={:.4} cores max={:.4} cores daemon RSS avg={:.1} MiB max={:.1} MiB -> {} (target avg<={})\n",
            i.sessions,
            i.sample_seconds,
            i.avg_cores,
            i.max_cores,
            mib(i.daemon_rss_avg_bytes),
            mib(i.daemon_rss_max_bytes),
            i.status,
            i.target_avg_cores,
        ));
    }
    if let Some(f) = &report.flood_memory {
        out.push_str(&format!(
            "[flood_memory]        window={:.0}s (warmup {:.0}s) slope={:.2} MiB/min (r2={:.3}) RSS {} -> {} MiB output={:.1} MiB launches={} -> {} (target |slope|<{:.0} MiB/min)\n",
            f.measure_seconds,
            f.warmup_seconds,
            f.slope_mib_per_min,
            f.slope_r2,
            mib(f.rss_start_bytes),
            mib(f.rss_end_bytes),
            mib(f.output_bytes_total),
            f.flood_launches,
            f.status,
            f.target_slope_mib_per_min,
        ));
    }
    if let Some(s) = &report.slow_consumer {
        out.push_str(&format!(
            "[slow_consumer]       pause={:.0}s snapshot p50={:.1}ms p95={:.1}ms max={:.1}ms resumed={} -> {} (target p95<{:.0}ms)\n",
            s.pause_seconds, s.snapshot_dist.p50_ms, s.snapshot_dist.p95_ms, s.snapshot_dist.max_ms,
            s.output_resumed, s.status, s.target_p95_ms,
        ));
    }
    if let Some(q) = &report.queue_wait {
        out.push_str(&format!(
            "[queue_wait]          concurrency={} depth={} p50={:.0}ms p95={:.0}ms max={:.0}ms reason={} -> {}\n",
            q.concurrency, q.queued_depth, q.dist.p50_ms, q.dist.p95_ms, q.dist.max_ms, q.queue_reason, q.status,
        ));
    }
    for (name, error) in &report.errors {
        out.push_str(&format!("[{name}] ERROR: {error}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_epoch_is_1970() {
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso8601(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(format_iso8601(86_400), "1970-01-02T00:00:00Z");
    }

    #[test]
    fn iso8601_known_date() {
        // 2026-09-05T00:00:00Z == 1788566400 (verified against Python UTC)
        assert_eq!(format_iso8601(1_788_566_400), "2026-09-05T00:00:00Z");
        assert_eq!(format_iso8601(1_788_595_200), "2026-09-05T08:00:00Z");
        // Leap-year day: 2024-02-29T12:00:00Z == 1709208000
        assert_eq!(format_iso8601(1_709_208_000), "2024-02-29T12:00:00Z");
    }

    #[test]
    fn compact_timestamp_shape() {
        let ts = compact_timestamp();
        assert!(ts.starts_with('2'), "{ts}");
        assert_eq!(ts.len(), 16, "{ts}");
        assert!(ts.ends_with('Z'));
    }

    #[test]
    fn status_applies_debug_caveat() {
        assert_eq!(status("release", true), "PASS");
        assert_eq!(status("release", false), "FAIL");
        assert!(status("debug", true).starts_with("MEASURED-DEBUG"));
        assert!(status("debug", false).starts_with("MEASURED-DEBUG"));
        assert_eq!(measured_status("release"), "MEASURED");
        assert_eq!(measured_status("debug"), "MEASURED-DEBUG");
    }

    #[test]
    fn dist_from_matches_stats_percentiles() {
        let samples: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        let d = dist_from(&samples);
        assert_eq!(d.n, 100);
        let p95 = crate::stats::percentile(samples.clone(), 95.0).unwrap();
        assert!((d.p95_ms - p95).abs() < 1e-9);
        assert_eq!(d.min_ms, 1.0);
        assert_eq!(d.max_ms, 100.0);
    }

    #[test]
    fn full_report_serializes_with_required_keys() {
        let report = BenchReport {
            schema: SCHEMA.into(),
            generated_utc: "2026-09-05T00:00:00Z".into(),
            mode: "full".into(),
            build: BuildInfo {
                profile: "release".into(),
                caveat: "targets apply to release".into(),
            },
            host: HostInfo {
                os: "Windows".into(),
                os_version: "test".into(),
                arch: "x86_64".into(),
                cpu_count: 8,
                cpu_brand: "test-cpu".into(),
                memory_total_bytes: 1 << 30,
            },
            latency: Some(LatencyResult {
                inputs: 200,
                warmup_inputs: 20,
                dist: dist_from(&[1.0, 2.0, 3.0]),
                target_p95_ms: TARGET_ECHO_P95_MS,
                target_p99_ms: TARGET_ECHO_P99_MS,
                status: "PASS".into(),
                notes: vec!["loopback".into()],
            }),
            idle_cpu: None,
            flood_memory: Some(FloodMemoryResult {
                warmup_seconds: 8.0,
                measure_seconds: 30.0,
                samples: vec![],
                slope_bytes_per_s: 0.0,
                slope_mib_per_min: 0.0,
                slope_r2: 1.0,
                rss_start_bytes: 100 << 20,
                rss_end_bytes: 101 << 20,
                output_bytes_total: 1 << 20,
                flood_launches: 1,
                config_overrides: serde_json::json!({}),
                target_slope_mib_per_min: TARGET_FLOOD_SLOPE_MIB_PER_MIN,
                status: "PASS".into(),
                notes: vec![],
            }),
            slow_consumer: None,
            queue_wait: Some(QueueWaitResult {
                concurrency: 1,
                queued_depth: 4,
                waits_ms: vec![100.0],
                dist: dist_from(&[100.0]),
                queue_reason: "WAIT_CONCURRENCY".into(),
                status: "MEASURED".into(),
                notes: vec![],
            }),
            replay_matrix: None,
            errors: std::collections::BTreeMap::new(),
        };
        let text = serde_json::to_string(&report).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&text).expect("reparse");
        assert_eq!(value["schema"], SCHEMA);
        for key in [
            "generated_utc",
            "mode",
            "build.profile",
            "host.cpu_count",
            "latency.dist.p95_ms",
            "latency.target_p95_ms",
            "flood_memory.slope_mib_per_min",
            "queue_wait.waits_ms",
        ] {
            let mut node = &value;
            for part in key.split('.') {
                node = &node[part];
            }
            assert!(!node.is_null(), "missing {key}");
        }
        // Omitted sections stay absent.
        assert!(value.get("idle_cpu").is_none() || value["idle_cpu"].is_null());
        // Round trip.
        let back: BenchReport = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back, report);
    }
}
