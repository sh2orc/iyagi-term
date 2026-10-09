//! Read-only discovery of locally installed agent CLIs for first-run mission
//! setup (`runtime.detect`).
//!
//! Nothing here stores state. Login markers are checked for presence (or a
//! top-level key) only; no credential or account content leaves this module
//! and none is logged. File-system and environment inputs arrive through
//! [`DetectionEnv`] so every lookup is testable with temporary directories.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use term_contracts::ids::U64String;
use term_contracts::launch::{Enforcement, LaunchPolicy};
use term_contracts::mission::rpc::{InstallationStatus, LoginHint};
use term_contracts::mission::types::{AuthRoute, Binding, Id, Role, RuntimeKind};

use super::capability_evidence;
use super::installation::{Installation, ProbeFailure};

/// Wire order of `RuntimeDetectResult.runtimes`.
pub const RUNTIMES: [RuntimeKind; 3] = [
    RuntimeKind::Codex,
    RuntimeKind::Claude,
    RuntimeKind::Opencode,
];

/// Roles the one-click team setup assigns, in display order.
pub const SETUP_ROLES: [Role; 4] = [Role::Lead, Role::Builder, Role::Reviewer, Role::Integrator];

/// `~/.claude.json` also holds project history; larger files are "unknown".
const CLAUDE_GLOBAL_READ_LIMIT: u64 = 32 * 1024 * 1024;
const SETTINGS_READ_LIMIT: u64 = 1024 * 1024;
const MODEL_ID_MAX_BYTES: usize = 128;

/// Home-relative install directories common to npm/bun/volta/cargo/native
/// installers. GUI-launched daemons often inherit a short PATH without them.
const HOME_BIN_DIRS: [&[&str]; 5] = [
    &[".local", "bin"],
    &[".npm-global", "bin"],
    &[".bun", "bin"],
    &[".volta", "bin"],
    &[".cargo", "bin"],
];

/// Process inputs of one detection pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectionEnv {
    /// Raw PATH value of the daemon process.
    pub path: Option<OsString>,
    pub home: Option<PathBuf>,
    /// `$CODEX_HOME` when set.
    pub codex_home: Option<PathBuf>,
    /// `$CLAUDE_CONFIG_DIR` when set.
    pub claude_config_dir: Option<PathBuf>,
    /// `%APPDATA%` (Windows npm global prefix parent).
    pub appdata: Option<PathBuf>,
    /// Fixed package-manager directories searched after PATH.
    pub system_dirs: Vec<PathBuf>,
}

impl DetectionEnv {
    pub fn current() -> Self {
        fn var(name: &str) -> Option<OsString> {
            std::env::var_os(name).filter(|value| !value.is_empty())
        }
        let home = if cfg!(windows) {
            var("USERPROFILE").or_else(|| var("HOME"))
        } else {
            var("HOME").or_else(|| var("USERPROFILE"))
        };
        DetectionEnv {
            path: var("PATH"),
            home: home.map(PathBuf::from),
            codex_home: var("CODEX_HOME").map(PathBuf::from),
            claude_config_dir: var("CLAUDE_CONFIG_DIR").map(PathBuf::from),
            appdata: if cfg!(windows) {
                var("APPDATA").map(PathBuf::from)
            } else {
                None
            },
            system_dirs: if cfg!(windows) {
                Vec::new()
            } else {
                vec![
                    PathBuf::from("/opt/homebrew/bin"),
                    PathBuf::from("/usr/local/bin"),
                ]
            },
        }
    }

    fn home_dir(&self) -> Option<&Path> {
        self.home.as_deref().filter(|home| home.is_absolute())
    }

    /// An explicit override wins even when unusable (relative): the CLI
    /// would not read the home fallback either, so the answer is unknown.
    fn override_or_home(&self, explicit: Option<&Path>, fallback: &str) -> Option<PathBuf> {
        match explicit {
            Some(dir) => dir.is_absolute().then(|| dir.to_path_buf()),
            None => self.home_dir().map(|home| home.join(fallback)),
        }
    }

    fn codex_dir(&self) -> Option<PathBuf> {
        self.override_or_home(self.codex_home.as_deref(), ".codex")
    }

    /// Also read outside detection: the model-picker hints for Claude Code
    /// come from this directory (it has no listing API).
    pub fn claude_dir(&self) -> Option<PathBuf> {
        self.override_or_home(self.claude_config_dir.as_deref(), ".claude")
    }

    /// Claude Code keeps its global state in `$CLAUDE_CONFIG_DIR/.claude.json`
    /// when the override is set, otherwise in `~/.claude.json`.
    fn claude_global_config(&self) -> Option<PathBuf> {
        match self.claude_config_dir.as_deref() {
            Some(dir) => dir.is_absolute().then(|| dir.join(".claude.json")),
            None => self.home_dir().map(|home| home.join(".claude.json")),
        }
    }
}

/// Everything one runtime reports before capability evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedFacts {
    pub runtime: RuntimeKind,
    /// Absolute executable path; `None` when nothing was found.
    pub program: Option<String>,
    pub version: Option<String>,
    pub installation: InstallationStatus,
    pub login: LoginHint,
    pub configured_model_id: Option<String>,
}

/// Local version lookup (`installation::observe` shape without identity pinning).
pub type VersionProbe<'a> =
    dyn Fn(&str, RuntimeKind) -> Result<Installation, ProbeFailure> + Sync + 'a;

/// Detect every runtime in [`RUNTIMES`] order. Version probes run in parallel
/// so the worst case is one probe deadline, not three.
pub fn detect_all(env: &DetectionEnv, probe: &VersionProbe<'_>) -> Vec<DetectedFacts> {
    std::thread::scope(|scope| {
        let workers: Vec<_> = RUNTIMES
            .into_iter()
            .map(|runtime| {
                let worker = std::thread::Builder::new()
                    .name("runtime-detect".into())
                    .spawn_scoped(scope, move || detect_one(runtime, env, probe));
                (runtime, worker)
            })
            .collect();
        workers
            .into_iter()
            .map(|(runtime, worker)| match worker {
                Ok(worker) => worker.join().unwrap_or(DetectedFacts {
                    runtime,
                    program: None,
                    version: None,
                    installation: InstallationStatus::Failed,
                    login: LoginHint::Unknown,
                    configured_model_id: None,
                }),
                // Thread exhaustion degrades to a sequential lookup.
                Err(_) => detect_one(runtime, env, probe),
            })
            .collect()
    })
}

pub fn detect_one(
    runtime: RuntimeKind,
    env: &DetectionEnv,
    probe: &VersionProbe<'_>,
) -> DetectedFacts {
    let login = login_hint(runtime, env);
    let configured_model_id = configured_model_id(runtime, env);
    let observed = find_program(runtime, env).map(|program| {
        let result = probe(&program, runtime);
        (program, result)
    });
    let (program, version, installation) = match observed {
        None => (None, None, InstallationStatus::NotFound),
        Some((program, Ok(found))) => (
            Some(program),
            Some(found.version),
            InstallationStatus::Verified,
        ),
        // Removed between lookup and probe: report it like a missing CLI.
        Some((_, Err(ProbeFailure::NotFound))) => (None, None, InstallationStatus::NotFound),
        Some((program, Err(failure))) => (Some(program), None, failure.installation_status()),
    };
    DetectedFacts {
        runtime,
        program,
        version,
        installation,
        login,
        configured_model_id,
    }
}

// ---- executable lookup -----------------------------------------------------

fn program_names(runtime: RuntimeKind) -> Vec<String> {
    let stem = match runtime {
        RuntimeKind::Codex => "codex",
        RuntimeKind::Claude => "claude",
        RuntimeKind::Opencode => "opencode",
        RuntimeKind::Fake => return Vec::new(),
    };
    if cfg!(windows) {
        vec![format!("{stem}.exe"), format!("{stem}.cmd")]
    } else {
        vec![stem.to_owned()]
    }
}

fn under(base: &Path, parts: &[&str]) -> PathBuf {
    parts
        .iter()
        .fold(base.to_path_buf(), |dir, part| dir.join(part))
}

/// PATH entries first, then fixed package-manager and per-user install
/// directories. Relative entries are dropped: `program` must be absolute.
pub fn search_dirs(runtime: RuntimeKind, env: &DetectionEnv) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env
        .path
        .as_deref()
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    dirs.extend(env.system_dirs.iter().cloned());
    if let Some(home) = env.home_dir() {
        dirs.extend(HOME_BIN_DIRS.iter().map(|parts| under(home, parts)));
        match runtime {
            RuntimeKind::Claude => dirs.push(under(home, &[".claude", "local"])),
            RuntimeKind::Opencode => dirs.push(under(home, &[".opencode", "bin"])),
            RuntimeKind::Codex | RuntimeKind::Fake => {}
        }
    }
    if let Some(appdata) = env.appdata.as_deref() {
        dirs.push(appdata.join("npm"));
    }
    let mut seen = HashSet::new();
    dirs.retain(|dir| dir.is_absolute() && seen.insert(dir.clone()));
    dirs
}

/// Regular file (symlinks followed) that the platform would execute.
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// First executable candidate as an absolute UTF-8 path (not canonicalized,
/// so a package-manager symlink keeps working across upgrades).
pub fn find_program(runtime: RuntimeKind, env: &DetectionEnv) -> Option<String> {
    let names = program_names(runtime);
    search_dirs(runtime, env).iter().find_map(|dir| {
        names
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| is_executable_file(candidate))
            .and_then(|candidate| candidate.to_str().map(str::to_owned))
    })
}

// ---- login markers and configured models ------------------------------------

/// Metadata first: opening a FIFO or device could block the RPC worker.
fn read_regular_file(path: &Path, limit: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

/// Derived struct deserializers also accept JSON arrays; these files are
/// objects, so anything else counts as a parse failure.
fn json_object<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return None;
    }
    serde_json::from_slice(bytes).ok()
}

fn file_presence(path: &Path) -> LoginHint {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => LoginHint::Found,
        Ok(_) => LoginHint::NotFound,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LoginHint::NotFound,
        Err(_) => LoginHint::Unknown,
    }
}

/// Only the presence of `oauthAccount` is decoded; every other field is
/// skipped by serde and the value itself is dropped immediately.
fn claude_login(path: &Path) -> LoginHint {
    #[derive(Deserialize)]
    struct GlobalConfig {
        #[serde(rename = "oauthAccount", default)]
        oauth_account: Option<serde_json::Value>,
    }
    let Some(bytes) = read_regular_file(path, CLAUDE_GLOBAL_READ_LIMIT) else {
        return LoginHint::Unknown;
    };
    match json_object::<GlobalConfig>(&bytes) {
        Some(config) if config.oauth_account.is_some() => LoginHint::Found,
        Some(_) => LoginHint::NotFound,
        None => LoginHint::Unknown,
    }
}

/// Top-level `cli_auth_credentials_store` from Codex `config.toml`, if set.
/// Tables end the top level; basic and literal strings are accepted.
fn codex_credentials_store(text: &str) -> Option<String> {
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            break;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().trim_matches('"') != "cli_auth_credentials_store" {
            continue;
        }
        let value = value.trim();
        let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        return value[1..]
            .split_once(quote)
            .map(|(store, _)| store.to_owned());
    }
    None
}

/// `auth.json` is only meaningful for the file credential store (the default).
/// A keyring/auto store keeps the login elsewhere, so presence proves nothing.
fn codex_login(codex_home: &Path) -> LoginHint {
    let store = read_regular_file(&codex_home.join("config.toml"), SETTINGS_READ_LIMIT)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| codex_credentials_store(&text));
    match store.as_deref() {
        None | Some("file") => file_presence(&codex_home.join("auth.json")),
        Some(_) => LoginHint::Unknown,
    }
}

pub fn login_hint(runtime: RuntimeKind, env: &DetectionEnv) -> LoginHint {
    match runtime {
        RuntimeKind::Codex => env
            .codex_dir()
            .map_or(LoginHint::Unknown, |dir| codex_login(&dir)),
        RuntimeKind::Claude => env
            .claude_global_config()
            .map_or(LoginHint::Unknown, |path| claude_login(&path)),
        // OpenCode uses stored connection references; presence proves nothing.
        RuntimeKind::Opencode | RuntimeKind::Fake => LoginHint::Unknown,
    }
}

fn clean_model_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()
        && trimmed.len() <= MODEL_ID_MAX_BYTES
        && !trimmed.chars().any(char::is_control))
    .then(|| trimmed.to_owned())
}

fn codex_configured_model(codex_home: &Path) -> Option<String> {
    let bytes = read_regular_file(&codex_home.join("config.toml"), SETTINGS_READ_LIMIT)?;
    let text = String::from_utf8(bytes).ok()?;
    // Same parser the model watcher uses: top-level `model`, overridden by
    // the active `profile` table when one is selected.
    let model = crate::agent_model::codex_config_model(&text, None).model?;
    clean_model_id(&model)
}

fn claude_configured_model(claude_dir: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct Settings {
        #[serde(default)]
        model: Option<serde_json::Value>,
    }
    let bytes = read_regular_file(&claude_dir.join("settings.json"), SETTINGS_READ_LIMIT)?;
    let settings: Settings = json_object(&bytes)?;
    clean_model_id(settings.model?.as_str()?)
}

pub fn configured_model_id(runtime: RuntimeKind, env: &DetectionEnv) -> Option<String> {
    match runtime {
        RuntimeKind::Codex => codex_configured_model(&env.codex_dir()?),
        RuntimeKind::Claude => claude_configured_model(&env.claude_dir()?),
        RuntimeKind::Opencode | RuntimeKind::Fake => None,
    }
}

// ---- candidate binding --------------------------------------------------------

/// The unsaved binding the one-click setup would create: managed subscription
/// login, no stored credential or endpoint reference, enabled. Capabilities
/// start unclaimed; the caller applies the service registry.
pub fn candidate_binding(
    runtime: RuntimeKind,
    program: &str,
    provider_id: &str,
    model_id: &str,
    version: &str,
) -> Binding {
    let zero = U64String::new(0).expect("zero fits");
    Binding {
        id: Id::generate(),
        revision: zero.clone(),
        label: String::new(),
        runtime,
        program: program.to_owned(),
        runtime_version: Some(version.to_owned()),
        provider_id: provider_id.to_owned(),
        model_id: model_id.to_owned(),
        effort: None,
        auth_route: AuthRoute::Subscription,
        credential_ref: None,
        endpoint_ref: None,
        capabilities: capability_evidence::unclaimed(),
        checked_at: None,
        enabled: true,
        experimental_version: None,
        local_evidence: None,
        estimated_run_cost_usd_micros: None,
        resource_policy: LaunchPolicy {
            reservation_bytes: zero,
            cpu_slots: 1,
            enforcement: Enforcement::Prefer,
            memory_max_bytes: None,
            cpu_max_cores: None,
            pids_max: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn env(root: &Path) -> DetectionEnv {
        DetectionEnv {
            path: None,
            home: Some(root.join("home")),
            codex_home: None,
            claude_config_dir: None,
            appdata: None,
            system_dirs: Vec::new(),
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[cfg(unix)]
    fn executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        write(path, "#!/bin/sh\n");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn path_entries_win_and_home_install_dirs_are_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env(dir.path());
        let on_path = dir.path().join("path-bin").join("codex");
        let in_home = dir
            .path()
            .join("home")
            .join(".local")
            .join("bin")
            .join("codex");
        executable(&on_path);
        executable(&in_home);
        env.path = Some(std::env::join_paths([dir.path().join("path-bin")]).unwrap());
        assert_eq!(
            find_program(RuntimeKind::Codex, &env).as_deref(),
            on_path.to_str()
        );
        // A GUI-launched daemon without the PATH entry still finds the CLI.
        env.path = None;
        assert_eq!(
            find_program(RuntimeKind::Codex, &env).as_deref(),
            in_home.to_str()
        );
        // Fixed system directories come before per-user fallbacks.
        let system = dir.path().join("system-bin").join("codex");
        executable(&system);
        env.system_dirs = vec![dir.path().join("system-bin")];
        assert_eq!(
            find_program(RuntimeKind::Codex, &env).as_deref(),
            system.to_str()
        );
    }

    #[test]
    #[cfg(unix)]
    fn only_absolute_executable_regular_files_are_candidates() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut env = env(dir.path());
        let plain = dir.path().join("plain").join("codex");
        write(&plain, "not executable");
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::create_dir_all(dir.path().join("dirs").join("codex")).unwrap();
        env.path = Some(
            std::env::join_paths([
                PathBuf::from("relative-bin"),
                dir.path().join("plain"),
                dir.path().join("dirs"),
            ])
            .unwrap(),
        );
        assert_eq!(find_program(RuntimeKind::Codex, &env), None);
        assert!(search_dirs(RuntimeKind::Codex, &env)
            .iter()
            .all(|dir| dir.is_absolute()));

        // Package managers install symlinks; they are accepted as-is.
        let target = dir.path().join("store").join("codex.js");
        executable(&target);
        std::fs::create_dir_all(dir.path().join("links")).unwrap();
        let link = dir.path().join("links").join("codex");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        env.path = Some(std::env::join_paths([dir.path().join("links")]).unwrap());
        assert_eq!(
            find_program(RuntimeKind::Codex, &env).as_deref(),
            link.to_str()
        );
    }

    #[test]
    #[cfg(unix)]
    fn runtime_specific_install_dirs_do_not_leak_across_runtimes() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let home = dir.path().join("home");
        let claude = home.join(".claude").join("local").join("claude");
        let opencode = home.join(".opencode").join("bin").join("opencode");
        executable(&claude);
        executable(&opencode);
        executable(&home.join(".claude").join("local").join("codex"));
        assert_eq!(
            find_program(RuntimeKind::Claude, &env).as_deref(),
            claude.to_str()
        );
        assert_eq!(
            find_program(RuntimeKind::Opencode, &env).as_deref(),
            opencode.to_str()
        );
        assert_eq!(find_program(RuntimeKind::Codex, &env), None);
        assert_eq!(find_program(RuntimeKind::Fake, &env), None);
    }

    #[test]
    fn codex_model_and_login_follow_codex_home() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env(dir.path());
        let default_home = dir.path().join("home").join(".codex");
        assert_eq!(login_hint(RuntimeKind::Codex, &env), LoginHint::NotFound);
        assert_eq!(configured_model_id(RuntimeKind::Codex, &env), None);

        write(&default_home.join("auth.json"), "{}");
        write(
            &default_home.join("config.toml"),
            "# comment\nmodel = \"gpt-top\" # trailing\n\n[profiles.fast]\nmodel = \"gpt-profile\"\n",
        );
        assert_eq!(login_hint(RuntimeKind::Codex, &env), LoginHint::Found);
        assert_eq!(
            configured_model_id(RuntimeKind::Codex, &env).as_deref(),
            Some("gpt-top")
        );

        // A model that only exists inside a table is not the default.
        write(
            &default_home.join("config.toml"),
            "[model_providers.x]\nmodel = \"inside-table\"\n",
        );
        assert_eq!(configured_model_id(RuntimeKind::Codex, &env), None);

        // Keyring/auto credential stores do not use auth.json: unknown either way.
        write(
            &default_home.join("config.toml"),
            "cli_auth_credentials_store = \"keyring\"\n",
        );
        assert_eq!(login_hint(RuntimeKind::Codex, &env), LoginHint::Unknown);
        write(
            &default_home.join("config.toml"),
            "cli_auth_credentials_store = 'file' # explicit default\n",
        );
        assert_eq!(login_hint(RuntimeKind::Codex, &env), LoginHint::Found);
        write(
            &default_home.join("config.toml"),
            "[profiles.x]\ncli_auth_credentials_store = \"keyring\"\n",
        );
        assert_eq!(login_hint(RuntimeKind::Codex, &env), LoginHint::Found);

        let custom = dir.path().join("custom-codex");
        std::fs::create_dir_all(&custom).unwrap();
        env.codex_home = Some(custom.clone());
        assert_eq!(login_hint(RuntimeKind::Codex, &env), LoginHint::NotFound);
        write(&custom.join("config.toml"), "model = 'gpt-custom'\n");
        assert_eq!(
            configured_model_id(RuntimeKind::Codex, &env).as_deref(),
            Some("gpt-custom")
        );

        // An unusable override is unknown, never silently the home default.
        env.codex_home = Some(PathBuf::from("relative-codex"));
        assert_eq!(login_hint(RuntimeKind::Codex, &env), LoginHint::Unknown);
        assert_eq!(configured_model_id(RuntimeKind::Codex, &env), None);
    }

    #[test]
    fn claude_login_is_the_oauth_account_key_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env(dir.path());
        let global = dir.path().join("home").join(".claude.json");
        assert_eq!(login_hint(RuntimeKind::Claude, &env), LoginHint::Unknown);
        for (text, want) in [
            ("{\"projects\":{}}", LoginHint::NotFound),
            ("{\"oauthAccount\":null}", LoginHint::NotFound),
            (
                "{\"oauthAccount\":{\"accountUuid\":\"fixture\"}}",
                LoginHint::Found,
            ),
            ("{\"oauthAccount\":", LoginHint::Unknown),
            ("[]", LoginHint::Unknown),
        ] {
            write(&global, text);
            assert_eq!(login_hint(RuntimeKind::Claude, &env), want, "{text}");
        }

        // With CLAUDE_CONFIG_DIR the home file is not consulted.
        let config = dir.path().join("claude-config");
        env.claude_config_dir = Some(config.clone());
        assert_eq!(login_hint(RuntimeKind::Claude, &env), LoginHint::Unknown);
        write(&config.join(".claude.json"), "{}");
        assert_eq!(login_hint(RuntimeKind::Claude, &env), LoginHint::NotFound);

        assert_eq!(login_hint(RuntimeKind::Opencode, &env), LoginHint::Unknown);
    }

    #[test]
    fn claude_model_is_a_string_in_settings_json() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env(dir.path());
        let settings = dir
            .path()
            .join("home")
            .join(".claude")
            .join("settings.json");
        assert_eq!(configured_model_id(RuntimeKind::Claude, &env), None);
        write(&settings, "{\"model\":\" opus \",\"hooks\":{}}");
        assert_eq!(
            configured_model_id(RuntimeKind::Claude, &env).as_deref(),
            Some("opus")
        );
        for text in [
            "{\"model\":42}",
            "{\"model\":\"\"}",
            "not json",
            "[\"opus\"]",
        ] {
            write(&settings, text);
            assert_eq!(
                configured_model_id(RuntimeKind::Claude, &env),
                None,
                "{text}"
            );
        }
        let config = dir.path().join("claude-config");
        write(&config.join("settings.json"), "{\"model\":\"sonnet\"}");
        env.claude_config_dir = Some(config);
        assert_eq!(
            configured_model_id(RuntimeKind::Claude, &env).as_deref(),
            Some("sonnet")
        );
        assert_eq!(configured_model_id(RuntimeKind::Opencode, &env), None);
    }

    #[test]
    fn missing_programs_are_not_found_in_wire_order_without_probing() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let probes = AtomicUsize::new(0);
        let facts = detect_all(&env, &|_: &str, _: RuntimeKind| {
            probes.fetch_add(1, Ordering::SeqCst);
            Err(ProbeFailure::Failed)
        });
        assert_eq!(probes.load(Ordering::SeqCst), 0);
        assert_eq!(
            facts.iter().map(|f| f.runtime).collect::<Vec<_>>(),
            RUNTIMES.to_vec()
        );
        for fact in facts {
            assert_eq!(fact.program, None);
            assert_eq!(fact.version, None);
            assert_eq!(fact.installation, InstallationStatus::NotFound);
        }
    }

    #[test]
    #[cfg(unix)]
    fn probe_outcomes_map_to_installation_status() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env(dir.path());
        let bin = dir.path().join("bin");
        for name in ["codex", "claude", "opencode"] {
            executable(&bin.join(name));
        }
        env.path = Some(std::env::join_paths([&bin]).unwrap());
        let facts = detect_all(&env, &|program: &str, runtime: RuntimeKind| {
            assert!(Path::new(program).is_absolute());
            match runtime {
                RuntimeKind::Codex => Ok(Installation {
                    version: "0.154.0".into(),
                    executable: None,
                }),
                RuntimeKind::Claude => Err(ProbeFailure::TimedOut),
                _ => Err(ProbeFailure::NotFound),
            }
        });
        assert_eq!(facts[0].program.as_deref(), bin.join("codex").to_str());
        assert_eq!(facts[0].version.as_deref(), Some("0.154.0"));
        assert_eq!(facts[0].installation, InstallationStatus::Verified);
        assert_eq!(facts[1].program.as_deref(), bin.join("claude").to_str());
        assert_eq!(facts[1].version, None);
        assert_eq!(facts[1].installation, InstallationStatus::TimedOut);
        assert_eq!(facts[2].program, None);
        assert_eq!(facts[2].installation, InstallationStatus::NotFound);
    }

    #[test]
    fn candidate_binding_is_a_managed_subscription_route() {
        let binding = candidate_binding(
            RuntimeKind::Codex,
            "/opt/homebrew/bin/codex",
            "openai",
            "gpt-5.6-luna",
            "0.154.0",
        );
        assert_eq!(binding.auth_route, AuthRoute::Subscription);
        assert!(binding.credential_ref.is_none() && binding.endpoint_ref.is_none());
        assert!(binding.enabled);
        assert_eq!(binding.runtime_version.as_deref(), Some("0.154.0"));
        assert_eq!(binding.capabilities, capability_evidence::unclaimed());
    }
}
