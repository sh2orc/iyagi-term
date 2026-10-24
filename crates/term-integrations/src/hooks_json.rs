//! Claude Code·Codex 공용 hooks JSON 조작(W1-5 확장: Codex도 동일 계약).
//!
//! 두 CLI의 설정 파일은 완전히 같은 셰이프를 쓴다:
//! `{"hooks": {"<Event>": [{"hooks": [{"type":"command","command":"…","timeout":10}]}]}}`.
//! 그래서 파일 읽기/쓰기·주입·제거·카운트를 여기 한 곳에 모으고,
//! `claude_hooks.rs`/`codex_hooks.rs`는 이벤트 목록과 "이 엔트리가 우리
//! 것인가" 판정자만 다르게 넘기는 얇은 어댑터로 둔다(중복 금지).
//!
//! 계약(§2.1 스펙 준수, claude_hooks.rs와 동일):
//! - 사용자 동의 없이 파일을 고치지 않는다. `status`가 현재 내용과
//!   적용 후 내용을 **모두** 돌려주고, UI가 diff를 보여 준 뒤 사용자가
//!   `apply`를 눌렀을 때만 쓴다.
//! - 쓰기는 원본 옆 `.iyagi.bak` 백업(최초 적용 전 원본 한 번만) +
//!   고유 임시 파일 → `sync_all` → atomic rename. 손상된 JSON은 거절한다.
//! - 우리가 넣은 항목은 **argv 수준**으로 식별한다. 부분 문자열이 아니라
//!   `command`를 셸 인용 규칙으로 토큰화해 ① 프로그램 basename이
//!   `iyagi-termd`(Windows `.exe` 허용) ② 첫 인자가 `hook` ③ `--agent`가
//!   기대하는 주체와 같은지를 본다 — 설치본처럼 공백이 든 경로를 인용한
//!   명령(`'/Applications/IYAGI.app/Contents/MacOS/iyagi-termd' hook`)과
//!   Windows `iyagi-termd.exe hook`을 모두 정확히 집어내고, 사용자의 다른
//!   hook이나 미래의 `--agent gemini` 같은 제3자 등록은 절대 건드리지
//!   않는다(부분 문자열 마커는 이 세 경우 모두를 틀렸다).
//! - 이미 있는 이벤트는 건드리지 않는다(중복 추가 금지). 아직 없는
//!   이벤트만 채운다 — 구버전(Notification만 등록)에서 업그레이드해도
//!   기존 사용자 설정과 우리 기존 등록 모두 그대로 두고 나머지만 보강한다.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

/// 우리 hook 명령의 프로그램 이름(경로·`.exe`를 뗀 basename 기준).
const DAEMON_PROGRAM: &str = "iyagi-termd";

/// `--agent`를 생략했을 때의 등록 주체. Claude Code는 구버전부터 옵션
/// 없이 등록해 왔으므로 "옵션 없음 == claude"로 읽는다.
const CLAUDE_AGENT: &str = "claude";

/// 최초 적용 전 원본 백업의 접미사.
const BACKUP_SUFFIX: &str = ".iyagi.bak";

/// 설정 파일 쓰기 직렬화. 같은 `~/.claude/settings.json`을 hooks 연동과
/// status line 연동(`claude_usage`)이 함께 고치므로, 백업·임시 파일·rename
/// 순서가 서로 끼어들지 않도록 프로세스 전역으로 한 줄로 세운다(쓰기는
/// 사용자 클릭 단위라 경합이 사실상 없고, 잡는 구간도 짧다).
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 한 group(하나의 등록 단위)의 command가 "우리 것"인지 판정하는 함수.
/// 클로저가 아닌 fn 포인터만 받는다 — 캡처가 없어 두 CLI 모듈이
/// `is_managed_command`를 자기 주체로 고정해 넘기는 최소 판정자로
/// 충분하기 때문(테스트에서도 그대로 재사용 가능).
pub type MarkerPredicate = fn(&str) -> bool;

#[derive(Debug, thiserror::Error)]
pub enum HooksError {
    #[error("home directory not found")]
    NoHome,
    #[error("settings file unreadable: {0}")]
    Read(String),
    #[error("settings file is not valid JSON: {0}")]
    InvalidJson(String),
    #[error("write failed: {0}")]
    Write(String),
}

impl HooksError {
    pub fn code(&self) -> &'static str {
        match self {
            HooksError::NoHome => "NO_HOME",
            HooksError::Read(_) => "READ_FAILED",
            HooksError::InvalidJson(_) => "INVALID_JSON",
            HooksError::Write(_) => "WRITE_FAILED",
        }
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HooksStatus {
    pub path: String,
    pub exists: bool,
    /// 현재 파일 내용(없으면 null). UI가 그대로 보여 준다.
    pub current: Option<String>,
    /// 적용 후 전체 내용(변경이 없으면 None — 이미 모두 등록됨).
    pub proposed: Option<String>,
    /// 이미 우리 훅이 몇 개 등록돼 있는가(우리 엔트리를 가진 group 수).
    pub managed_entries: usize,
    /// 완전히 등록됐을 때의 개수(= 관리 대상 이벤트 수).
    pub expected_entries: usize,
    /// hook이 부를 실행 파일 경로(데몬 바이너리 — 못 찾으면 PATH 폴백 문자열).
    pub hook_command: String,
}

/// 한 CLI의 등록 규칙: 관리 대상 이벤트 목록 + 마커 판정자 + 타임아웃(초).
/// 완전 등록 시의 엔트리 수는 `events.len()`(이벤트 하나당 정확히 하나만
/// 관리) — 각 CLI 모듈이 자기 `EXPECTED_ENTRIES` 상수로 노출한다.
#[derive(Clone, Copy)]
pub struct HooksSpec {
    pub events: &'static [&'static str],
    pub is_marker: MarkerPredicate,
    pub timeout_secs: u64,
}

/// 셸 인용을 아는 최소 토크나이저: hook `command` 문자열을 argv로 쪼갠다.
/// 홑따옴표 안은 전부 글자 그대로, 겹따옴표 안은 `\" \\ \$ \``만
/// 이스케이프로 해석하고, 따옴표 밖 백슬래시는 다음 한 글자를 그대로
/// 통과시킨다 — POSIX sh 규칙의 부분집합으로, 우리가 쓰는 `shell_quote`
/// 출력과 사용자가 손으로 쓴 평범한 명령을 읽기에 충분하다.
/// (파이프·리다이렉션·변수 확장은 해석하지 않는다. 그런 명령은 첫 토큰이
/// 우리 바이너리가 아니므로 판정자가 자연히 "우리 것 아님"으로 본다.)
fn command_argv(command: &str) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            c if c.is_whitespace() => {
                if started {
                    argv.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    current.push(c);
                }
            }
            '"' => {
                started = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.peek() {
                            Some(&next @ ('"' | '\\' | '$' | '`')) => {
                                current.push(next);
                                chars.next();
                            }
                            _ => current.push('\\'),
                        },
                        _ => current.push(c),
                    }
                }
            }
            '\\' => {
                started = true;
                if let Some(c) = chars.next() {
                    current.push(c);
                }
            }
            _ => {
                started = true;
                current.push(ch);
            }
        }
    }
    if started {
        argv.push(current);
    }
    argv
}

/// 프로그램 토큰이 우리 데몬인가: 경로를 떼고 남은 basename이
/// `iyagi-termd`여야 한다. Windows의 `.exe` 접미사는 떼고, 대소문자를
/// 가리지 않는 파일 시스템을 고려해 대소문자도 무시한다.
fn program_is_daemon(program: &str) -> bool {
    let basename = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let stem = match basename.get(basename.len().saturating_sub(4)..) {
        Some(tail) if tail.eq_ignore_ascii_case(".exe") => &basename[..basename.len() - 4],
        _ => basename,
    };
    stem.eq_ignore_ascii_case(DAEMON_PROGRAM)
}

/// `--agent <x>` / `--agent=<x>`에 적힌 등록 주체. 옵션이 없으면 None,
/// 값이 빠진 채 끊긴 옵션은 빈 문자열("알 수 없는 주체" — 아무도 자기
/// 것으로 세지 않아 지우지도 않는다).
fn declared_agent(args: &[String]) -> Option<&str> {
    for (index, arg) in args.iter().enumerate() {
        if let Some(value) = arg.strip_prefix("--agent=") {
            return Some(value);
        }
        if arg == "--agent" {
            return Some(args.get(index + 1).map(String::as_str).unwrap_or(""));
        }
    }
    None
}

/// 이 `command`가 우리가 등록한 hook인가. `agent`는 호출하는 CLI 어댑터가
/// 기대하는 주체다 — Claude는 `None`(옵션 없음 또는 `claude`), Codex는
/// `Some("codex")`. 판정은 argv 기준이라 경로 인용·`.exe`·`--agent=` 형태를
/// 모두 같은 것으로 보고, 다른 주체(`--agent gemini`)나 사용자 명령은
/// 누구의 것도 아니다.
pub fn is_managed_command(command: &str, agent: Option<&str>) -> bool {
    let argv = command_argv(command);
    let Some(program) = argv.first() else {
        return false;
    };
    if !program_is_daemon(program) {
        return false;
    }
    if argv.get(1).map(String::as_str) != Some("hook") {
        return false;
    }
    let expected = agent.unwrap_or(CLAUDE_AGENT);
    match declared_agent(&argv[2..]) {
        // 주체 이름은 우리가 쓰는 그대로(소문자) 비교한다 — 데몬의 인자
        // 파서도 값을 그대로 받으므로, 다른 표기는 우리 등록이 아니다.
        Some(found) => found == expected,
        // 옵션이 없으면 Claude 등록(구버전 호환). Codex는 반드시 명시한다.
        None => expected == CLAUDE_AGENT,
    }
}

fn entry_is_managed(entry: &Value, is_marker: MarkerPredicate) -> bool {
    entry
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(is_marker)
}

/// group(`{"hooks":[...]}`)에 우리 엔트리가 **하나라도** 있는지. 사용자가
/// 자기 엔트리와 우리 엔트리를 한 group에 섞어 둔 경우에도 "등록됨"으로
/// 센다 — `all()`이었다면 그런 group을 영원히 못 알아보고 적용마다 중복
/// group을 덧붙였다(제거도 되지 않았다).
fn group_has_managed(group: &Value, is_marker: MarkerPredicate) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|entries| entries.iter().any(|e| entry_is_managed(e, is_marker)))
}

fn count_for_event(settings: &Value, event: &str, is_marker: MarkerPredicate) -> usize {
    settings
        .get("hooks")
        .and_then(|h| h.get(event))
        .and_then(|g| g.as_array())
        .map(|groups| {
            groups
                .iter()
                .filter(|group| group_has_managed(group, is_marker))
                .count()
        })
        .unwrap_or(0)
}

/// 관리 대상 이벤트 전체에서 우리 그룹 수를 합산.
pub fn count_managed(settings: &Value, spec: &HooksSpec) -> usize {
    spec.events
        .iter()
        .map(|event| count_for_event(settings, event, spec.is_marker))
        .sum()
}

/// 순수 로직: 아직 등록되지 않은 이벤트에만 우리 그룹을 추가한다. 이미
/// 그 이벤트에 우리 그룹이 있으면 건드리지 않는다(중복 추가 금지 +
/// 부분 등록 상태에서 나머지만 보강하는 업그레이드 경로). 하나라도
/// 추가했으면 `Ok(true)`.
///
/// 사용자가 손으로 만든 낯선 셰이프(`{"hooks": []}`, `{"hooks": {"X": {}}}`)는
/// 패닉이 아니라 `InvalidJson`으로 거절한다 — `status`가 미리보기를 만들 때도
/// 이 함수를 타므로, 패닉은 설정 탭을 여는 것만으로 앱을 죽였다.
pub fn inject_managed(
    settings: &mut Value,
    hook_command: &str,
    spec: &HooksSpec,
) -> Result<bool, HooksError> {
    if !settings.is_object() {
        *settings = json!({});
    }
    let mut changed = false;
    for event in spec.events {
        if count_for_event(settings, event, spec.is_marker) > 0 {
            continue; // 이 이벤트는 이미 등록됨 — 중복 추가 금지
        }
        let obj = settings
            .as_object_mut()
            .expect("normalized to object above");
        let hooks_obj = obj
            .entry("hooks")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| HooksError::InvalidJson("hooks is not an object".to_string()))?;
        let group_array = hooks_obj
            .entry((*event).to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| HooksError::InvalidJson(format!("{event} group is not an array")))?;
        group_array.push(json!({
            "hooks": [
                { "type": "command", "command": hook_command, "timeout": spec.timeout_secs }
            ]
        }));
        changed = true;
    }
    Ok(changed)
}

/// 순수 로직: 관리 대상 이벤트 전부에서 우리 엔트리만 제거한다. 우리
/// 엔트리를 가진 group 안에 사용자 엔트리가 섞여 있으면 그 엔트리는 남기고
/// (group이 비면 group 자체를 치운다), 우리 엔트리가 없는 group은 아예
/// 건드리지 않는다. 빈 배열/객체는 정리한다. 지운 group 수를 반환.
pub fn remove_managed(settings: &mut Value, spec: &HooksSpec) -> usize {
    let mut removed = 0;
    {
        let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
            return 0;
        };
        let mut empty_events: Vec<String> = Vec::new();
        for event in spec.events {
            let Some(groups) = hooks.get_mut(*event).and_then(|g| g.as_array_mut()) else {
                continue;
            };
            groups.retain_mut(|group| {
                if !group_has_managed(group, spec.is_marker) {
                    return true; // 사용자 전용 group은 불가침
                }
                removed += 1;
                let Some(entries) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                    return false;
                };
                entries.retain(|entry| !entry_is_managed(entry, spec.is_marker));
                !entries.is_empty() // 우리 엔트리만 있던 group은 통째로 사라진다
            });
            if groups.is_empty() {
                empty_events.push((*event).to_string());
            }
        }
        for event in &empty_events {
            hooks.remove(event.as_str());
        }
    }
    // hooks 객체 자체가 비면(모든 이벤트를 정리한 뒤) 키를 지운다.
    let hooks_now_empty = settings
        .get("hooks")
        .and_then(|h| h.as_object())
        .map(|h| h.is_empty())
        .unwrap_or(false);
    if hooks_now_empty {
        if let Some(obj) = settings.as_object_mut() {
            obj.remove("hooks");
        }
    }
    removed
}

pub(super) fn read_settings(path: &Path) -> Result<Option<Value>, HooksError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            serde_json::from_str(trimmed)
                .map(Some)
                .map_err(|e| HooksError::InvalidJson(e.to_string()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(HooksError::Read(error.to_string())),
    }
}

/// 원본 옆 파일 경로(`settings.json` + `.iyagi.bak` →
/// `settings.json.iyagi.bak`). 확장자를 갈아치우는 대신 이름 끝에
/// 붙여, `.json`이 아닌 파일에서도 원본 이름이 그대로 남게 한다.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(OsString::from).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// 설정 파일 쓰기 — Claude hooks·Codex hooks·`claude_usage`(status line)가
/// 모두 이 한 곳을 쓴다(같은 파일에 두 연동이 각자 백업/임시 파일을 만들던
/// 중복 제거).
///
/// - `.iyagi.bak`은 **없을 때만** 만든다("최초 적용 전 원본"). 매번
///   덮어썼다면 두 번째 적용이 이미 우리 항목이 든 내용으로 원본 백업을
///   갈아치워, 사용자의 진짜 원본이 영구히 사라진다.
/// - 같은 디렉터리에 프로세스별 고유 임시 파일(`.iyagi.<pid>.tmp`)을
///   만들어(unix에서는 없는 경로에 0600 — 설정에는 토큰이 섞일 수 있어
///   umask 기본 모드로 존재하는 찰나도 없게) 내용 기록 → `sync_all()` →
///   (Unix) 원본 권한 보존 → `rename`. 중간에 실패하면 임시 파일을 지운다.
/// - 전 과정을 프로세스 전역 뮤텍스로 직렬화한다.
pub(crate) fn write_settings_file(path: &Path, content: &str) -> Result<(), HooksError> {
    let _serialized = WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| HooksError::Write(e.to_string()))?;
    }
    let backup = sidecar(path, BACKUP_SUFFIX);
    if path.exists() && !backup.exists() {
        std::fs::copy(path, &backup).map_err(|e| HooksError::Write(format!("backup: {e}")))?;
    }
    let tmp = sidecar(path, &format!(".iyagi.{}.tmp", std::process::id()));
    if let Err(error) = write_durable(&tmp, content, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(HooksError::Write(error.to_string()));
    }
    std::fs::rename(&tmp, path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        HooksError::Write(error.to_string())
    })
}

/// 임시 파일에 내용을 쓰고 디스크까지 밀어 넣는다(rename 뒤 내용이 0바이트로
/// 남는 사고 방지). Unix에서는 원본 권한(0600 설정 파일 등)을 그대로 옮기고,
/// 원본이 없는 첫 생성은 임시 파일의 0600이 rename을 타고 그대로 이어진다 —
/// 예전에는 첫 생성이 umask 기본(0644)로 영구히 굳었다.
fn write_durable(tmp: &Path, content: &str, original: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = create_fresh_tmp(tmp)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(original) {
            let mode = meta.permissions().mode() & 0o7777;
            std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(mode))?;
        }
    }
    #[cfg(not(unix))]
    let _ = original;
    Ok(())
}

/// 임시 파일을 없는 경로에만 만들어 연다(`create_new` — 심볼릭 링크 선점이나
/// 찌꺼기 덮어쓰기가 없고, unix에서는 만드는 순간 0600이다). 같은 pid의 죽은
/// 실행이 남긴 찌꺼기가 있으면 치우고 한 번 더 시도한다. (Windows는 홈 폴더
/// 아래 기본 ACL을 따른다.)
fn create_fresh_tmp(tmp: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(tmp) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(tmp)?;
            options.open(tmp)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn pretty(settings: &Value) -> String {
    let mut out = serde_json::to_string_pretty(settings).expect("settings serialize");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 설치본(공백 있는 경로)에서 실제로 등록되는 Claude 명령.
    const INSTALLED_CLAUDE: &str = "'/Applications/IYAGI.app/Contents/MacOS/iyagi-termd' hook";
    /// 같은 설치본의 Codex 명령.
    const INSTALLED_CODEX: &str =
        "'/Applications/IYAGI.app/Contents/MacOS/iyagi-termd' hook --agent codex";

    fn claude_marker(command: &str) -> bool {
        is_managed_command(command, None)
    }

    fn codex_marker(command: &str) -> bool {
        is_managed_command(command, Some("codex"))
    }

    fn claude_spec() -> HooksSpec {
        HooksSpec {
            events: &["Notification", "SessionStart"],
            is_marker: claude_marker,
            timeout_secs: 10,
        }
    }

    fn codex_spec() -> HooksSpec {
        HooksSpec {
            events: &["SessionStart"],
            is_marker: codex_marker,
            timeout_secs: 10,
        }
    }

    #[test]
    fn tokenizer_reads_quoted_paths_and_escapes() {
        assert_eq!(
            command_argv(INSTALLED_CLAUDE),
            vec![
                "/Applications/IYAGI.app/Contents/MacOS/iyagi-termd".to_string(),
                "hook".to_string()
            ]
        );
        assert_eq!(
            command_argv("\"C:\\\\Program Files\\\\IYAGI\\\\iyagi-termd.exe\" hook"),
            vec![
                "C:\\Program Files\\IYAGI\\iyagi-termd.exe".to_string(),
                "hook".to_string()
            ]
        );
        // 따옴표 밖 백슬래시 이스케이프도 한 토큰으로 모은다.
        assert_eq!(
            command_argv("/opt/iyagi\\ term/iyagi-termd hook"),
            vec![
                "/opt/iyagi term/iyagi-termd".to_string(),
                "hook".to_string()
            ]
        );
        // shell_quote가 내놓는 홑따옴표 안의 홑따옴표(`'\''`)도 원문으로.
        assert_eq!(
            command_argv("'/opt/it'\\''s/iyagi-termd' hook"),
            vec!["/opt/it's/iyagi-termd".to_string(), "hook".to_string()]
        );
        assert_eq!(command_argv("   "), Vec::<String>::new());
    }

    #[test]
    fn managed_predicate_matches_quoted_exe_and_agent_forms() {
        // 공백 있는 설치 경로(인용) — 부분 문자열 마커가 놓치던 형태.
        assert!(is_managed_command(INSTALLED_CLAUDE, None));
        assert!(!is_managed_command(INSTALLED_CLAUDE, Some("codex")));
        assert!(is_managed_command(INSTALLED_CODEX, Some("codex")));
        assert!(!is_managed_command(INSTALLED_CODEX, None));
        // Windows `.exe`(대소문자 무시) — 마커가 영원히 못 맞추던 형태.
        assert!(is_managed_command(
            "\"C:\\\\Program Files\\\\IYAGI\\\\iyagi-termd.exe\" hook",
            None
        ));
        assert!(is_managed_command(
            "IYAGI-TERMD.EXE hook --agent codex",
            Some("codex")
        ));
        // 주체 이름은 정확히 같아야 한다(우리는 늘 소문자로 쓴다).
        assert!(!is_managed_command(
            "iyagi-termd hook --agent Codex",
            Some("codex")
        ));
        // 값이 빠진 채 끊긴 옵션은 누구의 것도 아니다(지우지 않는다).
        assert!(!is_managed_command("iyagi-termd hook --agent", None));
        assert!(!is_managed_command(
            "iyagi-termd hook --agent",
            Some("codex")
        ));
        // `--agent=codex` 한 토큰 형태.
        assert!(is_managed_command(
            "iyagi-termd hook --agent=codex",
            Some("codex")
        ));
        assert!(!is_managed_command("iyagi-termd hook --agent=codex", None));
        // 명시적 `--agent claude`도 Claude 것이다.
        assert!(is_managed_command("iyagi-termd hook --agent claude", None));
        // 미래의 제3자 등록은 Claude도 Codex도 자기 것으로 세지 않는다.
        assert!(!is_managed_command("iyagi-termd hook --agent gemini", None));
        assert!(!is_managed_command(
            "iyagi-termd hook --agent gemini",
            Some("codex")
        ));
        // 사용자 명령: 다른 프로그램, 다른 서브커맨드, 우리 이름을 품은 인자.
        assert!(!is_managed_command("echo user-own-hook", None));
        assert!(!is_managed_command("iyagi-termd claude-usage", None));
        assert!(!is_managed_command("notify-send 'iyagi-termd hook'", None));
        assert!(!is_managed_command("my-iyagi-termd hook", None));
        assert!(!is_managed_command("", None));
    }

    /// `status()`가 미리보기를 만들 때도 `inject_managed`를 타므로, 낯선
    /// 셰이프는 패닉이 아니라 거절이어야 한다(설정 탭을 여는 것만으로
    /// 앱이 죽던 자리).
    #[test]
    fn inject_refuses_non_object_hooks_instead_of_panicking() {
        let mut settings = json!({ "hooks": [] });
        let err = inject_managed(&mut settings, INSTALLED_CLAUDE, &claude_spec()).unwrap_err();
        assert_eq!(err.code(), "INVALID_JSON");
        assert!(err.to_string().contains("hooks is not an object"));
        // 원본은 그대로(거절은 파일을 고치지 않는다).
        assert_eq!(settings, json!({ "hooks": [] }));
    }

    #[test]
    fn inject_refuses_non_array_event_group_instead_of_panicking() {
        let mut settings = json!({ "hooks": { "Notification": {} } });
        let err = inject_managed(&mut settings, INSTALLED_CLAUDE, &claude_spec()).unwrap_err();
        assert_eq!(err.code(), "INVALID_JSON");
        assert!(err
            .to_string()
            .contains("Notification group is not an array"));
    }

    /// 사용자 엔트리와 우리 엔트리가 한 group에 섞여 있어도 등록으로 세고,
    /// 제거는 우리 엔트리만 빼고 사용자 엔트리는 남긴다.
    #[test]
    fn mixed_group_is_recognised_and_only_our_entry_is_removed() {
        let mut settings = json!({
            "hooks": {
                "Notification": [
                    { "hooks": [
                        { "type": "command", "command": "notify-send mine" },
                        { "type": "command", "command": INSTALLED_CLAUDE, "timeout": 10 }
                    ] }
                ]
            }
        });
        let spec = claude_spec();
        assert_eq!(count_managed(&settings, &spec), 1);
        // 이미 있는 이벤트이므로 Notification에는 중복 group을 덧붙이지 않는다.
        assert!(inject_managed(&mut settings, INSTALLED_CLAUDE, &spec).unwrap());
        assert_eq!(
            settings["hooks"]["Notification"].as_array().unwrap().len(),
            1
        );
        assert_eq!(count_managed(&settings, &spec), 2); // + SessionStart

        assert_eq!(remove_managed(&mut settings, &spec), 2);
        assert_eq!(count_managed(&settings, &spec), 0);
        // 섞여 있던 사용자 엔트리는 같은 group에 그대로 남는다.
        let kept = &settings["hooks"]["Notification"][0]["hooks"];
        assert_eq!(kept.as_array().unwrap().len(), 1);
        assert_eq!(kept[0]["command"], "notify-send mine");
        // 우리만 있던 SessionStart는 통째로 정리된다.
        assert!(settings["hooks"].get("SessionStart").is_none());
    }

    /// Codex 등록이 섞여 있어도 Claude 판정자는 자기 것으로 세지 않는다
    /// (그 반대도 같다) — 인용된 설치 경로에서도 유지된다.
    #[test]
    fn each_agent_only_claims_its_own_groups() {
        let mut settings = json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [ { "type": "command", "command": INSTALLED_CODEX, "timeout": 10 } ] }
                ]
            }
        });
        assert_eq!(count_managed(&settings, &claude_spec()), 0);
        assert_eq!(count_managed(&settings, &codex_spec()), 1);
        assert!(inject_managed(&mut settings, INSTALLED_CLAUDE, &claude_spec()).unwrap());
        assert_eq!(
            settings["hooks"]["SessionStart"].as_array().unwrap().len(),
            2
        );
        assert_eq!(remove_managed(&mut settings, &claude_spec()), 2);
        assert_eq!(count_managed(&settings, &codex_spec()), 1);
    }

    /// 백업은 "최초 적용 전 원본" 한 번만 — 두 번째 쓰기가 원본 백업을
    /// 덮어쓰면 사용자의 진짜 원본이 사라진다.
    #[test]
    fn backup_keeps_the_pristine_original_across_repeated_writes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{\"model\":\"pristine\"}\n").unwrap();

        write_settings_file(&path, "{\"model\":\"first\"}\n").unwrap();
        write_settings_file(&path, "{\"model\":\"second\"}\n").unwrap();

        let backup = std::fs::read_to_string(path.with_extension("json.iyagi.bak")).unwrap();
        assert!(
            backup.contains("pristine"),
            "backup was clobbered: {backup}"
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains("second"));
        // 임시 파일은 남지 않는다.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_preserves_the_original_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{}\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        write_settings_file(&path, "{\"a\":1}\n").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode must survive the atomic replace");
    }

    /// 처음 만드는 설정 파일은 그룹/다른 사용자에게 보이지 않는 모드로 시작한다
    /// — 예전에는 첫 생성이 umask 기본(0644)으로 굳어서 토큰이 든 설정이
    /// 계속 읽혔다.
    #[cfg(unix)]
    #[test]
    fn first_creation_starts_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        write_settings_file(&path, "{\"a\":1}\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "mode was {mode:o}");
    }

    #[test]
    fn write_creates_a_fresh_file_without_any_backup() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nested").join("hooks.json");
        write_settings_file(&path, "{}\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
        assert!(!path.with_extension("json.iyagi.bak").exists());
    }
}
