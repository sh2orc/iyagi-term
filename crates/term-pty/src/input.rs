//! Input path: bounded queue, idempotency dedup ring, resize coalescer
//! (spec `02-runner.md` §6).
//!
//! * UI input queue raw cap 64 KiB, max 4 KiB per single write chunk, one
//!   outstanding write at a time (the session's writer thread is the only
//!   consumer and pulls one chunk per PTY write).
//! * Paste > 1 MiB is rejected with "use file transfer" guidance; smaller
//!   pastes are fed as 4 KiB chunks as queue space allows. Byte-level UTF-8
//!   splitting is allowed on the terminal stream, but callers must encode a
//!   whole string to UTF-8 first and hand the bytes here — encoding is
//!   therefore applied consistently to the whole string.
//! * `InputDedup` remembers the last 256 `input_id -> outcome` entries; a
//!   duplicate id replays the remembered outcome instead of writing twice.
//!   A write that errors mid-way is recorded as `Unknown`.
//! * `ResizeCoalescer` applies the first resize immediately, then merges
//!   requests until the next 16 ms boundary, last one
//!   wins; identical dimensions are skipped.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use term_contracts::session::limits::{
    INPUT_CHUNK_BYTES, INPUT_DEDUP_ENTRIES, INPUT_QUEUE_BYTES, PASTE_BYTES, RESIZE_COALESCE_MS,
    TERMINAL_MAX_DIM, TERMINAL_MIN_DIM,
};

// ---------------------------------------------------------------------------
// Clock

/// Monotonic millisecond clock, injectable for tests.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Real monotonic clock (millis since an arbitrary fixed point).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_millis() as u64
    }
}

/// Manual clock for deterministic tests; `advance` moves time forward.
/// Cheap to clone: clones share the same virtual time.
#[derive(Debug, Clone, Default)]
pub struct ManualClock(Arc<ManualState>);

#[derive(Debug, Default)]
struct ManualState {
    ms: AtomicU64,
}

impl ManualClock {
    pub fn new(start_ms: u64) -> Self {
        let inner = Arc::new(ManualState::default());
        inner.ms.store(start_ms, Ordering::SeqCst);
        Self(inner)
    }

    pub fn advance(&self, ms: u64) {
        self.0.ms.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn set(&self, ms: u64) {
        self.0.ms.store(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.ms.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// Input queue

pub const MAX_CHUNK_BYTES: usize = INPUT_CHUNK_BYTES;
pub const MAX_QUEUE_BYTES: usize = INPUT_QUEUE_BYTES;
pub const MAX_PASTE_BYTES: usize = PASTE_BYTES;

/// xterm bracketed-paste markers (spec §6: the convention is preserved on
/// the wire while the paste string is chunked).
pub const BRACKETED_PASTE_OPEN: &[u8] = b"\x1b[200~";
pub const BRACKETED_PASTE_CLOSE: &[u8] = b"\x1b[201~";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InputQueueError {
    #[error("input chunk of {len} bytes exceeds the {max} byte single-write limit")]
    ChunkTooLarge { len: usize, max: usize },
    #[error("input queue full ({used}/{max} bytes); writer must drain first")]
    QueueFull { used: usize, max: usize },
    #[error("paste of {len} bytes exceeds the {max} byte limit; use file transfer instead")]
    PasteTooLarge { len: usize, max: usize },
    #[error("input queue is shutting down")]
    ShuttingDown,
}

/// One queued write: the idempotency id it belongs to plus the raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputChunk {
    pub input_id: String,
    pub bytes: Vec<u8>,
}

struct QueueState {
    chunks: VecDeque<InputChunk>,
    total_bytes: usize,
}

/// Bounded FIFO of pending input writes (spec §6 limits). Thread-safe;
/// producers may enqueue non-blockingly (`try` semantics) or wait for space.
pub struct InputQueue {
    state: Mutex<QueueState>,
    /// Signalled when bytes leave the queue (space opened) or on shutdown.
    space: Condvar,
    /// Signalled when a chunk arrives or on shutdown.
    items: Condvar,
    shutdown: AtomicBool,
}

impl InputQueue {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                chunks: VecDeque::new(),
                total_bytes: 0,
            }),
            space: Condvar::new(),
            items: Condvar::new(),
            shutdown: AtomicBool::new(false),
        }
    }

    fn validate_chunk(bytes: &[u8]) -> Result<(), InputQueueError> {
        if bytes.len() > MAX_CHUNK_BYTES {
            return Err(InputQueueError::ChunkTooLarge {
                len: bytes.len(),
                max: MAX_CHUNK_BYTES,
            });
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Enqueue one chunk (≤ 4 KiB). Non-blocking: `QueueFull` when the 64 KiB
    /// raw budget is exhausted, so callers can surface `BUSY`.
    pub fn try_enqueue(&self, input_id: &str, bytes: &[u8]) -> Result<(), InputQueueError> {
        Self::validate_chunk(bytes)?;
        let mut state = self.lock();
        if self.shutdown.load(Ordering::Acquire) {
            return Err(InputQueueError::ShuttingDown);
        }
        if state.total_bytes + bytes.len() > MAX_QUEUE_BYTES {
            return Err(InputQueueError::QueueFull {
                used: state.total_bytes,
                max: MAX_QUEUE_BYTES,
            });
        }
        state.chunks.push_back(InputChunk {
            input_id: input_id.to_string(),
            bytes: bytes.to_vec(),
        });
        state.total_bytes += bytes.len();
        drop(state);
        self.items.notify_all();
        Ok(())
    }

    /// Enqueue one chunk, waiting on the condvar until the writer drained
    /// enough space. Wakes immediately on shutdown with an error.
    pub fn enqueue_blocking(&self, input_id: &str, bytes: &[u8]) -> Result<(), InputQueueError> {
        Self::validate_chunk(bytes)?;
        let mut state = self.lock();
        loop {
            if self.shutdown.load(Ordering::Acquire) {
                return Err(InputQueueError::ShuttingDown);
            }
            if state.total_bytes + bytes.len() <= MAX_QUEUE_BYTES {
                state.chunks.push_back(InputChunk {
                    input_id: input_id.to_string(),
                    bytes: bytes.to_vec(),
                });
                state.total_bytes += bytes.len();
                drop(state);
                self.items.notify_all();
                return Ok(());
            }
            state = self.space.wait(state).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// Feed a paste. Rejects > 1 MiB (with file-transfer guidance); otherwise
    /// splits into 4 KiB chunks, queueing only as many as fit. Returns the
    /// number of bytes actually queued. Non-blocking variant.
    ///
    /// Partial acceptance is BEST-EFFORT only: when the queue fills mid-paste
    /// the tail (including a bracketed close marker) is not queued, and the
    /// dedup outcome records the last chunk's byte count. The daemon does not
    /// use this path (`write_input_await` carries paste chunks one by one);
    /// callers that need all-or-nothing must use `feed_paste_blocking`.
    pub fn feed_paste(
        &self,
        input_id: &str,
        data: &[u8],
        bracketed: bool,
    ) -> Result<usize, InputQueueError> {
        let overhead = if bracketed {
            BRACKETED_PASTE_OPEN.len() + BRACKETED_PASTE_CLOSE.len()
        } else {
            0
        };
        let total = data.len() + overhead;
        if total > MAX_PASTE_BYTES {
            return Err(InputQueueError::PasteTooLarge {
                len: total,
                max: MAX_PASTE_BYTES,
            });
        }
        let mut stream = Vec::with_capacity(total);
        if bracketed {
            stream.extend_from_slice(BRACKETED_PASTE_OPEN);
        }
        stream.extend_from_slice(data);
        if bracketed {
            stream.extend_from_slice(BRACKETED_PASTE_CLOSE);
        }
        let mut queued = 0usize;
        for chunk in stream.chunks(MAX_CHUNK_BYTES) {
            match self.try_enqueue(input_id, chunk) {
                Ok(()) => queued += chunk.len(),
                Err(InputQueueError::QueueFull { .. }) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(queued)
    }

    /// Blocking paste feed: waits for queue space per chunk until everything
    /// is queued or the queue shuts down.
    pub fn feed_paste_blocking(
        &self,
        input_id: &str,
        data: &[u8],
        bracketed: bool,
    ) -> Result<usize, InputQueueError> {
        let overhead = if bracketed {
            BRACKETED_PASTE_OPEN.len() + BRACKETED_PASTE_CLOSE.len()
        } else {
            0
        };
        let total = data.len() + overhead;
        if total > MAX_PASTE_BYTES {
            return Err(InputQueueError::PasteTooLarge {
                len: total,
                max: MAX_PASTE_BYTES,
            });
        }
        let mut stream = Vec::with_capacity(total);
        if bracketed {
            stream.extend_from_slice(BRACKETED_PASTE_OPEN);
        }
        stream.extend_from_slice(data);
        if bracketed {
            stream.extend_from_slice(BRACKETED_PASTE_CLOSE);
        }
        let mut queued = 0usize;
        for chunk in stream.chunks(MAX_CHUNK_BYTES) {
            match self.enqueue_blocking(input_id, chunk) {
                Ok(()) => queued += chunk.len(),
                Err(e) => return Err(e),
            }
        }
        Ok(queued)
    }

    /// Pull the next chunk without waiting (writer/test path).
    pub fn pop(&self) -> Option<InputChunk> {
        let mut state = self.lock();
        let chunk = state.chunks.pop_front()?;
        state.total_bytes -= chunk.bytes.len();
        drop(state);
        self.space.notify_all();
        Some(chunk)
    }

    /// Pull the next chunk, sleeping until one arrives or the queue shuts
    /// down. The single writer thread is the only caller, which keeps the
    /// "one outstanding write at a time" invariant.
    pub fn wait_pop(&self) -> Option<InputChunk> {
        let mut state = self.lock();
        loop {
            if let Some(chunk) = state.chunks.pop_front() {
                state.total_bytes -= chunk.bytes.len();
                drop(state);
                self.space.notify_all();
                return Some(chunk);
            }
            if self.shutdown.load(Ordering::Acquire) {
                return None;
            }
            state = self.items.wait(state).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// Wake all waiters and refuse further input (session finalize path).
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        // Notify through the mutex so condvar waiters in both pools wake.
        let _guard = self.lock();
        self.items.notify_all();
        self.space.notify_all();
        drop(_guard);
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// Bytes currently waiting in the queue.
    pub fn total_bytes(&self) -> usize {
        self.lock().total_bytes
    }
}

impl Default for InputQueue {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Dedup ring

/// Outcome of one `input_id` write. `Unknown` is recorded when the write
/// errored part-way (or the connection died mid-write): the bridge never
/// re-sends automatically (spec §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOutcome {
    /// `begin` was called, the writer has not reported yet.
    InFlight,
    /// Fully written; carries the accepted byte count.
    Accepted(u32),
    /// Write failed part-way or the transport died: outcome unknown.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupBegin {
    /// First sight of this id; the caller proceeds.
    New,
    /// Duplicate id: replay the remembered outcome, do not write again.
    Replay(InputOutcome),
}

/// Ring of the last [`INPUT_DEDUP_ENTRIES`] `input_id -> outcome` pairs.
/// Entries still `InFlight` when the ring wraps are evicted like any other
/// (their completion is then dropped — acceptable, the ring is a bounded
/// replay cache, not a durability log).
#[derive(Debug, Default)]
pub struct InputDedup {
    map: HashMap<String, InputOutcome>,
    order: VecDeque<String>,
}

impl InputDedup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a write attempt. Duplicate ids return the remembered outcome.
    pub fn begin(&mut self, input_id: &str) -> DedupBegin {
        if let Some(outcome) = self.map.get(input_id) {
            return DedupBegin::Replay(*outcome);
        }
        self.map
            .insert(input_id.to_string(), InputOutcome::InFlight);
        self.order.push_back(input_id.to_string());
        while self.order.len() > INPUT_DEDUP_ENTRIES {
            if let Some(evicted) = self.order.pop_front() {
                self.map.remove(&evicted);
            }
        }
        DedupBegin::New
    }

    /// Record a completed write (last report wins, so multi-chunk pastes
    /// under one id record the final chunk's result).
    ///
    /// Update-only: an id the ring already evicted (or never saw) is NOT
    /// re-inserted. Inserting here would bypass `order` and the entry could
    /// never be evicted again — a slow leak of one map entry per write on a
    /// busy session.
    pub fn complete(&mut self, input_id: &str, accepted_bytes: u32) {
        if let Some(outcome) = self.map.get_mut(input_id) {
            *outcome = InputOutcome::Accepted(accepted_bytes);
        }
    }

    /// Record `INPUT_OUTCOME_UNKNOWN` (write error mid-way). Update-only for
    /// the same reason as [`InputDedup::complete`].
    pub fn unknown(&mut self, input_id: &str) {
        if let Some(outcome) = self.map.get_mut(input_id) {
            *outcome = InputOutcome::Unknown;
        }
    }

    /// Forget an id (used when enqueue failed and the write never reached
    /// the writer: a retry should be treated as fresh).
    pub fn abort(&mut self, input_id: &str) {
        self.map.remove(input_id);
        self.order.retain(|id| id != input_id);
    }

    pub fn get(&self, input_id: &str) -> Option<InputOutcome> {
        self.map.get(input_id).copied()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Resize coalescer

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeSubmit {
    /// Stored; eligible now or at the next coalescing boundary.
    Queued,
    /// Identical to the pending/applied size; nothing to do.
    Skipped,
}

struct PendingResize {
    cols: u16,
    rows: u16,
    /// First submit uses the later of now and the previous apply + window.
    /// Subsequent submissions replace dimensions without extending it, so
    /// a continuous burst cannot postpone application indefinitely.
    fire_at_ms: u64,
}

/// Applies the first resize after idle immediately. Subsequent requests
/// within 16 ms of the last application are coalesced (last wins), without
/// extending the deadline. Identical dimensions are skipped. The actor
/// loop is the only owner.
pub struct ResizeCoalescer {
    window_ms: u64,
    clock: std::sync::Arc<dyn Clock>,
    pending: Option<PendingResize>,
    last_applied: Option<(u16, u16)>,
    next_apply_ms: u64,
}

impl ResizeCoalescer {
    pub fn new(clock: std::sync::Arc<dyn Clock>) -> Self {
        Self {
            window_ms: RESIZE_COALESCE_MS,
            clock,
            pending: None,
            last_applied: None,
            next_apply_ms: 0,
        }
    }

    pub fn with_window(mut self, window: Duration) -> Self {
        self.window_ms = window.as_millis() as u64;
        self
    }

    /// Register a resize request. Dimensions MUST already be validated
    /// (2..=1000) by the caller — asserted here, never silently fixed.
    pub fn submit(&mut self, cols: u16, rows: u16) -> ResizeSubmit {
        assert!(
            (TERMINAL_MIN_DIM..=TERMINAL_MAX_DIM).contains(&cols)
                && (TERMINAL_MIN_DIM..=TERMINAL_MAX_DIM).contains(&rows),
            "ResizeCoalescer received unvalidated dimensions {cols}x{rows}"
        );
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.cols == cols && p.rows == rows)
        {
            return ResizeSubmit::Skipped;
        }
        if self.pending.is_none() && self.last_applied == Some((cols, rows)) {
            return ResizeSubmit::Skipped;
        }
        match self.pending.take() {
            // Last-wins inside the window; keep the original fire deadline.
            Some(old) => {
                self.pending = Some(PendingResize {
                    cols,
                    rows,
                    fire_at_ms: old.fire_at_ms,
                });
            }
            None => {
                self.pending = Some(PendingResize {
                    cols,
                    rows,
                    fire_at_ms: self.clock.now_ms().max(self.next_apply_ms),
                });
            }
        }
        ResizeSubmit::Queued
    }

    /// Take the pending resize once the window elapsed. `None` otherwise.
    pub fn poll(&mut self) -> Option<(u16, u16)> {
        let pending = self.pending.as_ref()?;
        if self.clock.now_ms() < pending.fire_at_ms {
            return None;
        }
        let pending = self.pending.take().expect("checked above");
        self.last_applied = Some((pending.cols, pending.rows));
        self.next_apply_ms = self.clock.now_ms().saturating_add(self.window_ms);
        Some((pending.cols, pending.rows))
    }

    /// Currently pending dimensions, if any.
    pub fn pending_dims(&self) -> Option<(u16, u16)> {
        self.pending.as_ref().map(|p| (p.cols, p.rows))
    }

    /// Last applied dimensions (None before the first fire).
    pub fn last_applied(&self) -> Option<(u16, u16)> {
        self.last_applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // -- queue -------------------------------------------------------------

    #[test]
    fn enqueue_rejects_chunks_over_4kib() {
        let q = InputQueue::new();
        assert!(q.try_enqueue("i1", &[0u8; 4096]).is_ok());
        let err = q.try_enqueue("i2", &[0u8; 4097]).unwrap_err();
        assert_eq!(
            err,
            InputQueueError::ChunkTooLarge {
                len: 4097,
                max: 4096
            }
        );
    }

    #[test]
    fn queue_fills_at_64kib_and_reports_busy_style_error() {
        let q = InputQueue::new();
        for i in 0..16 {
            q.try_enqueue(&format!("i{i}"), &[7u8; 4096]).expect("fits");
        }
        assert_eq!(q.total_bytes(), 65_536);
        let err = q.try_enqueue("overflow", b"x").unwrap_err();
        assert_eq!(
            err,
            InputQueueError::QueueFull {
                used: 65_536,
                max: 65_536
            }
        );
        // Popping one chunk opens space again.
        q.pop();
        assert!(q.try_enqueue("after-drain", b"x").is_ok());
    }

    #[test]
    fn enqueue_blocking_waits_until_space_opens() {
        let q = Arc::new(InputQueue::new());
        for i in 0..16 {
            q.try_enqueue(&format!("i{i}"), &[1u8; 4096]).unwrap();
        }
        let producer = Arc::clone(&q);
        let blocked =
            std::thread::spawn(move || producer.enqueue_blocking("blocked", &[2u8; 4096]).is_ok());
        std::thread::sleep(Duration::from_millis(100));
        assert!(!blocked.is_finished(), "must still be waiting for space");
        q.pop(); // open 4 KiB
        assert!(blocked.join().expect("thread"));
    }

    #[test]
    fn paste_over_1mib_is_rejected_with_file_transfer_guidance() {
        let q = InputQueue::new();
        let big = vec![b'p'; 2 * 1024 * 1024];
        let err = q.feed_paste("p1", &big, false).unwrap_err();
        assert!(matches!(err, InputQueueError::PasteTooLarge { .. }));
        assert!(err.to_string().contains("use file transfer"));
        assert_eq!(q.total_bytes(), 0, "nothing queued for oversized paste");
    }

    #[test]
    fn paste_splits_into_4kib_chunks_as_space_allows() {
        let q = InputQueue::new();
        let data = vec![b'z'; 10 * 1024]; // 10 KiB -> 3 chunks
        let queued = q.feed_paste("p1", &data, false).expect("under limit");
        assert_eq!(queued, 10 * 1024);
        assert_eq!(q.total_bytes(), 10 * 1024);
        let mut chunks = Vec::new();
        while let Some(c) = q.pop() {
            chunks.push(c);
        }
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].bytes.len(), 4096);
        assert_eq!(chunks[2].bytes.len(), 2 * 1024);
        assert!(chunks.iter().all(|c| c.input_id == "p1"));
    }

    #[test]
    fn paste_stops_when_queue_fills_nonblocking() {
        let q = InputQueue::new();
        // Pre-fill with 15 chunks so exactly one 4 KiB chunk of space is left.
        for i in 0..15 {
            q.try_enqueue(&format!("fill-{i}"), &[3u8; 4096]).unwrap();
        }
        let data = vec![b'q'; 8 * 1024]; // would need 2 chunks
        let queued = q.feed_paste("p1", &data, false).expect("under limit");
        assert_eq!(queued, 4096, "only one chunk fit");
    }

    #[test]
    fn bracketed_paste_wraps_markers_and_counts_them_toward_the_limit() {
        let q = InputQueue::new();
        let data = vec![b'a'; 5000]; // 5012 bytes with markers -> 2 chunks
        let queued = q.feed_paste("p1", &data, true).expect("under limit");
        assert_eq!(
            queued,
            5000 + BRACKETED_PASTE_OPEN.len() + BRACKETED_PASTE_CLOSE.len()
        );
        let first = q.pop().expect("head chunk");
        assert!(first.bytes.starts_with(BRACKETED_PASTE_OPEN));
        let last = {
            let mut last = None;
            while let Some(c) = q.pop() {
                last = Some(c);
            }
            last.expect("tail chunk")
        };
        assert!(last.bytes.ends_with(BRACKETED_PASTE_CLOSE));
    }

    #[test]
    fn shutdown_wakes_wait_pop_and_refuses_input() {
        let q = Arc::new(InputQueue::new());
        let sleeper = Arc::clone(&q);
        let task = std::thread::spawn(move || sleeper.wait_pop());
        std::thread::sleep(Duration::from_millis(50));
        q.shutdown();
        assert!(task.join().expect("thread").is_none());
        assert_eq!(
            q.try_enqueue("late", b"x"),
            Err(InputQueueError::ShuttingDown)
        );
    }

    // -- dedup -------------------------------------------------------------

    #[test]
    fn dedup_replays_remembered_outcome() {
        let mut d = InputDedup::new();
        assert_eq!(d.begin("a"), DedupBegin::New);
        d.complete("a", 128);
        assert_eq!(
            d.begin("a"),
            DedupBegin::Replay(InputOutcome::Accepted(128))
        );
        // unknown outcome is replayed too
        assert_eq!(d.begin("b"), DedupBegin::New);
        d.unknown("b");
        assert_eq!(d.begin("b"), DedupBegin::Replay(InputOutcome::Unknown));
        // in-flight duplicate reports InFlight
        assert_eq!(d.begin("c"), DedupBegin::New);
        assert_eq!(d.begin("c"), DedupBegin::Replay(InputOutcome::InFlight));
    }

    #[test]
    fn dedup_ring_evicts_oldest_beyond_256() {
        let mut d = InputDedup::new();
        for i in 0..INPUT_DEDUP_ENTRIES {
            d.begin(&format!("id-{i}"));
        }
        assert_eq!(d.len(), INPUT_DEDUP_ENTRIES);
        assert_eq!(d.begin("id-0"), DedupBegin::Replay(InputOutcome::InFlight));
        d.begin("id-new"); // evicts id-0 (the oldest)
        assert_eq!(d.len(), INPUT_DEDUP_ENTRIES);
        assert_eq!(
            d.begin("id-1"),
            DedupBegin::Replay(InputOutcome::InFlight),
            "id-1 still remembered"
        );
        assert_eq!(d.begin("id-0"), DedupBegin::New, "oldest was evicted");
    }

    /// L8: `complete`/`unknown`은 갱신 전용이다 — `begin` 없이 들어온 id를
    /// 새로 넣으면 `order` 링을 우회해 영영 축출되지 않는 항목이 된다.
    #[test]
    fn dedup_complete_and_unknown_never_insert_new_entries() {
        let mut d = InputDedup::new();
        d.complete("ghost", 7);
        d.unknown("ghost-2");
        assert_eq!(d.len(), 0, "링에 들어가지 않은 id는 기록하지 않는다");
        assert_eq!(d.get("ghost"), None);
        assert_eq!(d.get("ghost-2"), None);
        assert_eq!(d.begin("ghost"), DedupBegin::New, "여전히 처음 보는 id다");

        // 링이 가득 차 축출된 id의 뒤늦은 완료도 되살리지 않는다.
        let mut d = InputDedup::new();
        d.begin("first");
        for i in 0..INPUT_DEDUP_ENTRIES {
            d.begin(&format!("id-{i}"));
        }
        assert_eq!(d.get("first"), None, "축출되었다");
        d.complete("first", 42);
        assert_eq!(d.get("first"), None, "되살아나지 않는다");
        assert_eq!(d.len(), INPUT_DEDUP_ENTRIES, "링 상한을 넘지 않는다");
        assert_eq!(d.map.len(), INPUT_DEDUP_ENTRIES, "맵도 링과 같은 크기다");
    }

    #[test]
    fn dedup_abort_makes_retry_fresh() {
        let mut d = InputDedup::new();
        d.begin("x");
        d.abort("x");
        assert_eq!(d.len(), 0);
        assert_eq!(d.begin("x"), DedupBegin::New);
    }

    // -- resize coalescer ---------------------------------------------------

    #[test]
    fn resize_after_idle_is_immediate_and_drag_keeps_only_the_latest() {
        let clock = Arc::new(ManualClock::new(0));
        let mut c = ResizeCoalescer::new(clock.clone());
        c.submit(80, 24);
        c.submit(100, 30);
        assert_eq!(c.poll(), Some((100, 30)), "first actor turn needs no timer");
        clock.advance(1);
        c.submit(110, 35);
        clock.advance(14);
        c.submit(120, 40);
        assert_eq!(c.poll(), None, "drag stays bounded to one apply per window");
        clock.advance(1);
        assert_eq!(
            c.poll(),
            Some((120, 40)),
            "later requests do not extend the deadline"
        );
        clock.advance(100);
        c.submit(140, 50);
        assert_eq!(c.poll(), Some((140, 50)), "idle resize is immediate again");
    }

    #[test]
    fn resize_identical_dims_are_skipped() {
        let clock = Arc::new(ManualClock::new(0));
        let mut c = ResizeCoalescer::new(Arc::clone(&clock) as Arc<dyn Clock>);
        c.submit(80, 24);
        clock.advance(16);
        assert_eq!(c.poll(), Some((80, 24)));
        // Same as applied -> skipped entirely.
        assert_eq!(c.submit(80, 24), ResizeSubmit::Skipped);
        assert_eq!(c.poll(), None);
        // Pending duplicate -> skipped.
        assert_eq!(c.submit(90, 30), ResizeSubmit::Queued);
        assert_eq!(c.submit(90, 30), ResizeSubmit::Skipped);
    }

    #[test]
    #[should_panic(expected = "unvalidated dimensions")]
    fn resize_rejects_unvalidated_dimensions() {
        let mut c = ResizeCoalescer::new(Arc::new(ManualClock::new(0)));
        c.submit(0, 0);
    }

    #[test]
    fn manual_and_system_clocks_advance() {
        let manual = ManualClock::new(100);
        assert_eq!(manual.now_ms(), 100);
        manual.advance(16);
        assert_eq!(manual.now_ms(), 116);
        let sys = SystemClock;
        let a = sys.now_ms();
        std::thread::sleep(Duration::from_millis(5));
        assert!(sys.now_ms() >= a);
    }
}
