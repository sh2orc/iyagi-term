//! Mission snapshot pagination with the per-connection cache (01 §4):
//! at most 2 snapshots per connection, 8 MiB each, 60 s TTL; pages of ≤50
//! entities that must fit one frame; cursors bind to their snapshot.

use std::collections::HashMap;
use std::sync::Mutex;

use term_contracts::ids::{ConnectionId, U64String};
use term_contracts::mission::rpc::MissionSnapshotParams;
use term_contracts::mission::types::{Entity, Id, SnapshotPage};
use term_contracts::rpc::MAX_FRAME_BYTES;
use term_storage::mission::types::{MissionSnapshotData, MissionStoreError, MissionStoreResult};
use term_storage::Storage;

/// defaults.json snapshot budget knobs.
const MAX_PER_CONNECTION: usize = 2;
const TTL_MS: u128 = 60_000;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const PAGE_ITEMS: usize = 50;
/// Envelope + page wrapper slack reserved under the frame cap.
const ENVELOPE_RESERVE: usize = 512;

struct CachedSnapshot {
    snapshot_id: Id,
    mission_id: Id,
    entities: Vec<Entity>,
    at_seq: u64,
    revision: u64,
    created_at: std::time::Instant,
}

#[derive(Default)]
pub struct SnapshotCache {
    per_connection: Mutex<HashMap<ConnectionId, Vec<CachedSnapshot>>>,
}

impl SnapshotCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connection close releases its cached snapshots (01 §4).
    pub fn drop_connection(&self, conn: &ConnectionId) {
        self.per_connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(conn);
    }

    pub fn page(
        &self,
        storage: &Storage,
        conn: &ConnectionId,
        params: &MissionSnapshotParams,
    ) -> MissionStoreResult<SnapshotPage> {
        let now = std::time::Instant::now();
        if let Some(snapshot_id) = &params.snapshot_id {
            return match self.page_from_cache(conn, snapshot_id, params, now) {
                Ok(page) => Ok(page),
                Err(error) => Err(error),
            };
        }
        if params.cursor.is_some() {
            return Err(MissionStoreError::InvalidArgument(
                "a cursor requires its snapshot_id".into(),
            ));
        }
        let materialized =
            storage
                .mission_snapshot(&params.mission_id)?
                .ok_or(MissionStoreError::NotFound {
                    what: "mission",
                    id: params.mission_id.to_string(),
                })?;
        let MissionSnapshotData {
            mission_id,
            revision,
            event_seq,
            entities,
        } = materialized;
        let expires_at = expiry_string(now);
        // Oversized projections are never cached (01 §3: a projection the
        // frame could not paginate must not be stored in the first place);
        // the first page still serves the head honestly.
        let total_bytes: usize = entities.iter().map(encoded_len).sum();
        let cached = CachedSnapshot {
            snapshot_id: Id::generate(),
            mission_id,
            entities,
            at_seq: event_seq,
            revision,
            created_at: now,
        };
        let page = build_page(&cached, 0, &expires_at);
        if total_bytes <= MAX_SNAPSHOT_BYTES {
            let mut guard = self
                .per_connection
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entries = guard.entry(conn.clone()).or_default();
            entries.retain(|entry| now.duration_since(entry.created_at).as_millis() < TTL_MS);
            while entries.len() >= MAX_PER_CONNECTION {
                entries.remove(0);
            }
            entries.push(cached);
        }
        Ok(page)
    }

    fn page_from_cache(
        &self,
        conn: &ConnectionId,
        snapshot_id: &Id,
        params: &MissionSnapshotParams,
        now: std::time::Instant,
    ) -> MissionStoreResult<SnapshotPage> {
        let mut guard = self
            .per_connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(entries) = guard.get_mut(conn) else {
            return Err(expired(snapshot_id));
        };
        entries.retain(|entry| now.duration_since(entry.created_at).as_millis() < TTL_MS);
        let Some(position) = entries
            .iter()
            .position(|entry| &entry.snapshot_id == snapshot_id)
        else {
            return Err(expired(snapshot_id));
        };
        let entry = &entries[position];
        if entry.mission_id != params.mission_id {
            // A cursor/snapshot used against another mission (01 §4).
            return Err(MissionStoreError::InvalidArgument(format!(
                "snapshot {} belongs to another mission",
                snapshot_id
            )));
        }
        let offset: usize = match params.cursor.as_deref() {
            None => 0,
            Some(raw) => raw.parse().map_err(|_| {
                MissionStoreError::InvalidArgument("cursor is not a page index".into())
            })?,
        };
        if offset > entry.entities.len() {
            return Err(MissionStoreError::InvalidArgument(
                "cursor points past the snapshot".into(),
            ));
        }
        let expires_at = expiry_string(entry.created_at);
        Ok(build_page(entry, offset, &expires_at))
    }
}

fn expired(snapshot_id: &Id) -> MissionStoreError {
    MissionStoreError::InvalidState(format!(
        "snapshot {snapshot_id} expired or unknown; request a fresh snapshot (snapshot_id: null)"
    ))
}

fn expiry_string(at: std::time::Instant) -> String {
    // Display-only wall-clock estimate for the client; correctness rides on
    // the daemon-side TTL check above.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let _ = at;
    term_storage::time::iso8601_from_unix(now.as_secs() as i64 + 60, now.subsec_millis())
}

fn encoded_len(entity: &Entity) -> usize {
    serde_json::to_vec(entity)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn build_page(cached: &CachedSnapshot, offset: usize, expires_at: &str) -> SnapshotPage {
    // Fill pages up to PAGE_ITEMS entities while the encoded page fits the
    // frame budget (01 §3: shrink the page rather than fail the snapshot).
    let mut end = offset;
    let mut budget = MAX_FRAME_BYTES.saturating_sub(ENVELOPE_RESERVE);
    while end < cached.entities.len() && end - offset < PAGE_ITEMS {
        let cost = encoded_len(&cached.entities[end]);
        if cost > budget {
            break;
        }
        budget -= cost;
        end += 1;
    }
    let next_cursor = if end < cached.entities.len() {
        Some(end.to_string())
    } else {
        None
    };
    SnapshotPage {
        snapshot_id: cached.snapshot_id.clone(),
        mission_id: cached.mission_id.clone(),
        at_seq: U64String::new(cached.at_seq).expect("fits SQLite bound"),
        revision: U64String::new(cached.revision).expect("fits SQLite bound"),
        entities: cached.entities[offset..end].to_vec(),
        next_cursor,
        expires_at: expires_at.to_string(),
    }
}
