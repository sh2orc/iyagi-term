//! 데이터 경로·runtime 파일 발견(T04).
//!
//! 원본 bridge `daemon_manager.rs`의 경로 계약을 그대로 옮긴다:
//! * 기본 데이터 디렉터리는 macOS도 XDG 규칙(`$XDG_DATA_HOME/Iyagi`,
//!   미설정 시 `~/.local/share/Iyagi`)이다 — `Library/Application Support`
//!   로 바꾸면 기존 세션을 찾지 못한다(IMPLEMENTATION_PLAN §2.2-1).
//! * endpoint는 `<data-dir>/runtime/endpoint`, control 인증 토큰은
//!   `<data-dir>/runtime/token`에서 읽는다. 소켓 경로를 직접 조합하지
//!   않는다(§2.2-3).
//! * 명시적 override는 절대 경로만 받는다(§2.2-2).
//!
//! 토큰은 절대 로그·오류 상세에 출력하지 않는다.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("neither XDG_DATA_HOME nor HOME is set")]
    NoDataHome,
    #[error("data_dir override must be an absolute path")]
    NotAbsolute,
}

/// 기본 데이터 디렉터리(Unix: XDG 규칙 + `Iyagi`).
pub fn default_data_dir() -> Result<PathBuf, DiscoveryError> {
    #[cfg(unix)]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("share"))
            })
            .ok_or(DiscoveryError::NoDataHome)?;
        Ok(base.join("Iyagi"))
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or(DiscoveryError::NoDataHome)?;
        Ok(base.join("Iyagi"))
    }
}

/// 데이터 디렉터리 결정. trailing separator는 정리하되 루트(`/`, `C:\`)는
/// 유지한다(원본 `trim_trailing_separators` 동작).
pub fn resolve_data_dir(override_dir: Option<&str>) -> Result<PathBuf, DiscoveryError> {
    match override_dir {
        Some(dir) => {
            let trimmed = trim_trailing_separators(dir);
            let path = PathBuf::from(trimmed);
            if !path.is_absolute() {
                return Err(DiscoveryError::NotAbsolute);
            }
            Ok(path)
        }
        None => default_data_dir(),
    }
}

/// 루트를 무너뜨리지 않는 trailing separator 정리.
fn trim_trailing_separators(dir: &str) -> &str {
    let trimmed = dir.trim_end_matches(['/', '\\']);
    if trimmed.len() < dir.len() && (trimmed.is_empty() || trimmed.ends_with(':')) {
        &dir[..trimmed.len() + 1]
    } else {
        trimmed
    }
}

/// `<data>/runtime`.
pub fn runtime_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("runtime")
}

/// 데몬이 기록하는 endpoint 문자열(소켓 경로/파이프 이름).
pub fn endpoint_file(data_dir: &Path) -> PathBuf {
    runtime_dir(data_dir).join("endpoint")
}

/// 데몬이 기록하는 control 인증 토큰(데몬 쪽 0600).
pub fn token_file(data_dir: &Path) -> PathBuf {
    runtime_dir(data_dir).join("token")
}

/// runtime 파일 읽기(trim; 없거나 비었으면 `None`).
pub fn read_runtime_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// endpoint/token이 둘 다 존재하는가(데몬 준비 판정).
pub fn runtime_files_present(data_dir: &Path) -> bool {
    endpoint_file(data_dir).is_file() && token_file(data_dir).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_separator_trim_keeps_roots() {
        assert_eq!(trim_trailing_separators(r"C:\"), r"C:\");
        assert_eq!(trim_trailing_separators(r"C:\\"), r"C:\");
        assert_eq!(trim_trailing_separators("/"), "/");
        assert_eq!(trim_trailing_separators("D:/data/"), "D:/data");
        assert_eq!(trim_trailing_separators("/tmp/x//"), "/tmp/x");
        assert_eq!(trim_trailing_separators("/tmp/x"), "/tmp/x");
    }

    #[test]
    fn runtime_files_live_under_runtime_dir() {
        let data = Path::new("/data");
        assert_eq!(endpoint_file(data), Path::new("/data/runtime/endpoint"));
        assert_eq!(token_file(data), Path::new("/data/runtime/token"));
    }

    #[test]
    fn data_dir_override_must_be_absolute() {
        assert!(resolve_data_dir(Some("relative/dir")).is_err());
        let dir = resolve_data_dir(Some("/tmp/Iyagi")).unwrap();
        assert!(dir.is_absolute());
        assert_eq!(resolve_data_dir(Some("/")).unwrap(), Path::new("/"));
    }
}
