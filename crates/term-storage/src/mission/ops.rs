//! Mission transaction executor: every mutation is one `BEGIN IMMEDIATE`
//! transaction that carries projection upserts, the event row, outbox
//! intents, the request dedupe row, and the revision bump together
//! (spec `01-contracts.md` §2 — no partial state ever lands).

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use term_contracts::ids::U64String;
use term_contracts::mission::types::{
    Change, ChangeOperation, Entity, EntityKind, ExecRecord, Mission, MissionEvent, MutationResult,
    Run, Task,
};

use super::types::{
    AppliedTransition, ApplyMissionTransition, MissionStoreError, MissionStoreResult, OutboxIntent,
};

/// Event inline-changes budget (`defaults.json` `max_event_bytes`).
const MAX_EVENT_BYTES: usize = 16_384;

/// Live-run spellings for the one-live-run-per-task guard (states.json).
const LIVE_RUN_STATES: &str = "('prepared','starting','running','awaiting_input','stopping')";

fn state_str<T: serde::Serialize>(value: &T) -> MissionStoreResult<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or_else(|| MissionStoreError::Corrupt("state did not serialize to a string".into()))
}

fn document<T: serde::Serialize>(value: &T) -> MissionStoreResult<String> {
    serde_json::to_string(value).map_err(|e| MissionStoreError::Corrupt(e.to_string()))
}

/// Execute one mission transaction. Idempotent by request_id: the identical
/// payload replays the stored first response; a different payload under the
/// same id is REQUEST_CONFLICT with no side effects.
pub fn apply(
    conn: &mut Connection,
    t: &ApplyMissionTransition,
) -> MissionStoreResult<AppliedTransition> {
    if t.fingerprint.len() != 64 || !t.fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(MissionStoreError::InvalidArgument(
            "fingerprint must be 64 hex characters".into(),
        ));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // 1. Request dedupe (01 §2 step 2) — before any existence/revision check.
    if let Some(stored) = tx
        .query_row(
            "SELECT fingerprint, response_json FROM orch_requests WHERE id = ?1",
            params![t.request_id.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if stored.0 != t.fingerprint {
            return Err(MissionStoreError::RequestConflict(t.request_id.clone()));
        }
        let result: MutationResult = serde_json::from_str(&stored.1)
            .map_err(|e| MissionStoreError::Corrupt(format!("stored request response: {e}")))?;
        // Duplicate replay must not touch any row.
        tx.commit()?;
        return Ok(AppliedTransition {
            result,
            replayed: true,
        });
    }

    // 2. Mission existence + expected_revision CAS.
    let current_revision: Option<i64> = tx
        .query_row(
            "SELECT revision FROM orch_missions WHERE id = ?1",
            params![t.mission_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let (current, new_revision) = match (&t.mode, current_revision) {
        (super::types::ApplyMode::Create, None) => (0u64, 1u64),
        (super::types::ApplyMode::Mutate { expected_revision }, Some(stored)) => {
            let stored = stored as u64;
            if stored != *expected_revision {
                return Err(MissionStoreError::RevisionConflict {
                    expected_revision: *expected_revision,
                    current_revision: stored,
                });
            }
            (stored, stored + 1)
        }
        (super::types::ApplyMode::Create, Some(_)) => {
            return Err(MissionStoreError::InvalidState(format!(
                "mission {} already exists",
                t.mission_id
            )))
        }
        (super::types::ApplyMode::Mutate { .. }, None) => {
            return Err(MissionStoreError::NotFound {
                what: "mission",
                id: t.mission_id.to_string(),
            })
        }
    };
    let _ = current;

    // 3. Validate the upsert set: unique (kind, id), mission-scoped, and a
    //    Mission projection (if any) carries the post-state revision.
    let mut seen: std::collections::HashSet<(EntityKind, String)> =
        std::collections::HashSet::new();
    for entity in &t.upserts {
        if !seen.insert((entity.kind(), entity.entity_id().to_string())) {
            return Err(MissionStoreError::InvalidArgument(format!(
                "duplicate upsert for entity {}",
                entity.entity_id()
            )));
        }
        if let Entity::Mission(m) = entity {
            if m.id != t.mission_id {
                return Err(MissionStoreError::InvalidArgument(
                    "upserted mission does not match transaction mission".into(),
                ));
            }
            if m.revision.get() != new_revision {
                return Err(MissionStoreError::InvalidArgument(format!(
                    "mission projection revision must be {new_revision}"
                )));
            }
        }
    }

    // 4. Derive the event change list (one event per transaction, 01 §2).
    let mut changes: Vec<Change> = t
        .upserts
        .iter()
        .map(|e| Change {
            entity_kind: e.kind(),
            entity_id: e.entity_id(),
            operation: ChangeOperation::Upsert,
        })
        .collect();
    changes.extend(t.deletes.iter().cloned());
    if changes.is_empty() {
        return Err(MissionStoreError::InvalidArgument(
            "mission transaction changes nothing".into(),
        ));
    }

    let event = MissionEvent {
        mission_id: t.mission_id.clone(),
        seq: U64String::new(new_revision).expect("revision within SQLite bound"),
        revision: U64String::new(new_revision).expect("revision within SQLite bound"),
        transaction_id: t.transaction_id.clone(),
        event_type: t.event_type,
        changes: if t.changes_ref.is_some() {
            None
        } else {
            Some(changes.clone())
        },
        changes_ref: t.changes_ref.clone(),
        created_at: t.created_at.clone(),
    };
    let payload = document(&event)?;
    if t.changes_ref.is_none() && payload.len() > MAX_EVENT_BYTES {
        return Err(MissionStoreError::InvalidArgument(format!(
            "event payload {} bytes exceeds the {} byte budget; store a changes artifact and pass changes_ref",
            payload.len(),
            MAX_EVENT_BYTES
        )));
    }

    // 5. Projection writes.
    for entity in &t.upserts {
        upsert_entity(&tx, entity)?;
    }
    for change in &t.deletes {
        delete_entity(&tx, change)?;
    }

    // 5b. Staged-artifact adoption (mission.create): the goal (and any other
    // staged inputs) move into the mission's scope atomically.
    for artifact in &t.adopt_staged_artifacts {
        let updated = tx.execute(
            "UPDATE orch_artifacts SET mission_id = ?2, staging_client_id = NULL, pinned = 1 WHERE id = ?1 AND mission_id IS NULL AND staging_client_id IS NOT NULL",
            params![artifact.as_str(), t.mission_id.as_str()],
        )?;
        if updated != 1 {
            return Err(MissionStoreError::InvalidArgument(format!(
                "staged artifact {artifact} is not adoptable (missing, already adopted, or owned by another client)"
            )));
        }
    }

    // 6. Outbox intents (duplicate dedupe_key with the same id is idempotent).
    for intent in &t.outbox {
        insert_outbox(&tx, intent)?;
    }
    for update in &t.outbox_updates {
        use super::types::OutboxState::*;
        if !matches!(
            (update.expected_state, update.state),
            (Prepared, Sending | Failed)
                | (Sending, Acknowledged | Failed | Unknown)
                | (Unknown, Acknowledged | Failed)
        ) {
            return Err(MissionStoreError::InvalidState(
                "illegal outbox transition".into(),
            ));
        }
        let changed = tx.execute(
            "UPDATE orch_outbox SET state = ?1, updated_at = ?2
             WHERE id = ?3 AND mission_id = ?4 AND state = ?5 AND fencing_token = ?6",
            params![
                update.state.as_str(),
                t.created_at,
                update.id.as_str(),
                t.mission_id.as_str(),
                update.expected_state.as_str(),
                update.fencing_token as i64
            ],
        )?;
        if changed != 1 {
            return Err(MissionStoreError::InvalidState(
                "outbox state or fencing token changed; resync before acting".into(),
            ));
        }
    }

    // 7. Event row. `housekeeping` mirrors the method (retention prune
    //    discriminator; migration 0006 backfilled legacy rows).
    tx.execute(
        "INSERT INTO orch_events (mission_id, seq, revision, transaction_id, event_type, payload_json, created_at, housekeeping)
         VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            t.mission_id.as_str(),
            new_revision as i64,
            t.transaction_id.as_str(),
            serde_json::to_value(t.event_type)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "changed".into()),
            payload,
            t.created_at,
            i64::from(is_housekeeping_method(&t.method)),
        ],
    )?;

    // 8. Request row + response.
    let result = MutationResult {
        mission_id: t.mission_id.clone(),
        revision: U64String::new(new_revision).expect("revision within SQLite bound"),
        event_seq: U64String::new(new_revision).expect("revision within SQLite bound"),
        entity_ids: changes.iter().map(|c| c.entity_id.clone()).collect(),
    };
    tx.execute(
        "INSERT INTO orch_requests (id, mission_id, method, fingerprint, response_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            t.request_id.as_str(),
            t.mission_id.as_str(),
            t.method,
            t.fingerprint,
            document(&result)?,
            t.created_at,
        ],
    )?;

    tx.commit()?;
    Ok(AppliedTransition {
        result,
        replayed: false,
    })
}

fn upsert_entity(tx: &rusqlite::Transaction, entity: &Entity) -> MissionStoreResult<()> {
    match entity {
        Entity::Mission(m) => upsert_mission(tx, m),
        Entity::Task(task) => upsert_task(tx, task),
        Entity::Run(run) => upsert_run(tx, run),
        Entity::Exec(exec) => upsert_exec(tx, exec),
        Entity::Message(value) => {
            upsert_generic(tx, "message", &value.mission_id, &value.id, value)
        }
        Entity::Decision(value) => {
            upsert_generic(tx, "decision", &value.mission_id, &value.id, value)
        }
        Entity::Workspace(value) => {
            upsert_generic(tx, "workspace", &value.mission_id, &value.id, value)
        }
        Entity::Candidate(value) => {
            upsert_generic(tx, "candidate", &value.mission_id, &value.id, value)
        }
        Entity::Verification(value) => {
            upsert_generic(tx, "verification", &value.mission_id, &value.id, value)
        }
        Entity::Finding(value) => {
            upsert_generic(tx, "finding", &value.mission_id, &value.id, value)
        }
        Entity::Knowledge(value) => {
            upsert_generic(tx, "knowledge", &value.mission_id, &value.id, value)
        }
    }
}

fn upsert_mission(tx: &rusqlite::Transaction, m: &Mission) -> MissionStoreResult<()> {
    tx.execute(
        "INSERT INTO orch_missions (id, revision, event_seq, state, document_json, created_at, updated_at, archived_at)
         VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET revision = excluded.revision, event_seq = excluded.event_seq,
           state = excluded.state, document_json = excluded.document_json,
           updated_at = excluded.updated_at, archived_at = excluded.archived_at",
        params![
            m.id.as_str(),
            m.revision.get() as i64,
            state_str(&m.state)?,
            document(m)?,
            m.created_at,
            m.updated_at,
            m.archived_at,
        ],
    )?;
    Ok(())
}

fn upsert_task(tx: &rusqlite::Transaction, task: &Task) -> MissionStoreResult<()> {
    tx.execute(
        "INSERT INTO orch_tasks (id, mission_id, state, ordinal, attempt_count, document_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET state = excluded.state, ordinal = excluded.ordinal,
           attempt_count = excluded.attempt_count, document_json = excluded.document_json",
        params![
            task.id.as_str(),
            task.mission_id.as_str(),
            state_str(&task.state)?,
            task.ordinal as i64,
            task.attempt_count as i64,
            document(task)?,
        ],
    )?;
    tx.execute(
        "DELETE FROM orch_dependencies WHERE task_id = ?1",
        params![task.id.as_str()],
    )?;
    for dep in &task.depends_on {
        tx.execute(
            "INSERT INTO orch_dependencies (mission_id, task_id, dependency_id) VALUES (?1, ?2, ?3)",
            params![task.mission_id.as_str(), task.id.as_str(), dep.as_str()],
        )?;
    }
    Ok(())
}

fn upsert_run(tx: &rusqlite::Transaction, run: &Run) -> MissionStoreResult<()> {
    if run.state.is_live() {
        let existing: Option<String> = tx
            .query_row(
                &format!(
                    "SELECT id FROM orch_runs WHERE task_id = ?1 AND state IN {LIVE_RUN_STATES} AND id != ?2"
                ),
                params![run.task_id.as_str(), run.id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(other) = existing {
            return Err(MissionStoreError::InvalidState(format!(
                "task {} already has a live run {other}",
                run.task_id
            )));
        }
    }
    tx.execute(
        "INSERT INTO orch_runs (id, mission_id, task_id, attempt, state, fencing_token, dispatch_state, document_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET state = excluded.state,
           fencing_token = excluded.fencing_token, dispatch_state = excluded.dispatch_state,
           document_json = excluded.document_json",
        params![
            run.id.as_str(),
            run.mission_id.as_str(),
            run.task_id.as_str(),
            run.attempt as i64,
            state_str(&run.state)?,
            run.fencing_token.get() as i64,
            state_str(&run.dispatch_state)?,
            document(run)?,
        ],
    )?;
    Ok(())
}

fn upsert_exec(tx: &rusqlite::Transaction, exec: &ExecRecord) -> MissionStoreResult<()> {
    tx.execute(
        "INSERT INTO orch_execs (id, mission_id, run_id, state, document_json)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET state = excluded.state, document_json = excluded.document_json",
        params![
            exec.id.as_str(),
            exec.mission_id.as_str(),
            exec.run_id.as_str(),
            state_str(&exec.state)?,
            document(exec)?,
        ],
    )?;
    Ok(())
}

fn upsert_generic<T: serde::Serialize>(
    tx: &rusqlite::Transaction,
    kind: &str,
    mission_id: &term_contracts::mission::types::Id,
    id: &term_contracts::mission::types::Id,
    value: &T,
) -> MissionStoreResult<()> {
    tx.execute(
        "INSERT INTO orch_entities (mission_id, kind, id, document_json) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(mission_id, kind, id) DO UPDATE SET document_json = excluded.document_json",
        params![mission_id.as_str(), kind, id.as_str(), document(value)?],
    )?;
    Ok(())
}

fn delete_entity(tx: &rusqlite::Transaction, change: &Change) -> MissionStoreResult<()> {
    let id = change.entity_id.as_str();
    match change.entity_kind {
        EntityKind::Mission => {
            return Err(MissionStoreError::InvalidArgument(
                "mission rows are never deleted by content expiry".into(),
            ))
        }
        EntityKind::Task => {
            tx.execute(
                "DELETE FROM orch_dependencies WHERE task_id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM orch_tasks WHERE id = ?1", params![id])?;
        }
        EntityKind::Run => {
            tx.execute("DELETE FROM orch_runs WHERE id = ?1", params![id])?;
        }
        EntityKind::Exec => {
            tx.execute("DELETE FROM orch_execs WHERE id = ?1", params![id])?;
        }
        EntityKind::Message
        | EntityKind::Decision
        | EntityKind::Workspace
        | EntityKind::Candidate
        | EntityKind::Verification
        | EntityKind::Finding
        | EntityKind::Knowledge => {
            tx.execute(
                "DELETE FROM orch_entities WHERE kind = ?1 AND id = ?2",
                params![super::types::entity_kind_str(change.entity_kind), id],
            )?;
        }
    }
    Ok(())
}

fn insert_outbox(tx: &rusqlite::Transaction, intent: &OutboxIntent) -> MissionStoreResult<()> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM orch_outbox WHERE dedupe_key = ?1",
            params![intent.dedupe_key],
            |row| row.get(0),
        )
        .optional()?;
    match existing {
        Some(existing_id) if existing_id == intent.id.as_str() => Ok(()), // idempotent replay
        Some(other) => Err(MissionStoreError::InvalidState(format!(
            "outbox dedupe key already owned by {other}"
        ))),
        None => {
            tx.execute(
                "INSERT INTO orch_outbox (id, mission_id, run_id, operation, dedupe_key, fencing_token, state, payload_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'prepared', ?7, ?8, ?8)",
                params![
                    intent.id.as_str(),
                    intent.mission_id.as_str(),
                    intent.run_id.as_ref().map(|id| id.as_str()),
                    intent.operation.as_str(),
                    intent.dedupe_key,
                    intent.fencing_token as i64,
                    serde_json::to_string(&intent.payload)
                        .map_err(|e| MissionStoreError::Corrupt(e.to_string()))?,
                    intent.created_at,
                ],
            )?;
            Ok(())
        }
    }
}

// ---- config-store transactions (bindings / templates / verification /
// repository registry) -----------------------------------------------
// Same dedupe discipline as mission transactions (01 §3: request cache
// without a mission scope), one BEGIN IMMEDIATE per save.

pub type SavedConfigResult = MissionStoreResult<SavedConfig>;

/// A stored config document (orch_bindings row or orch_config row).
#[derive(Debug, Clone, PartialEq)]
pub struct SavedConfig {
    pub document: serde_json::Value,
    pub revision: u64,
    pub replayed: bool,
}

/// Rebuild a replayed save outcome from its stored first response.
fn replay_saved(response: &serde_json::Value, field: &str) -> MissionStoreResult<SavedConfig> {
    let document = response
        .get(field)
        .cloned()
        .ok_or_else(|| MissionStoreError::Corrupt("stored config response shape".into()))?;
    let revision = document
        .get("revision")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    Ok(SavedConfig {
        document,
        revision,
        replayed: true,
    })
}

fn check_config_request(
    tx: &rusqlite::Transaction,
    request_id: &term_contracts::mission::types::Id,
    fingerprint: &str,
) -> MissionStoreResult<Option<serde_json::Value>> {
    let stored: Option<String> = tx
        .query_row(
            "SELECT response_json FROM orch_requests WHERE id = ?1",
            params![request_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    match stored {
        Some(response) => {
            let conflict: Option<String> = tx
                .query_row(
                    "SELECT fingerprint FROM orch_requests WHERE id = ?1",
                    params![request_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            if conflict.as_deref() != Some(fingerprint) {
                return Err(MissionStoreError::RequestConflict(request_id.clone()));
            }
            let value: serde_json::Value = serde_json::from_str(&response)
                .map_err(|e| MissionStoreError::Corrupt(format!("stored config response: {e}")))?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

/// CAS save for `orch_bindings`. `expected_revision == 0` creates; otherwise
/// the stored revision must match. The document's `revision` field is
/// normalized to the post-save revision.
pub fn save_binding(
    conn: &mut Connection,
    request_id: &term_contracts::mission::types::Id,
    method: &str,
    fingerprint: &str,
    expected_revision: u64,
    mut document: serde_json::Value,
    created_at: &str,
) -> MissionStoreResult<SavedConfig> {
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(MissionStoreError::InvalidArgument(
            "fingerprint must be 64 hex characters".into(),
        ));
    }
    let id = document
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| MissionStoreError::InvalidArgument("binding.id missing".into()))?
        .to_string();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(replay) = check_config_request(&tx, request_id, fingerprint)? {
        tx.commit()?;
        return replay_saved(&replay, "binding");
    }
    let stored_revision: Option<i64> = tx
        .query_row(
            "SELECT revision FROM orch_bindings WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?;
    let new_revision = match stored_revision {
        None => {
            if expected_revision != 0 {
                return Err(MissionStoreError::RevisionConflict {
                    expected_revision,
                    current_revision: 0,
                });
            }
            1
        }
        Some(stored) => {
            let stored = stored as u64;
            if stored != expected_revision {
                return Err(MissionStoreError::RevisionConflict {
                    expected_revision,
                    current_revision: stored,
                });
            }
            stored + 1
        }
    };
    if let Some(object) = document.as_object_mut() {
        object.insert(
            "revision".into(),
            serde_json::json!(new_revision.to_string()),
        );
    }
    tx.execute(
        "INSERT INTO orch_bindings (id, revision, document_json) VALUES (?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET revision = excluded.revision,
           document_json = excluded.document_json",
        params![
            id,
            new_revision as i64,
            serde_json::to_string(&document)
                .map_err(|e| MissionStoreError::Corrupt(e.to_string()))?
        ],
    )?;
    let response = serde_json::json!({ "binding": document });
    tx.execute(
        "INSERT INTO orch_requests (id, mission_id, method, fingerprint, response_json, created_at)
         VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
        params![
            request_id.as_str(),
            method,
            fingerprint,
            serde_json::to_string(&response)
                .map_err(|e| MissionStoreError::Corrupt(e.to_string()))?,
            created_at,
        ],
    )?;
    tx.commit()?;
    Ok(SavedConfig {
        document,
        revision: new_revision,
        replayed: false,
    })
}

/// CAS save for `orch_config` rows (kind = template | verification |
/// repository). Same dedupe/CAS rules as bindings.
/// Inputs for [`save_config`] (keeps the argument list under the lint bound).
pub struct SaveConfig<'a> {
    pub request_id: &'a term_contracts::mission::types::Id,
    pub method: &'a str,
    pub fingerprint: &'a str,
    pub kind: &'a str,
    pub expected_revision: u64,
    pub created_at: &'a str,
}

pub fn save_config(
    conn: &mut Connection,
    input: &SaveConfig<'_>,
    mut document: serde_json::Value,
) -> MissionStoreResult<SavedConfig> {
    let SaveConfig {
        request_id,
        method,
        fingerprint,
        kind,
        expected_revision,
        created_at,
    } = *input;
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(MissionStoreError::InvalidArgument(
            "fingerprint must be 64 hex characters".into(),
        ));
    }
    let id = document
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| MissionStoreError::InvalidArgument("config id missing".into()))?
        .to_string();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(replay) = check_config_request(&tx, request_id, fingerprint)? {
        tx.commit()?;
        return replay_saved(&replay, "saved");
    }
    if kind == "repository" {
        let old_common: Option<String> = tx
            .query_row(
                "SELECT json_extract(document_json, '$.common_dir') FROM orch_config
             WHERE kind = 'repository' AND id = ?1",
                [&id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let common = document["common_dir"].as_str();
        if old_common.as_deref().is_some_and(|old| Some(old) != common) {
            return Err(MissionStoreError::InvalidArgument(
                "registered Git common directory is immutable".into(),
            ));
        }
        if let Some(common) = common {
            let duplicate: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM orch_config WHERE kind = 'repository'
                 AND id != ?1 AND json_extract(document_json, '$.common_dir') = ?2)",
                params![id, common],
                |row| row.get(0),
            )?;
            if common.is_empty() || duplicate {
                return Err(MissionStoreError::InvalidArgument(
                    "Git common directory is empty or already registered".into(),
                ));
            }
        }
    }
    let stored_revision: Option<i64> = tx
        .query_row(
            "SELECT revision FROM orch_config WHERE kind = ?1 AND id = ?2",
            params![kind, id],
            |row| row.get(0),
        )
        .optional()?;
    let new_revision = match stored_revision {
        None => {
            if expected_revision != 0 {
                return Err(MissionStoreError::RevisionConflict {
                    expected_revision,
                    current_revision: 0,
                });
            }
            1
        }
        Some(stored) => {
            let stored = stored as u64;
            if stored != expected_revision {
                return Err(MissionStoreError::RevisionConflict {
                    expected_revision,
                    current_revision: stored,
                });
            }
            stored + 1
        }
    };
    if let Some(object) = document.as_object_mut() {
        object.insert(
            "revision".into(),
            serde_json::json!(new_revision.to_string()),
        );
    }
    tx.execute(
        "INSERT INTO orch_config (kind, id, revision, document_json) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(kind, id) DO UPDATE SET revision = excluded.revision,
           document_json = excluded.document_json",
        params![
            kind,
            id,
            new_revision as i64,
            serde_json::to_string(&document)
                .map_err(|e| MissionStoreError::Corrupt(e.to_string()))?
        ],
    )?;
    let response = serde_json::json!({ "saved": document });
    tx.execute(
        "INSERT INTO orch_requests (id, mission_id, method, fingerprint, response_json, created_at)
         VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
        params![
            request_id.as_str(),
            method,
            fingerprint,
            serde_json::to_string(&response)
                .map_err(|e| MissionStoreError::Corrupt(e.to_string()))?,
            created_at,
        ],
    )?;
    tx.commit()?;
    Ok(SavedConfig {
        document,
        revision: new_revision,
        replayed: false,
    })
}

/// Rows removed by one retention pass (events + requests), for logging and
/// the vacuum trigger.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PrunedRows {
    pub housekeeping_events: u64,
    pub requests: u64,
}

/// Transitions that only move elapsed time or activity stamps. Their event
/// rows are pure churn (a stuck, decision-blocked mission wrote one per
/// second); retention deletes them outside each mission's recent tail while
/// every other event is kept forever. The method strings are persisted data
/// (orch_requests.method), so this is the storage-side mirror of the
/// daemon's `mission::timing::is_housekeeping`.
fn is_housekeeping_method(method: &str) -> bool {
    matches!(method, "engine.time_checkpoint" | "engine.activity")
}

/// Retention pass over the orchestration tables (bounded-database fix).
///
/// **Housekeeping events**: rows flagged `housekeeping = 1` (set at insert
/// from the method, backfilled by migration 0006) that sit more than
/// `event_tail_per_mission` revisions behind their mission's head. The tail
/// keeps recent audit rows; every meaningful event is kept forever.
/// Correctness does not depend on old events: the event reader is
/// cursor-based (`seq > after_seq`, so a pruned gap is invisible to a client
/// that already consumed it) and mission state materializes from the head
/// revision plus entity tables, never from event replay.
///
/// **Requests**: the dedupe twin of every transition. Housekeeping-method
/// rows are kept only as the newest `event_tail_per_mission` per mission;
/// meaningful rows survive `request_retention_days` (a replay older than the
/// window just re-executes under the normal CAS rules).
///
/// Both deletions run in batches so the single mission writer connection is
/// never held inside one long transaction; each batch commits on its own.
pub fn prune_retention(
    conn: &mut Connection,
    event_tail_per_mission: i64,
    request_retention_days: u32,
) -> MissionStoreResult<PrunedRows> {
    if event_tail_per_mission < 0 {
        return Err(MissionStoreError::InvalidArgument(
            "event_tail_per_mission must be >= 0".into(),
        ));
    }
    const BATCH: usize = 2000;
    let mut pruned = PrunedRows::default();
    loop {
        let removed = conn.execute(
            "DELETE FROM orch_events WHERE rowid IN (
                 SELECT rowid FROM orch_events AS candidate
                 WHERE candidate.housekeeping = 1
                   AND candidate.seq <= (
                       SELECT MAX(seq) FROM orch_events AS head
                       WHERE head.mission_id = candidate.mission_id
                   ) - ?1
                 LIMIT ?2)",
            params![event_tail_per_mission, BATCH as i64],
        )?;
        pruned.housekeeping_events += removed as u64;
        if removed < BATCH {
            break;
        }
    }
    loop {
        let removed = conn.execute(
            "DELETE FROM orch_requests WHERE rowid IN (
                 SELECT rowid FROM (
                     SELECT rowid,
                            ROW_NUMBER() OVER (
                                PARTITION BY mission_id
                                ORDER BY created_at DESC, id DESC
                            ) AS rn
                     FROM orch_requests
                     WHERE method IN ('engine.time_checkpoint','engine.activity')
                       AND mission_id IS NOT NULL
                 ) WHERE rn > ?1 LIMIT ?2)",
            params![event_tail_per_mission, BATCH as i64],
        )?;
        pruned.requests += removed as u64;
        if removed < BATCH {
            break;
        }
    }
    let cutoff = crate::time::iso8601_days_ago(request_retention_days);
    loop {
        let removed = conn.execute(
            "DELETE FROM orch_requests WHERE rowid IN (
                 SELECT rowid FROM orch_requests
                 WHERE created_at < ?1
                   AND method NOT IN ('engine.time_checkpoint','engine.activity')
                 LIMIT ?2)",
            params![cutoff, BATCH as i64],
        )?;
        pruned.requests += removed as u64;
        if removed < BATCH {
            break;
        }
    }
    Ok(pruned)
}
