//! 앱 단일 실행: 같은 데이터 디렉터리를 쓰는 앱은 하나만 뜬다.
//!
//! 데이터 디렉터리가 같으면 데몬·세션도 같다. 두 번째 앱이 같은 세션에 입력
//! 권한(writer)으로 붙으면 먼저 뜬 창의 터미널이 알림 없이 입력을 잃고(새 writer가
//! 이긴다 — 데몬 §4), 세션당 화면 두 개 상한에 걸리며, 탭 배치 저장이 서로를
//! 덮어쓴다. 그래서 두 번째 실행은 이미 떠 있는 앱에 "앞으로 나와라"를 전하고
//! 곧바로 끝난다. 데이터 디렉터리가 다르면(XDG_DATA_HOME·LOCALAPPDATA로 나눈
//! 개발 인스턴스) 데몬도 다르므로 함께 뜬다.
//!
//! 판정은 `<data>/app/instance.lock`의 배타 잠금이다 — 프로세스가 죽으면 OS가
//! 풀어 주므로 남은 파일에 속지 않는다. 신호 창구는 Unix에서 사용자 전용 디렉터리의
//! `<data>/app/instance.sock`(경로가 소켓 길이 한도를 넘으면 사용자 임시 디렉터리의
//! 해시 이름), Windows에서 데이터 디렉터리마다 다른 이름의 named pipe다. 연결
//! 자체가 신호다 — 내용은 읽지 않고 창을 앞으로 가져오는 것 말고는 아무것도 하지
//! 않는다.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

/// 두 번째 실행이 첫 앱의 신호 창구를 기다리는 시간 — 둘이 거의 동시에 뜨면 첫
/// 앱이 아직 창구를 열기 전일 수 있다.
const SIGNAL_WAIT: Duration = Duration::from_secs(2);
const SIGNAL_RETRY: Duration = Duration::from_millis(50);

pub enum Startup {
    /// 이 데이터 디렉터리의 첫 앱이다. 잠금은 앱이 사는 동안 쥐고 있어야 한다.
    Primary(Primary),
    /// 다른 앱이 이미 이 데이터 디렉터리를 쓰고 있다 — 이 실행은 뜨지 않는다.
    /// `signaled`: 그 앱에 앞으로 나오라고 전했는지.
    Secondary { signaled: bool },
}

/// 첫 앱의 잠금과 신호 창구.
pub struct Primary {
    _lock: File,
    channel: Channel,
}

/// 이 데이터 디렉터리를 쓰는 첫 앱인지 정한다. 두 번째면 첫 앱에 신호를 보낸 뒤
/// `Secondary`를 돌려준다.
pub fn claim(data_dir: &Path) -> io::Result<Startup> {
    let dir = data_dir.join("app");
    create_private_dir(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("instance.lock"))?;
    let channel = Channel::for_data_dir(data_dir);
    match lock.try_lock() {
        Ok(()) => Ok(Startup::Primary(Primary {
            _lock: lock,
            channel,
        })),
        Err(fs::TryLockError::WouldBlock) => Ok(Startup::Secondary {
            signaled: channel.signal(SIGNAL_WAIT),
        }),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

impl Primary {
    /// 두 번째 실행이 신호를 보낼 때마다 `on_activate`를 부른다(백그라운드에서).
    /// 창구를 열지 못해도 잠금은 그대로라 두 번째 앱은 뜨지 않는다 — 이 앱을 앞으로
    /// 불러내지만 못 할 뿐이다.
    pub fn listen(&self, on_activate: impl Fn() + Send + Sync + 'static) -> io::Result<()> {
        self.channel.listen(on_activate)
    }
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

/// 신호를 보낼 수 있을 때까지 `wait` 동안 되풀이한다.
fn retry_until(wait: Duration, mut attempt: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if attempt() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(SIGNAL_RETRY);
    }
}

/// `sun_path` 크기(끝 NUL 포함): macOS/BSD 104, Linux 108.
#[cfg(unix)]
const SUN_PATH_MAX: usize = if cfg!(target_os = "macos") { 104 } else { 108 };

#[cfg(unix)]
struct Channel {
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl Channel {
    fn for_data_dir(data_dir: &Path) -> Self {
        let preferred = data_dir.join("app").join("instance.sock");
        if preferred.as_os_str().len() < SUN_PATH_MAX {
            return Self { path: preferred };
        }
        // 깊은 데이터 디렉터리는 소켓 경로 한도를 넘어 bind가 실패한다 — 사용자 전용 임시
        // 디렉터리(Linux $XDG_RUNTIME_DIR, macOS의 사용자별 $TMPDIR)에 데이터 디렉터리의
        // 해시 이름으로 둔다. 두 실행이 같은 사용자 환경에서 같은 이름을 얻는다.
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let key = data_dir.as_os_str().as_encoded_bytes();
        Self {
            path: base.join(format!("iyagi-app-{:016x}.sock", fnv1a(key))),
        }
    }

    fn signal(&self, wait: Duration) -> bool {
        use std::io::Write;
        retry_until(wait, || {
            std::os::unix::net::UnixStream::connect(&self.path)
                .and_then(|mut stream| stream.write_all(b"activate\n"))
                .is_ok()
        })
    }

    fn listen(&self, on_activate: impl Fn() + Send + Sync + 'static) -> io::Result<()> {
        // 잠금을 쥔 쪽만 여기 온다 — 남아 있는 소켓 파일은 지난 실행의 것이다.
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = std::os::unix::net::UnixListener::bind(&self.path)?;
        std::thread::Builder::new()
            .name("single-instance".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stream.is_ok() {
                        on_activate();
                    }
                }
            })?;
        Ok(())
    }
}

#[cfg(windows)]
struct Channel {
    name: String,
}

#[cfg(windows)]
impl Channel {
    fn for_data_dir(data_dir: &Path) -> Self {
        // 경로는 파이프 이름에 쓸 수 없는 문자를 품는다 — 대소문자를 무시한 경로의
        // 해시로 이름을 만든다(같은 디렉터리는 같은 이름).
        let key = data_dir.to_string_lossy().to_lowercase();
        Self {
            name: format!(r"\\.\pipe\iyagi-app-{:016x}", fnv1a(key.as_bytes())),
        }
    }

    fn signal(&self, wait: Duration) -> bool {
        // 서버가 다음 연결을 받으려고 새 인스턴스를 만드는 사이에는 열리지 않는다 — 되풀이한다.
        retry_until(wait, || {
            OpenOptions::new().write(true).open(&self.name).is_ok()
        })
    }

    fn listen(&self, on_activate: impl Fn() + Send + Sync + 'static) -> io::Result<()> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = self.name.clone();
        tauri::async_runtime::spawn(async move {
            let mut first = true;
            loop {
                // 첫 인스턴스로 이름을 차지한다 — 다른 프로세스가 먼저 만들었으면 실패한다.
                let server = match ServerOptions::new()
                    .first_pipe_instance(first)
                    .create(&name)
                {
                    Ok(server) => server,
                    Err(error) => {
                        eprintln!("iyagi: single-instance pipe unavailable ({error})");
                        return;
                    }
                };
                first = false;
                if server.connect().await.is_ok() {
                    on_activate();
                }
            }
        });
        Ok(())
    }
}

/// 64비트 FNV-1a — 신호 창구 이름용(보안 해시가 아니다: 같은 사용자 안에서 디렉터리를 구분할 뿐).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary(dir: &Path) -> Primary {
        match claim(dir).expect("claim") {
            Startup::Primary(primary) => primary,
            Startup::Secondary { .. } => panic!("the first claim on a data dir must start"),
        }
    }

    #[test]
    fn a_second_launch_on_the_same_data_dir_brings_the_first_forward_instead_of_starting() {
        let dir = tempfile::tempdir().unwrap();
        let first = primary(dir.path());
        let (activated, activations) = std::sync::mpsc::channel();
        first
            .listen(move || {
                let _ = activated.send(());
            })
            .expect("listen");

        match claim(dir.path()).expect("second claim") {
            Startup::Secondary { signaled } => assert!(signaled, "the first app was reached"),
            Startup::Primary(_) => panic!("a second app on the same data dir must not start"),
        }
        activations
            .recv_timeout(Duration::from_secs(2))
            .expect("the first app was asked to come forward");

        // 첫 앱이 끝나면(잠금이 풀리면) 다음 실행은 다시 첫 앱이 된다.
        drop(first);
        primary(dir.path());
    }

    #[test]
    fn a_data_dir_too_deep_for_a_socket_path_still_brings_the_first_app_forward() {
        // 소켓 경로 한도(macOS 104·Linux 108바이트)를 넘는 깊은 데이터 디렉터리.
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("a".repeat(60)).join("b".repeat(60));
        let first = primary(&deep);
        let (activated, activations) = std::sync::mpsc::channel();
        first
            .listen(move || {
                let _ = activated.send(());
            })
            .expect("listen even when the data dir is too deep for a socket path");
        assert!(matches!(
            claim(&deep).expect("second claim"),
            Startup::Secondary { signaled: true }
        ));
        activations
            .recv_timeout(Duration::from_secs(2))
            .expect("the first app was asked to come forward");
    }

    #[test]
    fn apps_on_different_data_dirs_start_independently() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let _first = primary(a.path());
        let _second = primary(b.path());
    }
}
