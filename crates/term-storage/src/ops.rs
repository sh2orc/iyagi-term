//! Write operations. Every function here runs on the single serialized writer
//! thread (or during `Storage::open` before the thread takes over the
//! connection), inside one transaction per operation where the operation spans
//! multiple statements (spec `01-contracts.md` §7: DB 쓰기는 전용 worker 하나에서
//! 직렬화).

use rusqlite::{params, Connection, OptionalExtension};
use term_contracts::agent_session::AgentSessionRecord;
use term_contracts::ids::{SessionId, WorkloadId};
use term_contracts::state::WorkloadState;
use term_contracts::workload::ProcessOwnership;

use crate::enums::*;
use crate::error::{StorageError, StorageResult};
use crate::queries;
use crate::time::now_iso8601;
use crate::types::{
    AgentSessionUpsert, LaunchIntent, LaunchIntentOutcome, ProcessOwnershipRow, ReconciledWorkload,
    RequestOutcome,
};

/// Reason code recorded for daemon-restart reconciliation events (spec §6).
pub(crate) const REASON_DAEMON_RESTART: &str = "DAEMON_RESTART";

/// `agent_sessions.end_reason`을 데몬 재시작 정리가 쓰는 값(§8). 워크로드
/// 쪽 `DAEMON_RESTART`와 달리 소문자 코드다 — 이 열은 계약
/// [`AgentSessionRecord::end_reason`]의 코드 집합을 따른다.
pub(crate) const AGENT_END_DAEMON_RESTART: &str = "daemon_restart";

/// Record one launch intent as a single transaction: tasks + attempts (managed
/// only) + workloads(QUEUED) + sessions + requests(accepted) + the initial
/// lifecycle event (spec §6: request_id와 fingerprint를 같은 transaction에서 저장).
///
/// Race-free duplicate handling: the requests row is checked inside the same
/// transaction that would insert it. Same id + same fingerprint returns the
/// existing workload's *current* state without writing anything; same id +
/// different fingerprint is [`StorageError::RequestConflict`].
pub(crate) fn record_launch_intent(
    conn: &mut Connection,
    intent: &LaunchIntent,
) -> StorageResult<LaunchIntentOutcome> {
    intent.validate()?;
    let now = now_iso8601();
    let tx = conn.transaction()?;

    let existing = tx
        .query_row(
            "SELECT fingerprint, outcome, workload_id FROM requests WHERE id = ?1",
            params![intent.request_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;

    if let Some((stored_fingerprint, outcome_str, workload_id_str)) = existing {
        if stored_fingerprint != intent.fingerprint {
            return Err(StorageError::RequestConflict {
                request_id: intent.request_id.to_string(),
            });
        }
        let (outcome, workload_id, state) =
            queries::request_state_parts(&tx, &outcome_str, &workload_id_str)?;
        return Ok(LaunchIntentOutcome::Existing {
            outcome,
            workload_id,
            state,
        });
    }

    if let (Some(task_id), Some(attempt_id)) = (&intent.task_id, &intent.attempt_id) {
        tx.execute(
            "INSERT INTO tasks(id, title, created_at) VALUES (?1, ?2, ?3)",
            params![task_id.as_str(), intent.title, now],
        )?;
        tx.execute(
            "INSERT INTO attempts(id, task_id, ordinal, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                attempt_id.as_str(),
                task_id.as_str(),
                intent.attempt_ordinal,
                now
            ],
        )?;
    }

    let effective_policy_json = serde_json::to_string(&intent.policy)
        .map_err(|e| StorageError::corrupt(format!("policy serialization failed: {e}")))?;
    tx.execute(
        "INSERT INTO workloads (
            id, attempt_id, mode, state, priority,
            reservation_bytes, cpu_slots, enforcement,
            memory_max_bytes, cpu_max_cores, pids_max,
            effective_policy_json, queue_reason,
            cancel_requested, root_exited,
            created_at
        ) VALUES (?1, ?2, ?3, 'QUEUED', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, 0, 0, ?12)",
        params![
            intent.workload_id.as_str(),
            intent.attempt_id.as_ref().map(|a| a.as_str()),
            launch_mode_to_str(intent.mode),
            intent.priority.0,
            intent.policy.reservation_bytes.get() as i64,
            intent.policy.cpu_slots,
            enforcement_to_str(intent.policy.enforcement),
            intent
                .policy
                .memory_max_bytes
                .as_ref()
                .map(|v| v.get() as i64),
            intent.policy.cpu_max_cores,
            intent.policy.pids_max,
            effective_policy_json,
            now,
        ],
    )?;
    tx.execute(
        "INSERT INTO sessions (
            id, workload_id, initial_cols, initial_rows,
            journal_relative_path, journal_limit_bytes, journal_bytes, last_seq,
            created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, ?7)",
        params![
            intent.session_id.as_str(),
            intent.workload_id.as_str(),
            intent.cols,
            intent.rows,
            intent.journal_relative_path,
            intent.journal_limit_bytes as i64,
            now,
        ],
    )?;
    tx.execute(
        "INSERT INTO requests(id, method, fingerprint, workload_id, outcome, created_at)
         VALUES (?1, ?2, ?3, ?4, 'accepted', ?5)",
        params![
            intent.request_id.as_str(),
            intent.method.as_str(),
            intent.fingerprint,
            intent.workload_id.as_str(),
            now,
        ],
    )?;
    tx.execute(
        "INSERT INTO lifecycle_events(workload_id, from_state, to_state, reason_code, created_at)
         VALUES (?1, NULL, 'QUEUED', NULL, ?2)",
        params![intent.workload_id.as_str(), now],
    )?;

    tx.commit()?;
    Ok(LaunchIntentOutcome::Created {
        workload_id: intent.workload_id.clone(),
        session_id: intent.session_id.clone(),
        state: WorkloadState::Queued,
    })
}

/// Validated state transition (spec §5 via `WorkloadState::can_transition`).
/// Appends a lifecycle event; entering STARTING stamps `started_at`, entering
/// a terminal state stamps `finished_at`/`exit_code` and upgrades the request
/// outcome. Illegal transitions error and write nothing.
pub(crate) fn transition_workload(
    conn: &mut Connection,
    workload_id: &WorkloadId,
    to: WorkloadState,
    exit_code: Option<i32>,
    reason_code: Option<&str>,
) -> StorageResult<()> {
    let now = now_iso8601();
    let tx = conn.transaction()?;

    let from_str: Option<String> = tx
        .query_row(
            "SELECT state FROM workloads WHERE id = ?1",
            params![workload_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let from =
        workload_state_from_str(from_str.as_deref().ok_or(StorageError::WorkloadNotFound {
            workload_id: workload_id.to_string(),
        })?)
        .ok_or_else(|| {
            StorageError::corrupt(format!(
                "unknown state {from_str:?} for workload {workload_id}"
            ))
        })?;

    if !from.can_transition(to) {
        return Err(StorageError::InvalidState {
            workload_id: workload_id.to_string(),
            from,
            to,
        });
    }

    let to_str = workload_state_to_str(to);
    if to == WorkloadState::Starting {
        tx.execute(
            "UPDATE workloads SET state = ?2, started_at = COALESCE(started_at, ?3) WHERE id = ?1",
            params![workload_id.as_str(), to_str, now],
        )?;
    } else if to.is_terminal() {
        tx.execute(
            "UPDATE workloads
             SET state = ?2, exit_code = ?3, last_error_code = ?4, finished_at = ?5
             WHERE id = ?1",
            params![workload_id.as_str(), to_str, exit_code, reason_code, now],
        )?;
    } else {
        tx.execute(
            "UPDATE workloads SET state = ?2 WHERE id = ?1",
            params![workload_id.as_str(), to_str],
        )?;
    }

    tx.execute(
        "INSERT INTO lifecycle_events(workload_id, from_state, to_state, reason_code, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            workload_id.as_str(),
            workload_state_to_str(from),
            to_str,
            reason_code,
            now,
        ],
    )?;

    if to.is_terminal() {
        // Request outcome upgrade on completion: SUCCEEDED/CANCELLED ->
        // completed, FAILED -> failed, INTERRUPTED -> unknown (daemon restart
        // or ambiguous gate release leaves the outcome undetermined, §6).
        let outcome = match to {
            WorkloadState::Succeeded | WorkloadState::Cancelled => RequestOutcome::Completed,
            WorkloadState::Failed => RequestOutcome::Failed,
            WorkloadState::Interrupted => RequestOutcome::Unknown,
            _ => unreachable!("terminal set checked above"),
        };
        tx.execute(
            "UPDATE requests SET outcome = ?2 WHERE workload_id = ?1 AND outcome = 'accepted'",
            params![workload_id.as_str(), request_outcome_to_str(outcome)],
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Insert or refresh the process_ownership row for a workload (spec §1: pid +
/// start_token + boot_id, all three compared before any signal).
pub(crate) fn save_group_identity(
    conn: &mut Connection,
    ownership: &ProcessOwnership,
) -> StorageResult<()> {
    if ownership.identity.pid == 0 {
        return Err(StorageError::InvalidArgument("pid must be > 0"));
    }
    let now = now_iso8601();
    let tx = conn.transaction()?;
    let exists = tx
        .query_row(
            "SELECT 1 FROM workloads WHERE id = ?1",
            params![ownership.workload_id.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        return Err(StorageError::WorkloadNotFound {
            workload_id: ownership.workload_id.to_string(),
        });
    }
    tx.execute(
        "INSERT INTO process_ownership (
            workload_id, pid, start_token, boot_id, group_kind, group_reference, coverage, recorded_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(workload_id) DO UPDATE SET
            pid = excluded.pid,
            start_token = excluded.start_token,
            boot_id = excluded.boot_id,
            group_kind = excluded.group_kind,
            group_reference = excluded.group_reference,
            coverage = excluded.coverage,
            recorded_at = excluded.recorded_at",
        params![
            ownership.workload_id.as_str(),
            ownership.identity.pid,
            ownership.identity.start_token,
            ownership.identity.boot_id,
            group_kind_to_str(ownership.group_kind),
            ownership.group_reference,
            usage_coverage_to_str(ownership.coverage),
            now,
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn set_queue_reason(
    conn: &mut Connection,
    workload_id: &WorkloadId,
    reason: Option<term_contracts::snapshot::QueueReason>,
) -> StorageResult<()> {
    set_optional_text_column(
        conn,
        workload_id,
        "queue_reason",
        reason.map(queue_reason_to_str),
    )
}

pub(crate) fn set_bool_flag(
    conn: &mut Connection,
    workload_id: &WorkloadId,
    column: &'static str,
    value: bool,
) -> StorageResult<()> {
    debug_assert!(matches!(column, "cancel_requested" | "root_exited"));
    let changed = conn.execute(
        &format!("UPDATE workloads SET {column} = ?2 WHERE id = ?1"),
        params![workload_id.as_str(), value as i64],
    )?;
    if changed == 0 {
        return Err(StorageError::WorkloadNotFound {
            workload_id: workload_id.to_string(),
        });
    }
    Ok(())
}

fn set_optional_text_column(
    conn: &mut Connection,
    workload_id: &WorkloadId,
    column: &'static str,
    value: Option<&str>,
) -> StorageResult<()> {
    debug_assert_eq!(column, "queue_reason");
    let changed = conn.execute(
        &format!("UPDATE workloads SET {column} = ?2 WHERE id = ?1"),
        params![workload_id.as_str(), value],
    )?;
    if changed == 0 {
        return Err(StorageError::WorkloadNotFound {
            workload_id: workload_id.to_string(),
        });
    }
    Ok(())
}

/// Session journal progress. `last_seq` is monotonic and both values must fit
/// the SQLite signed-INTEGER bound — at the cap the caller gets an explicit
/// lifecycle_events 정리(W2): 가장 최근 `keep_recent`개만 남긴다.
/// 무한 증가 방지가 목적이라 전역 상한으로 충분하다(감사는 최근 이력이면
/// 충분하다 — I03 남은 위험).
pub(crate) fn prune_lifecycle_events(conn: &mut Connection, keep_recent: i64) -> StorageResult<()> {
    if keep_recent < 0 {
        return Err(StorageError::InvalidArgument("keep_recent must be >= 0"));
    }
    conn.execute(
        "DELETE FROM lifecycle_events WHERE id <= (SELECT COALESCE(MAX(id), 0) - ?1 FROM lifecycle_events)",
        params![keep_recent],
    )?;
    Ok(())
}

/// Mark a session's journal deleted by retention (W1-2). Zeroes the byte
/// accounting and flags `replay_status='deleted'`; a pinned session is left
/// untouched (the WHERE clause makes it a no-op, not an error).
pub(crate) fn mark_journal_deleted(
    conn: &mut Connection,
    session_id: &SessionId,
) -> StorageResult<()> {
    conn.execute(
        "UPDATE sessions SET replay_status = 'deleted', journal_bytes = 0
         WHERE id = ?1 AND pinned = 0 AND replay_status != 'deleted'",
        params![session_id.as_str()],
    )?;
    Ok(())
}

/// error and the stored row is never wrapped (spec §1: seq가 상한에 도달하면
/// 명시적 오류). `journal_bytes` may shrink (retention truncation).
pub(crate) fn update_session_progress(
    conn: &mut Connection,
    session_id: &SessionId,
    last_seq: u64,
    journal_bytes: u64,
) -> StorageResult<()> {
    if last_seq > i64::MAX as u64 {
        return Err(StorageError::SeqOverflow {
            session_id: session_id.to_string(),
            attempted: last_seq,
        });
    }
    if journal_bytes > i64::MAX as u64 {
        return Err(StorageError::JournalBytesOverflow {
            session_id: session_id.to_string(),
            attempted: journal_bytes,
        });
    }
    let tx = conn.transaction()?;
    let stored: Option<(i64, i64)> = tx
        .query_row(
            "SELECT last_seq, journal_bytes FROM sessions WHERE id = ?1",
            params![session_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (stored_seq, _) = stored.ok_or(StorageError::SessionNotFound {
        session_id: session_id.to_string(),
    })?;
    if last_seq < stored_seq as u64 {
        return Err(StorageError::SeqRegression {
            session_id: session_id.to_string(),
            stored: stored_seq as u64,
            attempted: last_seq,
        });
    }
    tx.execute(
        "UPDATE sessions SET last_seq = ?2, journal_bytes = ?3 WHERE id = ?1",
        params![session_id.as_str(), last_seq as i64, journal_bytes as i64],
    )?;
    tx.commit()?;
    Ok(())
}

/// Crash reconciliation on open (spec §6: daemon 재시작 시 이전 작업을 재실행하지
/// 않는다). Every non-terminal workload becomes INTERRUPTED with one
/// `DAEMON_RESTART` lifecycle event; its request outcome becomes `unknown`.
/// process_ownership rows survive so the daemon can surface
/// `reconciliation_required` with identity information (and never kill by pid
/// alone).
pub(crate) fn reconcile_crashed_workloads(
    conn: &mut Connection,
) -> StorageResult<Vec<ReconciledWorkload>> {
    let now = now_iso8601();
    let tx = conn.transaction()?;

    let mut stmt = tx.prepare(
        "SELECT id, state FROM workloads
         WHERE state IN ('QUEUED', 'STARTING', 'RUNNING', 'STOPPING', 'DRAINING')
         ORDER BY created_at, id",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    let mut reconciled = Vec::with_capacity(rows.len());
    for (id_str, state_str) in rows {
        let workload_id = WorkloadId::parse(&id_str)
            .map_err(|_| StorageError::corrupt(format!("bad id {id_str:?}")))?;
        let previous_state = workload_state_from_str(&state_str).ok_or_else(|| {
            StorageError::corrupt(format!("unknown state {state_str:?} for workload {id_str}"))
        })?;
        if !previous_state.can_transition(WorkloadState::Interrupted) {
            return Err(StorageError::corrupt(format!(
                "workload {id_str} in state {state_str} cannot be reconciled"
            )));
        }
        tx.execute(
            "UPDATE workloads
             SET state = 'INTERRUPTED', last_error_code = ?2, finished_at = ?3
             WHERE id = ?1",
            params![id_str, REASON_DAEMON_RESTART, now],
        )?;
        tx.execute(
            "INSERT INTO lifecycle_events(workload_id, from_state, to_state, reason_code, created_at)
             VALUES (?1, ?2, 'INTERRUPTED', ?3, ?4)",
            params![
                id_str,
                workload_state_to_str(previous_state),
                REASON_DAEMON_RESTART,
                now,
            ],
        )?;
        tx.execute(
            "UPDATE requests SET outcome = 'unknown' WHERE workload_id = ?1 AND outcome = 'accepted'",
            params![id_str],
        )?;
        let ownership = load_ownership(&tx, &workload_id)?;
        reconciled.push(ReconciledWorkload {
            workload_id,
            previous_state,
            ownership,
        });
    }

    // 에이전트 세션도 같은 트랜잭션에서 닫는다(§8): 이전 프로세스가 남긴
    // 열린 행은 PTY가 끊긴 순간 이미 끝난 것이다. 워크로드와 달리 행 자체는
    // 남긴다 — 사용자가 "이어서 열기"로 복구할 목록이 그것이다.
    tx.execute(
        "UPDATE agent_sessions SET ended_at = ?1, end_reason = ?2 WHERE ended_at IS NULL",
        params![now, AGENT_END_DAEMON_RESTART],
    )?;

    tx.commit()?;
    Ok(reconciled)
}

// ---------------------------------------------------------------------------
// agent_sessions (spec `02-runner.md` §8)

/// 관찰한 에이전트 세션을 upsert한다. 같은 (workload, agent, session id)를
/// 다시 보면 새 행이 아니라 `last_seen_at` 갱신이다 — 그러면서 종료 표시
/// (`ended_at`/`end_reason`)를 지운다: 다시 관찰됐다는 것은 살아 있다는 뜻
/// 이므로, 데몬 재시작 정리가 닫아 둔 행이 되살아난다.
///
/// `title`/`program`/`pty_session_id`는 `COALESCE(new, old)`다 — 늦게
/// 도착한 hook 보고(제목을 모른다)가 프로세스 관찰이 채운 제목을 지우지
/// 않게 한다. `cwd`는 최초 관찰값을 유지한다.
pub(crate) fn upsert_agent_session(
    conn: &mut Connection,
    upsert: &AgentSessionUpsert,
) -> StorageResult<AgentSessionRecord> {
    if upsert.agent.trim().is_empty() {
        return Err(StorageError::InvalidArgument("agent must not be empty"));
    }
    if !term_contracts::agent_session::valid_session_id(&upsert.agent_session_id) {
        return Err(StorageError::InvalidArgument(
            "agent_session_id must be 1..=128 identifier chars",
        ));
    }
    let now = now_iso8601();
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO agent_sessions (
            id, workload_id, pty_session_id, agent, agent_session_id, cwd,
            title, program, source, first_seen_at, last_seen_at, ended_at, end_reason
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, NULL, NULL)
         ON CONFLICT(workload_id, agent, agent_session_id) DO UPDATE SET
            last_seen_at = excluded.last_seen_at,
            title = COALESCE(excluded.title, agent_sessions.title),
            program = COALESCE(excluded.program, agent_sessions.program),
            source = excluded.source,
            pty_session_id = COALESCE(excluded.pty_session_id, agent_sessions.pty_session_id),
            ended_at = NULL,
            end_reason = NULL",
        params![
            uuid::Uuid::new_v4().to_string(),
            upsert.workload_id.as_str(),
            upsert.pty_session_id.as_ref().map(|s| s.as_str()),
            upsert.agent,
            upsert.agent_session_id,
            upsert.cwd,
            upsert.title,
            upsert.program,
            upsert.source.as_str(),
            now,
        ],
    )?;
    let record = tx
        .query_row(
            "SELECT id, workload_id, pty_session_id, agent, agent_session_id, cwd,
                    title, program, source, first_seen_at, last_seen_at, ended_at, end_reason
             FROM agent_sessions
             WHERE workload_id = ?1 AND agent = ?2 AND agent_session_id = ?3",
            params![
                upsert.workload_id.as_str(),
                upsert.agent,
                upsert.agent_session_id
            ],
            queries::agent_session_row,
        )
        .optional()?
        .ok_or_else(|| StorageError::corrupt("agent session vanished right after upsert"))?;
    tx.commit()?;
    Ok(record)
}

/// 이 워크로드에서 아직 열려 있는 에이전트 세션을 모두 닫는다(pane 종료).
pub(crate) fn end_agent_sessions_for_workload(
    conn: &mut Connection,
    workload_id: &WorkloadId,
    reason: &str,
) -> StorageResult<usize> {
    let now = now_iso8601();
    let changed = conn.execute(
        "UPDATE agent_sessions SET ended_at = ?2, end_reason = ?3
         WHERE workload_id = ?1 AND ended_at IS NULL",
        params![workload_id.as_str(), now, reason],
    )?;
    Ok(changed)
}

/// 한 건만 닫는다(세션 교체·hook `SessionEnd`). 이미 닫혀 있으면 false.
pub(crate) fn end_agent_session(
    conn: &mut Connection,
    workload_id: &WorkloadId,
    agent: &str,
    agent_session_id: &str,
    reason: &str,
) -> StorageResult<bool> {
    let now = now_iso8601();
    let changed = conn.execute(
        "UPDATE agent_sessions SET ended_at = ?4, end_reason = ?5
         WHERE workload_id = ?1 AND agent = ?2 AND agent_session_id = ?3
           AND ended_at IS NULL",
        params![workload_id.as_str(), agent, agent_session_id, now, reason],
    )?;
    Ok(changed > 0)
}

/// 목록에서 한 건을 영구히 지운다(사용자 동작). 파일은 건드리지 않는다 —
/// 이 테이블은 에이전트 자신의 기록이 아니라 우리 쪽 색인일 뿐이다.
pub(crate) fn forget_agent_session(conn: &mut Connection, id: &str) -> StorageResult<bool> {
    let changed = conn.execute("DELETE FROM agent_sessions WHERE id = ?1", params![id])?;
    Ok(changed > 0)
}

/// 보존 정리: (1) 이미 종료됐고 `max_age_days`보다 오래 관찰되지 않은 행,
/// (2) 이미 종료됐고 최신 `keep_at_most`개 밖으로 밀려난 행을 지운다.
///
/// **아직 열려 있는 행(`ended_at IS NULL`)은 둘 중 어느 기준으로도 지우지
/// 않는다.** 지금 붙어 있는 대화가 목록에서 사라지면 "이어서 열기"가
/// 불가능해진다 — 개수 상한은 이력을 자르는 장치지 살아 있는 세션을 버리는
/// 장치가 아니다. 열린 행은 종료될 때(`end_reason`) 비로소 정리 대상이 된다.
pub(crate) fn prune_agent_sessions(
    conn: &mut Connection,
    max_age_days: u32,
    keep_at_most: u32,
) -> StorageResult<usize> {
    let cutoff = crate::time::iso8601_days_ago(max_age_days);
    let tx = conn.transaction()?;
    let aged = tx.execute(
        "DELETE FROM agent_sessions WHERE ended_at IS NOT NULL AND last_seen_at < ?1",
        params![cutoff],
    )?;
    // 정렬·LIMIT은 열린 행까지 포함해 센다(최신 N건 = 목록이 보여 주는
    // 그것). 지우는 것만 종료된 행으로 제한한다.
    let overflow = tx.execute(
        "DELETE FROM agent_sessions WHERE ended_at IS NOT NULL AND id NOT IN (
            SELECT id FROM agent_sessions ORDER BY last_seen_at DESC, id DESC LIMIT ?1
         )",
        params![keep_at_most],
    )?;
    tx.commit()?;
    Ok(aged + overflow)
}

pub(crate) fn load_ownership(
    conn: &Connection,
    workload_id: &WorkloadId,
) -> StorageResult<Option<ProcessOwnershipRow>> {
    let row = conn
        .query_row(
            "SELECT workload_id, pid, start_token, boot_id, group_kind, group_reference, coverage, recorded_at
             FROM process_ownership WHERE workload_id = ?1",
            params![workload_id.as_str()],
            |row| {
                Ok(ProcessOwnershipRaw {
                    workload_id: row.get(0)?,
                    pid: row.get(1)?,
                    start_token: row.get(2)?,
                    boot_id: row.get(3)?,
                    group_kind: row.get(4)?,
                    group_reference: row.get(5)?,
                    coverage: row.get(6)?,
                    recorded_at: row.get(7)?,
                })
            },
        )
        .optional()?;
    let raw = match row {
        Some(raw) => raw,
        None => return Ok(None),
    };
    Ok(Some(ProcessOwnershipRow {
        workload_id: WorkloadId::parse(&raw.workload_id)
            .map_err(|_| StorageError::corrupt(format!("bad workload id {:?}", raw.workload_id)))?,
        identity: term_contracts::ids::ProcessIdentity {
            pid: u32::try_from(raw.pid)
                .map_err(|_| StorageError::corrupt(format!("pid {} out of u32 range", raw.pid)))?,
            start_token: raw.start_token,
            boot_id: raw.boot_id,
        },
        group_kind: group_kind_from_str(&raw.group_kind)
            .ok_or_else(|| StorageError::corrupt(format!("bad group_kind {:?}", raw.group_kind)))?,
        group_reference: raw.group_reference,
        coverage: usage_coverage_from_str(&raw.coverage)
            .ok_or_else(|| StorageError::corrupt(format!("bad coverage {:?}", raw.coverage)))?,
        recorded_at: raw.recorded_at,
    }))
}

struct ProcessOwnershipRaw {
    workload_id: String,
    pid: i64,
    start_token: String,
    boot_id: String,
    group_kind: String,
    group_reference: Option<String>,
    coverage: String,
    recorded_at: String,
}
