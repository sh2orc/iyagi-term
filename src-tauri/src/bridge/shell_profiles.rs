//! zsh 런치 프로필(`ccd`/`ccg`) 설치 — Settings → Launch profiles.
//!
//! `ccd`는 Claude Code를 사용자의 기본 로그인(Anthropic)으로, `ccg`는 iyagi
//! Term에 등록한 Z.ai Coding Plan 키로 띄운다. 두 함수 모두 데몬의
//! `claude-exec` 서브커맨드를 부를 뿐이라 **비밀은 어디에도 적히지 않는다**
//! (키는 데몬이 자기 비밀 저장소에서 푼다 — `claude_provider` 라우팅과 같은 길).
//!
//! 계약(Claude/Codex hooks 연동과 같은 동의 모델):
//! - `status`는 **절대** 파일을 고치지 않는다. 현재 상태와 적용될 블록·스크립트
//!   미리보기를 함께 돌려주고, UI가 그것을 보여 준 뒤 사용자가 `apply`를
//!   눌렀을 때만 쓴다.
//! - 쓰기는 `hooks_json::write_settings_file`을 그대로 쓴다 — 최초 적용 전
//!   `.iyagi.bak` 원본 백업 한 번, 고유 임시 파일 → `sync_all` → atomic
//!   rename, 기존 파일 권한 보존. 사용자의 `~/.zshrc`도 같은 보호를 받는다.
//! - rc에 넣는 것은 마커 블록 **한 덩어리**(`source` 한 줄)뿐이다. 함수 본문은
//!   우리가 소유한 `<data_dir>/config/shell/claude-profiles.zsh`에 있어서,
//!   모델을 바꿔도 rc는 그대로고 제거는 마커 사이만 정확히 도려낸다.
//! - 이미 `ccd`/`ccg`를 쓰고 있는 사용자의 정의는 **기본적으로** 건드리지 않는다.
//!   정적 스캔(+설치 전에 한해 `zsh -ic 'whence -w …'` 동적 탐지)에서 하나라도
//!   걸리면 `apply`는 거절하고 무엇이 어디에 있었는지 그대로 돌려준다.
//! - 사용자가 명시적으로 "교체"를 고르면(`replace_existing`) 사용자의 줄은
//!   **여전히 지우지 않고**, 우리 블록을 `~/.zshrc` 맨 끝으로 옮겨 나중에
//!   정의된 우리 함수가 이기게 한다(스크립트는 같은 이름의 alias도 먼저
//!   `unalias`한다). 제거하면 블록만 빠지므로 사용자의 원래 정의가 그대로
//!   되살아난다. `.zshrc`보다 **나중에** 읽히는 파일(`.zlogin`)의 정의는 이
//!   방식으로 가릴 수 없어서 교체도 거절한다.

use std::io::Read as _;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use term_contracts::launch::ZAI_CLAUDE_MAIN_MODELS;

use super::hooks_json;

/// 관리 블록의 시작·끝 마커. 이 두 줄 사이만 우리 것이다(문자열이 바뀌면
/// 기존 설치를 못 알아보므로 절대 건드리지 않는다).
const BLOCK_BEGIN: &str = "# >>> Iyagi claude profiles (ccd/ccg) >>>";
const BLOCK_END: &str = "# <<< Iyagi claude profiles (ccd/ccg) <<<";

/// 우리가 소유한 스크립트 파일(`<data_dir>/config/shell/<이 이름>`).
const SCRIPT_FILE: &str = "claude-profiles.zsh";

/// 블록이 들어가는 rc 파일. zsh의 대화형 rc 하나만 고친다.
const RC_FILE: &str = ".zshrc";

/// `ccd`/`ccg`가 Claude Code에 항상 넘기는 권한 모드 플래그([`render_script`]).
const BYPASS_FLAG: &str = "--dangerously-skip-permissions";

/// 설치하는 함수 이름(충돌 검사 대상).
const PROFILE_NAMES: [&str; 2] = ["ccd", "ccg"];

/// 정적 충돌 스캔 대상. 존재하는 것만 읽고, 각각 1 MiB까지만 본다.
const SCANNED_FILES: [&str; 6] = [
    RC_FILE,
    ".zprofile",
    ".zshenv",
    ".zlogin",
    ".zsh_aliases",
    ".aliases",
];

/// 한 파일에서 읽어 보는 최대 바이트(거대한 rc가 설정 탭을 멈추지 않게).
const SCAN_FILE_CAP: u64 = 1024 * 1024;

/// 돌려주는 충돌 개수 상한(UI 목록과 오류 details가 무한히 커지지 않게).
const MAX_CONFLICTS: usize = 32;

/// 충돌 줄 원문의 길이 상한(문자 수).
const MAX_CONFLICT_TEXT: usize = 200;

/// `.zshrc`보다 나중에 읽혀 우리 블록을 다시 덮는 파일(로그인 셸의 `.zlogin`).
/// 여기 있는 정의는 블록을 rc 끝으로 옮겨도 가릴 수 없다.
const LATE_FILES: [&str; 1] = [".zlogin"];

/// 동적 탐지 결과의 `file` 자리 표시(진짜 파일을 모르기 때문).
const DYNAMIC_SOURCE: &str = "(zsh)";

/// `zsh -ic 'whence -w …'` 동적 탐지의 시한(초). 넘기면 죽이고 결과를 버린다.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// 동적 탐지에서 읽어 들이는 출력 상한.
const PROBE_OUTPUT_CAP: u64 = 8 * 1024;

/// `ccg`의 기본 주 모델. 허용 목록은 `ZAI_CLAUDE_MAIN_MODELS`(계약 크레이트)와
/// 하나의 출처를 공유한다 — 데몬이 받아들이지 않을 값을 스크립트에 박지 않는다.
pub const DEFAULT_MAIN_MODEL: &str = ZAI_CLAUDE_MAIN_MODELS[0];

#[derive(Debug, thiserror::Error)]
pub enum ShellProfilesError {
    #[error("home directory not found")]
    NoHome,
    #[error("unsupported environment: {0}")]
    Unsupported(&'static str),
    #[error("unknown main model")]
    InvalidModel,
    #[error("file unreadable: {0}")]
    Read(String),
    #[error("write failed: {0}")]
    Write(String),
    #[error("{} conflicting ccd/ccg definition(s) already exist", .0.len())]
    Conflict(Vec<Conflict>),
    #[error("{} ccd/ccg definition(s) load after ~/.zshrc and cannot be replaced", .0.len())]
    Unreplaceable(Vec<Conflict>),
}

impl ShellProfilesError {
    pub fn code(&self) -> &'static str {
        match self {
            ShellProfilesError::NoHome => "no_home",
            ShellProfilesError::Unsupported(_) => "shell_profiles_unsupported",
            ShellProfilesError::InvalidModel => "invalid_main_model",
            ShellProfilesError::Read(_) => "read_failed",
            ShellProfilesError::Write(_) => "write_failed",
            ShellProfilesError::Conflict(_) => "shell_profiles_conflict",
            ShellProfilesError::Unreplaceable(_) => "shell_profiles_conflict_unreplaceable",
        }
    }
}

/// 이미 존재하는 `ccd`/`ccg` 정의 한 건. 우리는 이 줄을 고치지 않는다 — 사용자가
/// 직접 정리하거나 "교체"(우리 블록이 나중에 정의해 가린다)를 고르도록
/// "어느 파일 몇 번째 줄"을 그대로 보여 준다.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    /// `ccd` 또는 `ccg`.
    pub name: String,
    /// 발견된 파일 경로. 동적 탐지 결과는 `DYNAMIC_SOURCE`(`(zsh)`).
    pub file: String,
    /// 1부터 세는 줄 번호. 동적 탐지 결과는 0(줄을 모른다).
    pub line: usize,
    /// 문제 줄 원문(앞뒤 공백 제거, 200자에서 자름).
    pub text: String,
    /// 우리 블록을 rc 끝에 두면 이 정의를 가릴 수 있는가(`.zlogin`이면 거짓).
    pub replaceable: bool,
}

/// 설치 상태 + 적용 제안. `status`는 이 값을 만들기 위해 파일을 읽기만 한다.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellProfilesStatus {
    /// 이 환경에서 설치할 수 있는가(아래 `reason`이 비어 있는가).
    pub supported: bool,
    /// 설치 불가 사유: `windows` | `no_home` | `shell_not_zsh` |
    /// `daemon_binary_missing`. 가능하면 `None`.
    pub reason: Option<&'static str>,
    /// 로그인 셸(`$SHELL`). 값이 없으면 `None`.
    pub shell: Option<String>,
    /// 블록이 들어갈 rc 경로(home을 모르면 빈 문자열).
    pub rc_path: String,
    pub rc_exists: bool,
    /// rc에 우리 마커 블록이 있는가.
    pub installed: bool,
    /// 설치돼 있고 블록·스크립트 내용이 지금 렌더링과 완전히 같은가.
    pub up_to_date: bool,
    pub script_path: String,
    pub script_exists: bool,
    /// 스크립트가 부를 데몬 절대경로(못 찾으면 `None`).
    pub daemon_binary: Option<String>,
    /// 적용하면 rc에 들어갈 블록(이미 최신이면 `None`).
    pub proposed_block: Option<String>,
    /// 적용하면 스크립트 파일에 들어갈 내용(항상 미리보기로 제공).
    pub script_preview: String,
    /// 우리 블록 밖에 이미 존재하는 `ccd`/`ccg` 정의들.
    pub conflicts: Vec<Conflict>,
    /// 설치돼 있고, 위 충돌이 **모두** 우리 블록보다 먼저 읽혀 지금 우리
    /// 함수가 이기고 있는가(= 사용자가 교체를 골랐던 상태). 충돌이 없으면 거짓.
    pub overriding: bool,
    /// `ccg`가 쓸 주 모델.
    pub main_model: String,
}

/// 환경 의존 입력 묶음. 테스트가 로그인 셸을 주입하고 동적 탐지를 끌 수 있게
/// 한 곳에 모은다(단위 테스트가 진짜 zsh를 띄우면 안 된다).
#[derive(Debug, Clone)]
struct Probe {
    shell: Option<String>,
    allow_dynamic: bool,
}

impl Probe {
    fn from_env() -> Self {
        Self {
            shell: login_shell(),
            allow_dynamic: true,
        }
    }

    #[cfg(test)]
    fn zsh_static() -> Self {
        Self {
            shell: Some("/bin/zsh".to_string()),
            allow_dynamic: false,
        }
    }
}

/// `$HOME`. Windows에서는 지원 대상이 아니지만 상태 계산은 계속 돌아야 하므로
/// `USERPROFILE`도 함께 본다(claude_hooks와 같은 해석).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// 로그인 셸. 빈 값은 "모름"으로 본다.
fn login_shell() -> Option<String> {
    std::env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// 경로에서 프로그램 이름만(윈도 `.exe`는 뗀다).
fn shell_basename(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.get(name.len().saturating_sub(4)..) {
        Some(tail) if tail.eq_ignore_ascii_case(".exe") => &name[..name.len() - 4],
        _ => name,
    }
}

/// `$SHELL`이 zsh인가. **값이 없으면 참**으로 본다 — macOS 기본 셸이 zsh이고,
/// GUI 앱은 `$SHELL`을 물려받지 못하는 실행 경로가 있어서 "모름"을 "아님"으로
/// 단정하면 정상 환경에서 기능이 통째로 잠긴다.
fn shell_is_zsh(shell: Option<&str>) -> bool {
    match shell {
        Some(path) => shell_basename(path) == "zsh",
        None => true,
    }
}

/// 데몬이 받아들이는 주 모델인가.
pub fn is_known_main_model(value: &str) -> bool {
    ZAI_CLAUDE_MAIN_MODELS.contains(&value)
}

/// 미리보기용 모델 정규화: 모르는 값(과 `None`)은 기본 모델로 되돌린다.
/// `apply`는 이걸 쓰지 않는다 — 거기서는 모르는 값을 조용히 바꾸지 않고 거절한다.
pub fn sanitize_main_model(value: Option<String>) -> String {
    value
        .filter(|model| is_known_main_model(model))
        .unwrap_or_else(|| DEFAULT_MAIN_MODEL.to_string())
}

/// 우리가 소유한 스크립트 경로.
pub fn script_path(data_dir: &Path) -> PathBuf {
    data_dir.join("config").join("shell").join(SCRIPT_FILE)
}

/// 블록이 들어갈 rc 경로.
pub fn rc_path(home: &Path) -> PathBuf {
    home.join(RC_FILE)
}

// ------------------------------------------------------------- 텍스트 렌더링

/// POSIX 홑따옴표 인용. 이 모듈이 만드는 텍스트는 언제나 zsh가 읽으므로
/// (Windows는 애초에 `supported=false`) 플랫폼 분기 없이 unix 규칙만 쓴다.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// 겹따옴표 **안**에 들어갈 문자열 이스케이프. `${VAR:-기본값}`의 기본값
/// 자리에 경로를 그대로 박기 때문에, 경로에 든 `"` `$` `` ` `` `\`가 셸에
/// 해석되지 않도록 백슬래시를 앞세운다.
fn double_quoted_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 스크립트 파일 전체 내용(순수 함수).
///
/// `IYAGI_DAEMON_BIN`을 먼저 보는 이유: 개발 빌드나 이동 설치에서 사용자가
/// rc를 다시 쓰지 않고도 데몬 경로를 덮어쓸 수 있게 하는 탈출구다.
///
/// 두 함수 모두 `--dangerously-skip-permissions`로 띄운다(bypass permissions
/// 모드). `ccd`/`ccg`는 손으로 쓰던 같은 이름의 헬퍼를 대체하는 것이라 그
/// 동작을 그대로 잇는다 — 플래그가 없으면 Claude Code가 설정의 기본 모드
/// (auto 등)로 떠서 "교체" 뒤에 권한 모드가 조용히 바뀐다.
pub fn render_script(daemon_bin_abs: &str, data_dir: &str, main_model: &str) -> String {
    let binary = double_quoted_escape(daemon_bin_abs);
    let dir = shell_quote(data_dir);
    let model = shell_quote(main_model);
    format!(
        "# Generated by iyagi (Settings → Launch profiles → ccd / ccg). Do not edit — re-apply from the app.\n\
         # ccd: Claude Code → Anthropic (your default login)\n\
         # ccg: Claude Code → Z.ai GLM (Coding Plan key registered in iyagi)\n\
         # Both start in bypass-permissions mode ({BYPASS_FLAG}).\n\
         # `|| true`: 두 별칭이 없으면 unalias가 1을 돌려주는데, 사용자의\n\
         # `setopt err_exit`가 스크립트 첫 줄에서 소싱을 끊어버리는 것을 막는다.\n\
         unalias ccd ccg 2>/dev/null || true\n\
         ccd() {{ \"${{IYAGI_DAEMON_BIN:-{binary}}}\" --data-dir {dir} claude-exec --provider anthropic -- {BYPASS_FLAG} \"$@\"; }}\n\
         ccg() {{ \"${{IYAGI_DAEMON_BIN:-{binary}}}\" --data-dir {dir} claude-exec --provider zai --main-model {model} -- {BYPASS_FLAG} \"$@\"; }}\n"
    )
}

/// rc에 들어갈 관리 블록(순수 함수). 스크립트가 사라져도 셸이 깨지지 않도록
/// `-r` 검사 뒤에만 `source`한다 — 앱을 지운 사용자의 새 셸이 매번 오류를
/// 뱉지 않는다.
pub fn render_block(script_path: &str) -> String {
    let quoted = shell_quote(script_path);
    format!("{BLOCK_BEGIN}\n[ -r {quoted} ] && source {quoted}\n{BLOCK_END}\n")
}

/// rc 안의 관리 블록 범위(끝 마커의 줄바꿈까지 포함).
///
/// 끝 마커 없이 시작 마커만 남은 찌꺼기가 있으면 **마지막** 시작 마커를 쓴다 —
/// 그렇지 않으면 사용자가 손으로 지우다 남긴 한 줄 때문에 새 블록까지 통째로
/// 삼켜 그 사이의 사용자 설정이 사라진다.
pub fn find_block(rc: &str) -> Option<Range<usize>> {
    let mut begin: Option<usize> = None;
    let mut offset = 0usize;
    for line in rc.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed == BLOCK_BEGIN {
            begin = Some(offset);
        } else if trimmed == BLOCK_END {
            if let Some(start) = begin {
                return Some(start..offset + line.len());
            }
        }
        offset += line.len();
    }
    None
}

/// 블록을 제자리 교체하거나(있을 때) 끝에 덧붙인다(없을 때). 멱등이다 —
/// 같은 블록으로 두 번 부르면 두 번째는 글자 하나 바뀌지 않는다.
pub fn upsert_block(rc: &str, block: &str) -> String {
    if let Some(range) = find_block(rc) {
        let mut out = String::with_capacity(rc.len() + block.len());
        out.push_str(&rc[..range.start]);
        out.push_str(block);
        out.push_str(&rc[range.end..]);
        return out;
    }
    if rc.trim().is_empty() {
        return block.to_string();
    }
    let mut out = String::with_capacity(rc.len() + block.len() + 2);
    out.push_str(rc);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    // 사용자의 마지막 줄과 우리 블록 사이의 빈 줄(제거할 때 다시 걷어낸다).
    out.push('\n');
    out.push_str(block);
    out
}

/// 블록을 rc **맨 끝**에 둔다(이미 있던 블록은 걷어내고 다시 붙인다). 교체
/// 모드 전용이다 — 사용자의 `ccd`/`ccg`보다 나중에 읽혀야 우리 정의가 이긴다.
/// 이미 끝에 같은 블록이 있으면 글자 하나 바뀌지 않는다(멱등).
pub fn move_block_to_end(rc: &str, block: &str) -> String {
    let (stripped, _) = strip_block(rc);
    upsert_block(&stripped, block)
}

/// 블록만 도려낸다. 우리가 넣었던 빈 구분 줄도 함께 걷어내므로 설치 전
/// 원문으로 정확히 되돌아간다. 두 번째 호출은 `false`(바뀐 것 없음).
pub fn strip_block(rc: &str) -> (String, bool) {
    let mut out = rc.to_string();
    let mut changed = false;
    // 손으로 복사해 둔 블록이 여러 벌 있어도 전부 치운다(매 회 최소 두 줄이
    // 줄어들므로 반드시 끝난다).
    while let Some(range) = find_block(&out) {
        let mut next = String::with_capacity(out.len());
        next.push_str(&out[..range.start]);
        if next.ends_with("\n\n") {
            next.pop();
        }
        next.push_str(&out[range.end..]);
        out = next;
        changed = true;
    }
    (out, changed)
}

// ------------------------------------------------------------------ 충돌 검사

/// 잘라낸 줄 원문(경계를 문자 단위로 지켜 UTF-8을 깨지 않는다).
fn clip(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((index, _)) => format!("{}…", &text[..index]),
        None => text.to_string(),
    }
}

/// 파일을 최대 `SCAN_FILE_CAP`바이트까지 읽는다. 없거나 못 읽으면 `None`
/// (스캔은 최선 노력이다 — 읽기 실패가 설정 탭을 막지 않는다).
fn read_capped(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut buffer = Vec::new();
    file.take(SCAN_FILE_CAP).read_to_end(&mut buffer).ok()?;
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

/// 정확한 읽기(있음/없음/오류를 구분). 쓰기 전에는 이걸 쓴다 — 읽지 못한
/// 파일을 "빈 파일"로 착각해 사용자의 rc를 통째로 갈아엎으면 안 된다.
fn read_text(path: &Path) -> Result<Option<String>, ShellProfilesError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ShellProfilesError::Read(error.to_string())),
    }
}

/// 이 줄이 `name`을 정의하는가. 정규식 없이 세 형태만 본다:
/// `alias name=…`, `name() …`(`name () …` 포함), `function name …`.
fn line_defines(line: &str, name: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }
    // alias name=…
    let alias = trimmed
        .strip_prefix("alias")
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .and_then(|rest| rest.trim_start().strip_prefix(name))
        .is_some_and(|after| after.starts_with('='));
    // name() { … } / name () { … }
    let shorthand = trimmed
        .strip_prefix(name)
        .is_some_and(|after| after.trim_start().starts_with('('));
    // function name { … } — `function ccdx`는 다른 함수다(이름 뒤가 식별자면 아님).
    let keyword = trimmed
        .strip_prefix("function")
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .and_then(|rest| rest.trim_start().strip_prefix(name))
        .is_some_and(|after| !after.starts_with(|c: char| c.is_alphanumeric() || c == '_'));
    alias || shorthand || keyword
}

/// 한 파일 본문을 훑어 충돌을 모은다. `skip`은 (있다면) 우리 관리 블록 범위로,
/// 그 안의 줄은 "우리가 넣은 것"이라 충돌이 아니다.
fn scan_text(
    text: &str,
    file: &str,
    replaceable: bool,
    skip: Option<Range<usize>>,
    out: &mut Vec<Conflict>,
) {
    let mut offset = 0usize;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let start = offset;
        offset += line.len();
        if out.len() >= MAX_CONFLICTS {
            return;
        }
        if skip.as_ref().is_some_and(|range| range.contains(&start)) {
            continue;
        }
        for name in PROFILE_NAMES {
            if line_defines(line, name) {
                out.push(Conflict {
                    name: name.to_string(),
                    file: file.to_string(),
                    line: index + 1,
                    text: clip(line.trim(), MAX_CONFLICT_TEXT),
                    replaceable,
                });
                break;
            }
        }
    }
}

/// 홈의 zsh rc 계열 파일들을 정적으로 훑어 이미 존재하는 `ccd`/`ccg` 정의를
/// 찾는다(우리 관리 블록 안은 제외). 파일을 고치지 않는 순수 조회다.
pub fn find_conflicts(home: &Path) -> Vec<Conflict> {
    let mut out = Vec::new();
    for name in SCANNED_FILES {
        if out.len() >= MAX_CONFLICTS {
            break;
        }
        let path = home.join(name);
        let Some(text) = read_capped(&path) else {
            continue;
        };
        let skip = if name == RC_FILE {
            find_block(&text)
        } else {
            None
        };
        let replaceable = !LATE_FILES.contains(&name);
        scan_text(&text, &path.display().to_string(), replaceable, skip, &mut out);
    }
    out
}

/// `.zshrc`가 다른 파일을 `source`(또는 `.`)하는 줄이 우리 블록 시작 줄
/// **뒤**에 하나라도 있으면 참 — 그 파일의 정의는 우리보다 나중에 로드돼
/// 이긴다("가려진" 것이 아니다). 최선 노력 판정: 주석은 제외하되 조건부
/// source(`[ -f … ] && source …`)까지 잡도록 "source"가 나온 줄은 넓게 본다 —
/// 놓치면 UI가 실제 충돌을 숨기는 방향으로만 흘린다.
fn rc_sources_file_after_block(rc_text: &str, conflict_file: &str, begin_line: usize) -> bool {
    let Some(base) = std::path::Path::new(conflict_file)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
    else {
        return false;
    };
    rc_text.lines().enumerate().any(|(index, line)| {
        let lineno = index + 1;
        if lineno <= begin_line || line.trim_start().starts_with('#') {
            return false;
        }
        let trimmed = line.trim();
        (trimmed.starts_with(". ") || trimmed.contains("source")) && line.contains(base)
    })
}

/// `whence -w` 출력 해석: `ccd: function` 같은 줄에서 `none`이 아닌 것만 충돌.
fn parse_whence(stdout: &str) -> Vec<Conflict> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some((name, kind)) = line.trim().split_once(':') else {
            continue;
        };
        let (name, kind) = (name.trim(), kind.trim());
        if !PROFILE_NAMES.contains(&name) || kind.is_empty() || kind == "none" {
            continue;
        }
        out.push(Conflict {
            name: name.to_string(),
            file: DYNAMIC_SOURCE.to_string(),
            line: 0,
            text: clip(&format!("{name}: {kind}"), MAX_CONFLICT_TEXT),
            // `zsh -ic`는 비로그인 셸이라 `.zlogin`을 읽지 않는다 — 여기서 잡힌
            // 정의는 `.zshrc` 끝보다 먼저 생긴 것이다.
            replaceable: true,
        });
    }
    out
}

/// 최선 노력 동적 탐지: 사용자의 대화형 zsh를 한 번 띄워 `ccd`/`ccg`가 이미
/// 잡혀 있는지 묻는다. 정적 스캔이 못 보는 곳(oh-my-zsh 플러그인, `.zshrc`가
/// source하는 사설 파일)을 잡기 위한 것이라, 3초 안에 답하지 않거나 어떤
/// 이유로든 실패하면 **조용히 빈 목록**을 돌려준다(설치를 막지 않는다).
fn probe_dynamic_conflicts() -> Vec<Conflict> {
    let mut command = std::process::Command::new("zsh");
    command
        .arg("-ic")
        .arg("whence -w ccd ccg")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let Ok(mut child) = command.spawn() else {
        return Vec::new();
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Vec::new();
    };
    // 두 파이프 모두 별도 스레드가 비운다 — 대화형 rc가 쏟는 잡음이 파이프를
    // 채우면 자식이 write에서 멈춰 아래 시한 루프가 매번 kill로 끝난다.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = (&mut stdout)
            .take(PROBE_OUTPUT_CAP)
            .read_to_end(&mut buffer);
        let _ = tx.send(buffer);
    });
    if let Some(mut stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = (&mut stderr).take(PROBE_OUTPUT_CAP).read_to_end(&mut sink);
        });
    }
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            // 종료 코드는 보지 않는다 — `whence`는 못 찾으면 비영으로 끝난다.
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
        }
    }
    match rx.recv_timeout(Duration::from_millis(500)) {
        Ok(bytes) => parse_whence(&String::from_utf8_lossy(&bytes)),
        Err(_) => Vec::new(),
    }
}

/// [`DYNAMIC_PROBE_CACHE`]가 유효한 것으로 보는 기간. `status`는 설정 탭이
/// 뜰 때, 그리고 모델을 바꿀 때마다(=매 렌더) 호출되는데, `probe_dynamic_conflicts`는
/// 그때마다 최대 `PROBE_TIMEOUT`(3초)까지 대화형 zsh를 새로 띄워 블로킹한다.
/// 사용자가 셸 설정을 60초 안에 다시 고칠 가능성은 낮으므로, 그 사이의
/// 반복 호출은 마지막 결과를 그대로 돌려준다.
const DYNAMIC_PROBE_TTL: Duration = Duration::from_secs(60);

/// 프로세스 전역 캐시(마지막으로 탐지한 시각, 그때의 결과). `status`만 이걸
/// 거친다 — `apply`는 실제로 파일을 쓰기 직전이라 오래된 "충돌 없음"을 믿고
/// 사용자 정의를 덮어쓰면 안 되므로 항상 새로 탐지한다(아래 `force` 인자).
static DYNAMIC_PROBE_CACHE: std::sync::Mutex<Option<(Instant, Vec<Conflict>)>> =
    std::sync::Mutex::new(None);

/// 캐시를 거친 동적 탐지. `force`가 참이면(=`apply` 직전) 캐시를 무시하고
/// 항상 새로 띄운 뒤 그 결과로 캐시를 갱신한다. `status_with`가 이미
/// `probe.allow_dynamic`으로 걸러 부르므로, 단위 테스트는 이 함수까지
/// 내려오지 않는다(진짜 zsh를 띄우지 않는다).
fn probe_dynamic_conflicts_cached(force: bool) -> Vec<Conflict> {
    if !force {
        if let Some((at, conflicts)) = DYNAMIC_PROBE_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            if at.elapsed() < DYNAMIC_PROBE_TTL {
                return conflicts.clone();
            }
        }
    }
    let found = probe_dynamic_conflicts();
    *DYNAMIC_PROBE_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((Instant::now(), found.clone()));
    found
}

// ------------------------------------------------------------------ 상태·적용

fn write_error(error: hooks_json::HooksError) -> ShellProfilesError {
    ShellProfilesError::Write(error.to_string())
}

/// 스크립트 디렉터리는 소유자 전용으로 만든다(비밀은 없지만, 우리가 만드는
/// 데이터 루트의 다른 디렉터리와 같은 규칙을 지킨다).
fn ensure_private_dir(dir: &Path) -> Result<(), ShellProfilesError> {
    std::fs::create_dir_all(dir).map_err(|e| ShellProfilesError::Write(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| ShellProfilesError::Write(e.to_string()))?;
    }
    Ok(())
}

/// 현재 상태 + 적용 제안. **파일을 절대 고치지 않는다.**
pub fn status(
    home: Option<&Path>,
    data_dir: &Path,
    daemon_bin: Option<&Path>,
    main_model: &str,
) -> ShellProfilesStatus {
    // 일반 조회는 캐시를 쓴다 — 아래 `status_with`의 `fresh_probe` 설명 참고.
    status_with(home, data_dir, daemon_bin, main_model, &Probe::from_env(), false)
}

/// `fresh_probe`가 참이면 동적 탐지 캐시를 건너뛰고 항상 새로 탐지한다
/// (`apply_with`가 쓰기 직전에 쓴다). 그 외에는 [`DYNAMIC_PROBE_CACHE`]를
/// 그대로 재사용한다 — 마운트 때와 모델 변경 때마다 반복되는 `status` 호출이
/// 매번 대화형 zsh를 새로 띄우지 않게 한다.
fn status_with(
    home: Option<&Path>,
    data_dir: &Path,
    daemon_bin: Option<&Path>,
    main_model: &str,
    probe: &Probe,
    fresh_probe: bool,
) -> ShellProfilesStatus {
    let script = script_path(data_dir);
    let script_display = script.display().to_string();
    let block = render_block(&script_display);
    let binary = daemon_bin.map(|path| path.display().to_string());
    // 미리보기는 데몬을 못 찾아도 보여 준다 — 사용자가 무엇이 설치될지 읽고
    // 판단할 수 있어야 한다(설치 자체는 `daemon_binary_missing`으로 막힌다).
    let script_text = render_script(
        binary
            .as_deref()
            .unwrap_or_else(|| super::daemon_manager::daemon_binary_name()),
        &data_dir.display().to_string(),
        main_model,
    );

    let rc = home.map(rc_path);
    let rc_exists = rc.as_deref().is_some_and(Path::is_file);
    let rc_text = rc
        .as_deref()
        .and_then(|path| read_text(path).ok().flatten())
        .unwrap_or_default();
    let block_range = find_block(&rc_text);
    let current_block = block_range.clone().map(|range| rc_text[range].to_string());
    let installed = current_block.is_some();
    let script_on_disk = std::fs::read_to_string(&script).ok();
    let up_to_date = current_block.as_deref() == Some(block.as_str())
        && script_on_disk.as_deref() == Some(script_text.as_str());

    let reason = if cfg!(windows) {
        // zsh 프로필은 unix 전용이다(WSL 안쪽은 이 앱이 건드리는 홈이 아니다).
        Some("windows")
    } else if home.is_none() {
        Some("no_home")
    } else if !shell_is_zsh(probe.shell.as_deref()) {
        Some("shell_not_zsh")
    } else if daemon_bin.is_none() {
        Some("daemon_binary_missing")
    } else {
        None
    };

    let mut conflicts = home.map(find_conflicts).unwrap_or_default();
    // 동적 탐지는 설치 전에만 의미가 있다 — 설치한 뒤에는 우리 함수가 잡히므로
    // 자기 자신을 충돌로 신고하게 된다.
    if reason.is_none() && !installed && probe.allow_dynamic {
        for found in probe_dynamic_conflicts_cached(fresh_probe) {
            if !conflicts.iter().any(|known| known.name == found.name) {
                conflicts.push(found);
            }
        }
    }

    // 우리 블록이 시작하는 줄(1-based). 이보다 위에 있는 rc의 정의는 우리
    // `source`가 나중에 덮으므로 "가려진" 것이다.
    let block_line = block_range.map(|range| rc_text[..range.start].matches('\n').count() + 1);
    let rc_display = rc.as_deref().map(|path| path.display().to_string());
    let overriding = match block_line {
        Some(begin) if !conflicts.is_empty() => conflicts.iter().all(|conflict| {
            if rc_display.as_deref() == Some(conflict.file.as_str()) {
                conflict.line < begin
            } else {
                // 다른 파일의 정의는 그 파일이 우리 블록 **앞에서** source될
                // 때만 가려진다 — 블록 뒤의 source 줄이면 사용자 정의가 나중에
                // 로드돼 이긴다(놓치면 UI가 충돌을 숨기므로 넓게 잡는다).
                conflict.replaceable && !rc_sources_file_after_block(&rc_text, &conflict.file, begin)
            }
        }),
        _ => false,
    };

    ShellProfilesStatus {
        supported: reason.is_none(),
        reason,
        shell: probe.shell.clone(),
        rc_path: rc
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        rc_exists,
        installed,
        up_to_date,
        script_path: script_display,
        script_exists: script.is_file(),
        daemon_binary: binary,
        proposed_block: if up_to_date { None } else { Some(block) },
        script_preview: script_text,
        conflicts,
        overriding,
        main_model: main_model.to_string(),
    }
}

/// 동의 후 적용: 스크립트를 쓰고 rc에 블록을 넣는다(백업 + atomic 쓰기).
/// 이미 같은 내용이면 어느 파일도 다시 쓰지 않는다(멱등).
///
/// `replace_existing`이 거짓이면 사용자 정의 `ccd`/`ccg`가 있을 때 거절한다.
/// 참이면(사용자가 "교체"를 고름) 사용자의 줄은 그대로 두고 블록을 rc 맨
/// 끝으로 옮겨 우리 정의가 이기게 한다 — `.zlogin`처럼 그 뒤에 읽히는
/// 정의가 하나라도 있으면 가릴 수 없으므로 그때도 거절한다.
pub fn apply(
    home: &Path,
    data_dir: &Path,
    daemon_bin: Option<&Path>,
    main_model: &str,
    replace_existing: bool,
) -> Result<ShellProfilesStatus, ShellProfilesError> {
    let probe = Probe::from_env();
    apply_with(home, data_dir, daemon_bin, main_model, replace_existing, &probe)
}

fn apply_with(
    home: &Path,
    data_dir: &Path,
    daemon_bin: Option<&Path>,
    main_model: &str,
    replace_existing: bool,
    probe: &Probe,
) -> Result<ShellProfilesStatus, ShellProfilesError> {
    if !is_known_main_model(main_model) {
        return Err(ShellProfilesError::InvalidModel);
    }
    // 쓰기 직전 판정이다 — 캐시된 "충돌 없음"을 믿고 사용자 정의를 덮어쓰지
    // 않도록 항상 새로 탐지한다(`fresh_probe = true`).
    let current = status_with(Some(home), data_dir, daemon_bin, main_model, probe, true);
    if !current.supported {
        return Err(ShellProfilesError::Unsupported(current.reason.unwrap_or("unsupported")));
    }
    // 사용자가 이미 쓰고 있는 `ccd`/`ccg`는 우리 것이 아니다 — 교체를 고르지
    // 않았다면 무엇이 걸렸는지 그대로 돌려주고 멈춘다(파일은 하나도 건드리지
    // 않았다). 교체를 골랐어도 `.zshrc` 뒤에 읽히는 정의는 가릴 수 없다.
    let replacing = !current.conflicts.is_empty();
    if replacing && !replace_existing {
        return Err(ShellProfilesError::Conflict(current.conflicts));
    }
    if replacing && current.conflicts.iter().any(|conflict| !conflict.replaceable) {
        let late = current.conflicts.into_iter().filter(|c| !c.replaceable).collect();
        return Err(ShellProfilesError::Unreplaceable(late));
    }
    let binary = daemon_bin.ok_or(ShellProfilesError::Unsupported("daemon_binary_missing"))?;

    let script = script_path(data_dir);
    ensure_private_dir(script.parent().expect("script path has a parent"))?;
    let script_text = render_script(
        &binary.display().to_string(),
        &data_dir.display().to_string(),
        main_model,
    );
    if std::fs::read_to_string(&script).ok().as_deref() != Some(script_text.as_str()) {
        hooks_json::write_settings_file(&script, &script_text).map_err(write_error)?;
    }

    let rc = rc_path(home);
    let rc_text = read_text(&rc)?.unwrap_or_default();
    let block = render_block(&script.display().to_string());
    // 교체 모드에서만 블록을 끝으로 옮긴다 — 충돌이 없으면 사용자가 옮겨 둔
    // 자리(있다면)를 존중해 제자리 교체한다.
    let next = if replacing {
        move_block_to_end(&rc_text, &block)
    } else {
        upsert_block(&rc_text, &block)
    };
    if next != rc_text {
        hooks_json::write_settings_file(&rc, &next).map_err(write_error)?;
    }
    // 설치 직후라 `installed = true` — 아래 `status_with`는 동적 탐지를 아예
    // 건너뛰므로 `fresh_probe` 값은 결과에 영향을 주지 않는다.
    Ok(status_with(Some(home), data_dir, daemon_bin, main_model, probe, false))
}

/// 제거: rc에서 우리 블록만 도려내고 스크립트 파일을 지운다. 사용자가 셸을
/// 바꾼 뒤에도(= `supported=false`) 항상 동작해야 한다.
pub fn remove(
    home: &Path,
    data_dir: &Path,
    daemon_bin: Option<&Path>,
    main_model: &str,
) -> Result<ShellProfilesStatus, ShellProfilesError> {
    remove_with(home, data_dir, daemon_bin, main_model, &Probe::from_env())
}

fn remove_with(
    home: &Path,
    data_dir: &Path,
    daemon_bin: Option<&Path>,
    main_model: &str,
    probe: &Probe,
) -> Result<ShellProfilesStatus, ShellProfilesError> {
    let rc = rc_path(home);
    if let Some(rc_text) = read_text(&rc)? {
        let (next, changed) = strip_block(&rc_text);
        if changed {
            hooks_json::write_settings_file(&rc, &next).map_err(write_error)?;
        }
    }
    match std::fs::remove_file(script_path(data_dir)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ShellProfilesError::Write(error.to_string())),
    }
    // 제거는 반환 상태의 충돌 목록이 즉시 최신일 필요가 없다 — 캐시를 쓴다.
    Ok(status_with(Some(home), data_dir, daemon_bin, main_model, probe, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `rc_sources_file_after_block` — 블록 뒤의 source 줄 판정(순수 함수).
    #[test]
    fn sources_after_block_detects_only_lines_below_the_block() {
        let rc = "source ~/.zsh_aliases\n# iyagi BEGIN\n# iyagi END\n. ~/.zsh_aliases\n";
        // 블록은 2번 줄 — 위(1)의 source는 아니고 아래(4)의 `.`은 맞다.
        assert!(rc_sources_file_after_block(rc, "/home/u/.zsh_aliases", 2));
        assert!(!rc_sources_file_after_block(rc, "/home/u/.zsh_aliases", 4));
        // 주석은 실행되지 않는다.
        let commented = "# source ~/.zsh_aliases\n# iyagi BEGIN\n# iyagi END\n";
        assert!(!rc_sources_file_after_block(commented, "/home/u/.zsh_aliases", 2));
        // 조건부 source(`[ -f ] && source`)도 잡는다.
        let conditional = "# iyagi BEGIN\n# iyagi END\n[[ -f ~/.zsh_aliases ]] && source ~/.zsh_aliases\n";
        assert!(rc_sources_file_after_block(conditional, "/home/u/.zsh_aliases", 1));
        // 언급이 없으면 거짓.
        assert!(!rc_sources_file_after_block("", "/home/u/.zsh_aliases", 1));
    }

    /// 블록 **뒤**에서 source되는 교체 가능 파일의 충돌은 "가려짐"이 아니다 —
    /// 사용자 정의가 실제로 이기는 자리다(교체 유도가 사라지지 않게).
    #[test]
    fn a_sourced_file_below_the_block_is_not_reported_overridden() {
        let home = tempfile::tempdir().expect("home");
        let data = tempfile::tempdir().expect("data");
        std::fs::write(
            home.path().join(".zsh_aliases"),
            "alias ccd='echo user-ccd'\n",
        )
        .unwrap();
        let applied = replace_as(home.path(), data.path(), &probe()).expect("replace installs");
        assert!(applied.overriding, "rc 안 충돌이 없으니 이 시점엔 가려짐: {applied:?}");

        // 사용자가 블록 뒤에서 alias 파일을 source하기 시작한다.
        let rc = home.path().join(".zshrc");
        let mut text = std::fs::read_to_string(&rc).unwrap();
        text.push_str("source ~/.zsh_aliases\n");
        std::fs::write(&rc, text).unwrap();

        let status = status_with(
            Some(home.path()),
            data.path(),
            Some(&bin_of(data.path())),
            DEFAULT_MAIN_MODEL,
            &probe(),
            false,
        );
        assert!(
            status.conflicts.iter().any(|c| c.file.ends_with(".zsh_aliases")),
            "conflict must be visible: {:?}", status.conflicts
        );
        assert!(
            !status.overriding,
            "user definitions load after our block — not shadowed: {status:?}"
        );
    }
    /// 단위 테스트는 진짜 zsh를 띄우지 않는다(정적 스캔만).
    fn probe() -> Probe {
        Probe::zsh_static()
    }

    type Applied = Result<ShellProfilesStatus, ShellProfilesError>;

    /// 테스트가 쓰는 가짜 데몬 경로(존재 여부는 보지 않는다).
    fn bin_of(data: &Path) -> PathBuf {
        data.join("iyagi-termd")
    }

    fn apply_as(home: &Path, data: &Path, model: &str, probe: &Probe) -> Applied {
        apply_with(home, data, Some(&bin_of(data)), model, false, probe)
    }

    fn replace_as(home: &Path, data: &Path, probe: &Probe) -> Applied {
        apply_with(home, data, Some(&bin_of(data)), DEFAULT_MAIN_MODEL, true, probe)
    }

    fn remove_as(home: &Path, data: &Path, probe: &Probe) -> Applied {
        remove_with(home, data, Some(&bin_of(data)), DEFAULT_MAIN_MODEL, probe)
    }

    #[test]
    fn upsert_replaces_in_place_and_strip_restores_the_original() {
        let script = "/home/u/.local/share/Iyagi/config/shell/claude-profiles.zsh";
        let block = render_block(script);
        let rc = "export PATH=/usr/bin\nalias ll='ls -al'\n";

        let once = upsert_block(rc, &block);
        assert!(once.starts_with(rc), "user lines must stay on top: {once}");
        assert!(once.contains(BLOCK_BEGIN) && once.contains(BLOCK_END));
        assert!(once.contains(&format!("[ -r '{script}' ] && source '{script}'")));
        // 같은 블록으로 다시 부르면 글자 하나 바뀌지 않는다.
        assert_eq!(once, upsert_block(&once, &block));

        // 경로가 바뀐 블록은 제자리에서 교체된다(두 벌이 쌓이지 않는다).
        let moved = render_block("/other/path/claude-profiles.zsh");
        let replaced = upsert_block(&once, &moved);
        assert_eq!(replaced.matches(BLOCK_BEGIN).count(), 1);
        assert!(replaced.contains("/other/path/claude-profiles.zsh"));
        assert!(!replaced.contains(script));
        assert!(replaced.starts_with(rc));

        // 제거는 구분 빈 줄까지 걷어내 설치 전 원문으로 되돌린다.
        let (stripped, changed) = strip_block(&replaced);
        assert!(changed);
        assert_eq!(stripped, rc);
        let (again, changed_again) = strip_block(&stripped);
        assert!(!changed_again, "strip must be idempotent");
        assert_eq!(again, rc);
    }

    #[test]
    fn upsert_into_an_empty_rc_writes_only_the_block() {
        let block = render_block("/x/claude-profiles.zsh");
        assert_eq!(upsert_block("", &block), block);
        assert_eq!(upsert_block("\n\n", &block), block);
        // 줄바꿈으로 끝나지 않는 rc도 안전하게 이어 붙인다.
        let joined = upsert_block("alias ll='ls'", &block);
        assert!(joined.starts_with("alias ll='ls'\n\n"));
        assert!(joined.ends_with(&block));
    }

    /// 끝 마커 없이 남은 시작 마커 찌꺼기가 그 아래 사용자 설정을 삼키면 안 된다.
    #[test]
    fn a_stray_begin_marker_does_not_swallow_user_lines() {
        let block = render_block("/x/claude-profiles.zsh");
        let rc = format!("{BLOCK_BEGIN}\nexport KEEP=1\n{block}");
        let range = find_block(&rc).expect("the complete block is found");
        assert_eq!(&rc[range], block);
        let (stripped, changed) = strip_block(&rc);
        assert!(changed);
        assert!(stripped.contains("export KEEP=1"));
    }

    #[test]
    fn render_script_quotes_paths_with_spaces_and_escapes_dollars() {
        // 설치본처럼 공백이 든 경로 + 셸이 확장했을 `$`.
        let text = render_script("/opt/Iyagi $T/iyagi-termd", "/My Dir", "glm-5.3-flash[1m]");
        // 겹따옴표 안의 `$`는 이스케이프되고, 경로의 공백은 그대로 살아 있다.
        assert!(text.contains("\"${IYAGI_DAEMON_BIN:-/opt/Iyagi \\$T/iyagi-termd}\""));
        assert!(text.contains(
            "--data-dir '/My Dir' claude-exec --provider anthropic -- --dangerously-skip-permissions \"$@\"; }"
        ));
        assert!(text.contains(
            "--provider zai --main-model 'glm-5.3-flash[1m]' -- --dangerously-skip-permissions \"$@\"; }"
        ));
        assert!(text.ends_with('\n'));
        // 홑따옴표가 든 경로도 POSIX 인용 규칙을 지킨다.
        assert!(render_script("/x", "/it's", DEFAULT_MAIN_MODEL).contains("'/it'\\''s'"));
        // 백슬래시와 백틱도 셸 해석에서 빠져나온다.
        assert!(render_script("/a\\b`c", "/d", DEFAULT_MAIN_MODEL).contains("/a\\\\b\\`c"));
    }

    #[test]
    fn conflict_scan_sees_other_definitions_and_ignores_our_own_block() {
        let home = tempfile::TempDir::new().unwrap();
        // 마커 안에 "충돌처럼 보이는" 줄을 일부러 넣는다 — 우리 것이므로 무시.
        let ours = format!("{BLOCK_BEGIN}\nccd() {{ mine; }}\nccg() {{ mine; }}\n{BLOCK_END}\n");
        let write = |name: &str, body: &str| std::fs::write(home.path().join(name), body).unwrap();
        write(".zshrc", &format!("alias ccg='cc --glm'\n{ours}"));
        write(".zsh_aliases", "function ccd {\n  echo aliased\n}\n");
        write(".zprofile", "# ccd() disabled\nalias ccdx='nope'\nfunction ccdx { :; }\n");

        let found = find_conflicts(home.path());
        assert!(
            found.iter().any(|c| c.name == "ccg" && c.file.ends_with(".zshrc") && c.line == 1),
            "{found:?}"
        );
        assert_eq!(
            found.iter().filter(|c| c.file.ends_with(".zshrc")).count(),
            1,
            "lines inside our markers are not conflicts: {found:?}"
        );
        assert!(
            found
                .iter()
                .any(|c| c.name == "ccd" && c.file.ends_with(".zsh_aliases") && c.line == 1),
            "{found:?}"
        );
        // 주석과 비슷한 이름(`ccdx`)은 건드리지 않는다.
        assert!(!found.iter().any(|c| c.file.ends_with(".zprofile")), "{found:?}");
    }

    #[test]
    fn whence_output_only_reports_real_bindings() {
        let found = parse_whence("ccd: none\nccg: alias\nother: function\nccd:function\n");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "ccg");
        assert_eq!(found[0].file, DYNAMIC_SOURCE);
        assert_eq!(found[0].line, 0);
        assert_eq!(found[0].text, "ccg: alias");
        assert_eq!(found[1].name, "ccd");
        assert!(parse_whence("ccd: none\nccg: none\n").is_empty());
        assert!(parse_whence("zsh: command not found").is_empty());
    }

    /// 캐시가 신선하면(`DYNAMIC_PROBE_TTL` 안) `force=false` 호출은 다시
    /// 탐지하지 않고 마지막 결과를 그대로 돌려준다. 값을 직접 채워 넣고
    /// 확인한다 — 진짜 zsh는 띄우지 않는다(단위 테스트 규칙).
    #[test]
    fn dynamic_probe_cache_serves_recent_results_without_reprobing() {
        let cached = vec![Conflict {
            name: "ccd".to_string(),
            file: DYNAMIC_SOURCE.to_string(),
            line: 0,
            text: "ccd: function".to_string(),
            replaceable: true,
        }];
        *DYNAMIC_PROBE_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some((Instant::now(), cached.clone()));

        let served = probe_dynamic_conflicts_cached(false);
        assert_eq!(served, cached, "a fresh cache entry must be served as-is");
    }

    #[test]
    fn login_shell_basename_decides_support_and_absent_means_zsh() {
        assert!(shell_is_zsh(Some("/bin/zsh")));
        assert!(shell_is_zsh(Some("/usr/local/bin/zsh")));
        // GUI 실행에서 `$SHELL`이 비어 오는 경우: macOS 기본이 zsh다.
        assert!(shell_is_zsh(None));
        assert!(!shell_is_zsh(Some("/bin/bash")));
        assert!(!shell_is_zsh(Some("/usr/bin/fish")));
    }

    #[test]
    fn only_contract_models_are_accepted() {
        assert!(is_known_main_model(DEFAULT_MAIN_MODEL));
        assert!(is_known_main_model("glm-5.3-flash[1m]"));
        assert!(!is_known_main_model("glm-5.3"));
        assert!(!is_known_main_model(""));
        assert_eq!(sanitize_main_model(None), DEFAULT_MAIN_MODEL);
        assert_eq!(sanitize_main_model(Some("nope".into())), DEFAULT_MAIN_MODEL);
        assert_eq!(sanitize_main_model(Some("glm-5.3-flash[1m]".into())), "glm-5.3-flash[1m]");
    }

    #[cfg(unix)]
    #[test]
    fn status_without_an_rc_proposes_the_block_and_writes_nothing() {
        let home = tempfile::TempDir::new().unwrap();
        let data = tempfile::TempDir::new().unwrap();
        let binary = data.path().join("iyagi-termd");

        let st = status_with(
            Some(home.path()),
            data.path(),
            Some(&binary),
            DEFAULT_MAIN_MODEL,
            &probe(),
            false,
        );
        assert!(st.supported, "reason: {:?}", st.reason);
        assert_eq!(st.reason, None);
        assert!(!st.installed);
        assert!(!st.up_to_date);
        assert!(!st.rc_exists);
        assert!(!st.script_exists);
        assert!(st.rc_path.ends_with(".zshrc"));
        assert_eq!(st.proposed_block.as_deref(), Some(render_block(&st.script_path).as_str()));
        assert!(st.script_preview.contains("claude-exec --provider zai"));
        assert!(st.conflicts.is_empty());
        assert_eq!(st.main_model, DEFAULT_MAIN_MODEL);
        let expected_binary = binary.display().to_string();
        assert_eq!(st.daemon_binary.as_deref(), Some(expected_binary.as_str()));
        // 조회는 아무것도 만들지 않는다.
        assert!(!home.path().join(".zshrc").exists());
        assert!(!data.path().join("config").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_environments_are_named_not_guessed() {
        let home = tempfile::TempDir::new().unwrap();
        let data = tempfile::TempDir::new().unwrap();
        let binary = data.path().join("iyagi-termd");

        let bash = Probe {
            shell: Some("/bin/bash".to_string()),
            allow_dynamic: false,
        };
        let st = status_with(
            Some(home.path()),
            data.path(),
            Some(&binary),
            DEFAULT_MAIN_MODEL,
            &bash,
            false,
        );
        assert!(!st.supported);
        assert_eq!(st.reason, Some("shell_not_zsh"));
        assert_eq!(st.shell.as_deref(), Some("/bin/bash"));

        let st =
            status_with(Some(home.path()), data.path(), None, DEFAULT_MAIN_MODEL, &probe(), false);
        assert_eq!(st.reason, Some("daemon_binary_missing"));
        assert!(st.daemon_binary.is_none());
        // 미리보기는 그래도 보여 준다(PATH 폴백 이름).
        assert!(st.script_preview.contains("iyagi-termd"));

        let st =
            status_with(None, data.path(), Some(&binary), DEFAULT_MAIN_MODEL, &probe(), false);
        assert_eq!(st.reason, Some("no_home"));
        assert!(st.rc_path.is_empty());
        assert!(!st.rc_exists);
    }

    #[cfg(unix)]
    #[test]
    fn apply_then_remove_roundtrips_rc_and_script() {
        let home = tempfile::TempDir::new().unwrap();
        let data = tempfile::TempDir::new().unwrap();
        let rc = home.path().join(".zshrc");
        std::fs::write(&rc, "export EDITOR=vim\n").unwrap();

        let applied = apply_as(home.path(), data.path(), DEFAULT_MAIN_MODEL, &probe()).unwrap();
        assert!(applied.installed && applied.up_to_date);
        assert!(applied.proposed_block.is_none());
        assert!(applied.script_exists);

        let rc_text = std::fs::read_to_string(&rc).unwrap();
        assert!(rc_text.starts_with("export EDITOR=vim\n"));
        assert!(rc_text.contains(BLOCK_BEGIN) && rc_text.contains(BLOCK_END));
        // 원본 백업이 남는다(hooks_json 계약).
        assert!(home.path().join(".zshrc.iyagi.bak").exists());

        let script = script_path(data.path());
        let script_text = std::fs::read_to_string(&script).unwrap();
        assert!(script_text.contains("ccd() {") && script_text.contains("ccg() {"));
        assert!(script_text.contains(&format!("--main-model '{DEFAULT_MAIN_MODEL}'")));
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::fs::metadata(script.parent().unwrap()).unwrap();
            assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        }

        // 재적용은 멱등: 두 파일 모두 다시 쓰이지 않는다.
        let before = std::fs::metadata(&rc).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let again = apply_as(home.path(), data.path(), DEFAULT_MAIN_MODEL, &probe()).unwrap();
        assert!(again.up_to_date);
        assert_eq!(before, std::fs::metadata(&rc).unwrap().modified().unwrap());

        // 모델을 바꾸면 스크립트만 다시 쓰이고 rc 블록은 그대로다.
        let switched = apply_as(home.path(), data.path(), "glm-5.3-flash[1m]", &probe()).unwrap();
        assert!(switched.up_to_date);
        let switched_text = std::fs::read_to_string(&script).unwrap();
        assert!(switched_text.contains("--main-model 'glm-5.3-flash[1m]'"));
        assert_eq!(before, std::fs::metadata(&rc).unwrap().modified().unwrap());

        let removed = remove_as(home.path(), data.path(), &probe()).unwrap();
        assert!(!removed.installed && !removed.up_to_date);
        assert!(!removed.script_exists);
        assert!(!script.exists());
        let restored = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(restored, "export EDITOR=vim\n", "removal must restore the rc");
        // 두 번째 제거는 아무 일도 하지 않는다.
        assert!(remove_as(home.path(), data.path(), &probe()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn apply_refuses_unknown_models_and_existing_definitions() {
        let home = tempfile::TempDir::new().unwrap();
        let data = tempfile::TempDir::new().unwrap();

        let error = apply_as(home.path(), data.path(), "gpt-5", &probe())
            .expect_err("unknown models never reach the script");
        assert_eq!(error.code(), "invalid_main_model");

        std::fs::write(home.path().join(".zshrc"), "ccd() { claude; }\n").unwrap();
        let error = apply_as(home.path(), data.path(), DEFAULT_MAIN_MODEL, &probe())
            .expect_err("an existing ccd is never overwritten");
        assert_eq!(error.code(), "shell_profiles_conflict");
        match error {
            ShellProfilesError::Conflict(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].name, "ccd");
                assert_eq!(items[0].line, 1);
            }
            other => panic!("unexpected error: {other:?}"),
        }
        // 거절은 아무 파일도 만들거나 고치지 않는다.
        assert!(!script_path(data.path()).exists());
        assert_eq!(
            std::fs::read_to_string(home.path().join(".zshrc")).unwrap(),
            "ccd() { claude; }\n"
        );

        // 지원되지 않는 셸에서도 거절한다(같은 이유 코드를 그대로 싣는다).
        std::fs::remove_file(home.path().join(".zshrc")).unwrap();
        let bash = Probe {
            shell: Some("/bin/bash".to_string()),
            allow_dynamic: false,
        };
        let error = apply_as(home.path(), data.path(), DEFAULT_MAIN_MODEL, &bash)
            .expect_err("non-zsh logins cannot install");
        assert_eq!(error.code(), "shell_profiles_unsupported");
    }

    /// 제거는 사용자가 셸을 바꾼 뒤에도(= `supported=false`) 동작해야 한다.
    #[cfg(unix)]
    #[test]
    fn remove_works_even_when_the_login_shell_changed() {
        let home = tempfile::TempDir::new().unwrap();
        let data = tempfile::TempDir::new().unwrap();
        apply_as(home.path(), data.path(), DEFAULT_MAIN_MODEL, &probe()).unwrap();

        let bash = Probe {
            shell: Some("/bin/bash".to_string()),
            allow_dynamic: false,
        };
        let removed = remove_as(home.path(), data.path(), &bash).unwrap();
        assert!(!removed.supported);
        assert!(!removed.installed);
        assert!(!script_path(data.path()).exists());
    }

    /// 교체: 사용자의 줄은 그대로 남기고, 블록을 rc 끝으로 옮겨 우리 정의가
    /// 이기게 한다. 제거하면 사용자 원래 정의만 남는다.
    #[cfg(unix)]
    #[test]
    fn replace_keeps_user_lines_and_moves_the_block_to_the_end() {
        let home = tempfile::TempDir::new().unwrap();
        let data = tempfile::TempDir::new().unwrap();
        let rc = home.path().join(".zshrc");
        let user = "function ccd() {\n  claude \"$@\"\n}\nalias ccg='cc --glm'\nexport AFTER=1\n";
        std::fs::write(&rc, user).unwrap();

        // 교체를 고르지 않으면 여전히 거절한다.
        let error = apply_as(home.path(), data.path(), DEFAULT_MAIN_MODEL, &probe()).unwrap_err();
        assert_eq!(error.code(), "shell_profiles_conflict");

        let replaced = replace_as(home.path(), data.path(), &probe()).unwrap();
        assert!(replaced.installed && replaced.up_to_date);
        assert!(replaced.overriding, "{replaced:?}");
        assert_eq!(replaced.conflicts.len(), 2);
        let text = std::fs::read_to_string(&rc).unwrap();
        assert!(
            text.starts_with(user),
            "user lines must stay untouched: {text}"
        );
        assert!(text.ends_with(&render_block(&replaced.script_path)));
        // 스크립트는 같은 이름의 alias부터 치운다. `|| true`가 없으면 별칭이
        // 없는 셸(setopt err_exit)에서 unalias의 exit 1이 스크립트를 끊는다.
        let script = std::fs::read_to_string(script_path(data.path())).unwrap();
        assert!(script.contains("unalias ccd ccg 2>/dev/null || true\nccd() {"));

        // 이미 끝에 있으면 다시 교체해도 글자 하나 바뀌지 않는다.
        let again = replace_as(home.path(), data.path(), &probe()).unwrap();
        assert!(again.overriding);
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), text);

        let removed = remove_as(home.path(), data.path(), &probe()).unwrap();
        assert!(!removed.installed && !removed.overriding);
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), user);
    }

    /// 블록 **아래**에 생긴 사용자 정의는 우리를 덮으므로 overriding이 아니고,
    /// 교체를 고르면 블록이 그 아래로 내려간다.
    #[cfg(unix)]
    #[test]
    fn a_definition_after_our_block_is_not_overridden_until_replaced() {
        let home = tempfile::TempDir::new().unwrap();
        let data = tempfile::TempDir::new().unwrap();
        let rc = home.path().join(".zshrc");
        std::fs::write(&rc, "export A=1\n").unwrap();
        apply_as(home.path(), data.path(), DEFAULT_MAIN_MODEL, &probe()).unwrap();
        let mut text = std::fs::read_to_string(&rc).unwrap();
        text.push_str("ccg() { mine; }\n");
        std::fs::write(&rc, &text).unwrap();

        let st = status_with(
            Some(home.path()),
            data.path(),
            Some(&bin_of(data.path())),
            DEFAULT_MAIN_MODEL,
            &probe(),
            false,
        );
        assert!(st.installed && !st.overriding);
        assert_eq!(st.conflicts.len(), 1);

        let replaced = replace_as(home.path(), data.path(), &probe()).unwrap();
        assert!(replaced.overriding);
        let moved = std::fs::read_to_string(&rc).unwrap();
        assert!(moved.find("ccg() { mine; }").unwrap() < moved.find(BLOCK_BEGIN).unwrap());
        assert_eq!(moved.matches(BLOCK_BEGIN).count(), 1);
    }

    /// `.zlogin`은 `.zshrc` 뒤에 읽혀 우리 블록을 다시 덮는다 — 교체도 거절.
    #[cfg(unix)]
    #[test]
    fn replace_refuses_definitions_that_load_after_the_rc() {
        let home = tempfile::TempDir::new().unwrap();
        let data = tempfile::TempDir::new().unwrap();
        std::fs::write(home.path().join(".zshrc"), "alias ccd=claude\n").unwrap();
        std::fs::write(home.path().join(".zlogin"), "ccg() { late; }\n").unwrap();

        let error = replace_as(home.path(), data.path(), &probe()).unwrap_err();
        assert_eq!(error.code(), "shell_profiles_conflict_unreplaceable");
        match error {
            ShellProfilesError::Unreplaceable(items) => {
                assert_eq!(items.len(), 1);
                assert!(items[0].file.ends_with(".zlogin"));
                assert!(!items[0].replaceable);
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(!script_path(data.path()).exists());
        assert_eq!(
            std::fs::read_to_string(home.path().join(".zshrc")).unwrap(),
            "alias ccd=claude\n"
        );
    }
}
