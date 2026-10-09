//! Bounded durable display output, separate from mission result evidence.
use super::{
    service::{Handled, MissionService},
    workflow,
};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use term_contracts::{
    ids::U64String,
    mission::{
        rpc::{MissionActivityParams, MissionActivityResult},
        types::*,
        MissionErrorCode, MissionRpcError,
    },
};

const TAIL_BYTES: usize = 1024 * 1024;
#[derive(Default, Serialize, Deserialize)]
struct Tail {
    base: u64,
    body: String,
}
fn io_error(error: impl std::fmt::Display) -> MissionRpcError {
    MissionRpcError::new(
        MissionErrorCode::StorageUnavailable,
        format!("activity storage: {error}"),
    )
}
impl MissionService {
    fn read_activity_tail(&self, mission: &Id, run: &Id) -> Result<Tail, MissionRpcError> {
        let path = self.artifacts.activity_path(mission, run);
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Tail::default()),
            Err(e) => return Err(io_error(e)),
        };
        let mut bytes = Vec::new();
        file.take((TAIL_BYTES * 6 + 1024) as u64)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        let tail: Tail = serde_json::from_slice(&bytes).map_err(io_error)?;
        if tail.body.len() > TAIL_BYTES {
            return Err(io_error("activity tail exceeds its bound"));
        }
        Ok(tail)
    }

    pub(super) fn append_activity(
        &self,
        mission: &Id,
        run: &Id,
        chunk: &str,
    ) -> Result<(), MissionRpcError> {
        // Write-side serialization only: the actor coalesces per-run deltas
        // and flushes them from its single thread, and the temp+rename
        // publish keeps concurrent readers consistent without any lock
        // (audit F1: readers no longer share this mutex).
        let _guard = self
            .activity_guard
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut tail = self.read_activity_tail(mission, run)?;
        let text: String = crate::agent_model::strip_ansi(chunk)
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .collect();
        tail.body.push_str(&text);
        if tail.body.len() > TAIL_BYTES {
            let mut drop = tail.body.len() - TAIL_BYTES;
            while !tail.body.is_char_boundary(drop) {
                drop += 1;
            }
            tail.body.drain(..drop);
            tail.base += drop as u64;
        }
        let path = self.artifacts.activity_path(mission, run);
        let parent = path.parent().expect("activity parent");
        std::fs::create_dir_all(parent).map_err(io_error)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
        serde_json::to_writer(&mut temp, &tail).map_err(io_error)?;
        temp.flush()
            .and_then(|_| temp.as_file().sync_all())
            .map_err(io_error)?;
        temp.persist(path).map_err(io_error)?;
        Ok(())
    }

    /// Fenced application of one (possibly coalesced) display chunk: refresh
    /// `last_activity_at` at most once per second, then append to the run's
    /// durable tail. Split out of `apply_adapter_event` so text deltas can
    /// classify before any mission snapshot materialization (audit F1);
    /// the actor additionally coalesces consecutive deltas and reaches this
    /// once per flush window, while direct callers append synchronously.
    pub(super) fn apply_activity(
        &self,
        mission_id: &Id,
        run_id: &Id,
        fencing_token: u64,
        chunk: &str,
    ) -> Result<bool, MissionRpcError> {
        let snapshot = workflow::load_entities(&self.storage, mission_id)?;
        let Some(run) = snapshot.runs.iter().find(|r| &r.id == run_id) else {
            return Ok(false);
        };
        if run.fencing_token.get() != fencing_token || run.state.is_terminal() {
            return Ok(false);
        }
        if !snapshot.tasks.iter().any(|t| t.id == run.task_id) {
            return Ok(false);
        }
        let timestamp = term_storage::time::now_iso8601();
        // Text deltas do not advance the mission revision per token.
        // One timestamp update per second keeps stale-activity UI useful.
        if run.last_activity_at.as_deref().and_then(|s| s.get(..19)) != timestamp.get(..19) {
            let mut next = run.clone();
            next.last_activity_at = Some(timestamp);
            self.commit_actor(
                snapshot.mission.clone(),
                super::timing::ACTIVITY_METHOD,
                vec![Entity::Run(Box::new(next))],
                vec![],
            )?;
        }
        self.append_activity(mission_id, run_id, chunk)?;
        Ok(true)
    }

    pub(super) fn mission_activity(
        &self,
        params: &serde_json::Value,
    ) -> Result<Handled, MissionRpcError> {
        let params: MissionActivityParams = Self::parse(params)?;
        if !(4..=65536).contains(&params.max_bytes) {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "max_bytes must be between 4 and 65536",
            ));
        }
        let snapshot = workflow::load_entities(&self.storage, &params.mission_id)?;
        let run = snapshot
            .runs
            .iter()
            .find(|r| r.id == params.run_id)
            .ok_or_else(|| {
                MissionRpcError::new(MissionErrorCode::NotFound, "run not found in this mission")
            })?;
        // The tail is published by atomic rename, so the poll path takes no
        // writer lock (audit F1); display-only reads never block a flush.
        let tail = self.read_activity_tail(&params.mission_id, &params.run_id)?;
        let end = tail.base + tail.body.len() as u64;
        let start = params.after_offset.get().max(tail.base);
        if start > end || !tail.body.is_char_boundary((start - tail.base) as usize) {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "activity offset is outside the available stream or splits a character",
            ));
        }
        let from = (start - tail.base) as usize;
        let mut to = (from + params.max_bytes as usize).min(tail.body.len());
        while !tail.body.is_char_boundary(to) {
            to -= 1;
        }
        let body = &tail.body.as_bytes()[from..to];
        let window_end = tail.base + to as u64;
        let body_ref = if body.is_empty() {
            None
        } else {
            // Absolute stream offsets are stable (the tail only drops from
            // the front and offsets ride the drain), so one committed
            // artifact per (mission, run, window) is reused instead of
            // minting a fresh file+row+fsync set on every poll (audit F2).
            Some(
                self.artifacts
                    .reuse_activity_window(
                        &params.mission_id,
                        &params.run_id,
                        start,
                        window_end,
                        body,
                    )
                    .map_err(|(code, message)| MissionRpcError::new(code, message))?,
            )
        };
        let result = MissionActivityResult {
            body_ref,
            next_offset: U64String::new(window_end).expect("offset bound"),
            complete: run.state.is_terminal() && to == tail.body.len(),
        };
        Ok(serde_json::to_value(result)
            .expect("activity result")
            .into())
    }
}
