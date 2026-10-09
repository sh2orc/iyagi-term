//! The [`Storage`] handle: open/migrate/reconcile, one serialized writer
//! thread, and a small read pool. The API is synchronous by design — callers
//! wrap it in `spawn_blocking` on the async side.

use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::sync::{Condvar, Mutex};
use std::thread::JoinHandle;

use rusqlite::{Connection, OpenFlags};
use term_contracts::agent_session::AgentSessionRecord;
use term_contracts::ids::{RequestId, SessionId, WorkloadId};
use term_contracts::mission::types::{Id as MissionId, Mission};
use term_contracts::snapshot::QueueReason;
use term_contracts::state::WorkloadState;
use term_contracts::workload::{ProcessOwnership, WorkloadRecord};

use crate::error::{StorageError, StorageResult};
use crate::migration;
use crate::mission::types::{
    AppliedTransition, ApplyMissionTransition, MissionListCursor, MissionSnapshotData,
    MissionStoreError, MissionStoreResult, OutboxState, StoredEvent, StoredOutbox, StoredRequest,
};
use crate::ops;
use crate::queries;
use crate::types::{
    AgentSessionUpsert, LaunchIntent, LaunchIntentOutcome, QueuedWorkload, ReconciledWorkload,
    RequestResolution, SessionRecord,
};
use crate::writer::{self, Flag, ReplySender, WriterCommand};

/// Read pool depth: enough for concurrent snapshot reads without contention.
const READ_POOL_SIZE: usize = 4;

pub struct Storage {
    commands: Sender<WriterCommand>,
    writer_thread: Option<JoinHandle<()>>,
    reads: ReadPool,
    reconciliation: Mutex<Option<Vec<ReconciledWorkload>>>,
}

impl Storage {
    /// Open (creating if needed), migrate to the latest schema version, and
    /// run crash reconciliation: every workload left in a non-terminal state
    /// by a previous process is marked INTERRUPTED with a `DAEMON_RESTART`
    /// lifecycle event, and the report is retrievable once via
    /// [`Storage::take_reconciliation_report`] (spec §6).
    pub fn open(path: impl AsRef<Path>) -> StorageResult<Storage> {
        let mut conn = Connection::open(path.as_ref())?;
        configure_write_connection(&conn)?;

        migration::migrate(&mut conn)?;
        let report = ops::reconcile_crashed_workloads(&mut conn)?;

        let (tx, rx) = mpsc::channel::<WriterCommand>();
        let writer_thread = writer::spawn(conn, rx);
        let reads = ReadPool::open(path.as_ref(), READ_POOL_SIZE)?;

        Ok(Storage {
            commands: tx,
            writer_thread: Some(writer_thread),
            reads,
            reconciliation: Mutex::new(Some(report)),
        })
    }

    /// Durably record a launch intent (single transaction, duplicate-safe).
    /// Re-playing the same request id + fingerprint returns the existing
    /// workload's current state; a different fingerprint under the same id is
    /// `REQUEST_CONFLICT` (spec §6).
    pub fn record_launch_intent(&self, intent: LaunchIntent) -> StorageResult<LaunchIntentOutcome> {
        self.exec(|reply| WriterCommand::RecordLaunchIntent {
            intent: Box::new(intent),
            reply,
        })
    }

    /// Read-only request id lookup for retry paths: was this request id seen,
    /// with which fingerprint equivalence? (spec §7 `DAEMON_UNAVAILABLE`).
    pub fn resolve_request(
        &self,
        request_id: &RequestId,
        fingerprint: &str,
    ) -> StorageResult<RequestResolution> {
        self.read(|conn| queries::resolve_request(conn, request_id, fingerprint))
    }

    /// QUEUED -> STARTING. Stamps `started_at`.
    pub fn mark_starting(&self, workload_id: &WorkloadId) -> StorageResult<()> {
        self.transition(workload_id.clone(), WorkloadState::Starting)
    }

    /// STARTING -> RUNNING.
    pub fn mark_running(&self, workload_id: &WorkloadId) -> StorageResult<()> {
        self.transition(workload_id.clone(), WorkloadState::Running)
    }

    /// Any legal non-terminal transition (e.g. RUNNING -> STOPPING,
    /// STOPPING/RUNNING -> DRAINING), validated by `WorkloadState::can_transition`.
    /// Terminal targets belong to [`Storage::mark_terminal`].
    pub fn transition_to(&self, workload_id: &WorkloadId, to: WorkloadState) -> StorageResult<()> {
        if to.is_terminal() {
            return Err(StorageError::InvalidArgument(
                "terminal targets must go through mark_terminal",
            ));
        }
        self.transition(workload_id.clone(), to)
    }

    /// Terminal transition into SUCCEEDED/FAILED/CANCELLED/INTERRUPTED with
    /// exit code and reason; also upgrades the request outcome
    /// (SUCCEEDED/CANCELLED -> completed, FAILED -> failed, INTERRUPTED ->
    /// unknown).
    pub fn mark_terminal(
        &self,
        workload_id: &WorkloadId,
        to: WorkloadState,
        exit_code: Option<i32>,
        reason_code: Option<String>,
    ) -> StorageResult<()> {
        if !to.is_terminal() {
            return Err(StorageError::InvalidArgument(
                "mark_terminal requires a terminal state",
            ));
        }
        self.exec(|reply| WriterCommand::Transition {
            workload_id: workload_id.clone(),
            to,
            exit_code,
            reason_code,
            reply,
        })
    }

    /// Persist the process ownership row (pid + start_token + boot_id and the
    /// group reference) for a workload (spec §1/§6).
    pub fn save_group_identity(&self, ownership: ProcessOwnership) -> StorageResult<()> {
        self.exec(|reply| WriterCommand::SaveGroupIdentity { ownership, reply })
    }

    /// Overwrite (or clear) the scheduler's wait reason for a workload.
    pub fn set_queue_reason(
        &self,
        workload_id: &WorkloadId,
        reason: Option<QueueReason>,
    ) -> StorageResult<()> {
        self.exec(|reply| WriterCommand::SetQueueReason {
            workload_id: workload_id.clone(),
            reason,
            reply,
        })
    }

    pub fn set_cancel_requested(&self, workload_id: &WorkloadId, value: bool) -> StorageResult<()> {
        self.exec(|reply| WriterCommand::SetFlag {
            workload_id: workload_id.clone(),
            flag: Flag::CancelRequested,
            value,
            reply,
        })
    }

    pub fn set_root_exited(&self, workload_id: &WorkloadId, value: bool) -> StorageResult<()> {
        self.exec(|reply| WriterCommand::SetFlag {
            workload_id: workload_id.clone(),
            flag: Flag::RootExited,
            value,
            reply,
        })
    }

    pub fn workload_record(
        &self,
        workload_id: &WorkloadId,
    ) -> StorageResult<Option<WorkloadRecord>> {
        self.read(|conn| queries::workload_record(conn, workload_id))
    }

    /// QUEUED entries ordered priority -> created_at -> id.
    pub fn queue_snapshot(&self) -> StorageResult<Vec<QueuedWorkload>> {
        self.read(queries::queue_snapshot)
    }

    /// Every workload in a non-terminal state.
    pub fn active_workloads(&self) -> StorageResult<Vec<WorkloadRecord>> {
        self.read(queries::active_workloads)
    }

    pub fn sessions(&self) -> StorageResult<Vec<SessionRecord>> {
        self.read(queries::sessions)
    }

    pub fn session(&self, id: &SessionId) -> StorageResult<Option<SessionRecord>> {
        self.read(|conn| queries::session(conn, id))
    }

    /// Session journal progress with checked arithmetic: values beyond the
    /// SQLite signed-INTEGER bound and backwards sequences are explicit
    /// errors; the stored row never wraps (spec §1).
    pub fn update_session_progress(
        &self,
        session_id: &SessionId,
        last_seq: u64,
        journal_bytes: u64,
    ) -> StorageResult<()> {
        self.exec(|reply| WriterCommand::UpdateSessionProgress {
            session_id: session_id.clone(),
            last_seq,
            journal_bytes,
            reply,
        })
    }

    /// Retention candidates (W1-2): journals of terminal, unpinned,
    /// not-yet-deleted sessions. The daemon skips any session it still holds.
    pub fn terminal_unpinned_sessions(&self) -> StorageResult<Vec<SessionRecord>> {
        self.read(queries::terminal_unpinned_sessions)
    }

    /// lifecycle_events를 최근 `keep_recent`개로 정리한다(W2).
    pub fn prune_lifecycle_events(&self, keep_recent: i64) -> StorageResult<()> {
        self.exec(|reply| WriterCommand::PruneLifecycleEvents { keep_recent, reply })
    }

    /// Mark a session's journal deleted by retention (W1-2). Pinned rows are
    /// a silent no-op — retention never overrides a user pin.
    pub fn mark_journal_deleted(&self, session_id: &SessionId) -> StorageResult<()> {
        self.exec(|reply| WriterCommand::MarkJournalDeleted {
            session_id: session_id.clone(),
            reply,
        })
    }

    // -- agent sessions (spec `02-runner.md` §8) ----------------------------

    /// 관찰한 에이전트 세션을 기록/갱신하고 그 행을 돌려준다. 재관찰은
    /// `last_seen_at` 갱신이며 종료 표시를 지운다. 돌려주는 레코드의
    /// `active`는 항상 false다 — 살아 있는 워크로드 대조는 데몬의 몫이다.
    pub fn upsert_agent_session(
        &self,
        upsert: AgentSessionUpsert,
    ) -> StorageResult<AgentSessionRecord> {
        self.exec(|reply| WriterCommand::UpsertAgentSession {
            upsert: Box::new(upsert),
            reply,
        })
    }

    /// 이 워크로드에서 아직 열려 있는 에이전트 세션을 모두 닫는다(pane
    /// 종료). 닫은 행 수를 돌려준다.
    pub fn end_agent_sessions_for_workload(
        &self,
        workload_id: &WorkloadId,
        reason: &str,
    ) -> StorageResult<usize> {
        self.exec(|reply| WriterCommand::EndAgentSessionsForWorkload {
            workload_id: workload_id.clone(),
            reason: reason.to_string(),
            reply,
        })
    }

    /// 한 건만 닫는다(세션 교체 `replaced`, hook `hook_end`). 이미 닫혀
    /// 있거나 없는 행이면 false.
    pub fn end_agent_session(
        &self,
        workload_id: &WorkloadId,
        agent: &str,
        agent_session_id: &str,
        reason: &str,
    ) -> StorageResult<bool> {
        self.exec(|reply| WriterCommand::EndAgentSession {
            workload_id: workload_id.clone(),
            agent: agent.to_string(),
            agent_session_id: agent_session_id.to_string(),
            reason: reason.to_string(),
            reply,
        })
    }

    /// 복구 후보 목록: (agent, agent_session_id)별 최신 한 건씩,
    /// `last_seen_at` 내림차순 `limit`개. `cwd`를 주면 그 폴더만.
    pub fn list_agent_sessions(
        &self,
        limit: u32,
        cwd: Option<&str>,
    ) -> StorageResult<Vec<AgentSessionRecord>> {
        self.list_agent_sessions_filtered(limit, cwd, None, None)
    }

    /// Apply recovery identity filters before deduplication and the response limit.
    pub fn list_agent_sessions_filtered(
        &self,
        limit: u32,
        cwd: Option<&str>,
        workload_id: Option<&WorkloadId>,
        pty_session_id: Option<&SessionId>,
    ) -> StorageResult<Vec<AgentSessionRecord>> {
        self.read(|conn| {
            queries::list_agent_sessions(conn, limit, cwd, workload_id, pty_session_id)
        })
    }

    /// 목록에서 한 건을 영구히 지운다(사용자 동작).
    pub fn forget_agent_session(&self, id: &str) -> StorageResult<bool> {
        self.exec(|reply| WriterCommand::ForgetAgentSession {
            id: id.to_string(),
            reply,
        })
    }

    /// 보존 정리: 종료됐고 오래된 행 + 최신 `keep_at_most`개 밖의 행.
    pub fn prune_agent_sessions(
        &self,
        max_age_days: u32,
        keep_at_most: u32,
    ) -> StorageResult<usize> {
        self.exec(|reply| WriterCommand::PruneAgentSessions {
            max_age_days,
            keep_at_most,
            reply,
        })
    }

    /// The reconciliation report from this open (first caller wins; None on
    /// later calls or when the previous process shut down cleanly).
    pub fn take_reconciliation_report(&self) -> Option<Vec<ReconciledWorkload>> {
        self.reconciliation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    /// Current journal mode as reported by a read connection (diagnostics;
    /// tests assert "wal").
    pub fn journal_mode(&self) -> StorageResult<String> {
        self.read(queries::journal_mode)
    }

    // ---- O1 mission API (ticket O03) ------------------------------------

    /// One atomic mission transaction (projections + event + outbox +
    /// request row + revision). Duplicate request ids replay the stored
    /// first response; a different payload is `RequestConflict`.
    pub fn apply_mission_transition(
        &self,
        transition: ApplyMissionTransition,
    ) -> MissionStoreResult<AppliedTransition> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(WriterCommand::MissionApply {
                transition: Box::new(transition),
                reply: reply_tx,
            })
            .map_err(|_| MissionStoreError::WriterClosed)?;
        reply_rx
            .recv()
            .map_err(|_| MissionStoreError::WriterClosed)?
    }

    /// Retention pass: prune housekeeping events outside each mission's
    /// recent tail and dedupe requests past their window (bounded database).
    pub fn prune_mission_retention(
        &self,
        event_tail_per_mission: i64,
        request_retention_days: u32,
    ) -> MissionStoreResult<crate::mission::ops::PrunedRows> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(WriterCommand::MissionPruneRetention {
                event_tail_per_mission,
                request_retention_days,
                reply: reply_tx,
            })
            .map_err(|_| MissionStoreError::WriterClosed)?;
        reply_rx
            .recv()
            .map_err(|_| MissionStoreError::WriterClosed)?
    }

    /// Rebuild the database file, returning freed pages to the OS. Runs on
    /// the writer connection (autocommit); a busy reader makes it fail rather
    /// than wait — callers treat failure as "try again next sweep".
    pub fn vacuum(&self) -> StorageResult<()> {
        self.exec(|reply| WriterCommand::Vacuum { reply })
    }

    /// Materialize one mission's current entities (no artifact bodies).
    pub fn mission_snapshot(
        &self,
        mission_id: &MissionId,
    ) -> MissionStoreResult<Option<MissionSnapshotData>> {
        self.read_mission(|conn| crate::mission::queries::materialize(conn, mission_id))
    }

    /// Committed events strictly after `after_seq`, plus the high watermark.
    pub fn mission_events(
        &self,
        mission_id: &MissionId,
        after_seq: u64,
        limit: u32,
    ) -> MissionStoreResult<(Vec<StoredEvent>, u64)> {
        self.read_mission(|conn| {
            crate::mission::queries::events_after(conn, mission_id, after_seq, limit)
        })
    }

    /// Stored request dedupe lookup.
    pub fn mission_request(
        &self,
        request_id: &MissionId,
    ) -> MissionStoreResult<Option<StoredRequest>> {
        self.read_mission(|conn| crate::mission::queries::get_request(conn, request_id))
    }

    /// mission.list keyset page (updated_at DESC, id DESC).
    pub fn mission_list(
        &self,
        cursor: Option<MissionListCursor>,
        limit: u32,
        archived: bool,
    ) -> MissionStoreResult<(Vec<Mission>, Option<MissionListCursor>)> {
        self.read_mission(|conn| {
            crate::mission::queries::list_missions(conn, cursor.as_ref(), limit, archived)
        })
    }

    /// CAS save for `orch_bindings` (request-deduped, mission-less scope).
    pub fn save_mission_binding(
        &self,
        request_id: MissionId,
        method: &str,
        fingerprint: &str,
        expected_revision: u64,
        document: serde_json::Value,
        created_at: String,
    ) -> MissionStoreResult<crate::mission::ops::SavedConfig> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(WriterCommand::MissionSaveBinding {
                request_id,
                method: method.to_string(),
                fingerprint: fingerprint.to_string(),
                expected_revision,
                document,
                created_at,
                reply: reply_tx,
            })
            .map_err(|_| MissionStoreError::WriterClosed)?;
        reply_rx
            .recv()
            .map_err(|_| MissionStoreError::WriterClosed)?
    }

    /// CAS save for `orch_config` rows (template | verification | repository).
    /// Thin parameter forwarding over the writer channel; the storage-side
    /// `SaveConfig` bundle keeps the transaction builder under the arity
    /// lint, this facade mirrors the writer command shape 1:1.
    #[allow(clippy::too_many_arguments)]
    pub fn save_mission_config(
        &self,
        request_id: MissionId,
        method: &str,
        fingerprint: &str,
        kind: &str,
        expected_revision: u64,
        document: serde_json::Value,
        created_at: String,
    ) -> MissionStoreResult<crate::mission::ops::SavedConfig> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(WriterCommand::MissionSaveConfig {
                request_id,
                method: method.to_string(),
                fingerprint: fingerprint.to_string(),
                kind: kind.to_string(),
                expected_revision,
                document,
                created_at,
                reply: reply_tx,
            })
            .map_err(|_| MissionStoreError::WriterClosed)?;
        reply_rx
            .recv()
            .map_err(|_| MissionStoreError::WriterClosed)?
    }

    /// All stored bindings (read pool).
    pub fn mission_bindings(&self) -> MissionStoreResult<Vec<serde_json::Value>> {
        self.read_mission(crate::mission::queries::list_bindings)
    }

    pub fn mission_rate_limit_runs(
        &self,
        now_ms: u64,
    ) -> MissionStoreResult<Vec<term_contracts::mission::types::Run>> {
        self.read_mission(|conn| crate::mission::queries::rate_limit_runs(conn, now_ms))
    }

    pub fn mission_exec_recovery_records(
        &self,
        current_owner: &term_contracts::mission::types::Id,
        previous: &[term_contracts::mission::types::Id],
    ) -> MissionStoreResult<Vec<term_contracts::mission::types::ExecRecord>> {
        self.read_mission(|conn| {
            crate::mission::queries::exec_recovery_records(conn, current_owner, previous)
        })
    }

    /// All stored config documents of one kind (read pool).
    pub fn mission_configs(&self, kind: &str) -> MissionStoreResult<Vec<serde_json::Value>> {
        self.read_mission(|conn| crate::mission::queries::list_configs(conn, kind))
    }

    /// Record a new artifact upload row.
    pub fn mission_upload_insert(
        &self,
        upload: crate::mission::artifacts::UploadRow,
    ) -> MissionStoreResult<()> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(WriterCommand::MissionArtifactUpload {
                op: crate::mission::artifacts::UploadOp::Insert(upload),
                reply: reply_tx,
            })
            .map_err(|_| MissionStoreError::WriterClosed)?;
        reply_rx
            .recv()
            .map_err(|_| MissionStoreError::WriterClosed)?
    }

    /// Advance an upload's append cursor.
    pub fn mission_upload_advance(
        &self,
        id: MissionId,
        from_offset: i64,
        to_offset: i64,
    ) -> MissionStoreResult<()> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(WriterCommand::MissionArtifactUpload {
                op: crate::mission::artifacts::UploadOp::Advance {
                    id,
                    from_offset,
                    to_offset,
                },
                reply: reply_tx,
            })
            .map_err(|_| MissionStoreError::WriterClosed)?;
        reply_rx
            .recv()
            .map_err(|_| MissionStoreError::WriterClosed)?
    }

    /// Atomically register a committed artifact and link its upload.
    pub fn mission_artifact_commit(
        &self,
        upload_id: MissionId,
        artifact: crate::mission::artifacts::ArtifactRow,
    ) -> MissionStoreResult<crate::mission::artifacts::ArtifactRow> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(WriterCommand::MissionArtifactCommit {
                upload_id,
                artifact,
                reply: reply_tx,
            })
            .map_err(|_| MissionStoreError::WriterClosed)?;
        reply_rx
            .recv()
            .map_err(|_| MissionStoreError::WriterClosed)?
    }

    /// Drop an upload row (post-sweep).
    pub fn mission_upload_drop(&self, id: MissionId) -> MissionStoreResult<()> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(WriterCommand::MissionArtifactUpload {
                op: crate::mission::artifacts::UploadOp::Drop(id),
                reply: reply_tx,
            })
            .map_err(|_| MissionStoreError::WriterClosed)?;
        reply_rx
            .recv()
            .map_err(|_| MissionStoreError::WriterClosed)?
    }

    /// Upload row lookup.
    pub fn mission_upload(
        &self,
        id: &MissionId,
    ) -> MissionStoreResult<Option<crate::mission::artifacts::UploadRow>> {
        self.read_mission(|conn| crate::mission::artifacts::get_upload(conn, id))
    }

    /// Artifact row lookup.
    pub fn mission_artifact(
        &self,
        id: &MissionId,
    ) -> MissionStoreResult<Option<crate::mission::artifacts::ArtifactRow>> {
        self.read_mission(|conn| crate::mission::artifacts::get_artifact(conn, id))
    }

    /// Expired uncommitted uploads (sweep input).
    pub fn mission_expired_uploads(
        &self,
        now_iso: &str,
    ) -> MissionStoreResult<Vec<(MissionId, String)>> {
        self.read_mission(|conn| crate::mission::artifacts::expired_uploads(conn, now_iso))
    }

    /// Non-terminal outbox rows (recovery scan).
    pub fn mission_outbox(&self) -> MissionStoreResult<Vec<StoredOutbox>> {
        self.read_mission(crate::mission::queries::pending_outbox)
    }

    /// Advance an outbox row's state (writer thread).
    pub fn set_mission_outbox_state(
        &self,
        id: MissionId,
        state: OutboxState,
        updated_at: String,
    ) -> MissionStoreResult<()> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(WriterCommand::MissionOutboxState {
                id,
                state,
                updated_at,
                reply: reply_tx,
            })
            .map_err(|_| MissionStoreError::WriterClosed)?;
        reply_rx
            .recv()
            .map_err(|_| MissionStoreError::WriterClosed)?
    }

    /// Keep all statements in one mission read on the same committed WAL
    /// snapshot. A leased connection alone does not pin a read transaction.
    pub fn read_mission<T>(
        &self,
        run: impl FnOnce(&Connection) -> MissionStoreResult<T>,
    ) -> MissionStoreResult<T> {
        self.reads.with_read_raw(|conn| {
            let transaction = conn.unchecked_transaction()?;
            let result = run(&transaction)?;
            transaction.commit()?;
            Ok(result)
        })
    }

    fn transition(&self, workload_id: WorkloadId, to: WorkloadState) -> StorageResult<()> {
        self.exec(|reply| WriterCommand::Transition {
            workload_id,
            to,
            exit_code: None,
            reason_code: None,
            reply,
        })
    }

    fn exec<T>(
        &self,
        build_command: impl FnOnce(ReplySender<T>) -> WriterCommand,
    ) -> StorageResult<T> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(build_command(reply_tx))
            .map_err(|_| StorageError::WriterClosed)?;
        reply_rx.recv().map_err(|_| StorageError::WriterClosed)?
    }

    fn read<T>(&self, run: impl FnOnce(&Connection) -> StorageResult<T>) -> StorageResult<T> {
        self.reads.with_read(run)
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        if let Some(handle) = self.writer_thread.take() {
            let (tx, rx) = mpsc::channel();
            if self
                .commands
                .send(WriterCommand::Shutdown { reply: tx })
                .is_ok()
            {
                let _ = rx.recv();
            }
            let _ = handle.join();
        }
    }
}

/// Connection policy per spec §7: every connection gets foreign_keys=ON and
/// busy_timeout=5000; the database runs in WAL; the dedicated writer
/// additionally runs synchronous=FULL — a conservative standing choice: the
/// pre-gate-release launch-intent writes must survive power loss, and WAL +
/// FULL costs one fsync per write transaction, which the serialized writer
/// already bounds.
fn configure_write_connection(conn: &Connection) -> StorageResult<()> {
    conn.execute_batch("PRAGMA busy_timeout = 5000;")?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StorageError::corrupt(format!(
            "journal_mode is {mode:?}, expected WAL"
        )));
    }
    conn.execute_batch("PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON;")?;
    Ok(())
}

/// Pool of read-only connections. WAL is a persistent database property set by
/// the writer at open; readers verify (a read PRAGMA) instead of trying to
/// write it on a read-only handle.
struct ReadPool {
    connections: Mutex<Vec<Connection>>,
    available: Condvar,
}

impl ReadPool {
    fn open(path: &Path, size: usize) -> StorageResult<ReadPool> {
        let mut connections = Vec::with_capacity(size);
        for _ in 0..size {
            let conn = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.execute_batch("PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON;")?;
            let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
            if !mode.eq_ignore_ascii_case("wal") {
                return Err(StorageError::corrupt(format!(
                    "journal_mode is {mode:?}, expected WAL"
                )));
            }
            connections.push(conn);
        }
        Ok(ReadPool {
            connections: Mutex::new(connections),
            available: Condvar::new(),
        })
    }

    fn with_read<T>(&self, run: impl FnOnce(&Connection) -> StorageResult<T>) -> StorageResult<T> {
        self.with_read_raw(run)
    }

    /// Mission reads keep their own error type; the pool itself never fails
    /// except by panic, which propagates. The lease returns the connection
    /// on every exit path, including a panic inside `run` (a lost connection
    /// would shrink the pool until readers block forever on the condvar).
    fn with_read_raw<T, E>(&self, run: impl FnOnce(&Connection) -> Result<T, E>) -> Result<T, E> {
        let mut guard = self
            .connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while guard.is_empty() {
            guard = self
                .available
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        let conn = guard.pop().expect("pool signaled a free connection");
        drop(guard);
        let lease = ReadLease {
            pool: self,
            conn: Some(conn),
        };
        run(lease.conn.as_ref().expect("lease holds its connection"))
    }
}

/// Checked-out read connection; hands itself back to the pool on drop.
struct ReadLease<'a> {
    pool: &'a ReadPool,
    conn: Option<Connection>,
}

impl Drop for ReadLease<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool
                .connections
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(conn);
            self.pool.available.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic inside a read closure must hand the connection back: after
    /// `READ_POOL_SIZE + 1` lost connections every later reader would block
    /// forever on the condvar.
    #[test]
    fn read_connection_returns_to_pool_after_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::open(dir.path().join("panic.sqlite")).expect("open");
        for _ in 0..(READ_POOL_SIZE + 1) {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                storage.read(|_| -> StorageResult<()> { panic!("reader panic") })
            }));
            assert!(outcome.is_err(), "the closure panics");
        }
        let mode = storage.journal_mode().expect("pool still serves reads");
        assert!(mode.eq_ignore_ascii_case("wal"));
    }
}
