//! Launch-gate protocol between the daemon and `iyagi-termd --launch-helper`
//! (spec `02-runner.md` §3).
//!
//! The helper is the PTY's first child. It connects the private gate endpoint,
//! reports its identity, and WAITS. Only after the daemon verified the peer,
//! attached the helper to the OS resource group, and persisted ownership does
//! it send the target spec and RELEASE. The helper then execs (Unix) or
//! spawns-and-waits (Windows) the target. The 256-bit nonce is one factor;
//! endpoint ACL + peer identity are verified alongside it, never alone.

use serde::{Deserialize, Serialize};

use crate::ids::ProcessIdentity;

pub const GATE_TIMEOUT_MS: u64 = 5_000;

/// Helper -> daemon, immediately after connecting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelperHello {
    /// The one-time 256-bit nonce passed via argv.
    pub nonce: String,
    pub identity: ProcessIdentity,
}

/// Daemon -> helper: the target to run once RELEASE arrives. Never logged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateTarget {
    pub program: String,
    pub argv: Vec<String>,
    pub env_overrides: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub env_clear: bool,
    /// Variable names removed from the child environment BEFORE env_overrides apply (neutralizes inherited provider/auth vars).
    #[serde(default)]
    pub env_remove: Vec<String>,
    pub cwd: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonToHelper {
    /// Single-use: a second RELEASE on the same helper is a protocol error.
    Release,
    /// Abort before target creation (group attach failed, cancelled, ...).
    Abort,
}

/// Helper -> daemon terminal report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HelperToDaemon {
    /// Target started. Unix: post-exec success via close-on-exec error pipe
    /// EOF; Windows: CreateProcess succeeded.
    Started,
    /// Target could not start; `errno`-style code (limited detail only).
    StartFailed { code: i32 },
    /// Target exited with this code (Windows helper waits; Unix daemon reads
    /// the PTY child directly and this variant is unused there).
    Exited { code: i32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_messages_round_trip() {
        let hello = HelperHello {
            nonce: "n".repeat(64),
            identity: ProcessIdentity {
                pid: 7,
                start_token: "42".into(),
                boot_id: "boot".into(),
            },
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert_eq!(serde_json::from_str::<HelperHello>(&json).unwrap(), hello);

        let release = DaemonToHelper::Release;
        let json = serde_json::to_string(&release).unwrap();
        assert!(json.contains("\"release\""));
        assert_eq!(
            serde_json::from_str::<DaemonToHelper>(&json).unwrap(),
            release
        );

        let failed = HelperToDaemon::StartFailed { code: 13 };
        let json = serde_json::to_string(&failed).unwrap();
        assert_eq!(
            serde_json::from_str::<HelperToDaemon>(&json).unwrap(),
            failed
        );
    }

    /// `env_remove`는 순서를 지키며 왕복하고, 이 필드를 모르는 구 데몬이 보낸
    /// 타깃(키 자체가 없음)은 빈 목록으로 읽힌다 — 헬퍼가 옛 프레임을 거절해
    /// 실행이 막히면 안 된다.
    #[test]
    fn gate_target_round_trips_env_remove_and_defaults_when_absent() {
        let target = GateTarget {
            program: "/usr/local/bin/claude".into(),
            argv: vec!["/usr/local/bin/claude".into()],
            env_overrides: std::collections::BTreeMap::new(),
            env_clear: false,
            env_remove: vec!["ANTHROPIC_API_KEY".into(), "CLAUDE_CODE_OAUTH_TOKEN".into()],
            cwd: "/home/dev".into(),
        };
        let json = serde_json::to_string(&target).unwrap();
        let back: GateTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(back, target);
        assert_eq!(
            back.env_remove,
            vec!["ANTHROPIC_API_KEY".to_string(), "CLAUDE_CODE_OAUTH_TOKEN".to_string()]
        );

        let legacy = serde_json::json!({
            "program": "/bin/zsh",
            "argv": ["/bin/zsh", "-l"],
            "env_overrides": {},
            "cwd": "/home/dev"
        });
        let back: GateTarget = serde_json::from_value(legacy).unwrap();
        assert!(back.env_remove.is_empty());
        assert!(!back.env_clear);
    }
}
