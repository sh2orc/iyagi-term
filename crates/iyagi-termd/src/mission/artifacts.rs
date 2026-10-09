//! Artifact byte store (ticket O05): begin/write/commit chunked uploads
//! with offset/hash verification, staging scoped to the creating client,
//! mission adoption inside `mission.create`, and bounded reads.
//!
//! Layout (04 §1): `<data>/missions/artifacts/<id-prefix>/<artifact-id>` for
//! bodies and `.../uploads/<upload-id>.part` for in-flight temp files. Body
//! paths carry only daemon-generated ids — never user text (04 §1). File
//! permissions are tightened on unix (0600); Windows relies on the profile
//! directory ACL.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine;
use sha2::{Digest, Sha256};
use term_contracts::ids::ConnectionId;
use term_contracts::mission::error::MissionErrorCode;
use term_contracts::mission::rpc::{
    ArtifactBeginParams, ArtifactCommitParams, ArtifactReadParams, ArtifactReadResult,
    ArtifactWriteParams,
};
use term_contracts::mission::types::{ArtifactRef, Id, Timestamp};
use term_contracts::mission::validation::{validate_sha256, MissionLimits};
use term_storage::mission::artifacts::{ArtifactRow, UploadRow};
use term_storage::mission::types::MissionStoreError;
use term_storage::Storage;

/// defaults.json artifact knobs.
const MAX_ARTIFACT_BYTES: i64 = 64 * 1024 * 1024;

/// Entry bound for the display-window reuse map. Reconnects and concurrent
/// viewers replay recent windows; a full map resets wholesale, and a miss
/// only costs one fresh window publish.
const ACTIVITY_WINDOW_CACHE_ENTRIES: usize = 1024;

pub struct ArtifactStore {
    storage: Arc<Storage>,
    root: PathBuf,
    limits: MissionLimits,
    now: Box<dyn Fn() -> String + Send + Sync>,
    /// Display-window reuse for `mission.activity` reads (audit F2):
    /// repeated polls of the same (mission, run, byte window) share one
    /// committed artifact instead of minting file+row+fsyncs per poll.
    activity_windows: Mutex<HashMap<(Id, Id, u64, u64), ArtifactRef>>,
}

type ArtifactError = (MissionErrorCode, String);

impl ArtifactStore {
    pub fn new(storage: Arc<Storage>, root: PathBuf) -> Self {
        ArtifactStore {
            storage,
            root,
            limits: MissionLimits::load(),
            now: Box::new(term_storage::time::now_iso8601),
            activity_windows: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn activity_path(&self, mission: &Id, run: &Id) -> PathBuf {
        self.root
            .join(mission.as_str())
            .join("activity")
            .join(format!("{run}.json"))
    }

    fn artifacts_root(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    fn uploads_root(&self) -> PathBuf {
        self.artifacts_root().join("uploads")
    }

    /// `artifacts/<id-prefix>/<id>` — prefix sharding keeps directories small.
    pub(crate) fn body_path(&self, id: &Id) -> PathBuf {
        let hex = id.as_str();
        self.artifacts_root().join(&hex[..2]).join(hex)
    }

    /// begin: validate the announcement, sweep expired uploads, open a temp
    /// file. `mission_id: null` stages for `mission.create` (01 §3).
    pub fn begin(
        &self,
        conn: &ConnectionId,
        params: &ArtifactBeginParams,
    ) -> Result<(Id, u32), String> {
        self.begin_checked(conn, params)
            .map_err(|(_, message)| message)
    }

    /// The helper must also verify this artifact's hash after opening it.
    pub(super) fn execution_body_path(
        &self,
        mission_id: &Id,
        reference: &ArtifactRef,
        max_bytes: usize,
    ) -> Result<PathBuf, ArtifactError> {
        self.read_mission_body(mission_id, reference, max_bytes)?;
        self.body_path(&reference.id).canonicalize().map_err(|_| {
            (
                MissionErrorCode::IntegrityFailed,
                "execution input artifact is unavailable".into(),
            )
        })
    }

    pub(super) fn begin_checked(
        &self,
        conn: &ConnectionId,
        params: &ArtifactBeginParams,
    ) -> Result<(Id, u32), ArtifactError> {
        validate_sha256(&params.sha256)
            .map_err(|e| (MissionErrorCode::InvalidArgument, e.message))?;
        if params.media_type.is_empty()
            || params.media_type.len() > 256
            || params.media_type.chars().any(|c| c.is_control())
        {
            return Err((
                MissionErrorCode::InvalidArgument,
                "media_type must be 1..=256 printable characters".into(),
            ));
        }
        let bytes = params.bytes.get() as i64;
        if bytes > MAX_ARTIFACT_BYTES {
            return Err((
                MissionErrorCode::InvalidArgument,
                "bytes exceed the artifact limit".into(),
            ));
        }
        self.sweep_expired_uploads();

        let upload_id = Id::generate();
        let temp_relative = format!("uploads/{}.part", upload_id);
        let temp_path = self.artifacts_root().join(&temp_relative);
        if let Some(parent) = temp_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                (
                    MissionErrorCode::StorageUnavailable,
                    format!("upload dir: {e}"),
                )
            })?;
        }
        std::fs::File::create(&temp_path).map_err(|e| {
            (
                MissionErrorCode::StorageUnavailable,
                format!("upload temp file: {e}"),
            )
        })?;
        restrict_permissions(&temp_path);

        // Expiry is a fixed 1 h from begin (defaults.json upload_expiry_ms).
        let expires_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            + 3600;
        let expires_at = term_storage::time::iso8601_from_unix(expires_epoch, 0);
        let row = UploadRow {
            id: upload_id.clone(),
            client_id: conn.to_string(),
            mission_id: params.mission_id.clone(),
            expected_bytes: bytes,
            next_offset: 0,
            expected_sha256: params.sha256.clone(),
            media_type: params.media_type.clone(),
            temp_relative_path: temp_relative,
            committed_artifact_id: None,
            expires_at,
        };
        if let Err(error) = self.storage.mission_upload_insert(row) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(store_artifact_error(error));
        }
        Ok((upload_id, self.limits.artifact_chunk_bytes as u32))
    }

    /// write: 4 KiB raw chunks as base64; only the exact next offset appends.
    /// A repeated offset must repeat identical bytes (01 §3).
    pub fn write(&self, params: &ArtifactWriteParams) -> Result<u64, ArtifactError> {
        let upload = self
            .storage
            .mission_upload(&params.upload_id)
            .map_err(store_artifact_error)?
            .ok_or_else(|| {
                (
                    MissionErrorCode::NotFound,
                    format!("upload {} not found", params.upload_id),
                )
            })?;
        let offset = params.offset.get() as i64;
        let data = base64::engine::general_purpose::STANDARD
            .decode(params.data_b64.as_bytes())
            .map_err(|e| {
                (
                    MissionErrorCode::InvalidArgument,
                    format!("data_b64 is not valid base64: {e}"),
                )
            })?;
        if data.len() as u64 > self.limits.artifact_chunk_bytes as u64 * 2 {
            return Err((
                MissionErrorCode::InvalidArgument,
                format!("chunk exceeds {} bytes", self.limits.artifact_chunk_bytes),
            ));
        }
        let temp_path = self.artifacts_root().join(&upload.temp_relative_path);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(&temp_path)
            .map_err(|e| (MissionErrorCode::Internal, format!("upload file: {e}")))?;
        let file_len = file
            .metadata()
            .map(|meta| meta.len() as i64)
            .map_err(|e| (MissionErrorCode::Internal, format!("upload file: {e}")))?;
        if offset > upload.next_offset {
            return Err((
                MissionErrorCode::InvalidArgument,
                format!(
                    "offset {} skips the append cursor at {}",
                    offset, upload.next_offset
                ),
            ));
        }
        if offset < upload.next_offset {
            // Replay of an earlier chunk: identical bytes is a no-op,
            // different bytes is a conflict.
            let start = offset;
            let end = offset + data.len() as i64;
            if end > upload.next_offset || end > file_len {
                return Err((
                    MissionErrorCode::RequestConflict,
                    "replayed chunk exceeds the committed range".into(),
                ));
            }
            if end > start {
                let mut existing = vec![0u8; (end - start) as usize];
                file.seek(SeekFrom::Start(start as u64))
                    .and_then(|_| file.read_exact(&mut existing))
                    .map_err(|e| (MissionErrorCode::Internal, format!("upload file: {e}")))?;
                if existing != data[..existing.len()] {
                    return Err((
                        MissionErrorCode::RequestConflict,
                        "replayed chunk carries different bytes at the same offset".into(),
                    ));
                }
            }
            return Ok(upload.next_offset as u64);
        }
        let new_offset = upload.next_offset + data.len() as i64;
        if file_len < upload.next_offset || file_len > new_offset {
            return Err((
                MissionErrorCode::Internal,
                format!(
                    "upload file length {file_len} disagrees with cursor {}",
                    upload.next_offset
                ),
            ));
        }
        if upload.next_offset + data.len() as i64 > upload.expected_bytes {
            return Err((
                MissionErrorCode::InvalidArgument,
                format!(
                    "chunk would exceed the announced {} bytes",
                    upload.expected_bytes
                ),
            ));
        }
        // A durable file append may precede a failed cursor transaction.
        // Verify its exact prefix, then append only bytes not already stored.
        let retained = (file_len - upload.next_offset) as usize;
        if retained > 0 {
            let mut existing = vec![0; retained];
            file.seek(SeekFrom::Start(upload.next_offset as u64))
                .and_then(|_| file.read_exact(&mut existing))
                .map_err(|e| (MissionErrorCode::Internal, format!("upload replay: {e}")))?;
            if existing != data[..retained] {
                return Err((
                    MissionErrorCode::RequestConflict,
                    "uncommitted chunk carries different bytes at the same offset".into(),
                ));
            }
        }
        file.write_all(&data[retained..])
            .and_then(|_| file.sync_data())
            .map_err(|e| (MissionErrorCode::Internal, format!("upload append: {e}")))?;
        self.storage
            .mission_upload_advance(params.upload_id.clone(), upload.next_offset, new_offset)
            .map_err(store_artifact_error)?;
        Ok(new_offset as u64)
    }

    /// commit: verify length + SHA-256, fsync, rename into place, register
    /// the artifact row atomically. A stable body path also permits retry
    /// after publication succeeds but the database commit fails.
    pub fn commit(&self, params: &ArtifactCommitParams) -> Result<ArtifactRef, ArtifactError> {
        let upload = self
            .storage
            .mission_upload(&params.upload_id)
            .map_err(store_artifact_error)?
            .ok_or_else(|| {
                (
                    MissionErrorCode::NotFound,
                    format!("upload {} not found", params.upload_id),
                )
            })?;
        if let Some(committed) = upload.committed_artifact_id {
            return self.artifact_ref(&committed);
        }
        let temp_path = self.artifacts_root().join(&upload.temp_relative_path);
        // The upload UUID is also its eventual artifact UUID. If publication
        // survives a failed DB commit or daemon crash, retry finds these exact
        // bytes and verifies them again before registering the row.
        let artifact_id = upload.id.clone();
        let body = self.body_path(&artifact_id);
        let mut published = false;
        // Open with write access: Windows refuses FlushFileBuffers (fsync)
        // on read-only handles, and the contract fsyncs before rename (01 §3).
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temp_path)
            .or_else(|error| {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error);
                }
                published = true;
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&body)
            })
            .map_err(|e| (MissionErrorCode::Internal, format!("upload file: {e}")))?;
        let file_len = file
            .metadata()
            .map(|meta| meta.len() as i64)
            .map_err(|e| (MissionErrorCode::Internal, format!("upload file: {e}")))?;
        if file_len != upload.expected_bytes {
            return Err((
                MissionErrorCode::InvalidArgument,
                format!(
                    "length {file_len} does not match the announced {} bytes",
                    upload.expected_bytes
                ),
            ));
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|e| (MissionErrorCode::Internal, format!("hash read: {e}")))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let digest: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if digest != upload.expected_sha256 {
            return Err((
                MissionErrorCode::IntegrityFailed,
                "sha256 mismatch; the transfer was corrupted — restart the upload".into(),
            ));
        }
        // fsync then atomic rename into the sharded body path.
        file.sync_all()
            .map_err(|e| (MissionErrorCode::Internal, format!("fsync: {e}")))?;
        drop(file);
        if let Some(parent) = body.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| (MissionErrorCode::Internal, format!("body dir: {e}")))?;
        }
        if !published {
            std::fs::rename(&temp_path, &body)
                .map_err(|e| (MissionErrorCode::Internal, format!("publish: {e}")))?;
        }
        restrict_permissions(&body);
        #[cfg(unix)]
        std::fs::File::open(body.parent().expect("sharded artifact path"))
            .and_then(|directory| directory.sync_all())
            .map_err(|e| {
                (
                    MissionErrorCode::StorageUnavailable,
                    format!("publish sync: {e}"),
                )
            })?;
        let staging_client = if upload.mission_id.is_none() {
            Some(upload.client_id.clone())
        } else {
            None
        };
        let row = ArtifactRow {
            id: artifact_id,
            mission_id: upload.mission_id.clone(),
            staging_client_id: staging_client,
            sha256: upload.expected_sha256.clone(),
            bytes: upload.expected_bytes,
            media_type: upload.media_type.clone(),
            relative_path: body
                .strip_prefix(&self.root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default(),
            content_state: "available".into(),
            pinned: false,
            created_at: (self.now)(),
        };
        let stored = self
            .storage
            .mission_artifact_commit(params.upload_id.clone(), row)
            .map_err(store_artifact_error)?;
        Ok(artifact_reference(&stored))
    }

    /// One-shot publish for daemon-generated display bodies (audit F2):
    /// writes the bytes into the staging path directly instead of driving
    /// the resumable per-chunk upload protocol — a 64 KiB page would cost
    /// ~16 chunk fsyncs plus one cursor transaction each — and then reuses
    /// `commit`'s hash verification, fsync, atomic rename, and row
    /// registration unchanged.
    pub(super) fn publish_display_body(
        &self,
        mission_id: &Id,
        body: &[u8],
    ) -> Result<ArtifactRef, ArtifactError> {
        let upload_id = Id::generate();
        let temp_relative = format!("uploads/{upload_id}.part");
        let temp_path = self.artifacts_root().join(&temp_relative);
        if let Some(parent) = temp_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                (
                    MissionErrorCode::StorageUnavailable,
                    format!("upload dir: {e}"),
                )
            })?;
        }
        let mut digest = Sha256::new();
        digest.update(body);
        let sha256: String = digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut file = std::fs::File::create(&temp_path).map_err(|e| {
            (
                MissionErrorCode::StorageUnavailable,
                format!("upload temp file: {e}"),
            )
        })?;
        restrict_permissions(&temp_path);
        file.write_all(body)
            .and_then(|_| file.sync_all())
            .map_err(|e| {
                (
                    MissionErrorCode::Internal,
                    format!("display body write: {e}"),
                )
            })?;
        drop(file);
        // Same 1 h staging expiry convention as `begin_checked`; a failed
        // publish leaves the row for sweep_expired_uploads.
        let expires_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            + 3600;
        let row = UploadRow {
            id: upload_id.clone(),
            // Engine-owned write: the synthetic label only names the upload row.
            client_id: "daemon.display".into(),
            mission_id: Some(mission_id.clone()),
            expected_bytes: body.len() as i64,
            next_offset: body.len() as i64,
            expected_sha256: sha256,
            media_type: "text/plain".into(),
            temp_relative_path: temp_relative,
            committed_artifact_id: None,
            expires_at: term_storage::time::iso8601_from_unix(expires_epoch, 0),
        };
        if let Err(error) = self.storage.mission_upload_insert(row) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(store_artifact_error(error));
        }
        self.commit(&ArtifactCommitParams { upload_id })
    }

    /// Serve one `mission.activity` display window (audit F2). Absolute
    /// stream offsets are stable — the tail only drops bytes from the front
    /// and offsets ride the drain — so the same (start, end) window always
    /// carries the same bytes while it remains reachable, and a repeated
    /// request reuses its committed artifact instead of minting a new
    /// file+row+fsync set. The map is process-local and bounded; a cold
    /// miss simply publishes a fresh window.
    pub(super) fn reuse_activity_window(
        &self,
        mission_id: &Id,
        run_id: &Id,
        start: u64,
        end: u64,
        body: &[u8],
    ) -> Result<ArtifactRef, ArtifactError> {
        let key = (mission_id.clone(), run_id.clone(), start, end);
        if let Some(reference) = self
            .activity_windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
            .cloned()
        {
            return Ok(reference);
        }
        let reference = self.publish_display_body(mission_id, body)?;
        let mut windows = self
            .activity_windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if windows.len() >= ACTIVITY_WINDOW_CACHE_ENTRIES {
            windows.clear();
        }
        windows.insert(key, reference.clone());
        Ok(reference)
    }

    /// read: caller-scope check happens in the engine layer; here we serve
    /// bounded slices of available bodies (01 §3: max 4 KiB).
    pub fn read(&self, params: &ArtifactReadParams) -> Result<ArtifactReadResult, ArtifactError> {
        let row = self
            .storage
            .mission_artifact(&params.artifact_id)
            .map_err(store_artifact_error)?
            .ok_or_else(|| {
                (
                    MissionErrorCode::NotFound,
                    format!("artifact {} not found", params.artifact_id),
                )
            })?;
        match row.content_state.as_str() {
            "available" => {}
            "expired" => {
                return Err((
                    MissionErrorCode::ContentExpired,
                    "artifact body expired; metadata and hash remain".into(),
                ))
            }
            other => {
                return Err((
                    MissionErrorCode::Internal,
                    format!("artifact body state {other:?}"),
                ))
            }
        }
        let path = self.root.join(&row.relative_path);
        let mut file = std::fs::File::open(&path)
            .map_err(|e| (MissionErrorCode::Internal, format!("body open: {e}")))?;
        let offset = params.offset.get();
        if offset > row.bytes as u64 {
            return Err((
                MissionErrorCode::InvalidArgument,
                format!("offset {offset} is past the {}-byte body", row.bytes),
            ));
        }
        let wanted = (params.max_bytes as u64).min(row.bytes as u64 - offset);
        let wanted = wanted.min(4096);
        let mut buffer = vec![0u8; wanted as usize];
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut buffer))
            .map_err(|e| (MissionErrorCode::Internal, format!("body read: {e}")))?;
        let next_offset = offset + buffer.len() as u64;
        Ok(ArtifactReadResult {
            data_b64: base64::engine::general_purpose::STANDARD.encode(&buffer),
            next_offset: term_contracts::ids::U64String::new(next_offset)
                .expect("fits SQLite bound"),
            complete: next_offset >= row.bytes as u64,
        })
    }

    /// mission.create adoption precondition: the staged artifact belongs to
    /// this connection and is still unadopted.
    pub fn adopt_check(&self, conn: &ConnectionId, artifact_id: &Id) -> Result<(), String> {
        let row = self
            .storage
            .mission_artifact(artifact_id)
            .map_err(|e| format!("artifact lookup: {e}"))?
            .ok_or_else(|| format!("staged artifact {artifact_id} not found"))?;
        match &row.mission_id {
            None => {}
            Some(_) => {
                return Err(format!(
                    "artifact {artifact_id} already belongs to a mission"
                ))
            }
        }
        if row.staging_client_id.as_deref() != Some(conn.to_string().as_str()) {
            return Err(format!(
                "staged artifact {artifact_id} belongs to another client"
            ));
        }
        Ok(())
    }

    /// References are claims from the caller; compare every field to the
    /// committed body metadata before using them as a task's contract.
    pub fn validate_reference(&self, reference: &ArtifactRef) -> Result<(), ArtifactError> {
        let stored = self.artifact_ref(&reference.id)?;
        if &stored != reference {
            return Err((
                MissionErrorCode::IntegrityFailed,
                "artifact reference does not match its committed content".into(),
            ));
        }
        let row = self
            .storage
            .mission_artifact(&reference.id)
            .map_err(store_artifact_error)?
            .ok_or_else(|| (MissionErrorCode::NotFound, "artifact missing".into()))?;
        if row.content_state != "available" {
            return Err((
                MissionErrorCode::ContentExpired,
                "artifact content is unavailable".into(),
            ));
        }
        Ok(())
    }

    /// Engine input: only read content belonging to this mission, with a
    /// caller-selected byte budget and a fresh hash check. Context builders
    /// and plan/result readers must not trust provider-supplied references.
    pub fn read_mission_body(
        &self,
        mission_id: &Id,
        reference: &ArtifactRef,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ArtifactError> {
        self.validate_reference(reference)?;
        let row = self
            .storage
            .mission_artifact(&reference.id)
            .map_err(store_artifact_error)?
            .ok_or_else(|| (MissionErrorCode::NotFound, "artifact missing".into()))?;
        if row.mission_id.as_ref() != Some(mission_id) {
            return Err((
                MissionErrorCode::PolicyDenied,
                "artifact belongs to another mission or is still staged".into(),
            ));
        }
        if reference.bytes.get() > max_bytes as u64 {
            return Err((
                MissionErrorCode::ContextTooLarge,
                "artifact exceeds the input byte budget".into(),
            ));
        }
        let file = std::fs::File::open(self.body_path(&reference.id)).map_err(|error| {
            (
                MissionErrorCode::IntegrityFailed,
                format!("artifact body unavailable: {error}"),
            )
        })?;
        let mut body = Vec::new();
        file.take((max_bytes as u64).saturating_add(1))
            .read_to_end(&mut body)
            .map_err(|error| {
                (
                    MissionErrorCode::IntegrityFailed,
                    format!("artifact read failed: {error}"),
                )
            })?;
        let hash = format!("{:x}", Sha256::digest(&body));
        if body.len() as u64 != reference.bytes.get() || hash != reference.sha256 {
            return Err((
                MissionErrorCode::IntegrityFailed,
                "artifact content no longer matches its committed hash and length".into(),
            ));
        }
        Ok(body)
    }

    fn artifact_ref(&self, id: &Id) -> Result<ArtifactRef, ArtifactError> {
        let row = self
            .storage
            .mission_artifact(id)
            .map_err(store_artifact_error)?
            .ok_or_else(|| {
                (
                    MissionErrorCode::Internal,
                    format!("committed artifact {id} missing"),
                )
            })?;
        Ok(artifact_reference(&row))
    }

    /// Remove uncommitted uploads older than the 1 h window (01 §3). Only
    /// paths the rows themselves name are touched.
    fn sweep_expired_uploads(&self) {
        let now = (self.now)();
        let Ok(expired) = self.storage.mission_expired_uploads(&now) else {
            return;
        };
        for (id, temp_relative) in expired {
            let path = self.artifacts_root().join(&temp_relative);
            if path.starts_with(self.uploads_root()) {
                let _ = std::fs::remove_file(&path);
            }
            let _ = self.storage.mission_upload_drop(id);
        }
    }
}

fn artifact_reference(row: &ArtifactRow) -> ArtifactRef {
    ArtifactRef {
        id: row.id.clone(),
        sha256: row.sha256.clone(),
        bytes: term_contracts::ids::U64String::new(row.bytes.max(0) as u64)
            .expect("fits SQLite bound"),
        media_type: row.media_type.clone(),
    }
}

fn store_artifact_error(error: MissionStoreError) -> ArtifactError {
    let code = match &error {
        MissionStoreError::InvalidState(_) => MissionErrorCode::InvalidState,
        MissionStoreError::InvalidArgument(_) => MissionErrorCode::InvalidArgument,
        MissionStoreError::NotFound { .. } => MissionErrorCode::NotFound,
        _ => MissionErrorCode::StorageUnavailable,
    };
    (code, error.to_string())
}

/// Private file mode on unix (04 §1); Windows profile ACLs already restrict.
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

/// Timestamp alias keeps the wire docs honest (unused today beyond rows).
#[allow(dead_code)]
type UnusedTimestamp = Timestamp;
