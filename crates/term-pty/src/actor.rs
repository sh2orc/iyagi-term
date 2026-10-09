//! SessionActor: per-session state serializer owning one PTY
//! (spec `02-runner.md` §2, §6, §7).
//!
//! Thread model (sync threads, no async runtime in the hot path):
//! * one READER thread: reads up to 16 KiB from the pty master into a
//!   bounded channel (~4 chunks) toward the actor loop — backpressure is
//!   applied by the OS pipe plus the channel cap, never by RAM growth;
//! * one WRITER thread: the only writer, pulls one ≤ 4 KiB chunk at a time
//!   from the [`InputQueue`] (one outstanding write), records dedup
//!   outcomes (partial-write errors => `INPUT_OUTCOME_UNKNOWN`);
//! * one ACTOR loop thread: owns journal appends, resize coalescing, epoch,
//!   exit polling and lifecycle transitions. The control mailbox is bounded
//!   (128); when full, ordinary requests fail with `Busy` while cancel
//!   travels over an independent [`AtomicBool`] that works even then.
//!
//! Output order contract: every output/resize passes the journal FIRST, then
//! the sink (`can_send`); when the sink is blocked the journal keeps moving
//! and only the emit cursor lags — bytes are never dropped. The in-memory
//! pending buffer is capped at `OUTPUT_HIGH_BYTES`; past that the actor stops
//! draining the reader channel (the reader thread blocks on its bounded
//! channel and the pty itself applies backpressure). I07 replaces this
//! buffer with journal replay.
//!
//! journal/flow (ticket I07) are wired through the local traits below; when
//! their real types land, adapters replace them without touching this loop.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use term_contracts::ids::SessionId;
use term_contracts::session::limits::OUTPUT_HIGH_BYTES;

use crate::input::{
    Clock, DedupBegin, InputDedup, InputOutcome, InputQueue, InputQueueError, ResizeCoalescer,
    SystemClock,
};
use crate::pty::{validate_size, PtyHandle};

/// Control mailbox capacity (spec §2: 128 control slots per actor).
pub const CONTROL_MAILBOX: usize = 128;
/// Reader→actor channel capacity, counted in ≤16 KiB chunks.
pub const READER_CHANNEL_CHUNKS: usize = 4;
/// Drain timeout after root exit + EOF while owned descendants are still
/// alive (spec §7; injectable via config for tests).
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
/// After the finalize SIGHUP, how long the direct child may take to exit
/// before it is SIGKILLed (Unix shell sessions; groups escalate separately).
#[cfg(unix)]
const DIRECT_CHILD_KILL_GRACE: Duration = Duration::from_millis(1_500);

/// HUP → grace → KILL on the pty's direct child. The child is our own,
/// unreaped until `poll_exit` reports it, so its pid cannot have been
/// recycled by the time the kill is sent (ownership is proven by the
/// parent relationship, never by pid alone).
#[cfg(unix)]
fn escalate_direct_child(pty: &PtyHandle) {
    let deadline = std::time::Instant::now() + DIRECT_CHILD_KILL_GRACE;
    loop {
        match pty.poll_exit() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {}
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let Some(pid) = pty.pid().filter(|p| *p > 0) else {
        return;
    };
    tracing::debug!(pid, "direct child ignored SIGHUP; escalating to SIGKILL");
    // SAFETY: plain kill(2) on our own unreaped child.
    let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while std::time::Instant::now() < deadline {
        if !matches!(pty.poll_exit(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Default child exit poll interval (spec §2: periodic try_wait, no
/// permanent per-session wait thread).
pub const DEFAULT_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Actor idle loop step.
const LOOP_STEP: Duration = Duration::from_millis(2);
/// Output quiescence window used to approximate "pty drained": ConPTY never
/// delivers EOF while the master is open, so after the root exits the actor
/// waits for this much silence (bounded by `drain_timeout` regardless).
const DRAIN_QUIESCENCE_MS: u64 = 250;
/// Degraded-journal retry bounds (transient I/O or disk-full): first retry
/// after 100 ms, doubling up to 10 s while the failure persists.
const JOURNAL_RETRY_MIN_MS: u64 = 100;
const JOURNAL_RETRY_MAX_MS: u64 = 10_000;
/// Unsavable-output stash bound while the journal is degraded. The newest
/// records survive (recovery replays them in order); the oldest are dropped
/// and counted, and the status message reports the loss.
const JOURNAL_STASH_LIMIT_BYTES: usize = 1024 * 1024;

/// A journal record waiting for a degraded journal to recover. Keeps output
/// and resize records in one order so the flush cannot reorder the journal.
enum StashItem {
    Output(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

fn stash_item_len(item: &StashItem) -> usize {
    match item {
        StashItem::Output(bytes) => bytes.len(),
        StashItem::Resize { .. } => 4,
    }
}

/// Bounded, drop-oldest push. The stash holds the newest records; dropped
/// bytes are lost from the journal (seq stays contiguous — seqs are assigned
/// only by successful appends).
fn journal_stash_push(
    stash: &mut VecDeque<StashItem>,
    stash_bytes: &mut usize,
    dropped_bytes: &mut u64,
    item: StashItem,
) {
    let mut len = stash_item_len(&item);
    *stash_bytes += len;
    stash.push_back(item);
    while *stash_bytes > JOURNAL_STASH_LIMIT_BYTES {
        match stash.pop_front() {
            Some(front) => {
                len = stash_item_len(&front);
                *stash_bytes -= len;
                *dropped_bytes += len as u64;
            }
            None => break,
        }
    }
}

/// Cap and budget failures are deterministic for this session — retrying
/// would spin forever. Everything else on the write path (I/O, disk full)
/// can clear on its own. The [`JournalSink`] trait carries `io::Error`, so
/// the classification downcasts back to the wrapped flow error; a bare
/// `io::Error` (test doubles) counts as transient.
fn journal_error_is_fatal(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<crate::journal::JournalFlowError>())
        .is_some_and(|flow| {
            matches!(
                flow,
                crate::journal::JournalFlowError::SessionCap { .. }
                    | crate::journal::JournalFlowError::GlobalCap { .. }
                    | crate::journal::JournalFlowError::PayloadTooLarge { .. }
            )
        })
}

// ---------------------------------------------------------------------------
// Local wiring traits (I07 adapters replace these)

/// Journal sink. Seq numbers start at 1 and cover outputs AND resizes in one
/// order (spec §5).
pub trait JournalSink: Send {
    /// Append raw output bytes (≤ 16 KiB per call); returns the record seq.
    fn append_output(&mut self, data: &[u8]) -> std::io::Result<u64>;
    /// Append a resize record; returns the record seq.
    fn append_resize(&mut self, cols: u16, rows: u16) -> std::io::Result<u64>;
}

/// In-memory journal used by tests and as the pre-I07 default. The first
/// record of a session is the initial size (spec §5).
#[derive(Debug, Default)]
pub struct MemJournal {
    next_seq: u64,
    /// Append-only record list (seq, kind).
    pub records: Vec<MemRecord>,
}

/// One [`MemJournal`] record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemRecord {
    pub seq: u64,
    pub kind: MemRecordKind,
}

/// Record kinds stored by [`MemJournal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemRecordKind {
    Output(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

impl MemJournal {
    pub fn new() -> Self {
        Self::default()
    }
}

impl JournalSink for MemJournal {
    fn append_output(&mut self, data: &[u8]) -> std::io::Result<u64> {
        self.next_seq += 1;
        let seq = self.next_seq;
        self.records.push(MemRecord {
            seq,
            kind: MemRecordKind::Output(data.to_vec()),
        });
        Ok(seq)
    }

    fn append_resize(&mut self, cols: u16, rows: u16) -> std::io::Result<u64> {
        self.next_seq += 1;
        let seq = self.next_seq;
        self.records.push(MemRecord {
            seq,
            kind: MemRecordKind::Resize { cols, rows },
        });
        Ok(seq)
    }
}

/// One ordered event toward a consumer (sink). Carries the same order the
/// journal saw, including the epoch that was current when it was emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorEvent {
    pub seq: u64,
    pub epoch: String,
    pub kind: ActorEventKind,
}

/// Event payload of [`ActorEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorEventKind {
    Output(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

/// Live output consumer (I07: the flow/credit ledger). `can_send` gating
/// keeps the emit cursor behind journal progress when a view is slow.
pub trait OutputSink: Send {
    fn can_send(&self) -> bool;
    fn emit(&mut self, event: ActorEvent);
}

/// Sink that accepts everything (pre-UI default; never blocks the journal).
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscardSink;

impl OutputSink for DiscardSink {
    fn can_send(&self) -> bool {
        true
    }
    fn emit(&mut self, _event: ActorEvent) {}
}

/// Root+descendants aliveness, polled by the actor loop (I06's platform
/// group provides the real implementation; the default sees no owned
/// descendants).
pub trait DescendantWatch: Send + Sync {
    fn owned_alive(&self) -> usize {
        0
    }

    /// Fallible variant: `None` when the platform could not answer (a
    /// transient process-table error). The actor keeps its last known
    /// count then — "unknown" is never read as "no descendants" (spec
    /// 03-resources: unknown measurements are reported, not zeroed).
    fn try_owned_alive(&self) -> Option<usize> {
        Some(self.owned_alive())
    }
}

/// No-descendants default [`DescendantWatch`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NoDescendants;

impl DescendantWatch for NoDescendants {}

/// Teardown information handed to [`SessionCleanup::teardown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeardownInfo {
    pub session_id: SessionId,
    pub cancelled: bool,
    pub exit_code: Option<i32>,
    pub descendants_remaining: bool,
}

/// Teardown hook invoked exactly once per session, right before PTY teardown
/// (cancel-during-starting included). The daemon wires platform group
/// teardown here (spec §7: recheck identity, use the owned OS group first).
pub trait SessionCleanup: Send + Sync {
    fn teardown(&self, info: &TeardownInfo);
}

/// No-op default [`SessionCleanup`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopCleanup;

impl SessionCleanup for NoopCleanup {
    fn teardown(&self, _info: &TeardownInfo) {}
}

// ---------------------------------------------------------------------------
// Public state

/// Lifecycle of one session actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLifecycle {
    Starting,
    Running,
    Draining,
    Finalized,
}

/// Consistent snapshot of actor-visible state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatus {
    pub session_id: SessionId,
    pub lifecycle: SessionLifecycle,
    pub epoch: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub descendants_remaining: bool,
    /// Root process exited (mid-flight signal: the workload may still be
    /// RUNNING with owned descendants alive — spec 02-runner §5).
    pub root_exited: bool,
    pub last_seq: u64,
    pub journal_error: Option<String>,
}

/// Handle-side errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActorError {
    #[error("control mailbox full ({CONTROL_MAILBOX}); retry later")]
    Busy,
    #[error("invalid resize dimensions {cols}x{rows}")]
    InvalidDimensions { cols: u16, rows: u16 },
    #[error(transparent)]
    Input(#[from] InputQueueError),
    #[error("session is shutting down")]
    ShuttingDown,
    /// The foreground program has not read its tty input for `blocked_ms`
    /// (queue full in raw mode: hung, busy or stopped). New input is refused
    /// instead of piling up behind the stalled write (it would otherwise be
    /// acknowledged as queued and land, all at once, much later).
    #[error("terminal input stalled for {blocked_ms} ms: the program is not reading input")]
    InputStalled { blocked_ms: u64 },
}

/// A write stalled this long (tty input queue full, no progress) makes the
/// handle refuse new input with [`ActorError::InputStalled`].
pub const INPUT_STALL_REJECT: Duration = Duration::from_millis(2_000);

/// Result of an input write request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputReply {
    /// Queued for the writer thread.
    Queued { bytes: u32 },
    /// The PTY write **completed** before the reply (W2: ADR-5 해소) —
    /// `bytes`는 실제 기록 바이트 수.
    Written { bytes: u32 },
    /// Duplicate input_id: nothing new written, remembered outcome returned.
    Replayed { outcome: InputOutcome },
}

/// input_id별 쓰기 완료 대기자(W2: `session.input` 완료 응답).
/// writer 스레드가 결과를 알리는 순간 fan-out한다. 대기자가 이미 떠난
/// send 실패는 조용히 무시된다(수신기 drop).
pub struct WriteCompletions {
    waiters: Mutex<std::collections::HashMap<String, Vec<std::sync::mpsc::Sender<InputOutcome>>>>,
}

impl WriteCompletions {
    pub fn new() -> Self {
        Self {
            waiters: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// 대기자 등록 — enqueue **전에** 호출해야 완료를 놓치지 않는다.
    fn register(&self, input_id: &str) -> std::sync::mpsc::Receiver<InputOutcome> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.waiters
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(input_id.to_string())
            .or_default()
            .push(tx);
        rx
    }

    /// 대기자 해제 — enqueue가 실패했거나 대기가 타임아웃해 완료 통지가
    /// 영영 오지 않는 경로에서 호출한다. 이걸 빼면 거부된 입력마다 맵에
    /// 항목이 하나씩 쌓여 세션 수명 내내 남는다(입력 큐가 가득 찼거나
    /// 큐가 shutdown된 뒤의 입력이 전부 여기에 해당한다).
    ///
    /// 한 input_id의 동시 대기자는 최대 하나다 — `InputDedup::begin`이
    /// 중복 id를 `Replay`로 돌려보내므로 두 번째 등록 자체가 없다. 따라서
    /// 항목 전체를 지워도 남의 대기자를 잃지 않는다.
    fn unregister(&self, input_id: &str) {
        self.waiters
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(input_id);
    }

    /// writer가 완료를 알린다.
    fn complete(&self, input_id: &str, outcome: InputOutcome) {
        let senders = self
            .waiters
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(input_id);
        if let Some(senders) = senders {
            for tx in senders {
                let _ = tx.send(outcome);
            }
        }
    }
}

impl Default for WriteCompletions {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Control plane (internal)

enum Control {
    Resize {
        cols: u16,
        rows: u16,
    },
    SetOwner {
        reply: std::sync::mpsc::Sender<String>,
    },
}

struct Shared {
    session_id: SessionId,
    control_tx: SyncSender<Control>,
    actor_thread: OnceLock<std::thread::Thread>,
    cancel: Arc<AtomicBool>,
    status: Mutex<SessionStatus>,
    dedup: Arc<Mutex<InputDedup>>,
    /// input_id별 쓰기 완료 통지(W2: write_input_await).
    completions: Arc<WriteCompletions>,
    /// The pty's input-stall marker ([`PtyHandle::input_blocked_marker`]).
    input_blocked: Arc<std::sync::atomic::AtomicU64>,
}

impl Shared {
    fn wake_actor(&self) {
        if let Some(thread) = self.actor_thread.get() {
            thread.unpark();
        }
    }

    fn update_status(&self, mutate: impl FnOnce(&mut SessionStatus)) {
        let mut status = self.status.lock().unwrap_or_else(|p| p.into_inner());
        mutate(&mut status);
    }

    fn status_snapshot(&self) -> SessionStatus {
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

// ---------------------------------------------------------------------------
// Handle

/// Cloneable driver handle for one session (the daemon holds one per
/// session; the UI bridge only sees RPCs, never this type directly).
pub struct SessionActorHandle {
    shared: Arc<Shared>,
    input: Arc<InputQueue>,
}

impl SessionActorHandle {
    pub fn session_id(&self) -> SessionId {
        self.shared.session_id.clone()
    }

    /// Queue one input write (≤ 4 KiB; the idempotency id is deduplicated
    /// against the last 256 outcomes). Chunk-size/queue-cap violations map
    /// to `ActorError::Input`; a full queue is the input-path `BUSY`.
    pub fn write_input(&self, input_id: &str, data: &[u8]) -> Result<InputReply, ActorError> {
        let mut dedup = self.shared.dedup.lock().unwrap_or_else(|p| p.into_inner());
        // Dedup 재생이 막힘 거절보다 먼저다: INPUT_OUTCOME_UNKNOWN 뒤의 재전송이
        // Stall 거절로 뒤바뀌면 클라이언트는 실제로는 성공한 쓰기를 "안 갔다"고
        // 오판한다(멱등 계약). 거절로 끝나면 dedup 자리를 돌려놓는다 — 재시도가
        // 새 입력으로 보이게(아래 enqueue 실패 경로와 같은 규율).
        if let DedupBegin::Replay(outcome) = dedup.begin(input_id) {
            return Ok(InputReply::Replayed { outcome });
        }
        if let Err(e) = self.refuse_if_stalled() {
            dedup.abort(input_id);
            return Err(e);
        }
        match self.input.try_enqueue(input_id, data) {
            Ok(()) => Ok(InputReply::Queued {
                bytes: data.len() as u32,
            }),
            Err(e) => {
                // Nothing reached the writer: forget the id so a retry is
                // treated as fresh rather than forever InFlight.
                dedup.abort(input_id);
                Err(ActorError::Input(e))
            }
        }
    }

    /// `write_input` + 완료 대기(W2: `session.input` 완료 응답 — ADR-5).
    /// enqueue **전에** 대기자를 등록해 완료 경쟁을 없앤다. `timeout` 안에
    /// PTY 쓰기가 끝나면 `Written`(실제 기록 바이트), 못 끝나면 `Queued`
    /// (정직한 종전 응답), 쓰기 실패는 `Queued`로 표면화된다(입력 경로는
    /// 재전송하지 않는다 — §6).
    pub fn write_input_await(
        &self,
        input_id: &str,
        data: &[u8],
        timeout: std::time::Duration,
    ) -> Result<InputReply, ActorError> {
        let mut dedup = self.shared.dedup.lock().unwrap_or_else(|p| p.into_inner());
        // 재생이 막힘 거절보다 먼저다(`write_input`과 같은 이유). 거절로 끝나면
        // dedup 자리를 돌려놓는다.
        if let DedupBegin::Replay(outcome) = dedup.begin(input_id) {
            return Ok(InputReply::Replayed { outcome });
        }
        if let Err(e) = self.refuse_if_stalled() {
            dedup.abort(input_id);
            return Err(e);
        }
        // 등록 → enqueue 순서가 경쟁을 없앤다: writer가 그새 끝내도
        // channel에 이미 쌓여 있어 recv로 받는다.
        let rx = self.shared.completions.register(input_id);
        match self.input.try_enqueue(input_id, data) {
            Ok(()) => {}
            Err(e) => {
                // 아무것도 writer에 닿지 않았다: 완료 통지가 올 리 없으니
                // 대기자도 같이 거둔다(그러지 않으면 거부 한 번당 항목 하나).
                self.shared.completions.unregister(input_id);
                dedup.abort(input_id);
                return Err(ActorError::Input(e));
            }
        }
        drop(dedup);
        match rx.recv_timeout(timeout) {
            Ok(InputOutcome::Accepted(n)) => Ok(InputReply::Written { bytes: n }),
            Ok(_) => Ok(InputReply::Queued {
                bytes: data.len() as u32,
            }),
            Err(_) => {
                // 타임아웃/송신자 소멸: 큐가 shutdown되면 남은 청크는
                // 영영 `complete()`에 닿지 않는다 — 대기자를 여기서 거둔다.
                self.shared.completions.unregister(input_id);
                Ok(InputReply::Queued {
                    bytes: data.len() as u32,
                })
            }
        }
    }

    /// Feed a paste through the bounded queue (1 MiB cap with file-transfer
    /// guidance, 4 KiB chunk split, bracketed-paste markers optional).
    /// Returns the bytes actually queued (non-blocking variant semantics).
    pub fn feed_paste(
        &self,
        input_id: &str,
        data: &[u8],
        bracketed: bool,
    ) -> Result<usize, ActorError> {
        let mut dedup = self.shared.dedup.lock().unwrap_or_else(|p| p.into_inner());
        // 재생이 막힘 거절보다 먼저다(`write_input`과 같은 이유).
        if let DedupBegin::Replay(outcome) = dedup.begin(input_id) {
            return match outcome {
                InputOutcome::Accepted(bytes) => Ok(bytes as usize),
                _ => Ok(0),
            };
        }
        if let Err(e) = self.refuse_if_stalled() {
            dedup.abort(input_id);
            return Err(e);
        }
        match self.input.feed_paste(input_id, data, bracketed) {
            Ok(queued) => Ok(queued),
            Err(e) => {
                dedup.abort(input_id);
                Err(ActorError::Input(e))
            }
        }
    }

    /// How long the in-progress input write has been stalled on a full tty
    /// input queue (`None` while input flows). Diagnostics and the input
    /// path's fast refusal read this.
    pub fn input_blocked_for(&self) -> Option<Duration> {
        crate::pty::input_blocked_for(&self.shared.input_blocked)
    }

    fn refuse_if_stalled(&self) -> Result<(), ActorError> {
        match self.input_blocked_for() {
            Some(blocked) if blocked >= INPUT_STALL_REJECT => Err(ActorError::InputStalled {
                blocked_ms: blocked.as_millis() as u64,
            }),
            _ => Ok(()),
        }
    }

    /// Request a resize (validated 2..=1000 at the handle; the actor then
    /// applies after idle immediately, coalesces follow-ups within 16 ms).
    /// Ordinary control op: `Busy` when the 128 mailbox slots are taken;
    /// cancel still works in that state.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), ActorError> {
        validate_size(cols, rows).map_err(|_| ActorError::InvalidDimensions { cols, rows })?;
        self.try_send_control(Control::Resize { cols, rows })
    }

    /// Change the input owner (attach/owner change): rotates the epoch UUID
    /// inside the actor loop and returns the new epoch.
    pub fn set_owner(&self, view: &str) -> Result<String, ActorError> {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        self.try_send_control(Control::SetOwner { reply: reply_tx })?;
        let _ = view; // ownership bookkeeping beyond epoch rotation lands with I07 flow ledger
        reply_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| ActorError::ShuttingDown)
    }

    /// Cancel the session. NEVER touches the mailbox: the flag is read every
    /// loop iteration even when control is saturated (spec §2).
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Release);
        self.shared.wake_actor();
    }

    pub fn is_cancelled(&self) -> bool {
        self.shared.cancel.load(Ordering::Acquire)
    }

    /// Current epoch UUID (rotates on owner change).
    pub fn epoch(&self) -> String {
        self.shared.status_snapshot().epoch
    }

    /// Current status snapshot.
    pub fn status(&self) -> SessionStatus {
        self.shared.status_snapshot()
    }

    /// Poll until the predicate holds on the visible status (driver/test
    /// convenience). Returns the matching snapshot or `None` on timeout.
    pub fn wait_status(
        &self,
        timeout: Duration,
        pred: impl Fn(&SessionStatus) -> bool,
    ) -> Option<SessionStatus> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let status = self.status();
            if pred(&status) {
                return Some(status);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Wait until the session finalized.
    pub fn wait_finalized(&self, timeout: Duration) -> Option<SessionStatus> {
        self.wait_status(timeout, |s| s.lifecycle == SessionLifecycle::Finalized)
    }

    fn try_send_control(&self, msg: Control) -> Result<(), ActorError> {
        match self.shared.control_tx.try_send(msg) {
            Ok(()) => {
                self.shared.wake_actor();
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(ActorError::Busy),
            Err(TrySendError::Disconnected(_)) => Err(ActorError::ShuttingDown),
        }
    }
}

// ---------------------------------------------------------------------------
// Config + start

/// Everything the actor owns for one session. Defaults: MemJournal,
/// DiscardSink, NoopCleanup, NoDescendants, SystemClock, 100 ms exit poll,
/// 2 s drain timeout.
pub struct SessionActorConfig {
    pub session_id: SessionId,
    pub initial_cols: u16,
    pub initial_rows: u16,
    pub pty: Arc<PtyHandle>,
    pub journal: Box<dyn JournalSink>,
    pub sink: Box<dyn OutputSink>,
    pub cleanup: Arc<dyn SessionCleanup>,
    pub descendants: Arc<dyn DescendantWatch>,
    pub clock: Arc<dyn Clock>,
    pub exit_poll_interval: Duration,
    pub drain_timeout: Duration,
}

impl SessionActorConfig {
    pub fn new(
        session_id: SessionId,
        initial_cols: u16,
        initial_rows: u16,
        pty: Arc<PtyHandle>,
    ) -> Self {
        Self {
            session_id,
            initial_cols,
            initial_rows,
            pty,
            journal: Box::new(MemJournal::new()),
            sink: Box::new(DiscardSink),
            cleanup: Arc::new(NoopCleanup),
            descendants: Arc::new(NoDescendants),
            clock: Arc::new(SystemClock),
            exit_poll_interval: DEFAULT_EXIT_POLL_INTERVAL,
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
        }
    }
}

/// Reader→actor payload (16 KiB chunks or the terminal EOF marker).
enum LoopInput {
    Bytes(Vec<u8>),
    Eof,
}

/// Start the session: spawns the reader thread, writer thread and the actor
/// loop. Returns the driver handle plus the actor join handle; the daemon
/// drives one dedicated thread per session and `join()` yields the final
/// status.
pub fn start(
    config: SessionActorConfig,
) -> (SessionActorHandle, std::thread::JoinHandle<SessionStatus>) {
    let SessionActorConfig {
        session_id,
        initial_cols,
        initial_rows,
        pty,
        journal,
        sink,
        cleanup,
        descendants,
        clock,
        exit_poll_interval,
        drain_timeout,
    } = config;

    let (control_tx, control_rx) = std::sync::mpsc::sync_channel::<Control>(CONTROL_MAILBOX);
    let (reader_tx, reader_rx) = std::sync::mpsc::sync_channel::<LoopInput>(READER_CHANNEL_CHUNKS);
    let cancel = Arc::new(AtomicBool::new(false));
    let dedup = Arc::new(Mutex::new(InputDedup::new()));
    let completions = Arc::new(WriteCompletions::new());

    let epoch = uuid::Uuid::new_v4().to_string();
    let shared = Arc::new(Shared {
        session_id: session_id.clone(),
        control_tx,
        actor_thread: OnceLock::new(),
        cancel: Arc::clone(&cancel),
        completions: Arc::clone(&completions),
        input_blocked: pty.input_blocked_marker(),
        status: Mutex::new(SessionStatus {
            session_id: session_id.clone(),
            lifecycle: SessionLifecycle::Starting,
            epoch: epoch.clone(),
            exit_code: None,
            cancelled: false,
            descendants_remaining: false,
            root_exited: false,
            last_seq: 0,
            journal_error: None,
        }),
        dedup: Arc::clone(&dedup),
    });

    let input = Arc::new(InputQueue::new());
    let handle = SessionActorHandle {
        shared: Arc::clone(&shared),
        input: Arc::clone(&input),
    };

    // Reader thread: pty master -> bounded chunk channel.
    let reader_pty = Arc::clone(&pty);
    let reader_cancel = Arc::clone(&cancel);
    let reader_shared = Arc::clone(&shared);
    let reader_thread = std::thread::Builder::new()
        .name(format!("pty-reader-{session_id}"))
        .spawn(move || {
            let send = |event| {
                let sent = reader_tx.send(event).is_ok();
                if sent {
                    // Wake AFTER enqueueing, so a wake just before park is
                    // retained and the actor always sees the new record.
                    reader_shared.wake_actor();
                }
                sent
            };
            let mut reader = match reader_pty.reader() {
                Ok(r) => r,
                Err(_) => {
                    send(LoopInput::Eof);
                    return;
                }
            };
            let mut buf = [0u8; 16 * 1024]; // spec §2: max 16 KiB per read
            loop {
                if reader_cancel.load(Ordering::Acquire) {
                    return;
                }
                match reader.read(&mut buf) {
                    Ok(0) => {
                        send(LoopInput::Eof);
                        return;
                    }
                    Ok(n) => {
                        // Blocking send: the bounded channel plus the OS
                        // buffer provide backpressure; the actor drops its
                        // receiver on finalize, which unblocks us.
                        if !send(LoopInput::Bytes(buf[..n].to_vec())) {
                            return;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        // Master closed or terminal destroyed: treat as EOF.
                        send(LoopInput::Eof);
                        return;
                    }
                }
            }
        })
        .expect("spawn pty reader thread");

    // Writer thread: input queue -> pty (single outstanding write).
    let writer_pty = Arc::clone(&pty);
    let writer_dedup = Arc::clone(&dedup);
    let writer_input = Arc::clone(&input);
    let writer_completions = Arc::clone(&completions);
    let writer_thread = std::thread::Builder::new()
        .name(format!("pty-writer-{session_id}"))
        .spawn(move || {
            loop {
                let Some(chunk) = writer_input.wait_pop() else {
                    return;
                };
                // One outstanding write at a time: this thread is the only
                // producer of pty writes, and it writes one whole chunk.
                let outcome = match writer_pty.write_input(&chunk.bytes) {
                    Ok(n) => InputOutcome::Accepted(n as u32),
                    // Write failed after (at most partial) bytes went out:
                    // outcome unknown, bridge never re-sends automatically.
                    Err(_) => InputOutcome::Unknown,
                };
                let mut dedup = writer_dedup.lock().unwrap_or_else(|p| p.into_inner());
                match outcome {
                    InputOutcome::Accepted(n) => dedup.complete(&chunk.input_id, n),
                    InputOutcome::Unknown => dedup.unknown(&chunk.input_id),
                    InputOutcome::InFlight => unreachable!("writer maps to final outcomes"),
                }
                // 쓰기 완료 대기자에게 fan-out(W2: write_input_await).
                writer_completions.complete(&chunk.input_id, outcome);
            }
        })
        .expect("spawn pty writer thread");

    // Actor loop thread.
    let loop_shared = Arc::clone(&shared);
    let actor_thread = std::thread::Builder::new()
        .name(format!("pty-actor-{session_id}"))
        .spawn(move || {
            let _ = loop_shared.actor_thread.set(std::thread::current());
            let mut journal = journal;
            let mut sink = sink;
            let mut coalescer = ResizeCoalescer::new(Arc::clone(&clock));
            let mut current_epoch = epoch;
            let mut pending: VecDeque<ActorEvent> = VecDeque::new();
            let mut pending_bytes = 0usize;
            let mut reader_eof = false;
            let mut root_exited = false;
            let mut exit_code: Option<i32> = None;
            let mut drain_started_ms: Option<u64> = None;
            let mut next_exit_poll_ms = 0u64;
            let mut cancelled_at_exit = false;
            let mut journal_err: Option<String> = None;
            let mut journal_fatal = false;
            let mut journal_retry_at_ms: u64 = 0;
            let mut journal_backoff_ms: u64 = JOURNAL_RETRY_MIN_MS;
            let mut journal_stash: VecDeque<StashItem> = VecDeque::new();
            let mut journal_stash_bytes: usize = 0;
            let mut journal_dropped_bytes: u64 = 0;
            let mut announced_running = false;
            let reader_rx = reader_rx;
            let mut last_byte_ms = clock.now_ms();
            let mut reader_channel_empty = true;
            let mut idle_rounds: u32 = 0;
            // Owned-descendant count, refreshed on the exit-poll cadence
            // while draining (a full process-table walk on macOS — polling
            // it every loop step burned a core per draining session).
            // `None` until the platform answered once.
            let mut owned_alive_known: Option<usize> = None;
            let mut next_owned_poll_ms = 0u64;
            let mut warned_unknown_group = false;

            // Spec §5: the first journal record of a session is the initial
            // size, before any output.
            match journal.append_resize(initial_cols, initial_rows) {
                Ok(seq) => {
                    loop_shared.update_status(|s| s.last_seq = seq);
                    pending.push_back(ActorEvent {
                        seq,
                        epoch: current_epoch.clone(),
                        kind: ActorEventKind::Resize {
                            cols: initial_cols,
                            rows: initial_rows,
                        },
                    });
                }
                Err(e) => {
                    // Same degradation rules as the read loop: caps stop
                    // intake for good; transient failures stash the initial
                    // size record and retry it before any output.
                    if journal_error_is_fatal(&e) {
                        journal_fatal = true;
                    } else {
                        journal_stash_push(
                            &mut journal_stash,
                            &mut journal_stash_bytes,
                            &mut journal_dropped_bytes,
                            StashItem::Resize {
                                cols: initial_cols,
                                rows: initial_rows,
                            },
                        );
                    }
                    journal_err = Some(e.to_string());
                    journal_retry_at_ms = clock.now_ms() + journal_backoff_ms;
                    loop_shared.update_status(|s| s.journal_error = journal_err.clone());
                }
            }

            loop {
                // 1. Cancel: independent flag, checked first every iteration,
                //    even when the control mailbox is saturated.
                if loop_shared.cancel.load(Ordering::Acquire) {
                    cancelled_at_exit = true;
                    break;
                }

                // 2. Control plane (bounded; drained without blocking).
                let mut control_seen = false;
                while let Ok(msg) = control_rx.try_recv() {
                    control_seen = true;
                    match msg {
                        Control::Resize { cols, rows } => {
                            let _ = coalescer.submit(cols, rows);
                        }
                        Control::SetOwner { reply } => {
                            // Epoch rotates on attach/owner change (spec §4):
                            // in-flight records keep the old epoch, later
                            // records carry the new one.
                            current_epoch = uuid::Uuid::new_v4().to_string();
                            loop_shared.update_status(|s| s.epoch = current_epoch.clone());
                            let _ = reply.send(current_epoch.clone());
                        }
                    }
                }

                // 3. Exit polling (parameterized try_wait; no wait thread).
                let now_ms = clock.now_ms();
                if now_ms >= next_exit_poll_ms {
                    next_exit_poll_ms = now_ms + exit_poll_interval.as_millis().max(1) as u64;
                    if !root_exited {
                        match pty.poll_exit() {
                            Ok(Some(status)) => {
                                root_exited = true;
                                exit_code = Some(status.exit_code() as i32);
                                loop_shared.update_status(|s| {
                                    s.exit_code = exit_code;
                                    s.root_exited = true;
                                });
                            }
                            Ok(None) => {}
                            Err(_) => {
                                // Child handle broken: treat root as gone;
                                // exit code stays unknown.
                                root_exited = true;
                                loop_shared.update_status(|s| s.root_exited = true);
                            }
                        }
                    }
                }

                // 4. Reader bytes: journal first, then pending emit. Paused
                //    when the pending buffer is over budget (backpressure
                //    chain: actor -> bounded channel -> reader -> pty OS
                //    buffer). A cap/budget journal failure stops intake
                //    entirely; a transient one (I/O, disk full) degrades to
                //    a bounded stash with retried, ordered flushes — a
                //    disk-full cascade must not freeze every session
                //    permanently (02-runner §5 keeps the root running).
                if journal_fatal {
                    reader_eof = true;
                }
                let journal_paused =
                    journal_err.is_some() && !journal_fatal && clock.now_ms() < journal_retry_at_ms;
                let mut reader_seen = false;
                while !reader_eof && !journal_paused && pending_bytes < OUTPUT_HIGH_BYTES {
                    match reader_rx.try_recv() {
                        Ok(LoopInput::Bytes(bytes)) => {
                            reader_seen = true;
                            last_byte_ms = clock.now_ms();
                            reader_channel_empty = false;
                            if journal_err.is_none() {
                                match journal.append_output(&bytes) {
                                    Ok(seq) => {
                                        pending_bytes += bytes.len();
                                        loop_shared.update_status(|s| s.last_seq = seq);
                                        pending.push_back(ActorEvent {
                                            seq,
                                            epoch: current_epoch.clone(),
                                            kind: ActorEventKind::Output(bytes),
                                        });
                                    }
                                    Err(e) => {
                                        if journal_error_is_fatal(&e) {
                                            journal_fatal = true;
                                            reader_eof = true;
                                        } else {
                                            journal_stash_push(
                                                &mut journal_stash,
                                                &mut journal_stash_bytes,
                                                &mut journal_dropped_bytes,
                                                StashItem::Output(bytes),
                                            );
                                        }
                                        journal_err =
                                            Some(format!("{e} ({journal_stash_bytes} B unsaved)"));
                                        journal_backoff_ms = (journal_backoff_ms * 2)
                                            .clamp(JOURNAL_RETRY_MIN_MS, JOURNAL_RETRY_MAX_MS);
                                        journal_retry_at_ms = clock.now_ms() + journal_backoff_ms;
                                        loop_shared.update_status(|s| {
                                            s.journal_error = journal_err.clone()
                                        });
                                        break;
                                    }
                                }
                            } else {
                                journal_stash_push(
                                    &mut journal_stash,
                                    &mut journal_stash_bytes,
                                    &mut journal_dropped_bytes,
                                    StashItem::Output(bytes),
                                );
                            }
                        }
                        Ok(LoopInput::Eof) => reader_eof = true,
                        Err(TryRecvError::Empty) => {
                            reader_channel_empty = true;
                            break;
                        }
                        Err(TryRecvError::Disconnected) => reader_eof = true,
                    }
                }

                // 4b. Degraded-journal retry: attempt an ordered flush of
                //     the stash (output and resize records keep their seq
                //     order). A full flush clears the degradation; a failure
                //     grows the backoff and leaves the remainder stashed.
                if journal_err.is_some() && !journal_fatal && clock.now_ms() >= journal_retry_at_ms
                {
                    let mut recovered = true;
                    while let Some(item) = journal_stash.pop_front() {
                        journal_stash_bytes -= stash_item_len(&item);
                        let appended = match &item {
                            StashItem::Output(bytes) => journal.append_output(bytes),
                            StashItem::Resize { cols, rows } => journal.append_resize(*cols, *rows),
                        };
                        match appended {
                            Ok(seq) => {
                                loop_shared.update_status(|s| s.last_seq = seq);
                                match item {
                                    StashItem::Output(bytes) => {
                                        pending_bytes += bytes.len();
                                        pending.push_back(ActorEvent {
                                            seq,
                                            epoch: current_epoch.clone(),
                                            kind: ActorEventKind::Output(bytes),
                                        });
                                    }
                                    StashItem::Resize { cols, rows } => {
                                        pending.push_back(ActorEvent {
                                            seq,
                                            epoch: current_epoch.clone(),
                                            kind: ActorEventKind::Resize { cols, rows },
                                        });
                                    }
                                }
                            }
                            Err(e) => {
                                if journal_error_is_fatal(&e) {
                                    journal_fatal = true;
                                }
                                journal_err = Some(format!(
                                    "{e} ({} KiB unsaved so far)",
                                    journal_stash_bytes / 1024
                                ));
                                journal_backoff_ms =
                                    (journal_backoff_ms * 2).min(JOURNAL_RETRY_MAX_MS);
                                journal_retry_at_ms = clock.now_ms() + journal_backoff_ms;
                                loop_shared
                                    .update_status(|s| s.journal_error = journal_err.clone());
                                recovered = false;
                                break;
                            }
                        }
                    }
                    if recovered && journal_err.is_some() {
                        journal_err = None;
                        journal_backoff_ms = JOURNAL_RETRY_MIN_MS;
                        if journal_dropped_bytes > 0 {
                            tracing::warn!(
                                dropped_bytes = journal_dropped_bytes,
                                "journal recovered; oldest degraded output was dropped"
                            );
                        }
                        loop_shared.update_status(|s| s.journal_error = None);
                    }
                }

                // 5. Coalesced resize, applied AFTER the outputs received so
                //    far (spec §5: emit order == journal order == apply
                //    order). While degraded the resize record joins the
                //    stash instead — appending it directly would reorder the
                //    journal around the stashed outputs.
                if let Some((cols, rows)) = coalescer.poll() {
                    if pty.resize(cols, rows).is_ok() {
                        if journal_err.is_none() {
                            if let Ok(seq) = journal.append_resize(cols, rows) {
                                loop_shared.update_status(|s| s.last_seq = seq);
                                pending.push_back(ActorEvent {
                                    seq,
                                    epoch: current_epoch.clone(),
                                    kind: ActorEventKind::Resize { cols, rows },
                                });
                            }
                        } else {
                            journal_stash_push(
                                &mut journal_stash,
                                &mut journal_stash_bytes,
                                &mut journal_dropped_bytes,
                                StashItem::Resize { cols, rows },
                            );
                        }
                    }
                }

                // 6. Emit as far as the sink allows. The journal already has
                //    every record, so a blocked sink only lags the cursor.
                while sink.can_send() {
                    match pending.pop_front() {
                        Some(event) => {
                            pending_bytes = pending_bytes.saturating_sub(event_output_len(&event));
                            sink.emit(event);
                        }
                        None => break,
                    }
                }

                // 7. Lifecycle transitions (spec §7: root exit alone is not
                //    completion; wait for the pty to drain and owned
                //    descendants to leave, bounded by the drain timeout).
                //    Unix ptys deliver EOF; ConPTY never does while the
                //    master lives, so "drained" there means output has gone
                //    quiet for DRAIN_QUIESCENCE_MS.
                if !announced_running {
                    announced_running = true;
                    loop_shared.update_status(|s| s.lifecycle = SessionLifecycle::Running);
                }
                //    A journal failure (cap / disk full) only stops intake:
                //    the spec surfaces JOURNAL_LIMIT/DISK_FULL and keeps the
                //    root running (02-runner §5). It is NOT a drain trigger
                //    — with no owned group (plain shells) the drain window
                //    would otherwise expire and finalize `kill()` a live
                //    root two seconds after the cap.
                let drain_trigger = root_exited || (reader_eof && journal_err.is_none());
                if drain_trigger {
                    let now = clock.now_ms();
                    if drain_started_ms.is_none() {
                        drain_started_ms = Some(now);
                        next_owned_poll_ms = 0;
                        loop_shared.update_status(|s| s.lifecycle = SessionLifecycle::Draining);
                    }
                    if now >= next_owned_poll_ms {
                        next_owned_poll_ms = now + exit_poll_interval.as_millis().max(1) as u64;
                        if let Some(count) = descendants.try_owned_alive() {
                            owned_alive_known = Some(count);
                        }
                    }
                    // Unknown (never answered) is not "empty": the loop keeps
                    // waiting rather than closing a pty whose group it cannot
                    // see. Cancel still ends it.
                    let group_empty = owned_alive_known == Some(0);
                    let quiet = now.saturating_sub(last_byte_ms) >= DRAIN_QUIESCENCE_MS;
                    let drained = root_exited
                        && group_empty
                        && reader_channel_empty
                        && pending.is_empty()
                        && (reader_eof || quiet);
                    let timed_out = now.saturating_sub(drain_started_ms.unwrap_or(0))
                        >= drain_timeout.as_millis() as u64;
                    if timed_out && owned_alive_known.is_none() && !warned_unknown_group {
                        warned_unknown_group = true;
                        tracing::warn!(
                            session = %loop_shared.session_id,
                            "owned-descendant count unknown past the drain timeout; \
                             waiting (cancel ends the session)"
                        );
                    }
                    // Spec 02-runner §5: root exit with owned descendants
                    // alive keeps the workload RUNNING (root_exited=true) —
                    // never an automatic success. The drain timeout only
                    // bounds the wait once the owned group is empty; closing
                    // the master here would kill the descendants on ConPTY
                    // (kill-on-close). Stay in the loop until the last
                    // descendant leaves or the caller cancels.
                    if (drained || timed_out) && group_empty {
                        break;
                    }
                }

                // Idle backoff: an idle session's loop relaxes from 2 ms to
                // 10 ms polling (8 idle shells at 500 Hz each dominated the
                // daemon's idle CPU; 06-verification §5 target ≤0.02 cores).
                // Any activity — reader bytes, control message, pending sink
                // backlog, drain phase — returns the loop to full speed.
                // A successfully drained channel is empty even during a
                // continuous redraw. Count bytes processed this round, not
                // just bytes left over, or a busy session backs off to 10ms.
                let activity = reader_seen || control_seen || !pending.is_empty();
                idle_rounds = if activity {
                    0
                } else {
                    idle_rounds.saturating_add(1)
                };
                let step = if idle_rounds >= 8 {
                    Duration::from_millis(10)
                } else {
                    LOOP_STEP
                };
                // Reader output and control messages wake the actor immediately.
                // The park permit retains wakes arriving just before this call.
                std::thread::park_timeout(step);
            }

            // ------------------------------------------------------------------
            // Finalize sequence (single place; runs for cancel AND natural
            // exit, including cancel-during-starting).
            input.shutdown(); // wake/refuse the writer path
            drop(reader_rx); // unblock a reader stuck on a full channel
            let _ = pty.kill(); // no-op when the child already exited
            #[cfg(unix)]
            if !root_exited {
                // portable-pty's kill() is SIGHUP. A foreground program that
                // ignores it (nohup, `trap '' HUP`) would outlive its
                // cancelled session; 02-runner §7 escalates TERM → grace →
                // KILL for groups, and the direct child gets the same
                // treatment here (shell sessions have no group).
                escalate_direct_child(&pty);
            }
            // Closing the master destroys the pty: a reader blocked inside a
            // ConPTY read is NOT interrupted by the child kill alone, while
            // ClosePseudoConsole tears conhost down and unblocks it.
            pty.close();
            while sink.can_send() {
                match pending.pop_front() {
                    Some(event) => sink.emit(event),
                    None => break,
                }
            }
            // A platform error here keeps the last known count rather than
            // reporting an empty group.
            let owned_alive = descendants
                .try_owned_alive()
                .or(owned_alive_known)
                .unwrap_or(0);
            let info = TeardownInfo {
                session_id: loop_shared.session_id.clone(),
                cancelled: cancelled_at_exit,
                exit_code,
                descendants_remaining: owned_alive > 0,
            };
            cleanup.teardown(&info);
            loop_shared.update_status(|s| {
                s.lifecycle = SessionLifecycle::Finalized;
                s.cancelled = cancelled_at_exit;
                s.exit_code = exit_code;
                s.descendants_remaining = owned_alive > 0;
            });
            tracing::debug!(
                session = %loop_shared.session_id,
                cancelled = cancelled_at_exit,
                exit_code = ?exit_code,
                descendants_remaining = owned_alive > 0,
                "session actor finalized"
            );

            // Reap the session threads so the join handle implies a fully
            // torn-down session.
            let _ = reader_thread.join();
            let _ = writer_thread.join();
            loop_shared.status_snapshot()
        })
        .expect("spawn pty actor thread");

    (handle, actor_thread)
}

fn event_output_len(event: &ActorEvent) -> usize {
    match &event.kind {
        ActorEventKind::Output(bytes) => bytes.len(),
        ActorEventKind::Resize { .. } => 0,
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn completions_fan_out_to_registered_waiters() {
        let completions = WriteCompletions::new();
        let rx = completions.register("in-1");
        // 등록 → enqueue 순서 계약: 등록 뒤 언제 완료가 와도 받는다.
        completions.complete("in-1", InputOutcome::Accepted(12));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)),
            Ok(InputOutcome::Accepted(12))
        );
        // 두 번째 완료(중복)는 대기자가 없어 조용히 무시된다.
        completions.complete("in-1", InputOutcome::Accepted(12));
    }

    #[test]
    fn wait_without_completion_times_out() {
        let completions = WriteCompletions::new();
        let rx = completions.register("in-2");
        assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
    }

    /// L7: 거부된 입력의 대기자는 `unregister`로 즉시 거둬진다 — 그러지
    /// 않으면 세션이 사는 동안 맵이 계속 자란다.
    #[test]
    fn unregister_drops_the_waiter_entry() {
        let completions = WriteCompletions::new();
        let rx = completions.register("in-4");
        assert_eq!(completions.waiters.lock().unwrap().len(), 1);
        completions.unregister("in-4");
        assert!(
            completions.waiters.lock().unwrap().is_empty(),
            "해제 후에는 항목이 남지 않는다"
        );
        // 해제된 대기자에게 오는 뒤늦은 완료는 조용히 버려진다.
        completions.complete("in-4", InputOutcome::Accepted(1));
        assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
        // 없는 id의 해제는 무해하다(멱등).
        completions.unregister("in-4");
        completions.unregister("never-registered");
        assert!(completions.waiters.lock().unwrap().is_empty());
    }

    #[test]
    fn multiple_waiters_all_receive() {
        let completions = WriteCompletions::new();
        let a = completions.register("in-3");
        let b = completions.register("in-3");
        completions.complete("in-3", InputOutcome::Unknown);
        assert_eq!(
            a.recv_timeout(Duration::from_secs(1)),
            Ok(InputOutcome::Unknown)
        );
        assert_eq!(
            b.recv_timeout(Duration::from_secs(1)),
            Ok(InputOutcome::Unknown)
        );
    }
}
