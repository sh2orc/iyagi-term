//! R2 remote-execution contracts (spec `05-remote.md`).
//!
//! The local daemon owns remote host registration and connection state; the
//! same gated-launch/idempotency/admission machinery runs on the remote
//! daemon. Transport: `ssh -T <alias> <runner_path> --gateway` with the
//! framed protocol multiplexed over the single stdio pair (gateway envelope).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::snapshot::Capabilities;

/// Registered remote host (spec 05 §1). `ssh_config_alias` must be a plain
/// alias from the user's SSH config — never a host string with options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RemoteHostConfig {
    pub id: String,
    pub ssh_config_alias: String,
    pub label: String,
    /// Absolute runner path verified at registration (no shell metacharacters).
    pub runner_path: String,
    pub protocol_version: u32,
}

/// Longest accepted remote host id, in bytes (see [`valid_remote_host_id`]).
pub const REMOTE_HOST_ID_MAX: usize = 128;

/// Remote host id rule. Registration ([`RemoteHostConfig::validate`]) and
/// launch placement (`ExecutorChoice::Remote` in `LaunchRequest::validate`)
/// both apply exactly this rule, so a host that registers can always be
/// launched to. Spec 05 §1 fixes no id format (a daemon-minted UUID and a
/// name like `host-1` are both fine), so the rule is shape-only: 1..=128
/// ASCII alphanumerics, `-`, `_` or `.`, starting with an alphanumeric — no
/// whitespace, control, path-separator or shell characters, no leading `-`
/// (read as a flag if the id ever reaches an argv) and no `.`/`..` segment.
pub fn valid_remote_host_id(id: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    id.len() <= REMOTE_HOST_ID_MAX
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id.chars().all(allowed)
}

impl RemoteHostConfig {
    /// Validation per spec 05 §1–§2.1: the id follows the shared host id
    /// rule; the alias must be a plain config alias; the runner path an
    /// absolute metacharacter-free path.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_remote_host_id(&self.id) {
            return Err("id must be 1..=128 ASCII letters, digits, '-', '_' or '.'");
        }
        let alias = &self.ssh_config_alias;
        if alias.is_empty()
            || alias.starts_with('-')
            || alias
                .chars()
                .any(|c| c.is_whitespace() || ";&|<>()$`\"'*?[]{}#!\\".contains(c))
        {
            return Err("ssh_config_alias must be a plain SSH config alias");
        }
        let path = &self.runner_path;
        // 원격 러너 경로는 원격 OS 기준 — 호스트와 무관하게 절대 경로 형태만
        // 검사한다(POSIX `/...` 또는 Windows `C:\...`/`C:/...`).
        let absolute = path.starts_with('/')
            || (path.len() >= 3
                && path.as_bytes()[0].is_ascii_alphabetic()
                && path.as_bytes()[1] == b':'
                && (path.as_bytes()[2] == b'/' || path.as_bytes()[2] == b'\\'));
        if !absolute
            || path
                .chars()
                .any(|c| c.is_whitespace() || ";&|<>()$`\"'*?[]{}#!".contains(c))
            || path.contains("..")
        {
            return Err("runner_path must be absolute without shell metacharacters");
        }
        // R2 targets Linux hosts (spec 05 §2.1): a POSIX runner path is
        // single-quoted into the remote command, and a login shell such as
        // fish still interprets `\` inside single quotes — reject it there.
        // The Windows drive shape keeps its separator until cmd/PowerShell
        // quoting is specified (registration refuses those hosts meanwhile).
        if path.starts_with('/') && path.contains('\\') {
            return Err("runner_path must not contain a backslash on a POSIX host");
        }
        if self.label.is_empty() {
            return Err("label must not be empty");
        }
        Ok(())
    }
}

/// Transport connection state, deliberately separate from any workload state
/// (spec 05 §2: 네트워크 단절을 작업 실패/성공으로 바꾸지 않는다).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RemoteConnectionState {
    Connected,
    Disconnected,
    Reconnecting,
    #[serde(rename = "AUTH_REQUIRED")]
    AuthRequired,
}

/// Classified remote failure kinds (spec 05 §5: host key change / auth
/// expiry / remote disk full / CLI missing must be distinct errors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum RemoteErrorKind {
    HostKeyChanged,
    AuthRequired,
    RemoteDiskFull,
    CliMissing,
    Transport,
    Timeout,
    ProtocolMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RemoteError {
    pub kind: RemoteErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RemoteHostStatus {
    pub host_id: String,
    pub state: RemoteConnectionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_capabilities: Option<Capabilities>,
    /// Last gateway round-trip in monotonic ms (measured by hello).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_trip_ms: Option<u64>,
    /// Reconnect backoff attempt counter (schedule 1,2,4,8,16,30s capped).
    #[serde(default)]
    pub reconnect_attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<RemoteError>,
}

/// Where a launch executes. R1 implies `local`; R2 adds remote hosts.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ExecutorChoice {
    #[default]
    Local,
    Remote {
        host_id: String,
    },
}

/// Gateway channel multiplexer: SSH `-T` provides one stdio pair; control
/// and data connections share it via this outer envelope. Channel 0 carries
/// the control connection's frames, channel 1 the data connection's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayEnvelope {
    pub v: u32,
    pub gw: bool,
    pub channel: u8,
    /// The complete inner wire frame (already JSON).
    pub frame: serde_json::Value,
}

impl GatewayEnvelope {
    pub const CONTROL: u8 = 0;
    pub const DATA: u8 = 1;

    pub fn wrap(channel: u8, frame: serde_json::Value) -> Self {
        Self {
            v: crate::rpc::PROTOCOL_VERSION,
            gw: true,
            channel,
            frame,
        }
    }
}

/// Source-sync snapshot description (spec 05 §3). Transfer itself is
/// content-addressed; these are the request parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SourceSnapshotRequest {
    /// Baseline commit OID (repo HEAD at request time).
    pub baseline_commit: String,
    /// Repo-relative working-directory root ("" = repo root).
    pub repo_relative_path: String,
    /// Dirty tracked files the user opted in to transfer (paths + content hashes).
    pub dirty_files: Vec<SnapshotFile>,
    /// Untracked files the user explicitly selected.
    pub untracked_files: Vec<SnapshotFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SnapshotFile {
    /// Repo-relative, POSIX separators, no `..`, no absolute, no symlink entries.
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Result of a finished remote attempt (spec 05 §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RemoteResultManifest {
    pub attempt_id: String,
    pub baseline_commit: String,
    pub changed_files: Vec<SnapshotFile>,
    /// True when local files diverged from the transferred baseline — the
    /// patch is then delivered into a separate worktree, never applied.
    pub conflict: bool,
    pub conflict_files: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_validation_rejects_metacharacters_and_dashes() {
        let base = RemoteHostConfig {
            id: "h".into(),
            ssh_config_alias: "buildbox".into(),
            label: "build".into(),
            runner_path: "/usr/local/bin/iyagi-termd".into(),
            protocol_version: 1,
        };
        assert!(base.validate().is_ok());

        let mut bad = base.clone();
        for alias in [
            "-oProxyCommand=x",
            "host -p 22",
            "host;rm",
            "a b",
            "host|cmd",
            "$(x)",
            "",
        ] {
            bad.ssh_config_alias = alias.into();
            assert!(bad.validate().is_err(), "alias {alias:?} must be rejected");
        }

        bad = base.clone();
        for path in [
            "iyagi-termd",
            "relative/x",
            "/a b/runner",
            "/usr/bin/../x",
            "/a;b/x",
            "/opt/iyagi\\runner",
        ] {
            bad.runner_path = path.into();
            assert!(bad.validate().is_err(), "path {path:?} must be rejected");
        }
        // Only POSIX paths reject `\` (spec 05 §2.1); the Windows drive
        // shape is still accepted by the contract.
        bad.runner_path = "C:\\iyagi\\iyagi-termd.exe".into();
        assert!(bad.validate().is_ok());
    }

    /// Registration and launch placement share one host id rule, so every
    /// registered host stays launchable (spec 05 §1 fixes no id format).
    #[test]
    fn host_id_rule_accepts_names_and_uuids_and_rejects_unsafe_shapes() {
        let mut host = RemoteHostConfig {
            id: "host-1".into(),
            ssh_config_alias: "buildbox".into(),
            label: "build".into(),
            runner_path: "/usr/local/bin/iyagi-termd".into(),
            protocol_version: 1,
        };
        let uuid_v4 = "6f1c1f5e-6a0e-4c3a-9d5b-2f0b8f1f9a11";
        let longest = "h".repeat(REMOTE_HOST_ID_MAX);
        for id in ["host-1", "h", "build_box.lan", uuid_v4, longest.as_str()] {
            host.id = id.into();
            assert!(valid_remote_host_id(id), "id {id:?} must be accepted");
            assert!(host.validate().is_ok(), "id {id:?} must register");
        }
        let too_long = "h".repeat(REMOTE_HOST_ID_MAX + 1);
        for id in [
            "",
            "-oProxyCommand=x",
            ".",
            "..",
            "../x",
            "a/b",
            "a\\b",
            "host 1",
            "host;rm",
            "{6f1c1f5e-6a0e-4c3a-9d5b-2f0b8f1f9a11}",
            "urn:uuid:6f1c1f5e-6a0e-4c3a-9d5b-2f0b8f1f9a11",
            "호스트",
            too_long.as_str(),
        ] {
            host.id = id.into();
            assert!(!valid_remote_host_id(id), "id {id:?} must be rejected");
            assert!(host.validate().is_err(), "id {id:?} must not register");
        }
    }

    #[test]
    fn executor_defaults_to_local_for_backcompat() {
        let json = serde_json::json!({});
        let choice: ExecutorChoice = serde_json::from_value(json).unwrap_or_default();
        assert_eq!(choice, ExecutorChoice::Local);
        let json = serde_json::json!({"kind": "remote", "host_id": "h1"});
        let choice: ExecutorChoice = serde_json::from_value(json).unwrap();
        assert_eq!(
            choice,
            ExecutorChoice::Remote {
                host_id: "h1".into()
            }
        );
    }

    #[test]
    fn gateway_envelope_round_trips() {
        let env = GatewayEnvelope::wrap(
            GatewayEnvelope::DATA,
            serde_json::json!({"v": 1, "id": "x", "method": "session.ack", "params": {}}),
        );
        let back: GatewayEnvelope =
            serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(back, env);
        assert_eq!(back.channel, 1);
        assert!(back.gw);
    }
}
