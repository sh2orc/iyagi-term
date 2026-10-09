//! Explicit conflict resolution retains the integration Task and workspace.
use super::{execution::PreparedRun, workflow, MissionService};
use serde_json::json;
use std::path::Path;
use term_contracts::mission::{types::*, MissionErrorCode, MissionRpcError};
use term_storage::mission::types::StoredOutbox;

fn invalid(message: &str) -> MissionRpcError {
    MissionRpcError::new(MissionErrorCode::InvalidState, message)
}

impl MissionService {
    /// A confirmed local exit does not make uncertain files an accepted input.
    /// Rebuild frozen sources in a fresh workspace and retain the quarantine.
    pub(super) fn prepare_integration_restart(
        &self,
        snapshot: &workflow::MissionEntities,
        task: &Task,
        run: &Run,
    ) -> Result<Task, MissionRpcError> {
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|w| {
                Some(&w.id) == run.workspace_id.as_ref()
                    && w.mission_id == snapshot.mission.id
                    && w.kind == WorkspaceKind::Integration
                    && w.owned_by_daemon
                    && w.state == WorkspaceState::Quarantined
                    && w.writer_run_id.is_none()
            })
            .ok_or_else(|| {
                invalid("unknown integration workspace is not quarantined and released")
            })?;
        if snapshot
            .runs
            .iter()
            .any(|r| r.workspace_id.as_ref() == Some(&workspace.id) && r.holds_execution_slot())
        {
            return Err(invalid(
                "integration workspace still has an owned execution",
            ));
        }
        let plan_ref = task
            .integration
            .as_ref()
            .map_or(&task.contract.objective_ref, |i| &i.plan_ref);
        let plan: super::integration_exec::Plan = serde_json::from_slice(
            &self
                .artifacts
                .read_mission_body(&snapshot.mission.id, plan_ref, 256 * 1024)
                .map_err(|(code, message)| MissionRpcError::new(code, message))?,
        )
        .map_err(|_| invalid("original integration plan is invalid"))?;
        if snapshot.mission.phase != Phase::Integrating
            || !plan.matches(snapshot)
            || plan.prior_candidate_id != snapshot.mission.candidate_id
        {
            return Err(invalid(
                "original integration inputs changed before recovery",
            ));
        }
        let mut next = task.clone();
        next.integration = Some(IntegrationTask {
            plan_ref: plan_ref.clone(),
            step: IntegrationStep::Automatic,
        });
        next.contract.objective_ref = plan_ref.clone();
        next.workspace_id = None;
        self.prepare_task_retry(snapshot, &next, None)
    }

    pub(super) fn prepare_integration_answer(
        &self,
        snapshot: &workflow::MissionEntities,
        decision: &Decision,
        option: Option<&str>,
    ) -> Result<Option<Task>, MissionRpcError> {
        let run = decision
            .requesting_run_id
            .as_ref()
            .and_then(|id| snapshot.runs.iter().find(|r| &r.id == id));
        let Some(task) = run.and_then(|r| {
            snapshot
                .tasks
                .iter()
                .find(|t| t.id == r.task_id && t.is_internal_integration())
        }) else {
            if option == Some("stop_mission") {
                return Ok(None);
            }
            return Err(invalid("this legacy conflict requires explicit replanning; its workspace cannot be adopted"));
        };
        if decision.state != DecisionState::Open
            || task.active_run_id.is_some()
            || task.state != TaskState::Failed
            || !matches!(
                snapshot.mission.state,
                MissionState::Running | MissionState::Paused | MissionState::Pausing
            )
            || snapshot
                .runs
                .iter()
                .filter(|r| r.task_id == task.id)
                .max_by_key(|r| r.attempt)
                .map(|r| &r.id)
                != decision.requesting_run_id.as_ref()
        {
            return Err(invalid("the conflict no longer owns this integration task"));
        }
        if option == Some("stop_mission") {
            return Ok(None);
        }
        if option != Some("resolve_and_reintegrate") {
            return Err(invalid(
                "choose the integrator resolution or stop the mission",
            ));
        }
        // A policy that does not allow the role is the user's setting to change.
        if !snapshot
            .mission
            .policy
            .allowed_roles
            .contains(&Role::Integrator)
        {
            return Err(MissionRpcError::new(
                MissionErrorCode::PolicyDenied,
                "enable the integrator role in this mission policy",
            ));
        }
        // The role is allowed: only an option the decision actually offered,
        // and only while an Integrator binding exists (none → no resolution).
        if !decision
            .options
            .iter()
            .any(|o| o.id == "resolve_and_reintegrate")
            || !workflow::integrator_available(&snapshot.mission)
        {
            return Err(MissionService::with_reason(
                MissionRpcError::new(
                    MissionErrorCode::InvalidArgument,
                    "this mission has no Integrator to resolve the conflict; exclude the candidate \
                     or stop the mission",
                ),
                "option_invalid",
            ));
        }
        let conflict_run_id = &run.expect("task has conflict run").id;
        let input = self.integration_conflict_input(snapshot, task, conflict_run_id)?;
        let mut next = task.clone();
        next.role = Some(Role::Integrator);
        if next.binding_id.is_none() {
            next.binding_id = snapshot
                .mission
                .role_bindings
                .iter()
                .find(|r| r.role == Role::Integrator)
                .map(|r| r.primary_binding_id.clone());
        }
        let plan_ref = next.integration.as_ref().map_or_else(
            || task.contract.objective_ref.clone(),
            |i| i.plan_ref.clone(),
        );
        next.integration = Some(IntegrationTask {
            plan_ref,
            step: IntegrationStep::Resolving {
                conflict_run_id: conflict_run_id.clone(),
            },
        });
        next.contract.allowed_paths = input.resume.allowed_paths;
        next.contract.objective_ref = workflow::store_artifact(&self.artifacts, &snapshot.mission.id, "application/json",
            &serde_json::to_vec(&json!({
                "instruction": "Resolve the recorded conflict in this retained workspace. Preserve the already applied changes and original requirements. Inspect base/ours/theirs; do not choose a side implicitly. Edit the conflicted files, remove conflict markers, and return a Patch report. The daemon will capture the result, apply remaining sources, and rerun verification and review. Do not reset or replace this workspace.",
                "conflict_run_id": conflict_run_id, "base_oid": input.plan.base_oid,
                "ours_head_oid": input.workspace.head_oid,
                "theirs": input.plan.sources.get(input.resume.applied_count - 1),
                "conflict_paths": input.resume.conflict_paths, "input_sources": input.plan.sources,
                "applied_count": input.resume.applied_count - 1,
            })).expect("integration resolution context"))?;
        next.workspace_id = Some(input.workspace.id);
        self.prepare_task_retry(snapshot, &next, None).map(Some)
    }

    pub(super) fn prepare_integration_resolver(
        &self,
        snapshot: workflow::MissionEntities,
        mut task: Task,
        mut run: Run,
        binding: Binding,
        intent: &StoredOutbox,
        root: &Path,
    ) -> Result<Option<PreparedRun>, MissionRpcError> {
        let Some(IntegrationTask {
            step: IntegrationStep::Resolving { conflict_run_id },
            ..
        }) = task.integration.as_ref()
        else {
            return Err(invalid("task is not resolving an integration conflict"));
        };
        if snapshot.mission.phase != Phase::Integrating
            || task.active_run_id.as_ref() != Some(&run.id)
            || !snapshot
                .mission
                .policy
                .allowed_roles
                .contains(&Role::Integrator)
        {
            return Err(invalid("integration resolution is no longer dispatchable"));
        }
        let input = self.integration_conflict_input(&snapshot, &task, conflict_run_id)?;
        let mut workspace = input.workspace;
        let path = Path::new(&workspace.path);
        let parent = root
            .join(snapshot.mission.id.as_str())
            .join("workspaces")
            .canonicalize()
            .map_err(|_| invalid("integration workspace root is unavailable"))?;
        let canonical = path
            .canonicalize()
            .map_err(|_| invalid("integration workspace is unavailable"))?;
        if canonical.parent() != Some(parent.as_path())
            || std::fs::symlink_metadata(path).map_or(true, |m| m.file_type().is_symlink())
        {
            return Err(invalid("integration workspace escaped its owned directory"));
        }
        if task.contract.allowed_paths != input.resume.allowed_paths {
            return Err(invalid("integration resolution scope changed"));
        }
        workspace.writer_run_id = Some(run.id.clone());
        workspace.lease_token = run.fencing_token.clone();
        run.workspace_id = Some(workspace.id.clone());
        task.workspace_id = Some(workspace.id.clone());
        self.claim_provider_start(snapshot, task, run, workspace, binding, intent)
    }

    pub(super) fn finish_integration_resolver(task: &mut Task, run: &Run, result: &ProviderResult) {
        if !matches!(result, ProviderResult::Patch { .. }) {
            return;
        }
        if let Some(IntegrationTask { step, .. }) = task.integration.as_mut() {
            if let IntegrationStep::Resolving { conflict_run_id } = step {
                *step = IntegrationStep::Continuing {
                    conflict_run_id: conflict_run_id.clone(),
                    resolution_run_id: run.id.clone(),
                };
                task.state = TaskState::Ready;
                task.blocked_code = None;
            }
        }
    }
}
