//! 에이전트 세션 식별·복구 계약(spec `02-runner.md` §8, `04-ui.md` §5).
//!
//! iyagi은 셸 안에서 실행된 AI 코딩 CLI(claude/codex)의 **자체 세션 id**
//! (`claude --resume <id>` / `codex resume <id>`의 인자)를 알아내 SQLite에
//! 기록하고, PTY가 끊긴 뒤(데몬 재시작·재부팅) 사용자가 같은 대화를
//! 이어서 열 수 있게 한다.
//!
//! 식별 경로는 셋이다. (1) 프로세스 관찰 — Claude Code는
//! `~/.claude/sessions/<pid>.json` 레지스트리, Codex는 열어 둔
//! `~/.codex/thread-writer-locks/<thread-id>.lock` fd. (2) CLI 공식 hook
//! (`SessionStart`/`SessionEnd`…)이 `iyagi-termd hook`을 거쳐 보내는
//! [`AgentSessionReport`]. (3) 관리 실행에서 iyagi이 `--session-id`로
//! 선지정한 값.
//!
//! 계약 원칙:
//! * hook 페이로드는 **신뢰할 수 없는 외부 입력**이다 — 길이 상한을
//!   넘기면 잘라 쓰지 않고 거절한다.
//! * 기록되는 것은 세션 id·cwd·표시용 제목뿐이다. 대화 내용·argv·env는
//!   저장하지 않는다(01 §7). 프롬프트 원문은 제목으로도 쓰지 않는다.
//! * 자동 재실행은 없다. 복구는 사용자가 명시적으로 "이어서 열기"를
//!   눌렀을 때만 새 실행으로 일어난다(01 §6 재시작 규칙 유지).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{SessionId, WorkloadId};

/// 세션 id를 어디서 얻었는가.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionSource {
    /// Claude Code 실행 중 레지스트리(`~/.claude/sessions/<pid>.json`).
    Registry,
    /// Codex가 열어 둔 스레드 잠금 파일(`thread-writer-locks/<id>.lock`).
    LockFile,
    /// CLI 공식 hook(`iyagi-termd hook`)이 보고.
    Hook,
    /// iyagi이 실행 인수로 선지정(`--session-id`).
    Launch,
}

impl AgentSessionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentSessionSource::Registry => "registry",
            AgentSessionSource::LockFile => "lock_file",
            AgentSessionSource::Hook => "hook",
            AgentSessionSource::Launch => "launch",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "registry" => Some(AgentSessionSource::Registry),
            "lock_file" => Some(AgentSessionSource::LockFile),
            "hook" => Some(AgentSessionSource::Hook),
            "launch" => Some(AgentSessionSource::Launch),
            _ => None,
        }
    }
}

/// hook이 보고하는 세션 수명 이벤트(CLI hook 이름을 정규화한 것).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionEvent {
    /// 새 세션 시작(`SessionStart` source=startup).
    Start,
    /// 기존 세션 재개(`SessionStart` source=resume).
    Resume,
    /// 대화 초기화로 새 id 발급(`SessionStart` source=clear).
    Clear,
    /// 프롬프트 제출(`UserPromptSubmit`) — 살아 있음 신호.
    Prompt,
    /// 응답 종료(`Stop`) — 살아 있음 신호.
    Stop,
    /// 세션 종료(`SessionEnd`).
    End,
}

/// `agent_session.report` params — `iyagi-termd hook`이 보낸다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentSessionReport {
    /// 서명 테이블의 에이전트 id("claude" | "codex" | "opencode").
    pub agent: String,
    /// 에이전트 자체 세션(스레드) id.
    pub session_id: String,
    pub event: AgentSessionEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 세션 기록 파일 경로(hook `transcript_path`). 표시·검증 용도로만 쓴다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// PTY spawn 때 넣은 `IYAGI_SESSION_ID` — pane 연결의 1순위 근거.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty_session_id: Option<String>,
    /// PTY spawn 때 넣은 `IYAGI_WORKLOAD_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    /// hook 프로세스의 조상 pid 사슬(가까운 순). env가 없을 때 데몬이
    /// 감지된 에이전트 pid·셸 pid와 대조한다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ancestor_pids: Vec<u32>,
    /// 신호 출처(예: "claude-code-hook", "codex-hook"). 상한 64자.
    pub source: String,
}

/// `agent_session.report` 결과.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentSessionReportResult {
    /// 연결된 워크로드(못 찾으면 null — 이 경우 기록하지 않는다: iyagi
    /// 밖에서 실행된 CLI의 hook은 전역 등록이라 여기로도 오기 때문이다).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<WorkloadId>,
    pub recorded: bool,
}

/// `agent_session.list` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[ts(export)]
pub struct AgentSessionListParams {
    /// 최대 개수(기본 50, 상한 200). 응답에는 개수 상한 위에 바이트 예산도
    /// 걸린다 — 컨트롤 프레임(64 KiB)을 넘는 응답은 내보낼 수 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// 이 cwd의 세션만(없으면 전체).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 복구 대상 워크로드. PTY도 지정하면 둘 중 하나와 일치하는 기록만 조회한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<WorkloadId>,
    /// 복구 대상 PTY. 대상 필터는 중복 제거·개수·바이트 제한 전에 적용한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty_session_id: Option<SessionId>,
}

/// 기록된 에이전트 세션 한 건(`agent_sessions` 행 + 라이브 여부).
/// 같은 (agent, session_id)가 여러 워크로드에서 관찰됐으면 가장 최근
/// 것만 돌려준다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentSessionRecord {
    /// 행 id(UUID v4).
    pub id: String,
    /// 이 세션을 관찰한 워크로드.
    pub workload_id: WorkloadId,
    /// 그 워크로드의 PTY 세션(있으면).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty_session_id: Option<SessionId>,
    pub agent: String,
    pub agent_session_id: String,
    pub cwd: String,
    /// 표시용 제목(에이전트가 붙인 이름 등). 프롬프트 원문은 넣지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 관찰된 실행 파일 절대 경로(네이티브 바이너리일 때만). 복구 실행의
    /// 후보 — 없으면 UI가 PATH 탐색 결과를 쓴다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    pub source: AgentSessionSource,
    /// ISO-8601 UTC.
    pub first_seen_at: String,
    pub last_seen_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// 종료 사유 코드(`workload_exited` | `replaced` | `daemon_restart` |
    /// `hook_end`). 없으면 아직 종료를 관찰하지 못한 것.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
    /// 지금 이 데몬의 살아 있는 워크로드 안에서 관찰 중인가. true면
    /// 복구가 아니라 해당 pane으로 이동하면 된다.
    pub active: bool,
}

/// `agent_session.forget` params — 목록에서 한 건 제거.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentSessionForgetParams {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentSessionForgetResult {
    pub forgotten: bool,
}

/// 필드별 길이 상한.
pub mod limits {
    pub const AGENT_MAX: usize = 32;
    pub const SESSION_ID_MAX: usize = 128;
    pub const PATH_MAX: usize = 4_096;
    pub const TITLE_MAX: usize = 200;
    pub const SOURCE_MAX: usize = 64;
    pub const ANCESTOR_PIDS_MAX: usize = 64;
    pub const LIST_DEFAULT: u32 = 50;
    /// 한 응답에 담을 수 있는 최대 건수. 프레임 예산(64 KiB)이 먼저 걸리는
    /// 일이 없도록 넉넉히 낮춰 둔다 — 한 건이 경로·제목을 합쳐 수백 바이트다.
    pub const LIST_MAX: u32 = 200;
}

/// 세션 id 문자 규칙: UUID/이름 형태만 — 경로·제어 문자·공백 금지.
///
/// **첫 글자는 ASCII 영숫자여야 한다.** 이 값은 복구 실행에서
/// `codex resume <id>` / `claude --resume <id>`의 인자로 들어가므로
/// `-`로 시작하면 id가 아니라 플래그로 먹힌다(예:
/// `--dangerously-bypass-approvals-and-sandbox`). 같은 이유로 `.`·`..`는
/// 경로이므로 거절된다. 규칙은 여기 한 곳에만 있고 hook 검증·프로세스
/// 관찰·argv 추출·스토리지 쓰기가 모두 이 함수를 부른다.
pub fn valid_session_id(text: &str) -> bool {
    if text.is_empty() || text.len() > limits::SESSION_ID_MAX {
        return false;
    }
    if !text.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return false;
    }
    text.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

impl AgentSessionReport {
    /// 신뢰할 수 없는 입력 검증. 잘라내지 않고 통째로 거절한다.
    pub fn validate(&self) -> Result<(), &'static str> {
        use limits::*;
        if self.agent.trim().is_empty() || self.agent.len() > AGENT_MAX {
            return Err("agent must be 1..=32 chars");
        }
        if !self
            .agent
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err("agent must be a lowercase identifier");
        }
        if !valid_session_id(&self.session_id) {
            return Err("session_id must be 1..=128 identifier chars");
        }
        for (name, value) in [
            ("cwd", &self.cwd),
            ("transcript_path", &self.transcript_path),
        ] {
            if let Some(value) = value {
                if value.is_empty() || value.len() > PATH_MAX || value.chars().any(char::is_control)
                {
                    let _ = name;
                    return Err("path fields must be 1..=4096 control-free chars");
                }
            }
        }
        if let Some(id) = &self.pty_session_id {
            if SessionId::parse(id).is_err() {
                return Err("pty_session_id must be a UUID v4");
            }
        }
        if let Some(id) = &self.workload_id {
            if WorkloadId::parse(id).is_err() {
                return Err("workload_id must be a UUID v4");
            }
        }
        if self.ancestor_pids.len() > ANCESTOR_PIDS_MAX {
            return Err("ancestor_pids exceeds 64 entries");
        }
        if self.source.trim().is_empty() || self.source.len() > SOURCE_MAX {
            return Err("source must be 1..=64 non-space chars");
        }
        Ok(())
    }
}

impl AgentSessionListParams {
    /// 기본값·상한을 적용한 limit.
    pub fn effective_limit(&self) -> u32 {
        self.limit
            .unwrap_or(limits::LIST_DEFAULT)
            .clamp(1, limits::LIST_MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> AgentSessionReport {
        AgentSessionReport {
            agent: "claude".into(),
            session_id: "7db2598e-c360-48fe-a2d5-0240993c9f7a".into(),
            event: AgentSessionEvent::Start,
            cwd: Some("/repo".into()),
            transcript_path: None,
            pty_session_id: Some(SessionId::generate().as_str().to_string()),
            workload_id: None,
            ancestor_pids: vec![100, 10, 1],
            source: "claude-code-hook".into(),
        }
    }

    #[test]
    fn valid_report_round_trips_with_snake_case_event() {
        let r = report();
        assert!(r.validate().is_ok());
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"event\":\"start\""));
        assert!(!json.contains("transcript_path"));
        let back: AgentSessionReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn hostile_fields_are_rejected_not_clamped() {
        let mut r = report();
        r.session_id = "../../etc/passwd".into();
        assert!(r.validate().is_err());
        r = report();
        r.session_id = "x".repeat(129);
        assert!(r.validate().is_err());
        r = report();
        r.agent = "Claude Code".into();
        assert!(r.validate().is_err());
        r = report();
        r.cwd = Some("bad\ncwd".into());
        assert!(r.validate().is_err());
        r = report();
        r.pty_session_id = Some("not-a-uuid".into());
        assert!(r.validate().is_err());
        r = report();
        r.ancestor_pids = vec![1; 65];
        assert!(r.validate().is_err());
        r = report();
        r.source = String::new();
        assert!(r.validate().is_err());
    }

    #[test]
    fn source_round_trips_through_storage_strings() {
        for source in [
            AgentSessionSource::Registry,
            AgentSessionSource::LockFile,
            AgentSessionSource::Hook,
            AgentSessionSource::Launch,
        ] {
            assert_eq!(AgentSessionSource::parse(source.as_str()), Some(source));
            assert_eq!(
                serde_json::to_string(&source).unwrap(),
                format!("\"{}\"", source.as_str())
            );
        }
        assert_eq!(AgentSessionSource::parse("bogus"), None);
    }

    #[test]
    fn list_limit_applies_default_and_bounds() {
        assert_eq!(AgentSessionListParams::default().effective_limit(), 50);
        let params = AgentSessionListParams {
            limit: Some(0),
            cwd: None,
            ..Default::default()
        };
        assert_eq!(params.effective_limit(), 1);
        let params = AgentSessionListParams {
            limit: Some(10_000),
            cwd: None,
            ..Default::default()
        };
        assert_eq!(params.effective_limit(), 200);
    }

    /// 세션 id는 `codex resume <id>`의 인자가 된다 — `-`로 시작하는 값이
    /// 통과하면 심어 둔 레지스트리·잠금·hook id가 플래그로 먹힌다.
    #[test]
    fn session_ids_must_start_with_an_alphanumeric_character() {
        for bad in [
            "-x",
            "--yolo",
            "--dangerously-bypass-approvals-and-sandbox",
            ".",
            "..",
            "./x",
            "_leading",
            ":leading",
            "",
            "has space",
            "sla/sh",
        ] {
            assert!(!valid_session_id(bad), "{bad:?} must be rejected");
        }
        for good in [
            "7db2598e-c360-48fe-a2d5-0240993c9f7a",
            "01a097e0-b0d4-7343-b27e-4ac4d3615822",
            "0",
            "a",
            "a.b_c:d-e",
            &"x".repeat(limits::SESSION_ID_MAX),
        ] {
            assert!(valid_session_id(good), "{good:?} must be accepted");
        }
        assert!(!valid_session_id(&"x".repeat(limits::SESSION_ID_MAX + 1)));
    }

    #[test]
    fn record_omits_absent_optionals() {
        let record = AgentSessionRecord {
            id: "row".into(),
            workload_id: WorkloadId::generate(),
            pty_session_id: None,
            agent: "codex".into(),
            agent_session_id: "01a097e0-b0d4-7343-b27e-4ac4d3615822".into(),
            cwd: "/repo".into(),
            title: None,
            program: None,
            source: AgentSessionSource::LockFile,
            first_seen_at: "2026-09-13T00:00:00.000Z".into(),
            last_seen_at: "2026-09-13T00:00:01.000Z".into(),
            ended_at: None,
            end_reason: None,
            active: true,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("title"));
        assert!(!json.contains("ended_at"));
        assert!(json.contains("\"source\":\"lock_file\""));
    }
}
