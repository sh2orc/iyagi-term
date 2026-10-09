//! Launch input contract and its ordered validation (spec `01-contracts.md` §2).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{RequestId, U64String};
use crate::remote::{valid_remote_host_id, ExecutorChoice};

/// Managed vs plain shell execution. Shells carry no task/attempt identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum LaunchMode {
    Shell,
    Managed,
}

/// 0 is the highest priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[repr(transparent)]
pub struct Priority(pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum Enforcement {
    /// Measure and gate admission only.
    Observe,
    /// Apply the OS limits that are available; enumerate what is missing.
    Prefer,
    /// Fail before start when any requested limit cannot be enforced.
    Require,
}

/// Resource policy attached to a launch (spec table `ResourcePolicy`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LaunchPolicy {
    /// Admission estimate, not a hard cap.
    pub reservation_bytes: U64String,
    pub cpu_slots: u32,
    pub enforcement: Enforcement,
    /// `null` leaves the OS memory hard cap unset.
    pub memory_max_bytes: Option<U64String>,
    pub cpu_max_cores: Option<f64>,
    pub pids_max: Option<u32>,
}

/// Z.ai Coding Plan 라우팅에서 Claude Code의 주 모델(opus/sonnet 슬롯)로 고를 수
/// 있는 모델 id. haiku/백그라운드 슬롯은 [`ZAI_CLAUDE_HAIKU_MODEL`]로 고정한다.
pub const ZAI_CLAUDE_MAIN_MODELS: [&str; 2] = ["glm-5.3[1m]", "glm-5.3-flash[1m]"];

/// Z.ai 라우팅 시 `ANTHROPIC_DEFAULT_HAIKU_MODEL`에 고정으로 들어가는 모델.
pub const ZAI_CLAUDE_HAIKU_MODEL: &str = "glm-5.3-flash[1m]";

/// Host-selected model provider for a Claude Code launch. This is a
/// non-secret *selector*: the UI never sends the credential, the daemon
/// resolves it from its own secret store at launch time. Absent (`null`)
/// means "Claude Code's own auth", i.e. no routing at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaudeProvider {
    /// Route through the Z.ai Coding Plan Anthropic-compatible endpoint with
    /// `main_model` in the opus/sonnet slots (one of
    /// [`ZAI_CLAUDE_MAIN_MODELS`]).
    ZaiCodingPlan { main_model: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LaunchRequest {
    pub request_id: RequestId,
    pub profile_id: String,
    /// Absolute existing directory (existence checked daemon-side).
    pub cwd: String,
    /// Absolute normalized executable path.
    pub program: String,
    /// Arguments excluding the program itself.
    pub argv: Vec<String>,
    pub env_overrides: BTreeMap<String, String>,
    pub mode: LaunchMode,
    #[serde(default)]
    pub executor: ExecutorChoice,
    pub cols: u16,
    pub rows: u16,
    pub priority: Priority,
    pub policy: LaunchPolicy,
    /// Host-selected model provider for Claude Code launches (non-secret selector; the daemon resolves the credential).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_provider: Option<ClaudeProvider>,
}

/// Hard limits from `defaults.json` mirrored as typed constants. The daemon
/// reads the same values from [`crate::defaults`]; validation keeps its own
/// copy so the contract layer stays dependency-free.
pub mod limits {
    pub const LAUNCH_ARGV_COUNT: usize = 256;
    pub const LAUNCH_ARGUMENT_BYTES: usize = 65_536;
    pub const TERMINAL_MIN_DIM: u16 = 2;
    pub const TERMINAL_MAX_DIM: u16 = 1000;
    pub const PRIORITY_MAX: u8 = 2;
}

/// Outcome of the ordered validation pipeline. The first failure wins so the
/// caller can answer with the most fundamental problem (spec §2 ordering:
/// protocol/length → ids/enums/env/cols/rows → absolute paths).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchValidation {
    Valid,
    Invalid(&'static str),
}

impl LaunchRequest {
    /// Pure, filesystem-independent validation. Duplicate-request detection
    /// and path existence are daemon-side concerns that need storage/fs.
    pub fn validate(&self) -> LaunchValidation {
        use LaunchValidation::Invalid;
        if self.argv.len() > limits::LAUNCH_ARGV_COUNT {
            return Invalid("argv count exceeds 256");
        }
        let mut total: usize = self.program.len();
        for a in &self.argv {
            total = total.saturating_add(a.len() + 1);
        }
        for (k, v) in &self.env_overrides {
            if k.contains('\0') || v.contains('\0') || k.is_empty() {
                return Invalid("env keys/values must be non-empty and NUL-free");
            }
            total = total.saturating_add(k.len() + v.len() + 2);
        }
        if total > limits::LAUNCH_ARGUMENT_BYTES {
            return Invalid("argv+env UTF-8 total exceeds 64 KiB");
        }
        // Executor variant shape (R2, spec `05-remote.md` §1): remote
        // execution is not implemented and the daemon rejects non-local
        // executors at `workload.launch`. The ordered pipeline still checks
        // the field so a malformed host id cannot pass silently through any
        // other validate() caller. The rule is the one host registration
        // applies (`RemoteHostConfig::validate`), so every registered host
        // stays launchable and every accepted id is one it could register.
        match &self.executor {
            ExecutorChoice::Local => {}
            ExecutorChoice::Remote { host_id } => {
                if !valid_remote_host_id(host_id) {
                    return Invalid("remote executor host_id is not a valid remote host id");
                }
            }
        }
        // Provider selector shape: the model id must be one the daemon knows
        // how to place into the Claude Code model slots. The credential
        // itself is resolved daemon-side and is never part of this request.
        if let Some(ClaudeProvider::ZaiCodingPlan { main_model }) = &self.claude_provider {
            if !ZAI_CLAUDE_MAIN_MODELS.contains(&main_model.as_str()) {
                return Invalid("claude_provider.main_model is not a supported Z.ai model");
            }
        }
        if !self.cwd.chars().all(|c| !c.is_control()) || self.cwd.is_empty() {
            return Invalid("cwd must be a non-empty control-free path");
        }
        if !std::path::Path::new(&self.cwd).is_absolute() {
            return Invalid("cwd must be absolute");
        }
        if !std::path::Path::new(&self.program).is_absolute() {
            return Invalid("program must be an absolute normalized path");
        }
        if self.program.contains("..") {
            return Invalid("program must not contain traversal segments");
        }
        if self.cols < limits::TERMINAL_MIN_DIM || self.cols > limits::TERMINAL_MAX_DIM {
            return Invalid("cols out of 2..=1000");
        }
        if self.rows < limits::TERMINAL_MIN_DIM || self.rows > limits::TERMINAL_MAX_DIM {
            return Invalid("rows out of 2..=1000");
        }
        if self.priority.0 > limits::PRIORITY_MAX {
            return Invalid("priority out of 0..=2");
        }
        if self.policy.cpu_slots == 0 || self.policy.cpu_slots > 1024 {
            return Invalid("cpu_slots out of 1..=1024");
        }
        if let Some(cores) = self.policy.cpu_max_cores {
            // schema.sql: CHECK (cpu_max_cores > 0) — reject here as
            // INVALID_ARGUMENT instead of surfacing a raw SQLite constraint
            // error from record_launch_intent. NaN is rejected too.
            if cores.is_nan() || cores <= 0.0 || cores > 1024.0 {
                return Invalid("cpu_max_cores out of (0, 1024]");
            }
        }
        if let Some(bytes) = &self.policy.memory_max_bytes {
            // schema.sql: CHECK (memory_max_bytes > 0).
            if bytes.get() == 0 {
                return Invalid("memory_max_bytes must be positive when set");
            }
        }
        if let Some(pids) = self.policy.pids_max {
            if pids == 0 {
                return Invalid("pids_max must be positive when set");
            }
        }
        LaunchValidation::Valid
    }

    /// Canonical fingerprint source: fixed serialization of the request with
    /// object keys sorted (BTreeMap everywhere) and no filesystem state.
    /// `mode` and `claude_provider` participate (a routed and an unrouted
    /// launch of the same command are different requests); the request id
    /// does NOT (it is the lookup key).
    pub fn fingerprint_source(&self) -> Vec<u8> {
        let canonical = CanonicalLaunch {
            profile_id: &self.profile_id,
            cwd: &self.cwd,
            program: &self.program,
            argv: &self.argv,
            env_overrides: &self.env_overrides,
            mode: self.mode,
            executor: &self.executor,
            cols: self.cols,
            rows: self.rows,
            priority: self.priority,
            policy: &self.policy,
            claude_provider: self.claude_provider.as_ref(),
        };
        serde_json::to_vec(&canonical).expect("canonical launch serialization is infallible")
    }
}

/// SHA-256 over [`LaunchRequest::fingerprint_source`], hex encoded.
pub fn launch_fingerprint(req: &LaunchRequest) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(req.fingerprint_source());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

#[derive(Serialize)]
struct CanonicalLaunch<'a> {
    profile_id: &'a str,
    cwd: &'a str,
    program: &'a str,
    argv: &'a [String],
    env_overrides: &'a BTreeMap<String, String>,
    mode: LaunchMode,
    executor: &'a ExecutorChoice,
    cols: u16,
    rows: u16,
    priority: Priority,
    policy: &'a LaunchPolicy,
    /// Omitted when absent so fingerprints of unrouted requests stay
    /// byte-identical to the ones minted before this field existed
    /// (idempotent replay across a daemon upgrade).
    #[serde(skip_serializing_if = "Option::is_none")]
    claude_provider: Option<&'a ClaudeProvider>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request() -> LaunchRequest {
        let root = if cfg!(windows) { "C:/repo" } else { "/repo" };
        LaunchRequest {
            request_id: RequestId::generate(),
            profile_id: "00000000-0000-4000-8000-000000000000".into(),
            cwd: root.into(),
            program: format!("{root}/bin/codex"),
            argv: vec!["--version".into()],
            env_overrides: BTreeMap::new(),
            mode: LaunchMode::Managed,
            executor: ExecutorChoice::Local,
            cols: 80,
            rows: 24,
            priority: Priority(1),
            policy: LaunchPolicy {
                reservation_bytes: U64String::new(2_147_483_648).unwrap(),
                cpu_slots: 1,
                enforcement: Enforcement::Observe,
                memory_max_bytes: None,
                cpu_max_cores: None,
                pids_max: None,
            },
            claude_provider: None,
        }
    }

    fn zai(main_model: &str) -> Option<ClaudeProvider> {
        Some(ClaudeProvider::ZaiCodingPlan {
            main_model: main_model.into(),
        })
    }

    #[test]
    fn valid_request_passes() {
        assert_eq!(base_request().validate(), LaunchValidation::Valid);
    }

    /// `claude_provider`는 선택 필드다: 없으면 직렬화에서 빠지고(구 앱/구 데몬과
    /// 바이트 호환), 있으면 `kind` 태그 유니온으로 왕복한다. 자격 증명은 이
    /// 요청 어디에도 실리지 않는다 — 모델 선택자만 오간다.
    #[test]
    fn claude_provider_round_trips_and_is_omitted_when_absent() {
        let plain = base_request();
        let json = serde_json::to_value(&plain).unwrap();
        assert!(
            json.get("claude_provider").is_none(),
            "absent selector must not be serialized: {json}"
        );
        let back: LaunchRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.claude_provider, None);

        let mut routed = base_request();
        routed.claude_provider = zai("glm-5.3[1m]");
        assert_eq!(routed.validate(), LaunchValidation::Valid);
        let json = serde_json::to_value(&routed).unwrap();
        assert_eq!(
            json["claude_provider"],
            serde_json::json!({"kind": "zai_coding_plan", "main_model": "glm-5.3[1m]"})
        );
        let back: LaunchRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back, routed);

        // Explicit null from a client that always writes the key.
        let mut json = serde_json::to_value(&plain).unwrap();
        json["claude_provider"] = serde_json::Value::Null;
        let back: LaunchRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.claude_provider, None);
    }

    #[test]
    fn claude_provider_rejects_unknown_main_model() {
        let mut req = base_request();
        for model in ["gpt-5", "glm-5.3", "", "GLM-5.3[1m]", "glm-5.3-flash"] {
            req.claude_provider = zai(model);
            assert_eq!(
                req.validate(),
                LaunchValidation::Invalid(
                    "claude_provider.main_model is not a supported Z.ai model"
                ),
                "model {model:?} must be rejected"
            );
        }
        for model in ZAI_CLAUDE_MAIN_MODELS {
            req.claude_provider = zai(model);
            assert_eq!(req.validate(), LaunchValidation::Valid, "model {model:?}");
        }
        assert!(ZAI_CLAUDE_MAIN_MODELS.contains(&ZAI_CLAUDE_HAIKU_MODEL));
        // Unknown `kind` tags are a wire error, not a silent fallback.
        assert!(serde_json::from_value::<ClaudeProvider>(
            serde_json::json!({"kind": "bedrock", "main_model": "x"})
        )
        .is_err());
    }

    /// 같은 명령이라도 라우팅 여부·주 모델이 다르면 다른 요청이다(중복 감지가
    /// 라우팅된 실행을 라우팅 안 된 실행으로 되돌려 주면 안 된다). 라우팅이
    /// 없는 요청의 지문은 이 필드가 생기기 전과 같아야 한다.
    #[test]
    fn fingerprint_distinguishes_claude_provider() {
        let plain = base_request();
        let mut routed = base_request();
        routed.claude_provider = zai("glm-5.3[1m]");
        let mut routed_flash = base_request();
        routed_flash.claude_provider = zai("glm-5.3-flash[1m]");
        assert_ne!(launch_fingerprint(&plain), launch_fingerprint(&routed));
        assert_ne!(
            launch_fingerprint(&routed),
            launch_fingerprint(&routed_flash)
        );
        let mut routed_again = base_request();
        routed_again.claude_provider = zai("glm-5.3[1m]");
        assert_eq!(
            launch_fingerprint(&routed),
            launch_fingerprint(&routed_again)
        );
        // Unrouted canonical bytes carry no trace of the field at all.
        let source = String::from_utf8(plain.fingerprint_source()).unwrap();
        assert!(!source.contains("claude_provider"), "{source}");
        let source = String::from_utf8(routed.fingerprint_source()).unwrap();
        assert!(
            source.contains(r#""claude_provider":{"kind":"zai_coding_plan""#),
            "{source}"
        );
    }

    #[test]
    fn absolute_path_shape_required() {
        let mut req = base_request();
        req.cwd = "relative/path".into();
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
        let root = if cfg!(windows) { "C:/repo" } else { "/repo" };
        req.cwd = root.into();
        req.program = "codex".into();
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
        req.program = format!("{root}/bin/codex");
        req.program = format!("{root}/usr/../bin/codex");
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
    }

    #[test]
    fn valid_variant_is_importable() {
        assert_eq!(base_request().validate(), LaunchValidation::Valid);
    }

    /// Remote launches are rejected by the daemon (R2 unimplemented); the
    /// pipeline still validates the variant so an empty or malformed host id
    /// cannot slip through. The rule is the registration rule: every id a
    /// `RemoteHostConfig` accepts (a UUID or a name like `host-1`) is `Valid`
    /// here — the "remote not supported" policy lives in the dispatch handler.
    #[test]
    fn remote_executor_host_id_follows_the_registration_rule() {
        use crate::remote::{RemoteHostConfig, REMOTE_HOST_ID_MAX};
        let mut req = base_request();
        let mut host = RemoteHostConfig {
            id: String::new(),
            ssh_config_alias: "buildbox".into(),
            label: "build".into(),
            runner_path: "/usr/local/bin/iyagi-termd".into(),
            protocol_version: 1,
        };
        let too_long = "h".repeat(REMOTE_HOST_ID_MAX + 1);
        for id in [
            "",
            "-oProxyCommand=x",
            "../x",
            "host 1",
            "{e2f5c8e0-6b1a-41d0-a08c-0020af31e880}",
            "urn:uuid:e2f5c8e0-6b1a-41d0-a08c-0020af31e880",
            too_long.as_str(),
        ] {
            req.executor = ExecutorChoice::Remote {
                host_id: id.to_owned(),
            };
            assert_ne!(req.validate(), LaunchValidation::Valid, "id {id:?}");
            host.id = id.to_owned();
            assert!(host.validate().is_err(), "id {id:?} must not register");
        }
        let v4 = uuid::Uuid::new_v4().to_string();
        for id in ["host-1", "buildbox", v4.as_str()] {
            req.executor = ExecutorChoice::Remote {
                host_id: id.to_owned(),
            };
            assert_eq!(req.validate(), LaunchValidation::Valid, "id {id:?}");
            host.id = id.to_owned();
            assert!(host.validate().is_ok(), "id {id:?} must register");
        }
        // Clients that omit executor deserialize to the Local default.
        req.executor = ExecutorChoice::Local;
        assert_eq!(req.validate(), LaunchValidation::Valid);
    }

    #[test]
    fn argv_count_and_bytes_are_rejected() {
        let mut req = base_request();
        req.argv = vec!["a".to_string(); 257];
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
        req.argv = vec!["x".repeat(65_536)];
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
    }

    #[test]
    fn nul_and_dimensions_rejected() {
        let mut req = base_request();
        req.env_overrides.insert("A".into(), "b\0c".into());
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
        req.env_overrides.clear();
        req.cols = 1001;
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
        req.cols = 1;
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
        req.cols = 80;
        req.priority = Priority(3);
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
    }

    /// Zero/NaN limits are rejected at the contract, matching schema.sql's
    /// CHECK constraints (otherwise the launch failed with an internal
    /// storage error instead of INVALID_ARGUMENT).
    #[test]
    fn zero_or_nan_limits_are_invalid_like_the_schema_checks() {
        let mut req = base_request();
        req.policy.cpu_max_cores = Some(0.0);
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
        req.policy.cpu_max_cores = Some(f64::NAN);
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
        req.policy.cpu_max_cores = Some(1025.0);
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
        req.policy.cpu_max_cores = Some(0.5);
        assert_eq!(req.validate(), LaunchValidation::Valid);
        req.policy.memory_max_bytes = Some(U64String::new(0).unwrap());
        assert!(matches!(req.validate(), LaunchValidation::Invalid(_)));
        req.policy.memory_max_bytes = Some(U64String::new(1 << 30).unwrap());
        assert_eq!(req.validate(), LaunchValidation::Valid);
    }

    #[test]
    fn fingerprint_is_stable_and_order_insensitive_for_env() {
        let mut a = base_request();
        let mut b = base_request();
        a.env_overrides.insert("X".into(), "1".into());
        a.env_overrides.insert("Y".into(), "2".into());
        b.env_overrides.insert("Y".into(), "2".into());
        b.env_overrides.insert("X".into(), "1".into());
        assert_eq!(launch_fingerprint(&a), launch_fingerprint(&b));
        b.argv.push("--changed".into());
        assert_ne!(launch_fingerprint(&a), launch_fingerprint(&b));
        // request_id is the dedup key, not part of the fingerprint body
        b.argv.pop();
        b.request_id = RequestId::generate();
        assert_eq!(launch_fingerprint(&a), launch_fingerprint(&b));
    }
}
