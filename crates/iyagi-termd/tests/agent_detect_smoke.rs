//! 에이전트 자동 감지 실환경 스모크(기본 `#[ignore]` — CI 환경의 설치
//! 여부와 무관하게 로컬에서만 돈다):
//! `cargo test -p iyagi-termd --test agent_detect_smoke -- --ignored`
//!
//! 진짜 zsh 세션 안에서 `claude --version`을 반복 실행해 감시 루프가
//! WorkloadSummary.agent에 "claude"를 올리는지, 반복이 멈추면 내려가는지
//! 확인한다. claude는 API 인증 없이도 --version이 가능해 부작용이 없다.

mod common;

use common::{b64, Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

fn zsh_launch() -> serde_json::Value {
    json!({
        "request_id": common::uuid_v4(),
        "profile_id": common::uuid_v4(),
        "cwd": std::env::temp_dir().to_string_lossy(),
        "program": "/bin/zsh",
        "argv": [],
        "env_overrides": {},
        "mode": "shell",
        "cols": 80,
        "rows": 24,
        "priority": 1,
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

fn agent_of(control: &mut Client, workload_id: &str) -> Option<String> {
    let snapshot = control
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    snapshot["workloads"].as_array().and_then(|list| {
        list.iter()
            .find(|w| w["workload_id"] == workload_id)
            .and_then(|w| w["agent"]["agent"].as_str().map(str::to_string))
    })
}

#[test]
#[ignore = "requires a real claude CLI on PATH; run locally with --ignored"]
fn claude_in_shell_is_detected_and_cleared() {
    let daemon = DaemonProc::spawn("agent-smoke", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = control
        .request("workload.launch", zsh_launch())
        .expect("launch zsh");
    assert_eq!(launch["state"], "RUNNING", "got {launch}");
    let workload_id = launch["workload_id"].as_str().expect("wid").to_string();
    let session_id = launch["session_id"].as_str().expect("sid").to_string();

    let attach = control
        .request(
            "session.attach",
            json!({"session_id": session_id, "view_id": common::uuid_v4(), "access": "writer"}),
        )
        .expect("attach");
    let epoch = attach["epoch"].as_str().expect("epoch").to_string();

    assert_eq!(agent_of(&mut control, &workload_id), None, "no agent yet");

    // claude를 계속 재실행해 트리에 늘 한 마리가 살아 있게 한다.
    let spin = "while true; do claude --version >/dev/null 2>&1; done\n";
    control
        .request(
            "session.input",
            json!({
                "session_id": session_id,
                "epoch": epoch,
                "input_id": "spin",
                "data_b64": b64(spin.as_bytes()),
            }),
        )
        .expect("input spin");

    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if agent_of(&mut control, &workload_id).as_deref() == Some("claude") {
            break;
        }
        assert!(Instant::now() < deadline, "claude was not detected in 25s");
        std::thread::sleep(Duration::from_millis(300));
    }

    // 루프를 끊면(^\x03) 트리에서 claude가 사라지고 배지가 내려간다.
    control
        .request(
            "session.input",
            json!({
                "session_id": session_id,
                "epoch": epoch,
                "input_id": "stop",
                "data_b64": b64(b"\x03"),
            }),
        )
        .expect("input interrupt");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if agent_of(&mut control, &workload_id).is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "agent badge did not clear in 20s"
        );
        std::thread::sleep(Duration::from_millis(300));
    }

    control
        .request(
            "session.input",
            json!({
                "session_id": session_id,
                "epoch": epoch,
                "input_id": "exit",
                "data_b64": b64(b"exit\n"),
            }),
        )
        .expect("input exit");
}
