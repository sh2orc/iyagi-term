//! Benchmark 6 — scrollback×journal-replay memory matrix (SOTA_GAP_REVIEW
//! W1-11 잔여, 데몬 측 반쪽). 프론트(xterm 버퍼 실메모리)는 앱의 진단
//! 명령이 담당하고, 여기서는 재생 파이프라인 자체의 비용을 잰다:
//!
//! N개 세션에 attributed 넓은 줄 출력을 흘려 저널을 채우고, 뷰를 뗀 뒤
//! **재접속(전체 재생)** 하면서 (1) 재생 완료까지의 시간, (2) 재생 직후
//! ACK through_seq 도달, (3) 데몬 RSS 델타를 측정한다. 세션당 저널 상한
//! 안에서 재생이 터지지 않고 예산을 유지하는지가 관찰 목적이다.
//!
//! 합성 부하임을 명시한다 — 실제 AI CLI 출력 패턴과 섞지 않는다(§8.1).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{cancel_and_wait, note, Ctx};
use crate::daemon::{self, DaemonProc};
use crate::report::{measured_status, ReplayMatrixResult};
use crate::samplers::ProcSampler;
use crate::wire::{uuid_v4, Conn};

/// 매트릭스 차원: 세션 수 × 세션당 출력. 32세션 상한(limits.sessions)과
/// 저널 세션 상한(128 MiB) 안에서 잡는다.
const SESSIONS: usize = 8;
/// 세션당 MiB — debug 빌드 시간을 고려해 2 MiB(스펙 축소판, 문서에 명시).
const PER_SESSION_MIB: u64 = 2;

pub fn run(ctx: &Ctx) -> Result<ReplayMatrixResult, String> {
    note(format!(
        "replay_matrix: {SESSIONS} sessions x {PER_SESSION_MIB} MiB attributed output, detach + full replay, {} build",
        ctx.profile
    ));

    let mut daemon: DaemonProc = DaemonProc::spawn(
        &ctx.daemon_bin,
        "replay-matrix",
        Some(json!({"limits": {"sessions": 32}})),
    )?;
    let outcome = measure(ctx, &mut daemon);
    match &outcome {
        Ok(result) => note(format!(
            "replay_matrix: replay p50={:.0}ms p95={:.0}ms max={:.0}ms, all-through={} -> {}",
            result.replay_p50_ms,
            result.replay_p95_ms,
            result.replay_max_ms,
            result.all_sessions_through,
            result.status
        )),
        Err(err) => {
            note(format!("replay_matrix FAILED: {err}"));
            note(format!("daemon stderr tail:\n{}", daemon.stderr_tail()));
        }
    }
    let _ = daemon::shutdown(&mut daemon, None);
    if !ctx.keep_data && outcome.is_ok() {
        daemon::cleanup_data_dir(&daemon.data_dir);
    }
    outcome
}

/// Value 오류를 문자열로(이 시나리오의 Result<String> 규약).
fn req(control: &mut Conn, method: &str, params: Value) -> Result<Value, String> {
    control.request(method, params).map_err(|e| e.to_string())
}

fn measure(ctx: &Ctx, daemon: &mut DaemonProc) -> Result<ReplayMatrixResult, String> {
    let cwd = daemon.data_dir.clone();
    let mut control = Conn::control(&daemon.endpoint, &daemon.token)?;
    let data_token = control
        .data_token
        .clone()
        .ok_or("control hello issued no data token")?;
    let mut data = Conn::data(&daemon.endpoint, &data_token)?;
    let mut sampler = ProcSampler::new(daemon.pid);

    // ---- 1단계: 세션마다 flood를 채우고 종료까지 ACK를 흘린다.
    // 출력 프레임은 data 연결로 오고 ACK도 data 연결에서 보낸다(§4).
    let mut sessions: Vec<(String, String, String, String)> = Vec::new(); // (session, workload, view, epoch)
    for _ in 0..SESSIONS {
        let request = json!({
            "request_id": uuid_v4(),
            "profile_id": uuid_v4(),
            "cwd": cwd.to_string_lossy(),
            "program": ctx.fixture_bin.to_string_lossy(),
            "argv": ["flood", "--bytes", (PER_SESSION_MIB << 20).to_string(), "--chunk", "16384", "--seed", "11", "--hold-ms", "900000"],
            "env_overrides": {},
            "mode": "shell",
            "executor": {"kind": "local"},
            "cols": 80,
            "rows": 24,
            "priority": 1,
            "policy": {
                "reservation_bytes": "536870912",
                "cpu_slots": 1,
                "enforcement": "observe",
                "memory_max_bytes": null,
                "cpu_max_cores": null,
                "pids_max": null
            }
        });
        let launch = req(&mut control, "workload.launch", request)?;
        if launch["state"] != "RUNNING" {
            return Err(format!("launch returned {}", launch["state"]));
        }
        let session_id = launch["session_id"]
            .as_str()
            .ok_or("no session_id")?
            .to_string();
        let workload_id = launch["workload_id"]
            .as_str()
            .ok_or("no workload_id")?
            .to_string();
        let view_id = uuid_v4();
        let attach = req(
            &mut control,
            "session.attach",
            json!({"session_id": session_id, "view_id": view_id, "access": "writer"}),
        )?;
        let epoch = attach["epoch"].as_str().ok_or("no epoch")?.to_string();

        // flood 완료 = 3초 무출력(세션은 hold로 살아 있다). 그동안 ACK.
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last_output = Instant::now();
        loop {
            if let Some(frame) = data.try_recv_frame(Duration::from_millis(50)) {
                let payload = &frame.value["payload"];
                if payload["session_id"].as_str() == Some(session_id.as_str()) {
                    let seq: u64 = payload["seq"]
                        .as_str()
                        .and_then(|v| v.parse().ok())
                        .or_else(|| payload["seq"].as_u64())
                        .unwrap_or(0);
                    if seq > 0 {
                        last_output = Instant::now();
                        data.send_ack(&session_id, &epoch, seq);
                    }
                }
            } else if last_output.elapsed() > Duration::from_secs(3) {
                break;
            }
            if Instant::now() > deadline {
                return Err(format!("flood did not finish for {session_id}"));
            }
        }
        sessions.push((session_id, workload_id, view_id, epoch));
    }

    // ---- 2단계: 1단계 뷰를 분리하고 RSS 기준선 후 재접속 = 전체 재생.
    for (session_id, _, view_id, _) in &sessions {
        let _ = req(
            &mut control,
            "session.detach",
            json!({"session_id": session_id, "view_id": view_id}),
        );
    }
    let _ = sampler.refresh();
    let baseline_rss = sampler.refresh().map(|r| r.rss_bytes).unwrap_or(0);

    let mut replays_ms: Vec<f64> = Vec::new();
    let mut all_through = true;
    for (session_id, _, _, _) in &sessions {
        let started = Instant::now();
        let attach = req(
            &mut control,
            "session.attach",
            json!({"session_id": session_id, "view_id": uuid_v4(), "access": "writer"}),
        )?;
        let epoch = attach["epoch"].as_str().ok_or("no epoch")?.to_string();
        let last_seq: u64 = attach["last_seq"]
            .as_str()
            .and_then(|v| v.parse().ok())
            .or_else(|| attach["last_seq"].as_u64())
            .ok_or("no last_seq")?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut acked = 0u64;
        loop {
            if let Some(frame) = data.try_recv_frame(Duration::from_millis(50)) {
                let payload = &frame.value["payload"];
                if payload["session_id"].as_str() == Some(session_id.as_str()) {
                    let seq: u64 = payload["seq"]
                        .as_str()
                        .and_then(|v| v.parse().ok())
                        .or_else(|| payload["seq"].as_u64())
                        .unwrap_or(0);
                    if seq > acked {
                        acked = seq;
                        data.send_ack(session_id, &epoch, seq);
                        if seq >= last_seq {
                            break;
                        }
                    }
                }
            }
            if Instant::now() > deadline {
                all_through = false;
                break;
            }
        }
        replays_ms.push(started.elapsed().as_secs_f64() * 1000.0);
    }

    let after_rss = sampler.refresh().map(|r| r.rss_bytes).unwrap_or(0);
    for (_, workload_id, _, _) in &sessions {
        let _ = cancel_and_wait(&mut control, workload_id, Duration::from_secs(10));
    }

    replays_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = replays_ms[replays_ms.len() / 2];
    let p95_idx = ((replays_ms.len() as f64 * 0.95) as usize).min(replays_ms.len() - 1);
    let max = replays_ms[replays_ms.len() - 1];
    let status = if all_through {
        "MEASURED"
    } else {
        "INCOMPLETE"
    };

    Ok(ReplayMatrixResult {
        sessions: SESSIONS as u32,
        per_session_mib: PER_SESSION_MIB,
        replay_p50_ms: p50,
        replay_p95_ms: p95_idx as f64 * 0.0 + replays_ms[p95_idx],
        replay_max_ms: max,
        all_sessions_through: all_through,
        daemon_rss_delta_bytes: after_rss.saturating_sub(baseline_rss),
        status: measured_status(status),
    })
}
