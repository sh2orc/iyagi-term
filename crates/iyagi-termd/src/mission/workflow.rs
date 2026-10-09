//! Mission workflow (ticket O13, spec 02 §4 steps 6–10 and 04 §4–§6): the
//! goal→completed pipeline composed from the existing primitives —
//! integration + immutable candidate minting, real verification-command
//! execution inside a fresh worktree, typed review findings, and the
//! `mission.accept` gate.
//!
//! Layering: this module never re-implements scheduling, Git, or process
//! supervision — it drives `workspace::*`, `engine::new_decision`, and one
//! `apply_mission_transition` per mutation with revision CAS. The
//! `MissionService.artifacts` handle is private to `service`, so entry
//! points take `&ArtifactStore` explicitly (the service router passes its
//! own; callers that drive the engine directly pass theirs over the same
//! mission root).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use term_contracts::ids::{ConnectionId, U64String};
use term_contracts::mission::error::{MissionErrorCode, MissionErrorDetails, MissionRpcError};
use term_contracts::mission::rpc::{methods, MissionAcceptParams};
use term_contracts::mission::types::{
    AgentResult, ArtifactRef, Candidate, Decision, DecisionKind, DecisionOption, Entity,
    ExpectedOutput, Finding, FindingResolution, FindingSeverity, Id, InputIntegrity, Mission,
    MissionEventType, MissionState, Phase, ProviderFindingDraft, Run, RunDispatchState, RunState,
    Task, TaskContract, TaskKind, TaskState, Verification, VerificationCommand, VerificationStatus,
    Workspace, WorkspaceKind, WorkspaceState,
};
use term_storage::mission::types::{AppliedTransition, ApplyMissionTransition, ApplyMode};
use term_storage::Storage;

use crate::exec::{BoundedSpool, DEFAULT_SPOOL_BYTES};
use crate::workspace::{self, add_detached_worktree, IntegrationOutcome};

use super::artifacts::ArtifactStore;
use super::engine::new_decision;
use super::service::{Handled, MissionService};

fn now() -> String {
    term_storage::time::now_iso8601()
}

fn next_revision(mission: &Mission) -> U64String {
    U64String::new(mission.revision.get() + 1).expect("fits SQLite bound")
}

/// Git failures with a stable `details.reason_code` where the user can act:
/// `git_unavailable`, `not_a_repository`, `no_commits`, `dirty_worktree`,
/// `base_changed`, `unmerged_paths`, `snapshot_too_large`.
pub(super) fn git_error(error: workspace::GitError) -> MissionRpcError {
    let reason_code = error.reason_code().map(str::to_string);
    let (code, message) = match error {
        workspace::GitError::DirtyWorktree(samples) => (
            MissionErrorCode::DirtyWorktree,
            format!("dirty worktree (samples: {samples:?}); clean, stash, or commit first"),
        ),
        workspace::GitError::NotARepository(path) => (
            MissionErrorCode::InvalidArgument,
            format!("{path} is not a git repository"),
        ),
        workspace::GitError::NoCommits(path) => (
            MissionErrorCode::InvalidArgument,
            format!("{path} has no commits yet; create an initial commit first"),
        ),
        workspace::GitError::BaseMoved => (
            MissionErrorCode::InvalidState,
            "repository HEAD moved while the base was being recorded; start again from the current base".to_string(),
        ),
        workspace::GitError::UnmergedPaths => (
            MissionErrorCode::DirtyWorktree,
            "the repository has unmerged paths; finish or abort the merge, then start again"
                .to_string(),
        ),
        workspace::GitError::SnapshotTooLarge(detail) => (
            MissionErrorCode::InvalidArgument,
            format!("uncommitted changes cannot be used as a base ({detail}); commit them, ignore them, or start from HEAD"),
        ),
        workspace::GitError::GitUnavailable(detail) => (
            MissionErrorCode::InvalidState,
            format!("git is not available to the daemon ({detail}); install Git and retry"),
        ),
        other => (
            MissionErrorCode::Internal,
            format!("git plumbing failed: {other}"),
        ),
    };
    MissionRpcError::with_details(
        code,
        message,
        MissionErrorDetails {
            reason_code,
            ..Default::default()
        },
    )
}

fn integration_error(error: workspace::IntegrationError) -> MissionRpcError {
    match error {
        workspace::IntegrationError::Empty => state_error(
            "integration_empty",
            "integration produced no changes; nothing to mint".into(),
        ),
        workspace::IntegrationError::InvalidInput(message) => {
            MissionRpcError::new(MissionErrorCode::IntegrityFailed, message)
        }
        workspace::IntegrationError::Git(git) => git_error(git),
        workspace::IntegrationError::Conflict { candidate, .. } => state_error(
            "integration_conflict",
            format!("candidate {candidate} conflicted before minting"),
        ),
    }
}

fn state_error(reason: &str, message: String) -> MissionRpcError {
    MissionRpcError::with_details(
        MissionErrorCode::InvalidState,
        message,
        MissionErrorDetails {
            reason_code: Some(reason.to_string()),
            ..Default::default()
        },
    )
}

// ---- shared plumbing -------------------------------------------------------

/// One projected mission snapshot split by entity kind (engine read model).
pub struct MissionEntities {
    pub mission: Mission,
    pub tasks: Vec<Task>,
    pub runs: Vec<Run>,
    pub execs: Vec<term_contracts::mission::types::ExecRecord>,
    pub workspaces: Vec<Workspace>,
    pub decisions: Vec<Decision>,
    pub candidates: Vec<Candidate>,
    pub verifications: Vec<Verification>,
    pub findings: Vec<Finding>,
    pub messages: Vec<term_contracts::mission::types::Message>,
}

pub fn load_entities(
    storage: &Storage,
    mission_id: &Id,
) -> Result<MissionEntities, MissionRpcError> {
    let snapshot = storage
        .mission_snapshot(mission_id)
        .map_err(MissionService::store_error)?
        .ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::NotFound,
                format!("mission {mission_id} not found"),
            )
        })?;
    let mut mission = None;
    let mut tasks = Vec::new();
    let mut runs = Vec::new();
    let mut execs = Vec::new();
    let mut workspaces = Vec::new();
    let mut decisions = Vec::new();
    let mut candidates = Vec::new();
    let mut verifications = Vec::new();
    let mut findings = Vec::new();
    let mut messages = Vec::new();
    for entity in snapshot.entities {
        match entity {
            Entity::Mission(value) => mission = Some(*value),
            Entity::Task(value) => tasks.push(*value),
            Entity::Run(value) => runs.push(*value),
            Entity::Exec(value) => execs.push(*value),
            Entity::Workspace(value) => workspaces.push(*value),
            Entity::Decision(value) => decisions.push(*value),
            Entity::Candidate(value) => candidates.push(*value),
            Entity::Verification(value) => verifications.push(*value),
            Entity::Finding(value) => findings.push(*value),
            Entity::Message(value) => messages.push(*value),
            _ => {}
        }
    }
    Ok(MissionEntities {
        mission: mission.ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::Internal,
                "mission snapshot lacks its mission row",
            )
        })?,
        tasks,
        runs,
        execs,
        workspaces,
        decisions,
        candidates,
        verifications,
        findings,
        messages,
    })
}

/// Commit one engine mutation: the caller supplies the post-state mission
/// (still at its pre-commit revision) plus entity upserts; this bumps the
/// revision, prepends the mission projection, and applies everything in a
/// single storage transaction (01 §2 — no partial state ever lands).
pub fn commit_upserts(
    service: &MissionService,
    mut next_mission: Mission,
    method: &str,
    fingerprint_seed: &str,
    event_type: MissionEventType,
    mut upserts: Vec<Entity>,
) -> Result<AppliedTransition, MissionRpcError> {
    next_mission.revision = next_revision(&next_mission);
    next_mission.updated_at = now();
    upserts.insert(0, Entity::Mission(Box::new(next_mission.clone())));
    let mut hasher = Sha256::new();
    hasher.update(method.as_bytes());
    hasher.update(b"\n");
    hasher.update(fingerprint_seed.as_bytes());
    let fingerprint: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let transition = ApplyMissionTransition {
        request_id: Id::generate(),
        method: method.to_string(),
        fingerprint,
        mission_id: next_mission.id.clone(),
        mode: ApplyMode::Mutate {
            expected_revision: next_mission.revision.get() - 1,
        },
        transaction_id: Id::generate(),
        event_type,
        upserts,
        deletes: Vec::new(),
        changes_ref: None,
        outbox: Vec::new(),
        outbox_updates: Vec::new(),
        adopt_staged_artifacts: Vec::new(),
        created_at: now(),
    };
    service
        .apply_timed_transition(transition)
        .map_err(MissionService::store_error)
}

/// Write one mission-scoped artifact through the chunked protocol (O05) and
/// return its reference. Engine-owned writes have no client connection; the
/// synthetic connection id only labels the upload row.
pub fn store_artifact(
    artifacts: &ArtifactStore,
    mission_id: &Id,
    media_type: &str,
    body: &[u8],
) -> Result<ArtifactRef, MissionRpcError> {
    store_artifact_with_retry(artifacts, mission_id, media_type, body, false)
}

fn retry_artifact_storage<T>(
    retry: bool,
    mut action: impl FnMut() -> Result<T, (MissionErrorCode, String)>,
) -> Result<T, MissionRpcError> {
    loop {
        match action() {
            Err((MissionErrorCode::StorageUnavailable, _)) if retry => {
                std::thread::sleep(Duration::from_millis(100));
            }
            result => return result.map_err(|(code, message)| MissionRpcError::new(code, message)),
        }
    }
}

pub(super) fn store_artifact_with_retry(
    artifacts: &ArtifactStore,
    mission_id: &Id,
    media_type: &str,
    body: &[u8],
    retry: bool,
) -> Result<ArtifactRef, MissionRpcError> {
    use base64::Engine;
    use term_contracts::mission::rpc::{
        ArtifactBeginParams, ArtifactCommitParams, ArtifactWriteParams,
    };
    let digest: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let conn = ConnectionId::generate();
    let (upload_id, chunk_bytes) = retry_artifact_storage(retry, || {
        artifacts.begin_checked(
            &conn,
            &ArtifactBeginParams {
                request_id: Id::generate(),
                mission_id: Some(mission_id.clone()),
                media_type: media_type.to_string(),
                bytes: U64String::new(body.len() as u64).expect("fits SQLite bound"),
                sha256: digest.clone(),
            },
        )
    })?;
    for (index, slice) in body.chunks(chunk_bytes.max(1) as usize).enumerate() {
        retry_artifact_storage(retry, || {
            artifacts.write(&ArtifactWriteParams {
                upload_id: upload_id.clone(),
                offset: U64String::new((index * chunk_bytes.max(1) as usize) as u64)
                    .expect("fits SQLite bound"),
                data_b64: base64::engine::general_purpose::STANDARD.encode(slice),
            })
        })?;
    }
    retry_artifact_storage(retry, || {
        artifacts.commit(&ArtifactCommitParams {
            upload_id: upload_id.clone(),
        })
    })
}

// ---- 1. integration (04 §4) -------------------------------------------------

/// Outcome of [`integrate_and_mint`].
pub enum MintResult {
    /// Clean integration: the immutable integrated candidate is committed.
    Integrated {
        outcome: IntegrationOutcome,
        candidate: Box<Candidate>,
    },
    /// Conflicting sources: a blocking Conflict decision was recorded and the
    /// mission keeps running (04 §4 — no implicit "ours", no mission failure).
    Conflict {
        outcome: IntegrationOutcome,
        decision_id: Id,
    },
}

/// Integrate writer candidates (plan topological order) onto a fresh
/// worktree at the mission base and mint the integrated candidate, or record
/// the partial state behind a blocking Conflict decision for the
/// integrator/user to resolve (02 §4 step 6).
pub fn integrate_and_mint(
    service: &MissionService,
    artifacts: &ArtifactStore,
    mission_id: &Id,
    repository: &Path,
    integration_worktree: &Path,
    sources: &[(Id, Vec<Id>)],
) -> Result<MintResult, MissionRpcError> {
    let snapshot = load_entities(&service.storage, mission_id)?;
    let sources = freeze_integration_sources(&snapshot, sources)?;
    let produced = produce_integration(
        repository,
        integration_worktree,
        mission_id,
        &snapshot.mission.base_oid,
        &sources,
    )?;
    publish_integration(
        service,
        artifacts,
        snapshot.mission,
        integration_worktree,
        &sources,
        produced,
        None,
    )
}

pub(super) fn freeze_integration_sources(
    snapshot: &MissionEntities,
    sources: &[(Id, Vec<Id>)],
) -> Result<Vec<workspace::integration::IntegrationSource>, MissionRpcError> {
    let mission = &snapshot.mission;
    // Freeze every input from one committed snapshot. Missing candidates
    // must not fall back to a ref or the mission base, and caller-supplied
    // provenance must not relabel another run's patch.
    sources
        .iter()
        .map(|(id, runs)| {
            let candidate = snapshot
                .candidates
                .iter()
                .find(|c| &c.id == id)
                .ok_or_else(|| {
                    MissionRpcError::new(
                        MissionErrorCode::IntegrityFailed,
                        "integration source candidate is missing",
                    )
                })?;
            if candidate.mission_id != mission.id
                || &candidate.source_run_ids != runs
                || runs.is_empty()
                || runs.iter().collect::<HashSet<_>>().len() != runs.len()
                || !runs.iter().all(|id| {
                    snapshot.runs.iter().any(|r| {
                        &r.id == id && r.mission_id == mission.id && r.state == RunState::Succeeded
                    })
                })
            {
                return Err(MissionRpcError::new(
                    MissionErrorCode::IntegrityFailed,
                    "integration source lost its successful run provenance",
                ));
            }
            Ok(workspace::integration::IntegrationSource {
                candidate_id: candidate.id.clone(),
                source_run_ids: candidate.source_run_ids.clone(),
                base_oid: candidate.base_oid.clone(),
                commit_oid: candidate.commit_oid.clone(),
                tree_oid: candidate.tree_oid.clone(),
            })
        })
        .collect::<Result<_, MissionRpcError>>()
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProducedIntegration {
    pub outcome: IntegrationOutcome,
    pub integrated: Option<workspace::IntegratedCandidate>,
    pub manifest: Option<Value>,
    #[serde(default)]
    pub workspace_head_oid: Option<String>,
}

pub(super) fn produce_integration(
    repository: &Path,
    integration_worktree: &Path,
    mission_id: &Id,
    base_oid: &str,
    sources: &[workspace::integration::IntegrationSource],
) -> Result<ProducedIntegration, MissionRpcError> {
    workspace::integration::validate_sources(repository, sources, base_oid)
        .map_err(integration_error)?;
    add_detached_worktree(repository, base_oid, integration_worktree).map_err(git_error)?;
    let (outcome, integrated) = workspace::integration::integrate(
        repository,
        integration_worktree,
        mission_id,
        sources,
        base_oid,
    )
    .map_err(integration_error)?;
    let manifest = integrated
        .as_ref()
        .map(|candidate| {
            manifest_document(
                integration_worktree,
                base_oid,
                &candidate.commit_oid,
                &candidate.tree_oid,
                &candidate.sources,
            )
        })
        .transpose()?;
    Ok(ProducedIntegration {
        outcome,
        integrated,
        manifest,
        workspace_head_oid: Some(
            crate::workspace::git::rev_parse(integration_worktree, "HEAD").map_err(git_error)?,
        ),
    })
}

/// An Integrator can resolve conflicts only when the mission policy allows
/// the role and a role binding names who performs it.
pub(super) fn integrator_available(mission: &Mission) -> bool {
    mission
        .policy
        .allowed_roles
        .contains(&term_contracts::mission::types::Role::Integrator)
        && mission
            .role_bindings
            .iter()
            .any(|r| r.role == term_contracts::mission::types::Role::Integrator)
}

pub(super) fn publish_integration(
    service: &MissionService,
    artifacts: &ArtifactStore,
    mission: Mission,
    integration_worktree: &Path,
    frozen_sources: &[workspace::integration::IntegrationSource],
    produced: ProducedIntegration,
    completion: Option<&super::integration_exec::Completion<'_>>,
) -> Result<MintResult, MissionRpcError> {
    let mission_id = &mission.id;
    let ProducedIntegration {
        outcome,
        integrated,
        manifest: produced_manifest,
        workspace_head_oid: _,
    } = produced;
    let Some(integrated) = integrated else {
        // Conflict path: keep the mission alive and ask for a resolution.
        let entities = load_entities(&service.storage, mission_id)?;
        let (conflicting, paths) = outcome
            .conflict
            .clone()
            .expect("integrate returned no candidate but no conflict either");
        let document = json!({
            "phase": "integration_conflict",
            "base_oid": mission.base_oid,
            "integration_worktree": integration_worktree.to_string_lossy(),
            "input_sources": frozen_sources,
            "applied_sources": outcome.sources,
            "conflict": {
                "candidate_id": conflicting.to_string(),
                "paths": paths,
            },
            "note": "partial integration preserved; no implicit ours/theirs choice was made",
        });
        let question_ref = store_artifact_with_retry(
            artifacts,
            mission_id,
            "application/json",
            document.to_string().as_bytes(),
            completion.is_some(),
        )?;
        let mut run_ids: HashSet<Id> = frozen_sources
            .iter()
            .flat_map(|source| source.source_run_ids.iter().cloned())
            .collect();
        let mut affected: Vec<Id> = Vec::new();
        for run in &entities.runs {
            if run_ids.remove(&run.id) && !affected.contains(&run.task_id) {
                affected.push(run.task_id.clone());
            }
        }
        let mut options = vec![
            DecisionOption {
                id: "resolve_and_reintegrate".into(),
                label: "Resolve the conflicts manually, then re-integrate".into(),
            },
            DecisionOption {
                id: "exclude_candidate".into(),
                label: "Exclude this candidate and dependent work, then ask the Lead to replan"
                    .into(),
            },
            DecisionOption {
                id: "stop_mission".into(),
                label: "Stop the mission".into(),
            },
        ];
        if completion.is_some() {
            options[0].label = "Ask the assigned integrator to resolve and continue".into();
        }
        // Quick missions may run without an Integrator: never offer a
        // resolution nobody is allowed or assigned to perform.
        if completion.is_some() && !integrator_available(&mission) {
            options.retain(|option| option.id != "resolve_and_reintegrate");
        }
        let (decision_id, decision) = new_decision(
            &mission,
            DecisionKind::Conflict,
            question_ref,
            options,
            affected,
            true,
            completion.map(|c| c.run_id()),
        );
        let mut next = mission.clone();
        next.phase = Phase::Integrating;
        next.open_decision_count += 1;
        commit_integration_projection(
            service,
            next,
            "engine.integrate.conflict",
            &format!("{}:{decision_id}", mission.id),
            MissionEventType::Changed,
            vec![Entity::Decision(Box::new(decision))],
            completion,
        )?;
        return Ok(MintResult::Conflict {
            outcome,
            decision_id,
        });
    };

    // Clean path: manifest artifact → immutable Candidate entity → mission
    // pointer + integration Workspace row, all in one transaction.
    let mut manifest = produced_manifest.ok_or_else(|| {
        MissionRpcError::new(
            MissionErrorCode::IntegrityFailed,
            "integration result is missing its manifest",
        )
    })?;
    let snapshot = load_entities(&service.storage, mission_id)?;
    let excluded = service.integration_exclusions(&snapshot)?;
    if integrated.sources.iter().any(|s| {
        s.source_run_ids
            .iter()
            .any(|id| excluded.run_ids.contains(id))
    }) {
        return Err(state_error(
            "excluded_source",
            "integration still includes excluded work".into(),
        ));
    }
    if !excluded.decision_ids.is_empty() {
        manifest["exclusion_decision_ids"] =
            serde_json::to_value(&excluded.decision_ids).expect("exclusion IDs");
    }
    let manifest_ref = store_artifact_with_retry(
        artifacts,
        mission_id,
        "application/json",
        manifest.to_string().as_bytes(),
        completion.is_some(),
    )?;
    let mut source_run_ids = integrated
        .sources
        .iter()
        .flat_map(|source| source.source_run_ids.iter().cloned())
        .collect::<Vec<_>>();
    if let Some(ids) = manifest.get("resolution_run_ids") {
        let ids: Vec<Id> = serde_json::from_value(ids.clone()).map_err(|_| {
            MissionRpcError::new(
                MissionErrorCode::IntegrityFailed,
                "invalid integration resolution provenance",
            )
        })?;
        for id in ids {
            if !source_run_ids.contains(&id) {
                source_run_ids.push(id);
            }
        }
    }
    let prior = snapshot
        .candidates
        .into_iter()
        .filter(|c| c.revision > 0)
        .max_by_key(|c| c.revision);
    let candidate = Candidate {
        id: integrated.candidate_id.clone(),
        mission_id: mission_id.clone(),
        revision: prior.as_ref().map_or(1, |c| c.revision + 1),
        base_oid: mission.base_oid.clone(),
        tree_oid: integrated.tree_oid.clone(),
        commit_oid: integrated.commit_oid.clone(),
        source_run_ids,
        manifest_ref,
        created_at: now(),
        supersedes_id: prior.map(|c| c.id),
    };
    let workspace_entity = Workspace {
        id: Id::generate(),
        mission_id: mission_id.clone(),
        path: integration_worktree.to_string_lossy().into_owned(),
        kind: WorkspaceKind::Integration,
        base_oid: mission.base_oid.clone(),
        head_oid: integrated.commit_oid.clone(),
        writer_run_id: None,
        lease_token: U64String::new(0).expect("fits SQLite bound"),
        // The synchronous integration finished; the worktree is retained
        // (04 §8: daemon-owned cleanup is a separate, checked operation).
        state: WorkspaceState::Retained,
        owned_by_daemon: true,
    };
    let mut next = mission.clone();
    next.phase = Phase::Validating;
    next.candidate_id = Some(candidate.id.clone());
    commit_integration_projection(
        service,
        next,
        "engine.integrate",
        &format!("{}:{}", mission.id, candidate.id),
        MissionEventType::Changed,
        vec![
            Entity::Candidate(Box::new(candidate.clone())),
            Entity::Workspace(Box::new(workspace_entity)),
        ],
        completion,
    )?;
    Ok(MintResult::Integrated {
        outcome,
        candidate: Box::new(candidate),
    })
}

fn commit_integration_projection(
    service: &MissionService,
    next: Mission,
    method: &str,
    key: &str,
    event: MissionEventType,
    upserts: Vec<Entity>,
    completion: Option<&super::integration_exec::Completion<'_>>,
) -> Result<(), MissionRpcError> {
    if let Some(completion) = completion {
        completion.commit(service, next, upserts)
    } else {
        commit_upserts(service, next, method, key, event, upserts).map(|_| ())
    }
}

/// Integrated-candidate manifest: base/commit/tree oids, the applied source
/// chain, and per-path stats read from the integration worktree.
pub(super) fn manifest_document(
    worktree: &Path,
    base: &str,
    commit: &str,
    tree: &str,
    sources: &[workspace::integration::IntegrationSource],
) -> Result<Value, MissionRpcError> {
    let entries = workspace::git::diff_names(worktree, base, commit).map_err(git_error)?;
    let mut json_entries = Vec::with_capacity(entries.len());
    for entry in &entries {
        let (bytes, sha256) = if entry.kind == workspace::git::ChangeKind::Deleted {
            (0, String::new())
        } else {
            let content =
                workspace::git::read_blob_bounded(worktree, commit, &entry.path, 64 * 1024 * 1024)
                    .map_err(git_error)?;
            (
                content.len() as u64,
                format!("{:x}", Sha256::digest(&content)),
            )
        };
        json_entries.push(json!({
            "path": entry.path,
            "change": workspace::capture::change_kind_of(entry),
            "bytes": bytes,
            "sha256": sha256,
        }));
    }
    Ok(json!({
        "base_oid": base,
        "commit_oid": commit,
        "tree_oid": tree,
        "sources": sources,
        "entries": json_entries,
    }))
}

// ---- 2. verification (04 §5) ------------------------------------------------

/// Inputs for one verification execution. The task/run pair comes from
/// [`mint_verify_task`]; `repository` + `worktree` locate the Git state.
pub struct VerificationRequest<'a> {
    pub mission_id: &'a Id,
    pub command: &'a VerificationCommand,
    pub candidate_id: &'a Id,
    pub repository: &'a Path,
    pub worktree: &'a Path,
    pub verify_task_id: &'a Id,
    pub verify_run_id: &'a Id,
    pub requirement_ids: Vec<Id>,
}

/// What a completed verification run produced.
pub struct VerificationRunResult {
    pub verification: Verification,
    pub timed_out: bool,
}

/// Create the per-command verify task + deterministic run (02 §4 step 7).
/// The task is required and model-less (`verify` → role/binding null).
pub fn mint_verify_task(
    service: &MissionService,
    artifacts: &ArtifactStore,
    mission_id: &Id,
    command: &VerificationCommand,
    requirement_ids: Vec<Id>,
) -> Result<(Id, Id), MissionRpcError> {
    let mission = service.read_mission(mission_id)?;
    if !mission
        .policy
        .allowed_verification_ids
        .contains(&command.id)
    {
        return Err(MissionRpcError::new(
            MissionErrorCode::PolicyDenied,
            format!(
                "verification command {} is outside the mission allowlist",
                command.id
            ),
        ));
    }
    let entities = load_entities(&service.storage, mission_id)?;
    let ordinal = entities
        .tasks
        .iter()
        .map(|task| task.ordinal)
        .max()
        .unwrap_or(0)
        + 1;
    let command_snapshot = serde_json::to_value(command).map_err(|error| {
        MissionRpcError::new(
            MissionErrorCode::Internal,
            format!("command snapshot: {error}"),
        )
    })?;
    let objective_ref = store_artifact(
        artifacts,
        mission_id,
        "application/json",
        command_snapshot.to_string().as_bytes(),
    )?;
    let timestamp = now();
    let task_id = Id::generate();
    let run_id = Id::generate();
    let task = Task {
        id: task_id.clone(),
        mission_id: mission_id.clone(),
        title: format!("verify: {}", command.title),
        kind: TaskKind::Verify,
        role: None,
        state: TaskState::Running,
        required: true,
        parent_task_id: None,
        depends_on: Vec::new(),
        contract: TaskContract {
            objective_ref: objective_ref.clone(),
            requirement_ids: requirement_ids.clone(),
            input_artifact_ids: Vec::new(),
            allowed_paths: Vec::new(),
            expected_outputs: vec![ExpectedOutput::Verification],
            verification_ids: vec![command.id.clone()],
            specialty: None,
        },
        binding_id: None,
        active_run_id: Some(run_id.clone()),
        ordinal,
        attempt_count: 1,
        repair_cycle: 0,
        failure_repair_run_ids: vec![],
        integration: None,
        replacement_of: None,
        blocked_code: None,
        dispatch_after_unix_ms: None,
        workspace_id: None,
        created_at: timestamp.clone(),
        updated_at: timestamp.clone(),
    };
    let run = Run {
        id: run_id.clone(),
        mission_id: mission_id.clone(),
        task_id: task_id.clone(),
        attempt: 1,
        state: RunState::Running,
        // Deterministic verification run: no provider binding (types note).
        binding_snapshot: None,
        requested_model: None,
        observed_model: None,
        provider_session_id: None,
        provider_turn_id: None,
        exec_id: None,
        pty_session_id: None,
        workspace_id: None,
        fencing_token: next_revision(&mission),
        dispatch_state: RunDispatchState::Acknowledged,
        context_ref: objective_ref,
        result_ref: None,
        usage: term_contracts::mission::validation::unknown_usage(),
        last_activity_at: Some(timestamp.clone()),
        active_time_ms: U64String::new(0).expect("fits SQLite bound"),
        started_at: Some(timestamp.clone()),
        ended_at: None,
        failure_code: None,
        reconciliation_ref: None,
        reconciliation_kind: None,
        rate_limit: None,
        retry_evidence: None,
    };
    let mut next = mission.clone();
    next.phase = Phase::Validating;
    commit_upserts(
        service,
        next,
        "engine.verify.prepare",
        &format!("{}:{task_id}", mission.id),
        MissionEventType::RunDispatched,
        vec![Entity::Task(Box::new(task)), Entity::Run(Box::new(run))],
    )?;
    Ok((task_id, run_id))
}

/// Execute one VerificationCommand as a real child process inside a fresh
/// verification worktree at the candidate commit (04 §5): argv only (never a
/// shell string), a watchdog thread that kills the process tree on timeout,
/// stdout/stderr into bounded spools, the log preserved as a mission-scoped
/// artifact, and the Verification + task/run outcome committed atomically.
/// A failed command fails the *task*, never the mission.
pub fn run_verification(
    service: &MissionService,
    artifacts: &ArtifactStore,
    request: &VerificationRequest<'_>,
) -> Result<VerificationRunResult, MissionRpcError> {
    run_verification_cancellable(service, artifacts, request, &AtomicBool::new(false), None)
}

pub(super) fn run_verification_cancellable(
    service: &MissionService,
    artifacts: &ArtifactStore,
    request: &VerificationRequest<'_>,
    cancel: &AtomicBool,
    executor: Option<&super::verification_exec::Executor>,
) -> Result<VerificationRunResult, MissionRpcError> {
    let mission = service.read_mission(request.mission_id)?;
    if !mission
        .policy
        .allowed_verification_ids
        .contains(&request.command.id)
    {
        return Err(MissionRpcError::new(
            MissionErrorCode::PolicyDenied,
            format!(
                "verification command {} is outside the mission allowlist",
                request.command.id
            ),
        ));
    }
    if mission.candidate_id.as_ref() != Some(request.candidate_id) {
        return Err(MissionRpcError::new(
            MissionErrorCode::StaleCandidate,
            "the mission no longer points at the candidate being verified",
        ));
    }
    let entities = load_entities(&service.storage, request.mission_id)?;
    let candidate = entities
        .candidates
        .iter()
        .find(|candidate| candidate.id == *request.candidate_id)
        .ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::NotFound,
                format!("candidate {} not found", request.candidate_id),
            )
        })?;
    let task = entities
        .tasks
        .iter()
        .find(|task| task.id == *request.verify_task_id)
        .ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::NotFound,
                format!("verify task {} not found", request.verify_task_id),
            )
        })?;
    let run = entities
        .runs
        .iter()
        .find(|run| run.id == *request.verify_run_id)
        .ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::NotFound,
                format!("verify run {} not found", request.verify_run_id),
            )
        })?;
    if task.kind != TaskKind::Verify || run.task_id != task.id {
        return Err(state_error(
            "verification_task_mismatch",
            "the run does not belong to the named verify task".into(),
        ));
    }
    if task.state.is_terminal() || run.state.is_terminal() {
        return Err(state_error(
            "verification_already_finished",
            "the verify task/run pair is already terminal".into(),
        ));
    }
    if !task.contract.verification_ids.contains(&request.command.id) {
        return Err(state_error(
            "verification_command_mismatch",
            "the verify task contract does not cover this command".into(),
        ));
    }

    // Independent worktree at the exact candidate commit (04 §5).
    if executor.is_some() {
        super::verification_isolation::Isolation::require_supported()?;
        crate::workspace::verification::create(
            request.repository,
            &candidate.commit_oid,
            request.worktree,
        )
        .map_err(git_error)?;
    } else {
        add_detached_worktree(request.repository, &candidate.commit_oid, request.worktree)
            .map_err(git_error)?;
    }
    let cwd = request.worktree.join(&request.command.cwd_relative);
    if !cwd.is_dir() {
        return Err(MissionRpcError::new(
            MissionErrorCode::InvalidArgument,
            format!(
                "verification cwd {:?} does not exist in the candidate worktree",
                cwd
            ),
        ));
    }

    let command_snapshot_ref = store_artifact(
        artifacts,
        request.mission_id,
        "application/json",
        serde_json::to_string(request.command)
            .unwrap_or_default()
            .as_bytes(),
    )?;
    let started_at = now();
    let (raw, environment, input_integrity) = if let Some(executor) = executor {
        let result = executor.execute(service, request, &cwd, cancel)?;
        (result.raw, result.environment, result.input_integrity)
    } else {
        let raw = execute_command(request.command, &cwd, cancel)?;
        (
            raw,
            json!({"cwd":cwd,"worktree":request.worktree,"input_integrity":"observed",
            "integrity_basis":"legacy direct executor has no OS input-write isolation"}),
            InputIntegrity::Observed,
        )
    };
    let ended_at = now();
    // This evidence is produced after execution; a storage outage must retry
    // the same bytes without repeating the command.
    let environment_ref = store_artifact_with_retry(
        artifacts,
        request.mission_id,
        "application/json",
        &serde_json::to_vec(&environment).expect("verification environment"),
        true,
    )?;
    // passed requires exit 0, no timeout, and known input integrity (04 §5).
    let status = if raw.cancelled {
        VerificationStatus::Cancelled
    } else if raw.exit_code == Some(0)
        && !raw.timed_out
        && input_integrity != InputIntegrity::Unknown
    {
        VerificationStatus::Passed
    } else {
        VerificationStatus::Failed
    };

    let log_body = format!(
        "iyagi verification log\ncommand: {} ({:?} {:?})\ncandidate: {}\nexit_code: {}\ntimed_out: {}\n--- stdout (retained {} of {} bytes) ---\n{}--- stderr (retained {} of {} bytes) ---\n{}",
        request.command.title,
        request.command.program,
        request.command.argv,
        request.candidate_id,
        match raw.exit_code { Some(code) => code.to_string(), None => "signal".to_string() },
        raw.timed_out,
        raw.stdout_tail.len(),
        raw.stdout_total,
        raw.stdout_tail,
        raw.stderr_tail.len(),
        raw.stderr_total,
        raw.stderr_tail,
    );
    // Retain the completed command's bounded log while retrying the same
    // upload/chunk/commit. A storage outage must never relaunch the command.
    let log_ref = store_artifact_with_retry(
        artifacts,
        request.mission_id,
        "text/plain",
        log_body.as_bytes(),
        true,
    )?;

    let verification = Verification {
        id: Id::generate(),
        mission_id: request.mission_id.clone(),
        candidate_id: request.candidate_id.clone(),
        task_id: request.verify_task_id.clone(),
        run_id: request.verify_run_id.clone(),
        command_snapshot_ref,
        environment_ref,
        requirement_ids: request.requirement_ids.clone(),
        status,
        input_integrity,
        exit_code: raw.exit_code,
        log_ref: log_ref.clone(),
        started_at,
        ended_at: ended_at.clone(),
    };

    // The command may outlive many user/actor mutations. Retry only the
    // evidence transaction against fresh projections; never execute it again.
    loop {
        let result = (|| loop {
            let current = load_entities(&service.storage, request.mission_id)?;
            let Some(mut next_run) = current.runs.iter().find(|r| r.id == run.id).cloned() else {
                return Err(state_error(
                    "verify_run_missing",
                    "verify run disappeared".into(),
                ));
            };
            if next_run.fencing_token != run.fencing_token || next_run.state.is_terminal() {
                return Err(state_error(
                    "verify_fence_changed",
                    "verify run ownership changed".into(),
                ));
            }
            let mut next_task = current
                .tasks
                .iter()
                .find(|t| t.id == task.id)
                .cloned()
                .ok_or_else(|| {
                    state_error("verify_task_missing", "verify task disappeared".into())
                })?;
            let mut evidence = verification.clone();
            let cancelled = cancel.load(Ordering::Acquire)
                || next_run.state == RunState::Stopping
                || next_task.state == TaskState::Cancelled
                || matches!(
                    current.mission.state,
                    MissionState::Stopping | MissionState::Cancelled
                )
                || current.mission.candidate_id.as_ref() != Some(request.candidate_id);
            if cancelled {
                evidence.status = VerificationStatus::Cancelled;
            }
            let passed = evidence.status == VerificationStatus::Passed;
            next_task.state = if cancelled {
                TaskState::Cancelled
            } else if passed {
                TaskState::Succeeded
            } else {
                TaskState::Failed
            };
            next_task.active_run_id = None;
            next_task.updated_at = ended_at.clone();
            next_run.state = if cancelled {
                RunState::Cancelled
            } else if passed {
                RunState::Succeeded
            } else {
                RunState::Failed
            };
            next_run.ended_at = Some(ended_at.clone());
            next_run.result_ref = Some(log_ref.clone());
            let mut next_mission = current.mission.clone();
            let required: HashSet<&Id> = current
                .mission
                .requirements
                .iter()
                .flat_map(|r| &r.verification_ids)
                .collect();
            let all_passed = required.iter().all(|command| {
                (passed && task.contract.verification_ids.contains(command))
                    || current.verifications.iter().any(|v| {
                        v.candidate_id == *request.candidate_id
                            && v.status == VerificationStatus::Passed
                            && current.tasks.iter().any(|t| {
                                t.id == v.task_id && t.contract.verification_ids.contains(command)
                            })
                    })
            });
            let all_tasks = current
                .tasks
                .iter()
                .filter(|t| t.kind == TaskKind::Verify && t.id != task.id)
                .all(|t| t.state.is_terminal());
            if passed && all_passed && all_tasks && next_mission.phase == Phase::Validating {
                next_mission.phase = Phase::Reviewing;
            }
            let mut updates = Vec::new();
            for intent in service
                .storage
                .mission_outbox()
                .map_err(MissionService::store_error)?
                .into_iter()
                .filter(|i| {
                    i.run_id.as_ref() == Some(&run.id)
                        && i.state == term_storage::mission::types::OutboxState::Sending
                })
            {
                use term_storage::mission::types::{OutboxOperation, OutboxState, OutboxUpdate};
                if matches!(
                    intent.operation,
                    OutboxOperation::Verify | OutboxOperation::Cancel
                ) {
                    updates.push(OutboxUpdate {
                        id: intent.id,
                        expected_state: OutboxState::Sending,
                        state: OutboxState::Acknowledged,
                        fencing_token: intent.fencing_token,
                    });
                }
            }
            let mut upserts = vec![
                Entity::Verification(Box::new(evidence.clone())),
                Entity::Task(Box::new(next_task)),
                Entity::Run(Box::new(next_run.clone())),
            ];
            if let Some(workspace_id) = &next_run.workspace_id {
                if let Some(mut workspace) = service
                    .storage
                    .mission_snapshot(request.mission_id)
                    .map_err(MissionService::store_error)?
                    .and_then(|s| {
                        s.entities.into_iter().find_map(|e| match e {
                            Entity::Workspace(w) if &w.id == workspace_id => Some(*w),
                            _ => None,
                        })
                    })
                {
                    workspace.state = WorkspaceState::Retained;
                    workspace.writer_run_id = None;
                    upserts.push(Entity::Workspace(Box::new(workspace)));
                }
            }
            match service.commit_actor(next_mission, "engine.verify", upserts, updates) {
                Err(e) if e.code == MissionErrorCode::RevisionConflict => continue,
                Err(e) => return Err(e),
                Ok(()) => {
                    return Ok(VerificationRunResult {
                        verification: evidence,
                        timed_out: raw.timed_out,
                    })
                }
            }
        })();
        match result {
            Err(error) if error.code == MissionErrorCode::StorageUnavailable => {
                std::thread::sleep(Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

/// Raw child-process observation (bounded, tree-killed on timeout).
pub(super) struct RawRun {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_total: u64,
    pub stderr_total: u64,
}

fn execute_command(
    command: &VerificationCommand,
    cwd: &Path,
    cancel: &AtomicBool,
) -> Result<RawRun, MissionRpcError> {
    let mut child = spawn_command(&command.program, &command.argv, cwd).map_err(|error| {
        MissionRpcError::new(
            MissionErrorCode::InvalidArgument,
            format!("spawn {:?}: {error}", command.program),
        )
    })?;
    let pid = child.id();
    let stdout_spool = Arc::new(Mutex::new(BoundedSpool::new(DEFAULT_SPOOL_BYTES)));
    let stderr_spool = Arc::new(Mutex::new(BoundedSpool::new(DEFAULT_SPOOL_BYTES)));
    let stdout_tap = tap_stream(child.stdout.take(), Arc::clone(&stdout_spool));
    let stderr_tap = tap_stream(child.stderr.take(), Arc::clone(&stderr_spool));

    // Poll the owned child, with no detached delayed PID kill. Terminate
    // descendants even when the root exited but a child still owns a pipe.
    let started = std::time::Instant::now();
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                kill_tree(pid);
                break status;
            }
            Ok(None) => {}
            Err(e) => {
                kill_tree(pid);
                let _ = child.wait();
                return Err(MissionRpcError::new(
                    MissionErrorCode::Internal,
                    format!("verification child wait: {e}"),
                ));
            }
        }
        cancelled = cancel.load(Ordering::Acquire);
        timed_out = started.elapsed() >= Duration::from_millis(command.timeout_ms);
        if cancelled || timed_out {
            kill_tree(pid);
            break child
                .wait()
                .map_err(|e| MissionRpcError::new(MissionErrorCode::Internal, e.to_string()))?;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if let Some(handle) = stdout_tap {
        let _ = handle.join();
    }
    if let Some(handle) = stderr_tap {
        let _ = handle.join();
    }
    let (stdout_tail, stdout_total) = {
        let mut spool = stdout_spool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (spool.take_tail(), spool.total_pushed())
    };
    let (stderr_tail, stderr_total) = {
        let mut spool = stderr_spool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (spool.take_tail(), spool.total_pushed())
    };
    Ok(RawRun {
        exit_code: status.code(),
        timed_out,
        cancelled,
        stdout_tail,
        stderr_tail,
        stdout_total,
        stderr_total,
    })
}

/// Spawn with explicit argv and piped output — never a shell string (03 §1).
/// Bare names that fail CreateProcess fall back to `cmd /C` once, matching
/// the binding probe (npm `.cmd` shims on Windows).
fn spawn_command(
    program: &str,
    argv: &[String],
    cwd: &Path,
) -> std::io::Result<std::process::Child> {
    let mut direct = Command::new(program);
    direct
        .args(argv)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group so the watchdog can signal the whole tree.
        direct.process_group(0);
    }
    match direct.spawn() {
        Ok(child) => Ok(child),
        Err(error) => {
            if program.contains('/') || program.contains('\\') {
                return Err(error);
            }
            #[cfg(not(windows))]
            return Err(error);
            #[cfg(windows)]
            {
                let mut shim = Command::new("cmd");
                shim.arg("/C")
                    .arg(program)
                    .args(argv)
                    .current_dir(cwd)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                shim.spawn().map_err(|_| error)
            }
        }
    }
}

/// Drain one pipe into a bounded spool until EOF.
fn tap_stream<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
    spool: Arc<Mutex<BoundedSpool>>,
) -> Option<JoinHandle<()>> {
    let mut pipe = pipe?;
    Some(std::thread::spawn(move || {
        let mut buffer = [0u8; 8 * 1024];
        loop {
            match std::io::Read::read(&mut pipe, &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let mut guard = spool
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.push_line(&buffer[..read]);
                }
            }
        }
    }))
}

#[cfg(windows)]
fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn kill_tree(pid: u32) {
    // The child owns this group (spawn_command). Avoid the external kill
    // utility: procps can parse an unseparated negative PID as options and
    // signal -1. Also reject sentinel/overflow values before negating.
    let Ok(group) = i32::try_from(pid) else {
        return;
    };
    if group <= 1 {
        return;
    }
    extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SIGKILL = 9 on the supported Unix platforms. No pointer arguments.
    unsafe {
        kill(-group, 9);
    }
}

#[cfg(all(test, unix))]
mod command_stop_tests {
    use super::kill_tree;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::Command;

    #[test]
    fn verification_cleanup_signals_only_its_owned_group() {
        let mut owned = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let mut independent = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        kill_tree(owned.id());
        let result = owned.wait().unwrap();
        let other = independent.try_wait().unwrap();
        let _ = independent.kill();
        let _ = independent.wait();
        assert_eq!(result.signal(), Some(9));
        assert!(other.is_none(), "unrelated execution must remain alive");
    }
}

// ---- 3. review findings (04 §6) ---------------------------------------------

/// Mint typed Finding entities from reviewer drafts (03 §2: providers
/// propose, the daemon resolves every reference and owns the ids). Findings
/// attach to the *current* candidate only — STALE_CANDIDATE otherwise.
pub fn record_findings(
    service: &MissionService,
    artifacts: &ArtifactStore,
    mission_id: &Id,
    reviewer_run_id: &Id,
    candidate_id: &Id,
    drafts: &[ProviderFindingDraft],
) -> Result<Vec<Finding>, MissionRpcError> {
    let mission = service.read_mission(mission_id)?;
    let entities = load_entities(&service.storage, mission_id)?;
    let run = entities
        .runs
        .iter()
        .find(|run| run.id == *reviewer_run_id)
        .ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::NotFound,
                format!("reviewer run {reviewer_run_id} not found"),
            )
        })?;
    let task = entities
        .tasks
        .iter()
        .find(|task| task.id == run.task_id)
        .ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::Internal,
                "reviewer run has no task projection",
            )
        })?;
    if task.kind != TaskKind::Review {
        return Err(state_error(
            "findings_require_review_run",
            "findings may only attach to review runs".into(),
        ));
    }
    if mission.candidate_id.as_ref() != Some(candidate_id) {
        return Err(MissionRpcError::new(
            MissionErrorCode::StaleCandidate,
            "findings must target the mission's current candidate",
        ));
    }
    let mut findings = Vec::new();
    for draft in drafts {
        let evidence_ref = store_artifact(
            artifacts,
            mission_id,
            "text/plain",
            draft.evidence_text.as_bytes(),
        )?;
        findings.push(Finding {
            id: Id::generate(),
            mission_id: mission_id.clone(),
            candidate_id: candidate_id.clone(),
            reviewer_run_id: reviewer_run_id.clone(),
            severity: draft.severity,
            path: draft.path.clone(),
            line: draft.line,
            evidence_ref,
            requirement_id: draft.requirement_id.clone(),
            resolution: FindingResolution::Open,
            resolution_ref: None,
        });
    }
    {
        let mut upserts: Vec<Entity> = findings
            .iter()
            .cloned()
            .map(|finding| Entity::Finding(Box::new(finding)))
            .collect();
        // A successful review needs candidate-bound evidence even when the
        // reviewer found no issues. A succeeded task alone proves nothing
        // about which version was inspected.
        if run.result_ref.is_none() {
            let report_ref = store_artifact(
                artifacts,
                mission_id,
                "text/plain",
                b"Independent review findings recorded",
            )?;
            let result = term_contracts::mission::types::AgentResult::Review {
                candidate_id: candidate_id.clone(),
                findings: findings.clone(),
                report_ref,
            };
            let mut next_run = run.clone();
            next_run.result_ref = Some(store_artifact(
                artifacts,
                mission_id,
                "application/json",
                &serde_json::to_vec(&result).expect("review result"),
            )?);
            upserts.push(Entity::Run(Box::new(next_run)));
        }
        let mut next = mission.clone();
        next.phase = Phase::Reviewing;
        commit_upserts(
            service,
            next,
            "engine.review",
            &format!("{mission_id}:{reviewer_run_id}:{}", findings.len()),
            MissionEventType::Changed,
            upserts,
        )?;
    }
    Ok(findings)
}

// ---- 4. the accept gate (04 §6 steps 1–7) ------------------------------------

/// Everything the pure gate needs, projected from one snapshot.
pub struct AcceptanceInputs<'a> {
    pub mission: &'a Mission,
    pub offered_revision: u64,
    pub offered_candidate_id: &'a Id,
    pub tasks: &'a [Task],
    pub runs: &'a [Run],
    pub verifications: &'a [Verification],
    pub findings: &'a [Finding],
    pub decisions: &'a [Decision],
    pub acknowledged_verification_ids: &'a [Id],
    pub human_requirement_ids: &'a [Id],
    /// Only IDs whose termination and explicit replacement were verified by the service.
    pub verified_reconciled_run_ids: &'a [Id],
    /// A Review-kind task reached `succeeded` on the current plan.
    pub review_complete: bool,
}

/// Why mission.accept was refused, in 04 §6 step order.
#[derive(Debug, Clone, PartialEq)]
pub enum Rejection {
    RevisionMismatch {
        current: u64,
        offered: u64,
    },
    CandidateMismatch {
        current: Option<Id>,
        offered: Id,
    },
    RequiredTaskNotSucceeded {
        task_id: Id,
        state: TaskState,
    },
    VerificationMissing {
        requirement_id: Id,
        verification_id: Id,
    },
    VerificationNotPassedOnCandidate {
        requirement_id: Id,
        verification_id: Id,
        status: VerificationStatus,
        verified_candidate: Option<Id>,
    },
    IntegrityPolicyUnmet {
        verification_id: Id,
    },
    ObservedNotAcknowledged {
        verification_id: Id,
    },
    ReviewIncomplete,
    OpenFinding {
        finding_id: Id,
        severity: FindingSeverity,
    },
    HumanCheckMissing {
        requirement_id: Id,
    },
    LiveRun {
        run_id: Id,
    },
    UnknownRun {
        run_id: Id,
    },
    OpenBlockingDecision {
        decision_id: Id,
    },
}

impl Rejection {
    pub fn to_error(&self) -> MissionRpcError {
        match self {
            Rejection::RevisionMismatch { current, offered } => MissionRpcError::with_details(
                MissionErrorCode::RevisionConflict,
                format!(
                    "mission revision is {current}, accept offered {offered}; resync and retry"
                ),
                MissionErrorDetails {
                    current_revision: Some(U64String::new(*current).expect("fits SQLite bound")),
                    reason_code: Some("revision_mismatch".into()),
                    ..Default::default()
                },
            ),
            Rejection::CandidateMismatch { current, offered } => MissionRpcError::with_details(
                MissionErrorCode::StaleCandidate,
                format!(
                    "mission candidate is {}, accept offered {offered}",
                    current
                        .as_ref()
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "unset".to_string())
                ),
                MissionErrorDetails {
                    reason_code: Some("candidate_mismatch".into()),
                    ..Default::default()
                },
            ),
            Rejection::RequiredTaskNotSucceeded { task_id, state } => state_error(
                "required_task_not_succeeded",
                format!("required task {task_id} is {state:?}, not succeeded"),
            ),
            Rejection::VerificationMissing {
                requirement_id,
                verification_id,
            } => state_error(
                "verification_missing",
                format!(
                    "requirement {requirement_id} has no verification for command \
                     {verification_id} on the current candidate"
                ),
            ),
            Rejection::VerificationNotPassedOnCandidate {
                requirement_id,
                verification_id,
                status,
                ..
            } => state_error(
                "verification_not_passed_on_candidate",
                format!(
                    "requirement {requirement_id}: command {verification_id} is {status:?} \
                     on the current candidate"
                ),
            ),
            Rejection::IntegrityPolicyUnmet { verification_id } => MissionRpcError::with_details(
                MissionErrorCode::PolicyDenied,
                format!(
                    "verification {verification_id} does not satisfy the integrity policy \
                     (strict requires enforced; unknown never passes)"
                ),
                MissionErrorDetails {
                    reason_code: Some("integrity_policy_unmet".into()),
                    ..Default::default()
                },
            ),
            Rejection::ObservedNotAcknowledged { verification_id } => {
                MissionRpcError::with_details(
                    MissionErrorCode::PolicyDenied,
                    format!(
                        "verification {verification_id} is observed (not enforced) and was not \
                     explicitly acknowledged; its limits must be confirmed before acceptance"
                    ),
                    MissionErrorDetails {
                        reason_code: Some("observed_not_acknowledged".into()),
                        ..Default::default()
                    },
                )
            }
            Rejection::ReviewIncomplete => state_error(
                "review_incomplete",
                "independent review is required but no review task has succeeded".into(),
            ),
            Rejection::OpenFinding {
                finding_id,
                severity,
            } => state_error(
                "open_finding",
                format!("open {severity:?} finding {finding_id} blocks acceptance"),
            ),
            Rejection::HumanCheckMissing { requirement_id } => state_error(
                "human_check_missing",
                format!("human check for requirement {requirement_id} is not confirmed"),
            ),
            Rejection::LiveRun { run_id } => {
                state_error("live_run", format!("run {run_id} is still live"))
            }
            Rejection::UnknownRun { run_id } => state_error(
                "unknown_run",
                format!("run {run_id} has an unknown outcome"),
            ),
            Rejection::OpenBlockingDecision { decision_id } => state_error(
                "open_blocking_decision",
                format!("blocking decision {decision_id} is still open"),
            ),
        }
    }
}

/// The 04 §6 accept predicate, steps 1–7 in order. Pure: no I/O, no clock,
/// no storage — the caller feeds one snapshot and applies step 8 itself.
pub fn acceptance_ready(inputs: &AcceptanceInputs<'_>) -> Result<(), Rejection> {
    let mission = inputs.mission;

    // 1. Expected revision/candidate id match the current state (W10). Only
    //    housekeeping commits (time/activity) may follow the offered revision.
    if !super::timing::revision_accepts(mission, inputs.offered_revision) {
        return Err(Rejection::RevisionMismatch {
            current: mission.revision.get(),
            offered: inputs.offered_revision,
        });
    }
    let Some(current_candidate) = mission.candidate_id.as_ref() else {
        return Err(Rejection::CandidateMismatch {
            current: None,
            offered: inputs.offered_candidate_id.clone(),
        });
    };
    if current_candidate != inputs.offered_candidate_id {
        return Err(Rejection::CandidateMismatch {
            current: Some(current_candidate.clone()),
            offered: inputs.offered_candidate_id.clone(),
        });
    }

    // 2. Only a validated replacement can retire a required contract.
    // Cancelling its execution is not permission to waive that contract.
    for task in inputs.tasks {
        if !task.required || task.state == TaskState::Superseded {
            continue;
        }
        if task.state != TaskState::Succeeded {
            return Err(Rejection::RequiredTaskNotSucceeded {
                task_id: task.id.clone(),
                state: task.state,
            });
        }
    }

    // 3. Every required command passed on the CURRENT candidate (W05: old
    //    candidates' evidence is never reused). Command coverage rides on
    //    each verify task's contract snapshot.
    let tasks_by_id: HashMap<&Id, &Task> =
        inputs.tasks.iter().map(|task| (&task.id, task)).collect();
    let mut passed_for_command: HashMap<&str, &Verification> = HashMap::new();
    let mut seen_for_command: HashMap<&str, &Verification> = HashMap::new();
    for verification in inputs.verifications {
        if Some(&verification.candidate_id) != mission.candidate_id.as_ref() {
            continue; // evidence from another candidate does not count
        }
        let Some(task) = tasks_by_id.get(&verification.task_id) else {
            continue;
        };
        for command in &task.contract.verification_ids {
            seen_for_command
                .entry(command.as_str())
                .or_insert(verification);
            if verification.status == VerificationStatus::Passed {
                passed_for_command
                    .entry(command.as_str())
                    .or_insert(verification);
            }
        }
    }
    for requirement in &mission.requirements {
        for command in &requirement.verification_ids {
            match (
                seen_for_command.get(command.as_str()),
                passed_for_command.get(command.as_str()),
            ) {
                (None, _) => {
                    return Err(Rejection::VerificationMissing {
                        requirement_id: requirement.id.clone(),
                        verification_id: command.clone(),
                    })
                }
                (Some(seen), None) => {
                    return Err(Rejection::VerificationNotPassedOnCandidate {
                        requirement_id: requirement.id.clone(),
                        verification_id: command.clone(),
                        status: seen.status,
                        verified_candidate: Some(seen.candidate_id.clone()),
                    })
                }
                (_, Some(_)) => {}
            }
        }
    }

    // 4. Integrity policy over the passed verifications (W09): strict needs
    //    enforced; normal needs an explicit acknowledgment for observed;
    //    unknown never passes (W07).
    let required_commands: HashSet<&str> = mission
        .requirements
        .iter()
        .flat_map(|requirement| requirement.verification_ids.iter().map(Id::as_str))
        .collect();
    for command in required_commands {
        let Some(verification) = passed_for_command.get(command) else {
            continue; // step 3 already rejected missing commands
        };
        match verification.input_integrity {
            InputIntegrity::Enforced => {}
            InputIntegrity::Observed => {
                if mission.policy.require_enforced_verification {
                    return Err(Rejection::IntegrityPolicyUnmet {
                        verification_id: verification.id.clone(),
                    });
                }
                if !inputs
                    .acknowledged_verification_ids
                    .contains(&verification.id)
                {
                    return Err(Rejection::ObservedNotAcknowledged {
                        verification_id: verification.id.clone(),
                    });
                }
            }
            InputIntegrity::Unknown => {
                return Err(Rejection::IntegrityPolicyUnmet {
                    verification_id: verification.id.clone(),
                });
            }
        }
    }

    // 5. Independent review complete; zero open blocking/major findings (W08).
    if mission.policy.require_independent_review && !inputs.review_complete {
        return Err(Rejection::ReviewIncomplete);
    }
    for finding in inputs.findings {
        if Some(&finding.candidate_id) == mission.candidate_id.as_ref()
            && finding.resolution == FindingResolution::Open
            && matches!(
                finding.severity,
                FindingSeverity::Blocking | FindingSeverity::Major
            )
        {
            return Err(Rejection::OpenFinding {
                finding_id: finding.id.clone(),
                severity: finding.severity,
            });
        }
    }

    // 6. Human-check requirements are all confirmed by the user.
    for requirement in &mission.requirements {
        if requirement.human_check && !inputs.human_requirement_ids.contains(&requirement.id) {
            return Err(Rejection::HumanCheckMissing {
                requirement_id: requirement.id.clone(),
            });
        }
    }

    // 7. No live writers, no unknown runs, no open blocking decisions.
    for run in inputs.runs {
        if run.state.is_live() {
            return Err(Rejection::LiveRun {
                run_id: run.id.clone(),
            });
        }
        if matches!(run.state, RunState::Unknown | RunState::Interrupted)
            && !inputs.verified_reconciled_run_ids.contains(&run.id)
        {
            return Err(Rejection::UnknownRun {
                run_id: run.id.clone(),
            });
        }
    }
    for decision in inputs.decisions {
        if decision.state == term_contracts::mission::types::DecisionState::Open
            && decision.blocking
        {
            return Err(Rejection::OpenBlockingDecision {
                decision_id: decision.id.clone(),
            });
        }
    }
    Ok(())
}

pub(super) fn current_review_complete(
    service: &MissionService,
    snapshot: &MissionEntities,
) -> Result<bool, MissionRpcError> {
    let Some(candidate) = snapshot
        .candidates
        .iter()
        .find(|c| Some(&c.id) == snapshot.mission.candidate_id.as_ref())
    else {
        return Ok(false);
    };
    for task in snapshot
        .tasks
        .iter()
        .filter(|t| t.kind == TaskKind::Review && t.state == TaskState::Succeeded)
    {
        if review_task_complete(service, snapshot, task, candidate)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn review_task_complete(
    service: &MissionService,
    snapshot: &MissionEntities,
    task: &Task,
    candidate: &Candidate,
) -> Result<bool, MissionRpcError> {
    if task.kind != TaskKind::Review || task.state != TaskState::Succeeded {
        return Ok(false);
    }
    for run in snapshot.runs.iter().filter(|r| {
        r.task_id == task.id
            && r.state == RunState::Succeeded
            && !candidate.source_run_ids.contains(&r.id)
    }) {
        let Some(reference) = &run.result_ref else {
            continue;
        };
        let bytes = service
            .artifacts
            .read_mission_body(
                &snapshot.mission.id,
                reference,
                service.limits.max_context_bytes,
            )
            .map_err(|(code, message)| MissionRpcError::new(code, message))?;
        if matches!(serde_json::from_slice::<AgentResult>(&bytes), Ok(AgentResult::Review { candidate_id, .. }) if candidate_id == candidate.id)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// mission.accept handler (04 §6 step 8): gate first, then accepted_at +
/// completed + Accepted event in exactly one transaction. Registered by the
/// service router; also usable directly by the engine in tests.
pub fn apply_accept(service: &MissionService, params: &Value) -> Result<Handled, MissionRpcError> {
    let params: MissionAcceptParams = serde_json::from_value(params.clone()).map_err(|error| {
        MissionRpcError::new(
            MissionErrorCode::InvalidArgument,
            format!("params mismatch: {error}"),
        )
    })?;
    let mission = service.read_mission(&params.mission_id)?;
    if mission.state != MissionState::Running {
        return Err(state_error(
            "not_awaiting_acceptance",
            "only a running mission can be accepted".into(),
        ));
    }
    let entities = load_entities(&service.storage, &params.mission_id)?;
    service.validate_exclusion_replacements(
        &entities,
        None,
        super::integration_exclusion::ExclusionCheck::Accepting,
    )?;
    let review_complete = current_review_complete(service, &entities)?;
    let acknowledged_runs = params
        .acknowledged_reconciled_run_ids
        .as_deref()
        .unwrap_or(&[]);
    let reconciliations = service.reviewed_reconciliations(&entities, acknowledged_runs)?;
    let inputs = AcceptanceInputs {
        mission: &mission,
        offered_revision: params.expected_revision.get(),
        offered_candidate_id: &params.candidate_id,
        tasks: &entities.tasks,
        runs: &entities.runs,
        verifications: &entities.verifications,
        findings: &entities.findings,
        decisions: &entities.decisions,
        acknowledged_verification_ids: &params.acknowledged_verification_ids,
        human_requirement_ids: &params.human_requirement_ids,
        verified_reconciled_run_ids: acknowledged_runs,
        review_complete,
    };
    acceptance_ready(&inputs).map_err(|rejection| rejection.to_error())?;

    let review_ref = if reconciliations.is_empty() {
        None
    } else {
        Some(store_artifact(&service.artifacts,
        &mission.id, "application/json", &serde_json::to_vec(&json!({"kind": "acceptance_reconciliation_review", "version": 1,
            "mission_id": mission.id, "candidate_id": params.candidate_id, "reconciliations": reconciliations})).expect("acceptance evidence"))?)
    };

    let timestamp = now();
    let mut next = mission.clone();
    next.state = MissionState::Completed;
    next.phase = Phase::Done;
    next.accepted_at = Some(timestamp.clone());
    next.revision = next_revision(&mission);
    next.updated_at = timestamp.clone();
    let transition = ApplyMissionTransition {
        request_id: params.request_id.clone(),
        method: methods::MISSION_ACCEPT.to_string(),
        fingerprint: fingerprint_of_params(methods::MISSION_ACCEPT, &params),
        mission_id: params.mission_id.clone(),
        mode: ApplyMode::Mutate {
            expected_revision: params.expected_revision.get(),
        },
        transaction_id: Id::generate(),
        event_type: MissionEventType::Accepted,
        upserts: vec![Entity::Mission(Box::new(next))],
        deletes: Vec::new(),
        changes_ref: review_ref,
        outbox: Vec::new(),
        outbox_updates: Vec::new(),
        adopt_staged_artifacts: Vec::new(),
        created_at: timestamp,
    };
    let applied = service
        .apply_timed_transition(transition)
        .map_err(MissionService::store_error)?;
    Ok(Handled {
        result: serde_json::to_value(&applied.result).unwrap_or(Value::Null),
        notify: Some((params.mission_id.clone(), applied.result.revision.get())),
    })
}

/// SHA-256 over method + canonical params (request_id excluded) — mirrors
/// `MissionService::fingerprint` so retries replay the stored response.
fn fingerprint_of_params(method: &str, params: &MissionAcceptParams) -> String {
    MissionService::fingerprint(
        method,
        &serde_json::to_value(params).expect("serializable acceptance"),
    )
}
