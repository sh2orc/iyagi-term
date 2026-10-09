//! Storage boundary for process ownership. Preparation returns the real
//! immutable manifest reference; later transitions can fail and callers must
//! retain ownership/reservations until persistence succeeds.

use term_contracts::mission::types::{ArtifactRef, ExecRecord, Id};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveredAction {
    Observe,
    Stop,
}

pub trait ExecPersistence: Send + Sync {
    /// Store the sanitized manifest and Prepared ownership before launch.
    /// Repeating the same exec id returns its original manifest reference.
    fn prepare(&self, record: ExecRecord, manifest: &[u8]) -> std::io::Result<ArtifactRef>;
    /// Persist an observed lifecycle transition. Exited is irreversible;
    /// an identical retry must succeed without creating another event.
    fn update(&self, record: ExecRecord) -> std::io::Result<()>;
    /// One consistent snapshot of older daemons' unfinished executions plus
    /// every requested previous reservation, even if it has now exited.
    /// Missing rows never mean termination. No process is adopted by this read.
    fn recovery_records(&self, _previous: &[Id]) -> std::io::Result<Vec<ExecRecord>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this persistence provider cannot restore execution reservations",
        ))
    }
    /// Revalidate the immutable launch and durable cancellation intent before
    /// touching the recovered native group. No public RPC accepts this proof.
    fn recovered_action(&self, _record: &ExecRecord) -> std::io::Result<RecoveredAction> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "native recovery validation unavailable",
        ))
    }
    /// CAS an older daemon's record to Exited only after the supervisor has
    /// observed its verified native group empty. This does not settle a Run's
    /// provider result or rewrite ownership to the current daemon.
    fn confirm_recovered_exit(
        &self,
        _expected: &ExecRecord,
        _ended_at: &str,
    ) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "native recovery persistence unavailable",
        ))
    }
}

pub(crate) fn same_launch(a: &ExecRecord, b: &ExecRecord) -> bool {
    a.id == b.id
        && a.mission_id == b.mission_id
        && a.run_id == b.run_id
        && a.owner_daemon_id == b.owner_daemon_id
        && a.resource_policy == b.resource_policy
        && a.launch_manifest_ref.sha256 == b.launch_manifest_ref.sha256
        && a.launch_manifest_ref.bytes == b.launch_manifest_ref.bytes
        && a.launch_manifest_ref.media_type == b.launch_manifest_ref.media_type
}

/// Deterministic adapters/tests may observe records without a database.
/// Production construction uses an explicit fallible persistence provider.
pub(crate) struct Observer(pub super::PersistExec);
impl ExecPersistence for Observer {
    fn prepare(&self, record: ExecRecord, _manifest: &[u8]) -> std::io::Result<ArtifactRef> {
        let reference = record.launch_manifest_ref.clone();
        (self.0)(record);
        Ok(reference)
    }
    fn update(&self, record: ExecRecord) -> std::io::Result<()> {
        (self.0)(record);
        Ok(())
    }
}
