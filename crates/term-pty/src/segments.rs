//! Rolling (segmented) journal helpers — the on-disk layout and the readers
//! that follow it (spec `02-runner.md` §5, 롤링 저널).
//!
//! A session's journal is a run of MTJ1 files sharing one base path:
//!
//! ```text
//! <base>          the ACTIVE segment (the writer appends here)
//! <base>.<k>      CLOSED segments, k = rotation index (older = smaller k)
//! ```
//!
//! Rotation renames the active file to `<base>.<k>` and starts a fresh
//! `<base>` whose first record is a size record (the size current at
//! rotation), so every retained segment replays on its own. `seq` keeps
//! counting across segments — the run is one contiguous stream whose head
//! moves forward as the oldest closed segments are deleted to stay under the
//! session limit. Readers resolve an index **closed-first**
//! ([`resolve_segment_path`]): `<base>.<k>` if it exists, else `<base>`.
//! That makes the rename race harmless — a reader holding index `k` finds
//! the same bytes before and after the rotation.
//!
//! [`SegmentTracker`] is the writer's live head/tail summary shared with the
//! daemon (attach start seq, dropped bytes) without locking the writer.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::journal::{JournalFlowError, JournalReader, JournalRecord, ScanStatus, HEADER_LEN};

/// Point-in-time view of a rolling journal's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentSnapshot {
    /// Rotation index of the active (`<base>`) segment.
    pub active_index: u64,
    /// Rotation index of the oldest retained segment.
    pub first_index: u64,
    /// First retained seq (1 until the head has been trimmed).
    pub first_seq: u64,
    /// Bytes deleted from the head so far (closed segments dropped).
    pub dropped_bytes: u64,
    /// Bytes currently on disk across all retained segments.
    pub retained_bytes: u64,
}

impl Default for SegmentSnapshot {
    fn default() -> Self {
        Self {
            active_index: 0,
            first_index: 0,
            first_seq: 1,
            dropped_bytes: 0,
            retained_bytes: 0,
        }
    }
}

/// Lock-free head/tail summary published by the writer on every rotation
/// and trim. Readers take a [`SegmentSnapshot`] and act on that.
#[derive(Debug)]
pub struct SegmentTracker {
    active_index: AtomicU64,
    first_index: AtomicU64,
    first_seq: AtomicU64,
    dropped_bytes: AtomicU64,
    retained_bytes: AtomicU64,
}

impl Default for SegmentTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl SegmentTracker {
    pub fn new() -> Self {
        Self {
            active_index: AtomicU64::new(0),
            first_index: AtomicU64::new(0),
            first_seq: AtomicU64::new(1),
            dropped_bytes: AtomicU64::new(0),
            retained_bytes: AtomicU64::new(0),
        }
    }

    /// A tracker frozen at `snapshot` (sessions whose writer is gone).
    pub fn frozen(snapshot: SegmentSnapshot) -> Self {
        let tracker = Self::new();
        tracker.publish(snapshot);
        tracker
    }

    pub fn snapshot(&self) -> SegmentSnapshot {
        SegmentSnapshot {
            active_index: self.active_index.load(Ordering::Acquire),
            first_index: self.first_index.load(Ordering::Acquire),
            first_seq: self.first_seq.load(Ordering::Acquire),
            dropped_bytes: self.dropped_bytes.load(Ordering::Acquire),
            retained_bytes: self.retained_bytes.load(Ordering::Acquire),
        }
    }

    pub fn first_seq(&self) -> u64 {
        self.first_seq.load(Ordering::Acquire)
    }

    pub fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes.load(Ordering::Acquire)
    }

    pub(crate) fn publish(&self, snapshot: SegmentSnapshot) {
        // Head first: a reader must never see a first_index whose file is
        // already gone (the writer publishes before it deletes).
        self.first_index
            .store(snapshot.first_index, Ordering::Release);
        self.first_seq.store(snapshot.first_seq, Ordering::Release);
        self.active_index
            .store(snapshot.active_index, Ordering::Release);
        self.dropped_bytes
            .store(snapshot.dropped_bytes, Ordering::Release);
        self.retained_bytes
            .store(snapshot.retained_bytes, Ordering::Release);
    }
}

/// `<base>.<index>` — the name a closed segment gets at rotation.
pub fn segment_path(base: &Path, index: u64) -> PathBuf {
    let mut os = base.as_os_str().to_os_string();
    os.push(format!(".{index}"));
    PathBuf::from(os)
}

/// Closed-first resolution of a rotation index: the closed file if it
/// exists, otherwise the active file (`base`). See the module docs for why
/// this ordering makes the rename race harmless.
pub fn resolve_segment_path(base: &Path, index: u64) -> PathBuf {
    let closed = segment_path(base, index);
    if closed.is_file() {
        closed
    } else {
        base.to_path_buf()
    }
}

/// Closed segments of `base` on disk, ascending by index.
pub fn list_closed_segments(base: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
    let Some(dir) = base.parent() else {
        return Ok(Vec::new());
    };
    let Some(stem) = base.file_name().and_then(|n| n.to_str()) else {
        return Ok(Vec::new());
    };
    let prefix = format!("{stem}.");
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        // Exactly the digits of an index — `x.mtj.bak` is not a segment.
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(index) = suffix.parse::<u64>() else {
            continue;
        };
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            found.push((index, entry.path()));
        }
    }
    found.sort_by_key(|(index, _)| *index);
    Ok(found)
}

/// Every file of the journal run (closed segments ascending, then the
/// active file if present) — what retention deletes.
pub fn journal_files(base: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = list_closed_segments(base)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, path)| path)
        .collect();
    if base.is_file() {
        files.push(base.to_path_buf());
    }
    files
}

/// Total on-disk bytes of the run (0 when nothing exists).
pub fn journal_files_bytes(base: &Path) -> u64 {
    journal_files(base)
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// Total on-disk bytes of every journal file directly inside `dir`. The
/// journals directory is flat, so this is the sum over all sessions of
/// what [`journal_files_bytes`] reports per base: `<id>.mtj` active files
/// and `<id>.mtj.<k>` closed segments. Names are filtered with the same
/// digit-suffix rule [`list_closed_segments`] applies to segments, so
/// unrelated files sharing the directory do not count. Metadata sizes
/// only — no content reads — and a missing or unreadable directory is 0.
pub fn journal_dir_bytes(dir: &Path) -> u64 {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return 0,
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let is_base = name.len() > 4 && name.ends_with(".mtj");
        let is_segment = match name.find(".mtj.") {
            // Only digits may follow `<base>.mtj.` — `x.mtj.bak` is not a
            // segment (same rule as `list_closed_segments`).
            Some(at) => {
                let suffix = &name[at + ".mtj.".len()..];
                !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
            }
            None => false,
        };
        if !is_base && !is_segment {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

/// Absolute position of a record inside the run: rotation index + byte
/// offset within that segment file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentCursor {
    pub index: u64,
    pub offset: u64,
}

impl SegmentCursor {
    /// First record boundary of segment `index`.
    pub fn head_of(index: u64) -> Self {
        Self {
            index,
            offset: HEADER_LEN as u64,
        }
    }
}

/// Segment-aware incremental window read (delivery pump, search).
///
/// Resumes at `cursor` — a record boundary whose record has seq
/// `seed_seq` (or the segment head with `seed_seq` = that segment's first
/// seq) — and collects up to `max_records` records with
/// `seed_seq..=to_seq`, crossing from a closed segment into the next one.
/// Each record comes back with its own absolute cursor so callers can seed
/// a seq→cursor checkpoint map. Reaching the end of the active file ends
/// the window (more may arrive later).
///
/// A cursor/seed behind the retained head fails with
/// [`JournalFlowError::HeadTrimmed`]: those bytes are gone and the caller
/// must restart from `head.first_seq` (a view that far behind re-attaches).
pub fn scan_window_segments(
    base: &Path,
    head: &SegmentSnapshot,
    cursor: SegmentCursor,
    seed_seq: u64,
    to_seq: u64,
    max_records: usize,
) -> Result<Vec<(JournalRecord, SegmentCursor)>, JournalFlowError> {
    if cursor.index < head.first_index || seed_seq < head.first_seq {
        return Err(JournalFlowError::HeadTrimmed {
            first_seq: head.first_seq,
        });
    }
    let mut out: Vec<(JournalRecord, SegmentCursor)> = Vec::new();
    let mut index = cursor.index;
    let mut offset = cursor.offset;
    let mut seed = seed_seq;
    loop {
        if out.len() >= max_records || seed > to_seq {
            break;
        }
        let path = resolve_segment_path(base, index);
        let window = match JournalReader::scan_window_resuming(
            &path,
            offset,
            seed,
            to_seq,
            max_records - out.len(),
        ) {
            Ok(window) => window,
            // The active file is momentarily absent between the rotation's
            // rename and the new create: nothing to read yet, try later.
            Err(JournalFlowError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound
                    && !segment_path(base, index).is_file() =>
            {
                break;
            }
            Err(error) => return Err(error),
        };
        for (record, relative) in window {
            seed = record.seq + 1;
            out.push((
                record,
                SegmentCursor {
                    index,
                    offset: offset + relative,
                },
            ));
        }
        if out.len() >= max_records || seed > to_seq {
            break;
        }
        // This file is exhausted (possibly resumed at its exact end). A
        // closed segment continues in the next one; the active file is
        // simply the current end of the stream.
        if !segment_path(base, index).is_file() {
            break;
        }
        index += 1;
        offset = HEADER_LEN as u64;
    }
    Ok(out)
}

/// One retained segment as scanned from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentInfo {
    pub index: u64,
    pub path: PathBuf,
    /// Seq of the first record (1 for the original head).
    pub first_seq: u64,
    /// Last verified seq (0 for an empty record area).
    pub last_seq: u64,
    pub status: ScanStatus,
    /// File size at scan time.
    pub bytes: u64,
    /// The `<base>` file (the writer's current segment).
    pub active: bool,
}

/// Scanned view of a whole journal run — the segment-aware counterpart of
/// [`JournalReader`], for replay/search over closed + active files.
#[derive(Debug)]
pub struct JournalSet {
    base: PathBuf,
    segments: Vec<SegmentInfo>,
}

impl JournalSet {
    /// List and fully scan every segment of `base`. The active file is
    /// scanned last; a missing active file (rotation in flight) is an error
    /// like a missing journal.
    pub fn open(base: impl AsRef<Path>) -> Result<Self, JournalFlowError> {
        let base = base.as_ref().to_path_buf();
        let closed = list_closed_segments(&base).map_err(|source| JournalFlowError::Io {
            path: base.clone(),
            source,
        })?;
        let active_index = closed.last().map(|(k, _)| k + 1).unwrap_or(0);
        let mut segments = Vec::with_capacity(closed.len() + 1);
        for (index, path) in closed {
            let reader = JournalReader::open(&path)?;
            segments.push(SegmentInfo {
                index,
                path,
                first_seq: reader.first_seq(),
                last_seq: reader.last_seq(),
                status: reader.status(),
                bytes: reader.journal_bytes(),
                active: false,
            });
        }
        let reader = JournalReader::open(&base)?;
        segments.push(SegmentInfo {
            index: active_index,
            path: base.clone(),
            first_seq: reader.first_seq(),
            last_seq: reader.last_seq(),
            status: reader.status(),
            bytes: reader.journal_bytes(),
            active: true,
        });
        Ok(Self { base, segments })
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    /// All segments on disk, ascending; the last one is the active file.
    pub fn segments(&self) -> &[SegmentInfo] {
        &self.segments
    }

    /// Segments that replay as one contiguous verified stream ending at the
    /// active file: the longest suffix whose closed members are `Ok` and
    /// whose seqs chain without a gap. Damage in older history only shortens
    /// the replayable head — it never hides the live tail.
    pub fn usable(&self) -> &[SegmentInfo] {
        let mut start = self.segments.len() - 1;
        while start > 0 {
            let prev = &self.segments[start - 1];
            let next = &self.segments[start];
            let contiguous = prev.status == ScanStatus::Ok
                && prev.last_seq > 0
                && next.first_seq == prev.last_seq + 1;
            if !contiguous {
                break;
            }
            start -= 1;
        }
        &self.segments[start..]
    }

    /// First seq that replays (1 until the head has been trimmed or
    /// damaged).
    pub fn first_seq(&self) -> u64 {
        self.usable()[0].first_seq.max(1)
    }

    /// Last verified seq of the active segment.
    pub fn last_seq(&self) -> u64 {
        self.segments[self.segments.len() - 1].last_seq
    }

    /// Scan status of the active segment (closed damage shows up as a
    /// shorter [`JournalSet::usable`] run instead).
    pub fn status(&self) -> ScanStatus {
        self.segments[self.segments.len() - 1].status
    }

    pub fn retained_bytes(&self) -> u64 {
        self.segments.iter().map(|s| s.bytes).sum()
    }

    /// Head/tail summary as a reader sees it (`dropped_bytes` is unknown
    /// from disk alone and reported as 0).
    pub fn head(&self) -> SegmentSnapshot {
        let usable = self.usable();
        SegmentSnapshot {
            active_index: self.segments[self.segments.len() - 1].index,
            first_index: usable[0].index,
            first_seq: self.first_seq(),
            dropped_bytes: 0,
            retained_bytes: self.retained_bytes(),
        }
    }

    /// Replay `from_seq..=to_seq` across the usable segments in order.
    /// Requests reaching below the usable head fail with `HeadTrimmed`.
    pub fn replay(
        &self,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<Vec<JournalRecord>, JournalFlowError> {
        let head = self.first_seq();
        let from = from_seq.max(1);
        if from < head && to_seq >= from {
            return Err(JournalFlowError::HeadTrimmed { first_seq: head });
        }
        let mut out = Vec::new();
        for segment in self.usable() {
            if segment.last_seq < from || segment.first_seq > to_seq {
                continue;
            }
            let reader = JournalReader::open(&segment.path)?;
            out.extend(reader.replay(from.max(segment.first_seq), to_seq.min(segment.last_seq))?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_paths_and_listing() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("abc.mtj");
        assert_eq!(segment_path(&base, 7), dir.path().join("abc.mtj.7"));
        std::fs::write(&base, b"").unwrap();
        std::fs::write(segment_path(&base, 3), b"").unwrap();
        std::fs::write(segment_path(&base, 12), b"").unwrap();
        std::fs::write(dir.path().join("abc.mtj.bak"), b"").unwrap();
        std::fs::write(dir.path().join("abcd.mtj.1"), b"").unwrap();
        let listed: Vec<u64> = list_closed_segments(&base)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(listed, vec![3, 12]);
        assert_eq!(resolve_segment_path(&base, 3), segment_path(&base, 3));
        assert_eq!(resolve_segment_path(&base, 13), base);
        assert_eq!(journal_files(&base).len(), 3);
        assert!(list_closed_segments(&dir.path().join("missing/x.mtj"))
            .unwrap()
            .is_empty());
    }

    /// Startup budget seeding walks the flat journals directory: `<id>.mtj`
    /// bases and digit-suffixed `<id>.mtj.<k>` segments of every session
    /// count, anything else does not, and a missing directory is 0.
    #[test]
    fn journal_dir_bytes_sums_only_journal_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("abc.mtj");
        std::fs::write(&base, [0u8; 20]).unwrap();
        std::fs::write(segment_path(&base, 3), [0u8; 7]).unwrap();
        std::fs::write(segment_path(&base, 12), [0u8; 9]).unwrap();
        // A segment of a different session — counted, unlike in the
        // per-base listing above.
        std::fs::write(dir.path().join("abcd.mtj.1"), [0u8; 100]).unwrap();
        std::fs::write(dir.path().join("abc.mtj.bak"), [0u8; 100]).unwrap();
        std::fs::write(dir.path().join("notes.txt"), [0u8; 100]).unwrap();
        assert_eq!(journal_dir_bytes(dir.path()), 20 + 7 + 9 + 100);
        assert_eq!(journal_dir_bytes(&dir.path().join("missing")), 0);
    }

    #[test]
    fn tracker_round_trips_a_snapshot() {
        let tracker = SegmentTracker::new();
        assert_eq!(tracker.snapshot(), SegmentSnapshot::default());
        let snap = SegmentSnapshot {
            active_index: 5,
            first_index: 2,
            first_seq: 900,
            dropped_bytes: 4096,
            retained_bytes: 8192,
        };
        tracker.publish(snap);
        assert_eq!(tracker.snapshot(), snap);
        assert_eq!(SegmentTracker::frozen(snap).snapshot(), snap);
    }
}
