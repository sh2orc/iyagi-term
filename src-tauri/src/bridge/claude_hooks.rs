//! Claude Code hooks 연동(W1-5 잔여 + 세션 캡처 확장: 동의 기반 자동등록).
//!
//! 계약(§2.1 스펙 준수):
//! - 사용자 동의 없이 파일을 고치지 않는다. `status`가 현재 내용과
//!   적용 후 내용을 **모두** 돌려주고, UI가 diff를 보여 준 뒤 사용자가
//!   `apply`를 눌렀을 때만 쓴다.
//! - 쓰기는 원본 옆 `.iyagi.bak` 백업(최초 적용 전 원본 한 번만) +
//!   atomic rename. 손상된 JSON은 거절한다.
//! - 우리가 넣은 항목은 `command`를 argv로 토큰화해 식별한다 — 프로그램
//!   basename이 `iyagi-termd`(경로 인용·Windows `.exe` 무관)이고 첫 인자가
//!   `hook`이며 `--agent`가 없거나 `claude`인 엔트리. Codex 등록
//!   (`codex_hooks.rs`)과 같은 파일을 공유하지 않지만, 혹시 뒤섞여도 서로의
//!   엔트리를 건드리지 않는다. `remove`는 그 엔트리만 정확히 지운다 —
//!   사용자의 다른 hook은 절대 건드리지 않는다.
//! - 관리 대상 이벤트는 `Notification`(승인 알림) + `SessionStart` +
//!   `SessionEnd`(세션 시작·종료 — resume용 session-id 캡처, 데몬 쪽 처리는
//!   별도) 셋이다. 이미 있는 이벤트는 건드리지 않고 없는 이벤트만 채운다
//!   — 구버전(Notification만 등록)에서 업그레이드해도 기존 등록은 그대로
//!   두고 나머지 둘만 보강한다.
//! - JSON 조작의 실제 구현은 `hooks_json`(Claude·Codex 공용)에 있다 —
//!   이 모듈은 이벤트 목록·마커 판정자·타임아웃만 정하는 얇은 어댑터다.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::hooks_json::{self, HooksSpec};

pub use hooks_json::{HooksError, HooksStatus};

/// 관리 대상 이벤트(순서는 등록 순서에 영향을 주지 않는다 — 존재 여부만 본다).
pub const MANAGED_EVENTS: [&str; 3] = ["Notification", "SessionStart", "SessionEnd"];

/// 완전히 등록됐을 때의 엔트리 수.
pub const EXPECTED_ENTRIES: usize = MANAGED_EVENTS.len();

/// 우리가 넣는 hook 엔트리의 `timeout`(초).
const HOOK_TIMEOUT_SECS: u64 = 10;

/// Claude 엔트리 판정: 공용 argv 판정자에 "주체 없음(= claude)"을 고정해
/// 넘긴다 — `--agent codex`/`--agent gemini` 같은 다른 주체의 등록은 같은
/// 파일에 뒤섞여 있어도 자기 것으로 세지 않는다.
fn is_managed_command(command: &str) -> bool {
    hooks_json::is_managed_command(command, None)
}

fn spec() -> HooksSpec {
    HooksSpec {
        events: &MANAGED_EVENTS,
        is_marker: is_managed_command,
        timeout_secs: HOOK_TIMEOUT_SECS,
    }
}

/// `~/.claude/settings.json` 경로(home 없으면 오류).
pub fn claude_settings_path() -> Result<PathBuf, HooksError> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or(HooksError::NoHome)?;
    Ok(home.join(".claude").join("settings.json"))
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

    #[test]
    fn inject_is_idempotent_and_removable_without_touching_user_hooks() {
        let mut settings = json!({
            "model": "sonnet",
            "hooks": {
                "Notification": [
                    { "hooks": [ { "type": "command", "command": "echo user-own-hook" } ] }
                ],
                "Stop": [
                    { "hooks": [ { "type": "command", "command": "notify-send done" } ] }
                ]
            }
        });
        assert!(inject_hook(&mut settings, "/opt/iyagi-termd hook").unwrap());
        assert_eq!(count_managed(&settings), EXPECTED_ENTRIES);
        // 두 번째 주입은 거부(중복 금지) — 이미 세 이벤트 모두 등록됨.
        assert!(!inject_hook(&mut settings, "/opt/iyagi-termd hook").unwrap());
        assert_eq!(count_managed(&settings), EXPECTED_ENTRIES);

        // 제거는 우리 엔트리만: 사용자 Notification·Stop은 그대로.
        assert_eq!(remove_managed(&mut settings), EXPECTED_ENTRIES);
        assert_eq!(count_managed(&settings), 0);
        assert_eq!(
            settings["hooks"]["Notification"][0]["hooks"][0]["command"],
            "echo user-own-hook"
        );
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "notify-send done"
        );
        // SessionStart/SessionEnd는 우리만 넣었으므로 완전히 정리된다.
        assert!(settings["hooks"].get("SessionStart").is_none());
        assert!(settings["hooks"].get("SessionEnd").is_none());
    }

    #[test]
    fn inject_creates_missing_skeleton_for_all_three_events() {
        let mut settings = json!({ "theme": "dark" });
        assert!(inject_hook(&mut settings, "iyagi-termd hook").unwrap());
        for event in MANAGED_EVENTS {
            let cmd = settings["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert_eq!(cmd, "iyagi-termd hook");
            assert_eq!(settings["hooks"][event][0]["hooks"][0]["timeout"], 10);
        }
        assert_eq!(settings["theme"], "dark");
        assert_eq!(count_managed(&settings), EXPECTED_ENTRIES);
    }

    /// 구버전(0.x)은 Notification만 등록했다 — 업그레이드 시 기존
    /// Notification 등록은 그대로 두고 SessionStart/SessionEnd만 보강한다.
    #[test]
    fn upgrade_from_notification_only_adds_only_missing_events() {
        let mut settings = json!({
            "hooks": {
                "Notification": [
                    { "hooks": [ { "type": "command", "command": "iyagi-termd hook" } ] }
                ]
            }
        });
        assert_eq!(count_managed(&settings), 1);
        assert!(inject_hook(&mut settings, "iyagi-termd hook").unwrap());
        assert_eq!(count_managed(&settings), EXPECTED_ENTRIES);
        // 기존 Notification 그룹은 중복되지 않았다(여전히 배열 길이 1).
        assert_eq!(
            settings["hooks"]["Notification"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            settings["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "iyagi-termd hook"
        );
        assert_eq!(
            settings["hooks"]["SessionEnd"][0]["hooks"][0]["command"],
            "iyagi-termd hook"
        );
        // 다시 주입해도 변화 없음(이미 3/3).
        assert!(!inject_hook(&mut settings, "iyagi-termd hook").unwrap());
    }

    #[test]
    fn remove_on_empty_settings_is_noop() {
        let mut settings = json!({});
        assert_eq!(remove_managed(&mut settings), 0);
        assert_eq!(settings, json!({}));
    }

    /// Codex 엔트리(`--agent codex`)는 같은 파일에 섞여 있어도 Claude
    /// 판정자가 자기 것으로 세거나 지우지 않는다.
    #[test]
    fn codex_entries_are_not_counted_or_removed() {
        let mut settings = json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [ { "type": "command", "command": "iyagi-termd hook --agent codex", "timeout": 10 } ] }
                ]
            }
        });
        assert_eq!(count_managed(&settings), 0);
        assert!(inject_hook(&mut settings, "iyagi-termd hook").unwrap());
        // Codex 그룹은 그대로 두고 우리 그룹을 같은 배열에 추가한다.
        assert_eq!(
            settings["hooks"]["SessionStart"].as_array().unwrap().len(),
            2
        );
        assert_eq!(remove_managed(&mut settings), EXPECTED_ENTRIES);
        assert_eq!(
            settings["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "iyagi-termd hook --agent codex"
        );
    }

    #[test]
    fn file_roundtrip_with_backup_and_full_removal() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{\n  \"model\": \"opus\"\n}\n").unwrap();

        let st = status(&path, "iyagi-termd hook").unwrap();
        assert_eq!(st.managed_entries, 0);
        assert_eq!(st.expected_entries, EXPECTED_ENTRIES);
        assert!(st.proposed.is_some());

        let applied = apply(&path, "iyagi-termd hook").unwrap();
        assert_eq!(applied.managed_entries, EXPECTED_ENTRIES);
        assert!(applied.proposed.is_none());
        // 백업이 원문 그대로.
        let backup = std::fs::read_to_string(path.with_extension("json.iyagi.bak")).unwrap();
        assert!(backup.contains("opus"));
        // 적용 파일은 유효 JSON이고 timeout이 찍혀 있다.
        let written_text = std::fs::read_to_string(&path).unwrap();
        assert!(written_text.contains("\"timeout\": 10"));
        let written: Value = serde_json::from_str(&written_text).unwrap();
        assert_eq!(count_managed(&written), EXPECTED_ENTRIES);
        assert_eq!(written["model"], "opus");

        // apply를 다시 불러도 이미 3/3이므로 파일을 다시 쓰지 않는다(idempotent).
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let reapplied = apply(&path, "iyagi-termd hook").unwrap();
        assert_eq!(reapplied.managed_entries, EXPECTED_ENTRIES);
        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "idempotent apply must not rewrite the file"
        );

        // 전체 제거 — 세 이벤트 모두 지워지고 사용자 값은 남는다.
        let removed = remove(&path, "iyagi-termd hook").unwrap();
        assert_eq!(removed.managed_entries, 0);
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(count_managed(&after), 0);
        assert_eq!(after["model"], "opus");
        assert!(
            after.get("hooks").is_none(),
            "hooks key cleaned up entirely"
        );
    }

    #[test]
    fn corrupt_json_is_rejected_not_overwritten() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "not json").unwrap();
        let err = status(&path, "iyagi-termd hook").unwrap_err();
        assert_eq!(err.code(), "INVALID_JSON");
        // apply도 같은 이유로 거절 — 원문 훼손 없음.
        assert!(apply(&path, "iyagi-termd hook").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }

    /// 설치본의 등록 명령은 경로에 공백이 있어 인용된다
    /// (`'/Applications/IYAGI.app/…/iyagi-termd' hook`) — 그 형태도
    /// 세고 지울 수 있어야 한다(부분 문자열 마커가 깨지던 자리).
    #[test]
    fn quoted_installed_path_command_is_detected_and_removable() {
        let command = "'/Applications/IYAGI.app/Contents/MacOS/iyagi-termd' hook";
        let mut settings = json!({});
        assert!(inject_hook(&mut settings, command).unwrap());
        assert_eq!(count_managed(&settings), EXPECTED_ENTRIES);
        // 두 번째 적용이 중복 그룹을 덧붙이지 않는다(마커가 못 맞추면 계속 쌓였다).
        assert!(!inject_hook(&mut settings, command).unwrap());
        for event in MANAGED_EVENTS {
            assert_eq!(settings["hooks"][event].as_array().unwrap().len(), 1);
        }
        assert_eq!(remove_managed(&mut settings), EXPECTED_ENTRIES);
        assert!(settings.get("hooks").is_none());
    }

    /// 사용자가 손으로 만든 낯선 셰이프에서 `status`(= 설정 탭 열기)가
    /// 패닉하지 않고 INVALID_JSON으로 거절한다.
    #[test]
    fn status_refuses_strange_hooks_shapes_without_panicking() {
        let dir = tempfile::TempDir::new().unwrap();
        for (name, body) in [
            ("array.json", "{\"hooks\": []}"),
            ("object.json", "{\"hooks\": {\"SessionStart\": {}}}"),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, body).unwrap();
            let err = status(&path, "iyagi-termd hook").unwrap_err();
            assert_eq!(err.code(), "INVALID_JSON", "{name}");
            assert!(apply(&path, "iyagi-termd hook").is_err(), "{name}");
            // 거절은 원문을 건드리지 않는다.
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        }
    }

    /// 부분 등록(1/3, 2/3)에서도 `proposed`가 채워져 있어야 UI가 "마저
    /// 적용" 버튼을 보여 줄 수 있다.
    #[test]
    fn status_proposes_completion_while_partially_registered() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "Notification": [
                        { "hooks": [ { "type": "command", "command": "iyagi-termd hook", "timeout": 10 } ] }
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let st = status(&path, "iyagi-termd hook").unwrap();
        assert_eq!(st.managed_entries, 1);
        assert_eq!(st.expected_entries, EXPECTED_ENTRIES);
        assert!(
            st.proposed.is_some(),
            "1/3 registered must still propose completion"
        );

        let applied = apply(&path, "iyagi-termd hook").unwrap();
        assert_eq!(applied.managed_entries, EXPECTED_ENTRIES);
        assert!(applied.proposed.is_none());
    }
}
