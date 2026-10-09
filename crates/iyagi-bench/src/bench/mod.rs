//! Benchmark implementations. Each one owns a fresh daemon on a hermetic
//! data dir, drives it over the real wire protocol, and returns its report
//! section (spec `06-verification.md` §5).

pub mod flood;
pub mod idle;
pub mod latency;
pub mod queue;
pub mod replay_matrix;

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::wire::{uuid_v4, Conn};

/// Shared run configuration.
pub struct Ctx {
    pub daemon_bin: PathBuf,
    pub fixture_bin: PathBuf,
    pub quick: bool,
    pub profile: &'static str,
    pub keep_data: bool,
}

/// Progress line (stdout).
pub fn note(msg: impl AsRef<str>) {
    println!("[bench] {}", msg.as_ref());
}

/// A launched+attached workload session.
pub struct LiveSession {
    pub workload_id: String,
    pub session_id: String,
    pub epoch: String,
}

/// Build a launch request shaped exactly like the app's (defaults.json
/// admission values, observe enforcement).
fn launch_request(fixture: &Path, args: &[&str], cwd: &Path, mode: &str, priority: u8) -> Value {
    json!({
        "request_id": uuid_v4(),
        "profile_id": uuid_v4(),
        "cwd": cwd.to_string_lossy(),
        "program": fixture.to_string_lossy(),
        "argv": args,
        "env_overrides": {},
        "mode": mode,
        "cols": 80,
        "rows": 24,
        "priority": priority,
        "policy": {
            "reservation_bytes": "2147483648",
            "cpu_slots": 1,
            "enforcement": "observe",
            "memory_max_bytes": null,
            "cpu_max_cores": null,
            "pids_max": null,
        },
    })
}

/// Launch a workload and attach a writer view on `control`. Shell mode is
/// direct (no gate); managed mode goes through the gated helper.
pub fn launch_and_attach(
    control: &mut Conn,
    ctx: &Ctx,
    args: &[&str],
    cwd: &Path,
    mode: &str,
) -> Result<LiveSession, String> {
    let request = launch_request(&ctx.fixture_bin, args, cwd, mode, 1);
    let launch = control
        .request("workload.launch", request)
        .map_err(|e| format!("launch ({mode}) failed: {e}"))?;
    let state = launch["state"].as_str().unwrap_or_default().to_string();
    if state != "RUNNING" {
        return Err(format!("launch returned state {state}: {launch}"));
    }
    let workload_id = launch["workload_id"]
        .as_str()
        .ok_or("launch result missing workload_id")?
        .to_string();
    let session_id = launch["session_id"]
        .as_str()
        .ok_or("launch result missing session_id")?
        .to_string();
    let epoch = attach_writer(control, &session_id)?;
    Ok(LiveSession {
        workload_id,
        session_id,
        epoch,
    })
}

/// Attach a writer view; returns the fresh epoch.
pub fn attach_writer(control: &mut Conn, session_id: &str) -> Result<String, String> {
    let attach = control
        .request(
            "session.attach",
            json!({"session_id": session_id, "view_id": uuid_v4(), "access": "writer"}),
        )
        .map_err(|e| format!("attach failed: {e}"))?;
    attach["epoch"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "attach result missing epoch".into())
}

/// Cancel a workload and wait (poll snapshot) for a terminal state.
pub fn cancel_and_wait(
    control: &mut Conn,
    workload_id: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let cancel = control
        .request(
            "workload.cancel",
            json!({"request_id": uuid_v4(), "workload_id": workload_id}),
        )
        .map_err(|e| format!("cancel failed: {e}"))?;
    if cancel["state"].as_str().is_none() {
        return Err(format!("cancel result missing state: {cancel}"));
    }
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let state = workload_state(control, workload_id)?;
        if matches!(
            state.as_str(),
            "CANCELLED" | "SUCCEEDED" | "FAILED" | "INTERRUPTED"
        ) {
            return Ok(state);
        }
        if std::time::Instant::now() > deadline {
            return Err(format!(
                "workload {workload_id} not terminal after cancel: {state}"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Current workload state from a snapshot.
pub fn workload_state(control: &mut Conn, workload_id: &str) -> Result<String, String> {
    let snapshot = control
        .request("system.snapshot", json!({}))
        .map_err(|e| format!("snapshot failed: {e}"))?;
    snapshot["workloads"]
        .as_array()
        .ok_or("snapshot.workloads missing")?
        .iter()
        .find(|w| w["workload_id"].as_str() == Some(workload_id))
        .and_then(|w| w["state"].as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("workload {workload_id} not in snapshot"))
}

/// Wait for a `workload.changed` event with the wanted state, returning the
/// event arrival instant. Scans stashed events first.
pub fn wait_workload_state_event(
    control: &mut Conn,
    workload_id: &str,
    want: &str,
    timeout: std::time::Duration,
) -> Result<std::time::Instant, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        // Stash scan first.
        let pos = control.events.iter().position(|f| {
            f.value.get("event").and_then(|v| v.as_str()) == Some("workload.changed")
                && f.value["payload"]["workload_id"].as_str() == Some(workload_id)
                && f.value["payload"]["state"].as_str() == Some(want)
        });
        if let Some(pos) = pos {
            let frame = control.events.remove(pos).expect("position just checked");
            return Ok(frame.at);
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "no workload.changed({want}) for {workload_id} within {timeout:?}"
            ));
        }
        match control.try_recv_frame(remaining.min(std::time::Duration::from_millis(200))) {
            Some(frame) => {
                if frame.value.get("event").is_some() {
                    control.events.push_back(frame);
                }
                // Stray responses are dropped (none expected here).
            }
            None if std::time::Instant::now() >= deadline => {
                return Err(format!(
                    "no workload.changed({want}) for {workload_id} within {timeout:?}"
                ));
            }
            None => {}
        }
    }
}
