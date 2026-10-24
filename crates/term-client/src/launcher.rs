//! 데몬 바이너리 탐색·분리 시작(T04).
//!
//! 원본 bridge `daemon_manager.rs`의 분리 실행 방식(프로세스 그룹 분리,
//! stdio null — 호스트 터미널 종료가 데몬 수명에 영향을 주지 않게)을
//! 유지하되, 탐색 순서는 GUI 번들 내부 경로만 전제하지 않는다
//! (IMPLEMENTATION_PLAN T04):
//!
//! 1. 명시적 지정(`--daemon-bin`)
//! 2. `IYAGI_TERMD_PATH` 환경변수
//! 3. 실행 파일 인접 위치(같은 디렉터리, 그 상위 `bin/`, 개발 배치 `../..`)
//! 4. GUI가 유지하는 안정 복사본 `<data_dir>/bin/iyagi-termd`
//! 5. `PATH` 탐색
//!
//! 소켓 연결 실패만으로 runtime lock/token을 삭제하지 않는다. singleton
//! 처리는 데몬의 프로세스 identity 로직에 맡긴다(§5.2).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::discovery;

/// 데몬 바이너리 파일 이름.
pub fn daemon_binary_name() -> &'static str {
    if cfg!(windows) {
        "iyagi-termd.exe"
    } else {
        "iyagi-termd"
    }
}

/// 탐색 순서대로 후보를 나열한다. 존재 검사는 하지 않는다(호출자가 고른다).
pub fn daemon_binary_candidates(explicit: Option<&Path>, data_dir: Option<&Path>) -> Vec<PathBuf> {
    let name = daemon_binary_name();
    let mut candidates = Vec::new();
    if let Some(path) = explicit {
        candidates.push(path.to_path_buf());
    }
    if let Ok(env_path) = std::env::var("IYAGI_TERMD_PATH") {
        if !env_path.trim().is_empty() {
            candidates.push(PathBuf::from(env_path));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // 실행 파일과 같은 디렉터리(번들·수동 설치 배치).
            candidates.push(dir.join(name));
            // 상위 bin/ 디렉터리 배치(<prefix>/bin/tui + <prefix>/bin/termd).
            candidates.push(dir.join("..").join("bin").join(name));
            // 개발 배치: <repo>/target/debug/{tui,termd}.
            candidates.push(dir.join("target").join("debug").join(name));
            candidates.push(dir.join("..").join("target").join("debug").join(name));
        }
    }
    if let Some(data_dir) = data_dir {
        // GUI 앱이 유지하는 안정 복사본과 같은 위치(공유 설치).
        candidates.push(data_dir.join("bin").join(name));
    }
    if let Some(found) = find_on_path(name) {
        candidates.push(found);
    }
    candidates
}

/// 첫 번째로 존재하는 후보.
pub fn locate(explicit: Option<&Path>, data_dir: Option<&Path>) -> Option<PathBuf> {
    daemon_binary_candidates(explicit, data_dir)
        .into_iter()
        .find(|p| p.is_file())
}

/// `PATH`에서 실행 파일을 찾는다(std는 경로 탐색을 제공하지 않는다).
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file() && is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// `iyagi-termd --version` 조회 상한 — 느린 디스크에서도 연결을 오래
/// 붙잡지 않게 한다(원본 daemon_manager.rs).
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// 디스크의 데몬 바이너리가 보고하는 build id(`--version` 출력에서 이름을
/// 뺀 나머지 — hello `daemon_version`과 같은 모양). 어떤 실패도 치명적이
/// 아니다(`None`).
pub async fn binary_build_version(binary: &Path) -> Option<String> {
    let mut command = tokio::process::Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(VERSION_PROBE_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    let version = line.strip_prefix("iyagi-termd").unwrap_or(line).trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// `DETACHED_PROCESS` / `CREATE_NEW_PROCESS_GROUP`(Windows 분리 실행).
#[cfg(windows)]
pub const DETACHED_PROCESS_FLAG: u32 = 0x0000_0008;
#[cfg(windows)]
pub const CREATE_NEW_PROCESS_GROUP_FLAG: u32 = 0x0000_0200;
#[cfg(windows)]
pub const CREATE_BREAKAWAY_FROM_JOB_FLAG: u32 = 0x0100_0000;

#[cfg(windows)]
pub fn windows_detached_flags() -> u32 {
    DETACHED_PROCESS_FLAG | CREATE_NEW_PROCESS_GROUP_FLAG
}

/// 분리 실행 trait(테스트는 실제 프로세스를 만들지 않게 주입).
pub trait DaemonSpawner: Send + Sync {
    /// 데몬을 앱 수명/콘솔과 분리해 시작한다. `Ok`가 데몬이 계속 도는
    /// 것은 아니다 — singleton lock 때문에 중복 인스턴스는 즉시 빠진다.
    fn spawn_detached(&self, binary: &Path, data_dir: &Path) -> io::Result<()>;
}

/// 프로덕션 spawner(std::process, OS별 분리 플래그).
pub struct OsDaemonSpawner;

impl DaemonSpawner for OsDaemonSpawner {
    fn spawn_detached(&self, binary: &Path, data_dir: &Path) -> io::Result<()> {
        let mut cmd = std::process::Command::new(binary);
        // std in/out/err를 앱 터미널에서 분리한다(02-runner §1-3).
        // 데몬은 <data_dir>/logs 아래 자기 로그를 소유한다.
        cmd.arg("--data-dir").arg(data_dir);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // 부모 job에서 벗어나려 먼저 BREAKAWAY을 시도한다(job이 거부하면
            // ERROR_ACCESS_DENIED — 그럴 땐 일반 분리 플래그로 되돌린다).
            cmd.creation_flags(windows_detached_flags() | CREATE_BREAKAWAY_FROM_JOB_FLAG);
            match cmd.spawn() {
                Ok(child) => return reap_in_background(child),
                Err(err)
                    if err.kind() == io::ErrorKind::PermissionDenied
                        || err.raw_os_error() == Some(5) =>
                {
                    cmd.creation_flags(windows_detached_flags());
                }
                Err(err) => return Err(err),
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // process_group(0) = setpgid(0,0): 터미널 신호가 앱을 겨냥해도
            // 데몬에 닿지 않게 새 프로세스 그룹.
            cmd.process_group(0);
        }
        reap_in_background(cmd.spawn()?)
    }
}

/// 자식 핸들을 놓아도 OS가 죽이지 않게, 백그라운드 스레드가 종료를 회수한다.
fn reap_in_background(mut child: std::process::Child) -> io::Result<()> {
    let _ = std::thread::Builder::new()
        .name("iyagi-termd-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

/// 데몬 시작 준비 예산(원본과 같은 10s)과 폴링 간격.
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);
pub const READY_POLL: Duration = Duration::from_millis(100);

/// runtime 디렉터리를 만들고 데몬을 분리 시작한다(singleton 안전).
pub fn spawn_daemon(
    locate: &dyn Fn() -> Option<PathBuf>,
    spawner: &dyn DaemonSpawner,
    data_dir: &Path,
) -> Result<(), crate::connection::BridgeError> {
    use crate::connection::BridgeError;
    std::fs::create_dir_all(discovery::runtime_dir(data_dir))
        .map_err(|e| BridgeError::daemon_unavailable(format!("cannot create runtime dir: {e}")))?;
    let binary =
        locate().ok_or_else(|| BridgeError::daemon_unavailable("iyagi-termd binary not found"))?;
    spawn_binary(spawner, &binary, data_dir)
}

/// 이미 결정된 바이너리로 데몬을 분리 시작한다(호출자가 탐색을 마친 경우).
/// `Ok`가 데몬이 계속 도는 것은 아니다(singleton lock — 중복 인스턴스는
/// 스스로 빠진다).
pub fn spawn_binary(
    spawner: &dyn DaemonSpawner,
    binary: &Path,
    data_dir: &Path,
) -> Result<(), crate::connection::BridgeError> {
    use crate::connection::BridgeError;
    std::fs::create_dir_all(discovery::runtime_dir(data_dir))
        .map_err(|e| BridgeError::daemon_unavailable(format!("cannot create runtime dir: {e}")))?;
    spawner
        .spawn_detached(binary, data_dir)
        .map_err(|e| BridgeError::daemon_unavailable(format!("failed to spawn iyagi-termd: {e}")))
}

/// runtime 파일이 나타날 때까지 폴링한다.
pub async fn wait_ready(data_dir: &Path, deadline: Instant) -> bool {
    loop {
        if discovery::runtime_files_present(data_dir) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_candidate_comes_first() {
        let explicit = PathBuf::from("/opt/termd/iyagi-termd");
        let candidates = daemon_binary_candidates(Some(&explicit), None);
        assert_eq!(candidates[0], explicit);
    }

    #[test]
    fn detached_spawner_refuses_missing_binary() {
        let dir = tempfile::tempdir().unwrap();
        let err = OsDaemonSpawner
            .spawn_detached(Path::new("/definitely/not/iyagi-termd"), dir.path())
            .unwrap_err();
        assert!(err.kind() == io::ErrorKind::NotFound || err.raw_os_error().is_some());
    }

    #[tokio::test]
    async fn wait_ready_times_out_when_files_never_appear() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(discovery::runtime_dir(dir.path())).unwrap();
        assert!(!wait_ready(dir.path(), Instant::now()).await);
    }

    #[tokio::test]
    async fn spawn_writes_runtime_files_then_reports_ready() {
        struct ReadySpawner;
        impl DaemonSpawner for ReadySpawner {
            fn spawn_detached(&self, _binary: &Path, data_dir: &Path) -> io::Result<()> {
                std::fs::create_dir_all(discovery::runtime_dir(data_dir))?;
                std::fs::write(discovery::endpoint_file(data_dir), "fake-endpoint")?;
                std::fs::write(discovery::token_file(data_dir), "fake-token")?;
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let locate = || Some(PathBuf::from("fake-iyagi-termd"));
        spawn_daemon(&locate, &ReadySpawner, dir.path()).unwrap();
        assert!(wait_ready(dir.path(), Instant::now() + READY_TIMEOUT).await);
        assert_eq!(
            discovery::read_runtime_file(&discovery::token_file(dir.path())).as_deref(),
            Some("fake-token")
        );
    }
}
