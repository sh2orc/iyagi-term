//! `iyagi-termd claude-exec` — 터미널 pane 밖에서 Claude Code를 띄울 때도
//! Iyagi이 쓰는 provider 라우팅을 그대로 씌우는 일회성 CLI.
//!
//! pane 런치(`claude_provider::resolve`)와 같은 표를 쓰지만 IPC도 데몬도
//! 거치지 않는다: 이 프로세스가 환경을 정리하고 **자기 자신을 Claude Code로
//! 바꾼다**(unix `exec`). 그래서 셸 함수·alias(`ccg`)가 이 명령을 감싸면
//! 사용자는 평소처럼 `claude`를 쓰면서 Z.ai Coding Plan으로 돌 수 있다.
//!
//! ```text
//! iyagi-termd [--data-dir DIR] claude-exec --provider zai|anthropic
//!            [--main-model <id>] [--program <path>] [--] <claude args...>
//! ```
//!
//! 규칙:
//!
//! * `--provider zai` — 비밀이 아닌 변수는 `claude_provider::zai_env`가
//!   만든다(pane과 한 글자도 다르지 않다). 상속받은 다른 provider 키·백엔드
//!   스위치·모델 지정은 `claude_provider::routed_env_remove`로 먼저 지운다.
//!   토큰은 데몬 데이터 루트의 로컬 암호화 저장소에서 **실행 시점에** 읽어
//!   자식 환경에만 얹는다.
//! * `--provider anthropic` — 호스트가 관리하던 변수(base URL·토큰·관리 표식)를
//!   지워 Claude Code가 원래 구독/API 키로 돌아가게 한다. 나머지는 "우리
//!   흔적일 때만" 지운다: 모델 변수는 값이 `glm`으로 시작할 때, 튜닝
//!   변수(compact window·API timeout·비필수 트래픽)는 값이 우리가 넣는 값과
//!   똑같을 때다. 사용자가 직접 고정한 값은 살아남는다.
//!
//! 비밀 취급: 토큰은 `Zeroizing`으로만 들고 다니고 자식 env에 얹은 뒤 drop에서
//! 지워진다. stdout/stderr/tracing에는 절대 싣지 않는다(디버그 한 줄에도
//! provider·모델 선택자만 담는다). 이 명령은 저널도 남기지 않는다.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use iyagi_termd_lib::claude_provider::{self, StartToken, REASON_KEY_UNREADABLE, TOKEN_ENV};
use term_contracts::launch::ZAI_CLAUDE_MAIN_MODELS;

/// `--main-model`을 주지 않았을 때의 Z.ai 주 모델(pane 기본값과 같다).
pub const DEFAULT_ZAI_MAIN_MODEL: &str = ZAI_CLAUDE_MAIN_MODELS[0];

/// `--provider anthropic`에서 조건 없이 지우는 변수: 호스트(iyagi)가
/// 넣어 둔 라우팅 흔적이다. 남아 있으면 Anthropic 구독으로 못 돌아간다.
const ANTHROPIC_ALWAYS_REMOVE: [&str; 3] = [
    "ANTHROPIC_BASE_URL",
    TOKEN_ENV,
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
];

/// `--provider anthropic`에서 **값이 Z.ai 모델일 때만** 지우는 변수. 사용자가
/// 직접 고정한 Anthropic 모델(`claude-opus-4-8` 같은)은 그대로 둔다.
const ANTHROPIC_MODEL_VARS: [&str; 4] = [
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];

/// `--provider anthropic`에서 **상속된 값이 우리가 넣는 값과 똑같을 때만**
/// 지우는 튜닝 변수와 그 값. Z.ai로 라우팅된 pane 안에서 이 명령을 부르면
/// 이 셋이 딸려 오는데, 사용자가 직접 다른 값을 넣었을 수도 있어서 값까지
/// 봐야 우리 흔적인지 알 수 있다. 값은
/// [`claude_provider::zai_env`](iyagi_termd_lib::claude_provider::zai_env)가
/// 넣는 것과 같아야 한다(`routed_tuning_values_match_what_zai_routing_injects`
/// 테스트가 어긋남을 잡는다).
const ANTHROPIC_ROUTED_TUNING: [(&str, &str); 3] = [
    claude_provider::ZAI_AUTO_COMPACT_WINDOW,
    claude_provider::ZAI_API_TIMEOUT_MS,
    ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
];

/// `--provider` 선택자. 문자열 검증은 clap과 [`plan`] 양쪽에서 한다(직접
/// 호출하는 테스트·스크립트가 clap을 안 거칠 수 있다).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    Zai,
    Anthropic,
}

impl Provider {
    fn parse(raw: &str) -> Result<Self, ExecError> {
        match raw {
            "zai" => Ok(Self::Zai),
            "anthropic" => Ok(Self::Anthropic),
            other => Err(ExecError::UnknownProvider(other.to_string())),
        }
    }
}

/// 자식 환경에 적용할 계획. **비밀은 들어 있지 않다** — 토큰은 spawn 직전에
/// 따로 얹는다(이 구조체는 로그·테스트에 그대로 찍혀도 안전하다).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ExecPlan {
    /// 덮어쓸 변수(비밀 아님).
    pub set: BTreeMap<String, String>,
    /// 상속분에서 먼저 지울 변수 이름.
    pub remove: Vec<String>,
}

/// 사용자에게 한 줄로 보여 주고 끝나는 실패들. 메시지는 고정 문구 + 사용자가
/// 준 인수뿐이라 비밀이 섞일 길이 없다.
#[derive(Debug, PartialEq, Eq)]
pub enum ExecError {
    UnknownProvider(String),
    UnsupportedMainModel(String),
    ProgramNotFound,
    KeyMissing,
    KeyUnreadable,
}

impl ExecError {
    /// stderr 한 줄. 키 관련 문구는 앱 UI의 토스트와 같은 안내(Settings →
    /// Z.ai Coding Plan)를 셸에서도 그대로 준다.
    pub fn message(&self) -> String {
        match self {
            Self::UnknownProvider(value) => {
                format!("claude-exec: unknown --provider `{value}` (expected `zai` or `anthropic`)")
            }
            Self::UnsupportedMainModel(value) => format!(
                "claude-exec: unsupported --main-model `{value}` (expected one of {})",
                ZAI_CLAUDE_MAIN_MODELS.join(", ")
            ),
            Self::ProgramNotFound => "claude-exec: `claude` was not found on PATH".to_string(),
            Self::KeyMissing => "ccg: Z.ai Coding Plan API key is not registered. Open iyagi → Settings → Z.ai Coding Plan and save your key.".to_string(),
            Self::KeyUnreadable => "ccg: the iyagi key store could not be read (Settings → Z.ai Coding Plan).".to_string(),
        }
    }

    /// 종료 코드: 프로그램을 못 찾으면 셸 관례대로 127, 나머지 사용법/설정
    /// 오류는 2다(실행은 했는데 실패한 경우만 126을 쓴다).
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::ProgramNotFound => 127,
            _ => 2,
        }
    }
}

/// provider 선택자를 자식 환경 계획으로 바꾼다. 디스크도 프로세스도 건드리지
/// 않는 순수 함수라 테스트가 전부 여기서 끝난다 — 토큰 읽기는 [`run`]이 한다.
///
/// `current_env`는 이 프로세스가 물려받은 환경(보통 `std::env::vars()`)이다.
/// `--provider anthropic`이 "값이 Z.ai 모델인 변수만" 지우려면 값을 봐야 한다.
pub fn plan(
    provider: &str,
    main_model: Option<&str>,
    current_env: impl Iterator<Item = (String, String)>,
) -> Result<ExecPlan, ExecError> {
    match Provider::parse(provider)? {
        Provider::Zai => {
            let main_model = main_model.unwrap_or(DEFAULT_ZAI_MAIN_MODEL);
            if !ZAI_CLAUDE_MAIN_MODELS.contains(&main_model) {
                return Err(ExecError::UnsupportedMainModel(main_model.to_string()));
            }
            Ok(ExecPlan {
                set: claude_provider::zai_env(main_model),
                remove: claude_provider::routed_env_remove(),
            })
        }
        // Anthropic으로 돌아갈 때는 아무것도 얹지 않는다: Claude Code가
        // 자기 설정(`~/.claude`)과 구독 자격증명을 그대로 쓰게 둔다.
        Provider::Anthropic => Ok(ExecPlan {
            set: BTreeMap::new(),
            remove: anthropic_remove(current_env),
        }),
    }
}

/// `--provider anthropic`의 `env_remove` 목록. 항상 지우는 것들이 먼저 오고,
/// "우리가 넣은 흔적으로 보이는" 것들이 이름 순으로 뒤에 붙는다(순서 고정 —
/// 로그·테스트가 안정적이다). 흔적 판정은 둘이다:
///
/// * 모델 변수([`ANTHROPIC_MODEL_VARS`]): 값이 `glm*`이면 Z.ai 고정이다.
/// * 튜닝 변수([`ANTHROPIC_ROUTED_TUNING`]): 값이 우리가 넣는 값과 **똑같을
///   때만** 우리 것으로 본다. 사용자가 직접 정한 값은 그대로 둔다.
fn anthropic_remove(current_env: impl Iterator<Item = (String, String)>) -> Vec<String> {
    let mut routed: BTreeSet<&'static str> = BTreeSet::new();
    for (name, value) in current_env {
        // Windows 환경 변수는 대소문자를 가리지 않는다 — 이름은 느슨하게
        // 맞추고 지우는 것은 항상 표준 대문자 이름이다.
        if let Some(var) = ANTHROPIC_MODEL_VARS
            .iter()
            .find(|candidate| candidate.eq_ignore_ascii_case(&name))
        {
            if is_zai_model(&value) {
                routed.insert(*var);
            }
            continue;
        }
        if let Some((var, injected)) = ANTHROPIC_ROUTED_TUNING
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(&name))
        {
            if value.as_str() == *injected {
                routed.insert(*var);
            }
        }
    }
    ANTHROPIC_ALWAYS_REMOVE
        .iter()
        .copied()
        .chain(routed)
        .map(str::to_string)
        .collect()
}

/// 값이 Z.ai 모델 id인가(대소문자 무시 `glm` 접두사).
fn is_zai_model(value: &str) -> bool {
    value
        .as_bytes()
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"glm"))
}

/// PATH에서 찾는 실행 파일 이름. npm shim(`claude.cmd`)은 Windows에서만
/// 의미가 있다.
fn program_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["claude.exe", "claude.cmd"]
    } else {
        &["claude"]
    }
}

/// 실행할 Claude Code. `--program`이 있으면 그대로 믿고(존재 확인은 exec에
/// 맡긴다), 없으면 PATH를 앞에서부터 훑는다.
fn find_claude(program: Option<PathBuf>) -> Result<PathBuf, ExecError> {
    if let Some(program) = program {
        return Ok(program);
    }
    let path = std::env::var_os("PATH").ok_or(ExecError::ProgramNotFound)?;
    let self_exe = std::env::current_exe().ok().map(canonical);
    search_path(&path, self_exe.as_deref()).ok_or(ExecError::ProgramNotFound)
}

/// PATH 항목을 순서대로 보며 첫 실행 후보를 고른다. `self_exe`(우리 자신의
/// 정규화된 경로)와 같은 파일은 건너뛴다 — `claude`라는 이름으로 이 데몬이
/// 링크돼 있으면 자기 자신을 무한히 exec하게 된다.
fn search_path(path: &OsStr, self_exe: Option<&Path>) -> Option<PathBuf> {
    std::env::split_paths(path).find_map(|dir| {
        program_names()
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| is_other_program_file(candidate, self_exe))
    })
}

/// 플랫폼이 실제로 실행할 수 있는 파일이고(디렉터리·없는 경로·실행 비트
/// 없는 파일은 탈락) 우리 자신이 아니다.
fn is_other_program_file(candidate: &Path, self_exe: Option<&Path>) -> bool {
    // 심볼릭 링크는 따라간다(패키지 매니저 설치본이 보통 링크다).
    let Ok(metadata) = std::fs::metadata(candidate) else {
        return false;
    };
    if !metadata.is_file() || !is_executable(&metadata) {
        return false;
    }
    match self_exe {
        Some(self_exe) => canonical(candidate.to_path_buf()).as_path() != self_exe,
        None => true,
    }
}

/// unix: 실행 비트가 하나라도 있어야 한다
/// (`agent_runtime::detection::is_executable_file`과 같은 규칙). PATH에 놓인
/// 동명의 문서·설정 파일을 exec해서 126으로 죽는 대신 다음 후보로 넘어간다.
#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

/// Windows: 실행 여부는 확장자가 정한다 — 후보 이름이 이미 `.exe`/`.cmd`다.
#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// 심볼릭 링크·`..`를 해소한 경로. 실패하면(권한·경합) 원본을 그대로 쓴다.
fn canonical(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// 서브커맨드 본체. 성공하면 **돌아오지 않는다**(unix: 이 프로세스가 Claude
/// Code로 바뀐다). 돌아온 값은 그대로 프로세스 종료 코드다.
pub fn run(
    data_dir: &Path,
    provider: &str,
    main_model: Option<&str>,
    program: Option<PathBuf>,
    args: &[String],
) -> i32 {
    // (1) 인수만으로 끝나는 검증. 디스크를 건드리기 전에 끝낸다. 환경은
    // `vars_os`로 읽어 UTF-8이 아닌 변수 하나가 이 명령을 패닉시키지 않게
    // 한다(우리가 보는 값은 전부 ASCII 모델 id다).
    let current_env = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)));
    let planned = match plan(provider, main_model, current_env) {
        Ok(planned) => planned,
        Err(error) => return fail(&error),
    };

    // (2) 프로그램. 키보다 먼저 본다(`claude_provider::resolve`와 같은 순서):
    // 띄울 게 없으면 저장소를 열 이유도 없다.
    let program = match find_claude(program) {
        Ok(program) => program,
        Err(error) => return fail(&error),
    };

    // (3) 토큰. Z.ai 라우팅일 때만, 실행 직전에 읽는다. OS 오류 내용은 싣지
    // 않고 reason code를 사용자 안내 문구로만 바꾼다.
    let token = match Provider::parse(provider) {
        Ok(Provider::Zai) => match StartToken::read(data_dir) {
            Ok(start) => Some(start.token),
            Err(error) => {
                let unreadable =
                    claude_provider::reason_code(&error) == Some(REASON_KEY_UNREADABLE);
                let error = if unreadable {
                    ExecError::KeyUnreadable
                } else {
                    ExecError::KeyMissing
                };
                return fail(&error);
            }
        },
        // provider 문자열은 (1)에서 이미 통과했다.
        _ => None,
    };

    // 선택자만 남긴다 — 토큰은 물론이고 실행 인수(프롬프트가 섞일 수 있다)도
    // 찍지 않는다. 기본 필터가 info라 평소엔 아무것도 나오지 않는다.
    tracing::debug!(
        provider = %provider,
        main_model = ?main_model,
        "claude-exec: handing off to Claude Code"
    );
    handoff(
        build_command(&program, args, &planned, token.as_ref().map(|t| t.as_str())),
        &program,
    )
}

/// 한 줄 오류를 stderr에 찍고 종료 코드를 돌려준다.
fn fail(error: &ExecError) -> i32 {
    eprintln!("{}", error.message());
    error.exit_code()
}

/// 계획대로 자식 명령을 조립한다. 지우기가 먼저, 얹기가 나중이고 토큰은 맨
/// 마지막이다. stdio는 그대로 물려준다(TTY가 유지돼야 Claude Code가 UI를
/// 그린다).
fn build_command(
    program: &Path,
    args: &[String],
    planned: &ExecPlan,
    token: Option<&str>,
) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    for name in &planned.remove {
        command.env_remove(name);
    }
    for (name, value) in &planned.set {
        command.env(name, value);
    }
    if let Some(token) = token {
        command.env(TOKEN_ENV, token);
    }
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

/// unix: 이 프로세스를 Claude Code로 대체한다. pid·프로세스 그룹·터미널
/// 제어권·시그널이 그대로 이어져 셸에는 중간 프로세스가 보이지 않는다.
/// `exec`는 성공하면 돌아오지 않는다.
#[cfg(unix)]
fn handoff(mut command: Command, program: &Path) -> i32 {
    use std::os::unix::process::CommandExt;
    let error = command.exec();
    eprintln!(
        "claude-exec: could not execute {}: {error}",
        program.display()
    );
    126
}

/// Windows: `exec`가 없으니 자식을 띄우고 기다린 뒤 종료 코드를 물려받는다.
/// 시그널로 끝난 경우(코드 없음)는 1로 본다.
#[cfg(not(unix))]
fn handoff(mut command: Command, program: &Path) -> i32 {
    match command.spawn().and_then(|mut child| child.wait()) {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!(
                "claude-exec: could not execute {}: {error}",
                program.display()
            );
            126
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iyagi_termd_lib::claude_provider::ENV_REMOVE;

    /// `plan`에 넘길 가짜 상속 환경.
    fn env(pairs: &[(&str, &str)]) -> std::vec::IntoIter<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[cfg(unix)]
    fn write_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write fake claude");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake claude");
    }

    #[cfg(not(unix))]
    fn write_executable(path: &Path) {
        std::fs::write(path, b"fake claude").expect("write fake claude");
    }

    /// Z.ai 계획은 pane 런치와 같은 표(8개)를 얹고 상속분을 전부 지운다.
    #[test]
    fn zai_plan_sets_the_provider_table_and_clears_inherited_overrides() {
        let planned =
            plan("zai", None, env(&[("ANTHROPIC_API_KEY", "sk-user")])).expect("default zai plan");

        assert_eq!(
            planned.set,
            claude_provider::zai_env(DEFAULT_ZAI_MAIN_MODEL)
        );
        assert_eq!(planned.set.len(), 8);
        assert_eq!(DEFAULT_ZAI_MAIN_MODEL, "glm-5.3[1m]");
        assert_eq!(
            planned
                .set
                .get("ANTHROPIC_DEFAULT_OPUS_MODEL")
                .map(String::as_str),
            Some("glm-5.3[1m]")
        );
        assert_eq!(
            planned
                .set
                .get("ANTHROPIC_DEFAULT_SONNET_MODEL")
                .map(String::as_str),
            Some("glm-5.3[1m]")
        );
        // 토큰은 계획에 없다 — `run`이 실행 직전에만 얹는다.
        assert!(!planned.set.contains_key(TOKEN_ENV));

        let removed: Vec<&str> = planned.remove.iter().map(String::as_str).collect();
        assert_eq!(removed, ENV_REMOVE.to_vec());
        // 사용자의 `~/.claude`는 건드리지 않는다.
        assert!(!removed.contains(&"CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn zai_plan_accepts_every_supported_main_model_and_rejects_anything_else() {
        for model in ZAI_CLAUDE_MAIN_MODELS {
            let planned = plan("zai", Some(model), env(&[])).expect("supported model");
            assert_eq!(
                planned
                    .set
                    .get("ANTHROPIC_DEFAULT_OPUS_MODEL")
                    .map(String::as_str),
                Some(model)
            );
            // haiku 슬롯은 주 모델과 무관하게 고정이다.
            assert_eq!(
                planned
                    .set
                    .get("ANTHROPIC_DEFAULT_HAIKU_MODEL")
                    .map(String::as_str),
                Some(term_contracts::launch::ZAI_CLAUDE_HAIKU_MODEL)
            );
        }

        for rejected in ["glm-4.6", "claude-opus-4-8", "GLM-5.3[1m]", ""] {
            let error = plan("zai", Some(rejected), env(&[])).unwrap_err();
            assert_eq!(
                error,
                ExecError::UnsupportedMainModel(rejected.to_string()),
                "{rejected}"
            );
            assert_eq!(error.exit_code(), 2);
            assert!(
                error
                    .message()
                    .starts_with("claude-exec: unsupported --main-model"),
                "{}",
                error.message()
            );
            assert!(
                error.message().contains("glm-5.3-flash[1m]"),
                "lists the valid ids"
            );
        }
    }

    /// Anthropic으로 돌아갈 때: 호스트가 넣은 변수와 Z.ai 모델 고정만 지우고
    /// 사용자가 직접 고정한 Anthropic 모델은 살린다.
    #[test]
    fn anthropic_plan_strips_zai_pins_but_keeps_a_real_anthropic_model() {
        let planned = plan(
            "anthropic",
            None,
            env(&[
                ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
                ("ANTHROPIC_AUTH_TOKEN", "zai-token"),
                ("ANTHROPIC_DEFAULT_OPUS_MODEL", "GLM-5.3[1m]"),
                ("ANTHROPIC_DEFAULT_SONNET_MODEL", "claude-opus-4-8"),
                ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "glm-5.3-flash[1m]"),
                ("ANTHROPIC_MODEL", "claude-opus-4-8"),
                ("API_TIMEOUT_MS", "3000000"),
                ("PATH", "/usr/bin"),
            ]),
        )
        .expect("anthropic plan");

        assert!(planned.set.is_empty(), "no provider variables are added");
        // 항상 지우는 셋이 먼저, 흔적으로 잡힌 것들이 이름 순으로 뒤에.
        let removed: Vec<&str> = planned.remove.iter().map(String::as_str).collect();
        assert_eq!(
            removed,
            vec![
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                "API_TIMEOUT_MS",
            ]
        );
        assert!(!removed.contains(&"ANTHROPIC_DEFAULT_SONNET_MODEL"));
        assert!(!removed.contains(&"ANTHROPIC_MODEL"));
    }

    /// 튜닝 변수는 값이 우리가 넣는 값과 같을 때만 지운다 — Z.ai pane 안에서
    /// 부른 `ccg --provider anthropic`은 깨끗해지고, 사용자가 직접 정한 값은
    /// 살아남는다.
    #[test]
    fn anthropic_plan_removes_our_tuning_values_but_keeps_user_pins() {
        let ours = plan(
            "anthropic",
            None,
            env(&[
                ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "1000000"),
                ("API_TIMEOUT_MS", "3000000"),
                ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
            ]),
        )
        .expect("anthropic plan");
        let removed: Vec<&str> = ours.remove.iter().map(String::as_str).collect();
        assert_eq!(
            removed,
            vec![
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
                "API_TIMEOUT_MS",
                "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            ]
        );

        let theirs = plan(
            "anthropic",
            None,
            env(&[
                ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "200000"),
                ("API_TIMEOUT_MS", "60000"),
                ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "0"),
            ]),
        )
        .expect("anthropic plan");
        let removed: Vec<&str> = theirs.remove.iter().map(String::as_str).collect();
        assert_eq!(removed, ANTHROPIC_ALWAYS_REMOVE.to_vec());
    }

    /// 튜닝 표의 값은 `zai_env`가 실제로 넣는 값과 같아야 한다 — 한쪽만 바뀌면
    /// `--provider anthropic`이 우리 흔적을 못 알아보고 남긴다.
    #[test]
    fn routed_tuning_values_match_what_zai_routing_injects() {
        let injected = claude_provider::zai_env(DEFAULT_ZAI_MAIN_MODEL);
        for (name, value) in ANTHROPIC_ROUTED_TUNING {
            assert_eq!(
                injected.get(name).map(String::as_str),
                Some(value),
                "{name}"
            );
        }
    }

    #[test]
    fn anthropic_plan_always_clears_the_host_managed_variables_and_ignores_main_model() {
        let planned = plan("anthropic", Some("glm-5.3[1m]"), env(&[])).expect("anthropic plan");
        let removed: Vec<&str> = planned.remove.iter().map(String::as_str).collect();
        assert_eq!(removed, ANTHROPIC_ALWAYS_REMOVE.to_vec());
        assert!(planned.set.is_empty());
    }

    #[test]
    fn unknown_provider_is_rejected() {
        let error = plan("bedrock", None, env(&[])).unwrap_err();
        assert_eq!(error, ExecError::UnknownProvider("bedrock".to_string()));
        assert_eq!(error.exit_code(), 2);
        assert!(error.message().contains("bedrock"));
    }

    #[test]
    fn error_messages_and_exit_codes_match_the_documented_contract() {
        assert_eq!(ExecError::ProgramNotFound.exit_code(), 127);
        assert_eq!(
            ExecError::ProgramNotFound.message(),
            "claude-exec: `claude` was not found on PATH"
        );
        assert_eq!(ExecError::KeyMissing.exit_code(), 2);
        assert!(ExecError::KeyMissing
            .message()
            .starts_with("ccg: Z.ai Coding Plan API key is not registered."));
        assert_eq!(ExecError::KeyUnreadable.exit_code(), 2);
        assert!(ExecError::KeyUnreadable
            .message()
            .contains("key store could not be read"));
    }

    #[test]
    fn explicit_program_wins_without_touching_path() {
        let program = PathBuf::from("/opt/claude/bin/claude");
        assert_eq!(
            find_claude(Some(program.clone())).expect("verbatim"),
            program
        );
    }

    /// PATH 탐색: 이름만 같은 디렉터리·실행 비트 없는 파일·우리 자신은
    /// 건너뛰고 첫 진짜 실행 파일을 고른다. 프로세스는 띄우지 않는다.
    #[test]
    fn path_search_skips_directories_and_our_own_executable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let name = program_names()[0];
        let mut dirs: Vec<PathBuf> = Vec::new();

        // (1) 이름은 맞지만 디렉터리다.
        let decoy_dir = tmp.path().join("decoy");
        std::fs::create_dir_all(decoy_dir.join(name)).expect("decoy directory");
        dirs.push(decoy_dir);

        // (2) unix 전용: 이름은 맞지만 실행 비트가 없다(0o644). Windows는
        // 실행 여부를 확장자가 정하므로 이 후보를 두지 않는다.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let noexec_dir = tmp.path().join("noexec");
            std::fs::create_dir_all(&noexec_dir).expect("noexec dir");
            let noexec = noexec_dir.join(name);
            std::fs::write(&noexec, b"# not a program\n").expect("write noexec decoy");
            std::fs::set_permissions(&noexec, std::fs::Permissions::from_mode(0o644))
                .expect("chmod 0o644");
            dirs.push(noexec_dir);
        }

        // (3) `claude`라는 이름으로 놓인 우리 자신.
        let self_dir = tmp.path().join("self");
        std::fs::create_dir_all(&self_dir).expect("self dir");
        let self_exe = self_dir.join(name);
        write_executable(&self_exe);
        dirs.push(self_dir);

        // (4) 진짜 Claude Code.
        let real_dir = tmp.path().join("real");
        std::fs::create_dir_all(&real_dir).expect("real dir");
        let real = real_dir.join(name);
        write_executable(&real);
        dirs.push(real_dir);

        let path = std::env::join_paths(&dirs).expect("PATH");
        let canonical_self = std::fs::canonicalize(&self_exe).expect("canonical self");
        let found =
            search_path(&path, Some(canonical_self.as_path())).expect("finds the real claude");
        assert_eq!(
            std::fs::canonicalize(&found).expect("canonical found"),
            std::fs::canonicalize(&real).expect("canonical real")
        );

        // 우리 자신을 모르면 첫 번째 실행 파일이 그대로 잡힌다(보통의 설치).
        assert_eq!(search_path(&path, None).expect("first file"), self_exe);

        // 후보가 없으면 None → `claude-exec`는 127로 끝난다.
        let empty = std::env::join_paths([tmp.path().join("nowhere")]).expect("PATH");
        assert!(search_path(&empty, None).is_none());
    }

    #[test]
    fn zai_model_values_are_detected_case_insensitively() {
        for value in ["glm-5.3[1m]", "GLM-5.3-flash[1m]", "Glm"] {
            assert!(is_zai_model(value), "{value}");
        }
        for value in ["claude-opus-4-8", "", "gl", "my-glm-model"] {
            assert!(!is_zai_model(value), "{value}");
        }
    }
}
