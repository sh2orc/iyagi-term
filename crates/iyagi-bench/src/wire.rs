//! Raw wire client for the daemon's transport (u32-LE-length + JSON frames,
//! spec `01-contracts.md` §3), independent of the daemon's test harness.
//!
//! Frames are stamped with the instant their body finished arriving (taken
//! in the reader thread), so request→event latency is measured at the
//! transport boundary, not at processing time.
//!
//! Windows note (mirrors `crates/iyagi-termd/tests/common/mod.rs`): a named
//! pipe opened once and duplicated serializes synchronous writes behind a
//! pending blocking `ReadFile` on the sibling handle, so the reader peeks
//! instead of blocking. `time_begin_period_1ms` is called at startup so the
//! 1 ms peek poll actually sleeps ~1 ms instead of ~15.6 ms.

use std::collections::VecDeque;
#[cfg(windows)]
use std::fs::File;

/// Data-channel write half. Named pipes are files on Windows; Unix-domain
/// sockets are UnixStreams. (개발 호스트가 Windows이라 File로 굳어 있던
/// 타입을 OS별 별칭으로 정리 — SOTA_GAP_REVIEW W1-11: bench 기준선을
/// macOS/Linux에서도 측정·커밋하기 위한 최소 수정.)
#[cfg(unix)]
type WireWriter = std::os::unix::net::UnixStream;
#[cfg(windows)]
type WireWriter = File;
use std::io::{Read, Write};
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Hard transport cap (`rpc_frame_bytes`, 64 KiB).
pub const MAX_FRAME_BYTES: usize = 65_536;

/// One decoded frame with its arrival timestamp.
#[derive(Debug, Clone)]
pub struct Stamped {
    pub value: Value,
    pub at: Instant,
}

/// Encode a frame: 4-byte little-endian length + JSON body.
pub fn encode_frame(value: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).expect("serialize frame body");
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    frame
}

/// Decode one complete frame byte string (inverse of [`encode_frame`]).
pub fn decode_frame_bytes(bytes: &[u8]) -> Option<Value> {
    if bytes.len() < 4 {
        return None;
    }
    let len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    if 4 + len != bytes.len() || len > MAX_FRAME_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes[4..]).ok()
}

/// A connected client (control or data role). Events broadcast by the
/// daemon are stashed in [`Conn::events`]; responses are matched by id.
pub struct Conn {
    writer: WireWriter,
    frames: Receiver<Stamped>,
    next_id: u64,
    /// Broadcast events (`workload.changed`, `queue.changed`, ...) seen
    /// while waiting for responses, oldest first.
    pub events: VecDeque<Stamped>,
    /// One-shot data token from the control hello (control conns only).
    pub data_token: Option<String>,
}

impl Conn {
    /// Perform a control-role hello against the daemon token.
    pub fn control(endpoint: &str, token: &str) -> Result<Conn, String> {
        let mut conn = Conn::new(endpoint)?;
        let result = conn.hello(json!({
            "client_id": uuid_v4(),
            "token": token,
            "role": "control",
        }))?;
        conn.data_token = result
            .get("data_token")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if conn.data_token.is_none() {
            return Err("control hello result has no data_token".into());
        }
        Ok(conn)
    }

    /// Perform a data-role hello, redeeming the control hello's one-shot
    /// token (5 s TTL — redeem promptly).
    pub fn data(endpoint: &str, data_token: &str) -> Result<Conn, String> {
        let mut conn = Conn::new(endpoint)?;
        conn.hello(json!({
            "client_id": uuid_v4(),
            "token": data_token,
            "role": "data",
        }))?;
        Ok(conn)
    }

    fn new(endpoint: &str) -> Result<Conn, String> {
        // Retry: the daemon may be between listener instances on Windows.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match connect_once(endpoint) {
                Ok((reader, writer)) => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    thread::spawn(move || read_loop(reader, tx));
                    return Ok(Conn {
                        writer,
                        frames: rx,
                        next_id: 1,
                        events: VecDeque::new(),
                        data_token: None,
                    });
                }
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                Err(e) => return Err(format!("connect to {endpoint} failed: {e}")),
            }
        }
    }

    fn hello(&mut self, params: Value) -> Result<Value, String> {
        self.send_frame(&json!({
            "v": 1,
            "id": "hello",
            "method": "hello",
            "params": params,
        }))
        .map_err(|e| format!("hello write: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("hello response timeout".into());
            }
            let frame = self
                .frames
                .recv_timeout(remaining)
                .map_err(|_| "connection closed before hello response".to_string())?;
            if frame.value.get("id").and_then(|v| v.as_str()) == Some("hello") {
                if let Some(result) = frame.value.get("result") {
                    return Ok(result.clone());
                }
                return Err(format!("hello rejected: {}", frame.value["error"]));
            }
        }
    }

    pub fn send_frame(&mut self, value: &Value) -> std::io::Result<()> {
        let bytes = encode_frame(value);
        self.writer.write_all(&bytes)?;
        self.writer.flush()
    }

    fn alloc_id(&mut self) -> String {
        let id = format!("req-{}", self.next_id);
        self.next_id += 1;
        id
    }

    /// Send a request and return the instant the frame was handed to the
    /// transport (just before `write_all`), for send→reply latency math.
    pub fn request_timed(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<(String, Instant), std::io::Error> {
        let id = self.alloc_id();
        let frame = encode_frame(&json!({
            "v": 1,
            "id": id,
            "method": method,
            "params": params,
        }));
        let at = Instant::now();
        self.writer.write_all(&frame)?;
        self.writer.flush()?;
        Ok((id, at))
    }

    /// Wait for the response to a request id (stashing events meanwhile).
    pub fn wait_response(&mut self, id: &str, timeout: Duration) -> Option<Result<Value, Value>> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let frame = self.frames.recv_timeout(remaining).ok()?;
            if frame.value.get("id").and_then(|v| v.as_str()) == Some(id) {
                return Some(match frame.value.get("result") {
                    Some(result) => Ok(result.clone()),
                    None => Err(frame.value.get("error").cloned().unwrap_or(Value::Null)),
                });
            }
            self.stash(frame);
        }
    }

    /// Standard request/response with a 30 s ceiling.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let (id, _at) = self
            .request_timed(method, params)
            .map_err(|e| json!({"code": "CLIENT_IO", "message": e.to_string()}))?;
        match self.wait_response(&id, Duration::from_secs(30)) {
            Some(result) => result,
            None => Err(json!({
                "code": "CLIENT_TIMEOUT",
                "message": format!("no response for {method} within 30s"),
            })),
        }
    }

    /// Data-connection ACK (no response frame on success, spec §4).
    pub fn send_ack(&mut self, session_id: &str, epoch: &str, through_seq: u64) {
        let id = self.alloc_id();
        let _ = self.send_frame(&json!({
            "v": 1,
            "id": id,
            "method": "session.ack",
            "params": {
                "session_id": session_id,
                "epoch": epoch,
                "through_seq": through_seq.to_string(),
            },
        }));
    }

    fn stash(&mut self, frame: Stamped) {
        if frame.value.get("event").is_some() {
            self.events.push_back(frame);
        }
    }

    /// Non-blocking-ish receive with a small timeout.
    pub fn try_recv_frame(&mut self, timeout: Duration) -> Option<Stamped> {
        self.frames.recv_timeout(timeout).ok()
    }

    /// Drain whatever is immediately pending into the events stash.
    pub fn drain_events(&mut self) {
        while let Ok(frame) = self.frames.try_recv() {
            self.stash(frame);
        }
    }

    /// Take the first stashed event of `kind` (scanned in order).
    pub fn pop_event(&mut self, kind: &str) -> Option<Stamped> {
        self.events
            .iter()
            .position(|f| f.value.get("event").and_then(|v| v.as_str()) == Some(kind))
            .map(|pos| self.events.remove(pos).expect("position just checked"))
    }
}

fn read_loop(mut reader: Box<dyn Read + Send>, tx: std::sync::mpsc::Sender<Stamped>) {
    let mut header = [0u8; 4];
    loop {
        if reader.read_exact(&mut header).is_err() {
            return;
        }
        let len = u32::from_le_bytes(header) as usize;
        if len > MAX_FRAME_BYTES {
            return;
        }
        let mut frame = header.to_vec();
        frame.resize(4 + len, 0);
        if reader.read_exact(&mut frame[4..]).is_err() {
            return;
        }
        let at = Instant::now();
        match decode_frame_bytes(&frame) {
            Some(value) => {
                if tx.send(Stamped { value, at }).is_err() {
                    return;
                }
            }
            None => return,
        }
    }
}

fn connect_once(endpoint: &str) -> std::io::Result<(Box<dyn Read + Send>, WireWriter)> {
    #[cfg(unix)]
    {
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
        Ok((Box::new(PeekReader { file }), writer))
    }
}

/// Named-pipe reader that only calls `ReadFile` when bytes are known to be
/// available, so the sibling writer handle never queues behind a pending
/// blocking read (Windows-only).
#[cfg(windows)]
struct PeekReader {
    file: File,
}

#[cfg(windows)]
impl Read for PeekReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::os::windows::io::AsRawHandle;
        loop {
            let available = unsafe { peek_available(self.file.as_raw_handle())? };
            if available > 0 {
                return self.file.read(buf);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(windows)]
unsafe fn peek_available(handle: std::os::windows::io::RawHandle) -> std::io::Result<u32> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
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

/// Ask Windows for 1 ms sleep granularity for this process, so the peek
/// poll above (and bench pacing) does not quantize to the ~15.6 ms default
/// timer. Best-effort; returns false when winmm is unavailable.
#[cfg(windows)]
pub fn time_begin_period_1ms() -> bool {
    #[link(name = "winmm")]
    unsafe extern "system" {
        fn timeBeginPeriod(period: u32) -> u32;
    }
    // 0 == TIMERR_NOERROR
    unsafe { timeBeginPeriod(1) == 0 }
}

#[cfg(not(windows))]
pub fn time_begin_period_1ms() -> bool {
    false
}

pub fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn b64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn unb64(text: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let value = json!({"v": 1, "id": "x", "method": "hello", "params": {}});
        let bytes = encode_frame(&value);
        assert_eq!(decode_frame_bytes(&bytes), Some(value));
    }

    #[test]
    fn frame_decode_rejects_mismatched_and_oversize() {
        let value = json!({"a": 1});
        let mut bytes = encode_frame(&value);
        bytes[3] += 1; // corrupt length
        assert_eq!(decode_frame_bytes(&bytes), None);
        assert_eq!(decode_frame_bytes(&[]), None);
        let big = [(MAX_FRAME_BYTES as u32 + 1).to_le_bytes(), [0u8; 4]].concat();
        assert_eq!(decode_frame_bytes(&big), None);
    }

    #[test]
    fn frame_encode_prefixes_le_length() {
        let value = json!({"k": 1234});
        let bytes = encode_frame(&value);
        let len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        assert_eq!(len + 4, bytes.len());
    }
}
