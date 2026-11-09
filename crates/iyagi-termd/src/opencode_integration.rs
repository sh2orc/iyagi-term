//! Inject session reporters into iyagi-owned PTYs without editing OpenCode's
//! global/project settings. Existing config and permission choices are preserved.

use std::{collections::BTreeMap, io::Write, path::Path};

const CONFIG: &str = "OPENCODE_CONFIG_CONTENT";
const TUI_CONFIG: &str = "OPENCODE_TUI_CONFIG";
const FILES: &[(&str, &str)] = &[
    (
        "reporter.mjs",
        include_str!("opencode_integration/reporter.mjs"),
    ),
    (
        "session.mjs",
        include_str!("opencode_integration/session.mjs"),
    ),
    ("tui.mjs", include_str!("opencode_integration/tui.mjs")),
];

pub fn configure(root: &Path, env: &mut BTreeMap<String, String>) {
    if let Err(error) = configure_inner(root, env) {
        // No config values in diagnostics: inline config may contain credentials.
        tracing::warn!(%error, "OpenCode session integration unavailable");
    }
}

fn configure_inner(root: &Path, env: &mut BTreeMap<String, String>) -> std::io::Result<()> {
    let root = root.canonicalize()?;
    let dir = root.join("config/opencode-session");
    let plugin = reqwest::Url::from_file_path(dir.join("session.mjs"))
        .map_err(|_| std::io::Error::other("invalid plugin path"))?;
    let existing = env
        .get(CONFIG)
        .cloned()
        .or_else(|| std::env::var(CONFIG).ok());
    let merged = merge_config(existing.as_deref(), plugin.as_str())
        .ok_or_else(|| std::io::Error::other("inline config cannot be safely extended"))?;
    std::fs::create_dir_all(&dir)?;
    for (name, content) in FILES {
        write_if_changed(&dir.join(name), content)?;
    }
    let tui_plugin = reqwest::Url::from_file_path(dir.join("tui.mjs"))
        .map_err(|_| std::io::Error::other("invalid TUI plugin path"))?;
    write_if_changed(
        &dir.join("tui.json"),
        &serde_json::json!({"plugin": [tui_plugin.as_str()]}).to_string(),
    )?;
    let program = daemon_program(&root)?;
    env.insert(CONFIG.into(), merged);
    // This optional TUI reporter also tracks selecting an existing conversation
    // without sending a prompt. Respect an explicitly selected TUI config file.
    if !env.contains_key(TUI_CONFIG) && std::env::var_os(TUI_CONFIG).is_none() {
        env.insert(
            TUI_CONFIG.into(),
            dir.join("tui.json").to_string_lossy().into_owned(),
        );
    }
    env.insert(
        "IYAGI_DAEMON_BIN".into(),
        program.to_string_lossy().into_owned(),
    );
    env.insert("IYAGI_DATA_DIR".into(), root.to_string_lossy().into_owned());
    Ok(())
}

/// PTY에 `IYAGI_DAEMON_BIN`으로 알려 줄 데몬 경로. 보통은 지금 도는 실행
/// 파일이지만, 오래 떠 있는 데몬은 repo 이름 변경·`cargo clean` 뒤에도 옛
/// 경로를 `current_exe`로 돌려준다 — 그 경로를 주입하면 셸의 `ccd`/`ccg`와
/// reporter가 없는 파일을 부른다. 그때는 앱이 기동 때마다 갱신하는 안정
/// 사본(`<root>/bin/iyagi-termd`)을 대신 쓴다.
fn daemon_program(root: &Path) -> std::io::Result<std::path::PathBuf> {
    let current = std::env::current_exe()?;
    if current.is_file() {
        return Ok(current);
    }
    let stable = root
        .join("bin")
        .join(format!("iyagi-termd{}", std::env::consts::EXE_SUFFIX));
    Ok(if stable.is_file() { stable } else { current })
}

fn merge_config(existing: Option<&str>, plugin: &str) -> Option<String> {
    let text = existing.filter(|s| !s.trim().is_empty()).unwrap_or("{}");
    if text.len() > 1024 * 1024 {
        return None;
    }
    let mut config: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = config.as_object_mut()?;
    let plugins = object
        .entry("plugin")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()?;
    if !plugins.iter().any(|value| value.as_str() == Some(plugin)) {
        plugins.push(serde_json::Value::String(plugin.into()));
    }
    serde_json::to_string(&config).ok()
}

fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    if std::fs::read(path).ok().as_deref() == Some(content.as_bytes()) {
        return Ok(());
    }
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    temp.write_all(content.as_bytes())?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_config_permissions_and_plugins_without_duplicate_registration() {
        let original = r#"{"permission":"ask","model":"provider/model","plugin":[["custom",{"enabled":true}]]}"#;
        let once = merge_config(Some(original), "file:///iyagi/session.mjs").unwrap();
        assert_eq!(
            merge_config(Some(&once), "file:///iyagi/session.mjs"),
            Some(once.clone())
        );
        let value: serde_json::Value = serde_json::from_str(&once).unwrap();
        assert_eq!(value["permission"], "ask");
        assert_eq!(value["model"], "provider/model");
        assert_eq!(
            value["plugin"][0],
            serde_json::json!(["custom", {"enabled":true}])
        );
        assert_eq!(value["plugin"].as_array().unwrap().len(), 2);
        for invalid in ["[]", "null", "{invalid}", r#"{"plugin":"custom"}"#] {
            assert!(merge_config(Some(invalid), "file:///iyagi/session.mjs").is_none());
        }
    }
}
