//! Control/data connections speaking term-contracts frames over a boxed
//! async stream (UDS on Unix, named pipe on Windows, in-memory duplex pair
//! in tests). Never owns PTYs — the daemon does (IMPLEMENTATION_SPEC §3).
//!
//! 원본 `src-tauri/src/bridge/connection.rs`에서 옮겼다(T02). 동작 보존:
//! * hello 제한 2s, 일반 RPC timeout 5s(timeout은 `Busy` — 재연결 아님),
//! * writer queue 깊이 64,
//! * 알 수 없는 RPC error 코드는 원문 그대로 보존(`RpcRaw`),
//! * 연결이 닫히면 pending 호출에 `DaemonUnavailable`로 응답.
//!
//! T03 추가: 이벤트 전달 정책. GUI의 기존 동작(unbounded)은 그대로 두고,
//! TUI는 [`EventPolicy::bounded`]로 열어 항목 수·바이트 예산이 찼을 때
//! 조용히 버리는 대신 연결을 명시적으로 실패시킨다.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
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

use crate::codec;
use crate::transport::BoxedStream;

/// First frame must be `hello` within 2s (`01-contracts.md` §3).
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
/// RPC timeout default (`01-contracts.md` §4) → `BUSY`, retryable. 재연결이 아니다.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(5);
/// Outgoing frame queue depth before backpressure.
pub const WRITER_QUEUE: usize = 64;

/// Bridge error surface. Transport/frame/io causes map onto spec error codes;
/// constructed messages never embed tokens or user payloads.
#[derive(Debug)]
pub enum BridgeError {
    Rpc(RpcError),
    /// An error body whose code is outside this build's R1 vocabulary (the
    /// O1 mission codes, or a future peer): carried verbatim so the caller
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

    /// `into_rpc(self)`가 만들 상태에서 code·retryable·message만 뽑는 대출
    /// 판정. `&self`로 읽는 경로(ConnectError::summary·retryable)가 오류를
    /// 소비하지 않게 한다. Frame·Io·RpcRaw의 retryable은 `RpcError::new`와
    /// 같은 code 기준 값을 쓴다.
    pub fn rpc_status(&self) -> (ErrorCode, bool, String) {
        match self {
            BridgeError::Rpc(err) => (err.code, err.retryable, err.message.clone()),
            BridgeError::RpcRaw(value) => {
                let code = ErrorCode::ProtocolMismatch;
                (code, code.retryable(), format!("raw rpc error: {value}"))
            }
            BridgeError::Frame(err) => {
                let code = ErrorCode::DaemonUnavailable;
                (code, code.retryable(), format!("bridge frame error: {err}"))
            }
            BridgeError::Io(err) => {
                let code = ErrorCode::DaemonUnavailable;
                (code, code.retryable(), format!("bridge io error: {err}"))
            }
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

/// 이벤트 전달 정책(T03).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventPolicy {
    /// 이벤트 채널 용량(프레임 수). 0이면 unbounded.
    pub channel_capacity: usize,
    /// 큐에 남아 있어도 되는 총 바이트(전송 예산). 0이면 무제한.
    pub byte_budget: usize,
}

impl EventPolicy {
    /// GUI 기존 동작: unbounded(변경 금지, T03 완료 기준).
    pub const fn unbounded() -> Self {
        Self {
            channel_capacity: 0,
            byte_budget: 0,
        }
    }

    /// TUI bounded 정책. 항목 수와 바이트 예산을 함께 제한한다.
    /// 예산이 가득 차면 이벤트를 버리지 않고 연결을 실패로 만든다
    /// (output drop 금지 — IMPLEMENTATION_PLAN §6.3/§9.3).
    pub const fn bounded(channel_capacity: usize, byte_budget: usize) -> Self {
        Self {
            channel_capacity,
            byte_budget,
        }
    }
}

impl Default for EventPolicy {
    fn default() -> Self {
        Self::unbounded()
    }
}

/// 소비자가 받는 이벤트 스트림. 정책에 따라 unbounded 또는 bounded.
pub enum EventDelivery {
    Unbounded(mpsc::UnboundedReceiver<Frame>),
    Bounded(BoundedEvents),
}

impl EventDelivery {
    /// 다음 이벤트를 기다린다. `None`은 스트림 끝(연결 종료)이다.
    pub async fn recv(&mut self) -> Option<Frame> {
        match self {
            EventDelivery::Unbounded(rx) => rx.recv().await,
            EventDelivery::Bounded(bounded) => bounded.recv().await,
        }
    }
}

/// bounded 이벤트 수신부: 프레임을 꺼낼 때마다 바이트 예산을 되돌린다.
pub struct BoundedEvents {
    rx: mpsc::Receiver<(usize, Frame)>,
    bytes: Arc<AtomicI64>,
}

impl BoundedEvents {
    pub async fn recv(&mut self) -> Option<Frame> {
        let (size, frame) = self.rx.recv().await?;
        self.bytes.fetch_sub(size as i64, Ordering::Release);
        Some(frame)
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

/// 이벤트 전달 경로. bounded에서는 try_send만 쓴다 — reader가 소비 지연에
/// 영구 대기하며 같은 연결의 RPC 응답을 막는 일이 없게 한다(§9.3).
enum EventSink {
    Unbounded(mpsc::UnboundedSender<Frame>),
    Bounded {
        tx: mpsc::Sender<(usize, Frame)>,
        bytes: Arc<AtomicI64>,
        budget: i64,
    },
}

enum SinkError {
    Closed,
    Overflow,
}

impl EventSink {
    fn send(&self, frame: Frame, size: usize) -> Result<(), SinkError> {
        match self {
            EventSink::Unbounded(tx) => tx.send(frame).map_err(|_| SinkError::Closed),
            EventSink::Bounded { tx, bytes, budget } => {
                let queued = bytes.fetch_add(size as i64, Ordering::AcqRel) + size as i64;
                if queued > *budget {
                    // 되돌린다 — 이 프레임은 전달되지 않으므로 예산에 남지 않게.
                    bytes.fetch_sub(size as i64, Ordering::Release);
                    return Err(SinkError::Overflow);
                }
                match tx.try_send((size, frame)) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full((size, _))) => {
                        bytes.fetch_sub(size as i64, Ordering::Release);
                        Err(SinkError::Overflow)
                    }
                    Err(mpsc::error::TrySendError::Closed((size, _))) => {
                        bytes.fetch_sub(size as i64, Ordering::Release);
                        Err(SinkError::Closed)
                    }
                }
            }
        }
    }
}

/// One framed daemon connection: hello handshake, an id-matched pending map
/// for RPCs, a single writer task, and a reader task that broadcasts events.
pub struct Connection {
    outgoing: mpsc::Sender<Value>,
    pending: PendingMap,
    closed: Arc<AtomicBool>,
    /// bounded 정책에서 전달 실패로 닫혔는지(재연결 사유 구분용).
    overflowed: Arc<AtomicBool>,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
}

impl Connection {
    /// Perform the hello handshake on a fresh stream, then start the
    /// reader/writer loops with the GUI's 기존 unbounded 이벤트 전달.
    /// Only control connections return a HelloResult; data connections
    /// acknowledge the link with connection_id and role.
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
        let (conn, hello, delivery) =
            Self::open_with_policy(stream, token, role, EventPolicy::unbounded()).await?;
        match delivery {
            EventDelivery::Unbounded(rx) => Ok((conn, hello, rx)),
            EventDelivery::Bounded(_) => Err(BridgeError::daemon_unavailable(
                "unreachable: unbounded policy produced a bounded channel",
            )),
        }
    }

    /// [`Connection::open`]과 같지만 이벤트 전달 정책을 고른다(T03, TUI).
    pub async fn open_with_policy(
        stream: BoxedStream,
        token: &str,
        role: HelloRole,
        policy: EventPolicy,
    ) -> Result<(Connection, Option<HelloResult>, EventDelivery), BridgeError> {
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
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let overflowed = Arc::new(AtomicBool::new(false));
        let (delivery, reader, writer) = match policy {
            EventPolicy {
                channel_capacity: 0,
                byte_budget: 0,
            } => {
                let (event_tx, event_rx) = mpsc::unbounded_channel::<Frame>();
                let (reader, writer) = spawn_loops(
                    read_half,
                    write_half,
                    out_rx,
                    pending.clone(),
                    EventSink::Unbounded(event_tx),
                    closed.clone(),
                    overflowed.clone(),
                );
                (EventDelivery::Unbounded(event_rx), reader, writer)
            }
            EventPolicy {
                channel_capacity,
                byte_budget,
            } => {
                let (event_tx, event_rx) = mpsc::channel::<(usize, Frame)>(channel_capacity.max(1));
                let bytes = Arc::new(AtomicI64::new(0));
                let (reader, writer) = spawn_loops(
                    read_half,
                    write_half,
                    out_rx,
                    pending.clone(),
                    EventSink::Bounded {
                        tx: event_tx,
                        bytes: Arc::clone(&bytes),
                        budget: byte_budget as i64,
                    },
                    closed.clone(),
                    overflowed.clone(),
                );
                (
                    EventDelivery::Bounded(BoundedEvents {
                        rx: event_rx,
                        bytes,
                    }),
                    reader,
                    writer,
                )
            }
        };
        Ok((
            Connection {
                outgoing: out_tx,
                pending,
                closed,
                overflowed,
                reader,
                writer,
            },
            hello,
            delivery,
        ))
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// bounded 정책에서 예산 초과로 닫혔는지(조용한 누락이 아님을 구분).
    pub fn overflowed(&self) -> bool {
        self.overflowed.load(Ordering::SeqCst)
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

fn spawn_loops<R, W>(
    read: R,
    write: W,
    out_rx: mpsc::Receiver<Value>,
    pending: PendingMap,
    sink: EventSink,
    closed: Arc<AtomicBool>,
    overflowed: Arc<AtomicBool>,
) -> (JoinHandle<()>, JoinHandle<()>)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let reader = tokio::spawn(reader_loop(read, pending, sink, closed.clone(), overflowed));
    let writer = tokio::spawn(writer_loop(write, out_rx, closed));
    (reader, writer)
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
    events: EventSink,
    closed: Arc<AtomicBool>,
    overflowed: Arc<AtomicBool>,
) where
    R: AsyncRead + Unpin,
{
    loop {
        let (frame, size) = match codec::read_frame_sized(&mut read).await {
            Ok(pair) => pair,
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
            Ok(other) => match events.send(other, size) {
                Ok(()) => {}
                Err(SinkError::Closed) => break,
                // 전송 한도 초과: 조용히 버리지 않고 연결을 실패로 만든다(§9.3).
                Err(SinkError::Overflow) => {
                    overflowed.store(true, Ordering::SeqCst);
                    tracing::warn!(
                        "term-client: event delivery budget exceeded; closing connection"
                    );
                    break;
                }
            },
            Err(err) => {
                // A single malformed frame does not close the connection
                // (read_frame already validated the byte level); malformed
                // envelopes are dropped with a redaction-free note.
                tracing::warn!(error = %err, "term-client: dropped malformed envelope from daemon");
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
                tracing::warn!(error = %err, "term-client: refused to encode outgoing frame");
            }
        }
    }
    closed.store(true, Ordering::SeqCst);
}

// 몇 가지 세부 구현 노트:
// * `Transport` 트레이트 재수출은 lib.rs에서 한다(순환을 피하려고 여기서는
//   import만 한다).
// * `dummy_handle` 경로는 실제로는 `spawn_loops`가 돌려준 핸들로 즉시 교체된다.

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

    /// T03: bounded 정책에서 예산 초과는 조용한 누락이 아니라 연결 실패다.
    /// 소비자가 이벤트를 안 읽는 동안 daemon이 계속 밀어 넣으면 reader가
    /// 닫히고, 진행 중 RPC는 DaemonUnavailable로 실패한다.
    #[tokio::test]
    async fn bounded_budget_overflow_closes_the_connection_loudly() {
        let (client, mut daemon) = duplex(16 * 1024);
        let peer = tokio::spawn(async move {
            let request = read_request(&mut daemon).await; // hello
            write_value(
                &mut daemon,
                json!({"v": 1, "id": request["id"], "result": hello_result_json(DATA_TOKEN)}),
            )
            .await;
            // 소비하지 않는 클라이언트를 위해 이벤트를 계속 밀어 넣는다.
            for seq in 1..=64u32 {
                write_value(
                    &mut daemon,
                    json!({"v": 1, "event": "queue.changed", "payload": {
                        "revision": seq, "blob": "x".repeat(512)
                    }}),
                )
                .await;
            }
        });

        let (conn, _hello, mut events) = Connection::open_with_policy(
            Box::new(client),
            "tok",
            HelloRole::Control,
            EventPolicy::bounded(8, 4 * 1024),
        )
        .await
        .unwrap();
        // 몇 개만 소비하고 멈춘다.
        let mut received = 0usize;
        while received < 2 {
            if events.recv().await.is_some() {
                received += 1;
            } else {
                break;
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while events.recv().await.is_some() {}
        })
        .await;
        assert!(conn.overflowed());
        assert!(conn.is_closed());
        peer.abort();
    }

    /// T03: bounded여도 소비가 따라가면 예산은 돌아온다(무한 닫힘 아님).
    #[tokio::test]
    async fn bounded_stream_survives_when_the_consumer_keeps_up() {
        let (client, mut daemon) = duplex(16 * 1024);
        const EVENTS: u32 = 32;
        let peer = tokio::spawn(async move {
            let request = read_request(&mut daemon).await; // hello
            write_value(
                &mut daemon,
                json!({"v": 1, "id": request["id"], "result": hello_result_json(DATA_TOKEN)}),
            )
            .await;
            for seq in 1..=EVENTS {
                write_value(
                    &mut daemon,
                    json!({"v": 1, "event": "queue.changed", "payload": {"revision": seq}}),
                )
                .await;
            }
        });

        let (_conn, _hello, mut events) = Connection::open_with_policy(
            Box::new(client),
            "tok",
            HelloRole::Control,
            EventPolicy::bounded(16, 8 * 1024),
        )
        .await
        .unwrap();
        let mut count = 0u32;
        let drained = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(_frame) = events.recv().await {
                count += 1;
            }
        })
        .await;
        peer.abort();
        let _ = drained;
        assert_eq!(count, EVENTS, "모든 이벤트가 순서대로 도착해야 한다");
    }
}
