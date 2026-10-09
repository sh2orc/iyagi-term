//! Mission read paths: snapshot materialization, event tail, request lookup,
//! keyset mission list, and outbox scans. All read-only; artifact bodies are
//! never touched here (01 §4: snapshots exclude artifact bodies).

use rusqlite::{params, Connection, OptionalExtension};
use term_contracts::mission::types::{
    Candidate, Decision, Entity, EntityKind, Finding, Id, Knowledge, Message, Mission,
    MissionEvent, MutationResult, Verification, Workspace,
};

use super::types::{
    MissionListCursor, MissionSnapshotData, MissionStoreError, MissionStoreResult, OutboxOperation,
    OutboxState, StoredEvent, StoredOutbox, StoredRequest,
};

fn corrupt(context: &str, error: impl std::fmt::Display) -> MissionStoreError {
    MissionStoreError::Corrupt(format!("{context}: {error}"))
}

/// A single SQL statement provides a consistent view across the pending set
/// and prior reservations. LEFT JOIN deliberately retains broken Run links
/// so corruption cannot silently erase process ownership.
pub fn exec_recovery_records(
    conn: &Connection,
    current_owner: &Id,
    previous: &[Id],
) -> MissionStoreResult<Vec<term_contracts::mission::types::ExecRecord>> {
    use term_contracts::mission::types::{ExecRecord, Run};
    const LIMIT: usize = 8192;
    if previous.len() > LIMIT {
        return Err(corrupt("exec recovery", "too many unresolved executions"));
    }
    let previous = serde_json::to_string(previous).map_err(|e| corrupt("exec recovery ids", e))?;
    let mut statement = conn.prepare(
        "WITH candidates AS (
            SELECT id FROM orch_execs WHERE state != 'exited'
              AND json_extract(document_json, '$.owner_daemon_id') IS NOT ?1
            UNION SELECT value FROM json_each(?2)
         )
         SELECT e.id, e.mission_id, e.run_id, e.state, e.document_json, r.document_json
         FROM candidates c LEFT JOIN orch_execs e ON e.id = c.id
         LEFT JOIN orch_runs r ON r.id = e.run_id AND r.mission_id = e.mission_id
         ORDER BY c.id LIMIT 8193",
    )?;
    let rows = statement.query_map(params![current_owner.as_str(), previous], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut records = vec![];
    for row in rows {
        let (id, mission_id, run_id, state, document, run) = row?;
        let exec: ExecRecord =
            serde_json::from_str(&document).map_err(|e| corrupt("exec recovery", e))?;
        let run: Run =
            serde_json::from_str(&run.ok_or_else(|| corrupt("exec recovery", "missing run"))?)
                .map_err(|e| corrupt("exec recovery run", e))?;
        if exec.id.as_str() != id
            || exec.mission_id.as_str() != mission_id
            || exec.run_id.as_str() != run_id
            || serde_json::to_value(exec.state).map_err(|e| corrupt("exec recovery state", e))?
                != state
            || run.id != exec.run_id
            || run.mission_id != exec.mission_id
            || run.exec_id.as_ref() != Some(&exec.id)
            || run
                .binding_snapshot
                .as_ref()
                .is_some_and(|binding| binding.resource_policy != exec.resource_policy)
            || &exec.owner_daemon_id == current_owner
        {
            return Err(corrupt(
                "exec recovery",
                "ownership columns or Run link disagree",
            ));
        }
        records.push(exec);
        if records.len() > LIMIT {
            return Err(corrupt("exec recovery", "too many unresolved executions"));
        }
    }
    Ok(records)
}

pub fn rate_limit_runs(
    conn: &Connection,
    now_ms: u64,
) -> MissionStoreResult<Vec<term_contracts::mission::types::Run>> {
    let mut statement = conn.prepare(
        "SELECT document_json FROM orch_runs
         WHERE json_type(document_json, '$.rate_limit') = 'object'
         AND CAST(json_extract(document_json, '$.rate_limit.resets_at_unix_ms') AS INTEGER) > ?1",
    )?;
    let rows = statement.query_map([now_ms.min(i64::MAX as u64) as i64], |row| {
        row.get::<_, String>(0)
    })?;
    rows.map(|row| serde_json::from_str(&row?).map_err(|e| corrupt("rate limit run", e)))
        .collect()
}

/// Materialize the current projection of one mission as entity values, with
/// the read transaction's watermark (revision = event_seq).
pub fn materialize(
    conn: &Connection,
    mission_id: &Id,
) -> MissionStoreResult<Option<MissionSnapshotData>> {
    let head: Option<(i64, i64)> = conn
        .query_row(
            "SELECT revision, event_seq FROM orch_missions WHERE id = ?1",
            params![mission_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((revision, event_seq)) = head else {
        return Ok(None);
    };

    let mut entities = Vec::new();

    let mission_doc: String = conn.query_row(
        "SELECT document_json FROM orch_missions WHERE id = ?1",
        params![mission_id.as_str()],
        |row| row.get(0),
    )?;
    let mission: Mission =
        serde_json::from_str(&mission_doc).map_err(|e| corrupt("mission document", e))?;
    entities.push(Entity::Mission(Box::new(mission)));

    for doc in query_values(
        conn,
        "SELECT document_json FROM orch_tasks WHERE mission_id = ?1 ORDER BY ordinal",
        params![mission_id.as_str()],
        |row| row.get::<_, String>(0),
    )? {
        entities.push(Entity::Task(
            serde_json::from_str(&doc).map_err(|e| corrupt("task document", e))?,
        ));
    }
    for doc in query_values(
        conn,
        "SELECT document_json FROM orch_runs WHERE mission_id = ?1 ORDER BY attempt",
        params![mission_id.as_str()],
        |row| row.get::<_, String>(0),
    )? {
        entities.push(Entity::Run(
            serde_json::from_str(&doc).map_err(|e| corrupt("run document", e))?,
        ));
    }
    for doc in query_values(
        conn,
        "SELECT document_json FROM orch_execs WHERE mission_id = ?1",
        params![mission_id.as_str()],
        |row| row.get::<_, String>(0),
    )? {
        entities.push(Entity::Exec(
            serde_json::from_str(&doc).map_err(|e| corrupt("exec document", e))?,
        ));
    }
    for (kind, doc) in query_values(
        conn,
        "SELECT kind, document_json FROM orch_entities WHERE mission_id = ?1",
        params![mission_id.as_str()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )? {
        entities.push(parse_generic_entity(&kind, &doc)?);
    }

    Ok(Some(MissionSnapshotData {
        mission_id: mission_id.clone(),
        revision: revision as u64,
        event_seq: event_seq as u64,
        entities,
    }))
}

fn parse_generic_entity(kind: &str, doc: &str) -> MissionStoreResult<Entity> {
    let bad = |e: serde_json::Error| corrupt(&format!("{kind} document"), e);
    Ok(match kind {
        "message" => Entity::Message(Box::new(serde_json::from_str::<Message>(doc).map_err(bad)?)),
        "decision" => Entity::Decision(Box::new(
            serde_json::from_str::<Decision>(doc).map_err(bad)?,
        )),
        "workspace" => Entity::Workspace(Box::new(
            serde_json::from_str::<Workspace>(doc).map_err(bad)?,
        )),
        "candidate" => Entity::Candidate(Box::new(
            serde_json::from_str::<Candidate>(doc).map_err(bad)?,
        )),
        "verification" => Entity::Verification(Box::new(
            serde_json::from_str::<Verification>(doc).map_err(bad)?,
        )),
        "finding" => Entity::Finding(Box::new(serde_json::from_str::<Finding>(doc).map_err(bad)?)),
        "knowledge" => Entity::Knowledge(Box::new(
            serde_json::from_str::<Knowledge>(doc).map_err(bad)?,
        )),
        other => {
            return Err(MissionStoreError::Corrupt(format!(
                "unknown orch_entities kind {other:?}"
            )))
        }
    })
}

/// Committed events after `after_seq`, ascending, plus the read transaction's
/// high watermark (07 §7: empty page keeps the requested after_seq).
pub fn events_after(
    conn: &Connection,
    mission_id: &Id,
    after_seq: u64,
    limit: u32,
) -> MissionStoreResult<(Vec<StoredEvent>, u64)> {
    let limit = limit.clamp(1, 50) as i64;
    let mut events = Vec::new();
    for payload in query_values(
        conn,
        "SELECT payload_json FROM orch_events WHERE mission_id = ?1 AND seq > ?2
         ORDER BY seq ASC LIMIT ?3",
        params![mission_id.as_str(), after_seq as i64, limit],
        |row| row.get::<_, String>(0),
    )? {
        let event: MissionEvent =
            serde_json::from_str(&payload).map_err(|e| corrupt("event payload", e))?;
        events.push(StoredEvent { event });
    }
    let watermark: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), ?2) FROM orch_events WHERE mission_id = ?1",
        params![mission_id.as_str(), after_seq as i64],
        |row| row.get(0),
    )?;
    Ok((events, watermark as u64))
}

/// Request dedupe lookup (timeout → request.get → re-send flow, 01 §2).
pub fn get_request(
    conn: &Connection,
    request_id: &Id,
) -> MissionStoreResult<Option<StoredRequest>> {
    let row: Option<(Option<String>, String, String, String, String)> = conn
        .query_row(
            "SELECT mission_id, method, fingerprint, response_json, created_at
             FROM orch_requests WHERE id = ?1",
            params![request_id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((mission_id, method, fingerprint, response_json, created_at)) = row else {
        return Ok(None);
    };
    let response: MutationResult =
        serde_json::from_str(&response_json).map_err(|e| corrupt("stored request response", e))?;
    Ok(Some(StoredRequest {
        request_id: request_id.clone(),
        mission_id: mission_id
            .map(|m| Id::parse(&m))
            .transpose()
            .map_err(|e| corrupt("request mission id", e))?,
        method,
        fingerprint,
        response,
        created_at,
    }))
}

/// mission.list keyset pagination: `updated_at DESC, id DESC` (07 §7).
pub fn list_missions(
    conn: &Connection,
    cursor: Option<&MissionListCursor>,
    limit: u32,
    archived: bool,
) -> MissionStoreResult<(Vec<Mission>, Option<MissionListCursor>)> {
    let limit = limit.clamp(1, 50) as i64;
    // `archived_at IS NULL` vs `IS NOT NULL` cannot be parameterized; the
    // cursor can (NULL = first page).
    let archive_clause = if archived {
        "archived_at IS NOT NULL"
    } else {
        "archived_at IS NULL"
    };
    let (cursor_updated_at, cursor_id) = match cursor {
        Some(cursor) => (Some(cursor.updated_at.clone()), Some(cursor.id.to_string())),
        None => (None, None),
    };
    let sql = format!(
        "SELECT document_json, updated_at, id FROM orch_missions
         WHERE (?1 IS NULL OR (updated_at < ?1 OR (updated_at = ?1 AND id < ?2)))
           AND {archive_clause}
         ORDER BY updated_at DESC, id DESC LIMIT ?3"
    );
    let read_row = |row: &rusqlite::Row| -> rusqlite::Result<(String, String, String)> {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    };
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(params![cursor_updated_at, cursor_id, limit], read_row)?
        .collect::<Result<Vec<_>, _>>()?;
    let mut missions = Vec::new();
    for (doc, updated_at, id) in rows {
        let mission: Mission =
            serde_json::from_str(&doc).map_err(|e| corrupt("mission document", e))?;
        if mission.updated_at != updated_at || mission.id.as_str() != id {
            return Err(MissionStoreError::Corrupt(format!(
                "mission row {id} disagrees with its document"
            )));
        }
        missions.push(mission);
    }
    let next = if missions.len() as i64 == limit {
        missions.last().map(|m| MissionListCursor {
            updated_at: m.updated_at.clone(),
            id: m.id.clone(),
        })
    } else {
        None
    };
    Ok((missions, next))
}

/// Outbox scan for recovery: every non-terminal row, oldest first.
pub fn pending_outbox(conn: &Connection) -> MissionStoreResult<Vec<StoredOutbox>> {
    let rows = query_values(
        conn,
        "SELECT id, mission_id, run_id, operation, dedupe_key, fencing_token, state, payload_json, created_at, updated_at
         FROM orch_outbox WHERE state IN ('prepared','sending','unknown')
         ORDER BY created_at ASC, id ASC",
        [],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
            ))
        },
    )?;
    let mut outbox = Vec::new();
    for (
        id,
        mission_id,
        run_id,
        operation,
        dedupe_key,
        fencing_token,
        state,
        payload,
        created_at,
        updated_at,
    ) in rows
    {
        outbox.push(StoredOutbox {
            id: Id::parse(&id).map_err(|e| corrupt("outbox id", e))?,
            mission_id: Id::parse(&mission_id).map_err(|e| corrupt("outbox mission id", e))?,
            run_id: run_id
                .map(|r| Id::parse(&r))
                .transpose()
                .map_err(|e| corrupt("outbox run id", e))?,
            operation: OutboxOperation::parse(&operation)
                .ok_or_else(|| corrupt("outbox operation", &operation))?,
            dedupe_key,
            fencing_token: fencing_token.max(0) as u64,
            state: OutboxState::parse(&state).ok_or_else(|| corrupt("outbox state", &state))?,
            payload: serde_json::from_str(&payload).map_err(|e| corrupt("outbox payload", e))?,
            created_at,
            updated_at,
        });
    }
    Ok(outbox)
}

/// Advance an outbox row's state (recovery/ack bookkeeping).
pub fn set_outbox_state(
    conn: &mut Connection,
    id: &Id,
    state: OutboxState,
    updated_at: &str,
) -> MissionStoreResult<()> {
    conn.execute(
        "UPDATE orch_outbox SET state = ?2, updated_at = ?3 WHERE id = ?1",
        params![id.as_str(), state.as_str(), updated_at],
    )?;
    Ok(())
}

/// The mission revision for quick CAS reads without full materialization.
pub fn mission_revision(conn: &Connection, mission_id: &Id) -> MissionStoreResult<Option<u64>> {
    let revision: Option<i64> = conn
        .query_row(
            "SELECT revision FROM orch_missions WHERE id = ?1",
            params![mission_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(revision.map(|r| r as u64))
}

fn query_values<T, P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
    map: impl Fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> MissionStoreResult<Vec<T>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement
        .query_map(params, map)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Entity kind of an entity id inside one mission (scope checks, E24).
pub fn entity_exists(
    conn: &Connection,
    mission_id: &Id,
    kind: EntityKind,
    id: &Id,
) -> MissionStoreResult<bool> {
    let found: Option<i64> =
        match kind {
            EntityKind::Mission => conn
                .query_row(
                    "SELECT 1 FROM orch_missions WHERE id = ?1",
                    params![id.as_str()],
                    |row| row.get(0),
                )
                .optional()?,
            EntityKind::Task => conn
                .query_row(
                    "SELECT 1 FROM orch_tasks WHERE mission_id = ?1 AND id = ?2",
                    params![mission_id.as_str(), id.as_str()],
                    |row| row.get(0),
                )
                .optional()?,
            EntityKind::Run => conn
                .query_row(
                    "SELECT 1 FROM orch_runs WHERE mission_id = ?1 AND id = ?2",
                    params![mission_id.as_str(), id.as_str()],
                    |row| row.get(0),
                )
                .optional()?,
            EntityKind::Exec => conn
                .query_row(
                    "SELECT 1 FROM orch_execs WHERE mission_id = ?1 AND id = ?2",
                    params![mission_id.as_str(), id.as_str()],
                    |row| row.get(0),
                )
                .optional()?,
            _ => return conn
                .query_row(
                    "SELECT 1 FROM orch_entities WHERE mission_id = ?1 AND kind = ?2 AND id = ?3",
                    params![
                        mission_id.as_str(),
                        super::types::entity_kind_str(kind),
                        id.as_str()
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map(|v| v.is_some())
                .map_err(MissionStoreError::from),
        };
    Ok(found.is_some())
}

/// All stored bindings in id order.
pub fn list_bindings(conn: &Connection) -> MissionStoreResult<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    for doc in query_values(
        conn,
        "SELECT document_json FROM orch_bindings ORDER BY id",
        [],
        |row| row.get::<_, String>(0),
    )? {
        out.push(serde_json::from_str(&doc).map_err(|e| corrupt("binding document", e))?);
    }
    Ok(out)
}

/// All stored config documents of one kind (template | verification |
/// repository), id order.
pub fn list_configs(conn: &Connection, kind: &str) -> MissionStoreResult<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    for doc in query_values(
        conn,
        "SELECT document_json FROM orch_config WHERE kind = ?1 ORDER BY id",
        params![kind],
        |row| row.get::<_, String>(0),
    )? {
        out.push(serde_json::from_str(&doc).map_err(|e| corrupt("config document", e))?);
    }
    Ok(out)
}
