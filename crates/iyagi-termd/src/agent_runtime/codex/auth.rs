//! Explicit authentication scope for the production app-server peer. Managed
//! subscription login remains Codex-owned. API keys use official login/start
//! with an ephemeral store in a private per-run CODEX_HOME.

use crate::agent_runtime::RunStart;
use crate::connections::{ConnectionStore, SecretRedactor};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use term_contracts::mission::types::{AuthRoute, RuntimeKind};
use zeroize::Zeroizing;

const API_URL: &str = "https://api.openai.com/v1";
const CHATGPT_URL: &str = "https://chatgpt.com/backend-api/";
const CHATGPT_MODEL_URL: &str = "https://chatgpt.com/backend-api/codex";

#[derive(Clone)]
pub struct AuthScope {
    pub(crate) home: PathBuf,
    pub(crate) ephemeral: bool,
}
impl AuthScope {
    pub fn verify_initialize(&self, result: &Value) -> bool {
        result["codexHome"].as_str().is_some_and(|path| {
            Path::new(path) == self.home
                || std::fs::canonicalize(path)
                    .ok()
                    .zip(std::fs::canonicalize(&self.home).ok())
                    .is_some_and(|(a, b)| a == b)
        })
    }
    pub fn verify_config(&self, config: &Value) -> bool {
        config["model_provider"] == "openai"
            && config["openai_base_url"]
                == if self.ephemeral {
                    API_URL
                } else {
                    CHATGPT_MODEL_URL
                }
            && config["chatgpt_base_url"] == CHATGPT_URL
            && config["model_providers"]
                .as_object()
                .is_some_and(|p| p.is_empty())
            && config["mcp_servers"].is_object()
            && [
                "apps",
                "plugins",
                "hooks",
                "multi_agent",
                "multi_agent_v2",
                "shell_snapshot",
                "browser_use",
                "computer_use",
                "in_app_browser",
            ]
            .iter()
            .all(|key| config["features"][key] == false)
            && config["notify"].as_array().is_some_and(|v| v.is_empty())
            && config["shell_environment_policy"]["experimental_use_profile"] == false
            && config["allow_login_shell"] == false
            && if self.ephemeral {
                config["cli_auth_credentials_store"] == "ephemeral"
                    && config["forced_login_method"] == "api"
            } else {
                config["forced_login_method"].is_null()
                    || config["forced_login_method"] == "chatgpt"
            }
    }

    /// Empty table overrides merge with managed-login config. Disable each
    /// effective MCP entry on the new thread instead of assuming `{}` clears it.
    pub(crate) fn thread_config(config: &Value, allow_network: bool) -> Value {
        let mut servers = serde_json::Map::new();
        if let Some(current) = config["mcp_servers"].as_object() {
            for key in current.keys() {
                servers.insert(key.clone(), serde_json::json!({"enabled":false}));
            }
        }
        let mut result = serde_json::json!({"mcp_servers":servers});
        if !allow_network {
            result["web_search"] = serde_json::json!("disabled");
        }
        result
    }
}

pub(crate) type PrivateDirectory = Arc<Mutex<Option<tempfile::TempDir>>>;

pub(crate) struct AuthLaunch {
    pub argv: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub scope: AuthScope,
    pub key: Option<Zeroizing<String>>,
    pub redactor: Option<Arc<SecretRedactor>>,
    pub private: Option<PrivateDirectory>,
}
impl AuthLaunch {
    pub fn prepare(
        run: &RunStart,
        connections: Option<&ConnectionStore>,
        root: &Path,
    ) -> io::Result<Self> {
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
        if run.binding.runtime != RuntimeKind::Codex || run.binding.provider_id != "openai" {
            return Err(invalid(
                "Codex authentication requires the supported OpenAI provider",
            ));
        }
        let mut environment = BTreeMap::new();
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
                environment.insert(name.into(), value);
            }
        }
        // Root flags must precede the subcommand: the isolation prefix keeps
        // this workload off the user's shared codex background daemon, so a
        // force stop can never take that daemon down with the run.
        let mut argv = super::daemon_isolation::argv_prefix(Path::new(&run.binding.program));
        argv.extend(
            [
                "app-server",
                "-c",
                "model_provider=\"openai\"",
                "-c",
                "model_providers={}",
                "-c",
                "chatgpt_base_url=\"https://chatgpt.com/backend-api/\"",
                "-c",
                "mcp_servers={}",
                "-c",
                "features.apps=false",
                "-c",
                "features.plugins=false",
                "-c",
                "features.hooks=false",
                "-c",
                "features.multi_agent=false",
                "-c",
                "features.multi_agent_v2=false",
                "-c",
                "features.shell_snapshot=false",
                "-c",
                "features.browser_use=false",
                "-c",
                "features.computer_use=false",
                "-c",
                "features.in_app_browser=false",
                "-c",
                "notify=[]",
                "-c",
                "allow_login_shell=false",
                "-c",
                "shell_environment_policy.experimental_use_profile=false",
                // Repository AGENTS.md discovery stops at a zero byte budget
                // (10 §5); the thread's instructionSources confirm the effect.
                "-c",
                "project_doc_max_bytes=0",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        // The built-in provider override is also used for managed-login model
        // requests. ChatGPT OAuth credentials must not be sent to the API-key
        // endpoint, even though account/read itself can still succeed.
        let model_url = if run.binding.auth_route == AuthRoute::Subscription {
            CHATGPT_MODEL_URL
        } else {
            API_URL
        };
        argv.extend(["-c".into(), format!("openai_base_url=\"{model_url}\"")]);
        let (scope, key, redactor, private) = match run.binding.auth_route {
            AuthRoute::ApiKey => {
                let resolved = connections
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "Codex connection store unavailable",
                        )
                    })?
                    .resolve_codex(&run.binding)?;
                crate::connections::private_directory(root)?;
                let private = tempfile::Builder::new().prefix("run-").tempdir_in(root)?;
                let home = private.path().join("codex");
                crate::connections::private_directory(&home)?;
                for (name, sub) in [
                    ("HOME", "home"),
                    ("USERPROFILE", "home"),
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
                    environment.insert(name.into(), path.to_string_lossy().into_owned());
                }
                argv.extend(
                    [
                        "-c",
                        "cli_auth_credentials_store=\"ephemeral\"",
                        "-c",
                        "forced_login_method=\"api\"",
                    ]
                    .into_iter()
                    .map(str::to_owned),
                );
                let redactor = Some(resolved.redactor());
                (
                    AuthScope {
                        home,
                        ephemeral: true,
                    },
                    Some(resolved.into_api_key()),
                    redactor,
                    Some(Arc::new(Mutex::new(Some(private)))),
                )
            }
            AuthRoute::Subscription => {
                if run.binding.credential_ref.is_some() || run.binding.endpoint_ref.is_some() {
                    return Err(invalid("Codex subscription uses its managed login; API connection references are not accepted"));
                }
                let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
                let user_home = std::env::var(home_key)
                    .map_err(|_| invalid("Codex managed login home is unavailable"))?;
                let home = std::env::var_os("CODEX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| Path::new(&user_home).join(".codex"));
                if !home.is_absolute() {
                    return Err(invalid("Codex managed login home must be absolute"));
                }
                // Keep the official shared credential/cache location. We do
                // not copy OAuth tokens or change the user's login method.
                environment.insert(home_key.into(), user_home);
                if let Ok(value) = std::env::var(if cfg!(windows) { "HOME" } else { "USERPROFILE" })
                {
                    environment.insert(
                        if cfg!(windows) { "HOME" } else { "USERPROFILE" }.into(),
                        value,
                    );
                }
                (
                    AuthScope {
                        home,
                        ephemeral: false,
                    },
                    None,
                    None,
                    None,
                )
            }
            _ => {
                return Err(invalid(
                    "Codex local and custom authentication are not connected",
                ))
            }
        };
        environment.insert(
            "CODEX_HOME".into(),
            scope.home.to_string_lossy().into_owned(),
        );
        Ok(Self {
            argv,
            environment,
            scope,
            key,
            redactor,
            private,
        })
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
