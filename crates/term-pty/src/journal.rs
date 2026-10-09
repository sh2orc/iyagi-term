//! MTJ1 journal codec: an append-only log of raw PTY output and resize
//! records that supports full-stream replay on re-attach
//! (spec `02-runner.md` §5).
//!
//! Wire format, all integers little-endian:
//!
//! ```text
//! file header: ASCII "MTJ1" (4 bytes) + session UUID raw bytes (16)
//! record: body_len u32, seq u64, kind u8, payload N bytes, crc32 u32
//!   body_len = 8 + 1 + N + 4   (max 16_397)
//!   kind 1 = raw output (N <= 16_384), kind 2 = cols u16 + rows u16
//!   crc32 = IEEE CRC-32 over seq || kind || payload
//! ```
//!
//! The first record of a session is always the initial size. `seq` starts at
//! 1 and must never gap; a gap is corruption. Appends are complete records
//! written with `write_all` and flushed at most every 250 ms via
//! [`JournalWriter::flush_tick`] (also on Drop and [`JournalWriter::finalize`]).
//! An incomplete final record is reported as [`ScanStatus::TailTruncated`]
//! and excluded from replay; mid-file damage is [`ScanStatus::Corrupt`] and
//! the remainder is never replayed as data.
//!
//! Disk usage is bounded per session (128 MiB default) and globally
//! (2 GiB default, shared through [`GlobalJournalBudget`]). Space is
//! reserved *before* every write. In rolling mode
//! ([`JournalOptions::segment_cap`], the daemon's default) the session
//! limit is a retention window: a full segment rotates into `<base>.<k>`
//! and the oldest closed segments are deleted (see [`crate::segments`]),
//! so the journal never stops for its own limit. A legacy single-file
//! writer, and a rolling writer with nothing left to trim, still return cap
//! errors — then the caller must stop reading the PTY.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crc32fast;
use uuid::Uuid;

use term_contracts::U64String;

use crate::segments::{segment_path, SegmentSnapshot, SegmentTracker};

/// File magic (`MTJ1`).
pub const MAGIC: [u8; 4] = *b"MTJ1";
/// `MTJ1` (4) + raw session UUID (16).
pub const HEADER_LEN: usize = 4 + 16;
/// `seq` (8) + `kind` (1) + `crc32` (4).
pub const RECORD_FRAMING_LEN: usize = 8 + 1 + 4;
/// Smallest legal `body_len` (empty output payload).
pub const MIN_BODY_LEN: u32 = RECORD_FRAMING_LEN as u32;
/// Largest legal `body_len` = 8 + 1 + 16_384 + 4 (spec §5).
pub const MAX_BODY_LEN: u32 = RECORD_FRAMING_LEN as u32 + MAX_OUTPUT_PAYLOAD as u32;
/// Raw output payload cap per record (`output_chunk_bytes`).
pub const MAX_OUTPUT_PAYLOAD: usize = 16_384;
/// Resize payload: `cols u16 LE + rows u16 LE`.
pub const RESIZE_PAYLOAD_LEN: usize = 4;
/// Flush cadence for appends (defaults.json `journal_flush`).
pub const FLUSH_INTERVAL_MS: u64 = 250;
/// defaults.json `journal_session_bytes` (128 MiB).
pub const DEFAULT_SESSION_LIMIT: u64 = 128 * 1024 * 1024;
/// defaults.json `journal_global_bytes` (2 GiB).
pub const DEFAULT_GLOBAL_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
/// defaults.json `journal_segment_bytes` (16 MiB): cap on a rolling
/// journal's segment target.
pub const DEFAULT_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;
/// Smallest segment target. A record is at most 16 KiB + framing, so a
/// segment always holds at least one record whatever the target.
pub const MIN_SEGMENT_BYTES: u64 = 4 * 1024;

/// Segment target for a session limit: an eighth of the limit, clamped to
/// `[MIN_SEGMENT_BYTES, segment_cap]`. Eight segments keep the retained
/// window at 7/8 of the limit or more after every trim.
pub fn segment_target_for(session_limit: u64, segment_cap: u64) -> u64 {
    (session_limit / 8)
        .max(MIN_SEGMENT_BYTES)
        .min(segment_cap.max(MIN_SEGMENT_BYTES))
}

/// How a writer treats its session limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalOptions {
    /// Per-session on-disk byte limit.
    pub session_limit: u64,
    /// `Some(cap)`: rolling mode — the journal is a run of segment files
    /// (`<base>`, `<base>.<k>`; see [`crate::segments`]); a full segment
    /// rotates and the oldest closed segments are deleted once retained
    /// bytes exceed the session limit. The journal never stops for its own
    /// limit. `cap` bounds the segment target ([`segment_target_for`]).
    /// `None`: legacy single file — the limit stops appends with
    /// [`JournalFlowError::SessionCap`].
    pub segment_cap: Option<u64>,
}
/// Seq ceiling from the contracts (`U64String::MAX`, SQLite signed range).
const SEQ_MAX: u64 = U64String::MAX;

/// Userspace write buffer; keeps the 250 ms flush cadence meaningful for
/// sub-buffer writes (records larger than this spill straight to the OS).
const WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// Read buffer for window scans and bounded record streams.
const SCAN_BUFFER_BYTES: usize = 64 * 1024;

/// Record kind on the journal stream (`kind` byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalRecordKind {
    /// Raw PTY output bytes (kind = 1, payload <= 16_384).
    Output,
    /// Terminal size change (kind = 2, payload = cols u16 LE + rows u16 LE).
    Resize,
}

impl JournalRecordKind {
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Output => 1,
            Self::Resize => 2,
        }
    }

    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Output),
            2 => Some(Self::Resize),
            _ => None,
        }
    }
}

/// One decoded journal record, in exact journal order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRecord {
    pub seq: u64,
    pub kind: JournalRecordKind,
    pub payload: Vec<u8>,
}

impl JournalRecord {
    /// Output record carrying `bytes`.
    pub fn output(seq: u64, bytes: &[u8]) -> Self {
        Self {
            seq,
            kind: JournalRecordKind::Output,
            payload: bytes.to_vec(),
        }
    }

    /// Resize record carrying `cols`/`rows` (payload = 4 bytes LE).
    pub fn resize(seq: u64, cols: u16, rows: u16) -> Self {
        let mut payload = [0u8; RESIZE_PAYLOAD_LEN];
        payload[..2].copy_from_slice(&cols.to_le_bytes());
        payload[2..].copy_from_slice(&rows.to_le_bytes());
        Self {
            seq,
            kind: JournalRecordKind::Resize,
            payload: payload.to_vec(),
        }
    }

    /// Raw output bytes when this is an output record.
    pub fn output_bytes(&self) -> Option<&[u8]> {
        (self.kind == JournalRecordKind::Output).then_some(self.payload.as_slice())
    }

    /// `(cols, rows)` when this is a resize record.
    pub fn resize_dims(&self) -> Option<(u16, u16)> {
        if self.kind != JournalRecordKind::Resize || self.payload.len() != RESIZE_PAYLOAD_LEN {
            return None;
        }
        let cols = u16::from_le_bytes([self.payload[0], self.payload[1]]);
        let rows = u16::from_le_bytes([self.payload[2], self.payload[3]]);
        Some((cols, rows))
    }
}

/// Journal and budget errors. `SessionCap`/`GlobalCap` mean the caller must
/// stop reading the PTY for this session; `DiskFull` is mapped from
/// disk-space I/O errors on the write path.
#[derive(Debug, thiserror::Error)]
pub enum JournalFlowError {
    #[error("journal I/O error on {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("disk full while writing journal {path}")]
    DiskFull { path: PathBuf },
    #[error("session journal cap reached ({limit} bytes); stop reading the PTY")]
    SessionCap { limit: u64 },
    #[error("global journal budget exhausted ({limit} bytes)")]
    GlobalCap { limit: u64 },
    #[error("journal output payload is {len} bytes; max is {MAX_OUTPUT_PAYLOAD}")]
    PayloadTooLarge { len: usize },
    #[error("the first journal record must be the initial size (kind=2 resize)")]
    FirstRecordNotSize,
    #[error("journal seq would exceed the contracts bound i64::MAX")]
    SeqExhausted,
    #[error("journal header invalid ({reason}): {path}")]
    BadHeader { path: PathBuf, reason: &'static str },
    #[error("journal session UUID mismatch: expected {expected}, found {found}")]
    HeaderUuidMismatch { expected: Uuid, found: Uuid },
    #[error("journal corrupt at seq {at_seq}")]
    Corrupt { at_seq: u64 },
    /// Rolling journal: the requested records were deleted with the head;
    /// the earliest still on disk is `first_seq`.
    #[error("journal head trimmed; records before seq {first_seq} are gone")]
    HeadTrimmed { first_seq: u64 },
}

/// Global on-disk journal budget shared by all session writers
/// (defaults.json `journal_global_bytes`, 2 GiB). Space is reserved before
/// every write; reservations represent on-disk bytes, so they are released
/// only when journal files are deleted (retention cleanup is out of scope
/// for this module).
#[derive(Debug)]
pub struct GlobalJournalBudget {
    limit: u64,
    used: u64,
}

impl GlobalJournalBudget {
    pub const fn new(limit: u64) -> Self {
        Self { limit, used: 0 }
    }

    /// Shared budget handle with a custom limit.
    pub fn shared(limit: u64) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::new(limit)))
    }

    /// Shared budget handle with the default 2 GiB limit.
    pub fn shared_default() -> Arc<Mutex<Self>> {
        Self::shared(DEFAULT_GLOBAL_LIMIT)
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    pub fn used(&self) -> u64 {
        self.used
    }

    /// Reserve `bytes` before writing them. Fails (changing nothing) when
    /// the global cap would be exceeded.
    pub fn try_reserve(&mut self, bytes: u64) -> Result<(), JournalFlowError> {
        match self.used.checked_add(bytes) {
            Some(next) if next <= self.limit => {
                self.used = next;
                Ok(())
            }
            _ => Err(JournalFlowError::GlobalCap { limit: self.limit }),
        }
    }

    /// Give reserved space back (e.g. after a failed write, or when a
    /// journal file is deleted by retention). Saturates at zero, so a
    /// release for bytes this budget never counted (e.g. a stale storage
    /// record after a restart) can never underflow the counter.
    pub fn release(&mut self, bytes: u64) {
        self.used = self.used.saturating_sub(bytes);
    }

    /// Adopt `bytes` already on disk (startup seeding). A fresh budget
    /// starts at zero, so without this the cap would ignore the journals
    /// previous daemon generations left behind — they stay on disk for
    /// `journal_retention_days` before retention deletes them. `used` may
    /// end up above `limit` on purpose: that is the honest state of the
    /// disk, and [`GlobalJournalBudget::try_reserve`] keeps refusing new
    /// writes until retention releases enough of it. A rolling writer also
    /// adopts a rotation's few opening bytes this way when a stuck segment
    /// delete leaves no other means to keep its active file well-formed.
    pub fn seed_used(&mut self, bytes: u64) {
        self.used = self.used.saturating_add(bytes);
    }
}

fn lock_budget(global: &Arc<Mutex<GlobalJournalBudget>>) -> MutexGuard<'_, GlobalJournalBudget> {
    // Counter-only state: recover from poisoning instead of failing the data path.
    global
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Encode one complete record: `body_len u32 LE | seq u64 LE | kind | payload
/// | crc32 u32 LE`. Low-level codec helper — callers validate payload limits.
pub fn encode_record(seq: u64, kind: JournalRecordKind, payload: &[u8]) -> Vec<u8> {
    let body_len = RECORD_FRAMING_LEN + payload.len();
    let mut crc = crc32fast::Hasher::new();
    crc.update(&seq.to_le_bytes());
    crc.update(&[kind.as_byte()]);
    crc.update(payload);
    let mut frame = Vec::with_capacity(4 + body_len);
    frame.extend_from_slice(&(body_len as u32).to_le_bytes());
    frame.extend_from_slice(&seq.to_le_bytes());
    frame.push(kind.as_byte());
    frame.extend_from_slice(payload);
    frame.extend_from_slice(&crc.finalize().to_le_bytes());
    frame
}

/// Classify a full scan of a journal file (`02-runner.md` §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStatus {
    /// Every record verified; seq contiguous from 1.
    Ok,
    /// The final record is incomplete (torn tail, e.g. a crash inside the
    /// 250 ms flush window). It is excluded from replay.
    TailTruncated,
    /// CRC mismatch, seq gap, or malformed framing at `at_seq` — the first
    /// seq that could not be verified. The rest is not replayed as data.
    Corrupt { at_seq: u64 },
}

/// One closed segment still on disk (rolling mode).
#[derive(Debug)]
struct ClosedSegment {
    index: u64,
    path: PathBuf,
    first_seq: u64,
    bytes: u64,
}

/// Rolling-mode state: segment rotation and head trimming.
#[derive(Debug)]
struct Rolling {
    segment_cap: u64,
    segment_target: u64,
    /// Oldest first.
    closed: VecDeque<ClosedSegment>,
    active_index: u64,
    active_first_seq: u64,
    /// Bytes in the active file (header + records).
    active_bytes: u64,
    /// Records in the active file. Rotation waits for a second record: a
    /// fresh segment holds exactly its leading size record, so requiring
    /// more than one keeps a rotation from rolling straight into another.
    active_records: u64,
    dropped_bytes: u64,
    tracker: Arc<SegmentTracker>,
}

/// Append-only MTJ1 writer with per-session and global byte accounting.
///
/// The first record appended must be the initial size
/// ([`JournalWriter::append_resize`]); `append_output` before it fails with
/// [`JournalFlowError::FirstRecordNotSize`].
///
/// In rolling mode ([`JournalOptions::segment_cap`]) the writer rotates
/// segment files and deletes the oldest ones to honour the session limit
/// instead of refusing appends; [`JournalWriter::first_seq`] then moves
/// forward and [`JournalWriter::dropped_bytes`] counts the deleted head.
pub struct JournalWriter {
    buf: BufWriter<File>,
    path: PathBuf,
    session_uuid: Uuid,
    session_limit: u64,
    global: Arc<Mutex<GlobalJournalBudget>>,
    last_seq: u64,
    /// Bytes retained on disk for this session (every segment; header +
    /// records). Legacy mode never trims, so it is also the total written.
    journal_bytes: u64,
    has_initial_size: bool,
    dirty: bool,
    last_flush_ms: Option<u64>,
    /// Rolling state; `None` = legacy single-file cap.
    rolling: Option<Rolling>,
    /// Last size record written — the leading record of every new segment.
    last_size: Option<(u16, u16)>,
}

impl JournalWriter {
    /// Open with the default session cap (128 MiB) and a private global
    /// budget. Production callers sharing sessions must use
    /// [`JournalWriter::with_budget`] instead.
    pub fn open(path: impl AsRef<Path>, session_uuid: Uuid) -> Result<Self, JournalFlowError> {
        Self::with_budget(
            path,
            session_uuid,
            DEFAULT_SESSION_LIMIT,
            GlobalJournalBudget::shared_default(),
        )
    }

    /// Open with an explicit per-session cap and a shared global budget
    /// (legacy single-file mode). Creates (or truncates) the file and
    /// writes the 20-byte header.
    pub fn with_budget(
        path: impl AsRef<Path>,
        session_uuid: Uuid,
        session_limit: u64,
        global: Arc<Mutex<GlobalJournalBudget>>,
    ) -> Result<Self, JournalFlowError> {
        Self::open_with(
            path,
            session_uuid,
            JournalOptions {
                session_limit,
                segment_cap: None,
            },
            global,
        )
    }

    /// Open with explicit [`JournalOptions`]. Creates (or truncates) the
    /// active file and writes the 20-byte header. Stale closed segments of
    /// an earlier run at the same base path are not adopted — a session id
    /// is minted once.
    pub fn open_with(
        path: impl AsRef<Path>,
        session_uuid: Uuid,
        options: JournalOptions,
        global: Arc<Mutex<GlobalJournalBudget>>,
    ) -> Result<Self, JournalFlowError> {
        let path = path.as_ref().to_path_buf();
        let file = File::create(&path).map_err(|source| JournalFlowError::Io {
            path: path.clone(),
            source,
        })?;
        let rolling = options.segment_cap.map(|cap| Rolling {
            segment_cap: cap,
            segment_target: segment_target_for(options.session_limit, cap),
            closed: VecDeque::new(),
            active_index: 0,
            active_first_seq: 1,
            active_bytes: 0,
            active_records: 0,
            dropped_bytes: 0,
            tracker: Arc::new(SegmentTracker::new()),
        });
        let mut writer = Self {
            buf: BufWriter::with_capacity(WRITE_BUFFER_BYTES, file),
            path,
            session_uuid,
            session_limit: options.session_limit,
            global,
            last_seq: 0,
            journal_bytes: 0,
            has_initial_size: false,
            dirty: false,
            last_flush_ms: None,
            rolling,
            last_size: None,
        };
        writer.make_room(HEADER_LEN as u64)?;
        writer.write_reserved(&Self::header(session_uuid))?;
        Ok(writer)
    }

    fn header(session_uuid: Uuid) -> [u8; HEADER_LEN] {
        let mut header = [0u8; HEADER_LEN];
        header[..4].copy_from_slice(&MAGIC);
        header[4..].copy_from_slice(session_uuid.as_bytes());
        header
    }

    /// Append a raw output chunk (<= 16_384 bytes); returns the assigned seq.
    pub fn append_output(&mut self, bytes: &[u8]) -> Result<u64, JournalFlowError> {
        if bytes.len() > MAX_OUTPUT_PAYLOAD {
            return Err(JournalFlowError::PayloadTooLarge { len: bytes.len() });
        }
        if !self.has_initial_size {
            return Err(JournalFlowError::FirstRecordNotSize);
        }
        self.append_record(JournalRecordKind::Output, bytes)
    }

    /// Append a resize record (also used for the mandatory initial size);
    /// returns the assigned seq. Output and resize share one seq order.
    pub fn append_resize(&mut self, cols: u16, rows: u16) -> Result<u64, JournalFlowError> {
        let payload = JournalRecord::resize(0, cols, rows).payload;
        let seq = self.append_record(JournalRecordKind::Resize, &payload)?;
        self.has_initial_size = true;
        self.last_size = Some((cols, rows));
        Ok(seq)
    }

    /// Flush buffered records to the OS if at least
    /// [`FLUSH_INTERVAL_MS`] elapsed since the last tick flush (and there is
    /// anything buffered). `now_ms` is caller-injected. Returns whether a
    /// flush happened.
    pub fn flush_tick(&mut self, now_ms: u64) -> Result<bool, JournalFlowError> {
        if !self.dirty {
            return Ok(false);
        }
        let due = self
            .last_flush_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= FLUSH_INTERVAL_MS);
        if !due {
            return Ok(false);
        }
        self.do_flush()?;
        self.last_flush_ms = Some(now_ms);
        Ok(true)
    }

    /// Explicit flush (e.g. on session teardown before Drop).
    pub fn finalize(&mut self) -> Result<(), JournalFlowError> {
        self.do_flush()
    }

    /// Flush immediately, bypassing the 250 ms cadence — used when a live
    /// delivery consumer has caught up to `last_seq` on disk while records
    /// still sit in the writer's buffer (interactive latency path).
    pub fn flush_now(&mut self) -> Result<(), JournalFlowError> {
        self.do_flush()?;
        self.last_flush_ms = None;
        Ok(())
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Total bytes this writer produced (header + records), reserved against
    /// both caps.
    pub fn journal_bytes(&self) -> u64 {
        self.journal_bytes
    }

    pub fn session_uuid(&self) -> Uuid {
        self.session_uuid
    }

    pub fn session_limit(&self) -> u64 {
        self.session_limit
    }

    /// Runtime cap change (`retention.set_limit`). Legacy mode: a ceiling
    /// for future appends, never a trim — a value below the bytes already
    /// written makes the next append fail with
    /// [`JournalFlowError::SessionCap`]. Rolling mode: the oldest closed
    /// segments are deleted right away until the retained bytes fit.
    pub fn set_session_limit(&mut self, limit: u64) {
        self.session_limit = limit;
        if let Some(rolling) = self.rolling.as_mut() {
            rolling.segment_target = segment_target_for(limit, rolling.segment_cap);
        }
        if self.rolling.is_some() {
            self.trim_to_fit(0);
            // The active segment alone may exceed the new limit: close it so
            // it can go too, leaving a fresh segment that opens with the size.
            if self.journal_bytes > self.session_limit
                && self.rolling.as_ref().is_some_and(|r| r.active_records > 1)
            {
                if let Err(error) = self.rotate() {
                    tracing::warn!(path = %self.path.display(), %error, "journal rotate on limit change failed");
                }
                self.trim_to_fit(0);
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Rolling mode?
    pub fn is_rolling(&self) -> bool {
        self.rolling.is_some()
    }

    /// Current segment target (rolling mode only).
    pub fn segment_target(&self) -> Option<u64> {
        self.rolling.as_ref().map(|r| r.segment_target)
    }

    /// First seq still on disk — 1 until the head has been trimmed.
    pub fn first_seq(&self) -> u64 {
        match &self.rolling {
            Some(rolling) => rolling
                .closed
                .front()
                .map(|s| s.first_seq)
                .unwrap_or(rolling.active_first_seq),
            None => 1,
        }
    }

    /// Bytes deleted from the head so far (rolling mode; 0 otherwise).
    pub fn dropped_bytes(&self) -> u64 {
        self.rolling.as_ref().map(|r| r.dropped_bytes).unwrap_or(0)
    }

    /// Live head/tail summary shared with readers (rolling mode only).
    pub fn tracker(&self) -> Option<Arc<SegmentTracker>> {
        self.rolling.as_ref().map(|r| Arc::clone(&r.tracker))
    }

    /// The run's shape right now (a legacy writer is a single segment 0).
    pub fn segment_snapshot(&self) -> SegmentSnapshot {
        match &self.rolling {
            Some(rolling) => SegmentSnapshot {
                active_index: rolling.active_index,
                first_index: rolling
                    .closed
                    .front()
                    .map(|s| s.index)
                    .unwrap_or(rolling.active_index),
                first_seq: self.first_seq(),
                dropped_bytes: rolling.dropped_bytes,
                retained_bytes: self.journal_bytes,
            },
            None => SegmentSnapshot {
                retained_bytes: self.journal_bytes,
                ..SegmentSnapshot::default()
            },
        }
    }

    fn next_seq(&self) -> Result<u64, JournalFlowError> {
        self.last_seq
            .checked_add(1)
            .filter(|seq| *seq <= SEQ_MAX)
            .ok_or(JournalFlowError::SeqExhausted)
    }

    /// Make room first (this may rotate, which consumes a seq for the new
    /// segment's size record), then assign the seq and write the frame.
    fn append_record(
        &mut self,
        kind: JournalRecordKind,
        payload: &[u8],
    ) -> Result<u64, JournalFlowError> {
        let need = (4 + RECORD_FRAMING_LEN + payload.len()) as u64;
        self.make_room(need)?;
        let seq = self.next_seq()?;
        let frame = encode_record(seq, kind, payload);
        debug_assert_eq!(frame.len() as u64, need);
        self.write_reserved(&frame)?;
        self.last_seq = seq;
        if let Some(rolling) = self.rolling.as_mut() {
            rolling.active_records += 1;
        }
        Ok(seq)
    }

    /// Settle the session limit for `need` more bytes. Legacy mode refuses
    /// with [`JournalFlowError::SessionCap`]; rolling mode rotates a full
    /// segment and deletes the oldest closed segments until the bytes fit,
    /// refusing only when a single record is larger than the whole limit.
    fn make_room(&mut self, need: u64) -> Result<(), JournalFlowError> {
        let Some(rolling) = self.rolling.as_ref() else {
            return match self.journal_bytes.checked_add(need) {
                Some(next) if next <= self.session_limit => Ok(()),
                _ => Err(JournalFlowError::SessionCap {
                    limit: self.session_limit,
                }),
            };
        };
        let segment_full = rolling.active_bytes.saturating_add(need) > rolling.segment_target;
        let closable = rolling.active_records > 1;
        if segment_full && closable {
            self.rotate()?;
        }
        self.trim_to_fit(need);
        // Still over with nothing closed left: the active segment is the
        // whole history — roll it out and drop it as well.
        if self.journal_bytes.saturating_add(need) > self.session_limit
            && self.rolling.as_ref().is_some_and(|r| r.active_records > 1)
        {
            self.rotate()?;
            self.trim_to_fit(need);
        }
        if self.journal_bytes.saturating_add(need) > self.session_limit {
            return Err(JournalFlowError::SessionCap {
                limit: self.session_limit,
            });
        }
        Ok(())
    }

    /// Reserve the global budget *before* writing, then `write_all` the
    /// complete frame. On write failure the reservation is returned and the
    /// session counter stays untouched. The session limit is the caller's
    /// business ([`JournalWriter::make_room`]).
    fn write_reserved(&mut self, frame: &[u8]) -> Result<(), JournalFlowError> {
        self.reserve_global(frame.len() as u64)?;
        self.write_prereserved(frame)
    }

    /// `write_all` a frame whose global reservation the caller already
    /// holds. On write failure that reservation is returned and the session
    /// counter stays untouched.
    fn write_prereserved(&mut self, frame: &[u8]) -> Result<(), JournalFlowError> {
        let need = frame.len() as u64;
        if let Err(source) = self.buf.write_all(frame) {
            lock_budget(&self.global).release(need);
            return Err(self.write_io_error(source));
        }
        self.journal_bytes += need;
        if let Some(rolling) = self.rolling.as_mut() {
            rolling.active_bytes += need;
        }
        self.dirty = true;
        self.publish_tracker();
        Ok(())
    }

    /// Global reservation. A rolling writer gives up its own oldest history
    /// before failing with [`JournalFlowError::GlobalCap`].
    fn reserve_global(&mut self, need: u64) -> Result<(), JournalFlowError> {
        loop {
            let outcome = lock_budget(&self.global).try_reserve(need);
            match outcome {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if !self.trim_oldest_closed() {
                        return Err(error);
                    }
                }
            }
        }
    }

    /// Close the active segment under its numbered name and start a fresh
    /// active file whose first record is the current size. Consumes one seq
    /// (that size record); the stream stays contiguous across the boundary.
    ///
    /// Everything that can refuse (the seq bound, and the global reservation
    /// for the new segment's header + size record) is settled *before* the
    /// rename, so a refusal leaves the writer exactly as it was: the active
    /// file keeps its header and records, and the next append retries the
    /// rotation. Reserving after the rename used to leave an empty,
    /// headerless active file behind whenever the budget was full and the
    /// oldest closed segment could not be unlinked. Only the global budget
    /// is reserved here; the session limit is settled by the caller's trim.
    fn rotate(&mut self) -> Result<(), JournalFlowError> {
        let Some((cols, rows)) = self.last_size else {
            // No size yet (nothing but a header): nothing worth closing.
            return Ok(());
        };
        let seq = self.next_seq()?;
        let payload = JournalRecord::resize(seq, cols, rows).payload;
        let size_record = encode_record(seq, JournalRecordKind::Resize, &payload);
        let mut opening = Self::header(self.session_uuid).to_vec();
        opening.extend_from_slice(&size_record);
        let opening_bytes = opening.len() as u64;
        let prepaid = match self.reserve_global(opening_bytes) {
            Ok(()) => true,
            Err(error) => {
                // Nothing older is left to trim: only the segment closing
                // now can pay for the opening (`pay_opening_with_closed`).
                // A closable segment (header + two records) holds enough.
                let rolling = self.rolling.as_ref().expect("rotate requires rolling mode");
                if !rolling.closed.is_empty() || rolling.active_bytes < opening_bytes {
                    return Err(error);
                }
                false
            }
        };
        let global = Arc::clone(&self.global);
        let refund = || {
            if prepaid {
                lock_budget(&global).release(opening_bytes);
            }
        };
        if let Err(error) = self.do_flush() {
            refund();
            return Err(error);
        }
        let (index, closed_first_seq, closed_bytes) = {
            let rolling = self.rolling.as_ref().expect("rotate requires rolling mode");
            (
                rolling.active_index,
                rolling.active_first_seq,
                rolling.active_bytes,
            )
        };
        let closed_path = segment_path(&self.path, index);
        if let Err(source) = std::fs::rename(&self.path, &closed_path) {
            refund();
            return Err(self.write_io_error(source));
        }
        let file = match File::create(&self.path) {
            Ok(file) => file,
            Err(source) => {
                // Put the segment back under the active name: the writer's
                // handle still points at that file, so appends carry on.
                if let Err(error) = std::fs::rename(&closed_path, &self.path) {
                    tracing::warn!(
                        path = %self.path.display(),
                        %error,
                        "journal rotate rollback failed"
                    );
                }
                refund();
                return Err(self.write_io_error(source));
            }
        };
        let previous = std::mem::replace(
            &mut self.buf,
            BufWriter::with_capacity(WRITE_BUFFER_BYTES, file),
        );
        drop(previous);
        self.dirty = false;
        {
            let rolling = self.rolling.as_mut().expect("rotate requires rolling mode");
            rolling.closed.push_back(ClosedSegment {
                index,
                path: closed_path,
                first_seq: closed_first_seq,
                bytes: closed_bytes,
            });
            rolling.active_index = index + 1;
            rolling.active_bytes = 0;
            rolling.active_records = 0;
            rolling.active_first_seq = seq;
        }
        if !prepaid {
            self.pay_opening_with_closed(opening_bytes);
        }
        // Header + leading size record, on the reservation settled above.
        self.write_prereserved(&opening)?;
        self.last_seq = seq;
        if let Some(rolling) = self.rolling.as_mut() {
            rolling.active_records = 1;
        }
        // Land the header + size record now: a reader (delivery pump, a
        // scan right after a limit change) must never find the active file
        // shorter than its header.
        self.do_flush()?;
        self.publish_tracker();
        Ok(())
    }

    /// Delete the oldest closed segments until `need` more bytes fit under
    /// the session limit (or nothing closed is left).
    fn trim_to_fit(&mut self, need: u64) {
        while self.journal_bytes.saturating_add(need) > self.session_limit {
            if !self.trim_oldest_closed() {
                break;
            }
        }
    }

    /// Drop the oldest closed segment. Returns false when there is none
    /// left, or when the unlink failed — the bytes then stay counted
    /// against the session limit and the global budget, and the segment
    /// stays at the head of the closed deque so a later trim retries the
    /// delete. The tracker learns the new head *before* the file goes
    /// away, so no reader resolves an index that is already deleted; on a
    /// failed unlink the previous head is published back (the file never
    /// went away, so that instant is consistent with disk too).
    fn trim_oldest_closed(&mut self) -> bool {
        self.trim_oldest_closed_keeping(0)
    }

    /// Rotation under a full global budget with no older history left: the
    /// segment that just closed pays for the new segment's opening. It is
    /// deleted like any trimmed head, and `opening` bytes of its reservation
    /// carry over to the new file instead of returning to the budget, so
    /// disk usage only shrinks and no other writer can take those bytes in
    /// between. If the unlink fails, the segment stays counted and queued
    /// for a later trim, and the opening is adopted over the cap anyway:
    /// those few bytes (once per stuck segment, since the fresh segment is
    /// not closable) keep the active file well-formed, where a headerless
    /// file would be silently corrupted by the next append. Appends then
    /// refuse with `GlobalCap` until the segment can go.
    fn pay_opening_with_closed(&mut self, opening: u64) {
        if !self.trim_oldest_closed_keeping(opening) {
            lock_budget(&self.global).seed_used(opening);
        }
    }

    /// [`JournalWriter::trim_oldest_closed`] that keeps `keep` bytes of the
    /// deleted segment's reservation for the caller instead of returning
    /// them to the global budget (the caller ensures the segment holds at
    /// least that many bytes).
    fn trim_oldest_closed_keeping(&mut self, keep: u64) -> bool {
        let segment = {
            let Some(rolling) = self.rolling.as_mut() else {
                return false;
            };
            let Some(segment) = rolling.closed.pop_front() else {
                return false;
            };
            rolling.dropped_bytes += segment.bytes;
            segment
        };
        let journal_bytes_before = self.journal_bytes;
        self.journal_bytes = self.journal_bytes.saturating_sub(segment.bytes);
        self.publish_tracker();
        if unlink_if_present(&segment.path) {
            // Unlink confirmed (or the file was already gone): only now may
            // the global budget give the bytes back.
            lock_budget(&self.global).release(segment.bytes.saturating_sub(keep));
            return true;
        }
        // The unlink failed (e.g. Windows refuses DeleteFile while another
        // handle is open without FILE_SHARE_DELETE). Releasing the bytes
        // anyway would silently lift the global disk cap, and forgetting
        // the segment would leak the file forever: restore the accounting
        // and put the segment back for the next trim to retry.
        if let Some(rolling) = self.rolling.as_mut() {
            rolling.dropped_bytes -= segment.bytes;
            rolling.closed.push_front(segment);
        }
        self.journal_bytes = journal_bytes_before;
        self.publish_tracker();
        false
    }

    fn publish_tracker(&self) {
        if let Some(rolling) = self.rolling.as_ref() {
            rolling.tracker.publish(self.segment_snapshot());
        }
    }

    fn do_flush(&mut self) -> Result<(), JournalFlowError> {
        if !self.dirty {
            return Ok(());
        }
        self.buf
            .flush()
            .map_err(|source| self.write_io_error(source))?;
        self.dirty = false;
        Ok(())
    }

    fn write_io_error(&self, source: io::Error) -> JournalFlowError {
        if is_disk_full(&source) {
            JournalFlowError::DiskFull {
                path: self.path.clone(),
            }
        } else {
            JournalFlowError::Io {
                path: self.path.clone(),
                source,
            }
        }
    }
}

impl Drop for JournalWriter {
    fn drop(&mut self) {
        if let Err(error) = self.buf.flush() {
            tracing::warn!(path = %self.path.display(), %error, "journal flush on drop failed");
        }
    }
}

/// True when `path` is confirmed gone from disk: the unlink succeeded, or
/// the file was already absent (someone else cleaned it up — the bytes are
/// off the disk either way, so a caller may release them). False means the
/// unlink failed and the file is still there; on Windows this happens
/// whenever another same-user process holds the file open without
/// FILE_SHARE_DELETE, so the caller must keep the bytes counted and retry.
fn unlink_if_present(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "journal segment delete failed; bytes stay counted and the delete is retried on a later trim"
            );
            false
        }
    }
}

/// True when an I/O error indicates the disk ran out of space
/// (`ERROR_DISK_FULL` 112 on Windows, `ENOSPC` 28 on Unix, plus the
/// corresponding `ErrorKind`s).
fn is_disk_full(error: &io::Error) -> bool {
    let kind = error.kind();
    if matches!(kind, io::ErrorKind::StorageFull | io::ErrorKind::WriteZero) {
        return true;
    }
    let code = error.raw_os_error();
    (cfg!(windows) && code == Some(112)) || (cfg!(unix) && code == Some(28))
}

/// What a scan step found.
enum Scanned {
    Record(JournalRecord),
    CleanEof,
    TailTruncated,
    Corrupt { at_seq: u64 },
}

/// Incremental record scanner over any reader; verifies framing, seq
/// contiguity, and CRCs record by record.
struct RecordScanner<R> {
    reader: R,
    path: PathBuf,
    next_seq: u64,
    /// Bytes consumed from `reader` so far (record boundaries only advance
    /// by complete records; used for seq→offset cursors).
    consumed: u64,
    /// File-head scan: the first record's seq becomes the continuity
    /// anchor (a rotated segment starts mid-stream) and that record must
    /// be a size record.
    unseeded: bool,
}

impl<R: Read> RecordScanner<R> {
    /// Byte offset of the next record boundary (bytes consumed so far).
    fn boundary_offset(&self) -> u64 {
        self.consumed
    }

    /// Scan from a file head: continuity anchors at the first record.
    fn new(reader: R, path: PathBuf) -> Self {
        Self {
            reader,
            path,
            next_seq: 0,
            consumed: 0,
            unseeded: true,
        }
    }

    /// Continuity seeded at `next_seq` — for scans that resume at a known
    /// record boundary instead of the journal start.
    fn new_seeded(reader: R, path: PathBuf, next_seq: u64) -> Self {
        Self {
            reader,
            path,
            next_seq,
            consumed: 0,
            unseeded: false,
        }
    }

    fn io_error(&self, source: io::Error) -> JournalFlowError {
        JournalFlowError::Io {
            path: self.path.clone(),
            source,
        }
    }

    fn scan_next(&mut self) -> Result<Scanned, JournalFlowError> {
        let expected = self.next_seq;
        let mut len_buf = [0u8; 4];
        match fill_exact(&mut self.reader, &mut len_buf).map_err(|e| self.io_error(e))? {
            0 => return Ok(Scanned::CleanEof),
            4 => {}
            _ => return Ok(Scanned::TailTruncated),
        }
        let body_len = u32::from_le_bytes(len_buf);
        if !(MIN_BODY_LEN..=MAX_BODY_LEN).contains(&body_len) {
            return Ok(Scanned::Corrupt {
                at_seq: expected.max(1),
            });
        }
        let mut body = vec![0u8; body_len as usize];
        if fill_exact(&mut self.reader, &mut body).map_err(|e| self.io_error(e))?
            != body_len as usize
        {
            return Ok(Scanned::TailTruncated);
        }
        self.consumed += 4 + body_len as u64;

        let payload_end = body_len as usize - 4;
        let seq = u64::from_le_bytes(body[0..8].try_into().expect("8-byte slice"));
        let first_record = self.unseeded;
        let expected = if first_record {
            if seq == 0 || seq > SEQ_MAX {
                return Ok(Scanned::Corrupt { at_seq: 1 });
            }
            seq
        } else {
            expected
        };
        let corrupt = || Ok(Scanned::Corrupt { at_seq: expected });
        if seq != expected || seq > SEQ_MAX {
            return corrupt();
        }
        let kind = match JournalRecordKind::from_byte(body[8]) {
            Some(kind) => kind,
            None => return corrupt(),
        };
        let payload_len = payload_end - 9;
        if kind == JournalRecordKind::Resize && payload_len != RESIZE_PAYLOAD_LEN {
            return corrupt();
        }
        if (expected == 1 || first_record) && kind != JournalRecordKind::Resize {
            // Spec §5: the session's first record is the initial size — and
            // every segment file opens with its size, too.
            return corrupt();
        }
        let crc_stored =
            u32::from_le_bytes(body[payload_end..].try_into().expect("4-byte CRC slice"));
        if crc32fast::hash(&body[..payload_end]) != crc_stored {
            return corrupt();
        }
        self.next_seq = expected.saturating_add(1);
        self.unseeded = false;
        Ok(Scanned::Record(JournalRecord {
            seq,
            kind,
            payload: body[9..payload_end].to_vec(),
        }))
    }
}

/// Read into `buf` returning how many bytes arrived (short = EOF hit).
fn fill_exact<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Open a journal file, validate its 20-byte header (magic and, when given,
/// the session UUID), and position the reader at the first record. Returns
/// the reader, the header UUID, and the file length.
fn open_at_first_record(
    path: &Path,
    expected_uuid: Option<Uuid>,
) -> Result<(BufReader<File>, Uuid, u64), JournalFlowError> {
    let io = |source: io::Error| JournalFlowError::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = File::open(path).map_err(io)?;
    let file_len = file.metadata().map_err(io)?.len();
    let mut reader = BufReader::new(file);
    let mut header = [0u8; HEADER_LEN];
    if fill_exact(&mut reader, &mut header).map_err(io)? != HEADER_LEN {
        return Err(JournalFlowError::BadHeader {
            path: path.to_path_buf(),
            reason: "file is shorter than the 20-byte header",
        });
    }
    if header[..4] != MAGIC {
        return Err(JournalFlowError::BadHeader {
            path: path.to_path_buf(),
            reason: "magic is not MTJ1",
        });
    }
    let session_uuid = Uuid::from_bytes(header[4..].try_into().expect("16-byte UUID"));
    if let Some(expected) = expected_uuid {
        if session_uuid != expected {
            return Err(JournalFlowError::HeaderUuidMismatch {
                expected,
                found: session_uuid,
            });
        }
    }
    Ok((reader, session_uuid, file_len))
}

/// Streaming replay iterator produced by [`JournalReader::iterate`].
pub struct JournalRecordIter {
    scanner: RecordScanner<BufReader<File>>,
    emit_from: u64,
    emit_to: u64,
}

impl Iterator for JournalRecordIter {
    type Item = Result<JournalRecord, JournalFlowError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.scanner.next_seq > self.emit_to {
                return None;
            }
            match self.scanner.scan_next() {
                Ok(Scanned::Record(record)) => {
                    if record.seq < self.emit_from {
                        continue;
                    }
                    return Some(Ok(record));
                }
                Ok(Scanned::CleanEof) | Ok(Scanned::TailTruncated) => {
                    // The file changed since the scan classified it.
                    return Some(Err(JournalFlowError::Io {
                        path: self.scanner.path.clone(),
                        source: io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "journal shrank mid-replay",
                        ),
                    }));
                }
                Ok(Scanned::Corrupt { at_seq }) => {
                    return Some(Err(JournalFlowError::Corrupt { at_seq }));
                }
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

/// Scanned view of an MTJ1 journal file. Opening verifies the header and
/// walks every record; record-level damage is reported through
/// [`JournalReader::status`] rather than an open error, so callers can
/// replay the intact prefix deliberately.
pub struct JournalReader {
    path: PathBuf,
    session_uuid: Uuid,
    status: ScanStatus,
    record_count: u64,
    first_seq: u64,
    last_seq: u64,
    file_len: u64,
}

impl JournalReader {
    /// Open and scan, trusting any session UUID in the header.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalFlowError> {
        Self::open_inner(path.as_ref(), None)
    }

    /// Open and scan, requiring the header to carry `expected` session UUID.
    pub fn open_with_session(
        path: impl AsRef<Path>,
        expected: Uuid,
    ) -> Result<Self, JournalFlowError> {
        Self::open_inner(path.as_ref(), Some(expected))
    }

    fn open_inner(path: &Path, expected: Option<Uuid>) -> Result<Self, JournalFlowError> {
        let (reader, session_uuid, file_len) = open_at_first_record(path, expected)?;
        let mut scanner = RecordScanner::new(reader, path.to_path_buf());
        let mut status = ScanStatus::Ok;
        let mut record_count = 0u64;
        let mut first_seq = 0u64;
        let mut last_seq = 0u64;
        loop {
            match scanner.scan_next()? {
                Scanned::Record(record) => {
                    if record_count == 0 {
                        first_seq = record.seq;
                    }
                    record_count += 1;
                    last_seq = record.seq;
                }
                Scanned::CleanEof => break,
                Scanned::TailTruncated => {
                    status = ScanStatus::TailTruncated;
                    break;
                }
                Scanned::Corrupt { at_seq } => {
                    status = ScanStatus::Corrupt { at_seq };
                    break;
                }
            }
        }
        Ok(Self {
            path: path.to_path_buf(),
            session_uuid,
            status,
            record_count,
            first_seq,
            last_seq,
            file_len,
        })
    }

    pub fn status(&self) -> ScanStatus {
        self.status
    }

    /// True when the final record was incomplete (replay excludes it).
    pub fn tail_truncated(&self) -> bool {
        self.status == ScanStatus::TailTruncated
    }

    pub fn session_uuid(&self) -> Uuid {
        self.session_uuid
    }

    /// Number of verified records (the intact prefix).
    pub fn record_count(&self) -> u64 {
        self.record_count
    }

    /// Last verified seq (0 for an empty record area).
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Seq of the first record (0 for an empty record area; > 1 for a
    /// rotated segment that starts mid-stream).
    pub fn first_seq(&self) -> u64 {
        self.first_seq
    }

    /// File size in bytes at scan time.
    pub fn journal_bytes(&self) -> u64 {
        self.file_len
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Re-play records `from_seq..=to_seq` in exact original order (outputs
    /// and resizes interleaved by seq). `to_seq` is clamped to the last
    /// verified seq; a torn tail is silently excluded (check
    /// [`JournalReader::status`]). Requesting a range that intersects a
    /// corrupt region is an error — the damaged remainder is never returned
    /// as data.
    pub fn replay(
        &self,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<Vec<JournalRecord>, JournalFlowError> {
        self.iterate(from_seq, to_seq)?
            .collect::<Result<Vec<_>, _>>()
    }

    /// Streaming variant of [`JournalReader::replay`]; re-opens the file and
    /// yields records one at a time.
    pub fn iterate(
        &self,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<JournalRecordIter, JournalFlowError> {
        if let ScanStatus::Corrupt { at_seq } = self.status {
            if to_seq >= at_seq && from_seq <= to_seq {
                return Err(JournalFlowError::Corrupt { at_seq });
            }
        }
        let from = from_seq.max(1);
        let to = to_seq.min(self.last_seq);
        // Empty range: emit_to below any seq makes the iterator stop immediately.
        let to = if to < from { 0 } else { to };
        // Re-validate the header and start scanning at the first record.
        let (reader, _, _) = open_at_first_record(&self.path, Some(self.session_uuid))?;
        Ok(JournalRecordIter {
            scanner: RecordScanner::new(reader, self.path.clone()),
            emit_from: from,
            emit_to: to,
        })
    }

    /// Window read from the journal HEAD: `start_offset` must be
    /// [`HEADER_LEN`] (the first record boundary). Scans forward collecting
    /// up to `max_records` records with `from_seq..=to_seq`, returning each
    /// record WITH its start offset so callers can seed a seq→offset cursor
    /// for [`JournalReader::scan_window_resuming`].
    ///
    /// Seq continuity is anchored at the file's first record (which must be
    /// a size record), so a mid-file `start_offset` cannot work — it would
    /// be reported as corruption. Such a call is rejected up front with an
    /// `InvalidInput` I/O error instead.
    pub fn scan_window(
        path: impl AsRef<Path>,
        start_offset: u64,
        from_seq: u64,
        to_seq: u64,
        max_records: usize,
    ) -> Result<Vec<(JournalRecord, u64)>, JournalFlowError> {
        if start_offset != HEADER_LEN as u64 {
            return Err(JournalFlowError::Io {
                path: path.as_ref().to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "scan_window reads from the journal head only; \
                     use scan_window_resuming for a mid-file boundary",
                ),
            });
        }
        Self::scan_window_inner(path, start_offset, from_seq, to_seq, max_records, false)
    }

    /// Like [`JournalReader::scan_window`], but the seq-continuity check is
    /// seeded at `from_seq` — for incremental scans resuming at a known,
    /// previously verified record boundary (e.g. `session.search` batches).
    /// The returned offsets are **relative to `start_offset`** (bytes consumed
    /// from the seek position), matching `scan_window`.
    pub fn scan_window_resuming(
        path: impl AsRef<Path>,
        start_offset: u64,
        from_seq: u64,
        to_seq: u64,
        max_records: usize,
    ) -> Result<Vec<(JournalRecord, u64)>, JournalFlowError> {
        Self::scan_window_inner(path, start_offset, from_seq, to_seq, max_records, true)
    }

    fn scan_window_inner(
        path: impl AsRef<Path>,
        start_offset: u64,
        from_seq: u64,
        to_seq: u64,
        max_records: usize,
        resume_seeded: bool,
    ) -> Result<Vec<(JournalRecord, u64)>, JournalFlowError> {
        let file = std::fs::File::open(path.as_ref()).map_err(|e| JournalFlowError::Io {
            path: path.as_ref().to_path_buf(),
            source: e,
        })?;
        use std::io::Seek as _;
        let mut file = file;
        file.seek(std::io::SeekFrom::Start(start_offset))
            .map_err(|e| JournalFlowError::Io {
                path: path.as_ref().to_path_buf(),
                source: e,
            })?;
        let reader = std::io::BufReader::with_capacity(SCAN_BUFFER_BYTES, file);
        let mut scanner = if resume_seeded {
            RecordScanner::new_seeded(reader, path.as_ref().to_path_buf(), from_seq)
        } else {
            RecordScanner::new(reader, path.as_ref().to_path_buf())
        };
        let mut out = Vec::with_capacity(max_records.min(64));
        loop {
            if out.len() >= max_records {
                break;
            }
            let offset = scanner.boundary_offset();
            match scanner.scan_next()? {
                Scanned::Record(record) => {
                    let seq = record.seq;
                    if seq > to_seq {
                        break;
                    }
                    if seq >= from_seq {
                        out.push((record, offset));
                    }
                    if seq == to_seq {
                        break;
                    }
                }
                Scanned::CleanEof | Scanned::TailTruncated => break,
                Scanned::Corrupt { at_seq } => return Err(JournalFlowError::Corrupt { at_seq }),
            }
        }
        Ok(out)
    }

    /// Stream the records of one journal file (a legacy journal, or one
    /// segment of a rolling run) reading at most `read_cap` bytes from
    /// disk, header and buffer fills included, so the cap bounds real I/O
    /// rather than the bytes of the records decoded (`session.search`). One
    /// handle serves the whole file. The magic is checked, and the first
    /// record anchors seq continuity and must be a size record, as at any
    /// segment head. See [`BoundedRecordStream`].
    pub fn stream_bounded(
        path: impl AsRef<Path>,
        read_cap: u64,
    ) -> Result<BoundedRecordStream, JournalFlowError> {
        let path = path.as_ref();
        let io = |source: io::Error| JournalFlowError::Io {
            path: path.to_path_buf(),
            source,
        };
        let file = File::open(path).map_err(io)?;
        let file_len = file.metadata().map_err(io)?.len();
        let capacity = read_cap.clamp(1, SCAN_BUFFER_BYTES as u64) as usize;
        let mut reader = BufReader::with_capacity(capacity, file.take(read_cap));
        let mut header = [0u8; HEADER_LEN];
        let filled = fill_exact(&mut reader, &mut header).map_err(io)?;
        let mut stream = BoundedRecordStream {
            scanner: RecordScanner::new(reader, path.to_path_buf()),
            read_cap,
            file_len,
            pending: None,
            done: filled < HEADER_LEN,
        };
        let bad_header = |reason: &'static str| JournalFlowError::BadHeader {
            path: path.to_path_buf(),
            reason,
        };
        if filled < HEADER_LEN {
            // A cap that ends inside the header is a cut, not damage.
            if !stream.cap_reached() {
                stream.pending = Some(bad_header("file is shorter than the 20-byte header"));
            }
        } else if header[..4] != MAGIC {
            stream.pending = Some(bad_header("magic is not MTJ1"));
        }
        Ok(stream)
    }
}

/// Record stream produced by [`JournalReader::stream_bounded`]. Yields
/// verified records in file order and ends at a clean end of file, a torn
/// tail, or the read cap ([`BoundedRecordStream::cap_reached`] tells a
/// stream cut short from a finished one). Damage (bad header, CRC mismatch,
/// seq gap) is yielded once as an error, and the stream ends there.
pub struct BoundedRecordStream {
    scanner: RecordScanner<BufReader<io::Take<File>>>,
    read_cap: u64,
    /// File size when the stream opened.
    file_len: u64,
    pending: Option<JournalFlowError>,
    done: bool,
}

impl BoundedRecordStream {
    /// Bytes read from disk so far (header and buffer fills included).
    pub fn bytes_read(&self) -> u64 {
        self.read_cap - self.scanner.reader.get_ref().limit()
    }

    /// True when the read cap stopped the stream before the end of the
    /// file (as sized when the stream opened).
    pub fn cap_reached(&self) -> bool {
        self.scanner.reader.get_ref().limit() == 0 && self.bytes_read() < self.file_len
    }
}

impl Iterator for BoundedRecordStream {
    type Item = Result<JournalRecord, JournalFlowError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(error) = self.pending.take() {
            self.done = true;
            return Some(Err(error));
        }
        if self.done {
            return None;
        }
        let last = match self.scanner.scan_next() {
            Ok(Scanned::Record(record)) => return Some(Ok(record)),
            Ok(Scanned::CleanEof | Scanned::TailTruncated) => None,
            Ok(Scanned::Corrupt { at_seq }) => Some(Err(JournalFlowError::Corrupt { at_seq })),
            Err(error) => Some(Err(error)),
        };
        self.done = true;
        last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `retention.set_limit`: a raised cap applies to the live writer; a cap
    /// below the bytes already written rejects the next append (ceiling,
    /// never a trim).
    #[test]
    fn session_limit_can_be_raised_at_runtime() {
        let dir = std::env::temp_dir().join(format!("mtj-limit-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.mtj");
        let mut writer = JournalWriter::with_budget(
            &path,
            Uuid::new_v4(),
            64,
            GlobalJournalBudget::shared_default(),
        )
        .unwrap();
        writer.append_resize(80, 24).unwrap(); // header 20 + resize 21 = 41
        let err = writer.append_output(&[b'x'; 64]).unwrap_err();
        assert!(
            matches!(err, JournalFlowError::SessionCap { limit: 64 }),
            "{err}"
        );
        writer.set_session_limit(1024);
        assert_eq!(writer.session_limit(), 1024);
        writer.append_output(&[b'x'; 64]).unwrap();
        writer.set_session_limit(writer.journal_bytes());
        let err = writer.append_output(b"y").unwrap_err();
        assert!(matches!(err, JournalFlowError::SessionCap { .. }), "{err}");
        drop(writer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crc32fast_is_ieee_crc32() {
        // Canonical CRC-32/IEEE-HDLC check value.
        assert_eq!(crc32fast::hash(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn encode_record_matches_spec_layout() {
        let frame = encode_record(1, JournalRecordKind::Resize, &[80, 0, 24, 0]);
        assert_eq!(frame.len(), 21);
        let mut expected = Vec::new();
        expected.extend_from_slice(&17u32.to_le_bytes()); // body_len = 8+1+4+4
        expected.extend_from_slice(&1u64.to_le_bytes()); // seq
        expected.push(2); // kind = resize
        expected.extend_from_slice(&80u16.to_le_bytes()); // cols
        expected.extend_from_slice(&24u16.to_le_bytes()); // rows
        expected.extend_from_slice(&crc32fast::hash(&expected[4..]).to_le_bytes());
        assert_eq!(frame, expected);
        assert_eq!(&frame[0..4], &[17, 0, 0, 0]);
        assert_eq!(frame[12], 2);
        assert_eq!(&frame[13..17], &[80, 0, 24, 0]);
    }

    #[test]
    fn encode_output_body_len_cap_is_exact() {
        let frame = encode_record(7, JournalRecordKind::Output, &vec![0u8; MAX_OUTPUT_PAYLOAD]);
        assert_eq!(
            u32::from_le_bytes(frame[0..4].try_into().unwrap()),
            MAX_BODY_LEN
        );
        assert_eq!(frame.len(), 4 + MAX_BODY_LEN as usize);
    }

    #[test]
    fn seq_guard_stops_at_contracts_bound() {
        assert_eq!(SEQ_MAX, i64::MAX as u64);
        let mut writer =
            JournalWriter::open(std::env::temp_dir().join("x.mtj"), Uuid::new_v4()).expect("open");
        writer.last_seq = SEQ_MAX - 1;
        assert_eq!(writer.next_seq().unwrap(), SEQ_MAX);
        writer.last_seq = SEQ_MAX;
        assert!(matches!(
            writer.next_seq(),
            Err(JournalFlowError::SeqExhausted)
        ));
        let _ = std::fs::remove_file(writer.path());
    }

    #[test]
    fn writer_rejects_output_before_initial_size_and_oversized_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.mtj");
        let mut writer = JournalWriter::open(&path, Uuid::new_v4()).unwrap();
        assert!(matches!(
            writer.append_output(b"early"),
            Err(JournalFlowError::FirstRecordNotSize)
        ));
        assert!(matches!(writer.append_resize(2, 2), Ok(1),));
        let big = vec![0u8; MAX_OUTPUT_PAYLOAD + 1];
        assert!(matches!(
            writer.append_output(&big),
            Err(JournalFlowError::PayloadTooLarge { len }) if len == MAX_OUTPUT_PAYLOAD + 1
        ));
        assert_eq!(writer.last_seq(), 1); // failed appends burn no seq
        assert_eq!(writer.journal_bytes(), HEADER_LEN as u64 + 21);
    }

    /// `scan_window` anchors seq continuity at 1, so it is head-only; a
    /// mid-file boundary is rejected up front (it used to surface as
    /// `Corrupt{at_seq: 1}`), and `scan_window_resuming` serves that case.
    #[test]
    fn scan_window_is_head_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.mtj");
        let uuid = Uuid::new_v4();
        {
            let mut writer = JournalWriter::open(&path, uuid).unwrap();
            writer.append_resize(80, 24).unwrap();
            writer.append_output(b"a").unwrap();
            writer.append_output(b"b").unwrap();
        }
        let head = HEADER_LEN as u64;
        let all = JournalReader::scan_window(&path, head, 1, 3, 10).unwrap();
        assert_eq!(
            all.iter().map(|(r, _)| r.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        // Record 1 (resize) is 21 bytes: the boundary of record 2.
        let second = head + 21;
        assert_eq!(all[1].1, 21, "offsets are relative to the scan start");
        match JournalReader::scan_window(&path, second, 2, 3, 10) {
            Err(JournalFlowError::Io { source, .. }) => {
                assert_eq!(source.kind(), io::ErrorKind::InvalidInput)
            }
            other => panic!("mid-file scan_window must be rejected, got {other:?}"),
        }
        let tail = JournalReader::scan_window_resuming(&path, second, 2, 3, 10).unwrap();
        assert_eq!(
            tail.iter().map(|(r, _)| r.seq).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn disk_full_io_errors_are_mapped() {
        assert!(is_disk_full(&io::Error::new(
            io::ErrorKind::StorageFull,
            "no space",
        )));
        if cfg!(windows) {
            assert!(is_disk_full(&io::Error::from_raw_os_error(112)));
            assert!(!is_disk_full(&io::Error::from_raw_os_error(5)));
        }
        assert!(!is_disk_full(&io::Error::new(
            io::ErrorKind::PermissionDenied,
            "denied",
        )));
    }

    // -- rolling (segmented) mode ------------------------------------------

    use crate::segments::{journal_files_bytes, scan_window_segments, JournalSet, SegmentCursor};

    fn rolling(path: &Path, limit: u64, global: Arc<Mutex<GlobalJournalBudget>>) -> JournalWriter {
        JournalWriter::open_with(
            path,
            Uuid::new_v4(),
            JournalOptions {
                session_limit: limit,
                segment_cap: Some(DEFAULT_SEGMENT_BYTES),
            },
            global,
        )
        .unwrap()
    }

    #[test]
    fn segment_target_is_an_eighth_clamped() {
        assert_eq!(
            segment_target_for(128 << 20, DEFAULT_SEGMENT_BYTES),
            16 << 20
        );
        assert_eq!(
            segment_target_for(1 << 20, DEFAULT_SEGMENT_BYTES),
            128 << 10
        );
        assert_eq!(
            segment_target_for(8192, DEFAULT_SEGMENT_BYTES),
            MIN_SEGMENT_BYTES
        );
        assert_eq!(segment_target_for(1 << 30, 1 << 20), 1 << 20);
        assert_eq!(segment_target_for(1 << 20, 1), MIN_SEGMENT_BYTES);
    }

    #[test]
    fn legacy_writer_reports_a_single_segment_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.mtj");
        let mut writer = JournalWriter::open(&path, Uuid::new_v4()).unwrap();
        writer.append_resize(80, 24).unwrap();
        writer.append_output(b"x").unwrap();
        assert!(!writer.is_rolling());
        assert!(writer.tracker().is_none());
        assert_eq!(writer.first_seq(), 1);
        assert_eq!(writer.dropped_bytes(), 0);
        let snap = writer.segment_snapshot();
        assert_eq!(
            (snap.active_index, snap.first_index, snap.first_seq),
            (0, 0, 1)
        );
        assert_eq!(snap.retained_bytes, writer.journal_bytes());
    }

    /// 32 KiB limit (4 KiB segments) under 200 KiB of output: the writer
    /// never refuses, retains ≤ the limit, moves the head, and what is left
    /// replays as one contiguous stream that opens with a size record.
    #[test]
    fn rolling_journal_rotates_trims_and_stays_replayable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roll.mtj");
        let global = GlobalJournalBudget::shared_default();
        let limit = 32 * 1024;
        let mut writer = rolling(&path, limit, global.clone());
        assert!(writer.is_rolling());
        assert_eq!(writer.segment_target(), Some(4096));
        writer.append_resize(80, 24).unwrap();
        let payload = vec![b'z'; 1000];
        let mut last = 1u64;
        for _ in 0..200 {
            let seq = writer.append_output(&payload).unwrap();
            assert!(seq > last, "seq must keep increasing across rotations");
            last = seq;
        }
        writer.finalize().unwrap();
        assert!(
            writer.journal_bytes() <= limit,
            "retained {}",
            writer.journal_bytes()
        );
        assert!(writer.first_seq() > 1);
        assert!(writer.dropped_bytes() > 0);
        assert_eq!(global.lock().unwrap().used(), writer.journal_bytes());
        assert_eq!(journal_files_bytes(&path), writer.journal_bytes());

        let set = JournalSet::open(&path).unwrap();
        assert!(set.segments().len() >= 2, "{:?}", set.segments());
        for segment in set.segments() {
            assert_eq!(segment.status, ScanStatus::Ok);
            let reader = JournalReader::open(&segment.path).unwrap();
            assert_eq!(reader.first_seq(), segment.first_seq);
            let first = reader.replay(segment.first_seq, segment.first_seq).unwrap();
            assert_eq!(first.len(), 1);
            assert_eq!(
                first[0].resize_dims(),
                Some((80, 24)),
                "segments open with the size"
            );
        }
        assert_eq!(set.first_seq(), writer.first_seq());
        assert_eq!(set.last_seq(), writer.last_seq());
        assert_eq!(set.retained_bytes(), writer.journal_bytes());

        let records = set.replay(set.first_seq(), set.last_seq()).unwrap();
        assert_eq!(records[0].kind, JournalRecordKind::Resize);
        for (i, record) in records.iter().enumerate() {
            assert_eq!(record.seq, set.first_seq() + i as u64);
        }
        assert_eq!(records.last().unwrap().seq, set.last_seq());
        let outputs = records
            .iter()
            .filter(|r| r.kind == JournalRecordKind::Output)
            .count();
        assert!(outputs >= 20, "at least 7/8 of the limit stays: {outputs}");
        assert!(records
            .iter()
            .filter(|r| r.kind == JournalRecordKind::Output)
            .all(|r| r.payload == payload));
        assert!(matches!(
            set.replay(1, set.last_seq()),
            Err(JournalFlowError::HeadTrimmed { .. })
        ));

        let snap = writer.tracker().unwrap().snapshot();
        assert_eq!(snap, writer.segment_snapshot());
        assert_eq!(snap.first_seq, set.first_seq());
        assert_eq!(snap.active_index, set.segments().last().unwrap().index);
        assert_eq!(snap.first_index, set.segments()[0].index);
        assert_eq!(snap.retained_bytes, writer.journal_bytes());
    }

    /// The delivery pump's read pattern: windows resumed at the previous
    /// record's cursor walk across segment boundaries without a gap, and a
    /// cursor behind the head is refused rather than served from the wrong file.
    #[test]
    fn rolling_window_scan_crosses_segments_and_refuses_trimmed_cursors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("win.mtj");
        let mut writer = rolling(&path, 16 * 1024, GlobalJournalBudget::shared_default());
        writer.append_resize(100, 30).unwrap();
        let payload = vec![b'w'; 1000];
        for _ in 0..40 {
            writer.append_output(&payload).unwrap();
        }
        writer.flush_now().unwrap();
        let head = writer.tracker().unwrap().snapshot();
        assert!(head.first_index > 0 && head.first_seq > 1, "{head:?}");

        let mut cursor = SegmentCursor::head_of(head.first_index);
        let mut seed = head.first_seq;
        let mut seqs: Vec<u64> = Vec::new();
        loop {
            let window = scan_window_segments(&path, &head, cursor, seed, u64::MAX, 5).unwrap();
            let fresh: Vec<_> = window
                .iter()
                .filter(|(record, _)| seqs.last().is_none_or(|last| record.seq > *last))
                .collect();
            if fresh.is_empty() {
                break;
            }
            for (record, _) in &fresh {
                seqs.push(record.seq);
            }
            let (last_record, last_cursor) = fresh.last().unwrap();
            cursor = *last_cursor;
            seed = last_record.seq;
        }
        assert_eq!(seqs.first().copied(), Some(head.first_seq));
        assert_eq!(seqs.last().copied(), Some(writer.last_seq()));
        for pair in seqs.windows(2) {
            assert_eq!(pair[1], pair[0] + 1);
        }
        assert!(matches!(
            scan_window_segments(&path, &head, SegmentCursor::head_of(0), 1, u64::MAX, 5),
            Err(JournalFlowError::HeadTrimmed { first_seq }) if first_seq == head.first_seq
        ));
        assert!(matches!(
            scan_window_segments(&path, &head, cursor, head.first_seq - 1, u64::MAX, 5),
            Err(JournalFlowError::HeadTrimmed { .. })
        ));
    }

    #[test]
    fn rolling_global_pressure_trims_own_history_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("global.mtj");
        let global = GlobalJournalBudget::shared(20 * 1024);
        let mut writer = rolling(&path, 64 * 1024, global.clone()); // 8 KiB segments
        writer.append_resize(80, 24).unwrap();
        let payload = vec![b'g'; 1000];
        for _ in 0..100 {
            writer.append_output(&payload).unwrap();
            assert!(global.lock().unwrap().used() <= 20 * 1024);
        }
        assert!(writer.journal_bytes() <= 20 * 1024);
        assert!(writer.first_seq() > 1);

        // A lone active segment has nothing to sacrifice: that corner keeps
        // the legacy stop.
        let lone = dir.path().join("lone.mtj");
        let mut lone_writer = rolling(&lone, 64 * 1024, GlobalJournalBudget::shared(2 * 1024));
        lone_writer.append_resize(80, 24).unwrap();
        let mut failed = None;
        for _ in 0..10 {
            if let Err(error) = lone_writer.append_output(&payload) {
                failed = Some(error);
                break;
            }
        }
        assert!(
            matches!(failed, Some(JournalFlowError::GlobalCap { .. })),
            "{failed:?}"
        );
    }

    /// A failed segment delete must not release bytes from the global
    /// budget nor forget the segment: the unlink is provoked to fail by
    /// making the journal directory non-writable, and the retry (after
    /// permissions are restored) must then complete the trim.
    #[test]
    #[cfg(unix)]
    fn failed_segment_delete_keeps_bytes_counted_and_retries() {
        if unsafe { libc::geteuid() } == 0 {
            // Root ignores directory permissions, so the failure path
            // cannot be provoked reliably.
            return;
        }
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stuck.mtj");
        let global = GlobalJournalBudget::shared(1 << 20);
        let mut writer = rolling(&path, 64 * 1024, global.clone()); // 8 KiB segments
        writer.append_resize(80, 24).unwrap();
        let payload = vec![b's'; 1000];
        for _ in 0..50 {
            writer.append_output(&payload).unwrap();
        }
        writer.flush_now().unwrap();
        // Closed segments exist but nothing is trimmed yet.
        assert_eq!(writer.first_seq(), 1);
        assert_eq!(writer.dropped_bytes(), 0);
        let before_bytes = writer.journal_bytes();
        let before_used = global.lock().unwrap().used();

        // Non-writable directory: unlink (and the rotate fallback) fail.
        let perms = std::fs::metadata(dir.path()).unwrap().permissions();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        writer.set_session_limit(16 * 1024);
        assert_eq!(
            writer.journal_bytes(),
            before_bytes,
            "a failed unlink must not free session bytes"
        );
        assert_eq!(writer.dropped_bytes(), 0);
        assert_eq!(writer.first_seq(), 1);
        assert_eq!(
            global.lock().unwrap().used(),
            before_used,
            "a failed unlink must not release the global budget"
        );

        // Permissions restored: the retained segment is retried and the
        // trim now completes.
        std::fs::set_permissions(dir.path(), perms).unwrap();
        writer.set_session_limit(16 * 1024);
        writer.flush_now().unwrap();
        assert!(writer.journal_bytes() <= 16 * 1024);
        assert!(writer.dropped_bytes() > 0);
        assert!(writer.first_seq() > 1);
        assert_eq!(global.lock().unwrap().used(), writer.journal_bytes());
        assert_eq!(journal_files_bytes(&path), writer.journal_bytes());
    }

    #[test]
    fn rolling_set_limit_lowering_trims_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lower.mtj");
        let mut writer = rolling(&path, 64 * 1024, GlobalJournalBudget::shared_default()); // 8 KiB segments
        writer.append_resize(80, 24).unwrap();
        let payload = vec![b'l'; 1000];
        for _ in 0..50 {
            writer.append_output(&payload).unwrap();
        }
        assert_eq!(writer.first_seq(), 1);
        assert_eq!(writer.dropped_bytes(), 0);
        let before = writer.journal_bytes();
        writer.set_session_limit(16 * 1024);
        writer.flush_now().unwrap();
        assert!(writer.journal_bytes() <= 16 * 1024);
        assert!(writer.first_seq() > 1);
        assert_eq!(writer.dropped_bytes(), before - writer.journal_bytes());
        assert_eq!(journal_files_bytes(&path), writer.journal_bytes());
        assert_eq!(writer.segment_target(), Some(MIN_SEGMENT_BYTES));
        // Raising only allows growth.
        writer.set_session_limit(1 << 20);
        let retained = writer.journal_bytes();
        writer.append_output(&payload).unwrap();
        assert!(writer.journal_bytes() > retained);
        assert_eq!(
            writer.tracker().unwrap().snapshot(),
            writer.segment_snapshot()
        );
    }

    /// Size-only floods rotate too (a segment closes once it holds more than
    /// its leading size record), so a rotation can never roll straight into
    /// another and the limit still holds.
    #[test]
    fn rolling_rotation_waits_for_a_second_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sizes.mtj");
        let mut writer = rolling(&path, 8 * 1024, GlobalJournalBudget::shared_default()); // 4 KiB segments
        for i in 0..600u16 {
            writer.append_resize(80 + i % 3, 24).unwrap();
        }
        writer.finalize().unwrap();
        assert!(writer.journal_bytes() <= 8 * 1024);
        assert!(writer.first_seq() > 1);
        let set = JournalSet::open(&path).unwrap();
        assert!(set.segments().len() <= 3, "{:?}", set.segments());
        for segment in set.segments() {
            assert_eq!(segment.status, ScanStatus::Ok);
        }
        let records = set.replay(set.first_seq(), set.last_seq()).unwrap();
        assert_eq!(records.last().unwrap().seq, writer.last_seq());
    }

    /// Fill a rolling writer's budget to its limit (standing in for other
    /// sessions and seeded journals) and return that limit.
    fn fill_budget(global: &Arc<Mutex<GlobalJournalBudget>>) -> u64 {
        let mut budget = global.lock().unwrap();
        let free = budget.limit() - budget.used();
        budget.seed_used(free);
        budget.limit()
    }

    /// A rotation the global budget refuses must leave the writer exactly
    /// as it was. The reservation used to happen after the rename: with the
    /// budget full and the oldest closed segment stuck (a directory in its
    /// place stands in for an undeletable file), the writer was left with
    /// an empty, headerless active file that the next append corrupted.
    #[test]
    fn rotation_refused_by_the_global_budget_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("refused.mtj");
        let global = GlobalJournalBudget::shared(1 << 20);
        let mut writer = rolling(&path, 64 * 1024, global.clone()); // 8 KiB segments
        writer.append_resize(80, 24).unwrap();
        let payload = vec![b'r'; 1000];
        // Segment 0 closes at the 9th output; 16 fill the next one again.
        for _ in 0..16 {
            writer.append_output(&payload).unwrap();
        }
        writer.flush_now().unwrap();
        let full_segment = 41 + 8 * 1017;
        assert_eq!(writer.journal_bytes(), 2 * full_segment);
        let before_seq = writer.last_seq();
        std::fs::remove_file(segment_path(&path, 0)).unwrap();
        std::fs::create_dir(segment_path(&path, 0)).unwrap();
        let limit = fill_budget(&global);

        let err = writer.append_output(&payload).unwrap_err();
        assert!(matches!(err, JournalFlowError::GlobalCap { .. }), "{err}");
        assert!(
            !segment_path(&path, 1).exists(),
            "nothing is renamed before the reservation"
        );
        assert_eq!(writer.journal_bytes(), 2 * full_segment);
        assert_eq!(writer.last_seq(), before_seq);
        assert_eq!(global.lock().unwrap().used(), limit);
        let active = JournalReader::open(&path).unwrap();
        assert_eq!(active.status(), ScanStatus::Ok);
        assert_eq!(active.last_seq(), before_seq);
        assert_eq!(active.journal_bytes(), full_segment);

        // Once space frees up, the same append rotates cleanly.
        std::fs::remove_dir(segment_path(&path, 0)).unwrap();
        global.lock().unwrap().release(64 * 1024);
        let seq = writer.append_output(&payload).unwrap();
        // The new segment's size record takes one seq.
        assert_eq!(seq, before_seq + 2);
        writer.finalize().unwrap();
        let active = JournalReader::open(&path).unwrap();
        assert_eq!(active.status(), ScanStatus::Ok);
        assert_eq!(active.first_seq(), before_seq + 1);
        assert_eq!(active.last_seq(), seq);
        let closed = std::fs::metadata(segment_path(&path, 1)).unwrap();
        assert_eq!(closed.len(), full_segment);
    }

    /// With the budget full and nothing older to trim, a rotation spends the
    /// segment it closes: that segment is deleted, part of its reservation
    /// pays for the new segment's opening, and the append goes through with
    /// disk usage only shrinking.
    #[test]
    fn rotation_under_a_full_budget_spends_the_closing_segment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spent.mtj");
        let global = GlobalJournalBudget::shared(1 << 20);
        let mut writer = rolling(&path, 64 * 1024, global.clone()); // 8 KiB segments
        writer.append_resize(80, 24).unwrap();
        let payload = vec![b's'; 1000];
        for _ in 0..8 {
            writer.append_output(&payload).unwrap();
        }
        let closing = writer.journal_bytes();
        assert_eq!(closing, 41 + 8 * 1017);
        let limit = fill_budget(&global);

        let seq = writer.append_output(&payload).unwrap();
        assert_eq!(seq, 11);
        assert!(!segment_path(&path, 0).exists());
        assert_eq!(writer.first_seq(), 10);
        assert_eq!(writer.dropped_bytes(), closing);
        assert_eq!(writer.journal_bytes(), 41 + 1017);
        writer.finalize().unwrap();
        let used = global.lock().unwrap().used();
        assert_eq!(used, limit - closing + writer.journal_bytes());
        assert_eq!(journal_files_bytes(&path), writer.journal_bytes());
        let active = JournalReader::open(&path).unwrap();
        assert_eq!(active.status(), ScanStatus::Ok);
        assert_eq!((active.first_seq(), active.last_seq()), (10, 11));
    }

    /// `session.search` reads through a capped stream: the cap bounds the
    /// bytes pulled from disk (header and buffer fills included), records
    /// come back in order until the cap cuts one, and `cap_reached` tells a
    /// stream cut short from a finished one.
    #[test]
    fn bounded_stream_caps_disk_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bounded.mtj");
        {
            let mut writer = JournalWriter::open(&path, Uuid::new_v4()).unwrap();
            writer.append_resize(80, 24).unwrap();
            for _ in 0..100 {
                writer.append_output(&[b'b'; 100]).unwrap();
            }
        }
        let file_len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(file_len, 41 + 100 * 117);

        let mut whole = JournalReader::stream_bounded(&path, u64::MAX).unwrap();
        let seqs: Vec<u64> = whole.by_ref().map(|r| r.unwrap().seq).collect();
        assert_eq!(seqs, (1..=101).collect::<Vec<u64>>());
        assert_eq!(whole.bytes_read(), file_len);
        assert!(!whole.cap_reached());

        // 1 KiB: header + size record (41) + 8 outputs (936); the 9th is cut.
        let mut capped = JournalReader::stream_bounded(&path, 1024).unwrap();
        let records: Vec<_> = capped.by_ref().map(Result::unwrap).collect();
        assert_eq!(records.len(), 9);
        assert_eq!(capped.bytes_read(), 1024);
        assert!(capped.cap_reached());

        // A cap that ends inside the header is a cut, not damage.
        let mut tiny = JournalReader::stream_bounded(&path, 10).unwrap();
        assert!(tiny.next().is_none());
        assert!(tiny.cap_reached());

        let junk = dir.path().join("junk.mtj");
        std::fs::write(&junk, [0u8; 64]).unwrap();
        let mut stream = JournalReader::stream_bounded(&junk, u64::MAX).unwrap();
        assert!(matches!(
            stream.next(),
            Some(Err(JournalFlowError::BadHeader { .. }))
        ));
        assert!(stream.next().is_none());
    }
}
