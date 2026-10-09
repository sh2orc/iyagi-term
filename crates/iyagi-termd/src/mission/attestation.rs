//! `mission.run.attest_exited`: the user confirms that a run's process is gone
//! although the daemon holds no termination evidence (an older daemon's
//! unrecoverable process group, or a run that never produced an Exec).
//!
//! The attestation releases exactly what an observed exit would release — the
//! run's execution slot, its workspace writer lease (the workspace stays
//! Quarantined) and the restored resource reservation of the older daemon's
//! Exec — and nothing else. It is stored as its own proof kind
//! (`Run.reconciliation_kind = user_attested`): the Run state, result and
//! timing stay unknown, the provider outcome and external effects stay
//! unconfirmed, and acceptance still requires the explicit
//! `acknowledged_reconciled_run_ids` review with a completed replacement.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use term_contracts::{
    ids::U64String,
    mission::{
        rpc::{methods, MissionRunAttestExitedParams},
        types::*,
        MissionErrorCode, MissionRpcError,
    },
};
use term_storage::mission::types::{ApplyMode, MissionStoreError};

use super::{
    exec_store::has_recoverable_group,
    reconciliation::{ended_exec, uncertain, ENDED},
    service::{ApplyPlan, Handled, MissionService},
    workflow::{self, MissionEntities},
};

pub(super) const USER_ATTESTED: &str = "user_attested";
const NOT_APPLICABLE: &str = "attestation_not_applicable";
const MAX_ATTESTATION_BYTES: usize = 64;
const PROOF_LIMIT: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AttestationProof {
    pub version: u32,
    pub kind: String,
    pub mission_id: Id,
    pub run_id: Id,
    pub fencing_token: U64String,
    pub attested_at: Timestamp,
    pub request_id: Id,
    /// Statement key the user confirmed (audit only).
    pub attestation: String,
    /// The Exec as the daemon last knew it, and the Exited projection this
    /// attestation wrote. Both null when the Run never had an Exec.
    pub exec_before: Option<ExecRecord>,
    pub exec_after: Option<ExecRecord>,
}

fn not_applicable(message: &str) -> MissionRpcError {
    MissionService::with_reason(
        MissionRpcError::new(MissionErrorCode::InvalidState, message),
        NOT_APPLICABLE,
    )
}

fn valid_attestation(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_ATTESTATION_BYTES
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl MissionService {
    /// Which Exec (if any) the attestation would close. Errors explain why
    /// the run is not a candidate; nothing is changed by this check.
    pub(super) fn attestation_target<'a>(
        &self,
        snapshot: &'a MissionEntities,
        run_id: &Id,
    ) -> Result<(&'a Run, Option<&'a ExecRecord>), MissionRpcError> {
        let run = snapshot
            .runs
            .iter()
            .find(|r| &r.id == run_id)
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::NotFound,
                    format!("run {run_id} not found in this mission"),
                )
            })?;
        if !uncertain(run) || run.reconciliation_ref.is_some() || !run.holds_execution_slot() {
            return Err(not_applicable(
                "only an unknown or interrupted run without termination evidence can be \
                 confirmed as exited",
            ));
        }
        let execs: Vec<&ExecRecord> = snapshot
            .execs
            .iter()
            .filter(|e| e.run_id == run.id)
            .collect();
        let exec = match (&run.exec_id, execs.as_slice()) {
            (None, []) => None,
            (Some(id), [exec]) if &exec.id == id && exec.mission_id == run.mission_id => {
                Some(*exec)
            }
            _ => {
                return Err(not_applicable(
                    "the run's execution record is inconsistent; it cannot be attested",
                ))
            }
        };
        if let Some(exec) = exec {
            if exec.state == ExecState::Exited || ended_exec(snapshot, run).is_some() {
                return Err(not_applicable(
                    "termination was already observed; automatic reconciliation releases this run",
                ));
            }
            if exec.owner_daemon_id == self.owner_daemon_id {
                return Err(not_applicable(
                    "this daemon still supervises the process; wait for its stop or cancel the \
                     mission",
                ));
            }
            // A durable native group identity lets this daemon's recovery
            // reclaim, observe and (on cancellation) stop the older daemon's
            // group, and only that observation may close the Exec. Nothing
            // records a permanent recovery failure, so the user can only
            // stand in for Execs without such an identity (or Runs without
            // any Exec).
            if has_recoverable_group(exec) {
                return Err(not_applicable(
                    "native recovery can still observe this process group; wait for its \
                     reconciliation",
                ));
            }
        }
        Ok((run, exec))
    }

    pub(super) fn run_attest_exited(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: MissionRunAttestExitedParams = Self::parse(params)?;
        if !valid_attestation(&params.attestation) {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "attestation must be a statement key of 1..=64 characters [a-z0-9_]",
            ));
        }
        let snapshot = workflow::load_entities(&self.storage, &params.mission_id)?;
        let mission = snapshot.mission.clone();
        if !super::timing::revision_accepts(&mission, params.expected_revision.get()) {
            return Err(Self::store_error(MissionStoreError::RevisionConflict {
                expected_revision: params.expected_revision.get(),
                current_revision: mission.revision.get(),
            }));
        }
        let (run, exec) = self.attestation_target(&snapshot, &params.run_id)?;
        let now = term_storage::time::now_iso8601();
        let exec_after = exec.map(|exec| {
            let mut ended = exec.clone();
            ended.state = ExecState::Exited;
            ended.ended_at = Some(now.clone());
            // Nothing observed the exit status.
            ended.exit_code = None;
            ended
        });
        let proof = AttestationProof {
            version: 1,
            kind: USER_ATTESTED.into(),
            mission_id: mission.id.clone(),
            run_id: run.id.clone(),
            fencing_token: run.fencing_token.clone(),
            attested_at: now.clone(),
            request_id: params.request_id.clone(),
            attestation: params.attestation.clone(),
            exec_before: exec.cloned(),
            exec_after: exec_after.clone(),
        };
        let mut next_run = run.clone();
        next_run.reconciliation_ref = Some(workflow::store_artifact(
            &self.artifacts,
            &mission.id,
            "application/json",
            &serde_json::to_vec(&proof).expect("serializable attestation"),
        )?);
        next_run.reconciliation_kind = Some(ReconciliationKind::UserAttested);
        let mut upserts = vec![];
        if let Some(exec) = exec_after {
            upserts.push(Entity::Exec(Box::new(exec)));
        }
        // Same release as an observed exit: the directory stays quarantined
        // and is never adopted by a retry.
        for workspace in &snapshot.workspaces {
            if Some(&workspace.id) == run.workspace_id.as_ref()
                && workspace.writer_run_id.as_ref() == Some(&run.id)
                && workspace.owned_by_daemon
            {
                let mut workspace = workspace.clone();
                workspace.writer_run_id = None;
                workspace.state = WorkspaceState::Quarantined;
                upserts.push(Entity::Workspace(Box::new(workspace)));
            }
        }
        for task in &snapshot.tasks {
            if task.id == run.task_id && task.active_run_id.as_ref() == Some(&run.id) {
                let mut task = task.clone();
                task.active_run_id = None;
                task.workspace_id = None;
                if !task.state.is_terminal() {
                    task.state = TaskState::Blocked;
                    task.blocked_code = Some(ENDED.into());
                    task.dispatch_after_unix_ms = None;
                }
                task.updated_at = now.clone();
                upserts.push(Entity::Task(Box::new(task)));
            }
        }
        upserts.push(Entity::Run(Box::new(next_run)));
        let mut next_mission = mission.clone();
        next_mission.revision =
            U64String::new(params.expected_revision.get() + 1).map_err(|_| {
                MissionRpcError::new(MissionErrorCode::InvalidArgument, "revision overflow")
            })?;
        next_mission.updated_at = now;
        upserts.insert(0, Entity::Mission(Box::new(next_mission)));
        let params_json = serde_json::to_value(&params).unwrap_or(Value::Null);
        let applied = self.apply(ApplyPlan {
            request_id: params.request_id,
            method: methods::MISSION_RUN_ATTEST_EXITED.to_string(),
            params: params_json,
            mission_id: params.mission_id.clone(),
            mode: ApplyMode::Mutate {
                expected_revision: params.expected_revision.get(),
            },
            event_type: MissionEventType::Reconciled,
            upserts,
            deletes: Vec::new(),
            outbox: Vec::new(),
            adopt: vec![],
        })?;
        Ok(Self::mutation_handled(&applied))
    }

    /// The stored attestation still describes this exact Run/fence and the
    /// Exec projection it closed. It never stands in for a durable exit proof.
    pub(super) fn attested_termination(&self, snapshot: &MissionEntities, run: &Run) -> bool {
        let Some(reference) = run.reconciliation_ref.as_ref() else {
            return false;
        };
        let Ok(bytes) =
            self.artifacts
                .read_mission_body(&snapshot.mission.id, reference, PROOF_LIMIT)
        else {
            return false;
        };
        let Ok(proof) = serde_json::from_slice::<AttestationProof>(&bytes) else {
            return false;
        };
        if proof.version != 1
            || proof.kind != USER_ATTESTED
            || proof.mission_id != run.mission_id
            || proof.run_id != run.id
            || proof.fencing_token != run.fencing_token
            || run.reconciliation_kind != Some(ReconciliationKind::UserAttested)
        {
            return false;
        }
        match (&run.exec_id, &proof.exec_after) {
            (None, None) => {
                proof.exec_before.is_none() && !snapshot.execs.iter().any(|e| e.run_id == run.id)
            }
            (Some(id), Some(after)) => {
                &after.id == id
                    && ended_exec(snapshot, run) == Some(after)
                    && self
                        .artifacts
                        .read_mission_body(&run.mission_id, &after.launch_manifest_ref, 256 * 1024)
                        .is_ok()
            }
            _ => false,
        }
    }
}
