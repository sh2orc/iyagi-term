//! Mission-scoped persistence for the shared pipe execution supervisor.
//! A run owns at most one Exec. The launch manifest is an actual owned,
//! hash-verified artifact and the Run link/Exec row commit atomically.

use super::{workflow, MissionService};
use crate::exec::persistence::{same_launch, ExecPersistence};
use std::sync::Arc;
use term_contracts::mission::{types::*, MissionErrorCode, MissionRpcError};

struct MissionExecStore {
    service: Arc<MissionService>,
}

impl MissionService {
    pub fn exec_persistence(self: &Arc<Self>) -> Arc<dyn ExecPersistence> {
        Arc::new(MissionExecStore {
            service: self.clone(),
        })
    }
}

fn failed(code: MissionErrorCode, message: &str) -> MissionRpcError {
    MissionRpcError::new(code, message)
}
fn io_error(error: MissionRpcError) -> std::io::Error {
    std::io::Error::other(format!("{:?}: {}", error.code, error.message))
}

enum DeterministicLaunch {
    Verification(super::verification_exec::Launch),
    Integration(super::integration_exec::Launch),
}
fn deterministic_launch(
    service: &MissionService,
    snapshot: &workflow::MissionEntities,
    run: &Run,
) -> Result<DeterministicLaunch, MissionRpcError> {
    if snapshot
        .tasks
        .iter()
        .any(|task| task.id == run.task_id && task.is_internal_integration())
    {
        super::integration_exec::load(service, snapshot, run).map(DeterministicLaunch::Integration)
    } else {
        super::verification_exec::load(service, snapshot, run)
            .map(DeterministicLaunch::Verification)
    }
}
impl DeterministicLaunch {
    fn matches(&self, record: &ExecRecord, manifest: &serde_json::Value) -> bool {
        match self {
            Self::Verification(launch) => launch.matches(record, manifest),
            Self::Integration(launch) => launch.matches(record, manifest),
        }
    }
    fn permits_release(&self, snapshot: &workflow::MissionEntities) -> bool {
        match self {
            Self::Verification(launch) => launch.permits_release(snapshot),
            Self::Integration(launch) => launch.permits_release(snapshot),
        }
    }
}

/// The Exec carries the native ownership proof a later daemon needs to
/// reclaim and observe its process group (root identity, start time, and a
/// durable group identity/reference of the matching kind). Native recovery
/// only ever adopts such records; `mission.run.attest_exited` refuses them.
pub(super) fn has_recoverable_group(record: &ExecRecord) -> bool {
    record.identity.is_some()
        && record.started_at.is_some()
        && record.group_reference.is_some()
        && matches!(
            (&record.group_identity, record.group_kind),
            (
                Some(term_contracts::workload::GroupRecoveryIdentity::CgroupV2 { .. }),
                Some(ExecGroupKind::Cgroup)
            ) | (
                Some(term_contracts::workload::GroupRecoveryIdentity::MacosGuardian { .. }),
                Some(ExecGroupKind::ObservedTree)
            )
        )
}

impl MissionExecStore {
    fn validate_recovered<'a>(
        &self,
        record: &ExecRecord,
        snapshot: &'a workflow::MissionEntities,
    ) -> Result<&'a Run, MissionRpcError> {
        if record.owner_daemon_id == self.service.owner_daemon_id
            || record.mission_id != snapshot.mission.id
            || !has_recoverable_group(record)
        {
            return Err(failed(
                MissionErrorCode::PolicyDenied,
                "execution lacks prior native ownership",
            ));
        }
        let run = snapshot
            .runs
            .iter()
            .find(|run| {
                run.id == record.run_id
                    && run.mission_id == record.mission_id
                    && run.exec_id.as_ref() == Some(&record.id)
            })
            .ok_or_else(|| {
                failed(
                    MissionErrorCode::IntegrityFailed,
                    "recovered execution lost its run link",
                )
            })?;
        if run
            .binding_snapshot
            .as_ref()
            .is_some_and(|binding| binding.resource_policy != record.resource_policy)
            || snapshot
                .execs
                .iter()
                .filter(|exec| exec.run_id == run.id)
                .count()
                != 1
        {
            return Err(failed(
                MissionErrorCode::IntegrityFailed,
                "recovered execution changed its launch",
            ));
        }
        let manifest = self
            .service
            .artifacts
            .read_mission_body(&record.mission_id, &record.launch_manifest_ref, 256 * 1024)
            .map_err(|(code, message)| MissionRpcError::new(code, message))?;
        if run.binding_snapshot.is_none() {
            let document = serde_json::from_slice(&manifest).map_err(|_| {
                failed(
                    MissionErrorCode::IntegrityFailed,
                    "invalid recovered deterministic manifest",
                )
            })?;
            if !deterministic_launch(&self.service, snapshot, run)?.matches(record, &document) {
                return Err(failed(
                    MissionErrorCode::IntegrityFailed,
                    "recovered deterministic command changed its launch contract",
                ));
            }
        }
        Ok(run)
    }

    fn recovered_action_record(
        &self,
        record: &ExecRecord,
    ) -> Result<crate::exec::persistence::RecoveredAction, MissionRpcError> {
        use crate::exec::persistence::RecoveredAction;
        use term_storage::mission::types::{OutboxOperation, OutboxState};
        let snapshot = workflow::load_entities(&self.service.storage, &record.mission_id)?;
        let run = self.validate_recovered(record, &snapshot)?;
        if record.state == ExecState::Exited || !snapshot.execs.contains(record) {
            return Err(failed(
                MissionErrorCode::IntegrityFailed,
                "recovered execution snapshot changed",
            ));
        }
        let cancelled = self
            .service
            .storage
            .mission_outbox()
            .map_err(MissionService::store_error)?
            .iter()
            .any(|intent| {
                intent.mission_id == record.mission_id
                    && intent.run_id.as_ref() == Some(&run.id)
                    && intent.fencing_token == run.fencing_token.get()
                    && intent.operation == OutboxOperation::Cancel
                    && matches!(intent.state, OutboxState::Prepared | OutboxState::Sending)
            });
        Ok(
            if cancelled || snapshot.mission.state == MissionState::Stopping {
                RecoveredAction::Stop
            } else {
                RecoveredAction::Observe
            },
        )
    }

    fn confirm_recovered_record(
        &self,
        expected: &ExecRecord,
        ended_at: &str,
    ) -> Result<(), MissionRpcError> {
        if expected.state == ExecState::Exited || ended_at.is_empty() {
            return Err(failed(
                MissionErrorCode::InvalidState,
                "invalid recovered exit observation",
            ));
        }
        let mut ended = expected.clone();
        ended.state = ExecState::Exited;
        ended.ended_at = Some(ended_at.into());
        // Native group emptiness does not reveal the provider exit status.
        ended.exit_code = None;
        for _ in 0..32 {
            let snapshot = workflow::load_entities(&self.service.storage, &expected.mission_id)?;
            self.validate_recovered(expected, &snapshot)?;
            let stored = snapshot
                .execs
                .iter()
                .find(|e| e.id == expected.id)
                .ok_or_else(|| {
                    failed(
                        MissionErrorCode::NotFound,
                        "recovered execution disappeared",
                    )
                })?;
            if stored == &ended {
                return Ok(());
            }
            if stored != expected {
                return Err(failed(
                    MissionErrorCode::IntegrityFailed,
                    "recovered execution changed before exit commit",
                ));
            }
            match self.service.commit_actor(
                snapshot.mission,
                "engine.exec_recovered_exit",
                vec![Entity::Exec(Box::new(ended.clone()))],
                vec![],
            ) {
                Err(error) if error.code == MissionErrorCode::RevisionConflict => continue,
                other => return other,
            }
        }
        Err(failed(
            MissionErrorCode::RevisionConflict,
            "recovered exit exhausted concurrent revision retries",
        ))
    }

    fn owned_workspace<'a>(
        &self,
        run: &Run,
        snapshot: &'a workflow::MissionEntities,
    ) -> Result<&'a Workspace, MissionRpcError> {
        snapshot
            .workspaces
            .iter()
            .find(|workspace| {
                Some(&workspace.id) == run.workspace_id.as_ref()
                    && workspace.mission_id == run.mission_id
                    && workspace.owned_by_daemon
                    && workspace.state == WorkspaceState::Busy
                    && workspace.writer_run_id.as_ref() == Some(&run.id)
                    && workspace.lease_token == run.fencing_token
            })
            .ok_or_else(|| {
                failed(
                    MissionErrorCode::PolicyDenied,
                    "exec run has no current owned workspace lease",
                )
            })
    }
    fn validate_owner<'a>(
        &self,
        record: &ExecRecord,
        snapshot: &'a workflow::MissionEntities,
    ) -> Result<&'a Run, MissionRpcError> {
        if record.mission_id != snapshot.mission.id
            || record.owner_daemon_id != self.service.owner_daemon_id
        {
            return Err(failed(
                MissionErrorCode::PolicyDenied,
                "exec owner does not match this daemon and mission",
            ));
        }
        snapshot
            .runs
            .iter()
            .find(|r| r.id == record.run_id)
            .ok_or_else(|| {
                failed(
                    MissionErrorCode::NotFound,
                    "exec run does not belong to this mission",
                )
            })
    }

    fn prepare_record(
        &self,
        mut record: ExecRecord,
        manifest: &[u8],
    ) -> Result<ArtifactRef, MissionRpcError> {
        use sha2::{Digest, Sha256};
        if record.state != ExecState::Prepared
            || record.identity.is_some()
            || record.group_kind.is_some()
            || record.group_reference.is_some()
            || record.group_identity.is_some()
            || record.started_at.is_some()
            || record.ended_at.is_some()
            || record.exit_code.is_some()
        {
            return Err(failed(
                MissionErrorCode::InvalidState,
                "new exec must be unstarted and prepared",
            ));
        }
        if manifest.len() > 256 * 1024
            || record.launch_manifest_ref.media_type != "application/json"
            || record.launch_manifest_ref.bytes.get() != manifest.len() as u64
            || record.launch_manifest_ref.sha256 != format!("{:x}", Sha256::digest(manifest))
        {
            return Err(failed(
                MissionErrorCode::IntegrityFailed,
                "exec manifest metadata does not match its body",
            ));
        }
        let document: serde_json::Value = serde_json::from_slice(manifest).map_err(|_| {
            failed(
                MissionErrorCode::InvalidArgument,
                "exec manifest must be JSON",
            )
        })?;
        let object = document.as_object().ok_or_else(|| {
            failed(
                MissionErrorCode::InvalidArgument,
                "exec manifest must be an object",
            )
        })?;
        if !(object.len() == 4 && !object.contains_key("env_clear")
            || object.len() == 5 && document["env_clear"].is_boolean())
            || !document["program"].is_string()
            || !document["cwd"].is_string()
            || !document["argv"]
                .as_array()
                .is_some_and(|a| a.iter().all(|v| v.is_string()))
            || !document["env_keys"]
                .as_array()
                .is_some_and(|a| a.iter().all(|v| v.is_string()))
        {
            return Err(failed(
                MissionErrorCode::InvalidArgument,
                "exec manifest requires program, argv, cwd and environment keys only",
            ));
        }
        // Validate before writing any artifact. The same checks run again
        // against each CAS snapshot before linking the prepared process.
        let snapshot = workflow::load_entities(&self.service.storage, &record.mission_id)?;
        let run = self.validate_owner(&record, &snapshot)?;
        if let Some(stored) = snapshot.execs.iter().find(|e| e.id == record.id).cloned() {
            if !same_launch(&stored, &record) || run.exec_id.as_ref() != Some(&stored.id) {
                return Err(failed(
                    MissionErrorCode::IntegrityFailed,
                    "exec id was already used for another launch",
                ));
            }
            self.service
                .artifacts
                .read_mission_body(&record.mission_id, &stored.launch_manifest_ref, 256 * 1024)
                .map_err(|(code, message)| MissionRpcError::new(code, message))?;
            return Ok(stored.launch_manifest_ref);
        }
        if run.exec_id.is_some()
            || !matches!(run.state, RunState::Prepared | RunState::Starting)
            || snapshot.mission.state != MissionState::Running
        {
            return Err(failed(
                MissionErrorCode::InvalidState,
                "run cannot acquire another exec",
            ));
        }
        let workspace = self.owned_workspace(run, &snapshot)?;
        if let Some(binding) = &run.binding_snapshot {
            if document["program"].as_str() != Some(&binding.program)
                || record.resource_policy != binding.resource_policy
                || document["cwd"].as_str() != Some(&workspace.path)
            {
                return Err(failed(
                    MissionErrorCode::PolicyDenied,
                    "exec launch differs from the reserved binding snapshot",
                ));
            }
        } else {
            let launch = deterministic_launch(&self.service, &snapshot, run)?;
            if !launch.matches(&record, &document) || !launch.permits_release(&snapshot) {
                return Err(failed(
                    MissionErrorCode::PolicyDenied,
                    "deterministic exec differs from its frozen launch contract",
                ));
            }
        }
        record.launch_manifest_ref = workflow::store_artifact(
            &self.service.artifacts,
            &record.mission_id,
            "application/json",
            manifest,
        )?;
        for _ in 0..32 {
            let snapshot = workflow::load_entities(&self.service.storage, &record.mission_id)?;
            let mut run = self.validate_owner(&record, &snapshot)?.clone();
            if let Some(stored) = snapshot.execs.iter().find(|e| e.id == record.id).cloned() {
                if same_launch(&stored, &record) && run.exec_id.as_ref() == Some(&stored.id) {
                    return Ok(stored.launch_manifest_ref);
                }
                return Err(failed(
                    MissionErrorCode::IntegrityFailed,
                    "exec id changed while preparing",
                ));
            }
            if run.exec_id.is_some()
                || !matches!(run.state, RunState::Prepared | RunState::Starting)
                || snapshot.mission.state != MissionState::Running
            {
                return Err(failed(
                    MissionErrorCode::InvalidState,
                    "run stopped before exec preparation",
                ));
            }
            self.owned_workspace(&run, &snapshot)?;
            if run.binding_snapshot.is_none() {
                let launch = deterministic_launch(&self.service, &snapshot, &run)?;
                if !launch.matches(&record, &document) || !launch.permits_release(&snapshot) {
                    return Err(failed(
                        MissionErrorCode::PolicyDenied,
                        "deterministic command changed before exec preparation",
                    ));
                }
            }
            run.exec_id = Some(record.id.clone());
            let result = self.service.commit_actor(
                snapshot.mission,
                "engine.exec_prepared",
                vec![
                    Entity::Run(Box::new(run)),
                    Entity::Exec(Box::new(record.clone())),
                ],
                vec![],
            );
            match result {
                Err(error) if error.code == MissionErrorCode::RevisionConflict => continue,
                other => {
                    other?;
                    return Ok(record.launch_manifest_ref);
                }
            }
        }
        Err(failed(
            MissionErrorCode::RevisionConflict,
            "exec preparation exhausted concurrent revision retries",
        ))
    }

    fn update_record(&self, record: ExecRecord) -> Result<(), MissionRpcError> {
        for _ in 0..32 {
            let snapshot = workflow::load_entities(&self.service.storage, &record.mission_id)?;
            let run = self.validate_owner(&record, &snapshot)?;
            let stored = snapshot
                .execs
                .iter()
                .find(|e| e.id == record.id)
                .cloned()
                .ok_or_else(|| failed(MissionErrorCode::NotFound, "exec has not been prepared"))?;
            if !same_launch(&stored, &record)
                || stored.launch_manifest_ref != record.launch_manifest_ref
                || run.exec_id.as_ref() != Some(&record.id)
            {
                return Err(failed(
                    MissionErrorCode::IntegrityFailed,
                    "exec observation does not match its owned launch",
                ));
            }
            if stored.state == record.state {
                if stored.identity == record.identity
                    && stored.group_kind == record.group_kind
                    && stored.group_reference == record.group_reference
                    && stored.group_identity == record.group_identity
                    && stored.started_at == record.started_at
                    && stored.exit_code == record.exit_code
                {
                    return Ok(());
                }
                return Err(failed(
                    MissionErrorCode::IntegrityFailed,
                    "exec observation rewrites existing evidence",
                ));
            }
            if !matches!(
                (stored.state, record.state),
                (ExecState::Prepared, ExecState::Spawned | ExecState::Exited)
                    | (ExecState::Spawned, ExecState::Stopping | ExecState::Exited)
                    | (ExecState::Stopping, ExecState::Exited)
            ) {
                return Err(failed(
                    MissionErrorCode::InvalidState,
                    "exec lifecycle transition is invalid",
                ));
            }
            if record.state == ExecState::Spawned
                && (!matches!(run.state, RunState::Prepared | RunState::Starting)
                    || snapshot.mission.state != MissionState::Running)
            {
                return Err(failed(
                    MissionErrorCode::InvalidState,
                    "run stopped before launch release",
                ));
            }
            if record.state == ExecState::Spawned {
                self.owned_workspace(run, &snapshot)?;
                if run.binding_snapshot.is_none() {
                    let launch = deterministic_launch(&self.service, &snapshot, run)?;
                    let body = self
                        .service
                        .artifacts
                        .read_mission_body(
                            &record.mission_id,
                            &record.launch_manifest_ref,
                            256 * 1024,
                        )
                        .map_err(|(code, message)| MissionRpcError::new(code, message))?;
                    let document = serde_json::from_slice(&body).map_err(|_| {
                        failed(
                            MissionErrorCode::IntegrityFailed,
                            "invalid deterministic manifest",
                        )
                    })?;
                    if !launch.matches(&record, &document) || !launch.permits_release(&snapshot) {
                        return Err(failed(
                            MissionErrorCode::PolicyDenied,
                            "deterministic command changed before launch release",
                        ));
                    }
                }
            }
            if stored.identity.is_some()
                && (stored.identity != record.identity
                    || stored.group_kind != record.group_kind
                    || stored.group_reference != record.group_reference
                    || stored.group_identity != record.group_identity
                    || stored.started_at != record.started_at)
            {
                return Err(failed(
                    MissionErrorCode::IntegrityFailed,
                    "exec process identity cannot be replaced",
                ));
            }
            if record.state == ExecState::Spawned
                && (record.identity.is_none()
                    || record.group_kind.is_none()
                    || record.group_reference.as_deref().is_none_or(str::is_empty)
                    || record.started_at.is_none()
                    || record.ended_at.is_some())
            {
                return Err(failed(
                    MissionErrorCode::IntegrityFailed,
                    "spawned exec lacks owned process evidence",
                ));
            }
            if record.state == ExecState::Exited && record.ended_at.is_none() {
                return Err(failed(
                    MissionErrorCode::IntegrityFailed,
                    "exited exec requires its observation time",
                ));
            }
            let mut upserts = vec![Entity::Exec(Box::new(record.clone()))];
            if record.state == ExecState::Spawned && run.binding_snapshot.is_none() {
                let mut started = run.clone();
                started.state = RunState::Running;
                started.started_at = record.started_at.clone();
                upserts.push(Entity::Run(Box::new(started)));
            }
            let result = self.service.commit_actor(
                snapshot.mission,
                "engine.exec_observed",
                upserts,
                vec![],
            );
            match result {
                Err(error) if error.code == MissionErrorCode::RevisionConflict => continue,
                other => return other,
            }
        }
        Err(failed(
            MissionErrorCode::RevisionConflict,
            "exec observation exhausted concurrent revision retries",
        ))
    }
}

impl ExecPersistence for MissionExecStore {
    fn recovered_action(
        &self,
        record: &ExecRecord,
    ) -> std::io::Result<crate::exec::persistence::RecoveredAction> {
        self.recovered_action_record(record).map_err(io_error)
    }
    fn confirm_recovered_exit(&self, expected: &ExecRecord, ended_at: &str) -> std::io::Result<()> {
        self.confirm_recovered_record(expected, ended_at)
            .map_err(io_error)
    }
    fn recovery_records(&self, previous: &[Id]) -> std::io::Result<Vec<ExecRecord>> {
        self.service
            .storage
            .mission_exec_recovery_records(&self.service.owner_daemon_id, previous)
            .map_err(MissionService::store_error)
            .map_err(io_error)
    }
    fn prepare(&self, record: ExecRecord, manifest: &[u8]) -> std::io::Result<ArtifactRef> {
        self.prepare_record(record, manifest).map_err(io_error)
    }
    fn update(&self, record: ExecRecord) -> std::io::Result<()> {
        self.update_record(record).map_err(io_error)
    }
}
