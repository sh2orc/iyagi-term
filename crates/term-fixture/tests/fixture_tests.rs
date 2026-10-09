//! Integration tests for the fixture binary itself (pipe-level, no PTY).
//! PTY-level use of this binary lives in term-pty's integration tests.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

const EXE: &str = env!("CARGO_BIN_EXE_term-fixture");

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("term-fixture-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(EXE)
        .args(args)
        .output()
        .expect("term-fixture binary runs")
}

#[test]
fn echo_round_trips_raw_bytes_through_pipes() {
    let mut child = Command::new(EXE)
        .arg("echo")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn echo");
    let payload = "hello raw echo \u{Ac00}\u{1F600}\n".as_bytes().to_vec();
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&payload)
        .expect("write stdin");
    let mut out = Vec::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_end(&mut out)
        .expect("read stdout");
    let status = child.wait().expect("wait");
    assert!(status.success());
    assert_eq!(out, payload);
}

#[test]
fn exit_reports_code_after_delay() {
    let t0 = std::time::Instant::now();
    let out = run(&["exit", "--code", "7", "--delay-ms", "120"]);
    assert!(t0.elapsed() >= Duration::from_millis(100));
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn side_effect_creates_once_and_detects_duplicates() {
    let path = temp_path("side-effect");
    let first = run(&["side-effect", "--file", path.to_str().unwrap()]);
    assert_eq!(first.status.code(), Some(0));
    assert!(path.is_file());
    let second = run(&["side-effect", "--file", path.to_str().unwrap()]);
    assert_eq!(second.status.code(), Some(3));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn gate_observer_writes_marker_and_exits_zero() {
    let path = temp_path("gate-observer");
    assert!(!path.exists());
    let out = run(&["gate-observer", "--marker", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    let body = std::fs::read_to_string(&path).expect("marker readable");
    assert!(body.contains("pid="), "marker holds the pid: {body}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn flood_is_deterministic_per_seed() {
    let a = run(&[
        "flood", "--bytes", "65536", "--chunk", "4096", "--seed", "42",
    ]);
    let b = run(&[
        "flood", "--bytes", "65536", "--chunk", "4096", "--seed", "42",
    ]);
    let c = run(&[
        "flood", "--bytes", "65536", "--chunk", "4096", "--seed", "43",
    ]);
    assert_eq!(a.status.code(), Some(0));
    assert_eq!(
        a.stderr, b.stderr,
        "same seed must produce identical report"
    );
    assert_ne!(a.stderr, c.stderr, "different seed must change the pattern");
    assert_eq!(a.stdout.len(), 65536);
    let report = String::from_utf8(a.stderr).expect("stderr utf8");
    assert!(report.contains("bytes=65536"), "report: {report}");
    assert!(report.contains("sha256="), "report: {report}");
}

#[test]
fn io_writes_and_reads_back_requested_bytes() {
    let path = temp_path("io");
    let out = run(&["io", "--bytes", "1048576", "--file", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(std::fs::metadata(&path).expect("io file").len(), 1_048_576);
    let report = String::from_utf8(out.stderr).expect("stderr utf8");
    assert!(report.contains("bytes=1048576"), "report: {report}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn tree_root_can_exit_early_leaving_descendants() {
    // depth 2 x 2 children; root leaves after 50ms while leaves hold 800ms.
    // No piped stdio: descendants inherit the handles, and waiting on pipe
    // EOF would (correctly) block until they exit — we measure the ROOT only.
    let t0 = std::time::Instant::now();
    let mut child = Command::new(EXE)
        .args([
            "tree",
            "--children",
            "2",
            "--depth",
            "2",
            "--hold-ms",
            "800",
            "--root-early-exit-ms",
            "50",
        ])
        .spawn()
        .expect("spawn tree root");
    let status = child.wait().expect("wait root");
    assert_eq!(status.code(), Some(0));
    assert!(
        t0.elapsed() < Duration::from_millis(600),
        "root must not wait for descendants: {:?}",
        t0.elapsed()
    );
}
