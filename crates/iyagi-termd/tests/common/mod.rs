//! Shared integration-test harness: spawn the real daemon binary on a
//! hermetic temp data dir, speak the wire protocol with a sync client.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};

/// Path of the daemon binary built by this package's test run.
pub fn daemon_bin() -> &'static str {
    env!("CARGO_BIN_EXE_iyagi-termd")
}

/// Locate the term-fixture test program. Build it with `cargo build -p
/// term-fixture` before running daemon integration tests. It is a separate
/// package, not a daemon dev-dependency. (Tests must never shell out to cargo: the target-dir lock
/// is held for the whole test run and would deadlock.)
pub fn fixture_bin() -> String {
    if let Some(env) = std::env::var_os("IYAGI_FIXTURE") {
        let path = std::path::PathBuf::from(env);
        if path.is_file() {
            return path.to_string_lossy().into_owned();
        }
    }
    let exe = std::path::PathBuf::from(daemon_bin());
    let target_debug = exe.parent().expect("daemon exe has a parent").to_path_buf();
    let candidate = target_debug.join(if cfg!(windows) {
        "term-fixture.exe"
    } else {
        "term-fixture"
    });
    assert!(
        candidate.is_file(),
        "term-fixture binary missing at {candidate:?}; run cargo build -p term-fixture first"
    );
    candidate.to_string_lossy().into_owned()
}

/// A duplex byte-stream client connection (UDS / named pipe).
pub struct Wire {
    writer: WireWriter,
    frames: Receiver<Value>,
    /// `kill()` 표식 — 닫힌 연결을 읽기 스레드(PeekReader 폴 루프)가 곧
    /// 놓아지게 한다.
    killed: Arc<AtomicBool>,
}

impl Wire {
    /// Connect with retry (the daemon may be between listener instances).
    pub fn connect(endpoint: &str) -> Wire {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let killed = Arc::new(AtomicBool::new(false));
            match connect_once(endpoint, &killed) {
                Ok((reader, mut writer)) => {
                    let (tx, rx) = mpsc::channel::<Value>();
                    let flag = Arc::clone(&killed);
                    std::thread::spawn(move || read_loop(reader, tx, flag));
                    let _ = writer.flush();
                    return Wire {
                        writer,
                        frames: rx,
                        killed,
                    };
                }
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => panic!("connect to {endpoint} failed: {e}"),
            }
        }
    }

    /// 지금 당장 연결을 닫는다 — 데몬이 이번 틱에 EOF를 본다. 그냥 drop하면
    /// 읽기 스레드가 붙들고 있는 복제 핸들 때문에 소켓/파이프가 열려 있어
    /// 데몬이 죽음을 영영 모른다(프레임이 멈춘 연결은 읽기가 블록된 채
    /// 남는다). 연결 하나만 죽이는 시험(데이터 연결 사망 → 뷰 축출)의 도구.
    pub fn kill(&mut self) {
        self.killed.store(true, Ordering::Relaxed);
        #[cfg(unix)]
        {
            // 소켓 전체를 내린다(복제 fd가 남아 있어도 즉시 EOF). 이 flag는
            // read_exact가 돌아올 수 없는 극한 상황의 예비 안전장치다.
            let _ = self.writer.shutdown(std::net::Shutdown::Both);
        }
        #[cfg(windows)]
        {
            // PeekReader의 폴 루프(2 ms마다)가 flag를 보고 빠져나간 뒤 남은
            // 핸들이 닫히면 파이프가 끊긴다.
        }
    }

    pub fn send_frame(&mut self, value: &Value) -> std::io::Result<()> {
        let body = serde_json::to_vec(value).expect("encode");
        let mut frame = (body.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&body);
        self.writer.write_all(&frame)?;
        self.writer.flush()
    }

    pub fn send_raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write raw");
        self.writer.flush().expect("flush raw");
    }

    /// Best-effort raw write (tolerates a peer that already closed).
    pub fn try_send_raw(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    /// Read one decoded frame with a timeout.
    pub fn recv_frame(&self, timeout: Duration) -> Option<Value> {
        self.frames.recv_timeout(timeout).ok()
    }
}

fn read_loop(mut reader: Box<dyn Read + Send>, tx: mpsc::Sender<Value>, killed: Arc<AtomicBool>) {
    let mut header = [0u8; 4];
    loop {
        if killed.load(Ordering::Relaxed) {
            return;
        }
        if reader.read_exact(&mut header).is_err() {
            return;
        }
        let len = u32::from_le_bytes(header) as usize;
        if len > 65_536 {
            return; // daemon will close us anyway
        }
        let mut body = vec![0u8; len];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        match serde_json::from_slice::<Value>(&body) {
            Ok(value) => {
                if tx.send(value).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

fn connect_once(
    endpoint: &str,
    killed: &Arc<AtomicBool>,
) -> std::io::Result<(Box<dyn Read + Send>, WireWriter)> {
    #[cfg(unix)]
    {
        // 이 경로에서는 쓰지 않는다(소켓 shutdown이 즉시 EOF를 만든다).
        let _ = killed;
        let stream = std::os::unix::net::UnixStream::connect(endpoint)?;
        stream.set_nonblocking(false)?;
        let writer = stream.try_clone()?;
        Ok((Box::new(stream), writer))
    }
    #[cfg(windows)]
    {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(endpoint)?;
        let writer = file.try_clone()?;
        // Windows quirk: a duplicated handle's synchronous writes serialize
        // behind a pending blocking ReadFile on the sibling handle (both
        // share one FILE_OBJECT). The reader therefore peeks instead of
        // blocking, so the writer never waits on a pending read.
        Ok((
            Box::new(PeekReader {
                file,
                killed: Arc::clone(killed),
            }),
            writer,
        ))
    }
}

/// 데이터 채널 쓰기 절반 — Windows는 이름붙은 파이프(File), Unix는
/// 도메인 소켓. 개발 호스트가 Windows이라 File로 굳어 있던 타입을
/// OS별 별칭으로 정리(macOS/Linux에서도 통합시험이 돌게 — §8).
#[cfg(unix)]
type WireWriter = std::os::unix::net::UnixStream;
#[cfg(windows)]
type WireWriter = std::fs::File;

/// Named-pipe reader that only calls `ReadFile` when bytes are known to be
/// available (Windows-only; see `connect_once`).
#[cfg(windows)]
struct PeekReader {
    file: std::fs::File,
    killed: Arc<AtomicBool>,
}

#[cfg(windows)]
impl Read for PeekReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::os::windows::io::AsRawHandle;
        loop {
            if self.killed.load(Ordering::Relaxed) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    "wire killed",
                ));
            }
            let available = unsafe { peek_available(self.file.as_raw_handle())? };
            if available > 0 {
                return self.file.read(buf);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(windows)]
unsafe fn peek_available(handle: std::os::windows::io::RawHandle) -> std::io::Result<u32> {
    #[link(name = "kernel32")]
    extern "system" {
        fn PeekNamedPipe(
            handle: std::os::windows::io::RawHandle,
            buffer: *mut u8,
            size: u32,
            read: *mut u32,
            available: *mut u32,
            left: *mut u32,
        ) -> i32;
    }
    let mut available: u32 = 0;
    let ok = PeekNamedPipe(
        handle,
        std::ptr::null_mut(),
        0,
        std::ptr::null_mut(),
        &mut available,
        std::ptr::null_mut(),
    );
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(available)
}

/// One running daemon process bound to a hermetic data dir.
pub struct DaemonProc {
    pub child: Child,
    pub data_dir: std::path::PathBuf,
    pub token: String,
    pub endpoint: String,
}

impl DaemonProc {
    pub fn spawn(tag: &str, config_overrides: Option<Value>) -> DaemonProc {
        let dir = tempfile::tempdir().expect("temp data dir");
        let data_dir = dir.keep();
        Self::spawn_on(data_dir, tag, config_overrides)
    }

    pub fn spawn_on(
        data_dir: std::path::PathBuf,
        tag: &str,
        config_overrides: Option<Value>,
    ) -> DaemonProc {
        Self::spawn_with_env(data_dir, tag, config_overrides, &[])
    }

    /// Same as [`DaemonProc::spawn_on`] plus extra environment for the daemon
    /// process. Used by suites that must redirect what the daemon observes
    /// outside its data dir (e.g. `CLAUDE_CONFIG_DIR` for agent-session
    /// resolution) instead of touching the developer's real home.
    pub fn spawn_with_env(
        data_dir: std::path::PathBuf,
        tag: &str,
        config_overrides: Option<Value>,
        env: &[(&str, &str)],
    ) -> DaemonProc {
        Self::spawn_command(
            data_dir,
            tag,
            config_overrides,
            env,
            Command::new(daemon_bin()),
        )
    }

    pub fn spawn_fixture(tag: &str) -> DaemonProc {
        Self::spawn_fixture_on(tempfile::tempdir().unwrap().keep(), tag)
    }

    pub fn spawn_fixture_on(data_dir: std::path::PathBuf, tag: &str) -> DaemonProc {
        let name = if cfg!(windows) {
            "mission-fixture-daemon.exe"
        } else {
            "mission-fixture-daemon"
        };
        let path = std::path::Path::new(daemon_bin())
            .parent()
            .unwrap()
            .join(name);
        assert!(
            path.is_file(),
            "build the test-only daemon with cargo build -p term-fixture"
        );
        let mut command = Command::new(path);
        command.arg("--protocol-fixture").arg(fixture_bin());
        Self::spawn_command(data_dir, tag, None, &[], command)
    }

    fn spawn_command(
        data_dir: std::path::PathBuf,
        tag: &str,
        config_overrides: Option<Value>,
        env: &[(&str, &str)],
        mut cmd: Command,
    ) -> DaemonProc {
        for (key, value) in env {
            cmd.env(key, value);
        }
        let stderr_path = data_dir.join("daemon-stderr.log");
        let stderr_file = std::fs::File::create(&stderr_path).expect("daemon stderr file");
        cmd.arg("--data-dir")
            .arg(&data_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr_file))
            .env(
                "RUST_LOG",
                if std::env::var_os("IYAGI_TEST_DEBUG").is_some() {
                    "iyagi_termd_lib=debug"
                } else {
                    "iyagi_termd_lib=info"
                },
            );
        if let Some(overrides) = config_overrides {
            let cfg_path = data_dir.with_extension("test-config.json");
            std::fs::write(&cfg_path, overrides.to_string()).expect("write test config");
            cmd.env("IYAGI_TEST_CONFIG", &cfg_path);
        }
        let child = cmd.spawn().expect("spawn daemon");
        let mut proc = DaemonProc {
            child,
            data_dir,
            token: String::new(),
            endpoint: String::new(),
        };
        let _ = tag;
        proc.wait_for_ready();
        proc
    }

    fn wait_for_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        let token_path = self.data_dir.join("runtime/token");
        let endpoint_path = self.data_dir.join("runtime/endpoint");
        while Instant::now() < deadline {
            if let (Ok(token), Ok(endpoint)) = (
                std::fs::read_to_string(&token_path),
                std::fs::read_to_string(&endpoint_path),
            ) {
                let token = token.trim().to_string();
                let endpoint = endpoint.trim().to_string();
                if !token.is_empty()
                    && !endpoint.is_empty()
                    && connect_once(&endpoint, &Arc::new(AtomicBool::new(false))).is_ok()
                {
                    self.token = token;
                    self.endpoint = endpoint;
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let stderr_tail =
            std::fs::read_to_string(self.data_dir.join("daemon-stderr.log")).unwrap_or_default();
        let tail: String = stderr_tail
            .lines()
            .rev()
            .take(15)
            .collect::<Vec<_>>()
            .join(" | ");
        panic!(
            "daemon did not become ready within 60s at {:?}; stderr tail: {tail}",
            self.data_dir
        );
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DaemonProc {
    fn drop(&mut self) {
        self.kill();
    }
}

/// High-level protocol client over one connection.
pub struct Client {
    wire: Wire,
    next_id: u64,
    pub events: Arc<Mutex<Vec<(String, Value)>>>,
    /// 기다리던 id가 아니라서 읽어 두기만 한 응답들(도착 순서 뒤집힘 대비).
    pending_responses: Vec<(String, Result<Value, Value>)>,
    pub conn_id: String,
    pub data_token: Option<String>,
}

impl Client {
    pub fn control(endpoint: &str, token: &str) -> (Client, Value) {
        // Windows named pipes can hand out an instance that is mid-disconnect
        // (connect succeeds, first write then gets BrokenPipe): retry the
        // handshake once on a fresh connection, mirroring real client retry.
        let mut last_err: Option<std::io::Error> = None;
        for _ in 0..3 {
            let mut wire = Wire::connect(endpoint);
            match hello(
                &mut wire,
                json!({
                    "client_id": uuid_v4(),
                    "token": token,
                    "role": "control",
                }),
            ) {
                Ok(result) => {
                    let data_token = result
                        .get("data_token")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let conn_id = result
                        .get("connection_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    return (
                        Client {
                            wire,
                            next_id: 1,
                            events: Arc::new(Mutex::new(Vec::new())),
                            pending_responses: Vec::new(),
                            conn_id,
                            data_token,
                        },
                        result,
                    );
                }
                Err(HandshakeError::Io(e)) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(HandshakeError::Rejected(value)) => {
                    panic!("hello rejected: {value}");
                }
            }
        }
        panic!("control hello failed after retries: {:?}", last_err);
    }

    pub fn data(endpoint: &str, data_token: &str) -> Client {
        let mut wire = Wire::connect(endpoint);
        hello(
            &mut wire,
            json!({
                "client_id": uuid_v4(),
                "token": data_token,
                "role": "data",
            }),
        )
        .expect("data hello");
        Client {
            wire,
            next_id: 1,
            events: Arc::new(Mutex::new(Vec::new())),
            pending_responses: Vec::new(),
            conn_id: String::new(),
            data_token: None,
        }
    }

    /// Send a request; returns `Ok(result)` or `Err(error object)`.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let id = format!("req-{}", self.next_id);
        self.next_id += 1;
        self.wire
            .send_frame(&json!({
                "v": 1,
                "id": id,
                "method": method,
                "params": params,
            }))
            .expect("write request");
        self.wait_response(&id, Duration::from_secs(15))
            .unwrap_or_else(|| panic!("no response for {method} within 15s"))
    }

    pub fn request_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Option<Result<Value, Value>> {
        let id = format!("req-{}", self.next_id);
        self.next_id += 1;
        self.wire
            .send_frame(&json!({
                "v": 1,
                "id": id,
                "method": method,
                "params": params,
            }))
            .expect("write request (timeout variant)");
        self.wait_response(&id, timeout)
    }

    /// Like `request`, but transport failures (write error, connection
    /// death, response timeout) come back as
    /// `Err({"code": "CONNECTION_LOST"})` instead of panicking — callers
    /// can reconnect and retry idempotent requests.
    pub fn request_lossy(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let id = format!("req-{}", self.next_id);
        self.next_id += 1;
        if self
            .wire
            .send_frame(&json!({
                "v": 1,
                "id": id,
                "method": method,
                "params": params,
            }))
            .is_err()
        {
            return Err(json!({"code": "CONNECTION_LOST", "message": "write failed"}));
        }
        self.wait_response(&id, Duration::from_secs(15))
            .unwrap_or_else(|| {
                Err(json!({"code": "CONNECTION_LOST", "message": "no response within 15s"}))
            })
    }

    fn wait_response(&mut self, id: &str, timeout: Duration) -> Option<Result<Value, Value>> {
        // 먼저 지나갔던 응답이면 그것을 돌려준다 — 기다리는 id가 아닌 응답을
        // 버리면, 교차하는 두 응답(deferred 완료 vs 인라인)의 도착 순서가 예상과
        // 다를 때 나중의 wait가 영영 받지 못한다.
        if let Some(pos) = self.pending_responses.iter().position(|(rid, _)| rid == id) {
            let (_, reply) = self.pending_responses.remove(pos);
            return Some(reply);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let frame = self.wire.recv_frame(remaining)?;
            if let Some(frame_id) = frame.get("id").and_then(|v| v.as_str()) {
                let reply = if let Some(result) = frame.get("result") {
                    Ok(result.clone())
                } else {
                    Err(frame.get("error").cloned().unwrap_or(Value::Null))
                };
                if frame_id == id {
                    return Some(reply);
                }
                self.pending_responses.push((frame_id.to_string(), reply));
                continue;
            }
            self.stash_event(frame);
        }
    }

    fn stash_event(&self, frame: Value) {
        if let Some(event) = frame.get("event").and_then(|v| v.as_str()) {
            let payload = frame.get("payload").cloned().unwrap_or(Value::Null);
            self.events
                .lock()
                .expect("events")
                .push((event.to_string(), payload));
        }
    }

    /// Wait for one event of `kind` (scanning already-stashed ones first).
    pub fn wait_event(&mut self, kind: &str, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let mut events = self.events.lock().expect("events");
                if let Some(pos) = events.iter().position(|(k, _)| k == kind) {
                    let (_, payload) = events.remove(pos);
                    return Some(payload);
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let frame = self.wire.recv_frame(remaining)?;
            self.stash_event(frame);
        }
    }

    /// Receive any next frame (stashing events) — for data connections
    /// streaming `session.output`.
    pub fn recv_any(&self, timeout: Duration) -> Option<Value> {
        let frame = self.wire.frames.recv_timeout(timeout).ok()?;
        self.stash_event(frame.clone());
        Some(frame)
    }

    /// Take one stashed event of `kind` without waiting (None if absent).
    pub fn pop_event(&self, kind: &str) -> Option<Value> {
        let mut events = self.events.lock().expect("events");
        events
            .iter()
            .position(|(k, _)| k == kind)
            .map(|pos| events.remove(pos).1)
    }

    /// True when the server has closed our connection (read EOF).
    pub fn closed(&self, timeout: Duration) -> bool {
        match self.wire.frames.recv_timeout(timeout) {
            Ok(frame) => {
                self.stash_event(frame.clone());
                false
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => true,
            Err(mpsc::RecvTimeoutError::Timeout) => false,
        }
    }

    /// 이 연결을 강제로 닫는다(데몬이 즉시 EOF를 본다). [`Wire::kill`] 참조.
    pub fn kill(&mut self) {
        self.wire.kill();
    }

    pub fn send_ack(&mut self, session_id: &str, epoch: &str, through_seq: u64) -> String {
        let id = format!("req-{}", self.next_id);
        self.next_id += 1;
        let _ = self.wire.send_frame(&json!({
            "v": 1,
            "id": id,
            "method": "session.ack",
            "params": {
                "session_id": session_id,
                "epoch": epoch,
                "through_seq": through_seq.to_string(),
            },
        }));
        id
    }

    /// Send a request without waiting for its response (crash-race tests).
    pub fn fire(&mut self, method: &str, params: Value) {
        let id = format!("req-{}", self.next_id);
        self.next_id += 1;
        let _ = self.wire.send_frame(&json!({
            "v": 1,
            "id": id,
            "method": method,
            "params": params,
        }));
    }

    /// Wait for the response to a previously fired request id.
    pub fn wait_fired(&mut self, id: &str, timeout: Duration) -> Option<Result<Value, Value>> {
        self.wait_response(id, timeout)
    }

    /// The id the next `fire`/`request` call will use.
    pub fn next_request_id(&self) -> String {
        format!("req-{}", self.next_id)
    }
}

#[derive(Debug)]
enum HandshakeError {
    Io(std::io::Error),
    Rejected(Value),
}

fn hello(wire: &mut Wire, params: Value) -> Result<Value, HandshakeError> {
    wire.send_frame(&json!({
        "v": 1,
        "id": "hello",
        "method": "hello",
        "params": params,
    }))
    .map_err(HandshakeError::Io)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HandshakeError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "hello response timeout",
            )));
        }
        let frame = wire.recv_frame(remaining).ok_or_else(|| {
            HandshakeError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "connection closed before hello response",
            ))
        })?;
        if frame.get("id").and_then(|v| v.as_str()) == Some("hello") {
            if let Some(result) = frame.get("result") {
                return Ok(result.clone());
            }
            return Err(HandshakeError::Rejected(
                frame.get("error").cloned().unwrap_or(Value::Null),
            ));
        }
    }
}

pub fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Control client that survives transient named-pipe connection loss by
/// reconnecting and retrying. The Windows pipe listener can hand out an
/// instance that is mid-disconnect (see `Client::control`), so long storms
/// occasionally lose a live connection. Retries MUST be idempotent at the
/// daemon (same request id → same workload).
pub struct RetryClient {
    endpoint: String,
    token: String,
    client: Client,
}

impl RetryClient {
    pub fn new(endpoint: &str, token: &str) -> RetryClient {
        let (client, _) = Client::control(endpoint, token);
        RetryClient {
            endpoint: endpoint.to_string(),
            token: token.to_string(),
            client,
        }
    }

    /// Request with up to 3 reconnect+retry rounds on transport loss.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let mut last = Err(json!({"code": "CONNECTION_LOST", "message": "never attempted"}));
        for _ in 0..3 {
            last = self.client.request_lossy(method, params.clone());
            if !matches!(&last, Err(e) if e["code"] == "CONNECTION_LOST") {
                return last;
            }
            self.client = Client::control(&self.endpoint, &self.token).0;
        }
        last
    }
}

pub fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn unb64(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Launch-request builder + snapshot polling shared by the B-series suites.

/// Managed/shell launch request with a tiny reservation (admission math
/// stays out of the way of whatever the suite is really testing).
pub fn launch_request(mode: &str, argv: &[&str], reservation_bytes: &str) -> Value {
    json!({
        "request_id": uuid_v4(),
        "profile_id": uuid_v4(),
        "cwd": std::env::temp_dir().to_string_lossy(),
        "program": fixture_bin(),
        "argv": argv,
        "env_overrides": {},
        "mode": mode,
        "cols": 80,
        "rows": 24,
        "priority": 1,
        "policy": {
            "reservation_bytes": reservation_bytes,
            "cpu_slots": 1,
            "enforcement": "observe",
            "memory_max_bytes": null,
            "cpu_max_cores": null,
            "pids_max": null,
        },
    })
}

/// This workload's summary from a fresh `system.snapshot` (None if absent).
pub fn snapshot_workload(client: &mut Client, workload_id: &serde_json::Value) -> Option<Value> {
    let snapshot = client
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    snapshot["workloads"]
        .as_array()
        .expect("workloads array")
        .iter()
        .find(|w| w["workload_id"] == *workload_id)
        .cloned()
}

/// Poll snapshots until the workload reaches one of `wanted` (panic on
/// timeout). Returns the matching summary.
pub fn wait_workload_state(
    client: &mut Client,
    workload_id: &serde_json::Value,
    wanted: &[&str],
    timeout: Duration,
) -> Value {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(summary) = snapshot_workload(client, workload_id) {
            if let Some(state) = summary["state"].as_str() {
                if wanted.contains(&state) {
                    return summary;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "workload {workload_id} never reached {wanted:?} within {:?}; last: {:?}",
        timeout,
        snapshot_workload(client, workload_id)
    );
}

/// A managed launch may legally answer QUEUED (wait state — e.g. a transient
/// telemetry miss at daemon startup); it must reach RUNNING promptly.
pub fn ensure_running(client: &mut Client, launch: &Value, timeout: Duration) -> Value {
    if launch["state"] == "RUNNING" {
        return launch.clone();
    }
    assert_eq!(
        launch["state"], "QUEUED",
        "launch must answer RUNNING or QUEUED, got {launch}"
    );
    wait_workload_state(client, &launch["workload_id"], &["RUNNING"], timeout)
}

/// Kill like `kill -9` (taskkill /F /T on Windows — no graceful teardown,
/// no chance to clean up, exactly the crash the suites inject).
pub fn force_kill(child: &mut Child) {
    if cfg!(windows) {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID"])
            .arg(child.id().to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    } else {
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(child.id().to_string())
            .status();
    }
    let _ = child.wait();
}

/// Wait for a process to exit on its own; None on timeout.
pub fn wait_exit(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
    None
}

/// Number of regular entries in a directory (0 when missing).
pub fn count_entries(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(|e| e.ok()).count())
        .unwrap_or(0)
}

/// Fold the admission-relaxing override into a test config: suites whose
/// subject is NOT admission must not depend on the host's transient memory
/// pressure (a loaded developer machine can legitimately sit at CRITICAL
/// pressure and would otherwise gate every managed launch behind
/// WAIT_HOST_PRESSURE forever).
pub fn relaxed_admission(mut config: Value) -> Value {
    if !config.is_object() {
        config = json!({});
    }
    if let Some(map) = config.as_object_mut() {
        map.insert(
            "admission".to_string(),
            json!({
                "host_reserve_min_bytes": 1,
                "host_reserve_percent": 0,
                "managed_budget_percent": 100,
                "critical_available_percent": 0,
                "critical_available_bytes": 0,
                "warning_available_percent": 0,
                "recovery_available_percent": 0,
            }),
        );
    }
    config
}

/// Remove CR/LF, C0 controls and ANSI escape sequences (CSI `ESC[...X`,
/// OSC `ESC]...BEL`, two-char ESC sequences) from a PTY byte stream.
/// ConPTY's re-encoded output wraps long lines and inserts cursor-position
/// sequences; the wrapped text itself survives, so stripping control/escape
/// runs reassembles it.
pub fn strip_terminal_controls(input: &[u8]) -> Vec<u8> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Text,
        Escape,
        Csi,
        Osc,
    }
    let mut out = Vec::with_capacity(input.len());
    let mut state = State::Text;
    for &b in input {
        match state {
            State::Text => match b {
                0x1b => state = State::Escape,
                b'\r' | b'\n' | 0x00..=0x1f | 0x7f => {}
                _ => out.push(b),
            },
            State::Escape => match b {
                b'[' => state = State::Csi,
                b']' => state = State::Osc,
                0x1b => state = State::Escape,
                _ => state = State::Text,
            },
            // CSI: parameters 0x30-0x3F, intermediates 0x20-0x2F, final 0x40-0x7E.
            State::Csi => {
                if (0x40..=0x7e).contains(&b) {
                    state = State::Text;
                }
            }
            // OSC: terminated by BEL or ST (ESC \).
            State::Osc => {
                if b == 0x07 {
                    state = State::Text;
                } else if b == 0x1b {
                    state = State::Escape;
                }
            }
        }
    }
    out
}
