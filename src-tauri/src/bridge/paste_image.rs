//! 클립보드·드롭 그림을 임시 파일로 떨군다 — 터미널이 그림을 받을 수 없으니
//! 경로를 대신 붙여넣기 위한 절반(`src/features/terminal/pasteImage.ts`).
//!
//! 웹뷰는 파일을 쓸 수 없으므로 앱 프로세스가 쓴다. 들어오는 것은 base64 한
//! 덩어리와 확장자뿐이고, 확장자는 허용 목록으로만 정해진다 — 경로 조각이
//! 섞여 들어와도 파일 이름을 만들 때 쓰이지 않는다.
//!
//! 스크린샷에는 남에게 보일 것이 아닌 것이 섞이기 쉬우므로, unix에서는 임시
//! 폴더 아래 `iyagi-<uid>` 계정 전용 폴더에 내려놓는다. 그 폴더까지의
//! 모든 컴포넌트가 우리 소유의 진짜 폴더인지 확인한 뒤에만 쓰고(다른 계정이
//! 선점한 경로에는 쓰지 않는다), 폴더는 0700, 파일은 만드는 순간부터 0600이다.
//! 하루 지난 것은 다음 붙여넣기 때 치운다 — 앱이 죽어도 임시 폴더가 계속
//! 불어나지 않게.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use term_contracts::error::{ErrorCode, RpcError};
use uuid::Uuid;

/// 붙여넣기 한 번의 상한. `clipboardImage.ts`의 `PASTE_IMAGE_MAX_BYTES`와 같은 값.
pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// 이보다 오래된 붙여넣기 파일은 다음 붙여넣기 때 지운다.
const RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
/// 허용 확장자 — 프론트의 `IMAGE_EXTENSIONS`와 같은 집합이다.
const ALLOWED_EXTENSIONS: &[&str] = &["png", "jpg", "gif", "webp", "bmp"];
/// 우리가 만든 파일만 지우기 위한 표식.
const FILE_PREFIX: &str = "iyagi-paste-";

/// unix: 임시 폴더 아래 uid 붙인 계정 전용 뿌리를 쓴다. 공용 `/tmp`에서 모든
/// 계정이 한 이름을 공유하면 첫 계정이 만든 0700 폴더에 나머지 계정이 갇히므로
/// 이름부터 갈라놓는다(경로 선점 자체는 `verify_paste_dir_owned`가 막는다).
#[cfg(unix)]
fn paste_dir() -> PathBuf {
    std::env::temp_dir()
        .join(format!("iyagi-{}", current_uid()))
        .join("paste")
}

/// Windows의 %TEMP%는 사용자 프로필 안 계정 전용 경로라 우회 없이 그대로 쓴다.
#[cfg(not(unix))]
fn paste_dir() -> PathBuf {
    std::env::temp_dir().join("iyagi").join("paste")
}

fn invalid(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorCode::InvalidArgument, message)
}

fn io_failed(what: &str, error: &std::io::Error) -> RpcError {
    RpcError::new(ErrorCode::InvalidState, format!("{what} failed: {error}"))
}

/// 허용 목록 안의 확장자만 통과시키고, 통과한 것은 목록의 상수로 바꾼다 —
/// 파일 이름에 들어가는 조각이 호출자가 준 문자열이 아니게 된다.
pub fn checked_extension(ext: &str) -> Result<&'static str, RpcError> {
    ALLOWED_EXTENSIONS
        .iter()
        .find(|allowed| allowed.eq_ignore_ascii_case(ext))
        .copied()
        .ok_or_else(|| invalid(format!("unsupported image extension: {ext}")))
}

/// base64 → 바이트. 상한은 풀기 전(길이 추정)과 푼 뒤 두 번 본다 — 큰 입력을
/// 메모리에 펼치고 나서야 거절하지 않게.
pub fn decode_image(data: &str) -> Result<Vec<u8>, RpcError> {
    if data.len() / 4 * 3 > MAX_IMAGE_BYTES {
        return Err(invalid("image exceeds the paste size limit"));
    }
    let bytes = STANDARD
        .decode(data)
        .map_err(|error| invalid(format!("invalid image data: {error}")))?;
    if bytes.is_empty() {
        return Err(invalid("empty image"));
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(invalid("image exceeds the paste size limit"));
    }
    Ok(bytes)
}

/// `iyagi-paste-<밀리초>-<짧은 uuid>.<ext>` — 시간순으로 읽히고 절대 겹치지 않는다.
fn file_name(ext: &str, millis: u128, id: &str) -> String {
    format!("{FILE_PREFIX}{millis}-{id}.{ext}")
}

/// chmod 실패를 버리지 않고 돌려준다 — 우리 소유라고 검증한 폴더에서
/// 실패한다면 뭔가 그르고, 조용히 넘기면 0700이 안 걸린 채로 진행된다.
#[cfg(unix)]
fn narrow(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn narrow(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// 이 프로세스의 uid. std에 getuid(2)가 없어 libc을 직접 부른다
/// (`system.rs`의 `account_login_shell`과 같은 길).
#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: 인자가 없고 프로세스 속성만 읽어 오는 순수 조회다.
    unsafe { libc::getuid() }
}

/// 임시 폴더 뿌리와 `dir` 사이의 모든 컴포넌트가 심볼릭 링크가 아닌 진짜
/// 폴더이고 현재 uid 소유인지 확인한다. 공용 `/tmp`에서 다른 계정이 같은
/// 경로를 선점하면 chmod는 EPERM으로 실패했고, 그 실패를 버렸기 때문에
/// 좁혀지지 않은 폴더 안으로 파일이 떨어졌다 — 이제는 검증이 어긋나면 쓰기를
/// 거절한다. 뿌리 자체(`/tmp`, `/var/folders/...` 등)는 root가 소유한 게
/// 정상이므로 검증에서 제외한다.
#[cfg(unix)]
fn verify_paste_dir_owned(dir: &Path) -> Result<(), RpcError> {
    use std::os::unix::fs::MetadataExt;

    let root = std::env::temp_dir();
    let mut chain: Vec<&Path> = Vec::new();
    let mut cursor = dir;
    while cursor != root {
        chain.push(cursor);
        match cursor.parent() {
            Some(parent) => cursor = parent,
            None => {
                return Err(RpcError::new(
                    ErrorCode::InvalidState,
                    "paste dir is not under the temp root".to_string(),
                ));
            }
        }
    }
    for path in chain.iter().rev() {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|error| io_failed("inspect paste dir", &error))?;
        // symlink_metadata는 링크 자체를 본다 — 가리키는 폴더가 아니라.
        if !meta.is_dir() || meta.uid() != current_uid() {
            return Err(RpcError::new(
                ErrorCode::InvalidState,
                format!("paste dir is not an owned directory: {}", path.display()),
            ));
        }
    }
    Ok(())
}

/// 하루 지난 우리 파일을 치운다. 실패는 무시한다 — 청소가 붙여넣기를 막으면 안 된다.
fn prune(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(FILE_PREFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > RETENTION);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 파일을 없는 경로에 새로 만들며 쓴다. unix에서는 만드는 순간부터 0600이고
/// 심볼릭 링크를 포함해 이미 있는 경로면 실패한다 — `fs::write`는 0644로
/// 만들고 나서야 좁히며 링크를 따라가서, 찰나의 노출과 경로 선점 두 문제가
/// 다 열려 있었다. (Windows는 사용자별 %TEMP%의 기본 ACL을 따른다.)
fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<(), RpcError> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| io_failed("create paste file", &error))?;
    file.write_all(bytes)
        .map_err(|error| io_failed("write paste file", &error))?;
    Ok(())
}

/// 한 장을 쓰고 그 절대 경로를 돌려준다. 폴더는 필요하면 만든다.
pub fn write_paste_file(
    dir: &Path,
    bytes: &[u8],
    ext: &'static str,
    now: SystemTime,
) -> Result<String, RpcError> {
    std::fs::create_dir_all(dir).map_err(|error| io_failed("create paste dir", &error))?;
    #[cfg(unix)]
    verify_paste_dir_owned(dir)?;
    narrow(dir, 0o700).map_err(|error| io_failed("narrow paste dir", &error))?;
    prune(dir, now);
    let millis = now
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or(0);
    let id = Uuid::new_v4().simple().to_string();
    let path = dir.join(file_name(ext, millis, &id[..8]));
    write_exclusive(&path, bytes)?;
    Ok(path.to_string_lossy().into_owned())
}

/// 클립보드·드롭 그림 저장. 돌려주는 값은 붙여넣을 절대 경로다.
#[tauri::command]
pub async fn paste_save_image(data: String, ext: String) -> Result<String, RpcError> {
    let ext = checked_extension(&ext)?;
    let bytes = decode_image(&data)?;
    // 파일 쓰기는 블로킹이다 — 붙여넣기 한 번이 IPC 런타임을 잡지 않게 옮긴다.
    tokio::task::spawn_blocking(move || {
        write_paste_file(&paste_dir(), &bytes, ext, SystemTime::now())
    })
    .await
    .map_err(|error| {
        RpcError::new(
            ErrorCode::InvalidState,
            format!("paste task failed: {error}"),
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_listed_extensions() {
        assert_eq!(checked_extension("PNG").unwrap(), "png");
        assert_eq!(checked_extension("jpg").unwrap(), "jpg");
        for bad in ["exe", "../png", "png/../x", "svg", ""] {
            assert!(checked_extension(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn rejects_empty_and_malformed_payloads() {
        assert!(decode_image("").is_err());
        assert!(decode_image("not base64!!").is_err());
        assert_eq!(
            decode_image(&STANDARD.encode([1u8, 2, 3])).unwrap(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn rejects_oversize_payload_before_decoding() {
        let oversize = "A".repeat(MAX_IMAGE_BYTES / 3 * 4 + 8);
        assert!(decode_image(&oversize).is_err());
    }

    #[test]
    fn writes_the_bytes_and_returns_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_paste_file(dir.path(), b"png-bytes", "png", SystemTime::now()).unwrap();
        assert!(path.ends_with(".png"), "{path}");
        assert_eq!(std::fs::read(&path).unwrap(), b"png-bytes");
    }

    /// 파일은 만들어진 순간부터 그룹/다른 사용자에게 보이지 않는다 —
    /// 0644로 만들고 나서 좁히면 좁히기 전 찰나가 남는다.
    #[cfg(unix)]
    #[test]
    fn creates_the_file_owner_only_from_the_start() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = write_paste_file(dir.path(), b"png", "png", SystemTime::now()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "mode was {mode:o}");
    }

    /// 심볼릭 링크로 선점된 경로에는 쓰지 않는다 — `create_dir_all`은 링크가
    /// 가리키는 폴더를 보고 성공하지만, 소유 검증은 링크 자체를 본다.
    #[cfg(unix)]
    #[test]
    fn refuses_to_write_through_a_symlinked_dir() {
        let base = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let link = base.path().join("paste-link");
        std::os::unix::fs::symlink(target.path(), &link).unwrap();

        let error = write_paste_file(&link, b"png", "png", SystemTime::now()).unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidState);
    }

    #[test]
    fn prunes_only_our_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("iyagi-paste-1-aaaaaaaa.png");
        let foreign = dir.path().join("someone-elses.png");
        std::fs::write(&stale, b"old").unwrap();
        std::fs::write(&foreign, b"keep").unwrap();

        // 지금이 보존 기간보다 한참 뒤인 것처럼 본다 — 파일 시각을 건드리지 않고
        // 같은 판정을 만든다.
        let later = SystemTime::now() + RETENTION + Duration::from_secs(60);
        let fresh = write_paste_file(dir.path(), b"new", "png", later).unwrap();

        assert!(!stale.exists(), "stale paste file must be removed");
        assert!(
            foreign.exists(),
            "files we did not create must be left alone"
        );
        assert!(Path::new(&fresh).exists());
    }
}
