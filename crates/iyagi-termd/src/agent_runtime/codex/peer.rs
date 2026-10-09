//! # Codex app-server transport peers (ticket O08, docs/orchestration/03-adapters.md §3)
//!
//! Framing decision (recorded in `done/orchestration/O08.md`): the official
//! app-server speaks JSON-RPC 2.0 over stdio as **newline-delimited JSON**
//! (JSONL, MCP-style) with the `"jsonrpc":"2.0"` member omitted on the wire.
//! The installed CLI's generated schemas (`fixtures/ClientRequest.json`,
//! `fixtures/JSONRPCRequest.json`) contain no `jsonrpc` property and no
//! Content-Length framing — one JSON object per line is the only shape they
//! describe. There is NO LSP-style `Content-Length` header.
//!
//! [`ProtocolPeer`] abstracts the bidirectional channel so the exact same
//! [`crate::agent_runtime::codex::CodexAdapter`] engine runs against:
//! * [`RecordedPeer`] — an offline JSONL transcript (tests, CI, replay), and
//! * [`LivePeer`] — the spawned `codex app-server` child over piped stdio.
//!
//! 03 §2 input limits: raw protocol lines are capped at 1 MiB (the exec
//! supervisor's `MAX_LINE_BYTES`); an over-long line is reported as
//! [`PeerEvent::Overcap`] and the adapter maps it to `RESULT_INVALID`,
//! never to a silent truncation. stderr goes to a bounded diagnostics tail.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

/// Re-exported cap so the codex protocol path stays in lockstep with the
/// generic exec path (03 §2: 1 MiB raw line maximum).
pub use crate::exec::MAX_LINE_BYTES;

/// Bounded stderr diagnostics tail kept by [`LivePeer`] (03 §2).
const DIAGNOSTICS_TAIL_BYTES: usize = 8 * 1024;

/// Why a send failed (broken pipe / child gone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerError(pub String);

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PeerError {}

/// What a [`ProtocolPeer::recv`] produced.
#[derive(Debug, Clone, PartialEq)]
pub enum PeerEvent {
    /// One decoded server→client JSON message.
    Message(Value),
    /// The peer closed the channel cleanly (child exit, transcript end).
    Eof,
    /// The channel broke mid-stream (io error / scripted drop).
    ConnectionLost,
    /// A raw line exceeded [`MAX_LINE_BYTES`] without a newline (03 §2).
    Overcap,
}

/// Bidirectional newline-delimited JSON-RPC channel to the app-server.
pub trait ProtocolPeer: Send + Sync {
    /// Production scope is checked before sending any API key or prompt.
    fn auth_scope(&self) -> Option<super::auth::AuthScope> {
        None
    }
    /// Single-use local credential, sent only through official login/start.
    fn take_api_key(&self) -> Option<zeroize::Zeroizing<String>> {
        None
    }
    /// Send one client→server message (request, notification, or response to
    /// a server request). The caller serializes; the wire adds only `\n`.
    fn send(&self, message: &Value) -> Result<(), PeerError>;
    /// Receive the next server→client message. Blocking for live peers.
    fn recv(&self) -> PeerEvent;
    /// Tear the channel down.
    fn close(&self);
    /// A terminal protocol event alone is not process-cleanup evidence.
    fn cleanup_confirmed(&self) -> bool {
        true
    }
}

// ---- recorded transcripts ---------------------------------------------------

/// One JSONL transcript line. `dir:"c"` lines are expected client→server
/// sends (matched by method); `dir:"s"` lines are replayed server→client
/// messages; `dir:"eof"` is a clean end; `dir:"drop"` is a mid-stream
/// connection loss.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "dir", rename_all = "lowercase")]
pub enum TranscriptLine {
    /// Expected client→server message: the adapter must send this method.
    C { msg: Value },
    /// Replayed server→client message.
    S { msg: Value },
    /// Clean end of stream.
    Eof,
    /// Connection lost mid-stream.
    Drop,
}

/// Parse a transcript file. Rejects over-cap lines so a fat fixture can
/// never mask the 03 §2 limit.
pub fn load_transcript(path: &Path) -> Result<Vec<TranscriptLine>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("transcript read {}: {e}", path.display()))?;
    parse_transcript(&raw)
}

/// Pure core of [`load_transcript`] (unit-testable without files).
pub fn parse_transcript(raw: &str) -> Result<Vec<TranscriptLine>, String> {
    let mut lines = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(format!(
                "transcript line {} exceeds the {} byte raw cap (03 §2)",
                index + 1,
                MAX_LINE_BYTES
            ));
        }
        let parsed: TranscriptLine = serde_json::from_str(line)
            .map_err(|e| format!("transcript line {}: {e}", index + 1))?;
        lines.push(parsed);
    }
    Ok(lines)
}

/// Offline peer replaying a recorded transcript. Playback walks ONE cursor
/// over the recorded interleave: `dir:"s"` lines replay in order, `dir:"c"`
/// lines are gates — `recv` blocks at a gate until the client actually sends
/// that method (or the peer closes). That reproduces the real causality of
/// the protocol (response after request, approval answer after the request
/// arrived, steer into an open turn) without any test-side races: once the
/// consumer observes the event that precedes a gate, the gate is the cursor
/// head and the matching send is deterministic.
pub struct RecordedPeer {
    /// Remaining transcript lines (both directions) in recorded order.
    lines: Mutex<VecDeque<TranscriptLine>>,
    /// Wakes gate-blocked readers when a send consumed their gate.
    gate_wake: std::sync::Condvar,
    closed: AtomicBool,
    /// Everything the client actually sent (for param assertions).
    sent: Mutex<Vec<Value>>,
    /// Protocol deviations (send after close).
    mismatches: Mutex<Vec<String>>,
}

impl RecordedPeer {
    /// Build a peer from parsed transcript lines.
    pub fn new(lines: Vec<TranscriptLine>) -> Arc<Self> {
        Arc::new(RecordedPeer {
            lines: Mutex::new(lines.into_iter().collect()),
            gate_wake: std::sync::Condvar::new(),
            closed: AtomicBool::new(false),
            sent: Mutex::new(Vec::new()),
            mismatches: Mutex::new(Vec::new()),
        })
    }

    /// Build a peer from a transcript file.
    pub fn from_file(path: &Path) -> Result<Arc<Self>, String> {
        Ok(Self::new(load_transcript(path)?))
    }

    /// Everything the client sent, in order (gated and spontaneous).
    pub fn sent_messages(&self) -> Vec<Value> {
        self.sent.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Methods the client sent, in order — cheap assertion helper
    /// (responses to server requests render as `<response>`).
    pub fn sent_methods(&self) -> Vec<String> {
        self.sent_messages()
            .iter()
            .map(|m| {
                m.get("method")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| "<response>".into())
            })
            .collect()
    }

    /// Recorded protocol deviations; empty means the client matched the
    /// transcript exactly.
    pub fn mismatches(&self) -> Vec<String> {
        self.mismatches
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl ProtocolPeer for RecordedPeer {
    fn send(&self, message: &Value) -> Result<(), PeerError> {
        if self.closed.load(Ordering::Acquire) {
            let error = format!("send after close: {message}");
            self.mismatches
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(error.clone());
            return Err(PeerError(error));
        }
        self.sent
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(message.clone());
        {
            let mut lines = self.lines.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(TranscriptLine::C { msg: next }) = lines.front() {
                // Match by method (requests/notifications); responses to
                // server requests have no method on either side.
                if next.get("method") == message.get("method") {
                    lines.pop_front();
                    drop(lines);
                    self.gate_wake.notify_all();
                }
            }
            // Anything else is a spontaneous send (a response or an
            // interrupt issued at an arbitrary point): recorded, cursor
            // untouched.
        }
        Ok(())
    }

    fn recv(&self) -> PeerEvent {
        let mut lines = self.lines.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            match lines.front().cloned() {
                Some(TranscriptLine::S { msg }) => {
                    lines.pop_front();
                    return PeerEvent::Message(msg);
                }
                Some(TranscriptLine::Eof) => {
                    lines.pop_front();
                    return PeerEvent::Eof;
                }
                Some(TranscriptLine::Drop) => {
                    lines.pop_front();
                    return PeerEvent::ConnectionLost;
                }
                Some(TranscriptLine::C { .. }) => {
                    // Gate: block until the client sends it or close().
                    if self.closed.load(Ordering::Acquire) {
                        return PeerEvent::Eof;
                    }
                    lines = self
                        .gate_wake
                        .wait(lines)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                None => return PeerEvent::Eof,
            }
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.gate_wake.notify_all();
    }
}

// ---- live child -------------------------------------------------------------

/// Live `codex app-server` child over piped stdio.
///
/// Spawn choice (recorded in O08.md): the exec supervisor's
/// [`crate::exec::SpawnRequest`] writes the stdin payload once and closes the
/// pipe — it cannot express the long-lived bidirectional JSON-RPC channel the
/// app-server needs (steer / interrupt / approval replies arrive mid-run).
/// The live peer therefore owns a plain piped child (explicit program path +
/// argv, never a shell string) and keeps the write handle open for the whole
/// run. Admission/resource accounting reconciliation is listed as a gap in
/// O08.md; the recorded-stream evidence in this ticket does not depend on it.
pub struct LivePeer {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    reader: Mutex<LineReader<ChildStdout>>,
    /// Bounded stderr tail (03 §2 diagnostics).
    diagnostics: Arc<Mutex<String>>,
    /// Exit code observed at close (confirmed reap).
    exit_code: Mutex<Option<i32>>,
    closed: AtomicBool,
    reaped: AtomicBool,
    group_id: u32,
}

impl LivePeer {
    /// Spawn the app-server: explicit program path + argv (never a shell
    /// string), piped stdio, `cwd` as working directory.
    pub fn spawn(program: &Path, argv: &[String], cwd: &Path) -> Result<Arc<Self>, PeerError> {
        let mut command = Command::new(program);
        command
            .args(argv)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|e| PeerError(format!("app-server spawn {}: {e}", program.display())))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PeerError("app-server stdout was not piped".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| PeerError("app-server stderr was not piped".into()))?;
        let diagnostics = Arc::new(Mutex::new(String::new()));
        // Bounded stderr spool: keep only the tail.
        let tail = Arc::clone(&diagnostics);
        std::thread::Builder::new()
            .name("codex-stderr".into())
            .spawn(move || spool_stderr(stderr, &tail))
            .map_err(|e| PeerError(format!("stderr spool thread: {e}")))?;
        let stdin = child.stdin.take();
        let group_id = child.id();
        Ok(Arc::new(LivePeer {
            reader: Mutex::new(LineReader::new(stdout)),
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            diagnostics,
            exit_code: Mutex::new(None),
            closed: AtomicBool::new(false),
            reaped: AtomicBool::new(false),
            group_id,
        }))
    }

    /// Bounded stderr tail captured so far (diagnostics only, 03 §2).
    pub fn diagnostics_tail(&self) -> String {
        self.diagnostics
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Exit code after [`ProtocolPeer::close`] reaped the child.
    pub fn observed_exit(&self) -> Option<i32> {
        *self.exit_code.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Reap the child: wait when it already exited, otherwise escalate
    /// TERM → grace → KILL against the owned process group, then wait inside
    /// a bound. Confirmed exit code, or `None` when the status could not be
    /// observed (a member in uninterruptible sleep outlives SIGKILL — the
    /// daemon logs it and moves on instead of parking on `wait` forever).
    fn kill_and_reap(&self) -> Option<i32> {
        /// Grace window between the group SIGTERM and SIGKILL: a clean
        /// app-server shutdown flushes its teardown before the hammer.
        const TERM_GRACE: Duration = Duration::from_millis(500);
        /// Bound on the final reap after SIGKILL.
        const REAP_BOUND: Duration = Duration::from_secs(5);
        /// Poll cadence of both windows (mirrors the exec ladder).
        const POLL: Duration = Duration::from_millis(15);

        let mut child = self.child.lock().unwrap_or_else(|p| p.into_inner());
        // Signal before reaping the root: its PID still belongs to this Child,
        // so a delayed cleanup cannot target a recycled process. Direct group
        // signals — no shell — against the group this spawn created
        // (`process_group(0)`), so its members are the peer's own
        // descendants; TERM runs first so a well-behaved app-server shuts
        // down cleanly instead of being killed on sight.
        #[cfg(unix)]
        {
            let pgid = self.group_id as libc::pid_t;
            unsafe { libc::kill(-pgid, libc::SIGTERM) };
            let deadline = std::time::Instant::now() + TERM_GRACE;
            while std::time::Instant::now() < deadline {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(POLL);
            }
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
        #[cfg(windows)]
        let _ = Command::new("taskkill")
            .args(["/PID", &self.group_id.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
        let deadline = std::time::Instant::now() + REAP_BOUND;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.reaped.store(true, Ordering::Release);
                    return status.code();
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        group = self.group_id,
                        error = %error,
                        "app-server peer reap failed"
                    );
                    return None;
                }
            }
            if std::time::Instant::now() >= deadline {
                tracing::warn!(
                    group = self.group_id,
                    "app-server peer did not die after SIGKILL within the reap bound; leaving it to the OS"
                );
                return None;
            }
            std::thread::sleep(POLL);
        }
    }
}

impl ProtocolPeer for LivePeer {
    fn send(&self, message: &Value) -> Result<(), PeerError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(PeerError("app-server peer already closed".into()));
        }
        let mut line =
            serde_json::to_string(message).map_err(|e| PeerError(format!("encode: {e}")))?;
        line.push('\n');
        if line.len() > MAX_LINE_BYTES {
            return Err(PeerError(format!(
                "outbound line exceeds the {} byte raw cap (03 §2)",
                MAX_LINE_BYTES
            )));
        }
        let mut stdin = self.stdin.lock().unwrap_or_else(|p| p.into_inner());
        match stdin.as_mut() {
            Some(pipe) => pipe
                .write_all(line.as_bytes())
                .and_then(|_| pipe.flush())
                .map_err(|e| PeerError(format!("app-server stdin write: {e}"))),
            None => Err(PeerError("app-server stdin already closed".into())),
        }
    }

    fn recv(&self) -> PeerEvent {
        if self.closed.load(Ordering::Acquire) {
            return PeerEvent::Eof;
        }
        self.reader
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .next_event()
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        // Drop stdin so a well-behaved app-server winds down, then reap.
        *self.stdin.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.exit_code.lock().unwrap_or_else(|p| p.into_inner()) = self.kill_and_reap();
    }

    fn cleanup_confirmed(&self) -> bool {
        if !self.reaped.load(Ordering::Acquire) {
            return false;
        }
        #[cfg(unix)]
        {
            // Retain the actor's ownership if any descendant still holds
            // the group. This probe never signals a recycled process.
            return Command::new("kill")
                .args(["-0", &format!("-{}", self.group_id)])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| !s.success());
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

/// Cap-enforcing line reader: one JSON value per `\n`, [`PeerEvent::Overcap`]
/// when a line passes [`MAX_LINE_BYTES`] without a newline (03 §2 — the run
/// is RESULT_INVALID, never silently truncated).
struct LineReader<R: Read> {
    inner: BufReader<R>,
    scratch: Vec<u8>,
}

impl<R: Read> LineReader<R> {
    fn new(inner: R) -> Self {
        LineReader {
            inner: BufReader::new(inner),
            scratch: Vec::new(),
        }
    }

    fn next_event(&mut self) -> PeerEvent {
        self.scratch.clear();
        loop {
            let available = match self.inner.fill_buf() {
                Ok(buf) => buf,
                Err(_) => return PeerEvent::ConnectionLost,
            };
            if available.is_empty() {
                // Clean EOF: parse a trailing newline-less remainder if any.
                return match decode_line(&self.scratch) {
                    Some(value) => PeerEvent::Message(value),
                    None => PeerEvent::Eof,
                };
            }
            match available.iter().position(|b| *b == b'\n') {
                Some(pos) => {
                    self.scratch.extend_from_slice(&available[..pos]);
                    self.inner.consume(pos + 1);
                    return match decode_line(&self.scratch) {
                        Some(value) => PeerEvent::Message(value),
                        None => PeerEvent::ConnectionLost, // undecodable line
                    };
                }
                None => {
                    let len = available.len();
                    self.scratch.extend_from_slice(available);
                    self.inner.consume(len);
                    if self.scratch.len() > MAX_LINE_BYTES {
                        return PeerEvent::Overcap;
                    }
                }
            }
        }
    }
}

/// Decode one raw line; `None` for blank / non-JSON (treated as stream
/// corruption by the caller, never as success).
fn decode_line(raw: &[u8]) -> Option<Value> {
    let trimmed: &[u8] = match raw.last() {
        Some(b'\r') => &raw[..raw.len() - 1],
        _ => raw,
    };
    if trimmed.iter().all(|b| b.is_ascii_whitespace()) {
        return None;
    }
    serde_json::from_slice(trimmed).ok()
}

/// Append-only bounded tail for stderr.
fn spool_stderr<R: Read>(mut stderr: R, tail: &Mutex<String>) {
    let mut chunk = [0u8; 1024];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let mut tail = tail.lock().unwrap_or_else(|p| p.into_inner());
                tail.push_str(&String::from_utf8_lossy(&chunk[..n]));
                if tail.len() > DIAGNOSTICS_TAIL_BYTES {
                    let cut = tail.len() - DIAGNOSTICS_TAIL_BYTES;
                    tail.drain(..cut);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transcript_parser_reads_every_direction() {
        let raw = concat!(
            "{\"dir\":\"c\",\"msg\":{\"id\":1,\"method\":\"initialize\",\"params\":{}}}\n",
            "{\"dir\":\"s\",\"msg\":{\"id\":1,\"result\":{\"userAgent\":\"x\"}}}\n",
            "{\"dir\":\"eof\"}\n",
            "{\"dir\":\"drop\"}\n",
        );
        let lines = parse_transcript(raw).expect("parses");
        assert_eq!(lines.len(), 4);
        assert!(matches!(lines[0], TranscriptLine::C { .. }));
        assert!(matches!(lines[3], TranscriptLine::Drop));
    }

    #[test]
    fn transcript_parser_rejects_over_cap_lines() {
        let fat = format!(
            "{{\"dir\":\"s\",\"msg\":{{\"pad\":\"{}\"}}}}",
            "x".repeat(MAX_LINE_BYTES)
        );
        assert!(parse_transcript(&fat).is_err());
    }

    #[test]
    fn recorded_peer_gates_on_expected_sends_in_order() {
        let peer = RecordedPeer::new(
            parse_transcript(
                "{\"dir\":\"c\",\"msg\":{\"id\":1,\"method\":\"initialize\",\"params\":{}}}\n\
             {\"dir\":\"s\",\"msg\":{\"id\":1,\"result\":{\"userAgent\":\"x\"}}}\n\
             {\"dir\":\"c\",\"msg\":{\"method\":\"initialized\"}}\n",
            )
            .expect("parses"),
        );
        // Handshake order: the response only replays after the request.
        let reader = {
            let peer = Arc::clone(&peer);
            std::thread::spawn(move || peer.recv())
        };
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(
            !reader.is_finished(),
            "recv blocks at the request gate until the client sends"
        );
        peer.send(&json!({"id":1,"method":"initialize","params":{"x":1}}))
            .expect("gate matched");
        assert_eq!(
            reader.join().expect("reader"),
            PeerEvent::Message(json!({"id":1,"result":{"userAgent":"x"}}))
        );
        // Spontaneous send outside the script is recorded, not an error.
        assert!(peer
            .send(&json!({"id":9,"result":{"decision":"accept"}}))
            .is_ok());
        assert_eq!(peer.sent_methods(), vec!["initialize", "<response>"]);
        assert!(peer.mismatches().is_empty());
    }

    #[test]
    fn recorded_peer_replays_inbound_until_eof() {
        let peer = RecordedPeer::new(vec![
            TranscriptLine::S {
                msg: json!({"method":"item/agentMessage/delta","params":{"delta":"a"}}),
            },
            TranscriptLine::Eof,
        ]);
        assert_eq!(
            peer.recv(),
            PeerEvent::Message(json!({"method":"item/agentMessage/delta","params":{"delta":"a"}}))
        );
        assert_eq!(peer.recv(), PeerEvent::Eof);
        assert_eq!(
            peer.recv(),
            PeerEvent::Eof,
            "exhausted transcript stays EOF"
        );
    }

    #[test]
    fn line_reader_flags_overcap_before_any_newline() {
        let fat = vec![b'x'; MAX_LINE_BYTES + 16];
        let mut reader = LineReader::new(&fat[..]);
        assert_eq!(reader.next_event(), PeerEvent::Overcap);
    }

    #[test]
    fn line_reader_decodes_lines_and_clean_eof() {
        let stream = b"{\"id\":1,\"result\":{\"ok\":true}}\n{\"method\":\"x\"}\n";
        let mut reader = LineReader::new(&stream[..]);
        assert_eq!(
            reader.next_event(),
            PeerEvent::Message(json!({"id":1,"result":{"ok":true}}))
        );
        assert_eq!(
            reader.next_event(),
            PeerEvent::Message(json!({"method":"x"}))
        );
        assert_eq!(reader.next_event(), PeerEvent::Eof);
    }
}
