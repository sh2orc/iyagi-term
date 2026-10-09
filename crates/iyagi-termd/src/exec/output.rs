//! Bounded pipe-output plumbing for PTY-less Exec children
//! (docs/orchestration/03-adapters.md §2, ticket O07).
//!
//! Contract highlights:
//! * raw protocol line/frame cap 1 MiB — an input that never sees a newline
//!   is cut at the cap and the run is marked `RESULT_INVALID` at finalize
//!   (03 §2). Cut bytes still flow to the caller's artifact-like sink so the
//!   body is preserved outside the IPC path.
//! * stderr keeps a bounded in-memory diagnostic tail; secret redaction runs
//!   *before* diagnostics retention (03 §2).
//! * stdout/stderr rings default to 1 MiB per stream; eviction only drops
//!   the oldest retained bytes — totals keep counting.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::{OutputSink, PathValidator};

/// Raw protocol line cap: 1 MiB (03 §2).
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Default retained tail per stream: 1 MiB.
pub const DEFAULT_SPOOL_BYTES: usize = 1024 * 1024;

/// A bounded protocol inbox shared by the print and interactive adapters.
/// Overflow invalidates the whole stream; a pipe pump never waits on a
/// stopped protocol consumer. Limits apply before allocating a queued copy.
pub struct LineInbox {
    receiver: std::sync::mpsc::Receiver<Vec<u8>>,
    bytes: Arc<std::sync::atomic::AtomicUsize>,
    overflow: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxError {
    Overflow,
    Closed,
    Timeout,
}

pub fn bounded_stdout_inbox() -> (OutputSink, LineInbox) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let (sender, receiver) = std::sync::mpsc::sync_channel(256);
    let bytes = Arc::new(AtomicUsize::new(0));
    let overflow = Arc::new(AtomicBool::new(false));
    let buffered = bytes.clone();
    let overrun = overflow.clone();
    let sink: OutputSink = Arc::new(move |stream, line| {
        if stream != StreamKind::Stdout || overrun.load(Ordering::Acquire) {
            return;
        }
        if buffered
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(line.len()).filter(|n| *n <= 8 * 1024 * 1024)
            })
            .is_err()
        {
            overrun.store(true, Ordering::Release);
            return;
        }
        if sender.try_send(line.to_vec()).is_err() {
            buffered.fetch_sub(line.len(), Ordering::AcqRel);
            overrun.store(true, Ordering::Release);
        }
    });
    (
        sink,
        LineInbox {
            receiver,
            bytes,
            overflow,
        },
    )
}

impl LineInbox {
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Result<Vec<u8>, InboxError> {
        use std::sync::atomic::Ordering;
        if self.overflow.load(Ordering::Acquire) {
            return Err(InboxError::Overflow);
        }
        let result = self.receiver.recv_timeout(timeout).map_err(|e| match e {
            std::sync::mpsc::RecvTimeoutError::Timeout => InboxError::Timeout,
            std::sync::mpsc::RecvTimeoutError::Disconnected => InboxError::Closed,
        });
        if let Ok(line) = &result {
            self.bytes.fetch_sub(line.len(), Ordering::AcqRel);
        }
        if self.overflow.load(Ordering::Acquire) {
            return Err(InboxError::Overflow);
        }
        result
    }
}

/// Which pipe a chunk belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Stdout,
    Stderr,
}

/// Secret scrubbing hook applied to both pipe sinks and diagnostic retention
/// (03 §2: "秘密値のredactionは診断保存前に行う").
pub trait Redactor: Send + Sync {
    fn redact(&self, line: &mut String);
}

/// Default no-op redactor.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRedactor;

impl Redactor for NoRedactor {
    fn redact(&self, _line: &mut String) {}
}

/// Closure-backed redactor for tests and small wiring sites.
pub struct FnRedactor<F: Fn(&mut String) + Send + Sync>(pub F);

impl<F: Fn(&mut String) + Send + Sync> Redactor for FnRedactor<F> {
    fn redact(&self, line: &mut String) {
        (self.0)(line)
    }
}

/// Verdict over the captured output once the stream reached EOF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputVerdict {
    /// Every line fit the raw line cap.
    Valid,
    /// At least one line was cut at the 1 MiB cap; the caller must mark the
    /// run `RESULT_INVALID` (03 §2), never success.
    Invalid { cuts: u32 },
}

/// Fixed-size byte ring retaining the most recent `cap` bytes pushed.
/// Pushing never fails; old bytes fall out of the head and are counted.
pub struct BoundedSpool {
    cap: usize,
    buf: VecDeque<u8>,
    total_pushed: u64,
    evicted: u64,
}

impl BoundedSpool {
    pub fn new(cap: usize) -> Self {
        BoundedSpool {
            // A zero/negative cap is nonsense; keep at least one byte so the
            // newest data is always observable.
            cap: cap.max(1),
            buf: VecDeque::new(),
            total_pushed: 0,
            evicted: 0,
        }
    }

    /// Append one line (with its trailing newline when present).
    pub fn push_line(&mut self, line: &[u8]) {
        self.total_pushed = self.total_pushed.saturating_add(line.len() as u64);
        self.buf.extend(line.iter().copied());
        while self.buf.len() > self.cap {
            self.buf.pop_front();
            self.evicted = self.evicted.saturating_add(1);
        }
    }

    /// Retained tail as lossy UTF-8 (diagnostics are for humans/logs).
    pub fn take_tail(&mut self) -> String {
        String::from_utf8_lossy(self.buf.make_contiguous()).into_owned()
    }

    pub fn retained_bytes(&self) -> usize {
        self.buf.len()
    }

    pub fn total_pushed(&self) -> u64 {
        self.total_pushed
    }

    pub fn evicted_bytes(&self) -> u64 {
        self.evicted
    }
}

/// Byte→line splitter with the 03 §2 raw line cap. `feed` delivers every
/// complete line (newline included); a line that reaches `max_line` without
/// a newline is flushed to `on_cut`, the rest of that line is swallowed, and
/// the cut counter increments. `finish` returns a short trailing fragment
/// that never saw a newline (kept, not marked invalid — the *missing final
/// event* is what invalidates the run, see E19).
pub struct LineCutter {
    pending: Vec<u8>,
    skipping: bool,
    cuts: u32,
    max_line: usize,
}

impl LineCutter {
    pub fn new(max_line: usize) -> Self {
        LineCutter {
            pending: Vec::with_capacity(8 * 1024),
            skipping: false,
            cuts: 0,
            max_line: max_line.max(1),
        }
    }

    /// Feed one read chunk. Returns how many lines were cut in this chunk.
    pub fn feed<F>(&mut self, chunk: &[u8], mut on_line: F) -> u32
    where
        F: FnMut(&[u8]),
    {
        let mut new_cuts = 0u32;
        for &b in chunk {
            if b == b'\n' {
                if self.skipping {
                    // Tail of an already-cut oversized line: discard.
                    self.skipping = false;
                    self.pending.clear();
                    continue;
                }
                self.pending.push(b'\n');
                on_line(&self.pending);
                self.pending.clear();
            } else if self.skipping {
                continue;
            } else {
                self.pending.push(b);
                if self.pending.len() >= self.max_line {
                    // 03 §2: cut at the cap; the run becomes RESULT_INVALID.
                    new_cuts += 1;
                    self.cuts += 1;
                    on_line(&self.pending);
                    self.pending.clear();
                    self.skipping = true;
                }
            }
        }
        new_cuts
    }

    /// EOF: the trailing fragment without a newline, if any. Not an overflow
    /// by itself — callers decide validity from the protocol view.
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        if self.skipping || self.pending.is_empty() {
            self.pending.clear();
            self.skipping = false;
            return None;
        }
        Some(std::mem::take(&mut self.pending))
    }

    pub fn cuts(&self) -> u32 {
        self.cuts
    }
}

/// One observed write outside the allowed scope. Policing writes is O06's
/// real capture; O07 only routes the fake child's declared writes through
/// the [`super::SpawnRequest::validate_path`] hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathViolation {
    pub path: String,
    pub bytes: u64,
}

/// Verdict of the path-validation hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathVerdict {
    Allow,
    Deny,
}

/// Aggregate state shared between one child's pump tasks and its handle.
pub(crate) struct StreamTap {
    pub kind: StreamKind,
    pub spool: Mutex<BoundedSpool>,
    /// Artifact-like sink: every complete (possibly cut) line, unbounded —
    /// the sink's own storage policy (O05) bounds persistence.
    pub sink: OutputSink,
    /// Redaction before either stream reaches a sink or diagnostic tail.
    pub redactor: Option<Arc<dyn Redactor>>,
    /// O07 fake-child file-write policing hook.
    pub validate_path: Option<PathValidator>,
    pub violations: Arc<Mutex<Vec<PathViolation>>>,
    pub total_bytes: std::sync::atomic::AtomicU64,
    pub cuts: std::sync::atomic::AtomicU32,
    pub done: std::sync::atomic::AtomicBool,
}

impl StreamTap {
    pub(crate) fn new(
        kind: StreamKind,
        spool_bytes: usize,
        sink: OutputSink,
        redactor: Option<Arc<dyn Redactor>>,
        validate_path: Option<PathValidator>,
        violations: Arc<Mutex<Vec<PathViolation>>>,
    ) -> Self {
        StreamTap {
            kind,
            spool: Mutex::new(BoundedSpool::new(spool_bytes)),
            sink,
            redactor,
            validate_path,
            violations,
            total_bytes: std::sync::atomic::AtomicU64::new(0),
            cuts: std::sync::atomic::AtomicU32::new(0),
            done: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// One complete line: redact before either sink or bounded tail, plus the
    /// fake-child write policing hook when the line declares a file write.
    pub(crate) fn line(&self, line: &[u8]) {
        use std::sync::atomic::Ordering::Relaxed;
        self.total_bytes.fetch_add(line.len() as u64, Relaxed);
        // A cut can end in the middle of a secret. Scrubbing complete tokens
        // cannot protect that prefix, so omit cut diagnostics when a secret
        // redactor is present. The stream's cut counter still invalidates it.
        let line = if self.redactor.is_some() && line.len() >= MAX_LINE_BYTES {
            b"[oversized output omitted]\n".as_slice()
        } else {
            line
        };
        let redacted = self.redactor.as_ref().map(|redactor| {
            let mut text = String::from_utf8_lossy(line).into_owned();
            redactor.redact(&mut text);
            text
        });
        let line = redacted.as_deref().map(str::as_bytes).unwrap_or(line);
        (self.sink)(self.kind, line);
        if let Some(validator) = self.validate_path.as_ref() {
            self.police_declared_write(line, validator);
        }
        let mut spool = self.spool.lock().unwrap_or_else(|p| p.into_inner());
        match self.kind {
            StreamKind::Stdout => spool.push_line(line),
            StreamKind::Stderr => {
                // Diagnostics: redact before retention (03 §2). The
                // original trailing newline (when present) is preserved.
                let mut bytes = line.to_vec();
                if bytes.last() != Some(&b'\n') {
                    bytes.push(b'\n');
                }
                spool.push_line(&bytes);
            }
        }
    }

    pub(crate) fn note_cut(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.cuts.fetch_add(1, Relaxed);
    }

    /// The term-fixture `agent-fake` protocol declares writes as
    /// `{"t":"file_write","path":...,"bytes":N}` before touching the file
    /// (key order is not guaranteed — serde_json sorts map keys by default,
    /// so the check parses instead of prefix-matching). O06 replaces this
    /// observer with real workspace capture.
    fn police_declared_write(&self, line: &[u8], validator: &PathValidator) {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            return; // non-JSON lines are normal activity/flood content
        };
        if value.get("t").and_then(|t| t.as_str()) != Some("file_write") {
            return;
        }
        let (Some(path), bytes) = (
            value.get("path").and_then(|p| p.as_str()),
            value.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0),
        ) else {
            return;
        };
        if validator(path, bytes) == PathVerdict::Deny {
            self.violations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(PathViolation {
                    path: path.to_string(),
                    bytes,
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spool_retains_tail_and_counts_eviction() {
        let mut spool = BoundedSpool::new(16);
        spool.push_line(b"abcdefghij\n"); // 11
        spool.push_line(b"klmnopqrst\n"); // 11 → 22 > 16, evict 6
        assert_eq!(spool.retained_bytes(), 16);
        assert_eq!(spool.evicted_bytes(), 6);
        assert_eq!(spool.total_pushed(), 22);
        assert_eq!(spool.take_tail(), "ghij\nklmnopqrst\n");
    }

    #[test]
    fn spool_keeps_latest_when_single_line_exceeds_cap() {
        let mut spool = BoundedSpool::new(4);
        spool.push_line(b"0123456789");
        assert_eq!(spool.retained_bytes(), 4);
        assert_eq!(spool.take_tail(), "6789");
    }

    #[test]
    fn line_cutter_emits_lines_and_marks_overflow() {
        let mut cutter = LineCutter::new(8);
        let mut lines = Vec::new();
        cutter.feed(b"ab\ncd", |l| lines.push(l.to_vec()));
        assert_eq!(lines, vec![b"ab\n".to_vec()]);
        // 8-byte cap without newline: cut at exactly 8, swallow the rest.
        let cuts = cutter.feed(b"efghijklmnop\nnext\n", |l| lines.push(l.to_vec()));
        assert_eq!(cuts, 1);
        assert_eq!(cutter.cuts(), 1);
        // 'cdefghij' (the pending 'cd' plus new bytes) flushed as the cut
        // line, 'klmnop' swallowed, 'next\n' delivered.
        assert_eq!(
            lines,
            vec![b"ab\n".to_vec(), b"cdefghij".to_vec(), b"next\n".to_vec()]
        );
        assert_eq!(cutter.finish(), None);
    }

    #[test]
    fn line_cutter_finish_returns_short_trailing_fragment() {
        let mut cutter = LineCutter::new(64);
        let mut seen = Vec::new();
        cutter.feed(b"{\"t\":\"result\",\"value\":{\"kind\":\"rep", |l| {
            seen.push(l.to_vec())
        });
        assert!(seen.is_empty());
        assert_eq!(
            cutter.finish(),
            Some(b"{\"t\":\"result\",\"value\":{\"kind\":\"rep".to_vec())
        );
        assert_eq!(cutter.finish(), None, "finish consumes the fragment");
        assert_eq!(cutter.cuts(), 0, "short fragment is not an overflow");
    }

    #[test]
    fn stderr_lines_are_redacted_before_retention() {
        let tap = StreamTap::new(
            StreamKind::Stderr,
            1024,
            Arc::new(|_, _| {}),
            Some(Arc::new(FnRedactor(|line: &mut String| {
                if let Some(pos) = line.find("sk-") {
                    let end = line[pos..]
                        .find(char::is_whitespace)
                        .map(|offset| pos + offset)
                        .unwrap_or(line.len());
                    line.replace_range(pos..end, "[REDACTED]");
                }
            }))),
            None,
            Arc::new(Mutex::new(Vec::new())),
        );
        tap.line(b"auth failed for sk-abcdef1234 token\n");
        let tail = tap.spool.lock().unwrap().take_tail();
        assert_eq!(tail, "auth failed for [REDACTED] token\n");
    }

    #[test]
    fn both_sinks_scrub_secrets_and_omit_partial_keys_at_the_line_cap() {
        for kind in [StreamKind::Stdout, StreamKind::Stderr] {
            let observed = Arc::new(Mutex::new(Vec::new()));
            let sink = observed.clone();
            let tap = StreamTap::new(
                kind,
                2048,
                Arc::new(move |_, bytes| sink.lock().unwrap().extend_from_slice(bytes)),
                Some(Arc::new(crate::connections::SecretRedactor::new([
                    "fake-private-key".into(),
                ]))),
                None,
                Arc::new(Mutex::new(Vec::new())),
            );
            let mut cutter = LineCutter::new(MAX_LINE_BYTES);
            cutter.feed(b"echo fake-private-key\n", |line| tap.line(line));
            let mut oversized = vec![b'x'; MAX_LINE_BYTES - 12];
            oversized.extend_from_slice(b"fake-private-key\n");
            assert_eq!(cutter.feed(&oversized, |line| tap.line(line)), 1);
            let output = String::from_utf8(observed.lock().unwrap().clone()).unwrap();
            assert_eq!(output, "echo [redacted]\n[oversized output omitted]\n");
            assert_eq!(tap.spool.lock().unwrap().take_tail(), output);
        }
    }

    #[test]
    fn declared_write_lines_are_policed() {
        let violations = Arc::new(Mutex::new(Vec::new()));
        let tap = StreamTap::new(
            StreamKind::Stdout,
            1024,
            Arc::new(|_, _| {}),
            None,
            Some(Arc::new(|path: &str, _b: u64| {
                if path.contains("outside") {
                    PathVerdict::Deny
                } else {
                    PathVerdict::Allow
                }
            })),
            Arc::clone(&violations),
        );
        // Key order is not guaranteed (serde_json sorts) — both orders work.
        tap.line(br#"{"t":"file_write","path":"C:/outside/x.txt","bytes":8}"#);
        tap.line(br#"{"bytes":8,"path":"C:/ws/ok.txt","t":"file_write"}"#);
        tap.line(br#"{"t":"activity","text":"hello"}"#);
        let violations = violations.lock().unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, "C:/outside/x.txt");
    }
}
