//! 에이전트 세션 식별·복구 종단 테스트(spec `02-runner.md` §8).
//!
//! 실제 claude CLI 없이도 결정적으로 돈다: Claude Code의 **실행 중
//! 레지스트리**(`$CLAUDE_CONFIG_DIR/sessions/<pid>.json`) 모양을 그대로
//! 쓰는 가짜 CLI를 셸 워크로드로 띄운다. 가짜 CLI는 npm 설치 경로
//! (`node_modules/@anthropic-ai/claude-code/claude`)에 놓아 데몬의 감지
//! 서명(경로 표식)에 걸리게 한다 — 프로덕션 감지 경로를 그대로 지난다.
//!
//! `CLAUDE_CONFIG_DIR`은 데몬 프로세스와 PTY 양쪽에 전용 임시 디렉터리로
//! 넣는다. 개발자의 진짜 `~/.claude`는 읽지도 쓰지도 않는다.
//!
//! 확인하는 것: 스냅샷의 `agent.session_*`, `agent_session.list`의 active
//! 표시·cwd·출처, 워크로드 종료 시 `workload_exited` 마감,
//! `agent_session.report`의 pane 대조(모르는 워크로드는 기록하지 않는다),
//! `agent_session.forget`.

#![cfg(unix)]

mod common;

use common::{wait_workload_state, Client, DaemonProc};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// 가짜 CLI가 보고할 세션 id·이름(고정값이라 단정이 결정적이다).
const FAKE_SESSION_ID: &str = "7db2598e-c360-48fe-a2d5-0240993c9f7a";
const FAKE_SESSION_NAME: &str = "fixture-name";

/// hook만이 아는 세션(가짜 CLI의 레지스트리에는 없는 id).
const HOOK_ONLY_SESSION_ID: &str = "01a097e0-b0d4-7343-b27e-4ac4d3615822";

#[test]
fn opencode_hooks_keep_two_same_directory_terminals_distinct() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("node_modules/opencode-ai");
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("opencode");
    std::fs::write(&script, r#"#!/bin/sh
printf '{"hook_event_name":"SessionStart","session_id":"%s"}' "$IYAGI_TEST_SESSION" | "$IYAGI_DAEMON_BIN" --data-dir "$IYAGI_DATA_DIR" hook --agent opencode
while :; do sleep 1; done
"#).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let daemon = DaemonProc::spawn("opencode-session-hooks", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let mut launches = Vec::new();
    for id in ["ses_first", "ses_second"] {
        let mut request = shell_launch(&script, root.path(), "unused");
        request["env_overrides"] = json!({"IYAGI_TEST_SESSION": id});
        launches.push((id, client.request("workload.launch", request).unwrap()));
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let ready = launches.iter().all(|(id, launch)| {
            agent_of(&mut client, &launch["workload_id"]).is_some_and(|agent| {
                agent["agent"] == "opencode"
                    && agent["session_id"] == *id
                    && agent["pid"].as_u64().unwrap_or(0) > 0
            })
        });
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "OpenCode session marks were not observed"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // Early hook reports must survive process detection on subsequent ticks.
    std::thread::sleep(Duration::from_millis(1800));
    for (id, launch) in launches {
        let agent = agent_of(&mut client, &launch["workload_id"]).unwrap();
        assert_eq!(agent["session_id"], id);
        let rows = client
            .request(
                "agent_session.list",
                json!({
                    "workload_id": launch["workload_id"], "limit": 1,
                }),
            )
            .unwrap();
        assert_eq!(rows[0]["agent_session_id"], id);
        assert_eq!(rows[0]["pty_session_id"], launch["session_id"]);
        assert_eq!(rows[0]["active"], true);
        client
            .request(
                "workload.cancel",
                json!({
                    "request_id": common::uuid_v4(), "workload_id": launch["workload_id"],
                }),
            )
            .unwrap();
        wait_workload_state(
            &mut client,
            &launch["workload_id"],
            &["CANCELLED", "SUCCEEDED", "FAILED", "INTERRUPTED"],
            Duration::from_secs(20),
        );
        let rows = client
            .request(
                "agent_session.list",
                json!({
                    "workload_id": launch["workload_id"], "limit": 1,
                }),
            )
            .unwrap();
        assert_eq!(rows[0]["agent_session_id"], id);
        assert_eq!(rows[0]["active"], false);
    }
}

/// npm 설치 형태의 가짜 `claude`를 만든다. 돌려주는 경로가 실행 파일이다.
///
/// 디렉터리 이름에 `@anthropic-ai/claude-code`가 들어 있어야 한다 —
/// `#!/bin/sh` 스크립트는 커널이 argv를 `["/bin/sh", "<스크립트 경로>"]`로
/// 바꿔 실행하므로, 프로세스 이름이 아니라 **경로 표식**으로 감지된다
/// (실제 npm 설치의 `node .../claude-code/cli.js`와 같은 경로다).
fn write_fake_claude(root: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let dir = root.join("node_modules/@anthropic-ai/claude-code");
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let script = dir.join("claude");
    // `exec sleep`은 프로세스 이름을 바꿔 버리므로 쓰지 않는다 — 셸이
    // 그대로 남아 있어야 레지스트리의 pid와 감지된 pid가 같다.
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
set -e
sessions="$CLAUDE_CONFIG_DIR/sessions"
mkdir -p "$sessions"
registry="$sessions/$$.json"
trap 'rm -f "$registry"' EXIT HUP INT TERM
cwd=$(pwd -P)
mkdir -p "$CLAUDE_CONFIG_DIR/projects/fixture"
printf '{{}}\n' > "$CLAUDE_CONFIG_DIR/projects/fixture/{FAKE_SESSION_ID}.jsonl"
printf '{{"pid":%s,"sessionId":"%s","cwd":"%s","name":"%s","status":"idle","kind":"interactive","entrypoint":"cli"}}' \
  "$$" "{FAKE_SESSION_ID}" "$cwd" "{FAKE_SESSION_NAME}" > "$registry"
while : ; do sleep 1 ; done
"#
        ),
    )
    .expect("write fixture");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fixture");
    script
}

fn shell_launch(program: &std::path::Path, cwd: &std::path::Path, claude_home: &str) -> Value {
    json!({
        "request_id": common::uuid_v4(),
        "profile_id": common::uuid_v4(),
        "cwd": cwd.to_string_lossy(),
        "program": program.to_string_lossy(),
        "argv": [],
        // 데몬 환경에서도 상속되지만, 명시해 두면 PTY 쪽이 확실해진다.
        "env_overrides": { "CLAUDE_CONFIG_DIR": claude_home },
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

/// 이 워크로드의 `agent` 블록(없으면 None).
fn agent_of(control: &mut Client, workload_id: &Value) -> Option<Value> {
    common::snapshot_workload(control, workload_id)
        .and_then(|w| w.get("agent").cloned())
        .filter(|a| !a.is_null())
}

fn list_sessions(control: &mut Client) -> Vec<Value> {
    control
        .request("agent_session.list", json!({}))
        .expect("agent_session.list")
        .as_array()
        .cloned()
        .expect("list returns an array")
}

#[test]
fn registry_session_is_observed_listed_closed_and_forgotten() {
    let fixtures = tempfile::tempdir().expect("fixture root");
    let claude_home = tempfile::tempdir().expect("claude home");
    let claude_home_str = claude_home.path().to_string_lossy().into_owned();
    let program = write_fake_claude(fixtures.path());

    // 실행 cwd는 데몬이 정규화해서 기록한다 — 기대값도 같게 만든다.
    let cwd = tempfile::tempdir().expect("launch cwd");
    let canonical_cwd = std::fs::canonicalize(cwd.path()).expect("canonical cwd");

    let data_dir = tempfile::tempdir().expect("data dir").keep();
    let daemon = DaemonProc::spawn_with_env(
        data_dir,
        "agent-session-e2e",
        None,
        &[("CLAUDE_CONFIG_DIR", claude_home_str.as_str())],
    );
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let launch = control
        .request(
            "workload.launch",
            shell_launch(&program, cwd.path(), &claude_home_str),
        )
        .expect("launch fake claude");
    assert_eq!(launch["state"], "RUNNING", "got {launch}");
    let workload_id = launch["workload_id"].clone();

    // 1) 스냅샷: 감지 + 레지스트리로 알아낸 세션이 실린다.
    let deadline = Instant::now() + Duration::from_secs(15);
    let agent = loop {
        if let Some(agent) = agent_of(&mut control, &workload_id) {
            if agent["session_id"] == FAKE_SESSION_ID {
                break agent;
            }
        }
        assert!(
            Instant::now() < deadline,
            "session id was not resolved in 15s; agent: {:?}; daemon stderr tail: {}",
            agent_of(&mut control, &workload_id),
            std::fs::read_to_string(daemon.data_dir.join("daemon-stderr.log"))
                .unwrap_or_default()
                .lines()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .join(" | ")
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(agent["agent"], "claude");
    assert_eq!(agent["session_source"], "registry");
    assert_eq!(agent["session_name"], FAKE_SESSION_NAME);
    assert_eq!(agent["session_status"], "idle");

    // 2) 목록: 한 건, 이 워크로드에서 **살아 있는** 것으로 표시된다.
    let listed = list_sessions(&mut control);
    assert_eq!(
        listed.len(),
        1,
        "expected exactly one record, got {listed:?}"
    );
    let record = listed[0].clone();
    let record_id = record["id"].as_str().expect("record id").to_string();
    assert_eq!(record["agent_session_id"], FAKE_SESSION_ID);
    assert_eq!(record["agent"], "claude");
    assert_eq!(record["workload_id"], workload_id);
    assert_eq!(record["pty_session_id"], launch["session_id"]);
    assert_eq!(record["active"], true, "live pane must not offer recovery");
    assert_eq!(record["source"], "registry");
    assert_eq!(
        record["cwd"].as_str(),
        Some(canonical_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(record["title"], FAKE_SESSION_NAME);
    assert!(record["ended_at"].is_null(), "still running: {record}");

    // 3) hook 보고: 모르는 워크로드(조상 근거도 없음)는 기록하지 않는다 —
    //    hook 등록은 전역이라 남의 CLI 신호도 여기로 온다.
    let foreign = control
        .request(
            "agent_session.report",
            json!({
                "agent": "claude",
                "session_id": FAKE_SESSION_ID,
                "event": "prompt",
                "workload_id": common::uuid_v4(),
                "source": "claude-code-hook",
            }),
        )
        .expect("report foreign");
    assert_eq!(foreign["recorded"], false, "got {foreign}");
    assert!(foreign["workload_id"].is_null(), "got {foreign}");

    // 4) 같은 보고에 우리 워크로드 id를 실으면 기록된다(살아 있는 동안).
    let ours = control
        .request(
            "agent_session.report",
            json!({
                "agent": "claude",
                "session_id": FAKE_SESSION_ID,
                "event": "prompt",
                "workload_id": workload_id,
                "source": "claude-code-hook",
            }),
        )
        .expect("report ours");
    assert_eq!(ours["recorded"], true, "got {ours}");
    assert_eq!(ours["workload_id"], workload_id);

    // 4b) hook이 관찰과 **다른** 세션을 보고하면 행이 따로 생긴다(같은 pane에서
    //     다른 대화를 연 것이다). 관찰이 확정한 id가 더 확실하므로 배지는
    //     바뀌지 않는다 — 기록만 남는다.
    let hook_only = control
        .request(
            "agent_session.report",
            json!({
                "agent": "claude",
                "session_id": HOOK_ONLY_SESSION_ID,
                "event": "prompt",
                "workload_id": workload_id,
                "source": "claude-code-hook",
            }),
        )
        .expect("report hook-only session");
    assert_eq!(hook_only["recorded"], true, "got {hook_only}");
    let hook_row = list_sessions(&mut control)
        .into_iter()
        .find(|r| r["agent_session_id"] == HOOK_ONLY_SESSION_ID)
        .expect("hook-only row is listed");
    let hook_row_id = hook_row["id"].as_str().expect("row id").to_string();
    assert_eq!(hook_row["source"], "hook");
    assert_eq!(hook_row["workload_id"], workload_id);
    assert!(hook_row["ended_at"].is_null(), "still running: {hook_row}");
    // 배지는 결국 관찰값으로 돌아온다 — 감시 틱이 레지스트리를 다시 읽어
    // hook이 써 넣은 값을 덮는다(관찰이 hook보다 확실하다).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let badge = agent_of(&mut control, &workload_id);
        if badge.as_ref().map(|a| a["session_id"].clone()) == Some(json!(FAKE_SESSION_ID)) {
            assert_eq!(badge.expect("badge")["session_source"], "registry");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "observation never reclaimed the badge: {badge:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    // 5) 워크로드를 끝내면 세션 기록도 닫힌다(감시 루프가 마감한다).
    control
        .request(
            "workload.cancel",
            json!({ "request_id": common::uuid_v4(), "workload_id": workload_id }),
        )
        .expect("cancel");
    wait_workload_state(
        &mut control,
        &workload_id,
        &["CANCELLED", "SUCCEEDED", "FAILED", "INTERRUPTED"],
        Duration::from_secs(20),
    );

    let deadline = Instant::now() + Duration::from_secs(15);
    let closed = loop {
        let listed = list_sessions(&mut control);
        let found = listed
            .iter()
            .find(|r| r["id"].as_str() == Some(record_id.as_str()))
            .cloned();
        if let Some(record) = found.clone() {
            if !record["ended_at"].is_null() && record["active"] == false {
                break record;
            }
        }
        assert!(
            Instant::now() < deadline,
            "record was not closed within 15s: {found:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(closed["agent_session_id"], FAKE_SESSION_ID);
    assert_eq!(closed["end_reason"], "workload_exited");

    // 5b) hook만으로 생긴 행도 같이 닫힌다 — 감시 루프가 저장한 행만 닫던
    //     예전 동작에서는 이 행이 열린 채로 영영 남았다.
    let hook_closed = list_sessions(&mut control)
        .into_iter()
        .find(|r| r["id"].as_str() == Some(hook_row_id.as_str()))
        .expect("hook-only row survives as a recovery candidate");
    assert!(
        !hook_closed["ended_at"].is_null(),
        "hook-only row must be closed too: {hook_closed}"
    );
    assert_eq!(hook_closed["end_reason"], "workload_exited");
    assert_eq!(hook_closed["active"], false);

    // 6) 종료된(아직 레지스트리에 남아 있는) 워크로드에도 hook `end`는 붙는다.
    let ended = control
        .request(
            "agent_session.report",
            json!({
                "agent": "claude",
                "session_id": FAKE_SESSION_ID,
                "event": "end",
                "workload_id": workload_id,
                "source": "claude-code-hook",
            }),
        )
        .expect("report end");
    assert_eq!(ended["recorded"], true, "got {ended}");
    assert_eq!(ended["workload_id"], workload_id);

    // 7) 잊기: 목록에서 사라지고 두 번째 호출은 false다.
    let forgotten = control
        .request("agent_session.forget", json!({ "id": record_id }))
        .expect("forget");
    assert_eq!(forgotten["forgotten"], true);
    assert_eq!(
        control
            .request("agent_session.forget", json!({ "id": hook_row_id }))
            .expect("forget hook-only row")["forgotten"],
        true
    );
    assert!(
        list_sessions(&mut control).is_empty(),
        "list must be empty after forget"
    );
    let again = control
        .request("agent_session.forget", json!({ "id": record_id }))
        .expect("forget again");
    assert_eq!(again["forgotten"], false);
}

/// 에이전트를 **감지하지 못한** pane(평범한 셸·관리 실행)에서 hook만으로
/// 생긴 행도 워크로드가 끝나면 닫힌다.
///
/// 감시 루프는 자기가 저장한 행이 있을 때만 마감하므로, 감지가 없던
/// 워크로드에서는 아무도 닫지 않았다 — 그 행은 데몬이 다시 뜰 때까지
/// (`daemon_restart`) 열린 채로 남아 목록에서 살아 있는 대화처럼 보였다.
/// 이제는 종료 상태를 확정하는 지점이 닫는다(§8).
#[test]
fn hook_only_rows_close_when_the_workload_exits() {
    let daemon = DaemonProc::spawn("agent-session-hook-only", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    // 어떤 에이전트 서명에도 걸리지 않는 셸 워크로드 — 감시 루프는 이
    // 워크로드에 세션을 하나도 저장하지 않는다.
    let launch = control
        .request(
            "workload.launch",
            common::launch_request(
                "shell",
                &["exit", "--code", "0", "--delay-ms", "60000"],
                "1048576",
            ),
        )
        .expect("launch a plain shell workload");
    assert_eq!(launch["state"], "RUNNING", "got {launch}");
    let workload_id = launch["workload_id"].clone();

    let recorded = control
        .request(
            "agent_session.report",
            json!({
                "agent": "claude",
                "session_id": HOOK_ONLY_SESSION_ID,
                "event": "start",
                "workload_id": workload_id,
                "cwd": "/repo",
                "source": "claude-code-hook",
            }),
        )
        .expect("report a hook-only session");
    assert_eq!(recorded["recorded"], true, "got {recorded}");

    let listed = list_sessions(&mut control);
    assert_eq!(
        listed.len(),
        1,
        "expected the hook row only, got {listed:?}"
    );
    assert_eq!(listed[0]["source"], "hook");
    assert_eq!(listed[0]["agent_session_id"], HOOK_ONLY_SESSION_ID);
    assert!(listed[0]["ended_at"].is_null(), "still running: {listed:?}");

    control
        .request(
            "workload.cancel",
            json!({ "request_id": common::uuid_v4(), "workload_id": workload_id }),
        )
        .expect("cancel");
    wait_workload_state(
        &mut control,
        &workload_id,
        &["CANCELLED", "SUCCEEDED", "FAILED", "INTERRUPTED"],
        Duration::from_secs(20),
    );

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let row = list_sessions(&mut control)
            .into_iter()
            .find(|r| r["agent_session_id"] == HOOK_ONLY_SESSION_ID);
        if let Some(row) = &row {
            if !row["ended_at"].is_null() {
                assert_eq!(row["end_reason"], "workload_exited", "got {row}");
                assert_eq!(row["active"], false);
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "hook-only row was not closed within 15s: {row:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// 세션 이벤트를 보낼 pane을 전혀 짚을 수 없으면 아무것도 기록하지 않는다.
/// (데몬을 띄우기만 하고 워크로드는 없다.)
#[test]
fn report_without_any_pane_evidence_records_nothing() {
    let daemon = DaemonProc::spawn("agent-session-orphan", None);
    let (mut control, _) = Client::control(&daemon.endpoint, &daemon.token);

    let result = control
        .request(
            "agent_session.report",
            json!({
                "agent": "codex",
                "session_id": "01a097e0-b0d4-7343-b27e-4ac4d3615822",
                "event": "start",
                "ancestor_pids": [999_999, 999_998],
                "source": "codex-hook",
            }),
        )
        .expect("report");
    assert_eq!(result["recorded"], false, "got {result}");
    assert!(result["workload_id"].is_null());
    assert!(list_sessions(&mut control).is_empty());

    // 계약 위반은 조용한 무시가 아니라 INVALID_ARGUMENT다.
    let error = control
        .request(
            "agent_session.report",
            json!({
                "agent": "claude",
                "session_id": "../../etc/passwd",
                "event": "start",
                "source": "claude-code-hook",
            }),
        )
        .expect_err("hostile session id must be rejected");
    assert_eq!(error["code"], "INVALID_ARGUMENT", "got {error}");
}
