//! Read-only queries, executed on the read pool against the latest committed
//! WAL snapshot.

use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, Row};
use term_contracts::agent_session::{AgentSessionRecord, AgentSessionSource};
use term_contracts::ids::{RequestId, SessionId, U64String, WorkloadId};
use term_contracts::launch::Priority;
use term_contracts::state::WorkloadState;
use term_contracts::workload::WorkloadRecord;

use crate::enums::*;
use crate::error::{StorageError, StorageResult};
use crate::types::{QueuedWorkload, RequestOutcome, RequestResolution, SessionRecord};

/// Parse helper shared with write ops: outcome + workload id strings from the
/// requests ledger plus the workload's current state.
pub(crate) fn request_state_parts(
    conn: &Connection,
    outcome_str: &str,
    workload_id_str: &str,
) -> StorageResult<(RequestOutcome, WorkloadId, WorkloadState)> {
    let outcome = request_outcome_from_str(outcome_str)
        .ok_or_else(|| StorageError::corrupt(format!("bad request outcome {outcome_str:?}")))?;
    let workload_id = WorkloadId::parse(workload_id_str)
        .map_err(|_| StorageError::corrupt(format!("bad workload id {workload_id_str:?}")))?;
    let state_str: String = conn
        .query_row(
            "SELECT state FROM workloads WHERE id = ?1",
            [workload_id_str],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::corrupt(format!(
                "request points at missing workload {workload_id_str}"
            ))
        })?;
    let state = workload_state_from_str(&state_str)
        .ok_or_else(|| StorageError::corrupt(format!("bad state {state_str:?}")))?;
    Ok((outcome, workload_id, state))
}

pub(crate) fn resolve_request(
    conn: &Connection,
    request_id: &RequestId,
    fingerprint: &str,
) -> StorageResult<RequestResolution> {
    let row = conn
        .query_row(
            "SELECT fingerprint, outcome, workload_id FROM requests WHERE id = ?1",
            [request_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    match row {
        None => Ok(RequestResolution::New),
        Some((stored_fingerprint, outcome_str, workload_id_str)) => {
            if stored_fingerprint != fingerprint {
                return Ok(RequestResolution::Conflict);
            }
            let (outcome, workload_id, state) =
                request_state_parts(conn, &outcome_str, &workload_id_str)?;
            Ok(RequestResolution::Existing {
                outcome,
                workload_id,
                state,
            })
        }
    }
}

pub(crate) fn workload_record(
    conn: &Connection,
    workload_id: &WorkloadId,
) -> StorageResult<Option<WorkloadRecord>> {
    let row = conn
        .query_row(
            "SELECT id, mode, state, priority, reservation_bytes, cpu_slots, enforcement,
                    memory_max_bytes, cpu_max_cores, pids_max, queue_reason,
                    cancel_requested, root_exited, exit_code, last_error_code
             FROM workloads WHERE id = ?1",
            [workload_id.as_str()],
            workload_row,
        )
        .optional()?;
    Ok(row)
}

pub(crate) fn active_workloads(conn: &Connection) -> StorageResult<Vec<WorkloadRecord>> {
    // Non-terminal = every state a live daemon can still act on (QUEUED,
    // STARTING, RUNNING, STOPPING, DRAINING).
    let mut stmt = conn.prepare(
        "SELECT id, mode, state, priority, reservation_bytes, cpu_slots, enforcement,
                memory_max_bytes, cpu_max_cores, pids_max, queue_reason,
                cancel_requested, root_exited, exit_code, last_error_code
         FROM workloads
         WHERE state IN ('QUEUED', 'STARTING', 'RUNNING', 'STOPPING', 'DRAINING')
         ORDER BY created_at, id",
    )?;
    let rows = stmt
        .query_map([], workload_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// QUEUED entries ordered priority (0 first) -> created_at -> id, matching the
/// `workloads_queue` index (spec §4 queue snapshot).
pub(crate) fn queue_snapshot(conn: &Connection) -> StorageResult<Vec<QueuedWorkload>> {
    let mut stmt = conn.prepare(
        "SELECT w.id,
                (SELECT r.id FROM requests r WHERE r.workload_id = w.id LIMIT 1) AS request_id,
                w.priority, w.queue_reason, w.created_at
         FROM workloads w
         WHERE w.state = 'QUEUED'
         ORDER BY w.priority ASC, w.created_at ASC, w.id ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let reason_str: Option<String> = row.get("queue_reason")?;
            let request_id_str: Option<String> = row.get("request_id")?;
            Ok((
                row.get::<_, String>("id")?,
                request_id_str,
                row.get::<_, i64>("priority")?,
                reason_str,
                row.get::<_, String>("created_at")?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    rows.into_iter()
        .map(
            |(id_str, request_id_str, priority, reason_str, created_at)| {
                let queue_reason = match &reason_str {
                    None => None,
                    Some(s) => Some(queue_reason_from_str(s).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            Type::Text,
                            Box::new(StorageError::corrupt(format!("bad queue_reason {s:?}"))),
                        )
                    })?),
                };
                Ok(QueuedWorkload {
                    workload_id: WorkloadId::parse(&id_str).map_err(|_| {
                        StorageError::corrupt(format!("bad workload id {id_str:?}"))
                    })?,
                    request_id: match &request_id_str {
                        None => None,
                        Some(s) => {
                            Some(RequestId::parse(s).map_err(|_| {
                                StorageError::corrupt(format!("bad request id {s:?}"))
                            })?)
                        }
                    },
                    priority: Priority(
                        u8::try_from(priority).map_err(|_| {
                            StorageError::corrupt(format!("bad priority {priority}"))
                        })?,
                    ),
                    queue_reason,
                    created_at,
                })
            },
        )
        .collect()
}

pub(crate) fn session(conn: &Connection, id: &SessionId) -> StorageResult<Option<SessionRecord>> {
    Ok(conn.query_row(
        "SELECT id, workload_id, initial_cols, initial_rows, journal_relative_path,
                journal_limit_bytes, journal_bytes, last_seq, replay_status, pinned, created_at
         FROM sessions WHERE id = ?1",
        [id.as_str()], session_row,
    ).optional()?)
}

pub(crate) fn sessions(conn: &Connection) -> StorageResult<Vec<SessionRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, workload_id, initial_cols, initial_rows, journal_relative_path,
                journal_limit_bytes, journal_bytes, last_seq, replay_status, pinned, created_at
         FROM sessions ORDER BY created_at, id",
    )?;
    let rows = stmt
        .query_map([], session_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Retention candidates (SOTA_GAP_REVIEW W1-2): journals whose workload is
/// terminal, not pinned, and not already deleted. Read-only join — the
/// daemon additionally skips sessions it still holds live.
pub(crate) fn terminal_unpinned_sessions(conn: &Connection) -> StorageResult<Vec<SessionRecord>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.workload_id, s.initial_cols, s.initial_rows, s.journal_relative_path,
                s.journal_limit_bytes, s.journal_bytes, s.last_seq, s.replay_status, s.pinned, s.created_at
         FROM sessions s
         JOIN workloads w ON w.id = s.workload_id
         WHERE w.state IN ('SUCCEEDED','FAILED','CANCELLED','INTERRUPTED')
           AND s.pinned = 0
           AND s.replay_status != 'deleted'
         ORDER BY s.created_at, s.id",
    )?;
    let rows = stmt
        .query_map([], session_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 기록된 에이전트 세션 목록(spec `02-runner.md` §8).
///
/// 같은 (agent, agent_session_id)가 여러 워크로드에서 관찰됐으면 가장 최근
/// 것 하나만 돌려준다 — 목록은 "이어서 열 수 있는 대화" 단위지 관찰 이력이
/// 아니다. 동률은 id 내림차순으로 갈라 결과가 틱마다 흔들리지 않게 한다.
/// `active`는 스토리지가 알 수 없으므로 항상 false다 — 살아 있는 워크로드
/// 대조는 데몬이 채운다.
pub(crate) fn list_agent_sessions(
    conn: &Connection,
    limit: u32,
    cwd: Option<&str>,
    workload_id: Option<&WorkloadId>,
    pty_session_id: Option<&SessionId>,
) -> StorageResult<Vec<AgentSessionRecord>> {
    let mut stmt = conn.prepare(
        "WITH filtered AS (
            SELECT id, workload_id, pty_session_id, agent, agent_session_id, cwd,
                   title, program, source, first_seen_at, last_seen_at, ended_at, end_reason
            FROM agent_sessions
            WHERE (?2 IS NULL OR cwd = ?2)
              AND ((?3 IS NULL AND ?4 IS NULL) OR workload_id = ?3 OR pty_session_id = ?4)
         )
         SELECT a.id, a.workload_id, a.pty_session_id, a.agent, a.agent_session_id, a.cwd,
                a.title, a.program, a.source, a.first_seen_at, a.last_seen_at,
                a.ended_at, a.end_reason
         FROM filtered a
         WHERE NOT EXISTS (
            SELECT 1 FROM filtered b
            WHERE b.agent = a.agent
              AND b.agent_session_id = a.agent_session_id
              AND (b.last_seen_at > a.last_seen_at
                   OR (b.last_seen_at = a.last_seen_at AND b.id > a.id))
         )
         ORDER BY a.last_seen_at DESC, a.id DESC
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(
            params![
                limit,
                cwd,
                workload_id.map(WorkloadId::as_str),
                pty_session_id.map(SessionId::as_str)
            ],
            agent_session_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `agent_sessions` 행 → 계약 레코드. 열 순서는 위 SELECT와 같다.
pub(crate) fn agent_session_row(row: &Row) -> rusqlite::Result<AgentSessionRecord> {
    let workload_id: String = row.get(1)?;
    let pty_session_id: Option<String> = row.get(2)?;
    let source: String = row.get(8)?;
    Ok(AgentSessionRecord {
        id: row.get(0)?,
        workload_id: WorkloadId::parse(&workload_id).map_err(from_sql_invalid_id)?,
        pty_session_id: pty_session_id
            .as_deref()
            .map(SessionId::parse)
            .transpose()
            .map_err(from_sql_invalid_id)?,
        agent: row.get(3)?,
        agent_session_id: row.get(4)?,
        cwd: row.get(5)?,
        title: row.get(6)?,
        program: row.get(7)?,
        source: AgentSessionSource::parse(&source).ok_or_else(|| bad_shape("source"))?,
        first_seen_at: row.get(9)?,
        last_seen_at: row.get(10)?,
        ended_at: row.get(11)?,
        end_reason: row.get(12)?,
        // 살아 있는 워크로드 대조는 데몬의 몫이다.
        active: false,
    })
}

fn session_row(row: &Row) -> rusqlite::Result<SessionRecord> {
    let id: String = row.get(0)?;
    let workload_id: String = row.get(1)?;
    let cols: i64 = row.get(2)?;
    let rows_: i64 = row.get(3)?;
    Ok(SessionRecord {
        id: SessionId::parse(&id).map_err(from_sql_invalid_id)?,
        workload_id: WorkloadId::parse(&workload_id).map_err(from_sql_invalid_id)?,
        initial_cols: u16::try_from(cols).map_err(|_| bad_shape("initial_cols"))?,
        initial_rows: u16::try_from(rows_).map_err(|_| bad_shape("initial_rows"))?,
        journal_relative_path: row.get(4)?,
        journal_limit_bytes: positive_u64(row.get(5)?, "journal_limit_bytes")?,
        journal_bytes: non_negative_u64(row.get(6)?, "journal_bytes")?,
        last_seq: non_negative_u64(row.get(7)?, "last_seq")?,
        replay_status: row.get(8)?,
        pinned: row.get::<_, i64>(9)? != 0,
        created_at: row.get(10)?,
    })
}

fn workload_row(row: &Row) -> rusqlite::Result<WorkloadRecord> {
    let id: String = row.get(0)?;
    let mode: String = row.get(1)?;
    let state: String = row.get(2)?;
    let priority: i64 = row.get(3)?;
    let reservation: i64 = row.get(4)?;
    let cpu_slots: i64 = row.get(5)?;
    let enforcement: String = row.get(6)?;
    let queue_reason: Option<String> = row.get(10)?;
    let exit_code: Option<i64> = row.get(13)?;

    Ok(WorkloadRecord {
        id: WorkloadId::parse(&id).map_err(from_sql_invalid_id)?,
        mode: launch_mode_from_str(&mode).ok_or_else(|| bad_shape("mode"))?,
        state: workload_state_from_str(&state).ok_or_else(|| bad_shape("state"))?,
        priority: Priority(u8::try_from(priority).map_err(|_| bad_shape("priority"))?),
        reservation_bytes: U64String::new(reservation as u64)
            .map_err(|_| bad_shape("reservation_bytes"))?,
        cpu_slots: u32::try_from(cpu_slots).map_err(|_| bad_shape("cpu_slots"))?,
        enforcement: enforcement_from_str(&enforcement).ok_or_else(|| bad_shape("enforcement"))?,
        memory_max_bytes: row
            .get::<_, Option<i64>>(7)?
            .map(|v| U64String::new(v as u64).map_err(|_| bad_shape("memory_max_bytes")))
            .transpose()?,
        cpu_max_cores: row.get(8)?,
        pids_max: row
            .get::<_, Option<i64>>(9)?
            .map(|v| u32::try_from(v).map_err(|_| bad_shape("pids_max")))
            .transpose()?,
        queue_reason: match queue_reason.as_deref() {
            None => None,
            Some(s) => Some(queue_reason_from_str(s).ok_or_else(|| bad_shape("queue_reason"))?),
        },
        cancel_requested: row.get::<_, i64>(11)? != 0,
        root_exited: row.get::<_, i64>(12)? != 0,
        exit_code: exit_code
            .map(|v| i32::try_from(v).map_err(|_| bad_shape("exit_code")))
            .transpose()?,
        last_error_code: row.get(14)?,
    })
}

fn from_sql_invalid_id(e: term_contracts::ids::IdParseError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(e))
}

fn bad_shape(column: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        Type::Text,
        Box::new(StorageError::corrupt(format!(
            "column {column} holds an unexpected value"
        ))),
    )
}

fn positive_u64(v: i64, column: &'static str) -> rusqlite::Result<u64> {
    if v > 0 {
        Ok(v as u64)
    } else {
        Err(bad_shape(column))
    }
}

fn non_negative_u64(v: i64, column: &'static str) -> rusqlite::Result<u64> {
    if v >= 0 {
        Ok(v as u64)
    } else {
        Err(bad_shape(column))
    }
}

/// PRAGMA journal_mode on the given connection (test/diagnostic accessor).
pub(crate) fn journal_mode(conn: &Connection) -> StorageResult<String> {
    Ok(conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?)
}
