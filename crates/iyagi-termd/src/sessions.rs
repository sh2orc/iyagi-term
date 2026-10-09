//! Session side of the daemon: per-session registry entry, attach views,
//! epoch/ownership bookkeeping, actor wiring adapters (journal sink, notify
//! sink, descendant watch, cleanup), and the journal-driven output pump.
//!
//! Delivery model (spec `02-runner.md` §4–5): every output/resize passes
//! through the MTJ1 journal; delivery to views is a cursor over the journal,
//! so replay-after-attach and live delivery are the same mechanism. A slow
//! view stops its own cursor (per-view credit watermarks via
//! `FlowController`) while the journal keeps moving.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use base64::Engine;
use term_contracts::ids::{ConnectionId, SessionId, ViewId, WorkloadId};
use term_contracts::session::{AttachAccess, SessionOutput, TerminalFrameKind};
use term_contracts::U64String;
use term_core::Clock as _;
use term_platform::ResourcePlatform;
use term_pty::actor::{ActorEvent, ActorEventKind, JournalSink, OutputSink};
use term_pty::flow::{FlowController, FlowTransition, SentRecord};
use term_pty::journal::{
    GlobalJournalBudget, JournalFlowError, JournalOptions, JournalReader, JournalWriter,
    HEADER_LEN, MAX_OUTPUT_PAYLOAD,
};
use term_pty::segments::{
    list_closed_segments, scan_window_segments, SegmentCursor, SegmentSnapshot, SegmentTracker,
};
use term_storage::Storage;

use crate::connections::SecretRedactor;
use crate::state::DaemonState;

/// One attached UI view of a session.
#[derive(Debug, Clone)]
pub struct ViewEntry {
    pub view_id: ViewId,
    pub access: AttachAccess,
    /// Control connection that attached this view; its linked data
    /// connection receives the output frames.
    pub conn: ConnectionId,
    /// Epoch the view attached under.
    pub epoch: String,
    /// Next journal seq to deliver to this view (replay + live cursor).
    pub next_seq: u64,
    /// Writer has an in-flight session.input (one outstanding per writer).
    pub input_in_flight: bool,
}

/// Per-session daemon state.
pub struct SessionEntry {
    pub session_id: SessionId,
    pub workload_id: WorkloadId,
    pub journal_path: PathBuf,
    /// Runtime-adjustable journal cap (`retention.set_limit`); atomic so an
    /// Arc-shared entry can be updated without exclusive access.
    pub journal_limit: AtomicU64,
    /// Current epoch UUID (rotates on attach / owner change).
    pub epoch: Mutex<String>,
    /// Current writer view (None while no writer is attached).
    pub owner_view: Mutex<Option<ViewId>>,
    pub views: Mutex<HashMap<ViewId, ViewEntry>>,
    /// Journal cursor high-water mark (set by the actor's notify sink).
    pub last_seq: AtomicU64,
    /// Live writer handle for interactive flushes when the delivery pump
    /// catches up to buffered (not yet flushed) records. `None` for sessions
    /// restored without a live actor.
    pub journal_inner: Mutex<Option<Arc<Mutex<JournalInner>>>>,
    /// seq → 세그먼트 커서(회전 번호 + 절대 오프셋) checkpoints(64레코드마다
    /// 하나) for incremental journal window reads. Without it every delivery
    /// batch re-scans the journal from its head (O(journal²) over a
    /// session's lifetime — the flood-RSS slope culprit). 헤드가 잘리면 그
    /// 아래 항목은 버린다(지워진 파일을 가리킨다).
    pub journal_offsets: Mutex<std::collections::BTreeMap<u64, SegmentCursor>>,
    /// 롤링 저널의 머리/꼬리 요약(writer가 회전·잘라내기마다 갱신). attach
    /// 시작 seq와 잘린 바이트의 출처이며 writer가 사라진 뒤에도 남는다.
    pub journal_segments: Arc<SegmentTracker>,
    /// 배달용 저널 읽기가 연속으로 실패하기 시작한 시각(`None` = 정상).
    /// 순간 오류는 재시도하지만 [`JOURNAL_READ_FAIL_SHED_AFTER`] 이상
    /// 이어지면 저널을 읽을 수 없는 상태다 — 뷰를 떼고 다시 붙으라고
    /// 알린다(조용히 멈춘 재생을 남기지 않는다).
    pub journal_read_failing_since: Mutex<Option<std::time::Instant>>,
    /// Recent resize records (seq, cols, rows, monotonic_ms) for
    /// `session.resize` applied_seq correlation.
    pub recent_resizes: Mutex<Vec<(u64, u16, u16, u64)>>,
    /// Completion notification, independent of the delivery pump's wakeup.
    pub resize_notify: tokio::sync::Notify,
    pub resize_in_flight: AtomicBool,
    /// Per-view flow credits over the shared global budget.
    pub flow: Mutex<FlowController>,
    /// Wakeup for the delivery pump.
    pub wake_tx: Mutex<()>,
    pub wake_cv: Condvar,
    /// `wake()`가 `wait()`보다 먼저 도착해도 잃지 않는다 — 조건 변수는
    /// 상태가 없어 exit 판정과 `wait_timeout` 사이의 wake가 사라지면
    /// attach 직후 첫 재생이 최대 200 ms 늦는다.
    pub wake_pending: AtomicBool,
    /// Stop flag for the pump (session registry removal / daemon shutdown).
    pub pump_stop: AtomicBool,
    /// Whether a delivery-pump thread is currently running. The pump exits
    /// when the last view detaches AND the actor finalized; a later attach
    /// (e.g. replaying a finished session's journal) restarts it via
    /// [`ensure_pump`].
    pub pump_alive: AtomicBool,
    /// Serializes the pump exit-decision with `ensure_pump` respawns.
    /// Lock order: pump_ctl → views (never the reverse).
    pub pump_ctl: std::sync::Mutex<()>,
    /// Current terminal size.
    pub size: Mutex<(u16, u16)>,
    /// Finalization marker (actor finished; the pump may still deliver
    /// buffered journal records to attached views).
    pub actor_finalized: AtomicBool,
}

impl SessionEntry {
    pub fn current_epoch(&self) -> String {
        self.epoch.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Wake the delivery pump (new records or returned credits).
    pub fn wake(&self) {
        let guard = self.wake_tx.lock().unwrap_or_else(|p| p.into_inner());
        self.wake_pending.store(true, Ordering::Release);
        self.wake_cv.notify_all();
        drop(guard);
    }

    /// Wait for pump work or timeout. A wake that landed since the last
    /// wait returns immediately (both sides hold `wake_tx`, so the flag and
    /// the notify cannot interleave with this check).
    pub fn wait(&self, timeout: Duration) {
        let guard = self.wake_tx.lock().unwrap_or_else(|p| p.into_inner());
        if self.wake_pending.swap(false, Ordering::AcqRel) {
            return;
        }
        let _ = self
            .wake_cv
            .wait_timeout(guard, timeout)
            .unwrap_or_else(|p| p.into_inner());
        self.wake_pending.store(false, Ordering::Release);
    }

    /// Record + announce a resize record (from the actor's emit path).
    pub fn record_resize(&self, seq: u64, cols: u16, rows: u16, now_ms: u64) {
        let mut ring = self
            .recent_resizes
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        ring.push((seq, cols, rows, now_ms));
        let len = ring.len();
        if len > 16 {
            ring.drain(..len - 16);
        }
        drop(ring);
        self.resize_notify.notify_waiters();
        self.wake();
    }

    /// 배달용 저널 읽기가 실패했다 — 연속 실패의 시작을 기록하고(첫 실패면
    /// 지금부터) 실패가 이어진 시간을 돌려준다.
    pub fn note_journal_read_failed(&self) -> Duration {
        let mut failing = self
            .journal_read_failing_since
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        failing
            .get_or_insert_with(std::time::Instant::now)
            .elapsed()
    }

    /// 배달용 저널 읽기 창을 지운다(읽기가 성공했거나, 실패로 뷰를 뗀 뒤
    /// 다음 판정을 위해 5초를 다시 센다).
    pub fn clear_journal_read_failure(&self) {
        *self
            .journal_read_failing_since
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
    }
}

// ---------------------------------------------------------------------------
// Actor wiring adapters

/// Journal sink writing the MTJ1 file and periodically syncing SQLite
/// session progress (`last_seq`, `journal_bytes`) — the DB gets metadata,
/// never PTY bytes (spec §5).
///
/// The writer is shared behind a mutex so a per-session flusher thread can
/// drive the 250 ms flush cadence even when appends stop arriving (a final
/// partial burst must still reach the delivery pump, which reads the file).
pub struct SharedJournal {
    inner: Arc<Mutex<JournalInner>>,
}

pub struct JournalInner {
    writer: JournalWriter,
    storage: Arc<Storage>,
    session_id: SessionId,
    last_progress_ms: u64,
    /// Last (seq, bytes) actually persisted — idle sessions must not
    /// fsync identical progress rows every 250 ms (idle-CPU target,
    /// 06-verification §5).
    last_persisted: (u64, u64),
    finished: bool,
}

impl JournalInner {
    /// `retention.set_limit`: 살아 있는 writer의 세션 상한을 바꾼다.
    /// `SessionEntry.journal_limit`만 갱신하면 RPC는 성공해도 writer는
    /// 예전 cap에서 계속 거부한다(무효 no-op).
    pub fn set_session_limit(&mut self, limit: u64) {
        self.writer.set_session_limit(limit);
    }

    pub fn session_limit(&self) -> u64 {
        self.writer.session_limit()
    }
}

impl SharedJournal {
    pub fn open(
        state: &DaemonState,
        session_id: &SessionId,
        journal_budget: Arc<Mutex<GlobalJournalBudget>>,
        session_limit: u64,
        segment_cap: u64,
    ) -> std::io::Result<(SharedJournal, Arc<Mutex<JournalInner>>, Arc<SegmentTracker>)> {
        let path = state.paths.journal(session_id.as_str());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let uuid = uuid::Uuid::parse_str(session_id.as_str())
            .map_err(|e| std::io::Error::other(format!("session id is not a UUID: {e}")))?;
        // 롤링 모드(02-runner §5): 상한에 닿으면 읽기를 멈추는 대신 가장
        // 오래된 세그먼트를 지운다. 세그먼트 목표는 상한/8(최대 segment_cap).
        let options = JournalOptions {
            session_limit,
            segment_cap: Some(segment_cap),
        };
        let writer = JournalWriter::open_with(&path, uuid, options, journal_budget)
            .map_err(|e| std::io::Error::other(format!("journal open failed: {e}")))?;
        let tracker = writer
            .tracker()
            .expect("rolling journal publishes a segment tracker");
        let inner = Arc::new(Mutex::new(JournalInner {
            writer,
            storage: Arc::clone(&state.storage),
            session_id: session_id.clone(),
            last_progress_ms: 0,
            last_persisted: (0, 0),
            finished: false,
        }));
        Ok((
            SharedJournal {
                inner: Arc::clone(&inner),
            },
            inner,
            tracker,
        ))
    }
}

impl JournalSink for SharedJournal {
    fn append_output(&mut self, data: &[u8]) -> std::io::Result<u64> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let now = term_core::MonotonicClock::new().now_ms();
        let seq = inner
            .writer
            .append_output(data)
            .map_err(std::io::Error::other)?;
        inner.tick(now);
        Ok(seq)
    }

    fn append_resize(&mut self, cols: u16, rows: u16) -> std::io::Result<u64> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let now = term_core::MonotonicClock::new().now_ms();
        let seq = inner
            .writer
            .append_resize(cols, rows)
            .map_err(std::io::Error::other)?;
        inner.tick(now);
        Ok(seq)
    }
}

/// 라우팅된 Claude pane(`claude_provider`)의 저널 앞단: PTY 출력 청크가
/// 저널에 적히기 전에 provider 토큰을 `[redacted]`로 바꾼다. 저널은 라이브
/// 뷰·재생의 유일한 원천이므로(02-runner §4–5) 여기 한 곳이면 토큰이
/// 디스크에도, 화면에도 남지 않는다.
///
/// v1 한계 — **청크 단위** 최선 노력이다. PTY 읽기 경계에 걸쳐 두 청크로
/// 쪼개진 토큰은 통과한다(경계 버퍼링은 대화형 지연을 늘리고 `[redacted]`
/// 자리 계산을 복잡하게 만들어 넣지 않았다; 토큰은 한 줄에 통째로
/// 출력되는 것이 보통이라 실제로는 드물다). 비라우팅 세션은 이 래퍼를
/// 거치지 않는다(`start_session_actor`가 `SharedJournal`을 그대로 꽂는다).
pub struct RedactingJournal<J: JournalSink> {
    inner: J,
    redactor: Arc<SecretRedactor>,
}

impl<J: JournalSink> RedactingJournal<J> {
    pub fn new(inner: J, redactor: Arc<SecretRedactor>) -> Self {
        RedactingJournal { inner, redactor }
    }
}

impl<J: JournalSink> JournalSink for RedactingJournal<J> {
    fn append_output(&mut self, data: &[u8]) -> std::io::Result<u64> {
        match redact_chunk(&self.redactor, data) {
            std::borrow::Cow::Borrowed(_) => self.inner.append_output(data),
            std::borrow::Cow::Owned(redacted) => {
                // 치환으로 청크가 자랄 수 있다(토큰이 `[redacted]`보다 짧을
                // 때). 레코드 상한을 넘기지 않게 나눠 넣고 마지막 seq를
                // 돌려준다 — 액터는 seq를 순서 표시로만 쓴다.
                let mut last = 0;
                for piece in redacted.chunks(MAX_OUTPUT_PAYLOAD) {
                    last = self.inner.append_output(piece)?;
                }
                Ok(last)
            }
        }
    }

    fn append_resize(&mut self, cols: u16, rows: u16) -> std::io::Result<u64> {
        self.inner.append_resize(cols, rows)
    }
}

/// 청크에서 등록된 비밀을 지운다. 토큰이 없으면(대부분의 청크) 복사 없이
/// 입력을 빌려 돌려준다. 유효한 UTF-8 구간마다 [`SecretRedactor::redact_plain`]
/// (부분 문자열 치환만 — JSON 재직렬화 없음, 그 밖의 바이트는 그대로)을
/// 돌리고, 깨진 바이트(청크 경계에서 잘린 멀티바이트 문자·이스케이프
/// 잔여물)는 그대로 흘린다 — 토큰은 ASCII이므로 깨진 바이트 안에 숨을 수
/// 없다.
pub(crate) fn redact_chunk<'a>(
    redactor: &SecretRedactor,
    data: &'a [u8],
) -> std::borrow::Cow<'a, [u8]> {
    if !redactor.contains_secret(data) {
        return std::borrow::Cow::Borrowed(data);
    }
    let scrub = |text: &str| -> String {
        let mut owned = text.to_owned();
        redactor.redact_plain(&mut owned);
        owned
    };
    let mut out = Vec::with_capacity(data.len());
    let mut rest = data;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                out.extend_from_slice(scrub(text).as_bytes());
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    let text = std::str::from_utf8(&rest[..valid]).expect("prefix is valid UTF-8");
                    out.extend_from_slice(scrub(text).as_bytes());
                }
                // `None`은 "끝에서 잘린 시퀀스": 남은 바이트 전부가 그것이다.
                let skip = error.error_len().unwrap_or(rest.len() - valid);
                out.extend_from_slice(&rest[valid..valid + skip]);
                rest = &rest[valid + skip..];
            }
        }
    }
    std::borrow::Cow::Owned(out)
}

impl JournalInner {
    /// Immediate flush for the interactive delivery path: the pump calls
    /// this when a live view needs records that are still buffered.
    pub fn flush_now(&mut self) {
        if let Err(e) = self.writer.flush_now() {
            tracing::warn!(session = %self.session_id, error = %e, "journal flush_now failed");
        }
    }

    /// Rate-limited flush + DB progress sync (250 ms cadence each).
    fn tick(&mut self, now_ms: u64) {
        if let Err(e) = self.writer.flush_tick(now_ms) {
            tracing::warn!(session = %self.session_id, error = %e, "journal flush failed");
        }
        if now_ms.saturating_sub(self.last_progress_ms) < 250 {
            return;
        }
        self.last_progress_ms = now_ms;
        self.sync_progress();
    }

    fn sync_progress(&mut self) {
        let seq = self.writer.last_seq();
        let bytes = self.writer.journal_bytes();
        if seq > 0 && (seq, bytes) != self.last_persisted {
            if let Err(e) = self
                .storage
                .update_session_progress(&self.session_id, seq, bytes)
            {
                tracing::warn!(session = %self.session_id, error = %e, "session progress sync failed");
            } else {
                self.last_persisted = (seq, bytes);
            }
        }
    }
}

impl Drop for JournalInner {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let _ = self.writer.finalize();
        self.sync_progress();
    }
}

/// Per-session journal flusher: keeps the 250 ms flush cadence alive while
/// the session runs so the delivery pump never waits on a buffered tail.
/// Exits when the session entry is finalized (the actor's Drop performs the
/// final flush + progress sync).
pub fn spawn_journal_flusher(
    state: Arc<DaemonState>,
    session: Arc<SessionEntry>,
    journal: Arc<Mutex<JournalInner>>,
) {
    std::thread::Builder::new()
        .name(format!("journal-flush-{}", session.session_id))
        .spawn(move || loop {
            if session.actor_finalized.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
            if session.actor_finalized.load(Ordering::Acquire) {
                return;
            }
            let mut inner = journal.lock().unwrap_or_else(|p| p.into_inner());
            inner.tick(state.now_ms());
        })
        .expect("spawn journal flusher");
}

/// 종료된 세션이 붙들고 있던 저널 쓰기 핸들을 놓아준다(02-runner §7의
/// teardown 순서: writer → master → group handle).
///
/// `SessionEntry.journal_inner`는 `Arc<Mutex<JournalInner>>`의 한 소유자다.
/// 이 참조를 비우지 않으면 actor 스레드와 flusher가 끝나도 strong count가
/// 0이 되지 않아 [`JournalInner`]의 Drop이 영영 돌지 않는다 — 즉 세션당
/// 64 KiB 쓰기 버퍼와 파일 서술자 하나가 데몬이 죽을 때까지 남는다.
/// 마지막 버퍼는 여기서 즉시 flush해 재생(파일 직접 읽기)이 곧바로
/// 전체 기록을 보게 한다. 반환값은 실제로 해제했는지 여부다.
pub fn release_journal(session: &SessionEntry) -> bool {
    let inner = session
        .journal_inner
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    match inner {
        Some(inner) => {
            inner.lock().unwrap_or_else(|p| p.into_inner()).flush_now();
            drop(inner);
            true
        }
        None => false,
    }
}

/// Output sink: forwards the emit cursor + resize announcements to the
/// session pump. `can_send` is always true — the journal is the buffering
/// layer and per-view slowness must never stall it (spec §4: "journal에
/// 공간이 있으면 PTY를 계속 읽고").
pub struct NotifySink {
    state: Arc<DaemonState>,
    session_id: SessionId,
}

impl NotifySink {
    pub fn new(state: Arc<DaemonState>, session_id: SessionId) -> Self {
        NotifySink { state, session_id }
    }

    fn session(&self) -> Option<Arc<SessionEntry>> {
        self.state.session(&self.session_id)
    }
}

impl OutputSink for NotifySink {
    fn can_send(&self) -> bool {
        true
    }

    fn emit(&mut self, event: ActorEvent) {
        let Some(session) = self.session() else {
            return;
        };
        session.last_seq.store(event.seq, Ordering::Release);
        if let ActorEventKind::Resize { cols, rows } = event.kind {
            session.record_resize(event.seq, cols, rows, self.state.now_ms());
        } else {
            session.wake();
        }
    }
}

/// Descendant watch over the OS resource group (managed sessions).
pub struct GroupWatch {
    platform: Arc<dyn ResourcePlatform>,
    group: term_platform::GroupHandle,
}

impl GroupWatch {
    pub fn new(platform: Arc<dyn ResourcePlatform>, group: term_platform::GroupHandle) -> Self {
        GroupWatch { platform, group }
    }
}

impl term_pty::actor::DescendantWatch for GroupWatch {
    fn owned_alive(&self) -> usize {
        self.try_owned_alive().unwrap_or(0)
    }

    /// A platform error is "unknown", not "nobody left": the actor keeps
    /// its last known count instead of ending the drain early.
    fn try_owned_alive(&self) -> Option<usize> {
        self.platform
            .member_identities(&self.group)
            .map(|members| members.len())
            .ok()
    }
}

// ---------------------------------------------------------------------------
// Delivery pump

/// Spawn the per-session delivery pump. The pump reads journal records in
/// order and pushes `session.output` frames to each view's data connection,
/// gated by the per-view flow credits. It exits when `pump_stop` is set and
/// no views remain.
pub fn spawn_pump(state: Arc<DaemonState>, session: Arc<SessionEntry>) {
    session.pump_alive.store(true, Ordering::Release);
    std::thread::Builder::new()
        .name(format!("pump-{}", session.session_id))
        .spawn(move || pump_loop(state, session))
        .expect("spawn session pump");
}

/// (Re)start the delivery pump when a view attaches to a session whose pump
/// already exited (finished session + all views detached earlier). Races with
/// the pump's own exit decision are serialized by `pump_ctl`.
pub fn ensure_pump(state: Arc<DaemonState>, session: Arc<SessionEntry>) {
    let _ctl = session.pump_ctl.lock().unwrap_or_else(|p| p.into_inner());
    if !session.pump_alive.load(Ordering::Acquire) && !session.pump_stop.load(Ordering::Acquire) {
        spawn_pump(state, Arc::clone(&session));
    }
}

fn pump_loop(state: Arc<DaemonState>, session: Arc<SessionEntry>) {
    loop {
        if session.pump_stop.load(Ordering::Acquire) {
            break;
        }
        let delivery = deliver_pending(&state, &session);
        // Exit decision under pump_ctl so a concurrent `ensure_pump` either
        // sees the pump still alive (and we see its freshly inserted view) or
        // observes the exit and respawns (lock order: pump_ctl → views).
        let (should_exit, view_count) = {
            let _ctl = session.pump_ctl.lock().unwrap_or_else(|p| p.into_inner());
            let view_count = session
                .views
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .len();
            if view_count == 0 && session.actor_finalized.load(Ordering::Acquire) {
                session.pump_alive.store(false, Ordering::Release);
                (true, view_count)
            } else {
                (false, view_count)
            }
        };
        if should_exit {
            break;
        }
        match delivery {
            Delivery::Progressed => std::thread::sleep(Duration::from_millis(1)),
            // 보낼 레코드가 남았는데 데이터 연결 큐가 찼다: writer는 몇 ms 안에
            // 비우므로 짧게만 쉬고 이어 보낸다(재생 속도를 20 ms 폴링에 묶지 않는다).
            Delivery::QueueFull => session.wait(Duration::from_millis(2)),
            Delivery::Idle => {
                // A live session with no attached view has nothing to deliver:
                // polling it at 50 Hz burned idle CPU for the whole (possibly
                // day-long) detached window. `attach` calls `wake()`, so the
                // longer sleep costs no latency.
                session.wait(Duration::from_millis(if view_count == 0 {
                    200
                } else {
                    20
                }));
            }
        }
    }
}

/// 전달 패스 한 번의 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// 레코드가 하나 이상 나갔다.
    Progressed,
    /// 보낼 레코드가 있지만 데이터 연결 큐에 여유가 없어 멈췄다.
    QueueFull,
    /// 보낼 것이 없거나 크레딧·읽기 오류로 멈췄다.
    Idle,
}

impl Delivery {
    fn from_progress(progressed: bool) -> Self {
        if progressed {
            Delivery::Progressed
        } else {
            Delivery::Idle
        }
    }
}

/// 데이터 연결 큐(`limits.control_queue_entries`)에 레코드를 하나 더 넣어도 되는가.
///
/// 펌프는 세션마다 따로 돌며 한 패스에 최대 64레코드를 밀어 넣고, flow 크레딧은
/// 바이트(256 KiB) 기준이라 작은 PTY 레코드 수천 개가 한 연결을 공유하는 128칸
/// 큐를 쉽게 채운다. 데이터 연결은 가득 차도 닫히지 않고(펌프는
/// `ConnHandle::reserve_frame`의 `QueueFull`에 물러난다), 이 검사는 여유분(1/8)을
/// 남기고 멈췄다가 writer가 비운 뒤 이어 보내는 히스테리시스다 — 가득 찬 큐에서
/// 한 칸씩 넣었다 물러났다 하지 않고, 인코딩도 낭비하지 않는다.
fn data_queue_has_room(conn: &crate::state::ConnHandle) -> bool {
    let reserve = conn.tx.max_capacity() / 8;
    conn.tx.capacity() > reserve
}

/// 배달용 저널 읽기 실패가 이 시간 이상 연속되면 일시 오류가 아니다(손상·
/// 권한·삭제). 붙은 뷰를 떼고 `session.replay_required`로 다시 붙으라고
/// 알린다 — 읽기 실패를 그대로 재시도만 하면 뷰는 남은 기록을 영원히 받지
/// 못하고, 완료 이벤트가 없는 프로토콜에서 클라이언트의 "기록 재생 중…"
/// 갇힘이 된다.
const JOURNAL_READ_FAIL_SHED_AFTER: Duration = Duration::from_secs(5);

/// 재생 바이트 예산(`AttachParams.max_replay_bytes`): 보존된 저널이 예산보다
/// 크면 뒤쪽 세그먼트만 재생하도록 시작 seq를 고른다. 활성 파일부터 거꾸로
/// 세그먼트 크기를 더해 예산 안에 드는 가장 오래된 세그먼트 머리(크기
/// 레코드)를 시작점으로 삼는다 — 활성 파일 하나가 예산을 넘어도 그 머리
/// (세그먼트 상한 이하)다. 고른 머리의 seq는 파일의 첫 레코드에서 읽고, 그
/// 자리를 배달 체크포인트로 심어 첫 배달이 앞 세그먼트를 훑지 않게 한다.
///
/// 돌려주는 값은 (시작 seq, 그 앞에서 건너뛴 보존 바이트). 예산 안에 들거나
/// 어떤 단계든 실패하면 `(from, 0)` — 예산 없이 종전대로 재생한다. 128 MiB
/// 저널을 통째로 xterm에 밀어 넣던 앱 시작(수 초의 청킹·끊김)을 몇 MiB로
/// 줄인다; 앞부분은 잘린 헤드와 같은 계약으로 다룬다(UI가 크기를 흔들어
/// TUI가 화면을 다시 그린다).
pub fn bounded_replay_start(
    base: &std::path::Path,
    head: &SegmentSnapshot,
    offsets: &Mutex<std::collections::BTreeMap<u64, SegmentCursor>>,
    from: u64,
    budget: u64,
) -> (u64, u64) {
    if budget == 0 {
        return (from, 0);
    }
    let mut files: Vec<(u64, PathBuf, u64)> = list_closed_segments(base)
        .unwrap_or_default()
        .into_iter()
        .filter(|(index, _)| *index >= head.first_index)
        .filter_map(|(index, path)| {
            std::fs::metadata(&path)
                .ok()
                .map(|meta| (index, path, meta.len()))
        })
        .collect();
    if let Ok(meta) = std::fs::metadata(base) {
        if meta.is_file() {
            files.push((head.active_index, base.to_path_buf(), meta.len()));
        }
    }
    if files.is_empty() {
        return (from, 0);
    }
    // 파일이 하나뿐(레거시 단일 파일 저널, 또는 아직 회전 전의 활성 파일):
    // 세그먼트 머리가 없으니 배달 체크포인트(64레코드마다)로 자른다. 파일을 한
    // 번 훑어 체크포인트를 채운다 — 페이로드는 버리고, 첫 배달이 어차피 같은
    // 훑기를 하므로 비용이 더 들지 않는다(뒤 배달은 이 체크포인트로 빨라진다).
    // 시작 레코드는 크기 레코드가 아니지만 계약은 같다: UI가 live에서 크기를
    // 흔들어 TUI가 다시 그린다. 128 MiB 레거시 저널을 통째로 재생하던 경로다.
    if files.len() == 1 {
        let (index, _, len) = &files[0];
        if *len <= budget {
            return (from, 0);
        }
        let mut cursor = SegmentCursor::head_of(*index);
        let mut next_seq = head.first_seq;
        loop {
            let window = match scan_window_segments(base, head, cursor, next_seq, u64::MAX, 256) {
                Ok(window) => window,
                Err(_) => return (from, 0),
            };
            let Some((last_record, last_cursor)) = window.last() else {
                break;
            };
            cursor = SegmentCursor {
                index: last_cursor.index,
                offset: last_cursor.offset + record_size(last_record),
            };
            next_seq = last_record.seq + 1;
            remember_checkpoints(offsets, &window);
        }
        let pick = offsets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .range(from.saturating_add(1)..)
            .find(|(_, at)| at.index == *index && len.saturating_sub(at.offset) <= budget)
            .map(|(seq, at)| (*seq, at.offset));
        return match pick {
            Some((seq, offset)) => (seq, offset.saturating_sub(HEADER_LEN as u64)),
            None => (from, 0),
        };
    }
    // 뒤에서부터 예산 안에 드는 만큼 포함한다 — 가장 새 파일은 언제나 포함.
    let newest = files.len() - 1;
    let mut total = 0u64;
    let mut chosen = newest;
    for (position, (_, _, bytes)) in files.iter().enumerate().rev() {
        if position != newest && total.saturating_add(*bytes) > budget {
            break;
        }
        total = total.saturating_add(*bytes);
        chosen = position;
    }
    if chosen == 0 {
        return (from, 0);
    }
    let (index, path, _) = &files[chosen];
    let first = match JournalReader::scan_window(path, HEADER_LEN as u64, 1, u64::MAX, 1) {
        Ok(window) => match window.first() {
            Some((record, _)) => record.seq,
            None => return (from, 0),
        },
        Err(_) => return (from, 0),
    };
    if first <= from {
        return (from, 0);
    }
    let skipped: u64 = files[..chosen].iter().map(|(_, _, bytes)| *bytes).sum();
    offsets.lock().unwrap_or_else(|p| p.into_inner()).insert(
        first,
        SegmentCursor {
            index: *index,
            offset: HEADER_LEN as u64,
        },
    );
    (first, skipped)
}

/// 배달 커서를 흐름 원장에 맞춘다(같은 epoch 한정). 같은 epoch에서 실제로
/// 나간 레코드의 기록은 원장뿐이라 원장이 기준이다: 커서가 뒤처졌으면(이미
/// 보낸 레코드를 또 보내려는 상태) 앞으로, 앞섰으면(사이 레코드를 아무도
/// 보내지 않은 상태) 뒤로 맞춘다. 읽기 창의 시작(`from`)이 낡은 커서로
/// 잡혀 보존 창 밖(`HeadTrimmed`)으로 떨어지는 일도 함께 막는다 — 그러면
/// 이미 따라잡은 뷰까지 "뒤처졌다"며 떼어내 UI가 전체 재생을 되풀이한다.
/// 잠금 순서는 `deliver_records`와 같다(flow → views).
fn realign_cursors_with_ledger(session: &Arc<SessionEntry>) {
    let flow = session.flow.lock().unwrap_or_else(|p| p.into_inner());
    let mut views = session.views.lock().unwrap_or_else(|p| p.into_inner());
    for (view_id, view) in views.iter_mut() {
        if flow.view_epoch(view_id).as_deref() != Some(view.epoch.as_str()) {
            continue;
        }
        let Some(sent_through) = flow.sent_through(view_id) else {
            continue;
        };
        let aligned = sent_through.saturating_add(1);
        if view.next_seq == aligned {
            continue;
        }
        if view.next_seq < aligned {
            tracing::debug!(
                session = %session.session_id,
                view = %view_id,
                cursor = view.next_seq,
                sent_through,
                "delivery cursor behind the flow ledger; realigning"
            );
        } else {
            tracing::warn!(
                session = %session.session_id,
                view = %view_id,
                cursor = view.next_seq,
                sent_through,
                "delivery cursor ahead of the flow ledger; rewinding to resend"
            );
        }
        view.next_seq = aligned;
    }
}

/// One delivery pass: send as many in-order records as credits and the data
/// connection queues allow.
fn deliver_pending(state: &Arc<DaemonState>, session: &Arc<SessionEntry>) -> Delivery {
    let mut progressed = false;
    loop {
        let target = session.last_seq.load(Ordering::Acquire);
        realign_cursors_with_ledger(session);
        // Views that still need records, with their data connections.
        let mut pending_views: Vec<(ViewId, ViewEntry, Arc<crate::state::ConnHandle>)> = Vec::new();
        {
            let views = session.views.lock().unwrap_or_else(|p| p.into_inner());
            for (id, view) in views.iter() {
                if view.next_seq > target {
                    continue;
                }
                // 데이터 연결이 없는 뷰는 여기서 걸러진다. 컨트롤 연결만 쓰는
                // 클라이언트(bench·시험 도구의 writer)에게는 정상 상태이므로
                // 펌프가 임의로 떼지 않는다 — 실제로 죽은 데이터 연결은
                // 연결 해체 지점(ipc.rs)에서 정확히 판별해 떼고
                // `session.replay_required`로 알린다(shed_views_of_control).
                let Some(data_conn) = state.data_conn_for(&view.conn) else {
                    continue;
                };
                pending_views.push((id.clone(), view.clone(), data_conn));
            }
        }
        if pending_views.is_empty() {
            return Delivery::from_progress(progressed);
        }
        // 큐가 찬 연결의 뷰는 이번 패스에서 뺀다 — 연결은 살아 있고(데이터 큐는
        // 넘침으로 닫히지 않는다) writer가 비우면 QueueFull 대기 뒤 이어 보낸다.
        pending_views.retain(|(_, _, data_conn)| data_queue_has_room(data_conn));
        if pending_views.is_empty() {
            return if progressed {
                Delivery::Progressed
            } else {
                Delivery::QueueFull
            };
        }

        // 크레딧이 있는 뷰만 읽기 창을 정한다. 크레딧이 막힌 뷰가 `from`을 붙잡으면
        // 보낼 수 있는 뷰가 그 창 밖에 있어도 같은 창만 헛되이 다시 읽는다.
        {
            let flow = session.flow.lock().unwrap_or_else(|p| p.into_inner());
            pending_views.retain(|(id, _, _)| flow.can_send(id));
        }
        if pending_views.is_empty() {
            return Delivery::from_progress(progressed);
        }

        // Batch-read the next records once for this pass.
        let from = pending_views
            .iter()
            .map(|(_, v, _)| v.next_seq)
            .min()
            .unwrap_or(u64::MAX);
        if from > target {
            return Delivery::from_progress(progressed);
        }
        let to = (from + 64).min(target);
        let head = session.journal_segments.snapshot();
        let records = match read_journal_range(
            &session.journal_path,
            &head,
            from,
            to,
            &session.journal_offsets,
        ) {
            Ok(records) => {
                // 읽기가 성공했다 — 연속 실패 창을 지운다.
                session.clear_journal_read_failure();
                records
            }
            Err(RangeError::HeadTrimmed { first_seq }) => {
                // 롤링 저널이 이 뷰가 아직 못 받은 레코드를 지웠다(보존 창보다
                // 뒤처진 뷰). 영원히 재시도하는 대신 뷰를 떼고 다시 붙으라고
                // 알린다 — 재생은 first_seq부터 다시 시작한다.
                if detach_views_behind(state, session, first_seq) == 0 {
                    return Delivery::from_progress(progressed);
                }
                continue;
            }
            Err(RangeError::Other(e)) => {
                // 읽기 실패가 이어지는지 추적한다: 짧은 오류는 다음 패스(수십 ms
                // 뒤)에 재시도한다. 5초 넘게 계속되면 저널이 읽을 수 없는
                // 상태(손상·권한·삭제)다. 읽기 실패를 이 판정 없이 계속
                // 재시도만 하면 붙은 뷰는 남은 기록을 영원히 받지 못하고 아무
                // 이벤트도 갖지 않는다 — 완료 이벤트가 없는 프로토콜에서
                // 클라이언트의 "기록 재생 중…" 갇힘이 된다. 읽기 실패가
                // 이어지면 뷰를 떼고 다시 붙으라고 알린다 — 조용히 멈춘 재생을
                // 남기지 않는다.
                let failing_for = session.note_journal_read_failed();
                if failing_for >= JOURNAL_READ_FAIL_SHED_AFTER {
                    tracing::error!(
                        session = %session.session_id,
                        error = %e,
                        failing_ms = failing_for.as_millis() as u64,
                        "journal unreadable for delivery; shedding attached views for re-attach"
                    );
                    // 창을 초기화한다: 읽기가 계속 실패하고 뷰가 다시 붙으면
                    // 5초 뒤 같은 판정을 다시 내린다.
                    session.clear_journal_read_failure();
                    // U64String::MAX(= i64::MAX)을 first_seq로 넘겨 모든 뷰가
                    // 뒤처진 것으로 만든다(u64::MAX은 U64String 상한을 넘어
                    // 패닉한다). 클라이언트는 이 이벤트의 session_id/view_id만
                    // 쓴다(first_seq는 무시한다).
                    detach_views_behind(state, session, U64String::MAX);
                } else {
                    tracing::warn!(
                        session = %session.session_id,
                        error = %e,
                        "journal read for delivery failed; retrying"
                    );
                }
                return Delivery::from_progress(progressed);
            }
        };
        if records.is_empty() {
            // Views need data (`from <= target`) but the file ends here: the
            // records are still in the writer's 250 ms buffer. Flush them now
            // and re-read once — this is the interactive-latency path (a
            // keystroke echo must not wait out the flush cadence).
            let inner = session
                .journal_inner
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            if let Some(inner) = inner {
                inner.lock().unwrap_or_else(|p| p.into_inner()).flush_now();
                if let Ok(records) = read_journal_range(
                    &session.journal_path,
                    &head,
                    from,
                    to,
                    &session.journal_offsets,
                ) {
                    // 재읽기가 성공했다 — 연속 실패 창을 지운다.
                    session.clear_journal_read_failure();
                    if !records.is_empty() {
                        let sent = deliver_records(state, session, &records, target);
                        return Delivery::from_progress(progressed || sent);
                    }
                }
            }
            return Delivery::from_progress(progressed);
        }

        if !deliver_records(state, session, &records, target) {
            // 아무것도 못 보냈다(큐가 찼거나, 연결이 닫히는 중이거나, 뷰가 다시
            // 붙었다): 같은 창을 곧바로 다시 읽으며 돌지 말고 펌프 대기로 넘긴다.
            if progressed {
                return Delivery::Progressed;
            }
            let queue_full = pending_views
                .iter()
                .any(|(_, _, data_conn)| !data_queue_has_room(data_conn));
            return if queue_full {
                Delivery::QueueFull
            } else {
                Delivery::Idle
            };
        }
        progressed = true;
    }
}

/// 보존 창보다 뒤처진 뷰(`next_seq < first_seq`)를 떼어내고
/// `session.replay_required`로 다시 붙으라고 알린다. 떼어낸 수를 돌려준다.
fn detach_views_behind(
    state: &Arc<DaemonState>,
    session: &Arc<SessionEntry>,
    first_seq: u64,
) -> usize {
    let behind: Vec<ViewEntry> = {
        let mut views = session.views.lock().unwrap_or_else(|p| p.into_inner());
        let ids: Vec<ViewId> = views
            .iter()
            .filter(|(_, view)| view.next_seq < first_seq)
            .map(|(id, _)| id.clone())
            .collect();
        ids.iter().filter_map(|id| views.remove(id)).collect()
    };
    if behind.is_empty() {
        return 0;
    }
    {
        let mut owner = session.owner_view.lock().unwrap_or_else(|p| p.into_inner());
        if owner
            .as_ref()
            .is_some_and(|owner| behind.iter().any(|view| &view.view_id == owner))
        {
            *owner = None;
        }
    }
    {
        let mut flow = session.flow.lock().unwrap_or_else(|p| p.into_inner());
        for view in &behind {
            flow.detach_view(&view.view_id);
        }
    }
    for view in &behind {
        tracing::info!(
            session = %session.session_id,
            view = %view.view_id,
            next_seq = view.next_seq,
            first_seq,
            "view fell behind the retained journal head; re-attach required"
        );
        state.broadcast_control(
            term_contracts::rpc::RpcEventKind::SessionReplayRequired,
            serde_json::json!({
                "session_id": session.session_id,
                "view_id": view.view_id,
                "epoch": view.epoch,
                "first_seq": U64String::new(first_seq).expect("seq fits i64"),
            }),
        );
    }
    state.bump_revision();
    if session
        .views
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty()
    {
        if let Some(entry) = state.workload_entry(&session.workload_id) {
            entry.lock().unwrap_or_else(|p| p.into_inner()).connection =
                term_contracts::state::TerminalConnection::Detached;
        }
    }
    behind.len()
}

/// 죽은 데이터 연결이 붙들고 있던 뷰를 떼고 `session.replay_required`로
/// 다시 붙으라고 알린다 — 조용히 멈춘 재생(클라이언트의 "기록 재생 중…"
/// 갇힘)을 남기지 않는다. 데이터 연결의 해체 지점(ipc.rs)에서 호출한다.
///
/// 왜 해체 지점인가 — 펌프의 스캔에서 `data_conn_for`이 `None`인 뷰는
/// "데이터 연결이 죽었다"가 아니라 "데이터 연결이 없다"다. 컨트롤 연결만
/// 쓰는 클라이언트(bench·시험 도구의 writer attach)에게는 없는 게 정상
/// 상태라 펌프가 임의로 떼면 안 된다. 반면 해체 지점에서는 그 연결이
/// 방금까지 살아 있었다는 것이 보증되므로(등록부에서 지워지는 순간이다)
/// 그 컨트롤의 뷰는 프레임을 받을 길이 영원히 사라졌음이 확실하다 —
/// 바로 떼고 알린다.
///
/// 경쟁 분석 — (1) 컨트롤 연결이 먼저 죽은 경우 `detach_all_views_of`가
/// 뷰를 이미 정리했으므로 여기서 건드릴 것이 없다(순서가 바뀌어도
/// 어느 쪽이 먼저 끝나든 뷰는 정확히 한 번 떨어진다). (2) 클라이언트가
/// 재접속 중인 경우 전체 재접속은 새 컨트롤 연결(새 data 토큰)로
/// 이뤄지므로 죽은 연결의 컨트롤 id로 새 뷰가 생기지 않는다. (3) 같은
/// view id가 새 epoch으로 다시 붙는 경쟁은 [`shed_view`]의 epoch 검사와
/// 원장(`flow.detach_view`)의 epoch 일치 검사가 막는다(deliver_records와
/// 같은 판정). 컨트롤은 살아 있으므로 `session.replay_required`를 받은
/// UI는 그 pane을 다시 붙인다(session_id/view_id만 쓴다).
pub fn shed_views_of_control(state: &Arc<DaemonState>, control: &ConnectionId) {
    let sessions: Vec<Arc<SessionEntry>> = state
        .sessions
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .cloned()
        .collect();
    for session in sessions {
        // 이 컨트롤 연결이 붙인 뷰들을 (원장 검사와 함께) 모은다.
        let views_of_conn: Vec<(ViewId, String, u64)> = {
            let views = session.views.lock().unwrap_or_else(|p| p.into_inner());
            views
                .iter()
                .filter(|(_, view)| &view.conn == control)
                .map(|(id, view)| (id.clone(), view.epoch.clone(), view.next_seq))
                .collect()
        };
        for (view_id, epoch, next_seq) in views_of_conn {
            {
                let mut flow = session.flow.lock().unwrap_or_else(|p| p.into_inner());
                // 모은 사이 같은 view id가 새 epoch으로 다시 붙었으면 원장은
                // 새 재생의 것이다 — 건드리지 않는다.
                if flow.view_epoch(&view_id).as_deref() != Some(epoch.as_str()) {
                    continue;
                }
                flow.detach_view(&view_id);
            }
            if shed_view(state, &session, &view_id, &epoch) {
                tracing::info!(
                    session = %session.session_id,
                    view = %view_id,
                    next_seq,
                    "view's data connection closed; re-attach required"
                );
                state.broadcast_control(
                    term_contracts::rpc::RpcEventKind::SessionReplayRequired,
                    serde_json::json!({
                        "session_id": session.session_id,
                        "view_id": view_id,
                        "epoch": epoch,
                        "first_seq": U64String::new(next_seq).expect("seq fits i64"),
                    }),
                );
            }
        }
    }
}

/// Deliver one batch of journal records to every pending view (reserve a
/// data-queue slot, account the credit, then send through the reservation;
/// a full queue or exhausted global budget backs off without advancing the
/// cursor). Returns true when at least one record moved.
fn deliver_records(
    state: &Arc<DaemonState>,
    session: &Arc<SessionEntry>,
    records: &[term_pty::journal::JournalRecord],
    target: u64,
) -> bool {
    let pending_views: Vec<(ViewId, ViewEntry, Arc<crate::state::ConnHandle>)> = {
        let views = session.views.lock().unwrap_or_else(|p| p.into_inner());
        views
            .iter()
            .filter_map(|(id, view)| {
                (view.next_seq <= target)
                    .then(|| {
                        state
                            .data_conn_for(&view.conn)
                            .map(|data_conn| (id.clone(), view.clone(), data_conn))
                    })
                    .flatten()
            })
            .collect()
    };
    if pending_views.is_empty() {
        return false;
    }
    let mut sent_any = false;
    for (view_id, mut view, data_conn) in pending_views {
        let mut flow = session.flow.lock().unwrap_or_else(|p| p.into_inner());
        // 뷰 목록을 읽은 뒤 같은 view id로 다시 붙었으면(새 epoch) 이 사본의
        // 커서·epoch는 낡았다. 옛 seq를 새 원장에 기록하면 새 재생이 영원히
        // "send stream gap"으로 막히므로 다음 패스에 새 항목으로 다시 읽는다.
        if flow.view_epoch(&view_id).as_deref() != Some(view.epoch.as_str()) {
            continue;
        }
        // 원장이 커서보다 앞서 있으면(같은 epoch에서 이미 나간 레코드) 커서를
        // 원장에 맞춘다. 커서가 뒤처진 채 남으면 다음 패스가 이미 보낸 seq를
        // 다시 넣고 원장이 "send stream gap: expected N+1, sent N"으로 거부한다
        // — 그 뷰는 레코드를 영영 못 받고 UI는 막힘 감시로 전체 재생을
        // 되풀이한다(화면 깜빡임·입력 차단). 원장이 보냈다고 기록한 레코드는
        // permit으로 이미 데이터 큐에 들어갔으므로 건너뛰는 것이 맞다.
        // 반대로 커서가 원장보다 앞서면 그 사이 레코드는 아무도 보내지 않은
        // 것이다 — 원장 다음 seq로 되돌려 빠짐없이 이어 보낸다. 어느 쪽이든
        // 같은 epoch에서 실제로 나간 것의 기록은 원장뿐이라 원장이 기준이다.
        if let Some(sent_through) = flow.sent_through(&view_id) {
            let aligned = sent_through.saturating_add(1);
            if view.next_seq != aligned {
                if view.next_seq < aligned {
                    tracing::debug!(
                        session = %session.session_id,
                        view = %view_id,
                        cursor = view.next_seq,
                        sent_through,
                        "delivery cursor behind the flow ledger; realigning"
                    );
                } else {
                    tracing::warn!(
                        session = %session.session_id,
                        view = %view_id,
                        cursor = view.next_seq,
                        sent_through,
                        "delivery cursor ahead of the flow ledger; rewinding to resend"
                    );
                }
                view.next_seq = aligned;
            }
        }
        // 이 뷰를 이번 패스에서 버린다(연결이 죽었거나 원장이 어긋났다).
        let mut dead = false;
        // 원장이 어긋나 버린 뷰: 데이터 연결은 살아 있어 전송 복구가 다시 붙이지
        // 않으므로 UI에 그 pane만 다시 붙으라고 알린다.
        let mut resync = false;
        for record in records {
            if record.seq < view.next_seq || record.seq > target {
                continue;
            }
            if !flow.can_send(&view_id) {
                break;
            }
            // 큐 여유는 미리 본다(인코딩 낭비를 막는 빠른 길). 실제 보호는 아래
            // reserve_frame의 QueueFull이 맡는다 — 여유 검사와 전송 사이에 같은
            // 연결을 쓰는 다른 세션의 펌프가 큐를 채울 수 있으므로.
            if !data_queue_has_room(&data_conn) {
                break;
            }
            let (kind, data_b64, raw_len, cols, rows) = match record.kind {
                term_pty::journal::JournalRecordKind::Output => {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&record.payload);
                    (
                        TerminalFrameKind::Output,
                        b64,
                        record.payload.len() as u32,
                        None,
                        None,
                    )
                }
                term_pty::journal::JournalRecordKind::Resize => {
                    let (cols, rows) = decode_resize(&record.payload);
                    (
                        TerminalFrameKind::Resize,
                        String::new(),
                        0,
                        Some(cols),
                        Some(rows),
                    )
                }
            };
            let output = SessionOutput {
                session_id: session.session_id.clone(),
                epoch: view.epoch.clone(),
                seq: U64String::new(record.seq).expect("seq fits i64"),
                kind,
                data_b64,
                raw_len,
                cols,
                rows,
            };
            let frame = match crate::state::encode_event(
                term_contracts::rpc::RpcEventKind::SessionOutput,
                serde_json::to_value(&output).unwrap_or(serde_json::Value::Null),
            ) {
                Ok(f) => f,
                Err(_) => break,
            };
            // Reserve capacity before accounting, then commit through the
            // permit. A full queue changes neither the ledger nor the cursor;
            // a rejected ledger entry drops the permit without sending bytes.
            let permit = match data_conn.reserve_frame() {
                Ok(permit) => permit,
                // 역압: 연결은 살아 있다. 커서·원장 모두 그대로 — 다음 패스가
                // 같은 seq를 다시 넣는다.
                Err(crate::state::FrameBackoff::QueueFull) => break,
                // 연결이 닫혔다: 데이터 연결은 control 연결과 달리 닫혀도 뷰
                // 정리가 따로 없으므로 여기서 뗀다.
                Err(crate::state::FrameBackoff::Closed) => {
                    dead = true;
                    break;
                }
            };
            match flow.record_sent(
                &view_id,
                SentRecord {
                    seq: record.seq,
                    raw_len,
                },
            ) {
                // 전역 원시·전송 예산은 모든 세션의 뷰가 나눠 쓴다: can_send와
                // 여기 사이에 다른 세션의 펌프가 남은 예산을 가져갈 수 있다.
                // 일시적 역압이므로 뷰를 떼지 않는다 — permit을 버려 칸을
                // 돌려주고 커서를 그대로 둔 채 다음 패스에 같은 seq를 다시 보낸다.
                Err(term_pty::flow::FlowError::RawBudgetExhausted { .. }) => break,
                Err(term_pty::flow::FlowError::TransportBudgetExhausted { .. }) => break,
                Err(e) => {
                    tracing::warn!(
                        session = %session.session_id,
                        view = %view_id,
                        epoch = %view.epoch,
                        seq = record.seq,
                        error = %e,
                        "flow record_sent rejected; shedding the view for re-attach"
                    );
                    // 원장이 이 (view, epoch)의 다음 seq를 받지 않는다 — 이미
                    // 어긋난 상태다. 버티면 매 패스 거부만 반복하므로 뷰를 떼고
                    // UI에 다시 붙으라고 알린다(조용히 멈춘 pane을 남기지 않는다).
                    dead = true;
                    resync = true;
                    break;
                }
                // B16: announce the blocked transition exactly once per
                // crossing (02-runner §4: 256 KiB 미소비 시 view 전송 중지를
                // 알린다 — `session.flow_blocked`). UI는 `blocked` 필드로
                // 배지를 켜고 끈다.
                Ok(Some(FlowTransition::Blocked { view: blocked })) => {
                    state.broadcast_control(
                        term_contracts::rpc::RpcEventKind::SessionFlowBlocked,
                        serde_json::json!({
                            "session_id": session.session_id,
                            "view_id": blocked,
                            "epoch": view.epoch,
                            "blocked": true,
                        }),
                    );
                }
                Ok(Some(FlowTransition::Unblocked { .. })) | Ok(None) => {}
            }
            permit.send(frame);
            view.next_seq = record.seq + 1;
            sent_any = true;
        }
        if dead {
            // 이 (view, epoch)은 더 보낼 수 없다. views·원장·소유자를 정리해
            // UI의 재접속(새 epoch)이 막힘 없이 시작되게 한다.
            flow.detach_view(&view_id);
            drop(flow);
            let shed = shed_view(state, session, &view_id, &view.epoch);
            if shed && resync {
                // UI는 `session.replay_required`에 그 pane만 새 epoch로 다시
                // 붙이는 것으로 답한다(detach_views_behind와 같은 경로).
                state.broadcast_control(
                    term_contracts::rpc::RpcEventKind::SessionReplayRequired,
                    serde_json::json!({
                        "session_id": session.session_id,
                        "view_id": view_id,
                        "epoch": view.epoch,
                        "first_seq": U64String::new(view.next_seq).expect("seq fits i64"),
                    }),
                );
            }
            continue;
        }
        // Write the cursor back.
        session
            .views
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(view_id)
            .and_modify(|v| {
                // 그사이 다시 붙은 뷰(새 epoch)의 재생 시작점을 옛 커서로 덮지 않는다.
                if v.epoch == view.epoch {
                    v.next_seq = view.next_seq;
                }
            });
    }
    sent_any
}

/// 펌프가 더 이상 보낼 수 없는 뷰를 등록부에서 뗀다(데이터 연결이 닫혔거나
/// 원장이 어긋난 경우). control 연결 해체와 달리 데이터 연결에는 뷰 정리가
/// 따로 없으므로 펌프가 직접 한다 — 놔두면 유령 뷰가 매 패스 전송을 시도해
/// 거부만 반복한다. 원장(`flow.detach_view`)은 호출자가 이미 풀었다.
/// 뗐으면 true, 그사이 같은 view id가 새 epoch으로 다시 붙어 건드리지 않았으면 false.
fn shed_view(
    state: &Arc<DaemonState>,
    session: &Arc<SessionEntry>,
    view_id: &ViewId,
    epoch: &str,
) -> bool {
    {
        let mut views = session.views.lock().unwrap_or_else(|p| p.into_inner());
        let current = views.get(view_id).is_some_and(|v| v.epoch == epoch);
        if !current {
            // 그사이 같은 view id가 새 epoch으로 다시 붙었다 — 새 재생을 건드리지 않는다.
            return false;
        }
        views.remove(view_id);
        let mut owner = session.owner_view.lock().unwrap_or_else(|p| p.into_inner());
        if owner.as_ref() == Some(view_id) {
            *owner = None;
        }
    }
    state.bump_revision();
    // 뷰가 모두 사라진 종료 세션도 회수 대상이다.
    if session
        .views
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty()
    {
        state.retire_session_if_cold(&session.session_id);
    }
    true
}

fn decode_resize(payload: &[u8]) -> (u16, u16) {
    if payload.len() >= 4 {
        (
            u16::from_le_bytes([payload[0], payload[1]]),
            u16::from_le_bytes([payload[2], payload[3]]),
        )
    } else {
        (0, 0)
    }
}

/// 체크포인트 간격(레코드 수). 배달 창 크기와 같은 64로 둔다 — 커서가
/// 배치마다 정확히 체크포인트에 놓이므로 스캔 낭비가 없다(맵 엔트리는
/// 64레코드당 16바이트로 여전히 유계다). 조회는 `from` 이하의 가장 가까운
/// 체크포인트로 seek한 뒤 최대 63레코드를 읽고 건너뛴다.
const RANGE_CHECKPOINT_EVERY: u64 = 64;
/// 한 번의 배달 창이 담는 최대 레코드 수(기존 배치 크기 유지).
const RANGE_WINDOW_RECORDS: usize = 64;

/// `read_journal_range` 실패 분류: 헤드가 잘려 되살릴 수 없는 요청(뷰를
/// 떼어 다시 붙인다)과 그 밖의 일시적 오류(다음 틱에 재시도)를 구분한다.
#[derive(Debug)]
enum RangeError {
    HeadTrimmed { first_seq: u64 },
    Other(String),
}

impl From<JournalFlowError> for RangeError {
    fn from(error: JournalFlowError) -> Self {
        match error {
            JournalFlowError::HeadTrimmed { first_seq } => RangeError::HeadTrimmed { first_seq },
            other => RangeError::Other(other.to_string()),
        }
    }
}

/// 디스크 프레이밍: 레코드 전체 크기 = 4 + RECORD_FRAMING_LEN + payload.
fn record_size(record: &term_pty::journal::JournalRecord) -> u64 {
    (4 + term_pty::journal::RECORD_FRAMING_LEN + record.payload.len()) as u64
}

fn read_journal_range(
    path: &std::path::Path,
    head: &SegmentSnapshot,
    from: u64,
    to: u64,
    offsets: &Mutex<std::collections::BTreeMap<u64, SegmentCursor>>,
) -> Result<Vec<term_pty::journal::JournalRecord>, RangeError> {
    if from == 0 || to < from {
        return Ok(Vec::new());
    }
    if from < head.first_seq {
        return Err(RangeError::HeadTrimmed {
            first_seq: head.first_seq,
        });
    }
    // 잘린 헤드 아래의 체크포인트는 지워진 파일을 가리킨다 — 버린다.
    {
        let mut cursor = offsets.lock().unwrap_or_else(|p| p.into_inner());
        if cursor
            .keys()
            .next()
            .is_some_and(|first| *first < head.first_seq)
        {
            *cursor = cursor.split_off(&head.first_seq);
        }
    }
    // 증분 경로: `from` 이하의 가장 가까운 체크포인트에서 이어서 스캔한다.
    // 체크포인트는 (seq, 세그먼트 커서)다. 시드된 연속성 검증으로 중간
    // 손상도 여전히 잡힌다.
    {
        let checkpoint = offsets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .range(..=from)
            .next_back()
            .map(|(seq, cursor)| (*seq, *cursor));
        if let Some((checkpoint_seq, checkpoint_cursor)) = checkpoint {
            // 창 상한은 "체크포인트→from 사이 스킵(≤63) + 배달 창(64)".
            // 스킵분을 상한에서 빼면 창이 from에 못 미쳐 빈 결과를 낸다.
            let max_scan = RANGE_CHECKPOINT_EVERY as usize + RANGE_WINDOW_RECORDS;
            match scan_window_segments(path, head, checkpoint_cursor, checkpoint_seq, to, max_scan)
            {
                Ok(window) => {
                    remember_checkpoints(offsets, &window);
                    let mut ranged: Vec<_> = window
                        .into_iter()
                        .map(|(record, _)| record)
                        .filter(|record| record.seq >= from)
                        .collect();
                    ranged.truncate(RANGE_WINDOW_RECORDS);
                    return Ok(ranged);
                }
                Err(JournalFlowError::HeadTrimmed { first_seq }) => {
                    return Err(RangeError::HeadTrimmed { first_seq });
                }
                Err(e) => {
                    // 드물게 손상이 이번 창에서 처음 관측됐거나 회전 경주에
                    // 걸린 경우 — 전체 경로가 헤드부터 다시 본다.
                    tracing::debug!(error = %e, "window scan fell back to full scan");
                }
            }
        }
    }

    // 전체 경로: 보존된 첫 세그먼트의 머리부터 창을 연달아 읽으며
    // [from..=to]를 모으고 체크포인트를 채운다. 매 배치가 파일 전체를
    // 재검증하던 이전 동작(I04 위험의 실체)을 대체한다.
    let mut out = Vec::new();
    let mut cursor = SegmentCursor::head_of(head.first_index);
    let mut next_seq = head.first_seq;
    loop {
        let window = scan_window_segments(path, head, cursor, next_seq, u64::MAX, 256)?;
        if window.is_empty() {
            break;
        }
        remember_checkpoints(offsets, &window);
        for (record, _) in &window {
            if record.seq > to {
                return Ok(out);
            }
            if record.seq >= from {
                out.push(record.clone());
                if out.len() >= RANGE_WINDOW_RECORDS {
                    return Ok(out);
                }
            }
        }
        let (last_record, last_cursor) = window.last().expect("non-empty window");
        // 다음 창은 마지막 레코드의 끝에서 시작한다(닫힌 세그먼트의 끝이면
        // 스캐너가 다음 세그먼트의 머리로 넘어간다).
        cursor = SegmentCursor {
            index: last_cursor.index,
            offset: last_cursor.offset + record_size(last_record),
        };
        next_seq = last_record.seq + 1;
    }
    Ok(out)
}

/// 창의 레코드 커서를 체크포인트로 기록한다(64레코드마다).
fn remember_checkpoints(
    offsets: &Mutex<std::collections::BTreeMap<u64, SegmentCursor>>,
    window: &[(term_pty::journal::JournalRecord, SegmentCursor)],
) {
    let mut cursor = offsets.lock().unwrap_or_else(|p| p.into_inner());
    for (record, at) in window {
        if record.seq % RANGE_CHECKPOINT_EVERY == 0 {
            cursor.insert(record.seq, *at);
        }
    }
}

/// Apply an ACK received on a data connection: route it to every view of
/// `session` owned by the acknowledging control connection.
pub fn apply_ack(
    state: &Arc<DaemonState>,
    session: &Arc<SessionEntry>,
    control: &ConnectionId,
    epoch: &str,
    through_seq: u64,
) -> Result<(), term_contracts::RpcError> {
    let views = session.views.lock().unwrap_or_else(|p| p.into_inner());
    let mine: Vec<ViewId> = views
        .iter()
        .filter(|(_, v)| &v.conn == control && v.epoch == epoch)
        .map(|(id, _)| id.clone())
        .collect();
    drop(views);
    if mine.is_empty() {
        // Old epoch or detached view: ignored per spec §4.
        return Ok(());
    }
    let mut unblocked: Vec<ViewId> = Vec::new();
    let mut flow = session.flow.lock().unwrap_or_else(|p| p.into_inner());
    let mut protocol_error: Option<term_pty::flow::FlowError> = None;
    for view in mine {
        match flow.on_ack(&view, epoch, through_seq) {
            Err(e) => protocol_error = Some(e),
            // 크레딧이 돌아와 막힘이 풀린 뷰가 있다 — 배지가 켜진 채 남지
            // 않게 `blocked: false`로 알린다(B16의 짝).
            Ok(term_pty::flow::AckOutcome::Advanced {
                unblocked_views, ..
            }) => {
                unblocked.extend(unblocked_views);
            }
            Ok(_) => {}
        }
    }
    session.wake();
    for view in unblocked {
        state.broadcast_control(
            term_contracts::rpc::RpcEventKind::SessionFlowBlocked,
            serde_json::json!({
                "session_id": session.session_id,
                "view_id": view,
                "epoch": epoch,
                "blocked": false,
            }),
        );
    }
    match protocol_error {
        // Any well-formed ACK advanced; a malformed one surfaces as a
        // protocol error so the client can be told (spec §4).
        Some(e) => Err(term_contracts::RpcError::new(
            term_contracts::ErrorCode::InvalidArgument,
            format!("ack rejected: {e}"),
        )),
        None => Ok(()),
    }
}

/// Helper for tests/IPC: a synchronous byte-stream client connection (UDS /
/// named pipe) usable as a `GateStream` on both platforms.
#[cfg(unix)]
pub struct SyncStream {
    inner: std::os::unix::net::UnixStream,
}

/// Windows: the named-pipe client is a plain `File`, and `ReadFile` on a
/// pipe has no per-call timeout. Reads therefore go through a dedicated
/// reader thread and a channel so `set_read_timeout` is real
/// (`recv_timeout` → `TimedOut`) — otherwise the helper's RELEASE wait and
/// the hook's 3 s cap were dead code on Windows and a wedged-but-accepting
/// daemon stalled the CLI. The thread exits on EOF/error (the peer closing
/// the pipe); a stream dropped mid-read leaves it parked in `ReadFile`
/// until then, which is fine for the short-lived helper/hook processes.
#[cfg(windows)]
pub struct SyncStream {
    inner: std::fs::File,
}

#[cfg(unix)]
impl SyncStream {
    pub fn connect(endpoint: &str) -> std::io::Result<Self> {
        use std::os::unix::net::UnixStream;
        let stream = UnixStream::connect(endpoint)?;
        stream.set_nonblocking(false)?;
        Ok(SyncStream { inner: stream })
    }

    /// Per-read timeout so a deadline-driven reader (`GateClient` /
    /// `DeadlineReader`) can enforce its deadline: a blocking UDS read
    /// otherwise never returns on a connected-but-silent peer. Used by
    /// callers that retry `WouldBlock`/`TimedOut` (the gate protocol) and by
    /// the hook subcommand, which caps each read at 3 s so a wedged daemon
    /// never stalls the CLI (02 §8.3).
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.inner.set_read_timeout(timeout)
    }
}

#[cfg(windows)]
impl SyncStream {
    pub fn connect(endpoint: &str) -> std::io::Result<Self> {
        // Named pipe client: retry until the gate timeout window closes.
        // Between the daemon's `create` of the server instance and its
        // `connect()` call the pipe exists but is not listening — opens fail
        // with ERROR_PIPE_BUSY — and before creation they fail with
        // ERROR_FILE_NOT_FOUND. Both are transient here.
        let deadline = std::time::Instant::now()
            + Duration::from_millis(term_contracts::gate::GATE_TIMEOUT_MS - 500);
        let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, "connect never succeeded");
        while std::time::Instant::now() < deadline {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(endpoint)
            {
                Ok(file) => return Ok(SyncStream { inner: file }),
                Err(e) => {
                    last = e;
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
        Err(last)
    }

    /// Windows named-pipe client reads are plain `File` reads. A dedicated
    /// reader thread (the 9509a10 experiment) parks a synchronous ReadFile
    /// on the file object, which then blocks WriteFile on every duplicated
    /// handle of the same pipe — the helper could never send its `Started`
    /// frame after awaiting RELEASE. Single-threaded blocking reads keep
    /// the gate's lock-step protocol correct; the deadline is enforced by
    /// the daemon side's async pipe adapter.
    pub fn set_read_timeout(&self, _timeout: Option<Duration>) -> std::io::Result<()> {
        Ok(())
    }

    /// Wrap an already-connected full-duplex pipe file (the daemon side of
    /// the gate after the sync-server accept).
    pub fn from_file(file: std::fs::File) -> Self {
        SyncStream { inner: file }
    }

    /// Duplicate the handle for a second owner of the same pipe.
    pub fn try_clone(&self) -> std::io::Result<Self> {
        Ok(SyncStream {
            inner: self.inner.try_clone()?,
        })
    }
}

/// O16's named-pipe rework dropped this along with the Windows reader
/// thread — the helper/hook gate clients on Unix still go through
/// `GateClient<S: GateStream>`, which requires `Read`.
#[cfg(unix)]
impl Read for SyncStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

#[cfg(windows)]
impl Read for SyncStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for SyncStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Blocking adapter over an async duplex stream so the sync gate protocol
/// (`GateServer`) can drive a tokio named-pipe stream from a blocking
/// thread. `Handle::block_on` is legal off the async workers.
pub struct BlockingStream<S> {
    inner: S,
    handle: tokio::runtime::Handle,
}

impl<S> BlockingStream<S> {
    pub fn new(inner: S, handle: tokio::runtime::Handle) -> Self {
        BlockingStream { inner, handle }
    }
}

impl<S: tokio::io::AsyncRead + Unpin> Read for BlockingStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // No per-read timeout stepping: cancelling a pending tokio pipe read
        // (the 9509a10 `timeout(50ms, …)` step) abandons the overlapped IRP
        // and wedges the pipe's shared state — after that, neither reads nor
        // writes on the accepted gate ever complete ("no start report from
        // helper" on every managed launch). A blocking read parks until
        // real data, EOF, or error; the launch thread accepts that a
        // connects-then-stalls helper pins it (R1-known trade-off, the same
        // one the pre-9509a10 gate made).
        let inner = &mut self.inner;
        self.handle
            .block_on(async move { tokio::io::AsyncReadExt::read(inner, buf).await })
            .map_err(|e| std::io::Error::other(e.to_string()))
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> Write for BlockingStream<S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.handle
            .block_on(tokio::io::AsyncWriteExt::write(&mut self.inner, buf))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.handle
            .block_on(tokio::io::AsyncWriteExt::flush(&mut self.inner))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_stream_connect_missing_endpoint_fails_cleanly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("no-such.sock");
        let result = SyncStream::connect(missing.to_str().expect("utf8 path"));
        assert!(result.is_err());
    }

    /// The gate read path is deliberately NON-cancellable: cancelling a
    /// pending tokio named-pipe read (the 50ms-step experiment) abandons the
    /// overlapped IRP and permanently wedges the pipe — every managed
    /// launch died with "no start report from helper". On an in-memory
    /// duplex the blocking read simply waits for bytes (proving no
    /// cancellation path remains).
    #[test]
    fn blocking_stream_read_delivers_late_bytes_without_cancellation() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let (ours, mut theirs) = tokio::io::duplex(64);
        let mut stream = BlockingStream::new(ours, rt.handle().clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            rt.block_on(async {
                tokio::io::AsyncWriteExt::write_all(&mut theirs, b"hello")
                    .await
                    .unwrap();
            });
        });
        let mut buf = [0u8; 8];
        let n = stream.read(&mut buf).expect("late bytes readable");
        assert_eq!(&buf[..n], b"hello");
    }
}

#[cfg(test)]
pub(crate) mod journal_release_tests {
    use super::*;

    /// 레지스트리에 들어가는 것과 같은 모양의 `SessionEntry` + 저널 핸들.
    pub(crate) fn session_entry(
        dir: &std::path::Path,
        session_id: &SessionId,
    ) -> (Arc<SessionEntry>, Arc<Mutex<JournalInner>>, PathBuf) {
        let path = dir.join(format!("{session_id}.mtj"));
        let uuid = uuid::Uuid::parse_str(session_id.as_str()).expect("session id is a uuid");
        let writer = JournalWriter::open(&path, uuid).expect("journal open");
        let segments = Arc::new(SegmentTracker::frozen(writer.segment_snapshot()));
        let storage =
            Arc::new(term_storage::Storage::open(dir.join("meta.db3")).expect("storage open"));
        let inner = Arc::new(Mutex::new(JournalInner {
            writer,
            storage,
            session_id: session_id.clone(),
            last_progress_ms: 0,
            last_persisted: (0, 0),
            finished: false,
        }));
        let session = Arc::new(SessionEntry {
            session_id: session_id.clone(),
            workload_id: WorkloadId::generate(),
            journal_path: path.clone(),
            journal_limit: AtomicU64::new(1 << 20),
            epoch: Mutex::new(uuid::Uuid::new_v4().to_string()),
            owner_view: Mutex::new(None),
            views: Mutex::new(HashMap::new()),
            last_seq: AtomicU64::new(0),
            journal_inner: Mutex::new(Some(Arc::clone(&inner))),
            journal_offsets: Mutex::new(std::collections::BTreeMap::new()),
            journal_segments: segments,
            journal_read_failing_since: Mutex::new(None),
            recent_resizes: Mutex::new(Vec::new()),
            resize_notify: tokio::sync::Notify::new(),
            resize_in_flight: AtomicBool::new(false),
            flow: Mutex::new(FlowController::with_budget(
                term_pty::flow::GlobalOutputBudget::shared(),
            )),
            wake_tx: Mutex::new(()),
            wake_cv: Condvar::new(),
            wake_pending: AtomicBool::new(false),
            pump_stop: AtomicBool::new(false),
            pump_alive: AtomicBool::new(false),
            pump_ctl: Mutex::new(()),
            size: Mutex::new((80, 24)),
            actor_finalized: AtomicBool::new(false),
        });
        (session, inner, path)
    }

    /// L2: 종료 세션의 저널 핸들 해제. `journal_inner`가 비워지고 버퍼에만
    /// 있던 꼬리가 즉시 디스크에 닿는다 — 이 참조를 남겨 두면 `JournalInner`
    /// 의 Drop이 영영 돌지 않아 세션마다 64 KiB 버퍼 + fd 하나가 샜다.
    #[test]
    fn release_journal_clears_the_handle_and_flushes_the_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session_id = SessionId::generate();
        let (session, inner, path) = session_entry(dir.path(), &session_id);

        // 초기 크기 + 출력 한 건: 250 ms 캐이던스라 아직 버퍼에만 있다.
        {
            let mut guard = inner.lock().unwrap_or_else(|p| p.into_inner());
            guard.writer.append_resize(80, 24).expect("resize record");
            guard
                .writer
                .append_output(b"tail-bytes")
                .expect("output record");
        }
        assert_eq!(
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
            0,
            "아직 아무것도 flush되지 않았다"
        );

        assert!(release_journal(&session), "핸들을 실제로 해제했다");
        assert!(
            session
                .journal_inner
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_none(),
            "종료 후 journal_inner는 None이다"
        );
        assert!(
            !release_journal(&session),
            "두 번째 호출은 할 일이 없다(멱등)"
        );

        // 꼬리까지 디스크에 있고 스캔이 깨끗하다(잘린 꼬리 아님).
        let scanned = term_pty::journal::JournalReader::open(&path).expect("scan");
        assert_eq!(scanned.status(), term_pty::journal::ScanStatus::Ok);
        assert_eq!(scanned.last_seq(), 2, "두 레코드 모두 디스크에 있다");

        // 마지막 Arc(테스트 소유)를 놓으면 JournalInner::drop이 파일을 닫는다.
        drop(inner);
        let scanned = term_pty::journal::JournalReader::open(&path).expect("scan after drop");
        assert_eq!(scanned.status(), term_pty::journal::ScanStatus::Ok);
    }
}

#[cfg(test)]
mod data_queue_tests {
    use super::data_queue_has_room;
    use crate::state::{CoalescedOut, ConnHandle, ConnRole, FrameBackoff};
    use term_contracts::ids::ConnectionId;
    use tokio::sync::{mpsc, Notify};

    fn handle(capacity: usize) -> (ConnHandle, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(capacity);
        (
            ConnHandle {
                conn_id: ConnectionId::generate(),
                role: ConnRole::Data,
                linked_control: None,
                tx,
                coalesced: std::sync::Mutex::new(CoalescedOut::default()),
                closed: std::sync::atomic::AtomicBool::new(false),
                close_wake: Notify::new(),
            },
            rx,
        )
    }

    /// 펌프는 큐의 1/8을 남기고 멈춘다 — 128칸 큐면 112개까지만 넣고, writer가
    /// 비운 뒤 이어 보낸다(가득 찬 큐에서 한 칸씩 넣었다 물러나지 않는 히스테리시스).
    #[test]
    fn pump_stops_before_the_data_queue_overflows() {
        let (conn, mut rx) = handle(128);
        let mut queued = 0;
        while data_queue_has_room(&conn) {
            assert!(
                conn.send(vec![b'x']),
                "여유가 있다고 본 큐는 넣기에 성공한다"
            );
            queued += 1;
        }
        assert_eq!(queued, 112);
        assert!(!conn.closed.load(std::sync::atomic::Ordering::Acquire));
        // writer가 비우면 다시 보낼 수 있다.
        rx.try_recv().unwrap();
        assert!(data_queue_has_room(&conn));
    }

    /// 여유분이 0칸인 작은 큐도 영원히 막히지 않는다.
    #[test]
    fn tiny_queue_still_accepts_records() {
        let (conn, _rx) = handle(1);
        assert!(data_queue_has_room(&conn));
        assert!(conn.send(vec![b'x']));
        assert!(!data_queue_has_room(&conn));
        assert!(!conn.closed.load(std::sync::atomic::Ordering::Acquire));
    }

    /// 여러 펌프가 여유 검사를 동시에 통과해도(검사와 넣기 사이의 경쟁) 예약은
    /// 큐 상한에서 `QueueFull`로 물러날 뿐 데이터 연결을 닫지 않는다 — 재생
    /// 폭주가 연결을 끊어 UI가 모든 pane을 다시 재생하던 순환의 회귀 시험.
    #[test]
    fn racing_pumps_back_off_at_the_cap_without_closing() {
        let (conn, mut rx) = handle(128);
        for _ in 0..128 {
            let permit = conn.reserve_frame().expect("below the cap");
            permit.send(vec![b'x']);
        }
        let refused = conn.reserve_frame().err();
        assert_eq!(refused, Some(FrameBackoff::QueueFull));
        assert!(!conn.send(vec![b'y']), "refused, not closed");
        assert!(!conn.closed.load(std::sync::atomic::Ordering::Acquire));
        rx.try_recv().unwrap();
        assert!(conn.reserve_frame().is_ok());
    }
}

#[cfg(test)]
mod bounded_replay_tests {
    use super::{bounded_replay_start, read_journal_range};
    use std::sync::Mutex;
    use term_pty::journal::{
        GlobalJournalBudget, JournalOptions, JournalRecordKind, JournalWriter,
    };
    use term_pty::segments::SegmentSnapshot;

    /// 4 KiB 세그먼트로 도는 저널에 출력을 넣어 여러 세그먼트를 만든다.
    fn rolling_journal(
        dir: &std::path::Path,
        outputs: usize,
    ) -> (std::path::PathBuf, SegmentSnapshot) {
        let base = dir.join("bounded.mtj");
        let mut writer = JournalWriter::open_with(
            &base,
            uuid::Uuid::new_v4(),
            JournalOptions {
                session_limit: 64 * 1024 * 1024,
                segment_cap: Some(4096),
            },
            GlobalJournalBudget::shared(64 * 1024 * 1024),
        )
        .unwrap();
        writer.append_resize(80, 24).unwrap();
        for i in 1..=outputs {
            writer
                .append_output(format!("payload-{i:05}\n").as_bytes())
                .unwrap();
        }
        writer.flush_now().unwrap();
        let head = writer.segment_snapshot();
        (base, head)
    }

    /// 예산이 저널보다 작으면 뒤쪽 세그먼트 머리(크기 레코드)에서 시작하고,
    /// 건너뛴 바이트를 알리며, 심은 체크포인트로 그 자리부터 곧바로 읽힌다.
    #[test]
    fn budget_starts_at_a_late_segment_head_and_reports_skipped_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let (base, head) = rolling_journal(dir.path(), 3000); // 약 48 KB → 세그먼트 여럿
        assert!(head.active_index >= 3, "{head:?}");
        let offsets = Mutex::new(std::collections::BTreeMap::new());

        // 넉넉한 예산: 그대로.
        assert_eq!(
            bounded_replay_start(&base, &head, &offsets, 1, u64::MAX),
            (1, 0)
        );

        // 8 KiB 예산: 앞 세그먼트들은 건너뛴다.
        let (from, skipped) = bounded_replay_start(&base, &head, &offsets, 1, 8 * 1024);
        assert!(from > 1 && skipped > 0, "from={from} skipped={skipped}");
        let records = read_journal_range(&base, &head, from, from + 5, &offsets).unwrap();
        assert_eq!(records.first().map(|r| r.seq), Some(from));
        assert!(
            matches!(records[0].kind, JournalRecordKind::Resize),
            "세그먼트 머리는 크기 레코드"
        );

        // 요청 시작점이 이미 예산 안이면 그대로 둔다(스냅샷 재개 존중).
        let inside = from + 3;
        assert_eq!(
            bounded_replay_start(&base, &head, &offsets, inside, 8 * 1024),
            (inside, 0)
        );
    }

    /// 예산 안에 드는 작은 저널은 예산과 무관하게 처음부터 재생한다.
    #[test]
    fn small_journal_is_never_bounded() {
        let dir = tempfile::TempDir::new().unwrap();
        let (base, head) = rolling_journal(dir.path(), 10);
        let offsets = Mutex::new(std::collections::BTreeMap::new());
        assert_eq!(
            bounded_replay_start(&base, &head, &offsets, 1, u64::MAX),
            (1, 0)
        );
    }

    /// 단일 파일(레거시) 저널은 체크포인트로 꼬리 예산만큼 잘라 시작하고, 그
    /// 자리부터 곧바로 읽힌다(훑으며 심은 체크포인트).
    #[test]
    fn legacy_single_file_journal_is_bounded_by_checkpoints() {
        let dir = tempfile::TempDir::new().unwrap();
        let base = dir.path().join("legacy.mtj");
        let mut writer = JournalWriter::open(&base, uuid::Uuid::new_v4()).unwrap();
        writer.append_resize(80, 24).unwrap();
        for i in 1..=3000 {
            writer
                .append_output(format!("payload-{i:05}\n").as_bytes())
                .unwrap();
        }
        writer.flush_now().unwrap();
        let head = SegmentSnapshot::default();
        let offsets = Mutex::new(std::collections::BTreeMap::new());
        let len = std::fs::metadata(&base).unwrap().len();

        assert_eq!(
            bounded_replay_start(&base, &head, &offsets, 1, len),
            (1, 0),
            "예산 안이면 그대로"
        );
        let (from, skipped) = bounded_replay_start(&base, &head, &offsets, 1, 8 * 1024);
        assert!(from > 1 && skipped > 0, "from={from} skipped={skipped}");
        assert!(
            len - skipped <= 8 * 1024 + 4096,
            "꼬리는 예산 근처(체크포인트 간격 오차)"
        );
        let records = read_journal_range(&base, &head, from, from + 5, &offsets).unwrap();
        assert_eq!(records.first().map(|r| r.seq), Some(from));
    }
}

#[cfg(test)]
mod range_tests {
    use super::{read_journal_range, RangeError};
    use std::sync::Mutex;
    use term_pty::journal::JournalWriter;
    use term_pty::segments::SegmentSnapshot;

    /// 단일 파일(legacy) 저널의 머리 요약: 세그먼트 0, seq 1부터.
    fn legacy() -> SegmentSnapshot {
        SegmentSnapshot::default()
    }

    /// N개 출력 레코드(resize가 seq 1)를 가진 저널을 만든다.
    fn journal_with(dir: &std::path::Path, outputs: usize) -> std::path::PathBuf {
        let path = dir.join("range.mtj");
        let mut writer = JournalWriter::open(&path, uuid::Uuid::new_v4()).unwrap();
        writer.append_resize(80, 24).unwrap();
        for i in 1..=outputs {
            writer
                .append_output(format!("payload-{i:05}\n").as_bytes())
                .unwrap();
        }
        writer.flush_now().unwrap();
        path
    }

    fn replay_all(path: &std::path::Path, from: u64, to: u64) -> Vec<u64> {
        term_pty::journal::JournalReader::open(path)
            .unwrap()
            .replay(from, to)
            .unwrap()
            .into_iter()
            .map(|r| r.seq)
            .collect()
    }

    #[test]
    fn ranges_match_full_replay_before_and_after_checkpoints() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = journal_with(dir.path(), 1000); // seq 1..=1001
        let offsets = Mutex::new(std::collections::BTreeMap::new());

        // 전체 경로(체크포인트 없음): [500..=520].
        let got: Vec<u64> = read_journal_range(&path, &legacy(), 500, 520, &offsets)
            .unwrap()
            .into_iter()
            .map(|r| r.seq)
            .collect();
        assert_eq!(got, (500..=520).collect::<Vec<u64>>());

        // 체크포인트가 채워졌는가(256·512 배수 seq).
        {
            let cursor = offsets.lock().unwrap();
            assert!(cursor.contains_key(&256) && cursor.contains_key(&512));
        }

        // 증분 경로: [600..=640] — 전체 replay와 정확히 일치해야 한다.
        let got: Vec<u64> = read_journal_range(&path, &legacy(), 600, 640, &offsets)
            .unwrap()
            .into_iter()
            .map(|r| r.seq)
            .collect();
        assert_eq!(got, replay_all(&path, 600, 640));

        // 체크포인트 사이(예: seq 300..=310, 직전 체크포인트 256)도 정확.
        let got: Vec<u64> = read_journal_range(&path, &legacy(), 300, 310, &offsets)
            .unwrap()
            .into_iter()
            .map(|r| r.seq)
            .collect();
        assert_eq!(got, (300..=310).collect::<Vec<u64>>());

        // 반복 호출(캐시 히트 안정성) — [600..=640]과 다시 비교.
        let again: Vec<u64> = read_journal_range(&path, &legacy(), 600, 640, &offsets)
            .unwrap()
            .into_iter()
            .map(|r| r.seq)
            .collect();
        assert_eq!(again, replay_all(&path, 600, 640));
    }

    #[test]
    fn head_window_and_beyond_eof_edges() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = journal_with(dir.path(), 10); // seq 1..=11
        let offsets = Mutex::new(std::collections::BTreeMap::new());

        let head: Vec<u64> = read_journal_range(&path, &legacy(), 1, 11, &offsets)
            .unwrap()
            .into_iter()
            .map(|r| r.seq)
            .collect();
        assert_eq!(head, (1..=11).collect::<Vec<u64>>());

        // EOF 너머는 빈 창, from>to도 빈 창.
        assert!(read_journal_range(&path, &legacy(), 50, 60, &offsets)
            .unwrap()
            .is_empty());
        assert!(read_journal_range(&path, &legacy(), 5, 4, &offsets)
            .unwrap()
            .is_empty());
        assert!(read_journal_range(&path, &legacy(), 0, 5, &offsets)
            .unwrap()
            .is_empty());

        // 창 상한: [2..=2000]은 64레코드에서 자른다(배치 크기 계약).
        // (이 저널엔 seq 1..=11만 있으므로 실제로는 10개가 전부다.)
        let capped = read_journal_range(&path, &legacy(), 2, 2000, &offsets).unwrap();
        assert_eq!(capped.len(), 10);
        assert_eq!(capped[0].seq, 2);
        let big = journal_with(dir.path(), 200);
        let offsets2 = Mutex::new(std::collections::BTreeMap::new());
        let capped2 = read_journal_range(&big, &legacy(), 2, 2000, &offsets2).unwrap();
        assert_eq!(capped2.len(), 64);
        assert_eq!(capped2[0].seq, 2);
    }

    /// 롤링 저널: 창 읽기가 세그먼트 경계를 넘고, 잘린 헤드 아래의 요청은
    /// HeadTrimmed로 구분된다(재시도 대상이 아니다).
    #[test]
    fn rolling_ranges_cross_segments_and_flag_trimmed_head() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("rolling.mtj");
        let mut writer = JournalWriter::open_with(
            &path,
            uuid::Uuid::new_v4(),
            term_pty::journal::JournalOptions {
                session_limit: 16 * 1024,
                segment_cap: Some(term_pty::journal::DEFAULT_SEGMENT_BYTES),
            },
            term_pty::journal::GlobalJournalBudget::shared_default(),
        )
        .unwrap();
        writer.append_resize(80, 24).unwrap();
        for i in 1..=1000 {
            writer
                .append_output(format!("payload-{i:05}\n").as_bytes())
                .unwrap();
        }
        writer.flush_now().unwrap();
        let head = writer.tracker().unwrap().snapshot();
        assert!(head.first_seq > 1, "{head:?}");
        let last_seq = writer.last_seq();
        let offsets = Mutex::new(std::collections::BTreeMap::new());

        // 헤드부터 끝까지 64개 창으로 이어 읽으면 전체 replay와 같다.
        let mut got: Vec<u64> = Vec::new();
        let mut from = head.first_seq;
        while from <= last_seq {
            let batch = read_journal_range(&path, &head, from, last_seq, &offsets).unwrap();
            if batch.is_empty() {
                break;
            }
            from = batch.last().unwrap().seq + 1;
            got.extend(batch.into_iter().map(|r| r.seq));
        }
        let expected: Vec<u64> = term_pty::segments::JournalSet::open(&path)
            .unwrap()
            .replay(head.first_seq, last_seq)
            .unwrap()
            .into_iter()
            .map(|r| r.seq)
            .collect();
        assert_eq!(got, expected);
        assert!(offsets
            .lock()
            .unwrap()
            .keys()
            .all(|seq| *seq >= head.first_seq));

        // 체크포인트 경유 증분 읽기도 같다.
        let mid = head.first_seq + 100;
        let incremental: Vec<u64> = read_journal_range(&path, &head, mid, mid + 20, &offsets)
            .unwrap()
            .into_iter()
            .map(|r| r.seq)
            .collect();
        assert_eq!(incremental, (mid..=mid + 20).collect::<Vec<u64>>());

        // 잘린 헤드 아래.
        assert!(matches!(
            read_journal_range(&path, &head, 1, 10, &offsets),
            Err(RangeError::HeadTrimmed { first_seq }) if first_seq == head.first_seq
        ));
    }
}

#[cfg(test)]
mod redaction_tests {
    use super::*;
    use term_pty::actor::{MemJournal, MemRecordKind};

    const KEY: &str = "zai-test-key-0123456789abcdef";

    fn redactor() -> Arc<SecretRedactor> {
        Arc::new(SecretRedactor::new([KEY.to_string()]))
    }

    fn outputs(journal: &MemJournal) -> Vec<Vec<u8>> {
        journal
            .records
            .iter()
            .filter_map(|r| match &r.kind {
                MemRecordKind::Output(bytes) => Some(bytes.clone()),
                MemRecordKind::Resize { .. } => None,
            })
            .collect()
    }

    #[test]
    fn chunk_without_the_token_is_passed_through_borrowed() {
        let redactor = redactor();
        let data = b"plain shell output\x1b[0m\r\n";
        assert!(matches!(
            redact_chunk(&redactor, data),
            std::borrow::Cow::Borrowed(bytes) if bytes == data
        ));
    }

    #[test]
    fn token_inside_a_chunk_is_replaced() {
        let redactor = redactor();
        let data = format!("$ echo $ANTHROPIC_AUTH_TOKEN\r\n{KEY}\r\n");
        let redacted = redact_chunk(&redactor, data.as_bytes());
        assert_eq!(
            &*redacted,
            b"$ echo $ANTHROPIC_AUTH_TOKEN\r\n[redacted]\r\n"
        );
    }

    #[test]
    fn invalid_utf8_tail_is_kept_and_the_valid_prefix_is_still_scrubbed() {
        let redactor = redactor();
        // 한글 'ㅎ'(E3 85 8E)의 첫 두 바이트만 담긴 청크 끝 + 중간의 잘못된 바이트.
        let mut data = format!("token={KEY} ").into_bytes();
        data.push(0xFF);
        data.extend_from_slice(format!(" again {KEY}").as_bytes());
        data.extend_from_slice(&[0xE3, 0x85]);
        let redacted = redact_chunk(&redactor, &data);
        let mut expected = b"token=[redacted] ".to_vec();
        expected.push(0xFF);
        expected.extend_from_slice(b" again [redacted]");
        expected.extend_from_slice(&[0xE3, 0x85]);
        assert_eq!(&*redacted, expected.as_slice());
    }

    /// 문서화된 v1 한계: 청크 경계에 걸친 토큰은 통과한다.
    #[test]
    fn token_split_across_two_chunks_passes_by_design() {
        let redactor = redactor();
        let (head, tail) = KEY.split_at(KEY.len() / 2);
        assert!(matches!(
            redact_chunk(&redactor, head.as_bytes()),
            std::borrow::Cow::Borrowed(_)
        ));
        assert!(matches!(
            redact_chunk(&redactor, tail.as_bytes()),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    /// JSON처럼 보이는 청크도 부분 문자열 치환만 한다 — 공백·줄바꿈이
    /// 그대로 남아야 화면과 저널이 사용자가 본 바이트와 같다.
    #[test]
    fn json_looking_chunk_keeps_its_whitespace() {
        let redactor = redactor();
        let data = format!("{{\"token\":\"{KEY}\",\n  \"x\": 1}}\r\n");
        let redacted = redact_chunk(&redactor, data.as_bytes());
        assert_eq!(&*redacted, b"{\"token\":\"[redacted]\",\n  \"x\": 1}\r\n");
        // 배열·문자열 리터럴 시작도 마찬가지.
        let data = format!("[ \"{KEY}\" ,\n1 ]");
        assert_eq!(
            &*redact_chunk(&redactor, data.as_bytes()),
            b"[ \"[redacted]\" ,\n1 ]"
        );
    }

    #[test]
    fn redacting_journal_scrubs_output_and_forwards_resizes() {
        let mut journal = RedactingJournal::new(MemJournal::new(), redactor());
        assert_eq!(journal.append_resize(80, 24).unwrap(), 1);
        assert_eq!(journal.append_output(b"hello").unwrap(), 2);
        assert_eq!(
            journal
                .append_output(format!("ANTHROPIC_AUTH_TOKEN={KEY}\n").as_bytes())
                .unwrap(),
            3
        );
        let inner = journal.inner;
        assert_eq!(
            outputs(&inner),
            vec![
                b"hello".to_vec(),
                b"ANTHROPIC_AUTH_TOKEN=[redacted]\n".to_vec()
            ]
        );
        assert!(matches!(
            inner.records[0].kind,
            MemRecordKind::Resize { cols: 80, rows: 24 }
        ));
    }

    #[test]
    fn grown_chunk_is_split_at_the_journal_record_cap() {
        // 1바이트 비밀 → 치환마다 9바이트씩 자란다; 상한 크기 청크가 통째로
        // 비밀이면 결과는 여러 레코드로 나뉘어야 한다.
        let redactor = Arc::new(SecretRedactor::new(["k".to_string()]));
        let mut journal = RedactingJournal::new(MemJournal::new(), redactor);
        journal.append_resize(80, 24).unwrap();
        let data = vec![b'k'; MAX_OUTPUT_PAYLOAD];
        let last = journal.append_output(&data).unwrap();
        let pieces = outputs(&journal.inner);
        assert!(pieces.len() > 1);
        assert!(pieces.iter().all(|p| p.len() <= MAX_OUTPUT_PAYLOAD));
        let joined: Vec<u8> = pieces.concat();
        assert_eq!(joined.len(), MAX_OUTPUT_PAYLOAD * "[redacted]".len());
        assert_eq!(last, 1 + pieces.len() as u64);
    }
}
