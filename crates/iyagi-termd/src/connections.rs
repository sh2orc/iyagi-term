//! Daemon-owned, immutable provider connections. Only opaque refs and public
//! endpoint metadata leave this module. API keys are provisioned locally on
//! stdin into the OS credential store, never through the mission RPC.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use term_contracts::mission::types::{AuthRoute, Binding, Id, RuntimeKind};
use zeroize::Zeroizing;

const MAX_KEY: usize = 4096;
const MAX_METADATA: u64 = 8192;

/// Z.ai Coding Plan의 Anthropic 호환 엔드포인트. 미션 바인딩 프리셋
/// (`ConnectionPreset::ClaudeZaiCoding`)과 터미널 `claude_provider` 라우팅
/// (`ANTHROPIC_BASE_URL`)이 같은 값을 쓴다.
pub const CLAUDE_ZAI_BASE_URL: &str = "https://api.z.ai/api/anthropic";

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum ConnectionPreset {
    OpenaiApi,
    AnthropicApi,
    ZaiApi,
    ZaiCoding,
    CodexApi,
    ClaudeApi,
    ClaudeSubscription,
    ClaudeZaiCoding,
}
impl ConnectionPreset {
    fn runtime(self) -> RuntimeKind {
        match self {
            Self::CodexApi => RuntimeKind::Codex,
            Self::ClaudeApi | Self::ClaudeSubscription | Self::ClaudeZaiCoding => {
                RuntimeKind::Claude
            }
            _ => RuntimeKind::Opencode,
        }
    }
    fn values(self) -> (&'static str, AuthRoute, &'static str) {
        match self {
            Self::OpenaiApi | Self::CodexApi => {
                ("openai", AuthRoute::ApiKey, "https://api.openai.com/v1")
            }
            Self::AnthropicApi => (
                "anthropic",
                AuthRoute::ApiKey,
                "https://api.anthropic.com/v1",
            ),
            Self::ClaudeApi => ("anthropic", AuthRoute::ApiKey, "https://api.anthropic.com"),
            Self::ClaudeSubscription => (
                "anthropic",
                AuthRoute::Subscription,
                "https://api.anthropic.com",
            ),
            Self::ClaudeZaiCoding => (
                "zai-coding-plan",
                AuthRoute::Subscription,
                CLAUDE_ZAI_BASE_URL,
            ),
            Self::ZaiApi => ("zai", AuthRoute::ApiKey, "https://api.z.ai/api/paas/v4"),
            Self::ZaiCoding => (
                "zai-coding-plan",
                AuthRoute::Subscription,
                "https://api.z.ai/api/coding/paas/v4",
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionInfo {
    pub endpoint_ref: Id,
    pub credential_ref: String,
    pub runtime: RuntimeKind,
    pub provider_id: String,
    pub auth_route: AuthRoute,
    pub base_url: String,
}
impl ConnectionInfo {
    fn validate(&self) -> io::Result<()> {
        if credential_id(&self.credential_ref).ok().as_ref() != Some(&self.endpoint_ref) {
            return Err(invalid("unsupported connection metadata"));
        }
        let valid = [
            ConnectionPreset::OpenaiApi,
            ConnectionPreset::AnthropicApi,
            ConnectionPreset::ZaiApi,
            ConnectionPreset::ZaiCoding,
            ConnectionPreset::CodexApi,
            ConnectionPreset::ClaudeApi,
            ConnectionPreset::ClaudeSubscription,
            ConnectionPreset::ClaudeZaiCoding,
        ]
        .iter()
        .any(|p| {
            let (provider, route, url) = p.values();
            self.runtime == p.runtime()
                && self.provider_id == provider
                && self.auth_route == route
                && self.base_url == url
        });
        if !valid {
            return Err(invalid(
                "provider, authentication route and endpoint do not match a supported connection",
            ));
        }
        Ok(())
    }
}

/// Test backends can be injected without reading the developer's keychain.
/// Implementations must return normalized errors, never OS diagnostic text.
pub trait CredentialStore: Send + Sync {
    fn put(&self, account: &str, value: &str) -> io::Result<()>;
    fn get(&self, account: &str) -> io::Result<Zeroizing<String>>;
    fn delete(&self, account: &str) -> io::Result<()>;
}

pub struct OsCredentialStore {
    service: String,
}
impl OsCredentialStore {
    pub fn for_directory(root: &Path) -> io::Result<Self> {
        let canonical = std::fs::canonicalize(root)
            .map_err(|_| invalid("connection directory is unavailable"))?;
        Ok(Self {
            service: format!(
                "com.iyagi.connections.{:x}",
                Sha256::digest(canonical.to_string_lossy().as_bytes())
            ),
        })
    }
    fn entry(&self, account: &str) -> io::Result<keyring::v1::Entry> {
        keyring::v1::Entry::new(&self.service, account).map_err(|_| unavailable())
    }
}
impl CredentialStore for OsCredentialStore {
    fn put(&self, account: &str, value: &str) -> io::Result<()> {
        self.entry(account)?
            .set_password(value)
            .map_err(|_| unavailable())
    }
    fn get(&self, account: &str) -> io::Result<Zeroizing<String>> {
        self.entry(account)?
            .get_password()
            .map(Zeroizing::new)
            .map_err(|_| unavailable())
    }
    fn delete(&self, account: &str) -> io::Result<()> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::v1::Error::NoEntry) => Ok(()),
            Err(_) => Err(unavailable()),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCredential {
    connection: ConnectionInfo,
    key: Zeroizing<String>,
}

pub struct ConnectionStore {
    root: PathBuf,
    credentials: Arc<dyn CredentialStore>,
}
impl ConnectionStore {
    pub fn production(data_root: &Path) -> io::Result<Self> {
        let root = data_root.join("config/connections");
        private_directory(&root)?;
        let credentials = Arc::new(OsCredentialStore::for_directory(&root)?);
        Ok(Self::with_credentials(root, credentials))
    }
    pub fn with_credentials(root: PathBuf, credentials: Arc<dyn CredentialStore>) -> Self {
        Self { root, credentials }
    }
    pub fn create(
        &self,
        preset: ConnectionPreset,
        key: Zeroizing<String>,
    ) -> io::Result<ConnectionInfo> {
        validate_key(&key)?;
        private_directory(&self.root)?;
        let (provider, auth_route, base_url) = preset.values();
        let endpoint_ref = Id::generate();
        let info = ConnectionInfo {
            credential_ref: format!("keyring:{endpoint_ref}"),
            endpoint_ref,
            runtime: preset.runtime(),
            provider_id: provider.into(),
            auth_route,
            base_url: base_url.into(),
        };
        let credential = StoredCredential {
            connection: info.clone(),
            key,
        };
        let encoded = Zeroizing::new(
            serde_json::to_string(&credential)
                .map_err(|_| invalid("credential could not be encoded"))?,
        );
        self.credentials.put(&info.credential_ref, &encoded)?;
        let saved = (|| {
            let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
            serde_json::to_writer(&mut file, &info)?;
            file.as_file().sync_all()?;
            file.persist_noclobber(self.path(&info.endpoint_ref))
                .map_err(|e| e.error)?;
            #[cfg(unix)]
            std::fs::File::open(&self.root)?.sync_all()?;
            io::Result::Ok(())
        })();
        if saved.is_err() {
            self.credentials.delete(&info.credential_ref)?;
            return Err(invalid(
                "connection metadata could not be saved; credential removed",
            ));
        }
        Ok(info)
    }
    fn path(&self, id: &Id) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }
    pub fn info(&self, id: &Id) -> io::Result<ConnectionInfo> {
        let path = self.path(id);
        let meta =
            std::fs::symlink_metadata(&path).map_err(|_| invalid("connection was not found"))?;
        if !meta.is_file() || meta.len() > MAX_METADATA {
            return Err(invalid("invalid connection metadata file"));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .map_err(|_| invalid("connection could not be read"))?
            .take(MAX_METADATA + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("connection could not be read"))?;
        if bytes.len() as u64 > MAX_METADATA {
            return Err(invalid("connection metadata exceeds its byte limit"));
        }
        let info: ConnectionInfo =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid connection metadata"))?;
        info.validate()?;
        if &info.endpoint_ref != id {
            return Err(invalid("connection identity mismatch"));
        }
        Ok(info)
    }
    pub fn list(&self) -> io::Result<Vec<ConnectionInfo>> {
        let mut result = vec![];
        for (index, entry) in std::fs::read_dir(&self.root)
            .map_err(|_| invalid("connection directory is unavailable"))?
            .enumerate()
        {
            if index >= 1000 {
                return Err(invalid("too many saved connection files"));
            }
            let entry = entry.map_err(|_| invalid("connection directory could not be read"))?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let id: Id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| Id::parse(s).ok())
                .ok_or_else(|| invalid("invalid connection file name"))?;
            result.push(self.info(&id)?);
            if result.len() > 1000 {
                return Err(invalid("too many saved connections"));
            }
        }
        result.sort_by(|a, b| a.endpoint_ref.to_string().cmp(&b.endpoint_ref.to_string()));
        Ok(result)
    }
    pub fn revoke(&self, id: &Id) -> io::Result<()> {
        let info = self.info(id)?;
        // Keep immutable metadata so existing run snapshots remain readable.
        self.credentials.delete(&info.credential_ref)
    }
    pub fn resolve_opencode(&self, binding: &Binding) -> io::Result<ResolvedConnection> {
        self.resolve(binding, RuntimeKind::Opencode)
    }
    pub fn resolve_codex(&self, binding: &Binding) -> io::Result<ResolvedConnection> {
        self.resolve(binding, RuntimeKind::Codex)
    }

    pub fn resolve_claude(&self, binding: &Binding) -> io::Result<ResolvedConnection> {
        self.resolve(binding, RuntimeKind::Claude)
    }
    fn resolve(&self, binding: &Binding, runtime: RuntimeKind) -> io::Result<ResolvedConnection> {
        if binding.runtime != runtime {
            return Err(invalid("connection resolver runtime mismatch"));
        }
        let id = binding
            .endpoint_ref
            .as_ref()
            .ok_or_else(|| invalid("binding requires an endpoint reference"))?;
        let info = self.info(id)?;
        if binding.runtime != info.runtime
            || binding.provider_id != info.provider_id
            || binding.auth_route != info.auth_route
            || binding.credential_ref.as_ref() != Some(&info.credential_ref)
        {
            return Err(invalid("binding does not match its saved connection"));
        }
        let raw = self.credentials.get(&info.credential_ref)?;
        if raw.len() > MAX_KEY * 2 + MAX_METADATA as usize {
            return Err(invalid("credential exceeds its byte limit"));
        }
        let stored: StoredCredential =
            serde_json::from_str(&raw).map_err(|_| invalid("invalid stored credential"))?;
        // The credential itself binds the complete destination. Editing the
        // public metadata cannot redirect this key to another endpoint.
        if stored.connection != info {
            return Err(invalid("credential destination mismatch"));
        }
        validate_key(&stored.key)?;
        Ok(ResolvedConnection {
            info,
            key: stored.key,
        })
    }
}

/// Deliberately no Debug/Serialize implementation: this is child-only data.
pub struct ResolvedConnection {
    pub info: ConnectionInfo,
    key: Zeroizing<String>,
}
impl ResolvedConnection {
    pub fn into_api_key(self) -> Zeroizing<String> {
        self.key
    }
    pub fn environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("IYAGI_PROVIDER_API_KEY".into(), self.key.to_string()),
            (
                "OPENCODE_CONFIG_CONTENT".into(),
                serde_json::json!({
                    "provider": { &self.info.provider_id: { "options": {
                        "baseURL": self.info.base_url, "apiKey": "{env:IYAGI_PROVIDER_API_KEY}"
                    }}}
                })
                .to_string(),
            ),
        ])
    }
    pub fn verify_config(&self, config: &serde_json::Value) -> io::Result<()> {
        let options = &config["provider"][&self.info.provider_id]["options"];
        if options["baseURL"].as_str() != Some(&self.info.base_url)
            || options["apiKey"].as_str() != Some(self.key.as_str())
        {
            return Err(invalid(
                "OpenCode effective credentials or endpoint differ from the saved connection",
            ));
        }
        let expected = serde_json::json!({&self.info.provider_id: {"options": {
            "baseURL": self.info.base_url, "apiKey": self.key.as_str()
        }}});
        if config["provider"] != expected
            || config
                .get("mcp")
                .is_some_and(|v| v.as_object().is_none_or(|m| !m.is_empty()))
            || config
                .get("plugin")
                .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
        {
            return Err(invalid(
                "OpenCode loaded unexpected provider or extension configuration",
            ));
        }
        Ok(())
    }
    pub fn redactor(&self) -> Arc<SecretRedactor> {
        self.redactor_with(vec![])
    }
    pub fn redactor_with(&self, mut secrets: Vec<String>) -> Arc<SecretRedactor> {
        secrets.push(self.key.to_string());
        Arc::new(SecretRedactor::new(secrets))
    }
}

pub struct SecretRedactor {
    secrets: Vec<Zeroizing<String>>,
}
impl SecretRedactor {
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        let mut values: Vec<String> = secrets.into_iter().filter(|s| !s.is_empty()).collect();
        let escaped: Vec<_> = values
            .iter()
            .filter_map(|s| serde_json::to_string(s).ok())
            .map(|s| s[1..s.len() - 1].to_owned())
            .collect();
        values.extend(escaped);
        values.sort_by_key(|s| std::cmp::Reverse(s.len()));
        values.dedup();
        Self {
            secrets: values.into_iter().map(Zeroizing::new).collect(),
        }
    }
    pub fn redact_json(&self, value: &mut serde_json::Value) {
        self.redact_value(value, 0);
    }
    /// 바이트 슬라이스에 등록된 비밀(JSON 이스케이프 변형 포함)이 하나라도
    /// 들어 있는지. UTF-8 검사 없이 돌아가므로 PTY 청크의 빠른 경로에
    /// 쓴다 — `false`면 복사 없이 그대로 흘려도 된다.
    pub fn contains_secret(&self, haystack: &[u8]) -> bool {
        self.secrets.iter().any(|secret| {
            let needle = secret.as_bytes();
            !needle.is_empty()
                && haystack.len() >= needle.len()
                && haystack
                    .windows(needle.len())
                    .any(|window| window == needle)
        })
    }
    /// PTY 청크용: 부분 문자열 치환만 한다(JSON 재직렬화 없음). 터미널
    /// 바이트는 사용자가 보는 그대로 저널에 남아야 하므로 `redact_text`의
    /// JSON 분기(비밀이 드러나면 `Value::to_string()`으로 공백·줄바꿈을
    /// 다시 찍는다)를 타지 않는다. 이스케이프 변형은 `new`가 이미 needle로
    /// 등록해 두었으니 JSON 문자열 안의 토큰도 여기서 잡힌다.
    pub fn redact_plain(&self, text: &mut String) {
        for secret in &self.secrets {
            if text.contains(secret.as_str()) {
                *text = text.replace(secret.as_str(), "[redacted]");
            }
        }
    }
    fn redact_value(&self, value: &mut serde_json::Value, depth: usize) -> bool {
        let mut changed = false;
        match value {
            serde_json::Value::String(s) => changed |= self.redact_text(s, depth + 1),
            serde_json::Value::Array(items) => {
                for value in items {
                    changed |= self.redact_value(value, depth + 1);
                }
            }
            serde_json::Value::Object(map) => {
                let old = std::mem::take(map);
                for (mut key, mut value) in old {
                    changed |= self.redact_text(&mut key, depth + 1);
                    changed |= self.redact_value(&mut value, depth + 1);
                    map.insert(key, value);
                }
            }
            _ => {}
        }
        changed
    }
    fn redact_text(&self, text: &mut String, depth: usize) -> bool {
        let mut changed = false;
        for secret in &self.secrets {
            if text.contains(secret.as_str()) {
                *text = text.replace(secret.as_str(), "[redacted]");
                changed = true;
            }
        }
        // Codex structured results are JSON encoded inside JSON strings.
        // Decode those layers before scrubbing, then re-encode only when a
        // secret changed. Ordinary output retains its original formatting.
        if depth < 128
            && matches!(
                text.trim_start().as_bytes().first(),
                Some(b'{' | b'[' | b'"')
            )
        {
            if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(text) {
                if self.redact_value(&mut value, depth + 1) {
                    *text = value.to_string();
                    changed = true;
                }
            }
        }
        changed
    }
}
impl crate::exec::output::Redactor for SecretRedactor {
    fn redact(&self, text: &mut String) {
        self.redact_text(text, 0);
    }
}

pub(crate) fn credential_id(reference: &str) -> io::Result<Id> {
    reference
        .strip_prefix("keyring:")
        .and_then(|id| Id::parse(id).ok())
        .ok_or_else(|| invalid("invalid credential reference"))
}
fn validate_key(key: &str) -> io::Result<()> {
    if !(8..=MAX_KEY).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(invalid(
            "API key must contain 8 to 4096 non-whitespace ASCII bytes",
        ));
    }
    Ok(())
}
pub(crate) fn private_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)
        .map_err(|_| invalid("private connection directory could not be created"))?;
    if !std::fs::symlink_metadata(path)
        .map_err(|_| invalid("private directory is unavailable"))?
        .is_dir()
    {
        return Err(invalid("private directory must not be a symlink"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| invalid("private directory permissions could not be set"))?;
    }
    Ok(())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "OS credential store is unavailable, locked, or the credential is missing",
    )
}

#[derive(Debug, clap::Subcommand)]
pub enum ConnectionCommand {
    /// Create an immutable provider connection. Reads the key only from stdin.
    Add {
        #[arg(long)]
        preset: ConnectionPreset,
        #[arg(long, required = true)]
        key_stdin: bool,
    },
    /// List public endpoint metadata and opaque credential refs (never keys).
    List,
    /// Delete a saved key; retain connection metadata for existing snapshots.
    Revoke {
        #[arg(long, value_parser = parse_endpoint)]
        endpoint: Id,
    },
}
fn parse_endpoint(text: &str) -> Result<Id, String> {
    Id::parse(text).map_err(|_| "endpoint must be a UUID v4".into())
}
pub fn run_cli(root: &Path, command: ConnectionCommand) -> i32 {
    let result = (|| {
        let store = ConnectionStore::production(root)?;
        let output = match command {
            ConnectionCommand::Add {
                preset,
                key_stdin: _,
            } => {
                use std::io::IsTerminal;
                if io::stdin().is_terminal() {
                    return Err(invalid(
                        "pipe the API key on stdin; terminal input would echo the secret",
                    ));
                }
                let mut raw = Zeroizing::new(Vec::new());
                io::stdin()
                    .take((MAX_KEY + 3) as u64)
                    .read_to_end(&mut raw)
                    .map_err(|_| invalid("API key stdin could not be read"))?;
                if raw.ends_with(b"\n") {
                    raw.pop();
                    if raw.ends_with(b"\r") {
                        raw.pop();
                    }
                }
                let key = Zeroizing::new(
                    std::str::from_utf8(&raw)
                        .map_err(|_| invalid("API key must be UTF-8"))?
                        .to_owned(),
                );
                serde_json::to_value(store.create(preset, key)?)
                    .map_err(|_| invalid("connection response could not be encoded"))?
            }
            ConnectionCommand::List => serde_json::to_value(store.list()?)
                .map_err(|_| invalid("connection list could not be encoded"))?,
            ConnectionCommand::Revoke { endpoint } => {
                store.revoke(&endpoint)?;
                serde_json::json!({"credential_removed":true,"endpoint_ref":endpoint})
            }
        };
        writeln!(io::stdout(), "{output}")
            .map_err(|_| invalid("connection response could not be written"))
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("connection: {error}");
            1
        }
    }
}
