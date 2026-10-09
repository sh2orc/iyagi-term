//! Control/data connections speaking term-contracts frames over a boxed
//! async stream (UDS on Unix, named pipe on Windows, in-memory duplex pair
//! in tests). Never owns PTYs — the daemon does (IMPLEMENTATION_SPEC §3).

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use uuid::Uuid;

use term_contracts::error::{ErrorCode, RpcError};
use term_contracts::rpc::{
    self, Frame, FrameError, HelloParams, HelloResult, HelloRole, RpcRequest, RpcResponse,
};

use super::codec;

/// First frame must be `hello` within 2s (`01-contracts.md` §3).
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
/// RPC timeout default (`01-contracts.md` §4) → `DAEMON_UNAVAILABLE`, retryable.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(5);
/// Outgoing frame queue depth before backpressure.
pub const WRITER_QUEUE: usize = 64;

/// This app build's version id, the same shape the daemon stamps into
/// `HelloResult.daemon_version`: `format!("{CARGO_PKG_VERSION} ({IYAGI_GIT_SHA})")`.
/// `IYAGI_GIT_SHA` includes the commit and shared daemon-source fingerprint,
/// so uncommitted dev rebuilds also change it. Compared to the connected daemon's
/// `daemon_version` to detect a stale, still-running daemon.
pub fn app_build_version() -> String {
    format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("IYAGI_GIT_SHA"))
}

/// Pure version-compare: the daemon is "outdated" when its reported build id
/// differs from this app's. An OLD daemon predates `daemon_version` and reports
/// `""` (serde default) — always different, so always outdated. Equal ids
/// (app and daemon built from the same source) are never outdated.
pub fn daemon_is_outdated(app_version: &str, daemon_version: &str) -> bool {
    app_version != daemon_version
}

/// Whether restarting would bring up a different daemon — the "outdated"
/// flag the UI turns into a restart offer. A restart spawns the binary on
/// disk, so when that binary's build id is known it is the only meaningful
/// reference: equal to the running daemon means a restart changes nothing
/// (e.g. only the daemon was rebuilt and is already running, while the app
/// still carries the older id — offering a restart there kills every
/// terminal for no gain, forever). Unknown on-disk id → compare with the app.
pub fn restart_would_update(
    app_version: &str,
    on_disk: Option<&str>,
    daemon_version: &str,
) -> bool {
    match on_disk {
        Some(next) => daemon_is_outdated(next, daemon_version),
        None => daemon_is_outdated(app_version, daemon_version),
    }
}

/// Bridge error surface. Transport/frame/io causes map onto spec error codes;
/// constructed messages never embed tokens or user payloads.
#[derive(Debug)]
pub enum BridgeError {
    Rpc(RpcError),
    /// An error body whose code is outside this build's R1 vocabulary (the
    /// O1 mission codes, or a future peer): carried verbatim so the webview
    /// sees the real `{code, message, retryable, details}` (01 §7).
    RpcRaw(serde_json::Value),
    Frame(FrameError),
    Io(std::io::Error),
}

impl BridgeError {
    pub fn daemon_unavailable(message: impl Into<String>) -> Self {
        BridgeError::Rpc(RpcError::new(ErrorCode::DaemonUnavailable, message))
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        BridgeError::Rpc(RpcError::new(ErrorCode::InvalidArgument, message))
    }

    pub fn into_rpc(self) -> RpcError {
        match self {
            BridgeError::Rpc(err) => err,
            BridgeError::RpcRaw(value) => RpcError::new(
                ErrorCode::ProtocolMismatch,
                format!("raw rpc error: {value}"),
            ),
            BridgeError::Frame(err) => RpcError::new(
                ErrorCode::DaemonUnavailable,
                format!("bridge frame error: {err}"),
            ),
            BridgeError::Io(err) => RpcError::new(
                ErrorCode::DaemonUnavailable,
                format!("bridge io error: {err}"),
            ),
        }
    }
}

impl From<RpcError> for BridgeError {
    fn from(value: RpcError) -> Self {
        BridgeError::Rpc(value)
    }
}

impl From<FrameError> for BridgeError {
    fn from(value: FrameError) -> Self {
        BridgeError::Frame(value)
    }
}

impl From<std::io::Error> for BridgeError {
    fn from(value: std::io::Error) -> Self {
        BridgeError::Io(value)
    }
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BridgeError::Rpc(err) => write!(f, "{err}"),
            BridgeError::RpcRaw(value) => write!(f, "rpc error: {value}"),
            BridgeError::Frame(err) => write!(f, "frame error: {err}"),
            BridgeError::Io(err) => write!(f, "io error: {err}"),
        }
    }
}

impl std::error::Error for BridgeError {}

/// Any full-duplex async byte stream (UDS, named pipe, test duplex).
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
pub type BoxedStream = Box<dyn Stream>;

/// Connects a daemon endpoint string (socket path / pipe name).
pub trait Transport: Send + Sync + 'static {
    fn connect<'a>(
        &'a self,
        endpoint: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<BoxedStream, BridgeError>> + Send + 'a>>;
}

/// OS transport: named pipe on Windows, UDS on Unix.
pub struct OsTransport;

#[cfg(windows)]
impl Transport for OsTransport {
    fn connect<'a>(
        &'a self,
        endpoint: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<BoxedStream, BridgeError>> + Send + 'a>> {
        Box::pin(async move {
            use tokio::net::windows::named_pipe::ClientOptions;
            // Retry while another client holds the pipe instance (ERROR_PIPE_BUSY).
            const ERROR_PIPE_BUSY: i32 = 231;
            let name = normalize_pipe_name(endpoint);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            loop {
                match ClientOptions::new().open(&name) {
                    Ok(client) => return Ok(Box::new(client) as BoxedStream),
                    Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                        if tokio::time::Instant::now() >= deadline {
                            return Err(BridgeError::daemon_unavailable(
                                "daemon named pipe stayed busy",
                            ));
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    Err(err) => return Err(BridgeError::Io(err)),
                }
            }
        })
    }
}

#[cfg(windows)]
fn normalize_pipe_name(endpoint: &str) -> String {
    let trimmed = endpoint.trim();
    if trimmed.starts_with(r"\\.\pipe\") || trimmed.starts_with(r"\\?\pipe\") {
        trimmed.to_string()
    } else {
        format!(r"\\.\pipe\{trimmed}")
    }
}

#[cfg(unix)]
impl Transport for OsTransport {
    fn connect<'a>(
        &'a self,
        endpoint: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<BoxedStream, BridgeError>> + Send + 'a>> {
        Box::pin(async move {
            let stream = tokio::net::UnixStream::connect(endpoint).await?;
            Ok(Box::new(stream) as BoxedStream)
        })
    }
}

/// Reader-side response: the typed envelope when it parses, otherwise the
/// raw object (unknown error codes from newer peers).
enum RawResponse {
    Typed(RpcResponse),
    Lenient {
        result: Option<serde_json::Value>,
        error: Option<serde_json::Value>,
    },
}

type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<RawResponse>>>>;

/// One framed daemon connection: hello handshake, an id-matched pending map
/// for RPCs, a single writer task, and a reader task that broadcasts events.
pub struct Connection {
    outgoing: mpsc::Sender<Value>,
    pending: PendingMap,
    closed: Arc<AtomicBool>,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
}

impl Connection {
    /// Perform the hello handshake on a fresh stream, then start the
    /// reader/writer loops. Only control connections return a HelloResult;
    /// data connections acknowledge the link with connection_id and role.
    pub async fn open(
        stream: BoxedStream,
        token: &str,
        role: HelloRole,
    ) -> Result<
        (
            Connection,
            Option<HelloResult>,
            mpsc::UnboundedReceiver<Frame>,
        ),
        BridgeError,
    > {
        let (mut read_half, mut write_half) = tokio::io::split(stream);

        let hello_id = Uuid::new_v4().to_string();
        let params = serde_json::to_value(HelloParams {
            client_id: Uuid::new_v4().to_string(),
            token: token.to_string(),
            role,
        })
        .map_err(|e| BridgeError::Frame(FrameError::InvalidJson(e.to_string())))?;
        let request = RpcRequest::new(hello_id.clone(), rpc::methods::HELLO, params);
        let value = serde_json::to_value(&request).expect("rpc request serializes");
        codec::write_frame(&mut write_half, &value).await?;

        enum HelloOutcome {
            Typed(RpcResponse),
            Lenient {
                error: Option<serde_json::Value>,
                result: Option<Value>,
            },
        }
        let response = timeout(HELLO_TIMEOUT, async {
            loop {
                let value = codec::read_frame(&mut read_half).await?;
                if let Ok(resp) = Frame::from_json(value.clone()) {
                    if let Frame::Response(resp) = resp {
                        if resp.id == hello_id {
                            return Ok::<HelloOutcome, BridgeError>(HelloOutcome::Typed(resp));
                        }
                    }
                    // Unrelated pre-hello traffic is ignored, not fatal.
                    continue;
                }
                if looks_like_response(&value)
                    && value.get("id").and_then(|v| v.as_str()) == Some(hello_id.as_str())
                {
                    return Ok(HelloOutcome::Lenient {
                        error: value.get("error").cloned(),
                        result: value.get("result").cloned(),
                    });
                }
            }
        })
        .await
        .map_err(|_| BridgeError::daemon_unavailable("daemon hello timed out"))??;

        let result = match response {
            HelloOutcome::Typed(resp) => {
                if let Some(err) = resp.error {
                    return Err(BridgeError::Rpc(err));
                }
                resp.result.unwrap_or(Value::Null)
            }
            HelloOutcome::Lenient { error, result } => {
                if let Some(raw) = error {
                    return Err(BridgeError::RpcRaw(raw));
                }
                result.unwrap_or(Value::Null)
            }
        };
        let malformed = |e: serde_json::Error| {
            BridgeError::Rpc(RpcError::new(
                ErrorCode::ProtocolMismatch,
                format!("hello result malformed: {e}"),
            ))
        };
        let hello = match role {
            HelloRole::Control => {
                let hello: HelloResult = serde_json::from_value(result).map_err(malformed)?;
                if hello.protocol != rpc::PROTOCOL_VERSION {
                    return Err(BridgeError::Rpc(RpcError::new(
                        ErrorCode::ProtocolMismatch,
                        "daemon protocol version differs from the bridge",
                    )));
                }
                Some(hello)
            }
            HelloRole::Data => {
                #[derive(serde::Deserialize)]
                struct DataHello {
                    #[serde(rename = "connection_id")]
                    _connection_id: term_contracts::ids::ConnectionId,
                    role: HelloRole,
                }
                let ack: DataHello = serde_json::from_value(result).map_err(malformed)?;
                if ack.role != HelloRole::Data {
                    return Err(BridgeError::Rpc(RpcError::new(
                        ErrorCode::ProtocolMismatch,
                        "expected data connection acknowledgement",
                    )));
                }
                None
            }
        };

        let (out_tx, out_rx) = mpsc::channel::<Value>(WRITER_QUEUE);
        let (event_tx, event_rx) = mpsc::unbounded_channel::<Frame>();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn(reader_loop(
            read_half,
            pending.clone(),
            event_tx,
            closed.clone(),
        ));
        let writer = tokio::spawn(writer_loop(write_half, out_rx, closed.clone()));
        Ok((
            Connection {
                outgoing: out_tx,
                pending,
                closed,
                reader,
                writer,
            },
            hello,
            event_rx,
        ))
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Issue one RPC and wait for the id-matched response (5s default).
    pub async fn call(&self, id: &str, method: &str, params: Value) -> Result<Value, BridgeError> {
        self.call_with_timeout(id, method, params, RPC_TIMEOUT)
            .await
    }

    pub async fn call_with_timeout(
        &self,
        id: &str,
        method: &str,
        params: Value,
        limit: Duration,
    ) -> Result<Value, BridgeError> {
        let request = RpcRequest::new(id, method, params);
        let value = serde_json::to_value(&request).expect("rpc request serializes");
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.to_string(), tx);
        if self.outgoing.send(value).await.is_err() {
            self.pending.lock().await.remove(id);
            return Err(BridgeError::daemon_unavailable("bridge writer closed"));
        }
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(resp)) => match resp {
                RawResponse::Typed(resp) => match resp.error {
                    Some(err) => Err(BridgeError::Rpc(err)),
                    None => Ok(resp.result.unwrap_or(Value::Null)),
                },
                RawResponse::Lenient { error, result, .. } => {
                    if let Some(raw) = error {
                        Err(BridgeError::RpcRaw(raw))
                    } else {
                        Ok(result.unwrap_or(Value::Null))
                    }
                }
            },
            // Reader dropped the pending entry: the connection died mid-call.
            Ok(Err(_)) => Err(BridgeError::daemon_unavailable(
                "connection closed during rpc",
            )),
            // The connection is still up — only this call was slow (a large
            // journal search, a snapshot under load). BUSY lets the client
            // retry once without tearing down every session's output route,
            // which a DAEMON_UNAVAILABLE reconnect would do.
            Err(_) => {
                self.pending.lock().await.remove(id);
                Err(BridgeError::Rpc(RpcError::new(
                    ErrorCode::Busy,
                    "rpc timed out",
                )))
            }
        }
    }

    /// Fire-and-forget request frame (`session.ack` on the data connection:
    /// 정상 처리 시 별도 응답 없음 — 01 §4).
    pub async fn send_only(&self, request: RpcRequest) -> Result<(), BridgeError> {
        let value = serde_json::to_value(&request).expect("rpc request serializes");
        self.outgoing
            .send(value)
            .await
            .map_err(|_| BridgeError::daemon_unavailable("bridge writer closed"))
    }

    /// Abort the loops and mark the connection closed. Safe to call twice.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.reader.abort();
        self.writer.abort();
    }
}

/// A dropped `JoinHandle` detaches its task instead of aborting it. Without
/// this, any path that let go of a `Connection` without `close()` (e.g. a
/// half-dead data connection being replaced) leaked a reader task — and the
/// forwarder task behind it — for the rest of the process lifetime.
impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

async fn reader_loop<R>(
    mut read: R,
    pending: PendingMap,
    events: mpsc::UnboundedSender<Frame>,
    closed: Arc<AtomicBool>,
) where
    R: AsyncRead + Unpin,
{
    loop {
        let frame = match codec::read_frame(&mut read).await {
            Ok(value) => value,
            Err(_) => break, // EOF or malformed stream: close (01 §3).
        };
        match Frame::from_json(frame.clone()) {
            Ok(Frame::Response(resp)) => {
                let mut map = pending.lock().await;
                if let Some(tx) = map.remove(&resp.id) {
                    let _ = tx.send(RawResponse::Typed(resp));
                }
            }
            Err(_) if looks_like_response(&frame) => {
                // Unknown error code (O1/future peer): resolve verbatim.
                let id = frame
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut map = pending.lock().await;
                if let Some(tx) = map.remove(&id) {
                    let _ = tx.send(RawResponse::Lenient {
                        result: frame.get("result").cloned(),
                        error: frame.get("error").cloned(),
                    });
                }
            }
            Ok(other) => {
                if events.send(other).is_err() {
                    break;
                }
            }
            Err(err) => {
                // A single malformed frame does not close the connection
                // (read_frame already validated the byte level); malformed
                // envelopes are dropped with a redaction-free note.
                tracing::warn!(error = %err, "bridge: dropped malformed envelope from daemon");
            }
        }
    }
    closed.store(true, Ordering::SeqCst);
    let mut map = pending.lock().await;
    for (_, tx) in map.drain() {
        let _ = tx.send(RawResponse::Typed(RpcResponse::err(
            "connection-closed",
            RpcError::new(ErrorCode::DaemonUnavailable, "daemon connection closed"),
        )));
    }
}

/// A response-shaped envelope: no `method`/`event` keys, `id` present.
fn looks_like_response(value: &serde_json::Value) -> bool {
    value.get("method").is_none() && value.get("event").is_none() && value.get("id").is_some()
}

async fn writer_loop<W>(mut write: W, mut rx: mpsc::Receiver<Value>, closed: Arc<AtomicBool>)
where
    W: AsyncWrite + Unpin,
{
    while let Some(value) = rx.recv().await {
        match rpc::encode_frame(&value) {
            Ok(bytes) => {
                if write.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "bridge: refused to encode outgoing frame");
            }
        }
    }
    closed.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use term_contracts::rpc::RpcEventKind;
    use tokio::io::{duplex, AsyncReadExt, DuplexStream};

    const SECRET_TOKEN: &str = "bridge-test-secret-token";
    const DATA_TOKEN: &str = "bridge-test-data-token";

    fn hello_result_json(data_token: &str) -> Value {
        json!({
            "daemon_id": "daemon-1",
            "protocol": 1,
            "connection_id": Uuid::new_v4().to_string(),
            "data_token": data_token,
            "daemon_version": "test-daemon-version",
            "capabilities": {
                "memory_limit_kind": {"support": "unsupported", "reason": "test"},
                "cpu_quota": {"support": "supported"},
                "process_count_limit": {"support": "supported"},
                "tree_accounting": {"support": "supported"},
                "reattach": {"support": "supported"},
                "resume": {"support": "unsupported", "reason": "test"},
                "platform": "test",
            }
        })
    }

    async fn read_request(stream: &mut DuplexStream) -> Value {
        // Read one raw frame off the daemon-side duplex end.
        let mut header = [0u8; 4];
        stream.read_exact(&mut header).await.unwrap();
        let mut body = vec![0u8; u32::from_le_bytes(header) as usize];
        stream.read_exact(&mut body).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn write_value(stream: &mut DuplexStream, value: Value) {
        let bytes = term_contracts::rpc::encode_frame(&value).unwrap();
        use tokio::io::AsyncWriteExt;
        stream.write_all(&bytes).await.unwrap();
    }

    #[tokio::test]
    async fn handshake_returns_hello_result_and_mints_data_token() {
        let (client, mut daemon) = duplex(4096);
        let peer = tokio::spawn(async move {
            let request = read_request(&mut daemon).await;
            assert_eq!(request["method"], "hello");
            assert_eq!(request["params"]["role"], "control");
            write_value(
                &mut daemon,
                json!({"v": 1, "id": request["id"], "result": hello_result_json(DATA_TOKEN)}),
            )
            .await;
        });

        let (_conn, hello, _events) =
            Connection::open(Box::new(client), SECRET_TOKEN, HelloRole::Control)
                .await
                .unwrap();
        peer.await.unwrap();
        let hello = hello.expect("control hello");
        assert_eq!(hello.protocol, 1);
        assert_eq!(hello.data_token, DATA_TOKEN);
        assert_eq!(hello.capabilities.platform, "test");
        assert_eq!(hello.daemon_version, "test-daemon-version");
    }

    #[test]
    fn daemon_is_outdated_only_when_build_ids_differ() {
        // Same build id → current.
        assert!(!daemon_is_outdated("0.1.0 (abc1234)", "0.1.0 (abc1234)"));
        // Different sha (rebuilt app, stale daemon) → outdated.
        assert!(daemon_is_outdated("0.1.0 (abc1234)", "0.1.0 (def5678)"));
        // OLD daemon predates the field and reports "" → outdated.
        assert!(daemon_is_outdated("0.1.0 (abc1234)", ""));
        // The app never reports an empty build id, so it never matches "".
        assert!(!app_build_version().is_empty());
    }

    #[test]
    fn restart_is_offered_only_when_it_would_bring_up_a_different_daemon() {
        let app = "0.1.0 (app1111)";
        // Rebuilt daemon binary, old process still running → restart helps.
        assert!(restart_would_update(
            app,
            Some("0.1.0 (new2222)"),
            "0.1.0 (old3333)"
        ));
        // Only the daemon was rebuilt and it already runs: the app id differs,
        // but a restart would spawn the very same binary → nothing to offer.
        assert!(!restart_would_update(
            app,
            Some("0.1.0 (new2222)"),
            "0.1.0 (new2222)"
        ));
        // Updated app bundle (new daemon on disk) while the old one runs.
        assert!(restart_would_update(app, Some(app), "0.1.0 (old3333)"));
        // On-disk id unknown (no binary / no --version): fall back to the app.
        assert!(restart_would_update(app, None, "0.1.0 (old3333)"));
        assert!(!restart_would_update(app, None, app));
    }

    #[tokio::test]
    async fn hello_error_propagates_without_leaking_the_token() {
        let (client, mut daemon) = duplex(4096);
        let peer = tokio::spawn(async move {
            let request = read_request(&mut daemon).await;
            write_value(
                &mut daemon,
                json!({"v": 1, "id": request["id"], "error": {
                    "code": "INVALID_ARGUMENT", "message": "token rejected", "retryable": false
                }}),
            )
            .await;
        });

        let err = match Connection::open(Box::new(client), SECRET_TOKEN, HelloRole::Control).await {
            Err(err) => err,
            Ok(_) => panic!("expected hello to fail"),
        };
        peer.await.unwrap();
        let rpc = err.into_rpc();
        assert_eq!(rpc.code, ErrorCode::InvalidArgument);
        let rendered = format!("{rpc}");
        assert!(rendered.contains("token rejected"));
        assert!(!rendered.contains(SECRET_TOKEN));
    }

    #[tokio::test]
    async fn protocol_mismatch_is_reported() {
        let (client, mut daemon) = duplex(4096);
        let peer = tokio::spawn(async move {
            let request = read_request(&mut daemon).await;
            write_value(
                &mut daemon,
                json!({"v": 1, "id": request["id"], "result": {
                    "daemon_id": "d", "protocol": 2,
                    "connection_id": Uuid::new_v4().to_string(),
                    "data_token": "t",
                    "capabilities": hello_result_json("t")["capabilities"]
                }}),
            )
            .await;
        });
        let err = match Connection::open(Box::new(client), "tok", HelloRole::Control).await {
            Err(err) => err.into_rpc(),
            Ok(_) => panic!("expected hello to fail"),
        };
        peer.await.unwrap();
        assert_eq!(err.code, ErrorCode::ProtocolMismatch);
    }

    #[tokio::test]
    async fn concurrent_calls_match_ids_even_with_reversed_responses() {
        let (client, mut daemon) = duplex(8192);
        let peer = tokio::spawn(async move {
            let request = read_request(&mut daemon).await; // hello
            write_value(
                &mut daemon,
                json!({"v": 1, "id": request["id"], "result": hello_result_json(DATA_TOKEN)}),
            )
            .await;
            // Four concurrent RPCs; answer in REVERSE order to prove the
            // pending map matches ids, not arrival order.
            let mut requests = Vec::new();
            for _ in 0..4 {
                requests.push(read_request(&mut daemon).await);
            }
            for request in requests.iter().rev() {
                write_value(
                    &mut daemon,
                    json!({"v": 1, "id": request["id"], "result": {"echo_id": request["id"], "method": request["method"]}}),
                )
                .await;
            }
        });

        let (conn, _hello, _events) = Connection::open(Box::new(client), "tok", HelloRole::Control)
            .await
            .unwrap();
        let conn = Arc::new(conn);
        let mut handles = Vec::new();
        for i in 0..4 {
            let conn = Arc::clone(&conn);
            handles.push(tokio::spawn(async move {
                conn.call(&format!("rpc-{i}"), "session.input", json!({"n": i}))
                    .await
                    .unwrap()
            }));
        }
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        peer.await.unwrap();

        let mut by_id: Vec<(String, String)> = results
            .iter()
            .map(|r| {
                (
                    r["echo_id"].as_str().unwrap().to_string(),
                    r["method"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        by_id.sort();
        assert_eq!(
            by_id,
            vec![
                ("rpc-0".to_string(), "session.input".to_string()),
                ("rpc-1".to_string(), "session.input".to_string()),
                ("rpc-2".to_string(), "session.input".to_string()),
                ("rpc-3".to_string(), "session.input".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn rpc_error_response_maps_to_rpc_error() {
        let (client, mut daemon) = duplex(4096);
        let peer = tokio::spawn(async move {
            let request = read_request(&mut daemon).await; // hello
            write_value(
                &mut daemon,
                json!({"v": 1, "id": request["id"], "result": hello_result_json(DATA_TOKEN)}),
            )
            .await;
            let request = read_request(&mut daemon).await;
            write_value(
                &mut daemon,
                json!({"v": 1, "id": request["id"], "error": {
                    "code": "STALE_EPOCH", "message": "epoch changed", "retryable": false
                }}),
            )
            .await;
        });
        let (conn, _hello, _events) = Connection::open(Box::new(client), "tok", HelloRole::Control)
            .await
            .unwrap();
        let err = conn
            .call("rpc-1", "session.input", json!({}))
            .await
            .unwrap_err()
            .into_rpc();
        peer.await.unwrap();
        assert_eq!(err.code, ErrorCode::StaleEpoch);
        assert!(!err.retryable);
    }

    #[tokio::test]
    async fn silent_peer_times_out_with_retryable_busy() {
        let (client, mut daemon) = duplex(4096);
        let peer = tokio::spawn(async move {
            let request = read_request(&mut daemon).await; // hello
            write_value(
                &mut daemon,
                json!({"v": 1, "id": request["id"], "result": hello_result_json(DATA_TOKEN)}),
            )
            .await;
            // Read the RPC but never answer.
            let _request = read_request(&mut daemon).await;
            std::future::pending::<()>().await;
        });
        let (conn, _hello, _events) = Connection::open(Box::new(client), "tok", HelloRole::Control)
            .await
            .unwrap();
        let err = conn
            .call_with_timeout(
                "rpc-1",
                "system.snapshot",
                json!({}),
                Duration::from_millis(80),
            )
            .await
            .unwrap_err()
            .into_rpc();
        peer.abort();
        // A slow answer on a live connection is BUSY (retry in place), not
        // DAEMON_UNAVAILABLE (which makes the client rebuild the transport).
        assert_eq!(err.code, ErrorCode::Busy);
        assert!(err.retryable);
        assert!(!conn.is_closed());
    }

    #[tokio::test]
    async fn closed_stream_fails_inflight_calls() {
        let (client, mut daemon) = duplex(4096);
        let peer = tokio::spawn(async move {
            let request = read_request(&mut daemon).await; // hello
            write_value(
                &mut daemon,
                json!({"v": 1, "id": request["id"], "result": hello_result_json(DATA_TOKEN)}),
            )
            .await;
            let _request = read_request(&mut daemon).await;
            // Drop the daemon side → EOF for the client reader.
        });
        let (conn, _hello, _events) = Connection::open(Box::new(client), "tok", HelloRole::Control)
            .await
            .unwrap();
        let conn = Arc::new(conn);
        let call = {
            let conn = Arc::clone(&conn);
            tokio::spawn(async move { conn.call("rpc-1", "system.snapshot", json!({})).await })
        };
        peer.await.unwrap();
        let err = call.await.unwrap().unwrap_err().into_rpc();
        assert_eq!(err.code, ErrorCode::DaemonUnavailable);
        assert!(conn.is_closed());
    }

    #[tokio::test]
    async fn data_connection_delivers_events_and_acks_are_fire_and_forget() {
        let (client, mut daemon) = duplex(8192);
        let session_id = Uuid::new_v4().to_string();
        let peer_session_id = session_id.clone();
        let seen_ack = Arc::new(Mutex::new(None::<Value>));
        let ack_sink = Arc::clone(&seen_ack);
        let peer = tokio::spawn(async move {
            let request = read_request(&mut daemon).await; // hello (data role)
            assert_eq!(request["params"]["role"], "data");
            assert_eq!(request["params"]["token"], DATA_TOKEN);
            write_value(
                &mut daemon,
                json!({"v": 1, "id": request["id"], "result": {"connection_id": Uuid::new_v4().to_string(), "role": "data"}}),
            )
            .await;
            // Push one session.output event.
            write_value(
                &mut daemon,
                json!({"v": 1, "event": "session.output", "payload": {
                    "session_id": peer_session_id, "epoch": "e1", "seq": "1", "kind": "output",
                    "data_b64": "aGk=", "raw_len": 2
                }}),
            )
            .await;
            // Expect the session.ack request, without answering it.
            let request = read_request(&mut daemon).await;
            assert_eq!(request["method"], "session.ack");
            *ack_sink.lock().await = Some(request["params"].clone());
        });

        let (conn, _hello, mut events) =
            Connection::open(Box::new(client), DATA_TOKEN, HelloRole::Data)
                .await
                .unwrap();
        let conn = Arc::new(conn);
        let ack = conn.send_only(RpcRequest::new(
            Uuid::new_v4().to_string(),
            rpc::methods::SESSION_ACK,
            json!({"session_id": session_id, "epoch": "e1", "through_seq": "1"}),
        ));
        let received = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        ack.await.unwrap();
        peer.await.unwrap();

        let frame = received.expect("event arrived").unwrap();
        match frame {
            Frame::Event(event) => {
                assert_eq!(event.event, RpcEventKind::SessionOutput);
                assert_eq!(event.payload["data_b64"], "aGk=");
            }
            other => panic!("expected event frame, got {other:?}"),
        }
        let params = seen_ack.lock().await.clone().unwrap();
        assert_eq!(params["through_seq"], "1");
        assert_eq!(params["epoch"], "e1");
    }
}
