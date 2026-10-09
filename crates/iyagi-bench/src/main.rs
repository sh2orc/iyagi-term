//! # iyagi-bench
//!
//! Offline benchmark harness for the real `iyagi-termd` binary (spec
//! `06-verification.md` §5 초기 합격 목표). No LLM calls, no network beyond
//! the daemon's private IPC transport.
//!
//! Sections:
//! 1. `latency`  — input→echo p50/p95/p99 over the data connection
//! 2. `idle`     — daemon CPU cores + RSS with K idle attached sessions
//! 3. `flood`    — steady-state daemon RSS slope under continuous flood
//! 4. (in flood) — slow-consumer pause: control snapshot p95 + recovery
//! 5. `queue`    — queue wait p50/p95 at managed concurrency 1
//!
//! Results land in `scripts/bench/results/<timestamp>.json` plus a human
//! summary on stdout. Debug builds are measured with an explicit caveat:
//! the spec targets are for release builds.

mod bench;
mod daemon;
mod report;
mod samplers;
mod stats;
mod wire;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use report::{BenchReport, BuildInfo};

use crate::bench::Ctx;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BenchKind {
    Latency,
    Idle,
    Flood,
    Queue,
    ReplayMatrix,
}

#[derive(Debug, Parser)]
#[command(
    name = "iyagi-bench",
    about = "Offline benchmarks against the real iyagi-termd binary (spec 06 §5)",
    version,
    disable_help_subcommand = true
)]
struct Cli {
    /// CI smoke profile: reduced sizes, whole run in ~60 s.
    #[arg(long)]
    quick: bool,

    /// Measure the release binaries (target/release). Default: debug.
    #[arg(long)]
    release: bool,

    /// Explicit daemon binary (overrides target-dir lookup).
    #[arg(long)]
    daemon: Option<PathBuf>,

    /// Explicit term-fixture binary (overrides target-dir lookup).
    #[arg(long)]
    fixture: Option<PathBuf>,

    /// Results directory (default: <repo>/scripts/bench/results).
    #[arg(long)]
    out_dir: Option<PathBuf>,

    /// Run only the listed benchmarks (repeatable).
    #[arg(long = "filter", value_enum)]
    filter: Vec<BenchKind>,

    /// Keep daemon data dirs for post-mortem (under %TEMP%/iyagi-bench-*).
    #[arg(long)]
    keep_data: bool,
}

/// Repository root (the binary lives at <root>/target/<profile>/).
fn repo_root() -> PathBuf {
    if let Ok(root) = std::env::var("IYAGI_BENCH_ROOT") {
        return PathBuf::from(root);
    }
    if let Ok(exe) = std::env::current_exe() {
        // .../target/<profile>/iyagi-bench.exe -> .../
        if let Some(target_dir) = exe.parent().and_then(|p| p.parent()) {
            if target_dir.file_name().is_some_and(|n| n == "target") {
                if let Some(root) = target_dir.parent() {
                    return root.to_path_buf();
                }
            }
        }
        exe
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

fn resolve_bin(root: &Path, profile: &str, name: &str, explicit: &Option<PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path.clone();
    }
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    root.join("target").join(profile).join(file)
}

fn require_bin(path: &Path, hint: &str) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("binary not found at {} — {}", path.display(), hint))
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let _ = wire::time_begin_period_1ms();

    let profile = if cli.release { "release" } else { "debug" };
    let root = repo_root();
    let daemon_bin = resolve_bin(&root, profile, "iyagi-termd", &cli.daemon);
    let fixture_bin = resolve_bin(&root, profile, "term-fixture", &cli.fixture);
    if let Err(e) = require_bin(&daemon_bin, "build with: cargo build -p iyagi-termd") {
        eprintln!("iyagi-bench: {e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = require_bin(&fixture_bin, "build with: cargo build -p term-fixture") {
        eprintln!("iyagi-bench: {e}");
        return ExitCode::FAILURE;
    }

    let ctx = Ctx {
        daemon_bin: daemon_bin.clone(),
        fixture_bin: fixture_bin.clone(),
        quick: cli.quick,
        profile: if cli.release { "release" } else { "debug" },
        keep_data: cli.keep_data,
    };
    let selected =
        |kind: BenchKind| -> bool { cli.filter.is_empty() || cli.filter.contains(&kind) };

    let mut report = BenchReport {
        schema: report::SCHEMA.to_string(),
        generated_utc: report::utc_now_iso8601(),
        mode: if cli.quick { "quick" } else { "full" }.into(),
        build: BuildInfo {
            profile: profile.into(),
            caveat: "spec targets (echo p95/p99, idle CPU, flood slope) are defined for release builds; a debug measurement carries MEASURED-DEBUG".into(),
        },
        host: samplers::host_info(),
        latency: None,
        idle_cpu: None,
        flood_memory: None,
        slow_consumer: None,
        queue_wait: None,
        replay_matrix: None,
        errors: std::collections::BTreeMap::new(),
    };

    println!(
        "[bench] daemon={} fixture={} mode={} build={}",
        daemon_bin.display(),
        fixture_bin.display(),
        report.mode,
        profile
    );

    if selected(BenchKind::Latency) {
        match bench::latency::run(&ctx) {
            Ok(result) => report.latency = Some(result),
            Err(e) => {
                report.errors.insert("latency".into(), e);
            }
        }
    }
    if selected(BenchKind::Idle) {
        match bench::idle::run(&ctx) {
            Ok(result) => report.idle_cpu = Some(result),
            Err(e) => {
                report.errors.insert("idle_cpu".into(), e);
            }
        }
    }
    if selected(BenchKind::Flood) {
        match bench::flood::run(&ctx) {
            Ok((memory, slow)) => {
                report.flood_memory = Some(memory);
                report.slow_consumer = Some(slow);
            }
            Err(e) => {
                report.errors.insert("flood".into(), e);
            }
        }
    }
    if selected(BenchKind::ReplayMatrix) {
        match bench::replay_matrix::run(&ctx) {
            Ok(result) => report.replay_matrix = Some(result),
            Err(e) => {
                report.errors.insert("replay_matrix".into(), e);
            }
        }
    }
    if selected(BenchKind::Queue) {
        match bench::queue::run(&ctx) {
            Ok(result) => report.queue_wait = Some(result),
            Err(e) => {
                report.errors.insert("queue_wait".into(), e);
            }
        }
    }

    // Persist JSON.
    let out_dir = cli
        .out_dir
        .unwrap_or_else(|| root.join("scripts").join("bench").join("results"));
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("iyagi-bench: cannot create {}: {e}", out_dir.display());
    }
    let stamp = report::compact_timestamp();
    let out_path = out_dir.join(format!("{stamp}_{}_{}.json", report.mode, profile));
    match serde_json::to_string_pretty(&report) {
        Ok(text) => {
            let _ = std::fs::File::create(&out_path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .map_err(|e| eprintln!("iyagi-bench: write failed: {e}"));
        }
        Err(e) => eprintln!("iyagi-bench: serialize report: {e}"),
    }

    println!("{}", report::render_summary(&report));
    println!("results: {}", out_path.display());

    // Exit code: hard failures only count in release mode (debug is a
    // recorded caveat; errors in any mode are reported, never hidden).
    let any_fail = report.latency.as_ref().is_some_and(|r| r.status == "FAIL")
        || report.idle_cpu.as_ref().is_some_and(|r| r.status == "FAIL")
        || report
            .flood_memory
            .as_ref()
            .is_some_and(|r| r.status == "FAIL")
        || report
            .slow_consumer
            .as_ref()
            .is_some_and(|r| r.status == "FAIL")
        || !report.errors.is_empty();
    if any_fail && cli.release {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
