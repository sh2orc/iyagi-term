//! IPC surface: platform listener (UDS / named pipe), async frame codec
//! (length-prefixed JSON per `01-contracts.md` §3), and the per-connection
//! loop (hello handshake, request dispatch, event fan-out).
//!
//! Violations — oversize frame, invalid UTF-8, nesting depth > 32, invalid
//! JSON, hello timeout — close THAT connection only; the daemon survives.
//! Tokens are never logged.

use std::io::Cursor;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use term_contracts::ids::ConnectionId;
use term_contracts::rpc::{
    decode_frame, encode_frame, Frame, FrameError, HelloParams, HelloResult, HelloRole,
    RpcResponse, MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
use term_contracts::{ErrorCode, RpcError};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::futures::Notified;
use tokio::sync::mpsc;

use crate::auth::constant_time_eq;
use crate::dispatch;
use crate::state::{CoalescedOut, ConnHandle, ConnRole, DaemonState};

/// This daemon's build id, reported in `HelloResult.daemon_version`: the crate
/// version plus the commit/source fingerprint stamped at build time by `build.rs`
/// (`IYAGI_GIT_SHA`). The app builds the same-shaped string from its own
/// `env!`s and compares them to detect a stale, still-running daemon. The sha
/// includes uncommitted source changes, which `CARGO_PKG_VERSION` alone does not.
pub fn build_version() -> String {
    format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("IYAGI_GIT_SHA"))
}

/// Async frame reader: reads `u32 LE len + body` with the hard cap, then
/// reuses the contracts' synchronous validator for UTF-8/JSON/depth.
pub async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Frame, FrameError> {
    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|_| FrameError::Truncated)?;
    let len = u32::from_le_bytes(header);
    if len as usize + 4 > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len, MAX_FRAME_BYTES as u32));
    }
    // Whole frame (header + body): the contracts' decoder re-reads the
    // length prefix, so hand it the complete byte stream.
    let mut frame = header.to_vec();
    frame.resize(4 + len as usize, 0);
    stream
        .read_exact(&mut frame[4..])
        .await
        .map_err(|_| FrameError::Truncated)?;
    let value = decode_frame(&mut Cursor::new(frame))?;
    Frame::from_json(value)
}

/// Async frame writer.
pub async fn write_frame<S: AsyncWrite + Unpin>(
    stream: &mut S,
    value: &serde_json::Value,
) -> Result<(), FrameError> {
    let bytes = encode_frame(value)?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|e| FrameError::Io(e.to_string()))?;
    stream
        .flush()
        .await
        .map_err(|e| FrameError::Io(e.to_string()))
}

/// Write already-encoded bytes and flush.
async fn write_bytes<S: AsyncWrite + Unpin>(stream: &mut S, bytes: &[u8]) -> std::io::Result<()> {
    stream.write_all(bytes).await?;
    stream.flush().await
}

/// Write one encoded frame, raced against the connection's close wake.
///
/// A `select!` arm body is not raced against the other branches: a peer
/// that stopped reading leaves `write_all`/`flush` pending forever, so a
/// close signalled from outside the writer (control queue overflow in
/// `ConnHandle::send`, or control teardown closing its data link) would
/// never be observed and the connection would outlive its views. Returns
/// false when the write failed or the connection was closed meanwhile.
async fn write_raced<S: AsyncWrite + Unpin>(
    stream: &mut S,
    bytes: &[u8],
    closed: Pin<&mut Notified<'_>>,
) -> bool {
    tokio::select! {
        written = write_bytes(stream, bytes) => written.is_ok(),
        _ = closed => false,
    }
}

/// Encode `value` and write it raced against the close wake
/// ([`write_raced`]). A value that cannot be encoded fails like a write.
async fn write_value_raced<S: AsyncWrite + Unpin>(
    stream: &mut S,
    value: &serde_json::Value,
    closed: Pin<&mut Notified<'_>>,
) -> bool {
    match encode_frame(value) {
        Ok(bytes) => write_raced(stream, &bytes, closed).await,
        Err(_) => false,
    }
}

/// Bind the main IPC endpoint and serve connections until shutdown.
pub async fn serve(state: Arc<DaemonState>) -> std::io::Result<()> {
    let endpoint = state.paths.main_endpoint();
    #[cfg(unix)]
    {
        use tokio::net::UnixListener;
        if std::path::Path::new(&endpoint).exists() {
            let _ = std::fs::remove_file(&endpoint);
        }
        let listener = UnixListener::bind(&endpoint)?;
        state.paths.write_endpoint(&endpoint)?;
        tracing::info!(endpoint = %endpoint, "ipc listening (uds)");
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(ok) => ok,
                Err(e) => {
                    tracing::warn!(error = %e, "uds accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                handle_connection(state, stream).await;
            });
        }
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ServerOptions;
        state.paths.write_endpoint(&endpoint)?;
        tracing::info!(endpoint = %endpoint, "ipc listening (named pipe)");
        let mut first = true;
        // The first-instance claim can also fail transiently while a
        // previous daemon's pipe handles are still being torn down. Retry
        // briefly (~2 s) before treating the name as squatted: a squat must
        // end this daemon (caller signals shutdown), not zombify it while
        // it holds the singleton lock.
        let mut first_attempts = 20u32;
        loop {
            let server = match ServerOptions::new()
                .first_pipe_instance(first)
                .create(&endpoint)
            {
                Ok(s) => s,
                Err(e) => {
                    if first {
                        if first_attempts > 1 {
                            first_attempts -= 1;
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                        // The very first instance failing means another
                        // process owns this pipe name — never serve.
                        return Err(std::io::Error::other(format!(
                            "named pipe create failed ({e}); another daemon may own the endpoint"
                        )));
                    }
                    tracing::warn!(error = %e, "named pipe create failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            first = false;
            if let Err(e) = server.connect().await {
                tracing::warn!(error = %e, "named pipe connect failed");
                continue;
            }
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                handle_connection(state, server).await;
            });
        }
    }
}

/// One connection: hello within `hello_timeout`, then request/response +
/// event fan-out until the peer goes away or a violation closes us.
async fn handle_connection<S>(state: Arc<DaemonState>, stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut read_half, mut write_half) = tokio::io::split(stream);

    // -- hello handshake (2 s) ---------------------------------------------
    let hello_timeout = state.config.hello_timeout();
    let first = match tokio::time::timeout(hello_timeout, read_frame(&mut read_half)).await {
        Err(_) => return,
        Ok(Err(_)) => return,
        Ok(Ok(frame)) => frame,
    };

    let Frame::Request(request) = first else {
        return; // first message must be a request
    };
    if request.method != term_contracts::rpc::methods::HELLO {
        let err = RpcResponse::err(
            request.id,
            RpcError::new(ErrorCode::InvalidArgument, "first message must be hello"),
        );
        let _ = write_frame(
            &mut write_half,
            &serde_json::to_value(&err).unwrap_or_default(),
        )
        .await;
        return;
    }
    if request.v != PROTOCOL_VERSION {
        let err = RpcResponse::err(
            request.id,
            RpcError::new(ErrorCode::ProtocolMismatch, "protocol version mismatch"),
        );
        let _ = write_frame(
            &mut write_half,
            &serde_json::to_value(&err).unwrap_or_default(),
        )
        .await;
        return; // no further methods on mismatch (spec §3)
    }
    let hello: HelloParams = match serde_json::from_value(request.params) {
        Ok(p) => p,
        Err(_) => return,
    };

    let conn_id = ConnectionId::generate();
    let (role, linked_control) = match hello.role {
        HelloRole::Control => {
            let expected = state.paths.read_token();
            if expected.is_empty() || !constant_time_eq(&hello.token, &expected) {
                let err = RpcResponse::err(
                    request.id,
                    RpcError::new(ErrorCode::InvalidArgument, "authentication failed"),
                );
                let _ = write_frame(
                    &mut write_half,
                    &serde_json::to_value(&err).unwrap_or_default(),
                )
                .await;
                return;
            }
            (ConnRole::Control, None)
        }
        HelloRole::Data => {
            let Some(control) = state.data_tokens.redeem(&hello.token) else {
                let err = RpcResponse::err(
                    request.id,
                    RpcError::new(ErrorCode::InvalidArgument, "data token invalid or expired"),
                );
                let _ = write_frame(
                    &mut write_half,
                    &serde_json::to_value(&err).unwrap_or_default(),
                )
                .await;
                return;
            };
            (ConnRole::Data, Some(control))
        }
    };

    // Outbound FIFO bound (defaults.json `limits.control_queue_entries`):
    // a control peer that stops draining gets this connection closed by the
    // overflow path in `ConnHandle::send` instead of growing daemon memory
    // unbounded. A data connection shares the entry bound but not the close:
    // its budget is byte-based (02-runner §2, per-view flow credit), so the
    // session pumps reserve a slot and back off on a full queue
    // (`ConnHandle::reserve_frame`) — a replay burst never drops the link.
    let queue_entries = state.config.defaults.limits.control_queue_entries.max(1) as usize;
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(queue_entries);
    let handle = Arc::new(ConnHandle {
        conn_id: conn_id.clone(),
        role,
        linked_control: linked_control.clone(),
        tx,
        coalesced: std::sync::Mutex::new(CoalescedOut::default()),
        closed: std::sync::atomic::AtomicBool::new(false),
        close_wake: tokio::sync::Notify::new(),
    });
    state.register_connection(Arc::clone(&handle));

    // Closing this connection from outside the writer (control queue
    // overflow in `ConnHandle::send`, or control teardown closing its data
    // link) wakes this future. Every socket write from here on is raced
    // against it (`write_raced`) as well as the select below, so teardown
    // never waits on a peer that stopped reading. Pinned once, before the
    // first write: a wake that races a winning arm or a write is not lost.
    let closed_wait = handle.close_wake.notified();
    tokio::pin!(closed_wait);

    // hello result (control only carries the fresh data token).
    match role {
        ConnRole::Control => {
            let data_token = state.data_tokens.issue(conn_id.clone());
            let caps = state.caps.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let result = HelloResult {
                daemon_id: state.daemon_id.clone(),
                protocol: PROTOCOL_VERSION,
                connection_id: conn_id.clone(),
                data_token,
                capabilities: caps,
                daemon_version: build_version(),
            };
            let response = RpcResponse::ok(
                request.id,
                serde_json::to_value(&result).unwrap_or_default(),
            );
            let value = serde_json::to_value(&response).unwrap_or_default();
            if !write_value_raced(&mut write_half, &value, closed_wait.as_mut()).await {
                state.remove_connection(&conn_id);
                return;
            }
        }
        ConnRole::Data => {
            // Spec §3: no formal hello result shape for data; ack the link.
            let response = RpcResponse::ok(
                request.id,
                serde_json::json!({ "connection_id": conn_id, "role": "data" }),
            );
            let value = serde_json::to_value(&response).unwrap_or_default();
            if !write_value_raced(&mut write_half, &value, closed_wait.as_mut()).await {
                state.remove_connection(&conn_id);
                return;
            }
        }
    }

    // -- request/event loop ---------------------------------------------------
    let state_for_loop = Arc::clone(&state);
    let conn_for_loop = conn_id.clone();
    let role_for_loop = role;
    let linked_for_loop = linked_control.clone();
    let mut violations: u32 = 0;

    // Bound completion tasks even for a peer flooding many sessions. A
    // single session also allows only one outstanding resize. Inputs get
    // their OWN budget: they count against the input cap, not the resize
    // budget — otherwise a connection with many stalled-input panes (each
    // waiting up to 750 ms) starves every legitimate resize into Busy. Both
    // caps default to 64 and are test-lowered via `limits.max_pending_*`.
    let max_pending_resizes = state.config.max_pending_resizes.max(1);
    let max_pending_inputs = state.config.max_pending_inputs.max(1);
    let pending_resizes = Arc::new(AtomicUsize::new(0));
    let pending_inputs = Arc::new(AtomicUsize::new(0));
    let mut completions = tokio::task::JoinSet::<serde_json::Value>::new();
    'connection: loop {
        // read_exact is not cancellation-safe. Keep the same future alive
        // when an event or resize completion wins the select mid-frame.
        let incoming = read_frame(&mut read_half);
        tokio::pin!(incoming);
        loop {
            tokio::select! {
                completed = completions.join_next(), if !completions.is_empty() => {
                    match completed {
                        Some(Ok(value)) => {
                            let closed = closed_wait.as_mut();
                            if !write_value_raced(&mut write_half, &value, closed).await {
                                break 'connection;
                            }
                        }
                        // A panic cannot leave a caller waiting indefinitely.
                        _ => break 'connection,
                    }
                }
                outbound = rx.recv() => {
                    // Every write races the close wake (`write_raced`): an arm
                    // body is not raced against `closed_wait` below.
                    match outbound {
                        Some(bytes) if bytes.is_empty() => {
                            // Wake sentinel: drain any backlog first, then
                            // flush latest-only snapshot events.
                            while let Ok(bytes) = rx.try_recv() {
                                if bytes.is_empty() {
                                    continue;
                                }
                                let closed = closed_wait.as_mut();
                                if !write_raced(&mut write_half, &bytes, closed).await {
                                    break 'connection;
                                }
                            }
                            for bytes in handle.take_coalesced() {
                                let closed = closed_wait.as_mut();
                                if !write_raced(&mut write_half, &bytes, closed).await {
                                    break 'connection;
                                }
                            }
                        }
                        Some(bytes) => {
                            let closed = closed_wait.as_mut();
                            if !write_raced(&mut write_half, &bytes, closed).await {
                                break 'connection;
                            }
                        }
                        None => break 'connection,
                    }
                }
                _ = &mut closed_wait => {
                    // Closed from outside the writer (control queue overflow,
                    // or control teardown closing this data link): stop
                    // serving it (slow-reader semantics — only this
                    // connection goes away, spec §3).
                    break 'connection;
                }
                frame = &mut incoming => {
                    let frame = match frame {
                        Ok(f) => f,
                        Err(e) => {
                            // Violation: close this connection only; the daemon
                            // survives (spec §3).
                            // 프레임 경계 어긋남의 사후 진단: 헤더가 65536을 초과하면
                            // 바이트 정렬이 깨진 것이므로 경고 레벨로 승격한다.
                            if matches!(
                                &e,
                                term_contracts::rpc::FrameError::TooLarge(n, _)
                                    if *n > 0x0010_0000 // 1 MiB — 어떤 정상 프레임도 이렇게 크지 않다
                            ) {
                                tracing::warn!(
                                    conn = %conn_for_loop,
                                    role = ?role_for_loop,
                                    error = %e,
                                    "ipc frame boundary corruption (suspicious header); closing connection"
                                );
                            } else {
                                tracing::debug!(conn = %conn_for_loop, error = %e, "ipc frame violation; closing connection");
                            }
                            break 'connection;
                        }
                    };
                    let Frame::Request(req) = frame else {
                        // Clients do not send events/responses.
                        violations += 1;
                        if violations >= 2 {
                            break 'connection;
                        }
                        continue 'connection;
                    };
                    if role_for_loop == ConnRole::Control {
                        // Per-kind budgets, checked in wire order before the
                        // deferred work is even created.
                        let over_limit = match req.method.as_str() {
                            term_contracts::rpc::methods::SESSION_RESIZE => {
                                pending_resizes.load(Ordering::Acquire) >= max_pending_resizes
                            }
                            term_contracts::rpc::methods::SESSION_INPUT => {
                                pending_inputs.load(Ordering::Acquire) >= max_pending_inputs
                            }
                            _ => false,
                        };
                        if over_limit {
                            let error = RpcResponse::err(req.id, RpcError::new(
                                ErrorCode::Busy, "too many pending session requests",
                            )
                            // 거절은 dispatch **전**에 내렸다 — 입력은 세션에
                            // 닿지 않았으므로 클라이언트는 이 간격 뒤 안전하게
                            // 다시 보낼 수 있다(완료들은 최대 750 ms 안에 빠진다).
                            .with_details(json!({
                                "reason_code": dispatch::REASON_PENDING_BUDGET,
                                "retry_after_ms": 250,
                            })));
                            let value = serde_json::to_value(error).unwrap_or_default();
                            let closed = closed_wait.as_mut();
                            if !write_value_raced(&mut write_half, &value, closed).await {
                                break 'connection;
                            }
                            continue 'connection;
                        }
                    }
                    let outcome = dispatch::route(
                        Arc::clone(&state_for_loop),
                        &conn_for_loop,
                        role_for_loop,
                        linked_for_loop.clone(),
                        &req,
                        &mut violations,
                    )
                    .await;
                    match outcome {
                        dispatch::Outcome::Reply(value) => {
                            let closed = closed_wait.as_mut();
                            if !write_value_raced(&mut write_half, &value, closed).await {
                                break 'connection;
                            }
                        }
                        dispatch::Outcome::Deferred(completion) => {
                            // Count deferred inputs/resizes against their own
                            // budgets for as long as they run, so the gates
                            // above see live pressure, not total spawn count.
                            match req.method.as_str() {
                                term_contracts::rpc::methods::SESSION_INPUT => {
                                    pending_inputs.fetch_add(1, Ordering::AcqRel);
                                    let counter = Arc::clone(&pending_inputs);
                                    completions.spawn(async move {
                                        let value = completion.await;
                                        counter.fetch_sub(1, Ordering::AcqRel);
                                        value
                                    });
                                }
                                term_contracts::rpc::methods::SESSION_RESIZE => {
                                    pending_resizes.fetch_add(1, Ordering::AcqRel);
                                    let counter = Arc::clone(&pending_resizes);
                                    completions.spawn(async move {
                                        let value = completion.await;
                                        counter.fetch_sub(1, Ordering::AcqRel);
                                        value
                                    });
                                }
                                _ => {
                                    completions.spawn(completion);
                                }
                            }
                        }
                        dispatch::Outcome::None => {
                            // session.ack: no response on success (spec §4).
                        }
                        dispatch::Outcome::Close(Some(value)) => {
                            let closed = closed_wait.as_mut();
                            let _ = write_value_raced(&mut write_half, &value, closed).await;
                            break 'connection;
                        }
                        dispatch::Outcome::Close(None) => break 'connection,
                    }
                    continue 'connection;
                }
            }
        }
    }
    // Cancels waits and releases per-session guards on disconnect.
    completions.shutdown().await;

    // Connection teardown: detach views owned by this control connection
    // (slow-reader semantics: only the connection goes away, spec §4).
    handle.closed.store(true, Ordering::Release);
    state.remove_connection(&conn_id);
    if role == ConnRole::Control {
        dispatch::detach_all_views_of(&state, &conn_id);
        // Its data link now serves nothing; closing it wakes that writer
        // even when blocked on a peer that stopped reading (data queues
        // back off instead of overflow-closing).
        state.close_linked_data(&conn_id);
    }
    if role == ConnRole::Data {
        // 데이터 연결이 죽었다(컨트롤 연결은 살아 있을 수 있다). 그
        // 컨트롤의 뷰는 프레임을 받을 길이 영원히 사라졌다 — 놔두면 펌프가
        // 매 패스 조용히 건너뛰기만 하고 클라이언트의 "기록 재생 중…"
        // 갇힘이 된다. 뷰를 떼고 다시 붙으라고 알린다. 컨트롤 연결이
        // 먼저 죽은 경우 detach_all_views_of가 이미 정리했으므로 여기서
        // 건드릴 것이 없다.
        if let Some(control) = linked_control.as_ref() {
            crate::sessions::shed_views_of_control(&state, control);
        }
    }
    tracing::debug!(conn = %conn_id, role = ?role, "ipc connection closed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip_over_async_cursor() {
        let value = serde_json::json!({"v":1,"id":"x","method":"hello","params":{}});
        let bytes = encode_frame(&value).expect("encode");
        let mut cursor = tokio::io::BufReader::new(std::io::Cursor::new(bytes));
        let frame = read_frame(&mut cursor).await.expect("decode");
        assert!(matches!(frame, Frame::Request(_)));
    }

    #[tokio::test]
    async fn oversize_header_is_rejected() {
        let mut poison: Vec<u8> = 70_000u32.to_le_bytes().to_vec();
        poison.extend_from_slice(&[0u8; 8]);
        let mut cursor = tokio::io::BufReader::new(std::io::Cursor::new(poison));
        let err = read_frame(&mut cursor).await.expect_err("must reject");
        assert!(matches!(err, FrameError::TooLarge(_, _)));
    }

    /// A write blocked on a peer that stopped reading still ends once the
    /// connection is closed from outside the writer: the close wake is
    /// raced against the socket write instead of waiting behind it.
    #[tokio::test]
    async fn blocked_write_observes_the_close_wake() {
        // An 8-byte pipe nobody reads: a 64-byte frame can never finish.
        let (mut stalled, _peer) = tokio::io::duplex(8);
        let wake = tokio::sync::Notify::new();
        let closed = wake.notified();
        tokio::pin!(closed);
        wake.notify_one();
        let frame = vec![0u8; 64];
        let write = write_raced(&mut stalled, &frame, closed.as_mut());
        let written = tokio::time::timeout(Duration::from_secs(5), write)
            .await
            .expect("the close wake must end a blocked write");
        assert!(!written);
    }

    /// With a reading peer and no close, the raced write completes.
    #[tokio::test]
    async fn raced_write_completes_for_a_reading_peer() {
        let (mut ours, mut peer) = tokio::io::duplex(1024);
        let wake = tokio::sync::Notify::new();
        let closed = wake.notified();
        tokio::pin!(closed);
        assert!(write_raced(&mut ours, b"frame", closed.as_mut()).await);
        let mut got = [0u8; 5];
        peer.read_exact(&mut got)
            .await
            .expect("peer reads the frame");
        assert_eq!(&got, b"frame");
    }
}
