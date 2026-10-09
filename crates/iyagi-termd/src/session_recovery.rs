//! Reopen retained output on demand after a daemon restart or registry eviction.
//! No actor or process is created. Recovered entries use the same bounded ring.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Condvar, Mutex};

use term_contracts::{error::ErrorCode, ids::SessionId, RpcError};
use term_pty::flow::FlowController;
use term_pty::journal::{JournalReader, ScanStatus};
use term_pty::segments::{list_closed_segments, SegmentSnapshot, SegmentTracker};

use crate::{sessions::SessionEntry, state::DaemonState};

pub(crate) fn load(
    state: &Arc<DaemonState>,
    id: &SessionId,
) -> Result<Arc<SessionEntry>, RpcError> {
    if let Some(session) = state.session(id) {
        return Ok(session);
    }
    let unavailable =
        || RpcError::new(ErrorCode::InvalidArgument, "session journal is unavailable");
    let storage_error = |_| RpcError::new(ErrorCode::DaemonUnavailable, "session lookup failed");
    let record = state
        .storage
        .session(id)
        .map_err(storage_error)?
        .ok_or_else(unavailable)?;
    let workload = state
        .storage
        .workload_record(&record.workload_id)
        .map_err(storage_error)?
        .ok_or_else(unavailable)?;
    if !workload.state.is_terminal() || record.replay_status == "deleted" {
        return Err(unavailable());
    }
    let base = state.paths.journal(id.as_str());
    let mut files = list_closed_segments(&base).map_err(|_| unavailable())?;
    let active_index = files.last().map_or(0, |(index, _)| index + 1);
    if base.is_file() {
        files.push((active_index, base.clone()));
    }
    let first_index = files.first().ok_or_else(unavailable)?.0;
    let expected = uuid::Uuid::parse_str(id.as_str()).map_err(|_| unavailable())?;
    let mut first_seq = 0;
    let mut last_seq = 0;
    let mut retained_bytes = 0;
    for (position, (index, path)) in files.iter().enumerate() {
        if *index != first_index + position as u64 {
            return Err(unavailable());
        }
        let reader = JournalReader::open_with_session(path, expected).map_err(|_| unavailable())?;
        match reader.status() {
            ScanStatus::Corrupt { .. } => return Err(unavailable()),
            ScanStatus::TailTruncated if position + 1 != files.len() => return Err(unavailable()),
            _ => {}
        }
        if reader.record_count() > 0 {
            if first_seq == 0 {
                first_seq = reader.first_seq();
            } else if reader.first_seq() != last_seq + 1 {
                return Err(unavailable());
            }
            last_seq = reader.last_seq();
        }
        retained_bytes += reader.journal_bytes();
    }
    let session = Arc::new(SessionEntry {
        session_id: id.clone(),
        workload_id: record.workload_id,
        journal_path: base,
        journal_limit: AtomicU64::new(record.journal_limit_bytes),
        epoch: Mutex::new(uuid::Uuid::new_v4().to_string()),
        owner_view: Mutex::new(None),
        views: Mutex::new(HashMap::new()),
        last_seq: AtomicU64::new(last_seq),
        journal_inner: Mutex::new(None),
        journal_offsets: Mutex::new(BTreeMap::new()),
        journal_segments: Arc::new(SegmentTracker::frozen(SegmentSnapshot {
            active_index: files.last().expect("nonempty").0,
            first_index,
            first_seq: first_seq.max(1),
            retained_bytes,
            // Dropped bytes are not persisted; first_seq still identifies a trimmed head.
            dropped_bytes: 0,
        })),
        journal_read_failing_since: Mutex::new(None),
        recent_resizes: Mutex::new(Vec::new()),
        resize_notify: tokio::sync::Notify::new(),
        resize_in_flight: AtomicBool::new(false),
        flow: Mutex::new(FlowController::with_budget(Arc::clone(&state.flow_budget))),
        wake_tx: Mutex::new(()),
        wake_cv: Condvar::new(),
        wake_pending: AtomicBool::new(false),
        pump_stop: AtomicBool::new(false),
        pump_alive: AtomicBool::new(false),
        pump_ctl: Mutex::new(()),
        size: Mutex::new((record.initial_cols, record.initial_rows)),
        actor_finalized: AtomicBool::new(true),
    });
    let session = state
        .sessions
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(id.clone())
        .or_insert(session)
        .clone();
    state.note_session_finalized(id);
    Ok(session)
}
