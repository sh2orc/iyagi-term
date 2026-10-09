//! Mission workspaces (ticket O06): Git-verified repositories, detached
//! worktrees, exclusive writer leases, scope-policed change capture, and
//! deterministic integration (spec docs/orchestration/04-workspaces.md).

pub mod capture;
pub mod git;
pub mod integration;
pub mod leases;
pub mod role_files;
pub(crate) mod verification;

pub use capture::{capture, CaptureError, CapturedCandidate, Manifest, ManifestEntry};
pub use git::{
    add_detached_worktree, ensure_clean, repository_identity, snapshot_working_tree, GitError,
    RepositoryIdentity, WorkingTreeSnapshot,
};
pub use integration::{integrate, IntegratedCandidate, IntegrationError, IntegrationOutcome};
pub use leases::{Lease, LeaseError, WorkspaceLeases};
