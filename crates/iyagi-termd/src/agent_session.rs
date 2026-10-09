//! 에이전트 자체 세션 id 알아내기(spec `02-runner.md` §8).
//!
//! 감시 루프가 (에이전트, pid)를 잡은 뒤, 그 프로세스가 지금 어느 대화를
//! 붙들고 있는지를 **프로세스 관찰만으로** 알아낸다. 출력 스캐닝도,
//! 에이전트에게 질문을 보내는 것도 아니다.
//!
//! * Claude Code — 실행 중 레지스트리 `<claude_home>/sessions/<pid>.json`.
//!   프로세스가 살아 있는 동안만 있고 종료하면 지워진다. 파일 안의 `pid`가
//!   우리가 물어본 pid와 같을 때만 믿는다(pid 재사용 방어). 이름·상태도
//!   여기서 온다. `$CLAUDE_CONFIG_DIR`(디렉터리)를 존중하고, 없으면
//!   `$HOME/.claude`다. 형제 파일 `<pid>.<hash>.key`는 무시한다.
//! * Codex — 세션이 사는 내내 **열어 둔** 잠금 파일
//!   `<codex_home>/thread-writer-locks/<thread-id>.lock`의 fd. 구버전은
//!   대신 `<codex_home>/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`을
//!   열어 둔다. fd 열거가 막힌 플랫폼(Windows)에서는 잠금 디렉터리의
//!   mtime이 프로세스 시작 시각 ±20초 안이고 **유일할 때만** 쓴다 —
//!   애매하면 포기한다(틀린 대화를 이어 여는 것이 모르는 것보다 나쁘다).
//!   제목은 `<codex_home>/session_index.jsonl`의 `thread_name`이다.
//!   sqlite 데이터베이스는 절대 열지 않는다.
//! * OpenCode — PTY에 주입한 공식 플러그인이 hook으로 보고한다.
//!   이 관찰 함수는 `None`이고, 감시 루프는 hook이 얻은 ID를 보존한다.
//!
//! 개인정보 원칙(01 §7): 여기서 만들어 내는 값은 세션 id·cwd·표시용
//! 제목·상태 라벨뿐이다. 대화 내용·프롬프트 원문·argv·env는 읽지도
//! 옮기지도 않는다.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use term_contracts::agent_session::{limits, valid_session_id, AgentSessionSource};
use term_platform::proc_scan::ProcessBrief;

/// 잠금 파일 후보를 프로세스 시작 시각과 맞춰 볼 때의 허용 오차.
/// 프로세스가 뜨고 잠금을 잡기까지의 지연 + mtime 해상도를 덮는다.
const LOCK_MTIME_TOLERANCE: Duration = Duration::from_secs(20);

/// 레지스트리 파일 읽기 상한. 실제 파일은 600바이트 남짓이다.
const REGISTRY_READ_MAX: u64 = 64 * 1024;

/// `session_index.jsonl` 읽기 상한(4 MiB). 넘으면 제목을 포기한다 —
/// 제목은 있으면 좋은 것이지 식별의 근거가 아니다.
const SESSION_INDEX_READ_MAX: u64 = 4 * 1024 * 1024;

/// 알아낸 에이전트 세션 한 건.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// 에이전트 자체 세션(스레드) id.
    pub session_id: String,
    /// 에이전트가 붙인 표시 이름(Claude 레지스트리 `name`).
    pub name: Option<String>,
    /// 에이전트가 스스로 보고한 활동 상태 원문("idle" | "busy" ...).
    pub status: Option<String>,
    /// 에이전트가 보고한 실행 디렉터리.
    pub cwd: Option<String>,
    /// 저장용 제목(Claude는 `name`, Codex는 `thread_name`).
    pub title: Option<String>,
    pub source: AgentSessionSource,
}

/// (에이전트, pid)의 지금 세션. 알아내지 못하면 `None` — 이것은 정상이며
/// (권한 없음, 아직 세션 시작 전, 지원하지 않는 에이전트) 오류가 아니다.
pub fn resolve(agent: &str, pid: u32, brief: &ProcessBrief) -> Option<Resolved> {
    // 호출자가 트리에서 고른 프로세스와 pid가 어긋나면 관찰 대상이 아니다.
    if brief.pid != pid {
        return None;
    }
    match agent {
        "claude" => resolve_claude(&claude_home()?, pid),
        "codex" => resolve_codex(&codex_home()?, pid),
        // OpenCode는 공식 플러그인의 hook 보고를 사용한다.
        _ => None,
    }
}

/// 관찰된 실행 파일 경로 — 그 파일 이름이 에이전트 바이너리 이름과 같을
/// 때만. node/python 래퍼 설치는 `None`이다(복구 실행의 후보로 쓸 수 없다:
/// `node cli.js`를 그대로 되살리면 우리가 모르는 인수가 빠진다).
pub fn observed_program(brief: &ProcessBrief, agent: &str) -> Option<String> {
    let exe = brief.exe.as_deref()?;
    let name = Path::new(exe).file_name()?.to_str()?;
    (name == agent).then(|| exe.to_string())
}

// ---------------------------------------------------------------------------
// Claude Code — 실행 중 레지스트리

/// `$CLAUDE_CONFIG_DIR`(디렉터리) 우선, 없으면 `$HOME/.claude`.
pub(crate) fn claude_home() -> Option<PathBuf> {
    home_dir_from_env("CLAUDE_CONFIG_DIR", ".claude")
}

fn resolve_claude(claude_home: &Path, pid: u32) -> Option<Resolved> {
    let path = claude_home.join("sessions").join(format!("{pid}.json"));
    let text = read_bounded(&path, REGISTRY_READ_MAX)?;
    let resolved = parse_claude_registry(&text, pid)?;
    // Claude publishes a runtime ID before it has saved any conversation, and
    // can publish a temporary ID while starting a resume. Neither is a valid
    // recovery target until a transcript exists. Do not replace the saved ID
    // with one of these startup markers.
    claude_transcript_exists(claude_home, &resolved.session_id, resolved.cwd.as_deref())
        .then_some(resolved)
}

/// Check file metadata only; never read prompts or transcript contents.
/// The cwd-derived path handles the common case with a single stat. A shallow
/// fallback supports moved sessions and Claude's hashed long project paths.
pub(crate) fn claude_transcript_exists(home: &Path, session_id: &str, cwd: Option<&str>) -> bool {
    if !valid_session_id(session_id) {
        return false;
    }
    let projects = home.join("projects");
    let file = format!("{session_id}.jsonl");
    let nonempty_file = |path: &Path| {
        std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
    };
    if let Some(cwd) = cwd {
        let project: String = cwd
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect();
        if nonempty_file(&projects.join(project).join(&file)) {
            return true;
        }
    }
    let Ok(entries) = std::fs::read_dir(projects) else {
        return false;
    };
    entries.take(4096).flatten().any(|entry| {
        entry.file_type().is_ok_and(|kind| kind.is_dir())
            && nonempty_file(&entry.path().join(&file))
    })
}

/// 터미널 UI가 셸 실행에 씌우는 `/usr/bin/env -u VAR ... <program> <args>`
/// 런처를 벗겨 실제 프로그램과 인수를 돌려준다. 런처가 아니거나 형태가
/// 어긋나면(`-u` 뒤에 값이 없거나 프로그램이 없으면) 입력을 그대로 돌려준다.
/// 셸 텍스트나 프롬프트 인수는 해석하지 않는다 — `claude_resume_id`와
/// `claude_provider`의 프로그램 판정이 같은 규칙을 쓴다.
pub(crate) fn strip_env_launcher(program: &str, argv: &[String]) -> (String, Vec<String>) {
    let (program, argv) = split_env_launcher(program, argv);
    (program.to_string(), argv.to_vec())
}

/// [`strip_env_launcher`]의 빌림 버전: `claude_resume_id`가 argv 안의 id를
/// 복사 없이 그대로 돌려줄 수 있게 한다.
fn split_env_launcher<'p, 'a: 'p>(program: &'p str, argv: &'a [String]) -> (&'p str, &'a [String]) {
    if program != "/usr/bin/env" {
        return (program, argv);
    }
    let mut offset = 0;
    while argv.get(offset).is_some_and(|arg| arg == "-u") {
        if argv.get(offset + 1).is_none() {
            return (program, argv);
        }
        offset += 2;
    }
    match argv.get(offset) {
        Some(inner) => (inner.as_str(), &argv[offset + 1..]),
        None => (program, argv),
    }
}

/// Check the exact UUID resume command emitted by the terminal UI. Names,
/// transcript paths, and arbitrary custom commands remain Claude's concern.
/// Recognize the env -u wrapper without interpreting shell text or prompt args.
pub(crate) fn claude_resume_id<'a>(program: &str, argv: &'a [String]) -> Option<&'a str> {
    // 형태가 어긋난 런처는 그대로 남아 `/usr/bin/env`로 판정되고, 그 argv는
    // `-u`로 시작하므로 아래 루프가 첫 인수에서 None을 돌려준다 — 예전의
    // 조기 None과 같은 결과다.
    let (program, argv) = split_env_launcher(program, argv);
    if crate::agent_watch::detect_agent_in_command(program, argv) != Some("claude") {
        return None;
    }
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        if matches!(
            arg.as_str(),
            "--dangerously-skip-permissions" | "--allow-dangerously-skip-permissions"
        ) {
            continue;
        }
        let id = if arg == "--resume" || arg == "-r" {
            args.next()?.as_str()
        } else if let Some(id) = arg.strip_prefix("--resume=") {
            id
        } else {
            return None;
        };
        return uuid::Uuid::parse_str(id).ok().map(|_| id);
    }
    None
}

/// 레지스트리 JSON → [`Resolved`]. 파일의 `pid`가 물어본 pid와 다르면
/// 거절한다 — 남은 파일이나 pid 재사용을 그대로 믿지 않는다.
fn parse_claude_registry(text: &str, pid: u32) -> Option<Resolved> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let file_pid = value.get("pid").and_then(serde_json::Value::as_u64)?;
    if file_pid != u64::from(pid) {
        return None;
    }
    let session_id = value.get("sessionId").and_then(|v| v.as_str())?;
    if !valid_session_id(session_id) {
        return None;
    }
    let name = bounded_text(
        value.get("name").and_then(|v| v.as_str()),
        limits::TITLE_MAX,
    );
    Some(Resolved {
        session_id: session_id.to_string(),
        name: name.clone(),
        status: bounded_text(value.get("status").and_then(|v| v.as_str()), 32),
        cwd: bounded_text(value.get("cwd").and_then(|v| v.as_str()), limits::PATH_MAX),
        title: name,
        source: AgentSessionSource::Registry,
    })
}

// ---------------------------------------------------------------------------
// Codex — 열어 둔 잠금 파일

/// `$CODEX_HOME` 우선, 없으면 `$HOME/.codex`.
pub(crate) fn codex_home() -> Option<PathBuf> {
    home_dir_from_env("CODEX_HOME", ".codex")
}

fn resolve_codex(codex_home: &Path, pid: u32) -> Option<Resolved> {
    let lock_dir = codex_home.join("thread-writer-locks");
    let session_id = codex_session_from_open_fds(pid, &lock_dir)
        .or_else(|| codex_session_from_lock_mtime(pid, &lock_dir))?;
    let title = codex_title(codex_home, &session_id);
    Some(Resolved {
        session_id,
        name: None,
        status: None,
        cwd: term_platform::proc_scan::process_cwd(pid),
        title,
        source: AgentSessionSource::LockFile,
    })
}

/// (1순위) 열어 둔 fd에서 직접 읽는다 — 확실한 근거다. 잠금 파일이 먼저고
/// 없으면 구버전의 rollout 파일이다. 어느 쪽이든 후보가 **한 가지 id일
/// 때만** 쓴다(아래 [`unique_session_id`]).
fn codex_session_from_open_fds(pid: u32, lock_dir: &Path) -> Option<String> {
    let paths = term_platform::proc_fds::open_file_paths(pid)?;
    unique_session_id(
        paths
            .iter()
            .filter_map(|path| lock_path_session_id(path, lock_dir)),
    )
    .or_else(|| unique_session_id(paths.iter().filter_map(|p| rollout_path_session_id(p))))
}

/// 후보가 **한 가지 id일 때만** 그 id. 서로 다른 id가 둘 이상 열려 있으면
/// (보조 스레드가 다른 스레드의 잠금을 들고 있거나, 심어 둔 파일이 fd
/// 테이블에 섞였을 때) 애매하므로 포기한다 — 틀린 대화를 이어 여는 것이
/// 모르는 것보다 나쁘다. 같은 id가 여러 fd로 열려 있는 것은 한 가지다.
fn unique_session_id(ids: impl Iterator<Item = String>) -> Option<String> {
    let mut hit: Option<String> = None;
    for id in ids {
        match &hit {
            Some(seen) if *seen == id => {}
            Some(_) => return None,
            None => hit = Some(id),
        }
    }
    hit
}

/// (2순위) fd를 못 읽는 환경: 잠금 디렉터리에서 프로세스 시작 시각과
/// 맞는 파일을 찾는다. **유일할 때만** 쓴다.
fn codex_session_from_lock_mtime(pid: u32, lock_dir: &Path) -> Option<String> {
    let start = term_platform::proc_scan::process_start_time_secs(pid)?;
    let candidates = lock_candidates(lock_dir);
    choose_lock_candidate(&candidates, start)
}

/// 잠금 디렉터리의 `*.lock` 파일과 mtime.
fn lock_candidates(lock_dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(lock_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lock") {
            continue;
        }
        if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
            out.push((path, modified));
        }
    }
    out
}

/// 시작 시각 ±[`LOCK_MTIME_TOLERANCE`] 안의 후보가 **정확히 하나**일 때만
/// 그 세션 id. 둘 이상이면 애매하므로 `None` — 틀린 대화를 이어 여는 것이
/// 모르는 것보다 나쁘다.
fn choose_lock_candidate(candidates: &[(PathBuf, SystemTime)], start_secs: u64) -> Option<String> {
    let mut hit: Option<String> = None;
    for (path, modified) in candidates {
        let Ok(since_epoch) = modified.duration_since(UNIX_EPOCH) else {
            continue;
        };
        let delta = since_epoch.as_secs().abs_diff(start_secs);
        if delta > LOCK_MTIME_TOLERANCE.as_secs() {
            continue;
        }
        let Some(id) = file_stem_session_id(path) else {
            continue;
        };
        if hit.is_some() {
            return None; // 애매하다
        }
        hit = Some(id);
    }
    hit
}

/// 경로가 잠금 디렉터리 바로 아래의 `<session id>.lock`이면 그 id.
fn lock_path_session_id(path: &Path, lock_dir: &Path) -> Option<String> {
    if path.extension().and_then(|e| e.to_str()) != Some("lock") {
        return None;
    }
    if path.parent()? != lock_dir {
        return None;
    }
    file_stem_session_id(path)
}

fn file_stem_session_id(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    valid_session_id(stem).then(|| stem.to_string())
}

/// 구버전 Codex: `<codex_home>/sessions/.../rollout-<ts>-<uuid>.jsonl`.
/// uuid는 `.jsonl` 앞 36자이며 UUID로 파싱돼야 한다.
fn rollout_path_session_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    rollout_session_id(name)
}

fn rollout_session_id(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix(".jsonl")?;
    let stem = stem.strip_prefix("rollout-")?;
    if stem.len() < 37 {
        return None; // 최소한 "-<uuid>"만큼은 더 있어야 한다
    }
    let id = &stem[stem.len() - 36..];
    // 잘라 낸 앞부분과 id 사이에는 구분자 '-'가 있어야 한다.
    if !stem[..stem.len() - 36].ends_with('-') {
        return None;
    }
    uuid::Uuid::parse_str(id).ok()?;
    Some(id.to_string())
}

/// `session_index.jsonl`에서 이 id의 `thread_name`. 없으면 `None`.
fn codex_title(codex_home: &Path, session_id: &str) -> Option<String> {
    let text = read_bounded(
        &codex_home.join("session_index.jsonl"),
        SESSION_INDEX_READ_MAX,
    )?;
    parse_session_index_title(&text, session_id)
}

/// 한 줄에 JSON 객체 하나. 같은 id가 여러 줄이면 마지막(가장 최근) 줄이
/// 이긴다 — 파일은 덧붙여 쓰이기 때문이다.
fn parse_session_index_title(contents: &str, session_id: &str) -> Option<String> {
    let mut found = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("id").and_then(|v| v.as_str()) != Some(session_id) {
            continue;
        }
        if let Some(name) = bounded_text(
            value.get("thread_name").and_then(|v| v.as_str()),
            limits::TITLE_MAX,
        ) {
            found = Some(name);
        }
    }
    found
}

// ---------------------------------------------------------------------------
// 공용 helper

/// `<var>`가 가리키는 디렉터리, 없으면 `$HOME/<fallback>`.
fn home_dir_from_env(var: &str, fallback: &str) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(var) {
        let path = PathBuf::from(dir);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let home = PathBuf::from(home);
    (!home.as_os_str().is_empty()).then(|| home.join(fallback))
}

/// 상한까지만 읽는다. 파일이 없거나 크면 `None`.
fn read_bounded(path: &Path, max_bytes: u64) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > max_bytes {
        return None;
    }
    let mut text = String::new();
    file.take(max_bytes).read_to_string(&mut text).ok()?;
    Some(text)
}

/// 빈 문자열·상한 초과는 없는 것으로 본다(자르지 않는다 — 잘린 제목은
/// 틀린 제목이다).
fn bounded_text(value: Option<&str>, max: usize) -> Option<String> {
    let text = value?.trim();
    (!text.is_empty() && text.len() <= max && !text.chars().any(char::is_control))
        .then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const CLAUDE_ID: &str = "7db2598e-c360-48fe-a2d5-0240993c9f7a";
    const CODEX_ID: &str = "01a097e0-b0d4-7343-b27e-4ac4d3615822";

    fn brief(pid: u32, exe: Option<&str>) -> ProcessBrief {
        ProcessBrief {
            pid,
            ppid: 1,
            name: exe
                .and_then(|e| e.rsplit_once('/').map(|(_, n)| n))
                .unwrap_or_default()
                .to_string(),
            exe: exe.map(str::to_string),
            cmd: Vec::new(),
        }
    }

    fn registry_json(pid: u32) -> String {
        format!(
            r#"{{"pid":{pid},"sessionId":"{CLAUDE_ID}","cwd":"/Users/x/project/iyagi",
             "startedAt":1789142618039,"procStart":"Fri Sep 11 16:03:37 2026","version":"2.1.268",
             "kind":"interactive","entrypoint":"cli","name":"iyagi-7d","nameSource":"derived",
             "status":"idle","updatedAt":1789142700000}}"#
        )
    }

    // -- Claude 레지스트리 ---------------------------------------------------

    #[test]
    fn claude_registry_yields_id_name_status_and_cwd() {
        let dir = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("sessions")).unwrap();
        let project = dir.path().join("projects/-Users-x-project-iyagi");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join(format!("{CLAUDE_ID}.jsonl")), b"{}\n").unwrap();
        fs::write(
            dir.path().join("sessions").join("13185.json"),
            registry_json(13185),
        )
        .unwrap();
        // 형제 키 파일은 무시된다(존재해도 방해하지 않는다).
        fs::write(dir.path().join("sessions").join("13185.abc.key"), "x").unwrap();

        let resolved = resolve_claude(dir.path(), 13185).expect("resolved");
        assert_eq!(resolved.session_id, CLAUDE_ID);
        assert_eq!(resolved.name.as_deref(), Some("iyagi-7d"));
        assert_eq!(resolved.title.as_deref(), Some("iyagi-7d"));
        assert_eq!(resolved.status.as_deref(), Some("idle"));
        assert_eq!(resolved.cwd.as_deref(), Some("/Users/x/project/iyagi"));
        assert_eq!(resolved.source, AgentSessionSource::Registry);
    }

    #[test]
    fn claude_registry_with_a_foreign_pid_is_rejected() {
        // pid 재사용/남은 파일: 파일 안의 pid가 다르면 믿지 않는다.
        assert!(parse_claude_registry(&registry_json(999), 13185).is_none());
        assert!(parse_claude_registry(&registry_json(13185), 13185).is_some());
    }

    #[test]
    fn claude_registry_rejects_hostile_or_missing_fields() {
        for text in [
            "",
            "not json",
            r#"{"sessionId":"x"}"#,                        // pid 없음
            r#"{"pid":1,"sessionId":""}"#,                 // 빈 id
            r#"{"pid":1,"sessionId":"../../etc/passwd"}"#, // 경로 문자
            r#"{"pid":1}"#,                                // id 없음
        ] {
            assert!(parse_claude_registry(text, 1).is_none(), "{text:?}");
        }
        // 제어 문자가 든 이름·상한을 넘는 이름은 통째로 버린다(자르지 않는다).
        let weird = r#"{"pid":1,"sessionId":"abc","name":"a\u0007b","status":"idle"}"#;
        let resolved = parse_claude_registry(weird, 1).unwrap();
        assert_eq!(resolved.session_id, "abc");
        assert_eq!(resolved.name, None, "제어 문자가 든 이름은 버린다");
        assert_eq!(resolved.status.as_deref(), Some("idle"));

        let long = format!(
            r#"{{"pid":1,"sessionId":"abc","name":"{}"}}"#,
            "n".repeat(limits::TITLE_MAX + 1)
        );
        assert_eq!(parse_claude_registry(&long, 1).unwrap().name, None);
    }

    #[test]
    fn claude_missing_registry_file_is_not_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(resolve_claude(dir.path(), 13185).is_none());
    }

    #[test]
    fn claude_startup_registry_id_waits_for_a_saved_transcript() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("sessions")).unwrap();
        fs::write(dir.path().join("sessions/13185.json"), registry_json(13185)).unwrap();
        assert!(resolve_claude(dir.path(), 13185).is_none());
        // Another session's file must not make this runtime ID resumable.
        let project = dir.path().join("projects/moved-or-hashed-project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("unrelated.jsonl"), b"{}\n").unwrap();
        let transcript = project.join(format!("{CLAUDE_ID}.jsonl"));
        fs::write(&transcript, b"").unwrap();
        assert!(resolve_claude(dir.path(), 13185).is_none());
        fs::write(&transcript, b"{}\n").unwrap();
        assert_eq!(
            resolve_claude(dir.path(), 13185).unwrap().session_id,
            CLAUDE_ID
        );
        fs::remove_file(&transcript).unwrap();
        assert!(resolve_claude(dir.path(), 13185).is_none());
    }

    #[test]
    fn claude_resume_preflight_recognizes_terminal_wrappers_and_leaves_names_to_the_cli() {
        let args = |values: &[&str]| values.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let wrapped = args(&[
            "-u",
            "NO_COLOR",
            "/opt/bin/claude",
            "--dangerously-skip-permissions",
            "--resume",
            CLAUDE_ID,
        ]);
        assert_eq!(claude_resume_id("/usr/bin/env", &wrapped), Some(CLAUDE_ID));
        assert_eq!(
            claude_resume_id("/opt/bin/claude", &args(&["-r", CLAUDE_ID])),
            Some(CLAUDE_ID)
        );
        assert_eq!(
            claude_resume_id("/opt/bin/claude", &args(&["--resume", "named-session"])),
            None
        );
        assert_eq!(
            claude_resume_id("/opt/bin/claude", &args(&["--session-id", CLAUDE_ID])),
            None
        );
        assert_eq!(
            claude_resume_id(
                "/opt/bin/claude",
                &args(&["--system-prompt", "--resume", CLAUDE_ID])
            ),
            None
        );
        assert_eq!(
            claude_resume_id("/opt/bin/codex", &args(&["resume", CLAUDE_ID])),
            None
        );
        // 형태가 어긋난 런처는 예전처럼 None이다.
        assert_eq!(claude_resume_id("/usr/bin/env", &args(&["-u"])), None);
        assert_eq!(
            claude_resume_id("/usr/bin/env", &args(&["-u", "NO_COLOR"])),
            None
        );
    }

    #[test]
    fn env_launcher_is_stripped_only_when_well_formed() {
        let args = |values: &[&str]| values.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // 잘 만들어진 런처: 프로그램과 나머지 인수만 남는다.
        let wrapped = args(&[
            "-u",
            "NO_COLOR",
            "-u",
            "FORCE_COLOR",
            "/x/claude",
            "--resume",
            CLAUDE_ID,
        ]);
        assert_eq!(
            strip_env_launcher("/usr/bin/env", &wrapped),
            ("/x/claude".to_string(), args(&["--resume", CLAUDE_ID]))
        );
        // `-u` 없이 바로 프로그램이 와도 벗긴다.
        assert_eq!(
            strip_env_launcher("/usr/bin/env", &args(&["/x/claude"])),
            ("/x/claude".to_string(), Vec::new())
        );
        // 런처가 아니면 그대로.
        assert_eq!(
            strip_env_launcher("/x/claude", &args(&["-r", CLAUDE_ID])),
            ("/x/claude".to_string(), args(&["-r", CLAUDE_ID]))
        );
        // 형태가 어긋나면(값 없는 -u, 프로그램 없음) 입력을 그대로 돌려준다.
        assert_eq!(
            strip_env_launcher("/usr/bin/env", &args(&["-u"])),
            ("/usr/bin/env".to_string(), args(&["-u"]))
        );
        assert_eq!(
            strip_env_launcher("/usr/bin/env", &args(&["-u", "NO_COLOR"])),
            ("/usr/bin/env".to_string(), args(&["-u", "NO_COLOR"]))
        );
        assert_eq!(
            strip_env_launcher("/usr/bin/env", &[]),
            ("/usr/bin/env".to_string(), Vec::new())
        );
    }

    // -- Codex 잠금 파일 -----------------------------------------------------

    #[test]
    fn codex_lock_path_must_sit_directly_in_the_lock_dir() {
        let lock_dir = Path::new("/home/x/.codex/thread-writer-locks");
        assert_eq!(
            lock_path_session_id(&lock_dir.join(format!("{CODEX_ID}.lock")), lock_dir),
            Some(CODEX_ID.to_string())
        );
        // 다른 디렉터리, 다른 확장자, 경로 문자가 든 이름은 모두 거절.
        assert_eq!(
            lock_path_session_id(Path::new("/tmp/other.lock"), lock_dir),
            None
        );
        assert_eq!(
            lock_path_session_id(&lock_dir.join("nested").join("a.lock"), lock_dir),
            None
        );
        assert_eq!(
            lock_path_session_id(&lock_dir.join(format!("{CODEX_ID}.json")), lock_dir),
            None
        );
    }

    /// 열린 fd에서 찾은 후보도 유일해야 한다 — 서로 다른 세션이 둘 보이면
    /// 어느 대화인지 알 수 없으므로 포기한다.
    #[test]
    fn open_fd_candidates_must_be_unique() {
        let one = |id: &str| vec![id.to_string()].into_iter();
        assert_eq!(unique_session_id(one(CODEX_ID)), Some(CODEX_ID.to_string()));
        // 같은 id가 여러 fd로 열려 있는 것은 한 가지다.
        assert_eq!(
            unique_session_id(vec![CODEX_ID.to_string(), CODEX_ID.to_string()].into_iter()),
            Some(CODEX_ID.to_string())
        );
        // 서로 다른 id가 둘 → 애매하므로 포기.
        assert_eq!(
            unique_session_id(vec![CODEX_ID.to_string(), CLAUDE_ID.to_string()].into_iter()),
            None
        );
        assert_eq!(unique_session_id(Vec::new().into_iter()), None);
    }

    /// 잠금 파일 둘이 동시에 열려 있으면(서로 다른 스레드) 아무것도 고르지
    /// 않는다 — 예전엔 fd 테이블에서 처음 걸린 것을 그대로 썼다.
    #[test]
    fn two_open_lock_paths_resolve_to_nothing() {
        let lock_dir = Path::new("/home/x/.codex/thread-writer-locks");
        let paths = [
            lock_dir.join(format!("{CODEX_ID}.lock")),
            lock_dir.join(format!("{CLAUDE_ID}.lock")),
        ];
        assert_eq!(
            unique_session_id(
                paths
                    .iter()
                    .filter_map(|path| lock_path_session_id(path, lock_dir))
            ),
            None
        );
        // 한 개만 열려 있으면 그것이다.
        assert_eq!(
            unique_session_id(
                paths[..1]
                    .iter()
                    .filter_map(|path| lock_path_session_id(path, lock_dir))
            ),
            Some(CODEX_ID.to_string())
        );
    }

    #[test]
    fn rollout_file_names_give_up_the_trailing_uuid_only() {
        assert_eq!(
            rollout_session_id(&format!("rollout-2026-09-11T16-03-37-{CODEX_ID}.jsonl")),
            Some(CODEX_ID.to_string())
        );
        for bad in [
            "rollout-2026-09-11.jsonl",            // uuid 없음
            &format!("rollout{CODEX_ID}.jsonl"),   // 접두사/구분자 없음
            &format!("other-{CODEX_ID}.jsonl"),    // rollout- 아님
            &format!("rollout-x-{CODEX_ID}.json"), // 확장자 다름
            "rollout-x-not-a-uuid-at-all-here-000000.jsonl",
        ] {
            assert_eq!(rollout_session_id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn lock_mtime_fallback_accepts_only_a_unique_match() {
        let base = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let start = 1_800_000_000u64;
        let at = |offset: i64| {
            if offset >= 0 {
                base + Duration::from_secs(offset as u64)
            } else {
                base - Duration::from_secs(offset.unsigned_abs())
            }
        };
        let lock = |name: &str| PathBuf::from(format!("/locks/{name}.lock"));

        // 창 안에 하나 → 그것.
        let one = vec![(lock(CODEX_ID), at(3)), (lock("other-id"), at(600))];
        assert_eq!(choose_lock_candidate(&one, start), Some(CODEX_ID.into()));

        // 창 안에 둘 → 애매하므로 포기.
        let two = vec![(lock(CODEX_ID), at(3)), (lock("second-id"), at(-5))];
        assert_eq!(choose_lock_candidate(&two, start), None);

        // 창 밖만 있으면 없음(경계 20초는 포함, 21초는 제외).
        assert_eq!(
            choose_lock_candidate(&[(lock(CODEX_ID), at(21))], start),
            None
        );
        assert_eq!(
            choose_lock_candidate(&[(lock(CODEX_ID), at(20))], start),
            Some(CODEX_ID.into())
        );
        assert_eq!(choose_lock_candidate(&[], start), None);
    }

    #[test]
    fn codex_resolution_reads_the_lock_dir_and_the_title_index() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock_dir = dir.path().join("thread-writer-locks");
        fs::create_dir_all(&lock_dir).unwrap();
        fs::write(lock_dir.join(format!("{CODEX_ID}.lock")), b"").unwrap();
        fs::write(
            dir.path().join("session_index.jsonl"),
            format!(
                "{{\"id\":\"{CODEX_ID}\",\"thread_name\":\"첫 이름\",\"updated_at\":\"a\"}}\n\
                 {{\"id\":\"other\",\"thread_name\":\"남의 것\"}}\n\
                 {{\"id\":\"{CODEX_ID}\",\"thread_name\":\"바뀐 이름\",\"updated_at\":\"b\"}}\n"
            ),
        )
        .unwrap();

        // 제목은 마지막 줄이 이긴다(파일은 덧붙여 쓰인다).
        assert_eq!(
            codex_title(dir.path(), CODEX_ID).as_deref(),
            Some("바뀐 이름")
        );
        assert_eq!(codex_title(dir.path(), "unknown-id"), None);

        // mtime 대체 경로: 지금 막 만든 잠금 파일 하나뿐이므로 유일하다.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let candidates = lock_candidates(&lock_dir);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            choose_lock_candidate(&candidates, now),
            Some(CODEX_ID.to_string())
        );
    }

    #[test]
    fn session_index_lines_that_are_not_json_are_skipped() {
        let text =
            format!("garbage\n\n{{\"id\":\"{CODEX_ID}\",\"thread_name\":\"ok\"}}\n{{broken\n");
        assert_eq!(
            parse_session_index_title(&text, CODEX_ID).as_deref(),
            Some("ok")
        );
        assert_eq!(parse_session_index_title("", CODEX_ID), None);
    }

    // -- 공용 ----------------------------------------------------------------

    #[test]
    fn observed_program_only_accepts_a_native_binary_name() {
        assert_eq!(
            observed_program(&brief(1, Some("/opt/homebrew/bin/claude")), "claude"),
            Some("/opt/homebrew/bin/claude".to_string())
        );
        // node 래퍼 설치는 복구 후보가 될 수 없다.
        assert_eq!(
            observed_program(&brief(1, Some("/usr/local/bin/node")), "claude"),
            None
        );
        assert_eq!(
            observed_program(&brief(1, Some("/bin/codex")), "claude"),
            None
        );
        assert_eq!(observed_program(&brief(1, None), "claude"), None);
    }

    #[test]
    fn resolve_refuses_a_brief_that_does_not_match_the_pid() {
        assert!(resolve("claude", 13185, &brief(999, Some("/bin/claude"))).is_none());
        // 지원하지 않는 에이전트는 언제나 None(실제 파일을 읽지 않는다).
        assert!(resolve("opencode", 1, &brief(1, Some("/bin/opencode"))).is_none());
    }

    #[test]
    fn read_bounded_refuses_oversized_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("big");
        fs::write(&path, vec![b'x'; 128]).unwrap();
        assert_eq!(read_bounded(&path, 127), None);
        assert_eq!(read_bounded(&path, 128).map(|s| s.len()), Some(128));
        assert_eq!(read_bounded(&dir.path().join("missing"), 128), None);
    }
}
