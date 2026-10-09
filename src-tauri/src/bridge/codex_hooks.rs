//! Codex CLI hooks 연동(W1-5 확장: Claude Code와 동일 계약, 세션 캡처).
//!
//! 계약(§2.1 스펙 준수, `claude_hooks.rs`와 동일):
//! - 사용자 동의 없이 파일을 고치지 않는다. `status`가 현재 내용과
//!   적용 후 내용을 **모두** 돌려주고, UI가 diff를 보여 준 뒤 사용자가
//!   `apply`를 눌렀을 때만 쓴다.
//! - 쓰기는 원본 옆 `.iyagi.bak` 백업(최초 적용 전 원본 한 번만) +
//!   atomic rename. 손상된 JSON은 거절한다.
//! - 우리가 넣은 항목은 `command`를 argv로 토큰화해 식별한다 — 프로그램
//!   basename이 `iyagi-termd`(경로 인용·Windows `.exe` 무관)이고 첫 인자가
//!   `hook`이며 `--agent`가 `codex`(`--agent=codex`도 같다)인 엔트리.
//!   `remove`는 그 엔트리만 정확히 지운다 — 사용자의 다른 hook은 절대
//!   건드리지 않는다.
//! - 관리 대상 이벤트는 `SessionStart` + `SessionEnd` 둘뿐이다(Codex는
//!   Claude의 `Notification` 같은 승인 알림 훅이 없다). 이미 있는 이벤트는
//!   건드리지 않고 없는 이벤트만 채운다.
//! - Codex 0.154 기준 `<CODEX_HOME>/hooks.json`(`CODEX_HOME` 미설정이면
//!   `~/.codex/hooks.json`)이 대상이며, Claude Code의
//!   `~/.claude/settings.json`과 완전히 같은 JSON 셰이프
//!   (`{"hooks": {"<Event>": [{"hooks":[{"type":"command","command":…}]}]}}`)를
//!   쓴다 — 그래서 실제 JSON 조작은 전부 `hooks_json`(Claude와 공용)에
//!   있고, 이 모듈은 이벤트 목록·마커 판정자·타임아웃·경로 해석만 맡는
//!   얇은 어댑터다.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::hooks_json::{self, HooksSpec};

pub use hooks_json::{HooksError, HooksStatus};

/// 등록 주체 — 우리가 Codex용으로 넣는 명령은 `--agent codex`를 달고,
/// 판정도 이 값으로 한다(Claude 등록이나 미래의 다른 주체와 섞이지 않는다).
pub const HOOK_AGENT: &str = "codex";

/// 관리 대상 이벤트. Codex에는 Claude의 `Notification`에 해당하는 승인
/// 알림 훅이 없다 — 세션 생명주기 캡처만 등록한다.
pub const MANAGED_EVENTS: [&str; 2] = ["SessionStart", "SessionEnd"];

/// 완전히 등록됐을 때의 엔트리 수.
pub const EXPECTED_ENTRIES: usize = MANAGED_EVENTS.len();

/// 우리가 넣는 hook 엔트리의 `timeout`(초).
const HOOK_TIMEOUT_SECS: u64 = 10;

/// Codex 엔트리 판정: 공용 argv 판정자에 주체 `codex`를 고정해 넘긴다.
fn is_managed_command(command: &str) -> bool {
    hooks_json::is_managed_command(command, Some(HOOK_AGENT))
}

fn spec() -> HooksSpec {
    HooksSpec {
        events: &MANAGED_EVENTS,
        is_marker: is_managed_command,
        timeout_secs: HOOK_TIMEOUT_SECS,
    }
}

/// `<CODEX_HOME>/hooks.json` — `CODEX_HOME`(디렉터리)이 있으면 그 아래,
/// 없으면 `~/.codex/hooks.json`(home 없으면 오류).
pub fn codex_hooks_path() -> Result<PathBuf, HooksError> {
    // 클로저로 싸는 이유: `var_os`는 키 타입이 제네릭이라 함수 아이템으로
    // 넘기면 `Fn(&str)`의 HRTB(모든 수명)에 맞지 않는다.
    codex_hooks_path_from(|key| std::env::var_os(key))
}

/// 같은 해석을 주입된 환경 조회 함수로 — 테스트가 프로세스 환경을
/// 건드리지 않게 하려고 분리했다(`set_var`는 병렬로 도는 다른 테스트와
/// 경주해 간헐 실패를 만든다. 게다가 Rust 2024부터는 unsafe다).
pub fn codex_hooks_path_from(
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, HooksError> {
    if let Some(dir) = env("CODEX_HOME") {
        return Ok(PathBuf::from(dir).join("hooks.json"));
    }
    let home = env("HOME")
        .or_else(|| env("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or(HooksError::NoHome)?;
    Ok(home.join(".codex").join("hooks.json"))
}

/// 순수 로직: JSON 값에 아직 없는 관리 이벤트만 채운다. 낯선 셰이프
/// (`{"hooks": []}` 등)는 패닉이 아니라 `InvalidJson`으로 거절한다.
pub fn inject_hook(settings: &mut Value, hook_command: &str) -> Result<bool, HooksError> {
    hooks_json::inject_managed(settings, hook_command, &spec())
}

/// 순수 로직: 우리 엔트리만 모든 관리 이벤트에서 제거한다. 지운 개수를 반환.
pub fn remove_managed(settings: &mut Value) -> usize {
    hooks_json::remove_managed(settings, &spec())
}

/// 우리가 넣은 엔트리 수 세기(관리 이벤트 전체 합산).
pub fn count_managed(settings: &Value) -> usize {
    hooks_json::count_managed(settings, &spec())
}

/// 현재 상태 + 적용 제안(파일은 건드리지 않는다).
pub fn status(path: &Path, hook_command: &str) -> Result<HooksStatus, HooksError> {
    let current_value = hooks_json::read_settings(path)?;
    let current_text = current_value.as_ref().map(hooks_json::pretty).or_else(|| {
        // 파일은 있는데 내용이 없는 경우(빈 파일)도 "있음"으로 본다.
        if path.exists() {
            Some(String::new())
        } else {
            None
        }
    });
    let managed = current_value.as_ref().map(count_managed).unwrap_or(0);
    let proposed = if managed >= EXPECTED_ENTRIES {
        None
    } else {
        let mut next = current_value
            .clone()
            .unwrap_or_else(|| serde_json::json!({}));
        inject_hook(&mut next, hook_command)?;
        Some(hooks_json::pretty(&next))
    };
    Ok(HooksStatus {
        path: path.display().to_string(),
        exists: path.exists(),
        current: current_text,
        proposed,
        managed_entries: managed,
        expected_entries: EXPECTED_ENTRIES,
        hook_command: hook_command.to_string(),
    })
}

/// 동의 후 적용(백업 + atomic 쓰기, 이미 등록된 이벤트는 건드리지 않음).
/// 파일이 없으면 `{"hooks": {…}}` 형태로 새로 만든다.
pub fn apply(path: &Path, hook_command: &str) -> Result<HooksStatus, HooksError> {
    let mut value = hooks_json::read_settings(path)?.unwrap_or_else(|| serde_json::json!({}));
    if inject_hook(&mut value, hook_command)? {
        hooks_json::write_settings_file(path, &hooks_json::pretty(&value))?;
    }
    status(path, hook_command)
}

/// 우리가 넣은 훅만 모든 이벤트에서 제거(사용자 hook은 불가침).
pub fn remove(path: &Path, hook_command: &str) -> Result<HooksStatus, HooksError> {
    if let Some(mut value) = hooks_json::read_settings(path)? {
        if remove_managed(&mut value) > 0 {
            hooks_json::write_settings_file(path, &hooks_json::pretty(&value))?;
        }
    }
    status(path, hook_command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 환경은 주입한다 — 프로세스 전역 `set_var`는 병렬 테스트와 경주한다.
    fn fake_env(
        pairs: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<OsString> {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    #[test]
    fn codex_home_env_overrides_default_path() {
        let path = codex_hooks_path_from(fake_env(&[
            ("CODEX_HOME", "/custom/codex"),
            ("HOME", "/home/user"),
        ]))
        .unwrap();
        assert_eq!(path, PathBuf::from("/custom/codex/hooks.json"));
    }

    #[test]
    fn falls_back_to_home_codex_then_fails_without_home() {
        let path = codex_hooks_path_from(fake_env(&[("HOME", "/home/user")])).unwrap();
        assert_eq!(path, PathBuf::from("/home/user/.codex/hooks.json"));
        // Windows 계정은 USERPROFILE만 갖는다.
        let path = codex_hooks_path_from(fake_env(&[("USERPROFILE", "/users/me")])).unwrap();
        assert_eq!(path, PathBuf::from("/users/me/.codex/hooks.json"));
        let err = codex_hooks_path_from(fake_env(&[])).unwrap_err();
        assert_eq!(err.code(), "NO_HOME");
    }

    #[test]
    fn inject_is_idempotent_and_removable_without_touching_user_hooks() {
        let mut settings = json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [ { "type": "command", "command": "echo user-own-hook" } ] }
                ],
                "Stop": [
                    { "hooks": [ { "type": "command", "command": "notify-send done" } ] }
                ]
            }
        });
        assert!(inject_hook(&mut settings, "/opt/iyagi-termd hook --agent codex").unwrap());
        assert_eq!(count_managed(&settings), EXPECTED_ENTRIES);
        // 두 번째 주입은 거부(중복 금지) — 이미 두 이벤트 모두 등록됨.
        assert!(!inject_hook(&mut settings, "/opt/iyagi-termd hook --agent codex").unwrap());
        assert_eq!(count_managed(&settings), EXPECTED_ENTRIES);

        assert_eq!(remove_managed(&mut settings), EXPECTED_ENTRIES);
        assert_eq!(count_managed(&settings), 0);
        // 사용자의 SessionStart 엔트리·Stop 훅은 그대로.
        assert_eq!(
            settings["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "echo user-own-hook"
        );
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "notify-send done"
        );
        assert!(settings["hooks"].get("SessionEnd").is_none());
    }

    #[test]
    fn inject_creates_missing_skeleton_for_both_events_no_notification() {
        let mut settings = json!({ "profile": "default" });
        assert!(inject_hook(&mut settings, "iyagi-termd hook --agent codex").unwrap());
        for event in MANAGED_EVENTS {
            let cmd = settings["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert_eq!(cmd, "iyagi-termd hook --agent codex");
            assert_eq!(settings["hooks"][event][0]["hooks"][0]["timeout"], 10);
        }
        assert!(settings["hooks"].get("Notification").is_none());
        assert_eq!(settings["profile"], "default");
        assert_eq!(count_managed(&settings), EXPECTED_ENTRIES);
    }

    #[test]
    fn remove_on_empty_settings_is_noop() {
        let mut settings = json!({});
        assert_eq!(remove_managed(&mut settings), 0);
        assert_eq!(settings, json!({}));
    }

    /// Claude 전용 엔트리(`--agent codex` 없는 `iyagi-termd hook`)는 Codex
    /// 판정자가 자기 것으로 세거나 지우지 않는다.
    #[test]
    fn claude_entries_are_not_counted_or_removed() {
        let mut settings = json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [ { "type": "command", "command": "iyagi-termd hook", "timeout": 10 } ] }
                ]
            }
        });
        assert_eq!(count_managed(&settings), 0);
        assert!(inject_hook(&mut settings, "iyagi-termd hook --agent codex").unwrap());
        assert_eq!(
            settings["hooks"]["SessionStart"].as_array().unwrap().len(),
            2
        );
        assert_eq!(remove_managed(&mut settings), EXPECTED_ENTRIES);
        assert_eq!(
            settings["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "iyagi-termd hook"
        );
    }

    #[test]
    fn file_roundtrip_with_backup_and_full_removal() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(&path, "{\n  \"other\": true\n}\n").unwrap();

        let st = status(&path, "iyagi-termd hook --agent codex").unwrap();
        assert_eq!(st.managed_entries, 0);
        assert_eq!(st.expected_entries, EXPECTED_ENTRIES);
        assert!(st.proposed.is_some());

        let applied = apply(&path, "iyagi-termd hook --agent codex").unwrap();
        assert_eq!(applied.managed_entries, EXPECTED_ENTRIES);
        assert!(applied.proposed.is_none());
        let backup = std::fs::read_to_string(path.with_extension("json.iyagi.bak")).unwrap();
        assert!(backup.contains("other"));
        let written_text = std::fs::read_to_string(&path).unwrap();
        assert!(written_text.contains("\"timeout\": 10"));
        assert!(written_text.contains("--agent codex"));
        let written: Value = serde_json::from_str(&written_text).unwrap();
        assert_eq!(count_managed(&written), EXPECTED_ENTRIES);
        assert_eq!(written["other"], true);

        let removed = remove(&path, "iyagi-termd hook --agent codex").unwrap();
        assert_eq!(removed.managed_entries, 0);
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(count_managed(&after), 0);
        assert_eq!(after["other"], true);
        assert!(
            after.get("hooks").is_none(),
            "hooks key cleaned up entirely"
        );
    }

    #[test]
    fn missing_file_is_created_fresh_on_apply() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("hooks.json");
        assert!(!path.exists());

        let st = status(&path, "iyagi-termd hook --agent codex").unwrap();
        assert!(!st.exists);
        assert!(st.current.is_none());
        assert!(st.proposed.is_some());

        let applied = apply(&path, "iyagi-termd hook --agent codex").unwrap();
        assert_eq!(applied.managed_entries, EXPECTED_ENTRIES);
        assert!(path.exists());
        // 새로 만든 파일에 백업은 없다(원본이 없었으므로).
        assert!(!path.with_extension("json.iyagi.bak").exists());
    }

    #[test]
    fn corrupt_json_is_rejected_not_overwritten() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(&path, "not json").unwrap();
        let err = status(&path, "iyagi-termd hook --agent codex").unwrap_err();
        assert_eq!(err.code(), "INVALID_JSON");
        assert!(apply(&path, "iyagi-termd hook --agent codex").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }

    /// Windows 설치본(`iyagi-termd.exe`, 인용된 경로)과 `--agent=codex`
    /// 한 토큰 형태도 우리 것으로 세고 중복 없이 다룬다 — 부분 문자열
    /// 마커(`hook --agent codex`)가 둘 다 놓쳤던 자리.
    #[test]
    fn exe_and_inline_agent_forms_are_managed_without_duplicates() {
        let command = "\"C:\\Program Files\\IYAGI\\iyagi-termd.exe\" hook --agent=codex";
        let mut settings = json!({});
        assert!(inject_hook(&mut settings, command).unwrap());
        assert_eq!(count_managed(&settings), EXPECTED_ENTRIES);
        assert!(!inject_hook(&mut settings, command).unwrap());
        for event in MANAGED_EVENTS {
            assert_eq!(settings["hooks"][event].as_array().unwrap().len(), 1);
        }
        assert_eq!(remove_managed(&mut settings), EXPECTED_ENTRIES);
        assert!(settings.get("hooks").is_none());
    }

    #[test]
    fn status_proposes_completion_while_partially_registered() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "SessionStart": [
                        { "hooks": [ { "type": "command", "command": "iyagi-termd hook --agent codex", "timeout": 10 } ] }
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let st = status(&path, "iyagi-termd hook --agent codex").unwrap();
        assert_eq!(st.managed_entries, 1);
        assert_eq!(st.expected_entries, EXPECTED_ENTRIES);
        assert!(st.proposed.is_some());

        let applied = apply(&path, "iyagi-termd hook --agent codex").unwrap();
        assert_eq!(applied.managed_entries, EXPECTED_ENTRIES);
        assert!(applied.proposed.is_none());
    }
}
