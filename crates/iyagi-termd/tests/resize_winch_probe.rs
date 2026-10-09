//! 실제 프로그램의 SIGWINCH 응답 계측 — 수동 실행 전용(`#[ignore]`).
//!
//! 줌/창 크기 최적화의 남은 가정을 실전 데이터로 확인한다:
//! - 실제 데몬에서 resize RPC 왕복과 저널 기록 배달까지의 시간
//! - TUI(vim)가 SIGWINCH에 답하기까지의 시간(덮개의 winchResponse)
//! - 일반 셸(zsh 프롬프트)이 답하는지(무응답 학습의 전제)
//!
//! 실행:
//! ```sh
//! cargo test -p iyagi-termd --test resize_winch_probe -- --ignored --nocapture
//! ```
//! Claude Code까지 재려면 `IYAGI_WINCH_CLAUDE=1`을 함께 붙인다.

mod common;

use common::{Client, DaemonProc};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

struct Round {
    rpc_us: u128,
    applied_us: u128,
    /// SIGWINCH 뒤 첫 출력(저널 기록 도착 기준). 없으면 None.
    answer_after_record_us: Option<u128>,
    answer_after_resize_us: Option<u128>,
    answer_bytes: usize,
}

fn launch_program(client: &mut Client, program: &str, argv: &[&str]) -> (String, String, String) {
    let mut req = common::launch_request("shell", &[], "1");
    req["program"] = json!(program);
    req["argv"] = json!(argv);
    // 데몬이 물려준 환경에는 TERM이 없다 — TUI는 이것만으로 죽는다.
    req["env_overrides"] = json!({
        "TERM": "xterm-256color",
        "COLORTERM": "truecolor",
        "HOME": std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()),
    });
    let launched = client.request("workload.launch", req).expect("launch");
    let session = launched["session_id"]
        .as_str()
        .expect("session_id")
        .to_owned();
    let workload = launched["workload_id"]
        .as_str()
        .expect("workload_id")
        .to_owned();
    let attached = client
        .request(
            "session.attach",
            json!({
                "session_id": session, "view_id": common::uuid_v4(), "access": "writer",
            }),
        )
        .expect("attach");
    let epoch = attached["epoch"].as_str().expect("epoch").to_owned();
    (workload, session, epoch)
}

/// 한 프로그램에 대해 `rounds`번 크기를 바꿔가며 단계별 시간을 잰다.
fn probe_rounds(
    control: &mut Client,
    data: &mut Client,
    session: &str,
    epoch: &str,
    tag: &str,
) -> Vec<Round> {
    let sizes = [
        (120, 36),
        (100, 30),
        (140, 44),
        (110, 33),
        (130, 39),
        (105, 31),
    ];
    let mut rounds = Vec::new();
    for (index, (cols, rows)) in sizes.into_iter().enumerate() {
        // 직전 라운드의 잔여 출력을 조용히 흘려 보낸다.
        while data.recv_any(Duration::from_millis(120)).is_some() {}
        let t0 = Instant::now();
        let request_id = control.next_request_id();
        let resize_id = format!("winch-{index}");
        control.fire(
            "session.resize",
            json!({
                "session_id": session, "epoch": epoch,
                "resize_id": resize_id, "cols": cols, "rows": rows,
            }),
        );
        // 1) RPC 응답(데몬이 저널 기록을 확인한 시점)
        let rpc_us;
        let applied_seq;
        loop {
            let frame = control
                .recv_any(Duration::from_secs(2))
                .expect("resize reply");
            if frame.get("id").and_then(Value::as_str) != Some(&request_id) {
                continue;
            }
            assert!(frame.get("error").is_none(), "resize failed: {frame}");
            rpc_us = t0.elapsed().as_micros();
            applied_seq = frame["result"]["applied_seq"]
                .as_str()
                .expect("applied_seq")
                .to_owned();
            break;
        }
        // 2) 데이터 연결로 크기 기록이 오는 시점 + 그 뒤 첫 출력(프로그램의 답)
        let mut applied_us: Option<u128> = None;
        let mut record_at: Option<Instant> = None;
        let mut answer_after_record_us: Option<u128> = None;
        let mut answer_after_resize_us: Option<u128> = None;
        let mut answer_bytes = 0usize;
        let deadline = Instant::now() + Duration::from_millis(700);
        while let Some(frame) = data.recv_any(deadline.saturating_duration_since(Instant::now())) {
            if frame.get("event").and_then(Value::as_str) != Some("session.output") {
                continue;
            }
            let payload = &frame["payload"];
            if payload["session_id"].as_str() != Some(session) {
                continue;
            }
            if payload["kind"] == "resize" {
                if payload["seq"].as_str() == Some(applied_seq.as_str()) {
                    applied_us = Some(t0.elapsed().as_micros());
                    record_at = Some(Instant::now());
                }
                continue;
            }
            // 출력 기록: 크기 기록을 본 뒤의 첫 출력이 답이다(저널 순서 보장).
            if applied_us.is_some() && answer_after_record_us.is_none() {
                let now = Instant::now();
                answer_after_record_us = Some((now - record_at.unwrap()).as_micros());
                answer_after_resize_us = Some(t0.elapsed().as_micros());
                answer_bytes += payload["raw_len"].as_u64().unwrap_or(0) as usize;
                break; // 첫 조각만 센다 — 덮개의 outputPending 시점과 같다.
            }
        }
        rounds.push(Round {
            rpc_us,
            applied_us: applied_us.expect("journal resize record"),
            answer_after_record_us,
            answer_after_resize_us,
            answer_bytes,
        });
        std::thread::sleep(Duration::from_millis(150));
    }
    eprintln!("== {tag} ==");
    for (i, r) in rounds.iter().enumerate() {
        eprintln!(
            "  round {i}: rpc {:>5}µs | record {:>5}µs | answer {} | bytes {}",
            r.rpc_us,
            r.applied_us,
            match r.answer_after_record_us {
                Some(us) => format!(
                    "{:>5}µs (총 {:>6}µs)",
                    us,
                    r.answer_after_resize_us.unwrap_or(0)
                ),
                None => "  silent ≥700ms".to_string(),
            },
            r.answer_bytes,
        );
    }
    rounds
}

fn probe(program: &str, argv: &[&str], tag: &str) {
    probe_with_boot_input(program, argv, tag, None);
}

fn probe_with_boot_input(program: &str, argv: &[&str], tag: &str, boot_input: Option<&str>) {
    let daemon = DaemonProc::spawn("winch-probe", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let mut data = Client::data(&daemon.endpoint, control.data_token.as_ref().unwrap());
    let (_workload, session, epoch) = launch_program(&mut control, program, argv);
    // 초기 화면(프롬프트/TUI 첫 그리기)이 나올 때까지 기다린다.
    let boot = Instant::now();
    let mut initial_bytes = 0u64;
    let mut initial_data = Vec::new();
    let mut exited = false;
    // 첫 출력(프롬프트/TUI 첫 화면)까지는 넉넉히 기다리고, 그 뒤 조용해질 때까지
    // 마저 흘린다. 부팅이 느린 프로그램(claude)을 600ms 컷으로 끊면 빈 화면
    // 상태에서 재게 된다.
    loop {
        let wait = if initial_bytes == 0 {
            Duration::from_secs(5)
        } else {
            Duration::from_millis(500)
        };
        let Some(frame) = data.recv_any(wait) else {
            break;
        };
        if frame.get("event").and_then(Value::as_str) == Some("session.exited") {
            exited = true;
        }
        if frame.get("event").and_then(Value::as_str) == Some("session.output") {
            initial_bytes += frame["payload"]["raw_len"].as_u64().unwrap_or(0);
            if let Some(b64) = frame["payload"]["data_b64"].as_str() {
                initial_data.extend(common::unb64(b64));
            }
        }
    }
    eprintln!(
        "== {tag}: 초기 출력 {initial_bytes}바이트, 부팅 후 {:?} ==",
        boot.elapsed()
    );
    let snapshot = control
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    if let Some(w) = snapshot["workloads"].as_array().and_then(|ws| {
        ws.iter()
            .find(|w| w["session_id"].as_str() == Some(session.as_str()))
    }) {
        eprintln!("   워크로드 상태: {}", w["state"]);
    }
    if std::env::var_os("IYAGI_WINCH_DUMP").is_some() {
        eprintln!(
            "   초기 출력(hex): {:?}",
            String::from_utf8_lossy(&initial_data)
        );
    }
    assert!(!exited, "{tag}: 프로그램이 이미 종료했다");
    assert!(
        initial_bytes > 0,
        "{tag}: 초기 출력이 없다 — 프로그램이 떴는지 확인"
    );
    if let Some(text) = boot_input {
        // hit-enter 프롬프트 등 부팅 메시지를 치운다(긴 경로로 메시지가 줄바꿈되면
        // vim이 "Press ENTER"에 멈춰 SIGWINCH 재그리기 관측이 막힌다).
        control
            .request(
                "session.input",
                json!({
                    "session_id": session, "epoch": epoch,
                    "input_id": common::uuid_v4(),
                    "data_b64": common::b64(text.as_bytes()),
                }),
            )
            .expect("boot input");
        let mut sink = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(400);
        while let Some(frame) = data.recv_any(deadline.saturating_duration_since(Instant::now())) {
            if frame.get("event").and_then(Value::as_str) == Some("session.output") {
                if let Some(b64) = frame["payload"]["data_b64"].as_str() {
                    sink.extend(common::unb64(b64));
                }
            }
        }
        eprintln!("   부팅 입력 후: {}바이트", sink.len());
    }
    probe_rounds(&mut control, &mut data, &session, &epoch, tag);
}

#[test]
#[ignore = "manual measurement against real programs"]
fn probe_zsh_prompt() {
    if !std::path::Path::new("/bin/zsh").is_file() {
        eprintln!("zsh 없음 — 건너뜀");
        return;
    }
    probe("/bin/zsh", &["-i"], "zsh -i (프롬프트)");
}

#[test]
#[ignore = "manual measurement against real programs"]
fn probe_vim_tui() {
    if !std::path::Path::new("/usr/bin/vim").is_file() {
        eprintln!("vim 없음 — 건너뜀");
        return;
    }
    std::fs::write("/tmp/iyagi-winch-probe.txt", "winch probe\n").unwrap();
    probe_with_boot_input(
        "/usr/bin/vim",
        &["-u", "NONE", "-i", "NONE", "/tmp/iyagi-winch-probe.txt"],
        "vim (TUI)",
        Some("\r"),
    );
}

#[test]
#[ignore = "opt-in: runs the real Claude Code TUI (IYAGI_WINCH_CLAUDE=1)"]
fn probe_claude_code() {
    if std::env::var_os("IYAGI_WINCH_CLAUDE").is_none() {
        eprintln!("IYAGI_WINCH_CLAUDE=1이 없어 건너뜀");
        return;
    }
    let out = std::process::Command::new("which")
        .arg("claude")
        .output()
        .expect("which claude");
    let claude = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if claude.is_empty() {
        eprintln!("claude 없음 — 건너뜀");
        return;
    }
    probe(&claude, &[], "claude (Claude Code TUI)");
}

#[test]
#[ignore = "diagnostic: does the kernel deliver SIGWINCH to daemon children"]
fn probe_winch_delivery() {
    let daemon = DaemonProc::spawn("winch-delivery", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);
    let mut data = Client::data(&daemon.endpoint, control.data_token.as_ref().unwrap());
    let (_w, session, epoch) = launch_program(
        &mut control,
        "/usr/bin/python3",
        &["-c", "import signal,time;\nsignal.signal(signal.SIGWINCH, lambda *a: print('WINCH-RECEIVED', flush=True))\nprint('READY', flush=True)\ntime.sleep(8)"],
    );
    let mut text = Vec::new();
    let read = |data: &mut Client, ms: u64, text: &mut Vec<u8>| {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while let Some(frame) = data.recv_any(deadline.saturating_duration_since(Instant::now())) {
            if frame.get("event").and_then(Value::as_str) != Some("session.output") {
                continue;
            }
            let payload = &frame["payload"];
            if payload["session_id"].as_str() == Some(session.as_str()) {
                if let Some(b64) = payload["data_b64"].as_str() {
                    text.extend(common::unb64(b64));
                }
            }
        }
    };
    read(&mut data, 800, &mut text);
    let _ = control.request(
        "session.resize",
        json!({"session_id": session, "epoch": epoch, "resize_id": "w1", "cols": 120, "rows": 36}),
    )
    .expect("resize");
    read(&mut data, 1500, &mut text);
    let out = String::from_utf8_lossy(&text);
    eprintln!("python output: {out:?}");
    assert!(out.contains("READY"), "no READY");
    assert!(
        out.contains("WINCH-RECEIVED"),
        "kernel did not deliver SIGWINCH to the child"
    );
}
