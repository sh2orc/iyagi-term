//! Consent-based Claude Code status-line integration for subscription usage.
//! The exact original `statusLine` value is kept in iyagi's private data
//! directory and restored on removal.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const MARKER: &str = "claude-usage";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("settings file unreadable: {0}")]
    Read(String),
    #[error("settings file is not valid JSON: {0}")]
    InvalidJson(String),
    #[error("write failed: {0}")]
    Write(String),
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub path: String,
    pub active: bool,
    pub proposed: Option<String>,
    pub command: String,
}

pub fn backup_path(data_dir: &Path) -> PathBuf {
    data_dir.join("config/claude-statusline-original.json")
}

pub fn status(path: &Path, command: &str) -> Result<Status, Error> {
    let current = read_settings(path)?.unwrap_or_else(|| json!({}));
    let active = is_managed(&current);
    let proposed = if active {
        None
    } else {
        let mut next = current;
        inject(&mut next, command);
        Some(pretty(&next))
    };
    Ok(Status {
        path: path.display().to_string(),
        active,
        proposed,
        command: command.to_string(),
    })
}

pub fn apply(path: &Path, data_dir: &Path, command: &str) -> Result<Status, Error> {
    let mut settings = read_settings(path)?.unwrap_or_else(|| json!({}));
    if !is_managed(&settings) {
        let original = settings.get("statusLine").cloned().unwrap_or(Value::Null);
        write_private_json(&backup_path(data_dir), &original)?;
        inject(&mut settings, command);
        write_settings(path, &pretty(&settings))?;
    }
    status(path, command)
}

pub fn remove(path: &Path, data_dir: &Path, command: &str) -> Result<Status, Error> {
    let Some(mut settings) = read_settings(path)? else {
        return status(path, command);
    };
    if !is_managed(&settings) {
        return status(path, command);
    }
    let backup = backup_path(data_dir);
    let backup_text = std::fs::read_to_string(&backup)
        .map_err(|error| Error::Read(format!("original statusLine backup: {error}")))?;
    let original = serde_json::from_str::<Value>(&backup_text)
        .map_err(|error| Error::InvalidJson(format!("original statusLine backup: {error}")))?;
    let object = settings
        .as_object_mut()
        .expect("settings normalized to object");
    if original.is_null() {
        object.remove("statusLine");
    } else {
        object.insert("statusLine".to_string(), original);
    }
    write_settings(path, &pretty(&settings))?;
    let _ = std::fs::remove_file(backup);
    status(path, command)
}

fn inject(settings: &mut Value, command: &str) {
    if !settings.is_object() {
        *settings = json!({});
    }
    settings.as_object_mut().expect("object").insert(
        "statusLine".to_string(),
        json!({ "type": "command", "command": command, "refreshInterval": 60 }),
    );
}

fn is_managed(settings: &Value) -> bool {
    settings
        .get("statusLine")
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(MARKER))
}

fn read_settings(path: &Path) -> Result<Option<Value>, Error> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| Error::InvalidJson(error.to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Read(error.to_string())),
    }
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON serializes");
    text.push('\n');
    text
}

/// 이 연동은 hooks 연동과 **같은** `~/.claude/settings.json`을 고치므로
/// 쓰기 구현도 공유한다(`hooks_json::write_settings_file`). 그쪽 계약:
/// `.iyagi.bak`은 최초 적용 전 원본만 남기고, 임시 파일은 프로세스별
/// 고유 이름에 `sync_all`과 원본 권한 보존을 거치며, 전 과정은 프로세스
/// 전역으로 직렬화된다. 따로 구현하던 시절엔 두 연동이 같은 `.bak`/`.tmp`
/// 이름을 두고 다퉈, 두 번째 적용이 사용자의 원본 백업을 우리 내용으로
/// 덮어썼다.
fn write_settings(path: &Path, content: &str) -> Result<(), Error> {
    super::hooks_json::write_settings_file(path, content)
        .map_err(|error| Error::Write(error.to_string()))
}

fn write_private_json(path: &Path, value: &Value) -> Result<(), Error> {
    let parent = path.parent().expect("backup has parent");
    std::fs::create_dir_all(parent).map_err(|e| Error::Write(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::Write(e.to_string()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, pretty(value)).map_err(|e| Error::Write(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| Error::Write(e.to_string()))?;
    }
    std::fs::rename(tmp, path).map_err(|e| Error::Write(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_exact_user_statusline() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let original = json!({"type":"command","command":"my-line --color","padding": 1});
        std::fs::write(
            &settings,
            pretty(&json!({"model":"sonnet","statusLine":original})),
        )
        .unwrap();

        apply(&settings, dir.path(), "'/opt/iyagi-termd' claude-usage").unwrap();
        assert!(status(&settings, "x").unwrap().active);
        remove(&settings, dir.path(), "'/opt/iyagi-termd' claude-usage").unwrap();

        let restored: Value =
            serde_json::from_str(&std::fs::read_to_string(settings).unwrap()).unwrap();
        assert_eq!(restored["statusLine"], original);
        assert_eq!(restored["model"], "sonnet");
    }

    /// 공용 writer 계약: 두 번째·세 번째 쓰기가 "최초 적용 전 원본"
    /// 백업을 덮어쓰지 않는다(hooks 연동과 같은 파일·같은 백업이다).
    #[test]
    fn repeated_writes_keep_the_pristine_backup() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(
            &settings,
            pretty(&json!({"statusLine": {"type":"command","command":"my-line"}})),
        )
        .unwrap();

        let command = "'/opt/iyagi-termd' claude-usage";
        apply(&settings, dir.path(), command).unwrap();
        remove(&settings, dir.path(), command).unwrap();
        apply(&settings, dir.path(), command).unwrap();

        let backup = std::fs::read_to_string(settings.with_extension("json.iyagi.bak")).unwrap();
        assert!(
            backup.contains("my-line"),
            "the pre-first-apply original must survive: {backup}"
        );
    }

    #[test]
    fn removes_statusline_when_none_existed() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        apply(&settings, dir.path(), "iyagi-termd claude-usage").unwrap();
        remove(&settings, dir.path(), "iyagi-termd claude-usage").unwrap();
        let restored: Value =
            serde_json::from_str(&std::fs::read_to_string(settings).unwrap()).unwrap();
        assert!(restored.get("statusLine").is_none());
    }
}
