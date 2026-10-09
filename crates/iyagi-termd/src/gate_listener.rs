//! Per-launch gate endpoint transport (spec `02-runner.md` §3).
//!
//! The gate protocol (`term_pty::gate`) is synchronous over `Read + Write`;
//! this module binds a private one-connection endpoint:
//!
//! * Unix — a UDS in the 0700 runtime dir;
//! * Windows — a named pipe with a per-launch random name (instance ACL is
//!   the process default; the one-time 256-bit nonce + verified peer
//!   identity carry the authentication, see the ADR in `done/I04-daemon.md`).

use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// Duplex byte stream as required by `GateStream` (`Read + Write`), with
/// independently boxed halves so a tokio pipe can be split underneath.
pub struct GateIo {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}

/// Per-read timeout on Unix gate streams. `term_pty::gate::DeadlineReader`
/// only checks its deadline when a read returns `WouldBlock`/`TimedOut`; a
/// blocking UDS read never does, so without this the spec's 5 s gate
/// deadline was dead code (a stuck helper hung the launch thread forever).
#[cfg(unix)]
pub const GATE_READ_STEP: Duration = Duration::from_millis(50);

#[cfg(unix)]
impl GateIo {
    fn from_unix(stream: std::os::unix::net::UnixStream) -> std::io::Result<Self> {
        stream.set_read_timeout(Some(GATE_READ_STEP))?;
        let writer = stream.try_clone()?;
        Ok(GateIo {
            reader: Box::new(stream),
            writer: Box::new(writer),
        })
    }
}

impl Read for GateIo {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Write for GateIo {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let out = self.writer.write(buf);
        tracing::trace!(
            len = buf.len(),
            result = ?out.as_ref().map(|n| *n).map_err(|e| e.to_string()),
            "GateIo::write"
        );
        out
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

/// One-connection gate listener.
#[cfg(unix)]
pub struct GateListener {
    listener: std::os::unix::net::UnixListener,
}

#[cfg(unix)]
impl GateListener {
    /// Signature mirrors the Windows `bind` (`endpoint`, tokio `Handle`) so
    /// the orchestrator's call site stays platform-independent. The UDS
    /// transport below is fully synchronous — the handle is unused here.
    pub fn bind(endpoint: &str, _handle: tokio::runtime::Handle) -> std::io::Result<Self> {
        use std::os::unix::net::UnixListener;
        let path = std::path::Path::new(endpoint);
        let _ = std::fs::remove_file(path);
        Ok(GateListener {
            listener: UnixListener::bind(path)?,
        })
    }

    /// Accept the single helper connection before `deadline` (polling
    /// non-blocking accept — UnixListener has no accept deadline).
    pub fn accept(self, deadline: Instant) -> std::io::Result<GateIo> {
        self.listener.set_nonblocking(true)?;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    return GateIo::from_unix(stream);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "gate accept deadline",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(windows)]
pub struct GateListener {
    endpoint: String,
}

#[cfg(windows)]
impl GateListener {
    /// The gate protocol is fully synchronous on Windows (plain
    /// CreateNamedPipeW), so no runtime handle is needed.
    pub fn bind(endpoint: &str, _handle: tokio::runtime::Handle) -> std::io::Result<Self> {
        Ok(GateListener {
            endpoint: endpoint.to_string(),
        })
    }

    /// Create a SYNCHRONOUS (non-overlapped) pipe server instance and wait
    /// for the helper to open it before `deadline`.
    ///
    /// The gate protocol is fully synchronous on both sides; driving it
    /// through cancellable tokio read steps (the previous approach) strands
    /// helper frames — a `timeout(50ms, read)` future dropped between polls
    /// abandons the overlapped IRP, and after RELEASE the helper's
    /// `Started` write never completes ("no start report from helper" on
    /// every managed launch). A plain `CreateNamedPipeW` without
    /// FILE_FLAG_OVERLAPPED matches the helper-side `SyncStream` exactly.
    pub fn accept(self, deadline: Instant) -> std::io::Result<GateIo> {
        use std::os::windows::io::FromRawHandle;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
        };
        use windows::Win32::System::Pipes::{
            CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
        };
        use windows::Win32::System::IO::CancelIoEx;

        const PIPE_BUFFER: u32 = 16 * 1024;
        let mut wide: Vec<u16> = self.endpoint.encode_utf16().collect();
        wide.push(0);
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_DUPLEX.0 | FILE_FLAG_FIRST_PIPE_INSTANCE.0),
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                None,
            )
        };
        if handle.is_invalid() {
            return Err(std::io::Error::last_os_error());
        }
        // ConnectNamedPipe blocks without a deadline on a sync handle; a
        // watchdog cancels it via CancelIoEx at the deadline — but ONLY if
        // the connect is still pending (cancelling after a completed
        // connect would abort in-flight gate writes instead).
        let connect = handle.0 as isize;
        let connected_early = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&connected_early);
        let watchdog = std::thread::spawn(move || {
            let deadline = deadline;
            while Instant::now() < deadline {
                if flag.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if !flag.load(std::sync::atomic::Ordering::Acquire) {
                unsafe {
                    CancelIoEx(HANDLE(connect as *mut _), None).ok();
                }
            }
        });
        // ConnectNamedPipe returns FALSE on success with ERROR_NO_DATA /
        // ERROR_PIPE_CONNECTED nuances; the canonical check is the last
        // error after the call.
        use windows::Win32::System::Pipes::ConnectNamedPipe;
        let ok = unsafe { ConnectNamedPipe(handle, None) };
        let err = std::io::Error::last_os_error();
        // Signal only AFTER the connect resolves: while ConnectNamedPipe is
        // still pending the watchdog must stay armed so a missing helper
        // times out instead of parking the launch thread forever.
        connected_early.store(true, std::sync::atomic::Ordering::Release);
        watchdog.join().ok();
        // ERROR_PIPE_CONNECTED(535)/ERROR_NO_DATA(232): a client already connected
        // between creation and ConnectNamedPipe — treat as success.
        let already = matches!(err.raw_os_error(), Some(535) | Some(232));
        if ok.is_err() && !already {
            unsafe { CloseHandle(handle).ok() };
            if err.raw_os_error() == Some(995) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "gate connect deadline",
                ));
            }
            return Err(err);
        }
        let file = unsafe { std::fs::File::from_raw_handle(handle.0 as _) };
        let stream = crate::sessions::SyncStream::from_file(file);
        Ok(GateIo {
            reader: Box::new(stream.try_clone()?),
            writer: Box::new(stream),
        })
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use crate::sessions::SyncStream;
    use std::time::Duration;

    /// 데몬 바이너리 경로 — 개발 호스트가 Windows이라 ".exe"로 굳어 있던
    /// 이름을 OS별로(macOS/Linux 통합시험 해금 — §8).
    fn daemon_bin(target_dir: &std::path::Path) -> std::path::PathBuf {
        let name = if cfg!(windows) {
            "iyagi-termd.exe"
        } else {
            "iyagi-termd"
        };
        target_dir.join(name)
    }

    #[test]
    fn gate_listener_accepts_sync_stream_from_thread() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        // Per-OS endpoint: on Unix a relative pipe-style name would bind a
        // socket file into the crate directory (test litter).
        let endpoint = if cfg!(windows) {
            format!(r"\\.\pipe\iyagi-gate-test2-{}", std::process::id())
        } else {
            format!("/tmp/iyagi-gate-thread-{}.sock", std::process::id())
        };
        let ep = endpoint.clone();
        let client_ep = endpoint.clone();
        let client = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            SyncStream::connect(&client_ep).expect("client connect")
        });
        let handle = rt.handle().clone();
        rt.block_on(async move {
            tokio::task::spawn_blocking(move || {
                let listener = GateListener::bind(&ep, handle).expect("bind");
                let _io = listener
                    .accept(Instant::now() + Duration::from_secs(5))
                    .expect("accept");
            })
            .await
            .expect("join");
        });
        client.join().expect("client thread").flush().ok();
        if !cfg!(windows) {
            let _ = std::fs::remove_file(&endpoint);
        }
    }

    /// A connected-but-silent helper must not hang the launch thread: the
    /// per-read timeout lets `DeadlineReader` enforce the gate deadline.
    #[cfg(unix)]
    #[test]
    fn silent_gate_peer_times_out_at_the_deadline() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let endpoint = format!("/tmp/iyagi-gate-silent-{}.sock", std::process::id());
        let client_ep = endpoint.clone();
        let client = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let stream = SyncStream::connect(&client_ep).expect("client connect");
            // Stay connected and say nothing for longer than the deadline.
            std::thread::sleep(Duration::from_millis(1500));
            drop(stream);
        });
        let handle = rt.handle().clone();
        rt.block_on(async move {
            tokio::task::spawn_blocking(move || {
                let listener = GateListener::bind(&endpoint, handle).expect("bind");
                let io = listener
                    .accept(Instant::now() + Duration::from_secs(5))
                    .expect("accept");
                let identity =
                    term_platform::identity::current_process_identity().expect("identity");
                let mut server = term_pty::gate::GateServer::new(io);
                let started = Instant::now();
                let err = server
                    .wait_hello(
                        "nonce",
                        &identity,
                        Instant::now() + Duration::from_millis(400),
                    )
                    .expect_err("a silent peer must not produce a hello");
                assert!(
                    matches!(err, term_pty::gate::GateError::Timeout),
                    "expected Timeout, got {err:?}"
                );
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "deadline must be enforced, waited {:?}",
                    started.elapsed()
                );
                let _ = std::fs::remove_file(&endpoint);
            })
            .await
            .expect("join");
        });
        client.join().expect("client thread");
    }

    #[test]
    fn pty_child_iyagi_termd_version_outputs() {
        let binding = std::env::current_exe().expect("test exe");
        let target = binding
            .parent()
            .and_then(|p| p.parent())
            .expect("target dir");
        let exe = daemon_bin(target).to_string_lossy().into_owned();
        let argv = vec![exe.clone(), "--version".to_string()];
        let pty = term_pty::pty::PtyHandle::spawn(
            80,
            24,
            &exe,
            &argv,
            &std::collections::BTreeMap::new(),
            &[],
            None,
        )
        .expect("spawn");
        // ConPTY never delivers EOF while the master lives; read on a worker
        // thread with a hard deadline.
        let mut reader = pty.reader().expect("reader");
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && out.len() < 200 {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(chunk) => out.extend_from_slice(&chunk),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if let Ok(Some(status)) = pty.poll_exit() {
                        eprintln!("child already exited: {status:?}");
                        break;
                    }
                }
            }
        }
        let text = String::from_utf8_lossy(&out);
        eprintln!("PTY child output: {text:?}");
        let _ = pty.kill();
        pty.close();
        assert!(text.contains("iyagi-termd"), "child produced no output");
    }

    #[test]
    fn gate_listener_accepts_the_real_helper_process() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        // Windows는 이름붙은 파이프, Unix는 도메인 소켓 — $TMPDIR 경로가
        // SUN_LEN을 넘지 않게 /tmp 아래의 짧은 경로를 쓴다(§8).
        let endpoint = if cfg!(unix) {
            format!("/tmp/iyagi-gate-test-{}.sock", std::process::id())
        } else {
            format!(r"\\.\pipe\iyagi-gate-helper-test-{}", std::process::id())
        };
        let binding = std::env::current_exe().expect("test exe");
        let target = binding
            .parent()
            .and_then(|p| p.parent())
            .expect("target dir");
        let exe = daemon_bin(target);
        let ep = endpoint.clone();
        let argv_ep = endpoint.clone();
        let handle = rt.handle().clone();
        rt.block_on(async move {
            tokio::task::spawn_blocking(move || {
                let listener = GateListener::bind(&ep, handle).expect("bind");
                // Reproduce the daemon's exact spawn: the helper as the
                // FIRST PTY child.
                let nonce = "f".repeat(64);
                let argv = vec![
                    exe.to_string_lossy().into_owned(),
                    "--launch-helper".to_string(),
                    argv_ep,
                    nonce,
                ];
                let pty = term_pty::pty::PtyHandle::spawn(
                    80,
                    24,
                    &exe.to_string_lossy(),
                    &argv,
                    &std::collections::BTreeMap::new(),
                    &[],
                    None,
                )
                .expect("spawn helper in pty");
                // Drain the helper's stderr (the PTY) for diagnostics.
                let pty_for_read = std::sync::Arc::new(pty);
                let pty_for_kill = std::sync::Arc::clone(&pty_for_read);
                let mut reader = pty_for_read.reader().expect("pty reader");
                let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                if tx.send(buf[..n].to_vec()).is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
                let io = match listener.accept(Instant::now() + Duration::from_secs(8)) {
                    Ok(io) => io,
                    Err(e) => {
                        let mut seen = Vec::new();
                        while let Ok(chunk) = rx.recv_timeout(Duration::from_millis(50)) {
                            seen.extend_from_slice(&chunk);
                        }
                        panic!(
                            "accept from helper failed: {e}; pty output: {:?}",
                            String::from_utf8_lossy(&seen)
                        );
                    }
                };
                drop(io);
                let _ = pty_for_kill.kill();
                pty_for_kill.close();
            })
            .await
            .expect("join");
        });
    }
}
