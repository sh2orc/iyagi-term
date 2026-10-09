//! Deterministic integration (ticket O06, spec 04 §4): apply source
//! candidates to a fresh integration worktree at the mission base, in plan
//! topological order (ties by task ordinal — the caller passes the order).
//! Conflicts stop the sequence and record the partial state for the
//! integrator role (invoked in O13); no implicit "ours" choice is ever made.

use std::{collections::HashSet, path::Path};

use term_contracts::mission::types::Id;

use super::git::{self, candidate_ref, commit_on_private_ref, GitError};

#[derive(Debug, thiserror::Error)]
pub enum IntegrationError {
    #[error("git: {0}")]
    Git(#[from] GitError),
    #[error("conflict applying candidate {candidate} (paths: {paths:?})")]
    Conflict { candidate: Id, paths: Vec<String> },
    #[error("integration produced no changes")]
    Empty,
    #[error("invalid integration input: {0}")]
    InvalidInput(String),
}

/// Frozen candidate identity, also retained in the applied-source evidence.
/// Private refs retain objects for GC; they never select integration content.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationSource {
    pub candidate_id: Id,
    pub source_run_ids: Vec<Id>,
    pub base_oid: String,
    pub commit_oid: String,
    pub tree_oid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationOutcome {
    pub sources: Vec<IntegrationSource>,
    /// None while no conflict happened; Some describes the conflicting
    /// candidate and the paths it collided on.
    pub conflict: Option<(Id, Vec<String>)>,
}

/// Check every frozen input without modifying worktrees, refs, or the index.
/// Mission callers also run this before creating the integration worktree.
pub fn validate_sources(
    repository: &Path,
    candidates: &[IntegrationSource],
    base_oid: &str,
) -> Result<(), IntegrationError> {
    git::commit_tree_oid(repository, base_oid)?;
    let mut ids = HashSet::new();
    for source in candidates {
        if !ids.insert(&source.candidate_id) {
            return Err(IntegrationError::InvalidInput("duplicate candidate".into()));
        }
        git::validate_oid(&source.tree_oid)?;
        git::commit_tree_oid(repository, &source.base_oid)?;
        if git::commit_tree_oid(repository, &source.commit_oid)? != source.tree_oid {
            return Err(IntegrationError::InvalidInput(format!(
                "candidate {} has a different tree",
                source.candidate_id
            )));
        }
    }
    Ok(())
}

/// Apply frozen candidates in the caller's plan order to a fresh worktree.
/// Repair patches use their own input base. A conflict stops the sequence
/// with partial evidence and no minted candidate (04 §4).
pub fn integrate(
    repository: &Path,
    integration_worktree: &Path,
    mission_id: &Id,
    candidates: &[IntegrationSource],
    base_oid: &str,
) -> Result<(IntegrationOutcome, Option<IntegratedCandidate>), IntegrationError> {
    // Check the whole input before applying the first patch. A bad later
    // source must not leave a misleading, partially applied conflict.
    validate_sources(repository, candidates, base_oid)?;
    git::ensure_detached_head(integration_worktree)?;
    git::ensure_clean(integration_worktree)?;
    if git::rev_parse(integration_worktree, "HEAD")? != base_oid {
        return Err(IntegrationError::InvalidInput(
            "integration workspace is not at the frozen mission base".into(),
        ));
    }
    // Research-only missions still bind review and acceptance to immutable
    // Git input. Their candidate is the original base, with no patch.
    if candidates.is_empty() {
        return Ok((
            IntegrationOutcome {
                sources: vec![],
                conflict: None,
            },
            Some(IntegratedCandidate {
                candidate_id: Id::generate(),
                commit_oid: base_oid.to_string(),
                tree_oid: git::rev_parse(integration_worktree, &format!("{base_oid}^{{tree}}"))?,
                sources: vec![],
                base_oid: base_oid.to_string(),
            }),
        ));
    }
    apply_remaining(integration_worktree, mission_id, candidates, base_oid, 0)
}

/// Continue after a separately owned integrator Run and a validated capture.
pub fn continue_integration(
    repository: &Path,
    integration_worktree: &Path,
    mission_id: &Id,
    candidates: &[IntegrationSource],
    base_oid: &str,
    applied_count: usize,
) -> Result<(IntegrationOutcome, Option<IntegratedCandidate>), IntegrationError> {
    validate_sources(repository, candidates, base_oid)?;
    git::ensure_detached_head(integration_worktree)?;
    git::ensure_clean(integration_worktree)?;
    if applied_count == 0 || applied_count > candidates.len() {
        return Err(IntegrationError::InvalidInput(
            "invalid resolved source position".into(),
        ));
    }
    apply_remaining(
        integration_worktree,
        mission_id,
        candidates,
        base_oid,
        applied_count,
    )
}

fn apply_remaining(
    integration_worktree: &Path,
    mission_id: &Id,
    candidates: &[IntegrationSource],
    base_oid: &str,
    applied_count: usize,
) -> Result<(IntegrationOutcome, Option<IntegratedCandidate>), IntegrationError> {
    let mut applied = candidates[..applied_count].to_vec();
    for source in &candidates[applied_count..] {
        let patch = git::diff_commit(integration_worktree, &source.base_oid, &source.commit_oid)?;
        let outcome = git::apply_three_way(integration_worktree, &patch)?;
        if !outcome.ok {
            return Ok((
                IntegrationOutcome {
                    sources: applied,
                    conflict: Some((source.candidate_id.clone(), outcome.conflict_paths)),
                },
                None,
            ));
        }
        applied.push(source.clone());
    }
    let clean = git::status_entries(integration_worktree)?.is_empty();
    if clean && git::rev_parse(integration_worktree, "HEAD")? == base_oid {
        return Err(IntegrationError::Empty);
    }
    // All sources applied cleanly: mint the integrated candidate.
    let integrated_id = Id::generate();
    let reference = candidate_ref(mission_id.as_str(), integrated_id.as_str());
    let (commit_oid, tree_oid) = if clean {
        git::retain_head_on_private_ref(integration_worktree, &reference)?
    } else {
        commit_on_private_ref(
            integration_worktree,
            &reference,
            &format!("iyagi integration {integrated_id}"),
        )?
    };
    let candidate = IntegratedCandidate {
        candidate_id: integrated_id,
        commit_oid,
        tree_oid,
        sources: applied,
        base_oid: base_oid.to_string(),
    };
    Ok((
        IntegrationOutcome {
            sources: candidate.sources.clone(),
            conflict: None,
        },
        Some(candidate),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegratedCandidate {
    pub candidate_id: Id,
    pub commit_oid: String,
    pub tree_oid: String,
    pub sources: Vec<IntegrationSource>,
    pub base_oid: String,
}
