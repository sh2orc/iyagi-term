//! Change capture (ticket O06, spec 04 §3): the model's patch result is a
//! claim — this module reads the actual Git state of a writer worktree,
//! polices the allowed-path scope, and mints immutable candidates on private
//! refs. It never reverts user files.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use term_contracts::mission::types::Id;

use super::git::{
    candidate_ref, commit_on_private_ref, diff_names, status_entries, ChangeKind, GitError,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    pub change: String, // added | modified | deleted
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub base_oid: String,
    pub entries: Vec<ManifestEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("git: {0}")]
    Git(#[from] GitError),
    #[error("scope violation: {0} (workspace retained for review)")]
    ScopeViolation(String),
    #[error("unsupported change: {0}")]
    Unsupported(String),
    #[error("nothing changed — no candidate minted")]
    Empty,
}

/// A minted candidate: immutable, referenced by a private ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedCandidate {
    pub candidate_id: Id,
    pub mission_id: Id,
    pub base_oid: String,
    pub commit_oid: String,
    pub tree_oid: String,
    pub manifest: Manifest,
    pub source_run_ids: Vec<Id>,
}

/// Path scoping (02 §5 rule 8): repository-relative, no traversal, no .git.
/// `allowed` entries are exact files or `dir/` prefixes.
pub fn path_allowed(path: &str, allowed: &[String]) -> bool {
    let normalized = path.replace('\\', "/");
    if normalized.starts_with('/') || normalized.contains("..") || normalized.contains('\0') {
        return false;
    }
    if normalized == ".git" || normalized.starts_with(".git/") {
        return false;
    }
    allowed.iter().any(|scope| {
        if let Some(prefix) = scope.strip_suffix('/') {
            normalized == prefix || normalized.starts_with(&format!("{prefix}/"))
        } else {
            &normalized == scope
        }
    })
}

fn symlink_target_escapes(root: &Path, link_path: &Path) -> bool {
    // canonicalize follows the whole chain, including symlinks in parents.
    // A dangling target cannot provide containment evidence.
    let Ok(meta) = std::fs::symlink_metadata(link_path) else {
        return false;
    };
    if !meta.file_type().is_symlink() {
        return false;
    }
    match (root.canonicalize(), link_path.canonicalize()) {
        (Ok(root), Ok(target)) => !target.starts_with(root),
        _ => true,
    }
}

/// Capture the worktree's actual changes into a candidate (04 §3 steps
/// 1–6). Scope violations retain the workspace and preserve the diff.
pub fn capture(
    worktree: &Path,
    mission_id: &Id,
    source_run_ids: Vec<Id>,
    allowed_paths: &[String],
    base_oid: &str,
) -> Result<CapturedCandidate, CaptureError> {
    // 2. Policing: everything changed must be inside the allowed scope,
    //    .git untouched, no submodule/LFS surprises.
    let pending = status_entries(worktree)?;
    let mut entries = pending.clone();
    // A provider may commit in its private worktree. Scope validation must
    // also cover committed changes relative to the original task input.
    entries.extend(
        diff_names(worktree, base_oid, "HEAD")?
            .into_iter()
            .map(|entry| ('M', entry.path)),
    );
    if entries.is_empty() {
        return Err(CaptureError::Empty);
    }
    for (_code, path) in &entries {
        let normalized = path.replace('\\', "/");
        if normalized == ".git" || normalized.starts_with(".git/") {
            return Err(CaptureError::ScopeViolation(format!(
                "change inside .git: {normalized}"
            )));
        }
        if !path_allowed(&normalized, allowed_paths) {
            return Err(CaptureError::ScopeViolation(format!(
                "changed path {normalized:?} is outside the allowed scope"
            )));
        }
        let absolute = worktree.join(path);
        if std::fs::symlink_metadata(&absolute).is_ok_and(|m| m.is_dir()) {
            return Err(CaptureError::Unsupported(format!(
                "submodule or directory change: {normalized:?}"
            )));
        }
        if symlink_target_escapes(worktree, &absolute) {
            return Err(CaptureError::ScopeViolation(format!(
                "symlink {normalized:?} resolves outside the workspace"
            )));
        }
    }

    // 5. Commit on the private ref (user branches never move).
    let candidate_id = Id::generate();
    let reference = candidate_ref(mission_id.as_str(), candidate_id.as_str());
    let (commit_oid, tree_oid) = if pending.is_empty() {
        super::git::retain_head_on_private_ref(worktree, &reference)?
    } else {
        commit_on_private_ref(
            worktree,
            &reference,
            &format!("iyagi candidate {candidate_id}"),
        )?
    };

    // 4. The manifest documents the candidate itself: every path the commit
    //    changes relative to the base, with real sizes and hashes read from
    //    the worktree (deleted files carry zero).
    let committed = diff_names(worktree, base_oid, &commit_oid)?;
    if committed.is_empty() {
        return Err(CaptureError::Empty);
    }
    let mut manifest_entries = Vec::new();
    for entry in &committed {
        let normalized = entry.path.replace('\\', "/");
        let (bytes, sha256) = if entry.kind == ChangeKind::Deleted {
            (0, String::new())
        } else {
            let bytes = super::git::read_blob_bounded(
                worktree,
                &commit_oid,
                &entry.path,
                64 * 1024 * 1024,
            )?;
            let digest = format!("{:x}", Sha256::digest(&bytes));
            (bytes.len() as u64, digest)
        };
        manifest_entries.push(ManifestEntry {
            path: normalized,
            change: match entry.kind {
                ChangeKind::Added => "added",
                ChangeKind::Modified => "modified",
                ChangeKind::Deleted => "deleted",
            }
            .to_string(),
            bytes,
            sha256,
        });
    }

    Ok(CapturedCandidate {
        candidate_id,
        mission_id: mission_id.clone(),
        base_oid: base_oid.to_string(),
        commit_oid,
        tree_oid,
        manifest: Manifest {
            base_oid: base_oid.to_string(),
            entries: manifest_entries,
        },
        source_run_ids,
    })
}

/// Change kind from a diff entry (kept for the integration module).
pub fn change_kind_of(entry: &super::git::DiffEntry) -> &'static str {
    match entry.kind {
        ChangeKind::Added => "added",
        ChangeKind::Modified => "modified",
        ChangeKind::Deleted => "deleted",
    }
}

#[cfg(test)]
mod tests {
    use super::super::git;
    use super::*;

    fn repo_with_base(dir: &Path) -> String {
        git::run_git_for_test(dir, &["init", "-q"]);
        std::fs::write(dir.join("README.md"), "base\n").unwrap();
        git::run_git_for_test(dir, &["add", "-A"]);
        git::run_git_for_test(dir, &["commit", "-m", "base", "-q"]);
        git::run_git_for_test(dir, &["rev-parse", "HEAD"])
    }

    #[test]
    fn path_scoping() {
        let allowed = vec!["src/api/".to_string(), "README.md".to_string()];
        assert!(path_allowed("src/api/a.ts", &allowed));
        assert!(path_allowed("README.md", &allowed));
        assert!(!path_allowed("src/ui/a.ts", &allowed));
        assert!(!path_allowed("../outside", &allowed));
        assert!(!path_allowed(".git/config", &allowed));
    }

    #[test]
    fn capture_mints_candidate_on_private_ref() {
        let dir = tempfile::tempdir().unwrap();
        let base = repo_with_base(dir.path());
        git::run_git_for_test(dir.path(), &["checkout", "--detach", "-q"]);
        std::fs::create_dir_all(dir.path().join("src/api")).unwrap();
        std::fs::write(dir.path().join("src/api").join("route.ts"), "export {};\n").unwrap();
        let mission = Id::generate();
        let candidate = capture(
            dir.path(),
            &mission,
            vec![Id::generate()],
            &["src/api/".to_string()],
            &base,
        )
        .unwrap();
        assert_eq!(candidate.manifest.entries.len(), 1);
        assert_eq!(candidate.manifest.entries[0].path, "src/api/route.ts");
        assert_eq!(candidate.commit_oid.len(), 40);
        // The private ref exists and points at the candidate commit.
        let reference = candidate_ref(mission.as_str(), candidate.candidate_id.as_str());
        let pointed = git::run_git_for_test(dir.path(), &["rev-parse", &reference]);
        assert_eq!(pointed, candidate.commit_oid);
    }

    #[test]
    fn scope_violation_retains_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let base = repo_with_base(dir.path());
        std::fs::write(dir.path().join("outside.txt"), "leak").unwrap();
        let mission = Id::generate();
        let error = capture(
            dir.path(),
            &mission,
            vec![],
            &["src/api/".to_string()],
            &base,
        )
        .unwrap_err();
        assert!(matches!(error, CaptureError::ScopeViolation(_)));
        // The violating file is still there — never auto-reverted (W04).
        assert!(dir.path().join("outside.txt").exists());
    }
}
