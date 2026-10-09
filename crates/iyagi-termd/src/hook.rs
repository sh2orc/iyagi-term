//! `iyagi-termd hook [--agent claude|codex|opencode]` — CLI 공식 hooks가 부르는
//! 게이트웨이(개입 신호 SOTA_GAP_REVIEW W1-5 + 에이전트 세션 식별
//! spec `02-runner.md` §8).
//!
//! 동작: 표준 입력으로 hook 페이로드(JSON)를 한 번 읽고, 이벤트 이름에
//! 따라 둘 중 하나의 RPC를 실행 데몬에 보낸다.
//!
//! | hook 이벤트 | RPC |
//! |---|---|
//! | `SessionStart`/`SessionEnd`/`UserPromptSubmit`/`Stop` | `agent_session.report` |
//! | `Notification`/`PermissionRequest` | `intervention.report` |
//! | 그 밖 | 아무것도 하지 않는다 |
//!
//! **이 명령은 실행·승인·입력 주입을 하지 않는다**(§2.1: 알림 텍스트를
//! 읽고 PTY에 yes를 넣지 않는다). 그리고 **무슨 일이 있어도 0으로
//! 끝난다** — hook이 실패를 들고 돌아가면 CLI가 사용자에게 오류를 보인다.
//! 데몬이 꺼져 있는 것은 hook의 잘못이 아니다.
//!
//! 등록은 사용자 동의 아래 CLI 설정에 추가하는 형태다(앱이 대신 편집하지
//! 않는다). Claude Code(`~/.claude/settings.json`)는 `iyagi-termd hook`,
//! Codex(`~/.codex/hooks.json`)는 `iyagi-termd hook --agent codex`다.
//!
//! 어느 pane에서 왔는지는 세 근거를 이 순서로 쓴다: PTY가 넣어 준 환경
//! 변수 `IYAGI_SESSION_ID`/`IYAGI_WORKLOAD_ID`(hook은 CLI의
//! 환경을, CLI는 PTY의 환경을 물려받는다), 그리고 이 프로세스의 조상 pid
//! 사슬. 셋 다 맞지 않으면 데몬이 기록하지 않는다 — hook 등록은 전역이라
//! iyagi 밖에서 실행된 CLI의 신호도 여기로 오기 때문이다.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use term_contracts::agent_session::{AgentSessionEvent, AgentSessionReport};
use term_contracts::ids::{SessionId, WorkloadId};
use term_contracts::rpc::{self, methods, HelloRole};
use term_contracts::{InterventionKind, InterventionReport};

use crate::orchestrator::{ENV_SESSION_ID, ENV_WORKLOAD_ID};
use crate::paths::Paths;
use crate::sessions::SyncStream;

/// 표준 입력 상한 — hook 페이로드는 알림 문구지 로그가 아니다.
const STDIN_LIMIT: usize = 64 * 1024;

/// 프레임 한 번 읽기 상한. hook은 **CLI를 붙들고** 돌기 때문에(hook이
/// 끝나야 CLI가 다음 일을 한다) 데몬이 연결만 받아 놓고 조용한 상황에서
/// 무한정 기다려선 안 된다. 3초면 로컬 UDS 응답에는 넉넉하고 사람이 느끼는
/// 멈춤으로는 짧다.
const RPC_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// 응답 하나를 찾기까지의 전체 상한. 컨트롤 연결에는 브로드캐스트 이벤트가
/// 섞여 들어오므로 프레임을 여러 번 읽는데, 프레임마다
/// [`RPC_READ_TIMEOUT`]이 붙으면 최악이 64배가 된다.
const RPC_DEADLINE: Duration = Duration::from_secs(5);

/// hook 프로세스가 데몬에 보고할 내용. `Ignore`는 정상 종료다.
enum Action {
    AgentSession(Box<AgentSessionReport>),
    Intervention(Box<InterventionReport>),
    Ignore,
}

/// PTY가 넣어 준 식별자와 조상 pid 사슬. 테스트가 프로세스 환경을 건드리지
/// 않고 규칙을 확인할 수 있도록 값으로 받는다.
struct HookOrigin {
    pty_session_id: Option<String>,
    workload_id: Option<String>,
    ancestor_pids: Vec<u32>,
}

impl HookOrigin {
    /// 실제 프로세스 환경에서 읽는다.
    fn from_process() -> HookOrigin {
        HookOrigin {
            pty_session_id: std::env::var(ENV_SESSION_ID).ok(),
            workload_id: std::env::var(ENV_WORKLOAD_ID).ok(),
            ancestor_pids: term_platform::proc_scan::ancestor_pids(std::process::id()),
        }
    }
}

/// hook은 언제나 0으로 끝난다. 실패는 stderr 한 줄이다.
pub fn run(data_dir: &std::path::Path, agent: &str) -> i32 {
    if let Err(message) = report_from_stdin(data_dir, agent) {
        eprintln!("iyagi-termd hook: {message}");
    }
    0
}

fn report_from_stdin(data_dir: &std::path::Path, agent: &str) -> Result<(), String> {
    let mut input = String::new();
    std::io::stdin()
        .take(STDIN_LIMIT as u64)
        .read_to_string(&mut input)
        .map_err(|e| format!("stdin read: {e}"))?;
    let payload: serde_json::Value =
        serde_json::from_str(&input).map_err(|e| format!("stdin is not JSON: {e}"))?;

    let (method, params) = match classify(&payload, agent, &HookOrigin::from_process()) {
        Action::Ignore => return Ok(()),
        Action::AgentSession(report) => (
            methods::AGENT_SESSION_REPORT,
            serde_json::to_value(&*report).map_err(|e| format!("encode: {e}"))?,
        ),
        Action::Intervention(report) => (
            methods::INTERVENTION_REPORT,
            serde_json::to_value(&*report).map_err(|e| format!("encode: {e}"))?,
        ),
    };
    let reply = send_rpc(data_dir, method, params)?;
    if let Some(error) = reply.get("error") {
        return Err(format!("daemon rejected: {error}"));
    }
    Ok(())
}

/// hook 이벤트 이름 → 어느 보고인가. 세션 이벤트인데 세션 id를 못 읽으면
/// [`Action::Ignore`]다(아무것도 하지 않고 0으로 끝난다).
fn classify(payload: &serde_json::Value, agent: &str, origin: &HookOrigin) -> Action {
    let event = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match event {
        "Notification" | "PermissionRequest" => {
            match map_hook_payload(payload, &hook_source(agent)) {
                Ok(report) => Action::Intervention(Box::new(report)),
                Err(_) => Action::Ignore,
            }
        }
        // `Stop`은 개입 알림이 아니라 살아 있음 신호다 — 응답이 끝날 때마다
        // 울리므로 알림으로 띄우면 소음이 된다. `SubagentStop`은 세션
        // 수명과 무관해 아무 보고도 하지 않는다.
        _ => match map_agent_session_payload(payload, agent, origin) {
            Some(report) => Action::AgentSession(Box::new(report)),
            None => Action::Ignore,
        },
    }
}

/// 이 에이전트의 신호 출처 라벨(계약 `AgentSessionReport.source`).
fn hook_source(agent: &str) -> String {
    match agent {
        "claude" => "claude-code-hook".to_string(),
        other => format!("{other}-hook"),
    }
}

/// hook 이벤트 이름 + `source`(SessionStart 전용) → 세션 수명 이벤트.
fn map_event(hook_event: &str, start_source: Option<&str>) -> Option<AgentSessionEvent> {
    match hook_event {
        "SessionStart" => Some(match start_source.unwrap_or("startup") {
            // compact는 같은 대화를 이어 가는 것이므로 재개로 본다.
            "resume" | "compact" => AgentSessionEvent::Resume,
            "clear" => AgentSessionEvent::Clear,
            _ => AgentSessionEvent::Start,
        }),
        "SessionEnd" => Some(AgentSessionEvent::End),
        "UserPromptSubmit" => Some(AgentSessionEvent::Prompt),
        "Stop" => Some(AgentSessionEvent::Stop),
        _ => None,
    }
}

/// hook 페이로드 → [`AgentSessionReport`]. 세션 이벤트가 아니거나 세션
/// id가 없거나 계약 검증에 걸리면 `None`이다(잘라 쓰지 않는다).
fn map_agent_session_payload(
    payload: &serde_json::Value,
    agent: &str,
    origin: &HookOrigin,
) -> Option<AgentSessionReport> {
    let hook_event = payload.get("hook_event_name").and_then(|v| v.as_str())?;
    let start_source = payload.get("source").and_then(|v| v.as_str());
    let event = map_event(hook_event, start_source)?;

    // Claude는 `session_id`, Codex의 일부 이벤트는 `thread_id`를 쓴다.
    let session_id = ["session_id", "thread_id", "sessionId"]
        .iter()
        .find_map(|key| payload.get(*key).and_then(|v| v.as_str()))?;

    let report = AgentSessionReport {
        agent: agent.to_string(),
        session_id: session_id.to_string(),
        event,
        cwd: text_field(payload, "cwd"),
        transcript_path: text_field(payload, "transcript_path"),
        // 형식이 어긋난 환경 변수는 통째로 버린다 — 그 근거만 포기하면
        // 되지 보고 전체를 버릴 이유는 없다(조상 pid 대조가 남는다).
        pty_session_id: origin
            .pty_session_id
            .as_deref()
            .filter(|id| SessionId::parse(id).is_ok())
            .map(str::to_string),
        workload_id: origin
            .workload_id
            .as_deref()
            .filter(|id| WorkloadId::parse(id).is_ok())
            .map(str::to_string),
        ancestor_pids: origin.ancestor_pids.clone(),
        source: hook_source(agent),
    };
    report.validate().ok()?;
    Some(report)
}

fn text_field(payload: &serde_json::Value, key: &str) -> Option<String> {
    let text = payload.get(key)?.as_str()?;
    (!text.is_empty()).then(|| text.to_string())
}

// ---------------------------------------------------------------------------
// RPC 배선

/// 연결 → hello(control) → 요청 → 응답. 컨트롤 연결은 브로드캐스트
/// 이벤트도 받으므로 "다음 한 프레임"이 응답이라고 가정할 수 없다 —
/// 요청 id와 일치하는 프레임이 올 때까지 읽는다(상한 64프레임).
///
/// 읽기에는 타임아웃이 걸려 있다([`RPC_READ_TIMEOUT`]/[`RPC_DEADLINE`]).
/// 시간이 넘으면 다른 모든 실패와 똑같이 stderr 한 줄로 끝난다(종료 코드는
/// 언제나 0이다) — 데몬이 느린 것 때문에 CLI가 멈추면 안 된다.
fn send_rpc(
    data_dir: &std::path::Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let paths = Paths::init(data_dir).map_err(|e| format!("data dir: {e}"))?;
    let endpoint = paths.main_endpoint();
    let token = paths.read_token();
    let mut stream = SyncStream::connect(&endpoint)
        .map_err(|e| format!("daemon connect ({endpoint}): {e} — 데몬이 실행 중인지 확인하세요"))?;
    // 걸지 못해도 보고는 시도한다(그 환경에서는 예전처럼 블로킹이다).
    // 쓰기 타임아웃은 `SyncStream`이 제공하지 않는다 — 64 KiB 이하 한 프레임은
    // 소켓 버퍼에 바로 들어간다.
    let _ = stream.set_read_timeout(Some(RPC_READ_TIMEOUT));

    let hello_id = format!("hook-{}", std::process::id());
    let hello = serde_json::json!({
        "v": rpc::PROTOCOL_VERSION,
        "id": hello_id,
        "method": methods::HELLO,
        "params": {
            "client_id": format!("iyagi-hook-{}", std::process::id()),
            "token": token,
            "role": HelloRole::Control,
        },
    });
    write_frame(&mut stream, &hello)?;
    read_reply(&mut stream, &hello_id)?;

    let request_id = format!("hook-r-{}", std::process::id());
    let request = serde_json::json!({
        "v": rpc::PROTOCOL_VERSION,
        "id": request_id,
        "method": method,
        "params": params,
    });
    write_frame(&mut stream, &request)?;
    read_reply(&mut stream, &request_id)
}

/// 이벤트 프레임이 섞여 들어와도 요청 id가 일치하는 응답을 찾아 반환한다.
/// 프레임 수(64)와 시간([`RPC_DEADLINE`]) 둘 다로 막는다 — 이벤트가 쏟아지는
/// 데몬에 hook(그리고 CLI)이 붙들리면 안 된다.
fn read_reply(stream: &mut SyncStream, id: &str) -> Result<serde_json::Value, String> {
    let deadline = Instant::now() + RPC_DEADLINE;
    for _ in 0..64 {
        let frame = rpc::decode_frame(stream).map_err(|e| format!("frame decode: {e}"))?;
        if frame.get("id").and_then(|v| v.as_str()) == Some(id) {
            return Ok(frame);
        }
        // 이벤트(또는 남의 응답) — 건너뛴다.
        if Instant::now() >= deadline {
            return Err(format!(
                "no reply matching request id within {:?}",
                RPC_DEADLINE
            ));
        }
    }
    Err("no reply matching request id within 64 frames".to_string())
}

fn write_frame(stream: &mut SyncStream, value: &serde_json::Value) -> Result<(), String> {
    let bytes = rpc::encode_frame(value).map_err(|e| format!("frame encode: {e}"))?;
    stream
        .write_all(&bytes)
        .map_err(|e| format!("write: {e}"))?;
    stream.flush().map_err(|e| format!("flush: {e}"))
}

/// hook 페이로드 → InterventionReport. 매핑은 라벨링일 뿐 자동 행동이
/// 아니며, 값은 원문에서만 만든다(길면 잘랐다고 표시).
fn map_hook_payload(
    payload: &serde_json::Value,
    source: &str,
) -> Result<InterventionReport, String> {
    let event = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let message = payload
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let cwd = payload.get("cwd").and_then(|v| v.as_str()).unwrap_or("");

    let kind = match event {
        "Notification" | "PermissionRequest" => {
            if looks_like_permission(message) {
                InterventionKind::Permission
            } else {
                InterventionKind::Notification
            }
        }
        _ => InterventionKind::Notification,
    };

    let title = if !message.trim().is_empty() {
        bound(message.trim(), 200)
    } else {
        bound(
            match event {
                "" => "CLI hook 신호".to_string(),
                other => format!("{other} hook"),
            }
            .as_str(),
            200,
        )
    };

    // 멱등 키: 페이로드 내용 해시 — hook 재시도·에이전트 재실행이 같은
    // 알림을 두 번 띄우지 않게 한다.
    let report_id = deterministic_id(event, message, cwd);

    Ok(InterventionReport {
        report_id,
        kind,
        title,
        detail: if cwd.is_empty() {
            None
        } else {
            Some(bound(cwd, 2000))
        },
        session_hint: if cwd.is_empty() {
            None
        } else {
            Some(bound(cwd, 4096))
        },
        source: source.to_string(),
    })
}

/// 승인 문구 휴리스틱(라벨링 전용). 원문 키워드만 본다.
fn looks_like_permission(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("permission")
        || lower.contains("approve")
        || lower.contains("allow")
        || lower.contains("do you want")
        || lower.contains("proceed")
}

fn bound(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    // "…"(UTF-8 3바이트)를 포함해 상한 안에 들어가게 자른다.
    // char 경계에서 자른다(바이트 중간 절단 방지).
    let mut end = max.saturating_sub(3);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn deterministic_id(event: &str, message: &str, cwd: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (event, message, cwd).hash(&mut hasher);
    format!("hook-{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_SOURCE: &str = "claude-code-hook";
    const SESSION: &str = "7db2598e-c360-48fe-a2d5-0240993c9f7a";

    fn origin() -> HookOrigin {
        HookOrigin {
            pty_session_id: None,
            workload_id: None,
            ancestor_pids: Vec::new(),
        }
    }

    fn session_report(payload: &serde_json::Value, origin: &HookOrigin) -> AgentSessionReport {
        session_report_of("claude", payload, origin)
    }

    fn session_report_of(
        agent: &str,
        payload: &serde_json::Value,
        origin: &HookOrigin,
    ) -> AgentSessionReport {
        match classify(payload, agent, origin) {
            Action::AgentSession(report) => *report,
            Action::Intervention(_) => panic!("expected a session report, got an intervention"),
            Action::Ignore => panic!("expected a session report, got Ignore"),
        }
    }

    fn is_ignored(payload: &serde_json::Value) -> bool {
        matches!(classify(payload, "claude", &origin()), Action::Ignore)
    }

    // -- 개입 알림(기존 경로) ------------------------------------------------

    #[test]
    fn notification_with_permission_wording_maps_to_permission() {
        let payload = serde_json::json!({
            "hook_event_name": "Notification",
            "message": "Claude needs your permission to use Bash",
            "cwd": "/repo"
        });
        let report = map_hook_payload(&payload, CLAUDE_SOURCE).unwrap();
        assert_eq!(report.kind, InterventionKind::Permission);
        assert_eq!(report.session_hint.as_deref(), Some("/repo"));
        assert_eq!(report.source, "claude-code-hook");
        assert!(report.validate().is_ok());
        assert!(matches!(
            classify(&payload, "claude", &origin()),
            Action::Intervention(_)
        ));
    }

    /// `Stop`은 더 이상 개입 알림이 아니다 — 응답이 끝날 때마다 울려
    /// 알림 서랍을 채우던 소음을 끊었다. 대신 세션 살아 있음 신호다.
    #[test]
    fn stop_is_a_session_liveness_signal_and_idle_notification_survives() {
        let stop = serde_json::json!({
            "hook_event_name": "Stop", "cwd": "/repo", "session_id": SESSION
        });
        let report = session_report(&stop, &origin());
        assert_eq!(report.event, AgentSessionEvent::Stop);
        assert_eq!(report.cwd.as_deref(), Some("/repo"));

        // SubagentStop은 세션 수명과 무관하므로 아무 보고도 하지 않는다.
        assert!(is_ignored(&serde_json::json!({
            "hook_event_name": "SubagentStop", "session_id": SESSION
        })));

        let idle = serde_json::json!({ "hook_event_name": "Notification", "message": "waiting for input" });
        let report = map_hook_payload(&idle, CLAUDE_SOURCE).unwrap();
        assert_eq!(report.kind, InterventionKind::Notification);
        assert!(report.validate().is_ok());
    }

    #[test]
    fn same_payload_dedups_and_oversize_title_is_marked_truncated() {
        let payload =
            serde_json::json!({ "hook_event_name": "Notification", "message": "x".repeat(500) });
        let a = map_hook_payload(&payload, CLAUDE_SOURCE).unwrap();
        let b = map_hook_payload(&payload, CLAUDE_SOURCE).unwrap();
        assert_eq!(a.report_id, b.report_id);
        assert!(a.title.ends_with('…'));
        assert!(a.title.len() <= 201);
        assert!(a.validate().is_ok());
    }

    #[test]
    fn empty_payload_still_produces_a_valid_report() {
        let report = map_hook_payload(&serde_json::json!({}), CLAUDE_SOURCE).unwrap();
        assert!(report.validate().is_ok());
        assert_eq!(report.kind, InterventionKind::Notification);
        // 다만 라우팅에서는 이벤트 이름이 없으므로 아무것도 보내지 않는다.
        assert!(is_ignored(&serde_json::json!({})));
    }

    // -- 세션 수명 이벤트 ----------------------------------------------------

    #[test]
    fn session_start_source_selects_start_resume_or_clear() {
        for (source, expected) in [
            (None, AgentSessionEvent::Start),
            (Some("startup"), AgentSessionEvent::Start),
            (Some("resume"), AgentSessionEvent::Resume),
            (Some("compact"), AgentSessionEvent::Resume),
            (Some("clear"), AgentSessionEvent::Clear),
            (Some("something-new"), AgentSessionEvent::Start),
        ] {
            assert_eq!(
                map_event("SessionStart", source),
                Some(expected),
                "{source:?}"
            );
        }
        assert_eq!(map_event("SessionEnd", None), Some(AgentSessionEvent::End));
        assert_eq!(
            map_event("UserPromptSubmit", None),
            Some(AgentSessionEvent::Prompt)
        );
        assert_eq!(map_event("Stop", None), Some(AgentSessionEvent::Stop));
        for unknown in ["SubagentStop", "PreToolUse", "", "Notification"] {
            assert_eq!(map_event(unknown, None), None, "{unknown:?}");
        }
    }

    #[test]
    fn env_identifiers_ride_along_and_bad_ones_are_dropped() {
        let payload = serde_json::json!({
            "hook_event_name": "SessionStart",
            "source": "resume",
            "session_id": SESSION,
            "transcript_path": "/tmp/t.jsonl",
            "cwd": "/repo",
        });
        let pty = SessionId::generate();
        let workload = WorkloadId::generate();
        let good = HookOrigin {
            pty_session_id: Some(pty.as_str().to_string()),
            workload_id: Some(workload.as_str().to_string()),
            ancestor_pids: vec![100, 10, 1],
        };
        let report = session_report(&payload, &good);
        assert_eq!(report.event, AgentSessionEvent::Resume);
        assert_eq!(report.session_id, SESSION);
        assert_eq!(report.pty_session_id.as_deref(), Some(pty.as_str()));
        assert_eq!(report.workload_id.as_deref(), Some(workload.as_str()));
        assert_eq!(report.ancestor_pids, vec![100, 10, 1]);
        assert_eq!(report.transcript_path.as_deref(), Some("/tmp/t.jsonl"));
        assert_eq!(report.source, "claude-code-hook");

        // UUID가 아닌 환경 값은 버리되 보고 자체는 살린다(조상 pid가 남는다).
        let junk = HookOrigin {
            pty_session_id: Some("not-a-uuid".into()),
            workload_id: Some(String::new()),
            ancestor_pids: vec![7],
        };
        let report = session_report(&payload, &junk);
        assert_eq!(report.pty_session_id, None);
        assert_eq!(report.workload_id, None);
        assert_eq!(report.ancestor_pids, vec![7]);
        assert!(report.validate().is_ok());
    }

    #[test]
    fn session_id_falls_back_to_thread_id_and_camel_case() {
        for key in ["session_id", "thread_id", "sessionId"] {
            let payload = serde_json::json!({
                "hook_event_name": "SessionEnd", key: SESSION, "reason": "exit"
            });
            let report = session_report(&payload, &origin());
            assert_eq!(report.session_id, SESSION, "{key}");
            assert_eq!(report.event, AgentSessionEvent::End);
        }
    }

    #[test]
    fn missing_or_hostile_session_ids_do_nothing() {
        // id 없음.
        assert!(is_ignored(
            &serde_json::json!({"hook_event_name": "SessionStart"})
        ));
        // 경로 문자가 든 id는 계약 검증에서 걸린다.
        assert!(is_ignored(&serde_json::json!({
            "hook_event_name": "SessionStart", "session_id": "../../etc/passwd"
        })));
        // 상한을 넘는 id.
        assert!(is_ignored(&serde_json::json!({
            "hook_event_name": "SessionStart", "session_id": "x".repeat(129)
        })));
        // 제어 문자가 든 cwd는 보고 전체를 거절한다(자르지 않는다).
        assert!(is_ignored(&serde_json::json!({
            "hook_event_name": "SessionStart", "session_id": SESSION, "cwd": "bad\ncwd"
        })));
        // 모르는 이벤트.
        assert!(is_ignored(&serde_json::json!({
            "hook_event_name": "PreToolUse", "session_id": SESSION
        })));
    }

    /// Codex 0.154의 hook은 Claude Code와 **같은 envelope**를 보낸다(필드
    /// 이름까지). 실제 페이로드를 고정해 둬 매핑이 어긋나면 여기서 걸린다.
    #[test]
    fn codex_hook_payloads_map_to_start_and_end_with_the_codex_source() {
        let start = serde_json::json!({
            "session_id": SESSION,
            "transcript_path": "/Users/x/.codex/sessions/2026/09/13/rollout-2026-09-13T01-02-03-7db2598e-c360-48fe-a2d5-0240993c9f7a.jsonl",
            "cwd": "/Users/x/project/iyagi",
            "hook_event_name": "SessionStart",
            "source": "startup",
            "model": "gpt-5.6-codex",
            "permission_mode": "auto",
        });
        let report = session_report_of("codex", &start, &origin());
        assert_eq!(report.agent, "codex");
        assert_eq!(report.source, "codex-hook");
        assert_eq!(report.event, AgentSessionEvent::Start);
        assert_eq!(report.session_id, SESSION);
        assert_eq!(report.cwd.as_deref(), Some("/Users/x/project/iyagi"));
        assert!(report.transcript_path.is_some(), "경로는 검증·표시용으로만");
        assert!(report.validate().is_ok());
        // `model`·`permission_mode`는 계약에 없다 — 세션 기록으로 옮기지 않는다.
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("permission_mode"), "{encoded}");
        assert!(!encoded.contains("gpt-5.6-codex"), "{encoded}");

        let end = serde_json::json!({
            "session_id": SESSION,
            "cwd": "/Users/x/project/iyagi",
            "hook_event_name": "SessionEnd",
            "reason": "exit",
        });
        let report = session_report_of("codex", &end, &origin());
        assert_eq!(report.event, AgentSessionEvent::End);
        assert_eq!(report.source, "codex-hook");
        assert_eq!(report.session_id, SESSION);
        assert!(report.validate().is_ok());

        // `resume`로 다시 붙는 경우도 같은 envelope다.
        let resumed = serde_json::json!({
            "session_id": SESSION,
            "cwd": "/Users/x/project/iyagi",
            "hook_event_name": "SessionStart",
            "source": "resume",
        });
        assert_eq!(
            session_report_of("codex", &resumed, &origin()).event,
            AgentSessionEvent::Resume
        );
    }

    /// hook은 CLI를 붙들고 돌기 때문에 읽기 상한이 짧고 유한해야 한다.
    #[test]
    fn rpc_timeouts_stay_short_and_bounded() {
        assert_eq!(RPC_READ_TIMEOUT, Duration::from_secs(3));
        assert!(RPC_DEADLINE >= RPC_READ_TIMEOUT);
        assert!(RPC_DEADLINE <= Duration::from_secs(10));
    }

    #[test]
    fn the_agent_flag_picks_the_source_label() {
        assert_eq!(hook_source("claude"), "claude-code-hook");
        assert_eq!(hook_source("codex"), "codex-hook");
        let payload = serde_json::json!({
            "hook_event_name": "UserPromptSubmit", "session_id": SESSION
        });
        let report = match classify(&payload, "codex", &origin()) {
            Action::AgentSession(report) => *report,
            _ => panic!("expected a session report"),
        };
        assert_eq!(report.agent, "codex");
        assert_eq!(report.source, "codex-hook");
        assert_eq!(report.event, AgentSessionEvent::Prompt);
        assert!(report.validate().is_ok());
    }
}
