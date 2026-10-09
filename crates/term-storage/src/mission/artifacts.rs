//! Artifact row bookkeeping (`orch_artifacts` / `orch_uploads`): upload
//! progression, atomic commit, and reads. File bytes live on disk under the
//! daemon's data root; this module owns only the rows (single writer).

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use term_contracts::mission::types::Id;

use super::types::{MissionStoreError, MissionStoreResult};

#[derive(Debug, Clone, PartialEq)]
pub struct UploadRow {
    pub id: Id,
    pub client_id: String,
    pub mission_id: Option<Id>,
    pub expected_bytes: i64,
    pub next_offset: i64,
    pub expected_sha256: String,
    pub media_type: String,
    pub temp_relative_path: String,
    pub committed_artifact_id: Option<Id>,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactRow {
    pub id: Id,
    pub mission_id: Option<Id>,
    pub staging_client_id: Option<String>,
    pub sha256: String,
    pub bytes: i64,
    pub media_type: String,
    pub relative_path: String,
    pub content_state: String,
    pub pinned: bool,
    pub created_at: String,
}

fn row_to_upload(row: &rusqlite::Row<'_>) -> rusqlite::Result<UploadRow> {
    let mission_id: Option<String> = row.get("mission_id")?;
    let committed: Option<String> = row.get("committed_artifact_id")?;
    Ok(UploadRow {
        id: Id::parse(&row.get::<_, String>("id")?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        client_id: row.get("client_id")?,
        mission_id: mission_id
            .map(|value| Id::parse(&value))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        expected_bytes: row.get("expected_bytes")?,
        next_offset: row.get("next_offset")?,
        expected_sha256: row.get("expected_sha256")?,
        media_type: row.get("media_type")?,
        temp_relative_path: row.get("temp_relative_path")?,
        committed_artifact_id: committed
            .map(|value| Id::parse(&value))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        expires_at: row.get("expires_at")?,
    })
}

fn row_to_artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactRow> {
    let mission_id: Option<String> = row.get("mission_id")?;
    let staging: Option<String> = row.get("staging_client_id")?;
    Ok(ArtifactRow {
        id: Id::parse(&row.get::<_, String>("id")?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?,
        mission_id: mission_id
            .map(|value| Id::parse(&value))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        staging_client_id: staging,
        sha256: row.get("sha256")?,
        bytes: row.get("bytes")?,
        media_type: row.get("media_type")?,
        relative_path: row.get("relative_path")?,
        content_state: row.get("content_state")?,
        pinned: row.get::<_, i64>("pinned")? != 0,
        created_at: row.get("created_at")?,
    })
}

pub fn insert_upload(conn: &mut Connection, upload: &UploadRow) -> MissionStoreResult<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO orch_uploads (id, client_id, mission_id, expected_bytes, next_offset,
            expected_sha256, media_type, temp_relative_path, committed_artifact_id, expires_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, NULL, ?8)",
        params![
            upload.id.as_str(),
            upload.client_id,
            upload.mission_id.as_ref().map(|id| id.as_str()),
            upload.expected_bytes,
            upload.expected_sha256,
            upload.media_type,
            upload.temp_relative_path,
            upload.expires_at,
        ],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(ffi, _)
            if matches!(
                ffi.extended_code,
                rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                    | rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            ) =>
        {
            MissionStoreError::InvalidState(format!("upload {} already exists", upload.id))
        }
        other => MissionStoreError::Sqlite(other),
    })?;
    tx.commit()?;
    Ok(())
}

pub fn get_upload(conn: &Connection, id: &Id) -> MissionStoreResult<Option<UploadRow>> {
    let row = conn
        .query_row(
            "SELECT id, client_id, mission_id, expected_bytes, next_offset, expected_sha256,
                media_type, temp_relative_path, committed_artifact_id, expires_at
             FROM orch_uploads WHERE id = ?1",
            params![id.as_str()],
            row_to_upload,
        )
        .optional()?;
    Ok(row)
}

/// Advance the append cursor. The caller verified the bytes; this only
/// records progression (offset CAS inside the single writer).
pub fn advance_upload(
    conn: &mut Connection,
    id: &Id,
    from_offset: i64,
    to_offset: i64,
) -> MissionStoreResult<()> {
    let updated = conn.execute(
        "UPDATE orch_uploads SET next_offset = ?3 WHERE id = ?1 AND next_offset = ?2",
        params![id.as_str(), from_offset, to_offset],
    )?;
    if updated != 1 {
        return Err(MissionStoreError::InvalidState(format!(
            "upload {id} cursor moved (expected next_offset {from_offset})"
        )));
    }
    Ok(())
}

/// Atomically register the committed artifact and link the upload (repeat
/// commits return the stored artifact id — idempotent).
pub fn commit_upload(
    conn: &mut Connection,
    upload_id: &Id,
    artifact: &ArtifactRow,
) -> MissionStoreResult<ArtifactRow> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let committed: Option<String> = tx
        .query_row(
            "SELECT committed_artifact_id FROM orch_uploads WHERE id = ?1",
            params![upload_id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if let Some(existing) = committed {
        let row = tx
            .query_row(
                "SELECT id, mission_id, staging_client_id, sha256, bytes, media_type,
                    relative_path, content_state, pinned, created_at
                 FROM orch_artifacts WHERE id = ?1",
                params![existing],
                row_to_artifact,
            )
            .optional()?;
        tx.commit()?;
        return row.ok_or_else(|| {
            MissionStoreError::Corrupt(format!(
                "upload {upload_id} points at missing artifact {existing}"
            ))
        });
    }
    tx.execute(
        "INSERT INTO orch_artifacts (id, mission_id, staging_client_id, sha256, bytes,
            media_type, relative_path, content_state, pinned, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            artifact.id.as_str(),
            artifact.mission_id.as_ref().map(|id| id.as_str()),
            artifact.staging_client_id,
            artifact.sha256,
            artifact.bytes,
            artifact.media_type,
            artifact.relative_path,
            artifact.content_state,
            if artifact.pinned { 1 } else { 0 },
            artifact.created_at,
        ],
    )?;
    tx.execute(
        "UPDATE orch_uploads SET committed_artifact_id = ?2 WHERE id = ?1",
        params![upload_id.as_str(), artifact.id.as_str()],
    )?;
    tx.commit()?;
    Ok(artifact.clone())
}

pub fn get_artifact(conn: &Connection, id: &Id) -> MissionStoreResult<Option<ArtifactRow>> {
    let row = conn
        .query_row(
            "SELECT id, mission_id, staging_client_id, sha256, bytes, media_type,
                relative_path, content_state, pinned, created_at
             FROM orch_artifacts WHERE id = ?1",
            params![id.as_str()],
            row_to_artifact,
        )
        .optional()?;
    Ok(row)
}

/// Expired uncommitted uploads (1 h) for the sweep. Returns temp paths.
pub fn expired_uploads(conn: &Connection, now_iso: &str) -> MissionStoreResult<Vec<(Id, String)>> {
    let mut statement = conn.prepare(
        "SELECT id, temp_relative_path FROM orch_uploads
         WHERE committed_artifact_id IS NULL AND expires_at < ?1",
    )?;
    let rows = statement
        .query_map(params![now_iso], |row| {
            Ok((
                Id::parse(&row.get::<_, String>(0)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                row.get::<_, String>(1)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Drop an upload row after its temp file was removed.
pub fn drop_upload(conn: &mut Connection, id: &Id) -> MissionStoreResult<()> {
    conn.execute(
        "DELETE FROM orch_uploads WHERE id = ?1",
        params![id.as_str()],
    )?;
    Ok(())
}

/// Writer-side upload operations (one command enum keeps the channel small).
pub enum UploadOp {
    Insert(UploadRow),
    Advance {
        id: Id,
        from_offset: i64,
        to_offset: i64,
    },
    Drop(Id),
}

pub fn apply_upload_op(conn: &mut Connection, op: &UploadOp) -> MissionStoreResult<()> {
    match op {
        UploadOp::Insert(upload) => insert_upload(conn, upload),
        UploadOp::Advance {
            id,
            from_offset,
            to_offset,
        } => advance_upload(conn, id, *from_offset, *to_offset),
        UploadOp::Drop(id) => drop_upload(conn, id),
    }
}
