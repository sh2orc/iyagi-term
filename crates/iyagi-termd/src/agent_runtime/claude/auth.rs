//! Authentication is verified over the running CLI's initialization control
//! response before its single user prompt is sent. See the pinned metadata
//! evidence and the official SDK control protocol referenced in the guide.
use super::{invalid_input, permission_denied, LaunchPlan};
use crate::agent_runtime::{RunStart, WorkspaceAccess};
use crate::connections::ConnectionStore;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use term_contracts::mission::types::{AuthRoute, RuntimeKind};

pub(crate) type PrivateDirectory = Arc<Mutex<Option<tempfile::TempDir>>>;

#[derive(Clone, Copy)]
enum Source {
    ApiKey,
    SubscriptionToken,
    ZaiToken,
    ManagedSubscription,
}

#[derive(Clone)]
pub struct AuthScope {
    source: Source,
    permission: &'static str,
}
impl AuthScope {
    pub fn verify_account(&self, result: &Value) -> bool {
        let account = &result["account"];
        account["apiProvider"] == "firstParty"
            && match self.source {
                Source::ApiKey => {
                    account["apiKeySource"] == "ANTHROPIC_API_KEY"
                        && matches!(account["tokenSource"].as_str(), None | Some("none"))
                }
                Source::SubscriptionToken => {
                    account["tokenSource"] == "CLAUDE_CODE_OAUTH_TOKEN"
                        && account["apiKeySource"].is_null()
                }
                Source::ZaiToken => {
                    account["tokenSource"] == "ANTHROPIC_AUTH_TOKEN"
                        && account["apiKeySource"].is_null()
                }
                Source::ManagedSubscription => {
                    account["tokenSource"].is_null()
                        && account["apiKeySource"].is_null()
                        && matches!(
                            account["subscriptionType"].as_str(),
                            Some("pro" | "max" | "team" | "enterprise")
                        )
                }
            }
    }
    pub fn verify_permissions(&self, result: &Value) -> bool {
        result["current_permission_mode"] == self.permission
    }
}

pub(crate) fn configure(
    run: &RunStart,
    plan: &mut LaunchPlan,
    connections: Option<&ConnectionStore>,
    secrets_root: Option<&Path>,
    root: &Path,
) -> std::io::Result<()> {
    if run.binding.runtime != RuntimeKind::Claude
        || !matches!(
            run.binding.provider_id.as_str(),
            "anthropic" | "zai-coding-plan"
        )
    {
        return Err(invalid_input(
            "Claude authentication requires a supported provider",
        ));
    }
    if plan.resume_session.is_some() {
        return Err(invalid_input(
            "Claude authenticated runs do not yet resume private sessions",
        ));
    }
    let mut env = BTreeMap::new();
    for name in [
        "PATH",
        "LANG",
        "LC_ALL",
        "TZ",
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
    ] {
        if let Ok(value) = std::env::var(name) {
            env.insert(name.to_owned(), value);
        }
    }
    let managed = run.binding.auth_route == AuthRoute::Subscription
        && run.binding.provider_id == "anthropic"
        && run.binding.credential_ref.is_none()
        && run.binding.endpoint_ref.is_none();
    let mut base_url = "https://api.anthropic.com".to_owned();
    let source = if managed {
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let home = std::env::var(home_key)
            .map_err(|_| invalid_input("Claude managed login home is unavailable"))?;
        let config = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(&home).join(".claude"));
        if !config.is_absolute() {
            return Err(invalid_input("Claude managed config path must be absolute"));
        }
        env.insert(home_key.into(), home);
        env.insert(
            "CLAUDE_CONFIG_DIR".into(),
            config.to_string_lossy().into_owned(),
        );
        plan.config_dir = config;
        Source::ManagedSubscription
    } else {
        if !matches!(
            run.binding.auth_route,
            AuthRoute::ApiKey | AuthRoute::Subscription
        ) {
            return Err(invalid_input(
                "Claude local and custom authentication are not connected",
            ));
        }
        // Z.ai Coding Plan can also launch on the daemon's own key store — the
        // same credential `claude-exec --provider zai` (the `ccg` launch
        // profile) hands the terminal CLI. Read at start time like the pane
        // path: a key removed while queued fails here with its reason code.
        // Stored-key connections keep serving every other route.
        let managed_zai = run.binding.provider_id == "zai-coding-plan"
            && run.binding.auth_route == AuthRoute::Subscription
            && run.binding.credential_ref.is_none()
            && run.binding.endpoint_ref.is_none();
        let (key, redactor, base, zai_keystore) = if managed_zai {
            let secrets_root = secrets_root.ok_or_else(|| {
                permission_denied("Claude key store is unavailable for Z.ai Coding Plan")
            })?;
            let token =
                crate::claude_provider::StartToken::read(secrets_root).map_err(|error| {
                    permission_denied(format!(
                        "Z.ai Coding Plan key: {}",
                        crate::claude_provider::reason_code(&error).unwrap_or("unavailable")
                    ))
                })?;
            let redactor = Arc::clone(&token.redactor);
            let key = token.token.clone();
            drop(token);
            (
                key,
                Some(redactor),
                crate::connections::CLAUDE_ZAI_BASE_URL.to_owned(),
                true,
            )
        } else {
            let resolved = connections
                .ok_or_else(|| permission_denied("Claude connection store unavailable"))?
                .resolve_claude(&run.binding)?;
            let base = resolved.info.base_url.clone();
            let redactor = resolved.redactor();
            let key = resolved.into_api_key();
            (key, Some(redactor), base, false)
        };
        base_url = base;
        crate::connections::private_directory(root)?;
        let private = tempfile::Builder::new().prefix("run-").tempdir_in(root)?;
        for (name, sub) in [
            ("HOME", "home"),
            ("USERPROFILE", "home"),
            ("CLAUDE_CONFIG_DIR", "claude"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
            ("TMPDIR", "tmp"),
            ("TMP", "tmp"),
            ("TEMP", "tmp"),
        ] {
            let path = private.path().join(sub);
            crate::connections::private_directory(&path)?;
            env.insert(name.into(), path.to_string_lossy().into_owned());
        }
        plan.config_dir = private.path().join("claude");
        plan.private = Some(Arc::new(Mutex::new(Some(private))));
        plan.redactor = redactor;
        let source = if zai_keystore || run.binding.provider_id == "zai-coding-plan" {
            Source::ZaiToken
        } else if run.binding.auth_route == AuthRoute::ApiKey {
            Source::ApiKey
        } else {
            Source::SubscriptionToken
        };
        env.insert(
            match source {
                Source::ApiKey => "ANTHROPIC_API_KEY",
                Source::ZaiToken => "ANTHROPIC_AUTH_TOKEN",
                _ => "CLAUDE_CODE_OAUTH_TOKEN",
            }
            .into(),
            key.to_string(),
        );
        source
    };
    // Host-owned routing prevents file settings from replacing the selected
    // provider, endpoint or credential. CLI/managed permission policy still
    // applies. No ambient auth, provider, proxy or telemetry vars survive.
    for (name, value) in [
        ("CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST", "1"),
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
        ("DISABLE_AUTOUPDATER", "1"),
    ] {
        env.insert(name.into(), value.into());
    }
    // The Z.ai route gets the tuning the terminal's `ccg` profile injects, from
    // the same constants (`claude_provider`): a mission on this provider must
    // not run with a different timeout or compaction point than a pane on it.
    // The timeout is a property of the route; the compaction window belongs to
    // the model, so it is set only for an id that declares the 1M context —
    // forcing 1M on a model that does not have it compacts too late to recover.
    if run.binding.provider_id == "zai-coding-plan" {
        let (name, value) = crate::claude_provider::ZAI_API_TIMEOUT_MS;
        env.insert(name.into(), value.into());
        if run.binding.model_id.trim().ends_with("[1m]") {
            let (name, value) = crate::claude_provider::ZAI_AUTO_COMPACT_WINDOW;
            env.insert(name.into(), value.into());
        }
    }
    env.insert("ANTHROPIC_BASE_URL".into(), base_url);
    let permission = match run.workspace_access {
        WorkspaceAccess::ReadOnly => "plan",
        WorkspaceAccess::Write => "acceptEdits",
    };
    plan.argv.extend(
        [
            "--input-format",
            "stream-json",
            "--safe-mode",
            "--restricted",
            "--strict-mcp-config",
            "--setting-sources",
            "",
            "--no-session-persistence",
            "--permission-mode",
            permission,
            "--tools",
            if run.workspace_access == WorkspaceAccess::Write {
                "Read,Glob,Grep,Edit,Write"
            } else {
                "Read,Glob,Grep"
            },
            "--json-schema",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    plan.argv
        .push(crate::agent_runtime::codex::task_result_output_schema(run.task_kind).to_string());
    super::guard_argv(&plan.argv)?;
    plan.env = env;
    plan.env_clear = true;
    plan.auth_scope = Some(AuthScope { source, permission });
    Ok(())
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
