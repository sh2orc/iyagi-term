//! `LaunchRequest.claude_provider` 해석 — Claude Code pane을 Z.ai Coding Plan
//! (Anthropic 호환 엔드포인트)으로 라우팅한다.
//!
//! 요청은 **선택자**만 나른다(어느 provider, 어느 주 모델). 자격증명은 이
//! 데몬이 자기 데이터 루트의 로컬 암호화 저장소(`term_secrets`, Settings →
//! Z.ai Coding Plan이 쓰는 그 파일)에서 실행 시점에 읽는다. 그래서:
//!
//! * 요청 자체는 절대 바꾸지 않는다 — fingerprint·DB·title에 토큰이 들어갈
//!   길이 없다. 해석 결과는 [`ResolvedClaudeProvider`]로 따로 나른다.
//! * 토큰의 수명은 짧다. 셸 모드: [`ResolvedClaudeProvider::apply`]가 자식
//!   env 맵에 얹고 `PtyHandle::spawn`이 돌아오면 [`zeroize_env`]로 그 맵을
//!   지운다. 관리 모드: descriptor(`WorkloadEntry`, 대기열에 남을 수 있다)에는
//!   토큰을 넣지 않고 [`StartToken`]이 **시작 시점**에 다시 읽어 gate
//!   프레임에만 얹는다 — 대기 중 Settings에서 키가 지워지면 `zai_key_missing`
//!   으로 실패하고, 바뀌면 새 키로 시작한다. 이 모듈이 쥐는 사본은 전부
//!   `Zeroizing`(drop 시 지워진다)이며, 자식 프로세스의 환경과 직렬화된 gate
//!   프레임 바이트에 남는 사본만 우리가 지울 수 없다.
//! * 로그에는 reason code만 남기고, 이 모듈의 어떤 타입도 Debug로 토큰을
//!   찍지 않는다(수동 `Debug`는 provider만 출력).
//! * 실패는 workload 행이 생기기 전에 `InvalidArgument` + `details.reason_code`
//!   로 돌려준다(UI가 토스트 문구로 매핑한다).
//!
//! 건드리지 않는 것: `CLAUDE_CONFIG_DIR`/`HOME`/`XDG_*`/`TMPDIR`(사용자의
//! `~/.claude`가 그대로 남아 hook·statusLine·세션·resume이 계속 동작한다),
//! `ANTHROPIC_API_KEY`(설정하지 않고 상속분은 지운다), `--model` argv.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use term_contracts::error::ErrorCode;
use term_contracts::launch::{ClaudeProvider, LaunchRequest, ZAI_CLAUDE_HAIKU_MODEL};
use term_contracts::RpcError;
use zeroize::{Zeroize, Zeroizing};

use crate::connections::{SecretRedactor, CLAUDE_ZAI_BASE_URL};

/// `details.reason_code` 값들. UI(`sessionController`/`managedRunErrors`)가
/// 같은 문자열로 토스트를 고르므로 바꾸면 양쪽을 함께 바꿔야 한다.
pub const REASON_KEY_MISSING: &str = "zai_key_missing";
pub const REASON_KEY_UNREADABLE: &str = "zai_key_unreadable";
pub const REASON_PROGRAM_MISMATCH: &str = "claude_provider_program_mismatch";
pub const REASON_ENV_CONFLICT: &str = "claude_provider_env_conflict";

/// 토큰이 들어가는 자식 env 변수. SPEC 표의 나머지 변수는 비밀이 아니다.
pub const TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";

/// 요청 `env_overrides`에 하나라도 있으면 라우팅을 거부하는 변수. 호스트가
/// 관리하는 provider 설정(base URL·토큰)과 충돌하거나 Claude Code를 다른
/// 백엔드로 돌리는 스위치다. `CLAUDE_CONFIG_DIR`은 허용한다.
pub const ENV_CONFLICT_KEYS: [&str; 8] = [
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

/// 라우팅된 실행에서 자식 환경으로부터 **먼저 지우는** 상속 변수
/// (`env_overrides`가 얹히기 전에 적용). 셸 rc나 데몬 환경에서 물려받은
/// 다른 provider 키·백엔드 스위치·모델 지정이 Z.ai 설정을 덮지 못하게 한다.
pub const ENV_REMOVE: [&str; 10] = [
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_GATEWAY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
];

/// 라우팅된 실행에 넘길 `env_remove` 목록(비라우팅은 빈 Vec). 대기열에서
/// 깨어나는 관리 실행이 `WorkloadEntry.claude_provider`만 보고 다시 만든다.
pub fn routed_env_remove() -> Vec<String> {
    ENV_REMOVE.iter().map(|name| (*name).to_string()).collect()
}

/// Z.ai 경로가 자격증명 말고 추가로 맞추는 값.
///
/// * 자동 압축 창 — Coding Plan이 파는 id는 CLI가 스스로 크기를 알지 못하는 1M
///   컨텍스트 모델이라 창을 직접 알려 줘야 한다. 1M을 선언하지 않은 모델에
///   넣으면 압축이 너무 늦어 한도에서 실패하므로, 경로가 아니라 **모델**이
///   조건이다.
/// * 요청 타임아웃 — GLM 응답은 CLI 기본 타임아웃에 걸릴 만큼 길어질 수 있다.
///   이쪽은 모델과 무관한 경로의 성질이다.
///
/// 터미널([`zai_env`])·`claude-exec` 정리표·미션 실행(`agent_runtime::claude::auth`)이
/// 모두 이 상수를 읽는다 — 값이 갈라지면 같은 키로 띄운 세 경로가 다르게 돈다.
pub const ZAI_AUTO_COMPACT_WINDOW: (&str, &str) = ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "1000000");
/// 요청 타임아웃. [`ZAI_AUTO_COMPACT_WINDOW`] 설명 참고.
pub const ZAI_API_TIMEOUT_MS: (&str, &str) = ("API_TIMEOUT_MS", "3000000");

/// Z.ai 라우팅이 자식 환경에 얹는 **비밀이 아닌** 변수 표(SPEC). 토큰
/// ([`TOKEN_ENV`])은 여기에 없다 — 호출자가 따로 얹는다([`StartToken`],
/// [`ResolvedClaudeProvider::apply`]).
///
/// PTY 런치([`resolve`])와 일회성 CLI(`iyagi-termd claude-exec`)가 같은 표를
/// 쓰도록 한곳에 둔다 — 두 경로가 갈라지면 `claude-exec`로 띄운 Claude Code가
/// pane에서 띄운 것과 다른 모델·타임아웃으로 돌게 된다.
pub fn zai_env(main_model: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        "ANTHROPIC_BASE_URL".to_string(),
        CLAUDE_ZAI_BASE_URL.to_string(),
    );
    env.insert(
        "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
        main_model.to_string(),
    );
    env.insert(
        "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
        main_model.to_string(),
    );
    env.insert(
        "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
        ZAI_CLAUDE_HAIKU_MODEL.to_string(),
    );
    for (name, value) in [ZAI_AUTO_COMPACT_WINDOW, ZAI_API_TIMEOUT_MS] {
        env.insert(name.to_string(), value.to_string());
    }
    env.insert(
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
        "1".to_string(),
    );
    env.insert(
        "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST".to_string(),
        "1".to_string(),
    );
    env
}

/// 이 모듈이 만든 `RpcError`의 `details.reason_code`. 다른 오류는 `None`.
pub fn reason_code(error: &RpcError) -> Option<&str> {
    error
        .details
        .as_ref()
        .and_then(|details| details.get("reason_code"))
        .and_then(serde_json::Value::as_str)
}

/// env 맵의 값을 전부 지운다(0으로 덮고 비운다). 자식이 환경을 복사해 간
/// 뒤(`PtyHandle::spawn` 반환, gate 프레임 전송)에 우리 쪽 사본을 정리하는
/// 용도다. 토큰뿐 아니라 사용자 override 값도 함께 지워진다 — 그 맵은 더
/// 읽지 않는다.
pub fn zeroize_env(env: &mut BTreeMap<String, String>) {
    for value in env.values_mut() {
        value.zeroize();
    }
}

/// 저장소에서 읽은 토큰과 그 토큰으로 만든 저널 redactor. 셸 모드는
/// [`resolve`]가 런치 시점에 얻어 그대로 쓰고, 관리 모드는
/// `start_managed_admitted`가 **시작 시점**에 [`StartToken::read`]로 다시
/// 얻는다(런치 시점 것은 대기열에 있는 동안 옛 키가 됐을 수 있다).
pub struct StartToken {
    /// 토큰. drop 시 지워진다.
    pub token: Zeroizing<String>,
    /// 저널 앞단 redactor(`WorkloadEntry.redactor`) — 같은 토큰을 지운다.
    pub redactor: Arc<SecretRedactor>,
}

impl StartToken {
    /// 데몬 데이터 루트의 로컬 암호화 저장소에서 읽는다. OS/IO 오류 내용은
    /// 절대 응답이나 로그에 싣지 않는다 — reason code 하나로 충분하다. 빈
    /// 키(손으로 만든 `zai.enc`에서만 가능)는 없는 것으로 친다.
    pub fn read(root: &Path) -> Result<Self, RpcError> {
        let token = match term_secrets::LocalSecretStore::new(root).read_zeroizing() {
            Ok(Some(token)) if !token.is_empty() => token,
            Ok(_) => {
                return Err(rejected(
                    REASON_KEY_MISSING,
                    "Z.ai Coding Plan API key is not configured",
                ))
            }
            Err(_) => {
                return Err(rejected(
                    REASON_KEY_UNREADABLE,
                    "Z.ai Coding Plan API key could not be read",
                ))
            }
        };
        // redactor로 넘긴 사본은 `SecretRedactor`가 Zeroizing으로 감싼다.
        let redactor = Arc::new(SecretRedactor::new([(*token).clone()]));
        Ok(StartToken { token, redactor })
    }

    /// `env`에 토큰을 얹은 사본(gate 프레임용). 원본(엔트리의 descriptor)은
    /// 그대로 토큰 없이 남는다. 호출자는 프레임을 보낸 뒤 [`zeroize_env`]로
    /// 사본을 지운다.
    pub fn env_with_token(&self, env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut env = env.clone();
        env.insert(TOKEN_ENV.to_string(), (*self.token).clone());
        env
    }
}

// 토큰이 `{:?}`로 새지 않게 아무 필드도 찍지 않는다.
impl fmt::Debug for StartToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StartToken").finish_non_exhaustive()
    }
}

/// 해석이 끝난 라우팅. 토큰은 `token`에만 있고(`env`에는 없다) `redactor`가
/// 같은 값을 PTY 출력에서 지운다.
pub struct ResolvedClaudeProvider {
    /// 토큰을 뺀 provider 변수(SPEC 표). `apply`/`apply_selector`가 기존
    /// 값을 덮는다.
    pub env: BTreeMap<String, String>,
    /// 런치 시점에 읽은 토큰. drop 시 지워진다.
    pub token: Zeroizing<String>,
    /// [`routed_env_remove`]와 같다 — `PtyHandle::spawn`/`GateTarget`에 그대로.
    pub env_remove: Vec<String>,
    /// 저널 앞단 redactor(`WorkloadEntry.redactor`).
    pub redactor: Arc<SecretRedactor>,
    /// 요청이 보낸 선택자 원본(비밀 아님).
    pub provider: ClaudeProvider,
}

impl ResolvedClaudeProvider {
    /// 셸 모드: provider 변수와 토큰을 전부 얹는다. `with_iyagi_identity` 뒤,
    /// `opencode_integration::configure` 앞에 호출. 충돌 규칙이 이미 사용자
    /// override를 걸러냈으므로 여기서 덮이는 값은 우리 식별자 변수와 겹치지
    /// 않는다. 호출자는 spawn이 돌아오면 [`zeroize_env`]로 맵을 지운다.
    pub fn apply(&self, env: &mut BTreeMap<String, String>) {
        self.apply_selector(env);
        env.insert(TOKEN_ENV.to_string(), (*self.token).clone());
    }

    /// 관리 모드: 토큰을 **뺀** provider 변수만 얹는다. descriptor는 대기열에
    /// 남을 수 있으므로 토큰은 시작 시점에 [`StartToken::read`]로 다시 읽어
    /// gate 프레임에만 넣는다.
    pub fn apply_selector(&self, env: &mut BTreeMap<String, String>) {
        for (name, value) in &self.env {
            env.insert(name.clone(), value.clone());
        }
    }
}

// 토큰이 `{:?}`로 새지 않게 provider만 찍는다(`token`/`redactor`는 생략).
impl fmt::Debug for ResolvedClaudeProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedClaudeProvider")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// 요청의 `claude_provider`를 해석한다. `None`이면 라우팅 없음(`Ok(None)`).
///
/// 순서: 프로그램 판정(env 런처를 벗긴 뒤 Claude Code여야 한다) → env
/// 충돌 → 키 읽기. 키는 앞의 두 검사를 통과했을 때만 디스크에서 읽는다.
pub fn resolve(
    root: &Path,
    request: &LaunchRequest,
) -> Result<Option<ResolvedClaudeProvider>, RpcError> {
    let Some(provider) = request.claude_provider.as_ref() else {
        return Ok(None);
    };
    let ClaudeProvider::ZaiCodingPlan { main_model } = provider;

    // (1) 프로그램 판정: 터미널 UI의 `/usr/bin/env -u ... <claude>` 런처를
    // 벗긴 뒤 에이전트 서명이 claude여야 한다. 일반 셸·codex·opencode는
    // 거부한다(자격증명을 엉뚱한 프로세스 환경에 넣지 않는다). Windows
    // 네이티브 설치본(`claude.exe`, 역슬래시 경로)도 서명이 잡는다.
    let (program, argv) = crate::agent_session::strip_env_launcher(&request.program, &request.argv);
    if crate::agent_watch::detect_agent_in_command(&program, &argv) != Some("claude") {
        return Err(rejected(
            REASON_PROGRAM_MISMATCH,
            "claude_provider is only valid for a Claude Code program",
        ));
    }

    // (2) 충돌: 사용자가 provider 변수를 직접 넘기면 누가 이기는지 모호하다.
    // 변수 *이름*만 메시지에 싣는다(값은 비밀일 수 있다).
    if let Some(name) = ENV_CONFLICT_KEYS
        .iter()
        .find(|name| request.env_overrides.contains_key(**name))
    {
        return Err(rejected(
            REASON_ENV_CONFLICT,
            format!("env override {name} conflicts with claude_provider routing"),
        ));
    }

    // (3) 키: 런치 시점에 있어야 한다(실패는 workload 행이 생기기 전에
    // 끝난다). 셸 모드는 이 토큰을 그대로 쓰고, 관리 모드는 시작 시점에
    // 다시 읽는다.
    let StartToken { token, redactor } = StartToken::read(root)?;

    tracing::debug!(main_model = %main_model, "claude_provider resolved: Z.ai Coding Plan routing");
    Ok(Some(ResolvedClaudeProvider {
        env: zai_env(main_model),
        token,
        env_remove: routed_env_remove(),
        redactor,
        provider: provider.clone(),
    }))
}

fn rejected(code: &'static str, message: impl Into<String>) -> RpcError {
    tracing::info!(reason_code = code, "claude_provider routing rejected");
    RpcError::new(ErrorCode::InvalidArgument, message)
        .with_details(serde_json::json!({ "reason_code": code }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_contracts::ids::RequestId;
    use term_contracts::launch::{Enforcement, LaunchMode, LaunchPolicy, Priority};
    use term_contracts::U64String;

    const KEY: &str = "zai-test-key-0123456789abcdef";
    const RESUME_ID: &str = "7db2598e-c360-48fe-a2d5-0240993c9f7a";

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    fn zai(main_model: &str) -> Option<ClaudeProvider> {
        Some(ClaudeProvider::ZaiCodingPlan {
            main_model: main_model.to_string(),
        })
    }

    fn request(program: &str, argv: &[&str], provider: Option<ClaudeProvider>) -> LaunchRequest {
        LaunchRequest {
            request_id: RequestId::generate(),
            profile_id: "profile".to_string(),
            cwd: "/tmp".to_string(),
            program: program.to_string(),
            argv: args(argv),
            env_overrides: BTreeMap::new(),
            mode: LaunchMode::Shell,
            executor: term_contracts::remote::ExecutorChoice::Local,
            cols: 80,
            rows: 24,
            priority: Priority(1),
            policy: LaunchPolicy {
                reservation_bytes: U64String::new(1 << 30).expect("fits"),
                cpu_slots: 1,
                enforcement: Enforcement::Observe,
                memory_max_bytes: None,
                cpu_max_cores: None,
                pids_max: None,
            },
            claude_provider: provider,
        }
    }

    fn store_with_key(dir: &Path) {
        term_secrets::LocalSecretStore::new(dir)
            .write(KEY)
            .expect("write test key");
    }

    #[test]
    fn unrouted_request_resolves_to_none_without_touching_the_store() {
        // 저장소 디렉터리가 아예 없어도 None이다.
        let dir = tempfile::tempdir().unwrap();
        let req = request("/usr/bin/zsh", &["-l"], None);
        assert!(resolve(&dir.path().join("missing"), &req)
            .unwrap()
            .is_none());
    }

    #[test]
    fn routed_launch_without_a_key_is_rejected_with_zai_key_missing() {
        let dir = tempfile::tempdir().unwrap();
        let req = request("/x/claude", &[], zai("glm-5.3[1m]"));
        let err = resolve(dir.path(), &req).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert_eq!(reason_code(&err), Some(REASON_KEY_MISSING));
        assert!(!err.message.contains(KEY));
        // 이 모듈 밖의 오류에는 reason code가 없다.
        assert_eq!(
            reason_code(&RpcError::new(ErrorCode::InvalidArgument, "other")),
            None
        );
    }

    #[test]
    fn unreadable_store_is_reported_without_os_detail() {
        let dir = tempfile::tempdir().unwrap();
        store_with_key(dir.path());
        // 암호문은 남고 마스터 키가 사라진 저장소: 읽기 오류.
        std::fs::remove_file(dir.path().join("secrets/master.key")).unwrap();
        let req = request("/x/claude", &[], zai("glm-5.3[1m]"));
        let err = resolve(dir.path(), &req).unwrap_err();
        assert_eq!(reason_code(&err), Some(REASON_KEY_UNREADABLE));
        assert!(
            !err.message.contains("secrets/"),
            "no path or OS detail: {}",
            err.message
        );
    }

    #[test]
    fn non_claude_program_is_rejected_before_the_key_is_read() {
        // 키가 없는 저장소로도 program_mismatch가 먼저다.
        let dir = tempfile::tempdir().unwrap();
        for (program, argv) in [
            ("/usr/bin/zsh", vec!["-l"]),
            ("/opt/bin/codex", vec!["resume", RESUME_ID]),
            ("/opt/bin/opencode", vec![]),
            ("/usr/bin/env", vec!["-u", "NO_COLOR", "/usr/bin/zsh"]),
            // Windows npm shim·문서 파일은 Claude Code 실행 파일이 아니다.
            (r"C:\npm\claude.cmd", vec![]),
            ("/x/claude.md", vec![]),
        ] {
            let req = request(program, &argv, zai("glm-5.3[1m]"));
            let err = resolve(dir.path(), &req).unwrap_err();
            assert_eq!(
                reason_code(&err),
                Some(REASON_PROGRAM_MISMATCH),
                "{program} {argv:?}"
            );
        }
    }

    #[test]
    fn env_wrapped_claude_resume_is_detected_as_claude() {
        let dir = tempfile::tempdir().unwrap();
        store_with_key(dir.path());
        let req = request(
            "/usr/bin/env",
            &[
                "-u",
                "NO_COLOR",
                "-u",
                "FORCE_COLOR",
                "-u",
                "CLICOLOR",
                "-u",
                "CLICOLOR_FORCE",
                "/x/claude",
                "--resume",
                RESUME_ID,
            ],
            zai("glm-5.3-flash[1m]"),
        );
        let resolved = resolve(dir.path(), &req).unwrap().expect("routed");
        assert_eq!(
            resolved.provider,
            ClaudeProvider::ZaiCodingPlan {
                main_model: "glm-5.3-flash[1m]".to_string()
            }
        );
        // 요청은 손대지 않았다(fingerprint 입력이 그대로다).
        assert_eq!(req.program, "/usr/bin/env");
        assert!(!req.env_overrides.contains_key(TOKEN_ENV));
    }

    /// Windows 네이티브 설치본은 `claude.exe`(역슬래시 경로)다 — 서명 판정이
    /// 확장자와 구분자를 알아야 라우팅이 Windows에서 살아 있다.
    #[test]
    fn windows_native_claude_exe_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        store_with_key(dir.path());
        for program in [
            r"C:\Users\x\.local\bin\claude.exe",
            r"C:\npm\claude.exe",
            r"C:\Program Files\Claude\CLAUDE.EXE",
        ] {
            let req = request(program, &["--continue"], zai("glm-5.3[1m]"));
            let resolved = resolve(dir.path(), &req).unwrap();
            assert!(resolved.is_some(), "{program}");
        }
    }

    #[test]
    fn conflicting_env_overrides_are_rejected_but_claude_config_dir_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        store_with_key(dir.path());
        for name in ENV_CONFLICT_KEYS {
            let mut req = request("/x/claude", &[], zai("glm-5.3[1m]"));
            req.env_overrides
                .insert(name.to_string(), "user-value".to_string());
            let err = resolve(dir.path(), &req).unwrap_err();
            assert_eq!(reason_code(&err), Some(REASON_ENV_CONFLICT), "{name}");
            assert!(err.message.contains(name));
            assert!(
                !err.message.contains("user-value"),
                "values never echo back"
            );
        }
        let mut req = request("/x/claude", &[], zai("glm-5.3[1m]"));
        req.env_overrides
            .insert("CLAUDE_CONFIG_DIR".to_string(), ".claude-work".to_string());
        assert!(resolve(dir.path(), &req).unwrap().is_some());
    }

    #[test]
    fn env_table_matches_the_spec_and_apply_overlays_without_dropping_identity() {
        let dir = tempfile::tempdir().unwrap();
        store_with_key(dir.path());
        let req = request("/x/claude", &[], zai("glm-5.3[1m]"));
        let resolved = resolve(dir.path(), &req).unwrap().expect("routed");

        // 비밀이 아닌 변수만 `env`에 있다; 토큰은 `token`이 따로 든다.
        let expected: BTreeMap<&str, &str> = [
            ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
            ("ANTHROPIC_DEFAULT_OPUS_MODEL", "glm-5.3[1m]"),
            ("ANTHROPIC_DEFAULT_SONNET_MODEL", "glm-5.3[1m]"),
            ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "glm-5.3-flash[1m]"),
            ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "1000000"),
            ("API_TIMEOUT_MS", "3000000"),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
            ("CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST", "1"),
        ]
        .into_iter()
        .collect();
        let actual: BTreeMap<&str, &str> = resolved
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(resolved.token.as_str(), KEY);
        // 설정하지 않는 것들(토큰은 `env`가 아니라 `apply`가 얹는다).
        for absent in [
            TOKEN_ENV,
            "ANTHROPIC_API_KEY",
            "CLAUDE_CONFIG_DIR",
            "HOME",
            "TMPDIR",
        ] {
            assert!(
                !resolved.env.contains_key(absent),
                "{absent} must not be set"
            );
        }
        assert_eq!(resolved.env_remove, routed_env_remove());
        assert_eq!(resolved.env_remove.len(), ENV_REMOVE.len());
        assert!(resolved.env_remove.iter().any(|n| n == "ANTHROPIC_API_KEY"));
        assert!(!resolved.env_remove.iter().any(|n| n == "CLAUDE_CONFIG_DIR"));

        // apply(셸 모드): 식별자 변수는 남고 provider 변수 + 토큰이 얹힌다.
        let mut env = BTreeMap::new();
        env.insert("IYAGI_SESSION_ID".to_string(), "s".to_string());
        env.insert("CLAUDE_CONFIG_DIR".to_string(), ".claude-work".to_string());
        resolved.apply(&mut env);
        assert_eq!(env.get("IYAGI_SESSION_ID").map(String::as_str), Some("s"));
        assert_eq!(
            env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some(".claude-work")
        );
        assert_eq!(env.get(TOKEN_ENV).map(String::as_str), Some(KEY));
        assert_eq!(env.len(), 2 + expected.len() + 1);

        // apply_selector(관리 모드): 토큰만 빠진다.
        let mut env = BTreeMap::new();
        env.insert("IYAGI_SESSION_ID".to_string(), "s".to_string());
        resolved.apply_selector(&mut env);
        assert!(!env.contains_key(TOKEN_ENV));
        assert_eq!(
            env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some(CLAUDE_ZAI_BASE_URL)
        );
        assert_eq!(env.len(), 1 + expected.len());
    }

    /// 관리 실행은 시작 시점에 토큰을 다시 읽는다: 대기열에 있는 동안 키가
    /// 바뀌면 새 키를, 지워졌으면 `zai_key_missing`을 본다.
    #[test]
    fn start_token_reads_the_current_key_and_overlays_only_the_token() {
        let dir = tempfile::tempdir().unwrap();
        store_with_key(dir.path());
        let start = StartToken::read(dir.path()).expect("key stored");
        assert_eq!(start.token.as_str(), KEY);
        assert!(start.redactor.contains_secret(KEY.as_bytes()));

        let mut descriptor_env = BTreeMap::new();
        descriptor_env.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            CLAUDE_ZAI_BASE_URL.to_string(),
        );
        let frame_env = start.env_with_token(&descriptor_env);
        assert_eq!(frame_env.get(TOKEN_ENV).map(String::as_str), Some(KEY));
        assert_eq!(frame_env.len(), 2);
        // 원본(엔트리의 descriptor)에는 토큰이 들어가지 않는다.
        assert!(!descriptor_env.contains_key(TOKEN_ENV));

        let debug = format!("{start:?}");
        assert!(!debug.contains(KEY), "{debug}");

        // 키가 바뀌면 다음 읽기가 새 값을 준다.
        term_secrets::LocalSecretStore::new(dir.path())
            .write("rotated-key-fedcba9876543210")
            .unwrap();
        let rotated = StartToken::read(dir.path()).unwrap();
        assert_eq!(rotated.token.as_str(), "rotated-key-fedcba9876543210");
        assert!(!rotated.redactor.contains_secret(KEY.as_bytes()));

        // 지워지면 reason code로 실패한다.
        term_secrets::LocalSecretStore::new(dir.path())
            .remove()
            .unwrap();
        let err = StartToken::read(dir.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert_eq!(reason_code(&err), Some(REASON_KEY_MISSING));
    }

    /// `zai_env`는 비밀이 아닌 8개 변수만 만든다. 일회성 CLI
    /// (`iyagi-termd claude-exec`)가 같은 표를 쓰므로 키 집합과 "토큰은 없다"는
    /// 성질이 여기서 고정된다.
    #[test]
    fn zai_env_builds_the_eight_non_secret_variables_without_the_token() {
        let env = zai_env("glm-5.3-flash[1m]");
        let names: Vec<&str> = env.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            vec![
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "API_TIMEOUT_MS",
                "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
                "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
            ]
        );
        // 주 모델은 opus/sonnet 슬롯에만 들어가고 haiku는 고정이다.
        assert_eq!(
            env.get("ANTHROPIC_DEFAULT_OPUS_MODEL").map(String::as_str),
            Some("glm-5.3-flash[1m]")
        );
        assert_eq!(
            env.get("ANTHROPIC_DEFAULT_SONNET_MODEL")
                .map(String::as_str),
            Some("glm-5.3-flash[1m]")
        );
        assert_eq!(
            env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL").map(String::as_str),
            Some(ZAI_CLAUDE_HAIKU_MODEL)
        );
        assert_eq!(
            env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some(CLAUDE_ZAI_BASE_URL)
        );
        for absent in [TOKEN_ENV, "ANTHROPIC_API_KEY", "CLAUDE_CONFIG_DIR"] {
            assert!(!env.contains_key(absent), "{absent} must not be set");
        }
    }

    #[test]
    fn zeroize_env_wipes_every_value_but_keeps_the_keys() {
        let mut env = BTreeMap::new();
        env.insert(TOKEN_ENV.to_string(), KEY.to_string());
        env.insert("IYAGI_SESSION_ID".to_string(), "s".to_string());
        zeroize_env(&mut env);
        assert_eq!(env.len(), 2);
        assert!(env.values().all(String::is_empty), "{env:?}");
    }

    #[test]
    fn redactor_scrubs_the_key_and_debug_never_prints_it() {
        let dir = tempfile::tempdir().unwrap();
        store_with_key(dir.path());
        let req = request("/x/claude", &[], zai("glm-5.3[1m]"));
        let resolved = resolve(dir.path(), &req).unwrap().expect("routed");

        let mut line = format!("ANTHROPIC_AUTH_TOKEN={KEY} ok");
        crate::exec::output::Redactor::redact(&*resolved.redactor, &mut line);
        assert_eq!(line, "ANTHROPIC_AUTH_TOKEN=[redacted] ok");
        let mut plain = format!("token={KEY}\r\n");
        resolved.redactor.redact_plain(&mut plain);
        assert_eq!(plain, "token=[redacted]\r\n");
        assert!(resolved.redactor.contains_secret(KEY.as_bytes()));
        assert!(!resolved.redactor.contains_secret(b"nothing here"));

        let debug = format!("{resolved:?}");
        assert!(!debug.contains(KEY), "{debug}");
        assert!(debug.contains("ZaiCodingPlan"), "{debug}");
        assert!(!debug.contains(TOKEN_ENV), "{debug}");
    }
}
