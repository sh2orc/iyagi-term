//! Exclusive writer leases for mission workspaces (ticket O06, spec 04 §2):
//! one writer run per workspace, logical fencing tokens, WORKSPACE_BUSY for
//! the second claimant, and no release without confirmed process exit (the
//! caller proves termination before calling `release`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use term_contracts::mission::types::Id;

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    #[error("workspace {0} already has writer run {1}")]
    WorkspaceBusy(Id, Id),
    #[error("lease token {0} is stale (current {1})")]
    StaleToken(u64, u64),
}

#[derive(Debug, Clone)]
pub struct Lease {
    pub workspace_id: Id,
    pub owner_run_id: Id,
    pub token: u64,
}

/// Daemon-owned registry. Tokens are logical fencing (04 §2): a newer token
/// does not prove the old process died — callers pair release with
/// out-of-band termination confirmation.
#[derive(Debug, Default)]
pub struct WorkspaceLeases {
    inner: Mutex<HashMap<Id, Lease>>,
    next_token: AtomicU64,
}

impl WorkspaceLeases {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire the exclusive writer lease; the second claimant gets
    /// WORKSPACE_BUSY (case E16).
    pub fn acquire(&self, workspace_id: Id, run_id: Id) -> Result<Lease, LeaseError> {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = guard.get(&workspace_id) {
            return Err(LeaseError::WorkspaceBusy(
                workspace_id,
                existing.owner_run_id.clone(),
            ));
        }
        let token = self.next_token.fetch_add(1, Ordering::SeqCst) + 1;
        let lease = Lease {
            workspace_id: workspace_id.clone(),
            owner_run_id: run_id,
            token,
        };
        guard.insert(workspace_id, lease.clone());
        Ok(lease)
    }

    /// Release only with the matching token; stale tokens are refused
    /// (case E11 semantics at the lease level).
    pub fn release(&self, workspace_id: &Id, token: u64) -> Result<(), LeaseError> {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match guard.get(workspace_id) {
            Some(lease) if lease.token == token => {
                guard.remove(workspace_id);
                Ok(())
            }
            Some(lease) => Err(LeaseError::StaleToken(token, lease.token)),
            None => Ok(()),
        }
    }

    /// Current holder, if any.
    pub fn holder(&self, workspace_id: &Id) -> Option<Lease> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(workspace_id)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_writer_is_busy_and_release_requires_fresh_token() {
        let leases = WorkspaceLeases::new();
        let workspace = Id::generate();
        let first = leases.acquire(workspace.clone(), Id::generate()).unwrap();
        let busy = leases
            .acquire(workspace.clone(), Id::generate())
            .unwrap_err();
        assert!(matches!(busy, LeaseError::WorkspaceBusy(_, _)));
        // Stale token cannot release.
        assert!(leases.release(&workspace, first.token + 10).is_err());
        leases.release(&workspace, first.token).unwrap();
        let second = leases.acquire(workspace, Id::generate()).unwrap();
        assert!(second.token > first.token, "tokens advance monotonically");
    }
}
