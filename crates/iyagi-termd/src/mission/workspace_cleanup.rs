//! `workspace.usage` / `workspace.cleanup` (04 §8): user-requested removal of
//! daemon-owned mission worktrees. Nothing here runs automatically.
//!
//! Retention rules:
//! * only terminal missions (completed/cancelled/failed, archived or not)
//!   whose runs hold no execution slot and whose Execs all exited;
//! * a workspace is removed only when its path is a daemon-minted directory
//!   (`<uuid>` or `integration-<uuid>`) directly inside
//!   `<missions root>/<mission>/workspaces`, a worker workspace's sibling
//!   ownership marker names this mission, workspace and repository, it is
//!   not (inside) the repository checkout, no writer lease or live run names
//!   it, it is not quarantined (uncertain or user-attested execution), it is
//!   registered as a worktree of the mission repository, and `git status` is
//!   clean — Git re-checks cleanliness itself during the removal (no
//!   `--force`), so later changes are never deleted;
//! * every candidate ref (including the accepted result) and its commits are
//!   kept; only the mission's intermediate input refs are deleted;
//! * no `git worktree prune`: it would drop other (user) worktree
//!   registrations of the repository whose directories are merely offline;
//! * the Workspace projection rows stay as history — usage reads the disk.
//!
//! Sizes and Git checks are bounded (time and entry budgets, short-lived
//! cache, killed Git queries) so the call stays well inside the bridge RPC
//! timeout; an interrupted walk reports a lower bound.
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use term_contracts::{
    ids::U64String,
    mission::{
        rpc::{
            WorkspaceCleanupParams, WorkspaceCleanupResult, WorkspaceKept, WorkspaceUsageEntry,
            WorkspaceUsageParams, WorkspaceUsageResult,
        },
        types::*,
        MissionErrorCode, MissionRpcError,
    },
};

use super::{
    service::{Handled, MissionService},
    workflow::{self, MissionEntities},
};
use crate::workspace::git;

/// Size-walk budget of one call.
const USAGE_BUDGET: Duration = Duration::from_millis(1500);
const USAGE_ENTRY_BUDGET: usize = 250_000;
/// `workspace.usage` starts no Git check after this (bridge timeout 5 s).
const USAGE_CALL_BUDGET: Duration = Duration::from_millis(3000);
/// Upper bound of one Git query during `workspace.usage`.
const USAGE_GIT_TIMEOUT: Duration = Duration::from_millis(750);
/// Cleanup starts no new item after this; the rest are reported as
/// `deferred` and a repeated request continues.
const CLEANUP_BUDGET: Duration = Duration::from_secs(3);
const SIZE_CACHE_TTL: Duration = Duration::from_secs(120);
const SIZE_CACHE_ENTRIES: usize = 4096;
const REPLAY_ENTRIES: usize = 256;
/// Sibling written by the macOS verification executor (verification_isolation.rs).
const VERIFICATION_OUTPUT_SUFFIX: &str = ".verification-output";
/// Sibling ownership marker of worker workspaces (execution.rs).
const OWNER_MARKER_EXTENSION: &str = "owner.json";
const MAX_OWNER_MARKER_BYTES: u64 = 64 * 1024;

pub(super) const MISSION_ACTIVE: &str = "mission_active";
pub(super) const RUN_UNRECONCILED: &str = "run_unreconciled";

// `WorkspaceKept.reason` slugs (01 §7, 04 §8).
const NOT_DAEMON_OWNED: &str = "not_daemon_owned";
const REPOSITORY_UNAVAILABLE: &str = "repository_unavailable";
const RUN_ACTIVE: &str = "run_active";
const QUARANTINED: &str = "quarantined";
const UNREGISTERED_WORKTREE: &str = "unregistered_worktree";
const DIRTY: &str = "dirty";
const STATUS_UNAVAILABLE: &str = "status_unavailable";
const DEFERRED: &str = "deferred";
const REMOVE_FAILED: &str = "remove_failed";

#[derive(Default)]
pub(crate) struct Housekeeping {
    sizes: Mutex<HashMap<PathBuf, (Instant, u64)>>,
    replays: Mutex<VecDeque<(Id, Id, WorkspaceCleanupResult)>>,
    cleanup: Mutex<()>,
}

struct Budget {
    deadline: Instant,
    entries: usize,
}

impl Budget {
    fn exhausted(&self) -> bool {
        self.entries == 0 || Instant::now() >= self.deadline
    }
}

/// The repository's registered worktrees as seen by one call.
#[derive(Clone)]
enum Registry {
    Listed(Vec<PathBuf>),
    /// Git answered with an error (repository moved, not a repository, ...).
    Unavailable,
    /// Git did not answer inside the call budget.
    Unknown,
}

/// Canonical paths of one mission that every workspace is compared against.
struct Scope {
    repository: Option<PathBuf>,
    workspaces: Option<PathBuf>,
}

/// Bounded, non-following size walk. Returns (bytes, complete).
fn walk_size(root: &Path, budget: &mut Budget) -> (u64, bool) {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        if budget.exhausted() {
            return (total, false);
        }
        budget.entries -= 1;
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|entry| entry.path()));
            }
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    (total, true)
}

/// A mission's cleanup precondition; `None` = eligible.
pub(super) fn blocked_reason(snapshot: &MissionEntities) -> Option<&'static str> {
    if !matches!(
        snapshot.mission.state,
        MissionState::Completed | MissionState::Cancelled | MissionState::Failed
    ) {
        return Some(MISSION_ACTIVE);
    }
    if snapshot.runs.iter().any(Run::holds_execution_slot)
        || snapshot.execs.iter().any(|e| e.state != ExecState::Exited)
    {
        return Some(RUN_UNRECONCILED);
    }
    None
}

fn exists_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

fn remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
}

/// `canonicalize` in the form Git prints. On Windows it returns verbatim
/// (`\\?\C:\…`, `\\?\UNC\…`) paths while Git reports `C:/…` and `//server/…`;
/// `Path` equality ignores the separator but not the verbatim prefix.
fn canonical(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok().map(without_verbatim_prefix)
}

#[cfg(windows)]
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let plain = path.to_str().and_then(|text| {
        text.strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .or_else(|| text.strip_prefix(r"\\?\").map(str::to_owned))
    });
    plain.map(PathBuf::from).unwrap_or(path)
}

#[cfg(not(windows))]
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

/// Directory names the daemon mints under `<mission>/workspaces`: a UUID
/// (worker/verification; legacy synchronous integration) or
/// `integration-<uuid>` (supervised integration).
fn daemon_workspace_name(name: &str) -> bool {
    Id::parse(name.strip_prefix("integration-").unwrap_or(name)).is_ok()
}

/// Worker workspaces get their sibling ownership marker before
/// `git worktree add` (execution.rs); it must name this mission, workspace and
/// repository. Integration and verification worktrees have no marker: their
/// daemon-minted name and location, the projection and the repository's
/// worktree registry stand in for it. Any other kind has no known creator.
fn owner_marker_matches(path: &Path, mission: &Mission, workspace: &Workspace) -> bool {
    if matches!(
        workspace.kind,
        WorkspaceKind::Integration | WorkspaceKind::Verification
    ) {
        return true;
    }
    let marker = path.with_extension(OWNER_MARKER_EXTENSION);
    let is_small_file = std::fs::symlink_metadata(&marker)
        .is_ok_and(|m| m.is_file() && m.len() <= MAX_OWNER_MARKER_BYTES);
    if !is_small_file {
        return false;
    }
    std::fs::read(&marker)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|owner| {
            owner["mission_id"] == mission.id.as_str()
                && owner["workspace_id"] == workspace.id.as_str()
                && owner["repository_id"] == mission.repository_id.as_str()
        })
}

/// Checks that need neither Git nor the size walk. `Ok` is the canonical
/// workspace path; `Err` is the `kept` reason.
fn removal_candidate(
    scope: &Scope,
    snapshot: &MissionEntities,
    workspace: &Workspace,
) -> Result<PathBuf, &'static str> {
    let path = canonical(Path::new(&workspace.path)).ok_or(NOT_DAEMON_OWNED)?;
    let minted = scope.workspaces.as_ref().is_some_and(|dir| {
        path.parent() == Some(dir.as_path())
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(daemon_workspace_name)
    });
    if !minted || !owner_marker_matches(&path, &snapshot.mission, workspace) {
        return Err(NOT_DAEMON_OWNED);
    }
    let repository = scope.repository.as_ref().ok_or(REPOSITORY_UNAVAILABLE)?;
    if path.starts_with(repository) || repository.starts_with(&path) {
        return Err(NOT_DAEMON_OWNED);
    }
    if workspace.writer_run_id.is_some()
        || snapshot
            .runs
            .iter()
            .any(|r| r.workspace_id.as_ref() == Some(&workspace.id) && r.holds_execution_slot())
    {
        return Err(RUN_ACTIVE);
    }
    // Files of an uncertain (or user-attested) execution stay for inspection.
    if workspace.state == WorkspaceState::Quarantined {
        return Err(QUARANTINED);
    }
    Ok(path)
}

fn list_registry(repository: &Path, timeout: Duration) -> Registry {
    match git::worktree_paths_within(repository, timeout) {
        Some(Ok(paths)) => Registry::Listed(
            paths
                .into_iter()
                .map(|p| canonical(&p).unwrap_or(p))
                .collect(),
        ),
        Some(Err(_)) => Registry::Unavailable,
        None => Registry::Unknown,
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| format!("{}{suffix}", n.to_string_lossy()))
        .unwrap_or_else(|| suffix.to_owned());
    path.with_file_name(name)
}

impl MissionService {
    /// `<missions root>/<mission>` derived from the artifact store layout.
    pub(super) fn mission_dir(&self, mission_id: &Id) -> Option<PathBuf> {
        let activity = self.artifacts.activity_path(mission_id, mission_id);
        Some(activity.parent()?.parent()?.to_path_buf())
    }

    fn cleanup_scope(&self, mission: &Mission) -> Scope {
        Scope {
            repository: canonical(Path::new(&mission.repository_path)),
            workspaces: self
                .mission_dir(&mission.id)
                .and_then(|dir| canonical(&dir.join("workspaces"))),
        }
    }

    fn on_disk_workspaces<'a>(&self, snapshot: &'a MissionEntities) -> Vec<&'a Workspace> {
        snapshot
            .workspaces
            .iter()
            .filter(|w| {
                w.owned_by_daemon
                    && w.mission_id == snapshot.mission.id
                    && exists_dir(Path::new(&w.path))
            })
            .collect()
    }

    fn cached_size(&self, path: &Path, budget: &mut Budget) -> u64 {
        let now = Instant::now();
        if let Some((at, bytes)) = self
            .workspace_housekeeping
            .sizes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(path)
        {
            if now.saturating_duration_since(*at) < SIZE_CACHE_TTL {
                return *bytes;
            }
        }
        let (mut bytes, mut complete) = walk_size(path, budget);
        let output = sibling(path, VERIFICATION_OUTPUT_SUFFIX);
        if complete && exists_dir(&output) {
            let (extra, done) = walk_size(&output, budget);
            bytes = bytes.saturating_add(extra);
            complete = done;
        }
        let mut sizes = self
            .workspace_housekeeping
            .sizes
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if sizes.len() >= SIZE_CACHE_ENTRIES {
            sizes.clear();
        }
        // A partial walk is a lower bound; cache it only briefly so the next
        // call can continue with a fresh budget.
        let at = if complete {
            now
        } else {
            now.checked_sub(SIZE_CACHE_TTL - Duration::from_secs(10))
                .unwrap_or(now)
        };
        sizes.insert(path.to_path_buf(), (at, bytes));
        bytes
    }

    /// Whether cleanup could remove at least one workspace. A Git check that
    /// cannot answer inside the call budget leaves the workspace offered; the
    /// cleanup reports its actual reason.
    fn any_removable(
        &self,
        snapshot: &MissionEntities,
        workspaces: &[&Workspace],
        deadline: Instant,
        registries: &mut HashMap<PathBuf, Registry>,
    ) -> bool {
        let scope = self.cleanup_scope(&snapshot.mission);
        workspaces.iter().any(|workspace| {
            let Ok(path) = removal_candidate(&scope, snapshot, workspace) else {
                return false;
            };
            let Some(repository) = scope.repository.as_ref() else {
                return false;
            };
            let Some(left) = remaining(deadline) else {
                return true;
            };
            let registry = registries
                .entry(repository.clone())
                .or_insert_with(|| list_registry(repository, left.min(USAGE_GIT_TIMEOUT)));
            match registry {
                Registry::Listed(paths) if !paths.contains(&path) => return false,
                Registry::Unavailable => return false,
                Registry::Listed(_) | Registry::Unknown => {}
            }
            let Some(left) = remaining(deadline) else {
                return true;
            };
            !matches!(
                git::is_clean_within(&path, left.min(USAGE_GIT_TIMEOUT)),
                Some(Ok(false) | Err(_))
            )
        })
    }

    fn usage_entry(
        &self,
        snapshot: &MissionEntities,
        budget: &mut Budget,
        deadline: Instant,
        registries: &mut HashMap<PathBuf, Registry>,
    ) -> WorkspaceUsageEntry {
        let workspaces = self.on_disk_workspaces(snapshot);
        let bytes = workspaces.iter().fold(0u64, |total, w| {
            total.saturating_add(self.cached_size(Path::new(&w.path), budget))
        });
        let blocked = blocked_reason(snapshot);
        let cleanable =
            blocked.is_none() && self.any_removable(snapshot, &workspaces, deadline, registries);
        WorkspaceUsageEntry {
            mission_id: snapshot.mission.id.clone(),
            workspaces: u32::try_from(workspaces.len()).unwrap_or(u32::MAX),
            bytes: U64String::new(bytes.min(i64::MAX as u64)).expect("clamped"),
            cleanable,
            blocked_reason: blocked.map(str::to_owned),
        }
    }

    pub(super) fn workspace_usage(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: WorkspaceUsageParams = Self::parse(params)?;
        let started = Instant::now();
        let deadline = started + USAGE_CALL_BUDGET;
        let mut budget = Budget {
            deadline: started + USAGE_BUDGET,
            entries: USAGE_ENTRY_BUDGET,
        };
        let mut registries = HashMap::new();
        let mut missions = Vec::new();
        match &params.mission_id {
            Some(id) => {
                let snapshot = workflow::load_entities(&self.storage, id)?;
                let entry = self.usage_entry(&snapshot, &mut budget, deadline, &mut registries);
                missions.push(entry);
            }
            None => {
                for archived in [false, true] {
                    let mut cursor = None;
                    loop {
                        let (page, next) = self
                            .storage
                            .mission_list(cursor, 50, archived)
                            .map_err(Self::store_error)?;
                        for mission in page {
                            let snapshot = workflow::load_entities(&self.storage, &mission.id)?;
                            let entry =
                                self.usage_entry(&snapshot, &mut budget, deadline, &mut registries);
                            if entry.workspaces > 0 {
                                missions.push(entry);
                            }
                        }
                        match next {
                            Some(next) => cursor = Some(next),
                            None => break,
                        }
                    }
                }
            }
        }
        let total = missions
            .iter()
            .fold(0u64, |total, m| total.saturating_add(m.bytes.get()));
        let result = WorkspaceUsageResult {
            missions,
            total_bytes: U64String::new(total.min(i64::MAX as u64)).expect("clamped"),
        };
        Ok(serde_json::to_value(result).unwrap_or(Value::Null).into())
    }

    pub(super) fn workspace_cleanup(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: WorkspaceCleanupParams = Self::parse(params)?;
        let _serialized = self
            .workspace_housekeeping
            .cleanup
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // Same request id → the first response (this daemon's lifetime). A
        // repeat after restart is still safe: removal converges.
        if let Some((_, mission_id, result)) = self
            .workspace_housekeeping
            .replays
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|(request_id, _, _)| request_id == &params.request_id)
        {
            if mission_id != &params.mission_id {
                return Err(MissionRpcError::new(
                    MissionErrorCode::RequestConflict,
                    "request id already recorded with a different payload",
                ));
            }
            return Ok(serde_json::to_value(result).unwrap_or(Value::Null).into());
        }
        let snapshot = workflow::load_entities(&self.storage, &params.mission_id)?;
        if let Some(reason) = blocked_reason(&snapshot) {
            return Err(Self::with_reason(
                MissionRpcError::new(
                    MissionErrorCode::InvalidState,
                    match reason {
                        MISSION_ACTIVE => "workspaces of an active mission are never cleaned up",
                        _ => {
                            "a run of this mission still has no termination evidence; confirm or \
                             reconcile it first"
                        }
                    },
                ),
                reason,
            ));
        }
        let result = self.clean_mission_workspaces(&snapshot);
        let mut replays = self
            .workspace_housekeeping
            .replays
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if replays.len() >= REPLAY_ENTRIES {
            replays.pop_front();
        }
        replays.push_back((params.request_id, params.mission_id, result.clone()));
        Ok(serde_json::to_value(result).unwrap_or(Value::Null).into())
    }

    fn clean_mission_workspaces(&self, snapshot: &MissionEntities) -> WorkspaceCleanupResult {
        let mission = &snapshot.mission;
        let started = Instant::now();
        let deadline = started + CLEANUP_BUDGET;
        let scope = self.cleanup_scope(mission);
        let mut kept = Vec::new();
        let mut removed = 0u32;
        let mut freed = 0u64;
        let mut sizes = Budget {
            deadline: started + USAGE_BUDGET,
            entries: USAGE_ENTRY_BUDGET,
        };
        // Intermediate task inputs first, while the budget is fresh: one
        // `update-ref --stdin` transaction and the private index scratch.
        // Candidate refs (the accepted result included) remain fetchable, and
        // a retained worktree keeps its own HEAD commit reachable.
        if let Some(repository) = scope.repository.as_ref() {
            let _ = git::delete_private_refs(
                repository,
                &format!("refs/iyagi/missions/{}/inputs", mission.id),
            );
        }
        let mission_dir = self.mission_dir(&mission.id);
        if let Some(indexes) = mission_dir.map(|dir| dir.join("input-indexes")) {
            if exists_dir(&indexes) {
                if remaining(deadline).is_some() {
                    let _ = std::fs::remove_dir_all(&indexes);
                } else {
                    kept.push(WorkspaceKept {
                        path: indexes.to_string_lossy().into_owned(),
                        reason: DEFERRED.into(),
                    });
                }
            }
        }
        let mut registry = None;
        for workspace in self.on_disk_workspaces(snapshot) {
            let mut keep = |reason: &str| {
                kept.push(WorkspaceKept {
                    path: workspace.path.clone(),
                    reason: reason.into(),
                })
            };
            // Nothing new starts once the budget is spent.
            if remaining(deadline).is_none() {
                keep(DEFERRED);
                continue;
            }
            let path = match removal_candidate(&scope, snapshot, workspace) {
                Ok(path) => path,
                Err(reason) => {
                    keep(reason);
                    continue;
                }
            };
            let Some(repository) = scope.repository.as_ref() else {
                keep(REPOSITORY_UNAVAILABLE);
                continue;
            };
            let Some(left) = remaining(deadline) else {
                keep(DEFERRED);
                continue;
            };
            // Never delete an arbitrary directory Git does not own (04 §1).
            match registry.get_or_insert_with(|| list_registry(repository, left)) {
                Registry::Listed(paths) if paths.contains(&path) => {}
                Registry::Listed(_) => {
                    keep(UNREGISTERED_WORKTREE);
                    continue;
                }
                Registry::Unavailable => {
                    keep(REPOSITORY_UNAVAILABLE);
                    continue;
                }
                Registry::Unknown => {
                    keep(DEFERRED);
                    continue;
                }
            }
            let Some(left) = remaining(deadline) else {
                keep(DEFERRED);
                continue;
            };
            match git::is_clean_within(&path, left) {
                Some(Ok(true)) => {}
                Some(Ok(false)) => {
                    keep(DIRTY);
                    continue;
                }
                Some(Err(_)) => {
                    keep(STATUS_UNAVAILABLE);
                    continue;
                }
                None => {
                    keep(DEFERRED);
                    continue;
                }
            }
            let bytes = self.cached_size(Path::new(&workspace.path), &mut sizes);
            // Without `--force` Git refuses a worktree that changed after the
            // status above; the directory then simply stays.
            let _ = git::remove_clean_worktree(repository, &path);
            if exists_dir(&path) {
                keep(REMOVE_FAILED);
                continue;
            }
            removed += 1;
            freed = freed.saturating_add(bytes);
            self.workspace_housekeeping
                .sizes
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(Path::new(&workspace.path));
            let marker = path.with_extension(OWNER_MARKER_EXTENSION);
            if std::fs::symlink_metadata(&marker).is_ok_and(|m| m.is_file()) {
                let _ = std::fs::remove_file(&marker);
            }
            let output = sibling(&path, VERIFICATION_OUTPUT_SUFFIX);
            if exists_dir(&output) && output.parent() == scope.workspaces.as_deref() {
                let _ = std::fs::remove_dir_all(&output);
            }
        }
        WorkspaceCleanupResult {
            removed,
            freed_bytes: U64String::new(freed.min(i64::MAX as u64)).expect("clamped"),
            kept,
        }
    }
}
