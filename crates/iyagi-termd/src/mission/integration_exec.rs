//! A deterministic integration is a scheduled Task/Run with one owned Exec.
//! The helper owns every Git subprocess, including worktree preparation and
//! manifest capture. Only bounded output from its pipe can publish a result.
use super::{verification_exec::Executor, workflow, MissionService};
use crate::{
    exec::{SpawnRequest, DEFAULT_SPOOL_BYTES},
    workspace::integration::IntegrationSource,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use term_contracts::{
    ids::U64String,
    launch::{Enforcement, LaunchPolicy},
    mission::{types::*, MissionErrorCode, MissionRpcError},
};
use term_storage::mission::types::{OutboxOperation, OutboxState, OutboxUpdate, StoredOutbox};

const LIMIT: usize = 256 * 1024;
fn error(code: MissionErrorCode, message: &str) -> MissionRpcError {
    MissionRpcError::new(code, message)
}
fn integrity(message: &str) -> MissionRpcError {
    error(MissionErrorCode::IntegrityFailed, message)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub(super) kind: String,
    pub(super) version: u32,
    pub(super) mission_id: Id,
    pub(super) repository: String,
    pub(super) base_oid: String,
    pub(super) prior_candidate_id: Option<Id>,
    pub(super) sources: Vec<IntegrationSource>,
}
impl Plan {
    pub(super) fn matches(&self, snapshot: &workflow::MissionEntities) -> bool {
        self.kind == "integration_plan"
            && self.version == 1
            && self.mission_id == snapshot.mission.id
            && self.repository == snapshot.mission.repository_path
            && self.base_oid == snapshot.mission.base_oid
            && self.sources.iter().all(|source| {
                snapshot.candidates.iter().any(|candidate| {
                    candidate.id == source.candidate_id
                        && candidate.mission_id == self.mission_id
                        && candidate.base_oid == source.base_oid
                        && candidate.commit_oid == source.commit_oid
                        && candidate.tree_oid == source.tree_oid
                        && candidate.source_run_ids == source.source_run_ids
                })
            })
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    plan: Plan,
    task_id: Id,
    run_id: Id,
    workspace_id: Id,
    worktree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resume: Option<Resume>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Resume {
    pub applied_count: usize,
    pub conflict_paths: Vec<String>,
    pub resolution_run_ids: Vec<Id>,
    pub allowed_paths: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Launch {
    kind: String,
    version: u32,
    input: Input,
    input_ref: ArtifactRef,
    input_path: String,
    program: String,
    timeout_ms: u64,
    resource_policy: LaunchPolicy,
}
impl Launch {
    fn argv(&self) -> Vec<String> {
        vec![
            "--integration-helper".into(),
            self.input_path.clone(),
            self.input_ref.sha256.clone(),
        ]
    }
    pub(super) fn matches(&self, record: &ExecRecord, manifest: &serde_json::Value) -> bool {
        record.resource_policy == self.resource_policy
            && manifest["program"] == self.program
            && manifest["cwd"] == self.input.plan.repository
            && manifest["argv"] == serde_json::json!(self.argv())
            && manifest["env_keys"] == serde_json::json!([])
            && manifest.get("env_clear").is_none_or(|v| v == false)
    }
    pub(super) fn permits_release(&self, snapshot: &workflow::MissionEntities) -> bool {
        snapshot.mission.phase == Phase::Integrating
            && snapshot.mission.candidate_id == self.input.plan.prior_candidate_id
            && self.input.plan.matches(snapshot)
    }
}

pub(super) fn load(
    service: &MissionService,
    snapshot: &workflow::MissionEntities,
    run: &Run,
) -> Result<Launch, MissionRpcError> {
    let body = service
        .artifacts
        .read_mission_body(&run.mission_id, &run.context_ref, LIMIT)
        .map_err(|(c, m)| MissionRpcError::new(c, m))?;
    let launch: Launch = serde_json::from_slice(&body)
        .map_err(|_| integrity("invalid integration launch contract"))?;
    let input = &launch.input;
    if launch.kind != "integration_exec"
        || launch.version != 1
        || run.binding_snapshot.is_some()
        || input.run_id != run.id
        || input.task_id != run.task_id
        || Some(&input.workspace_id) != run.workspace_id.as_ref()
        || !input.plan.matches(snapshot)
        || !snapshot
            .tasks
            .iter()
            .any(|task| task.id == run.task_id && task.is_internal_integration())
        || !snapshot.workspaces.iter().any(|w| {
            w.id == input.workspace_id
                && w.kind == WorkspaceKind::Integration
                && w.path == input.worktree
                && w.base_oid == input.plan.base_oid
                && w.owned_by_daemon
        })
    {
        return Err(integrity(
            "integration launch lost its task, sources or workspace",
        ));
    }
    let bytes = service
        .artifacts
        .read_mission_body(&run.mission_id, &launch.input_ref, LIMIT)
        .map_err(|(c, m)| MissionRpcError::new(c, m))?;
    if bytes != serde_json::to_vec(input).expect("integration input")
        || service
            .artifacts
            .execution_body_path(&run.mission_id, &launch.input_ref, LIMIT)
            .map_err(|(c, m)| MissionRpcError::new(c, m))?
            != PathBuf::from(&launch.input_path)
    {
        return Err(integrity("integration helper input changed"));
    }
    Ok(launch)
}

pub(super) struct ConflictInput {
    pub workspace: Workspace,
    pub plan: Plan,
    pub resume: Resume,
}

impl MissionService {
    pub(super) fn integration_conflict_input(
        &self,
        snapshot: &workflow::MissionEntities,
        task: &Task,
        conflict_run_id: &Id,
    ) -> Result<ConflictInput, MissionRpcError> {
        let run = snapshot
            .runs
            .iter()
            .find(|r| {
                &r.id == conflict_run_id
                    && r.task_id == task.id
                    && r.state == RunState::Failed
                    && r.ended_at.is_some()
                    && r.binding_snapshot.is_none()
            })
            .ok_or_else(|| integrity("missing failed integration execution"))?;
        let launch = load(self, snapshot, run)?;
        if !launch.permits_release(snapshot)
            || !snapshot.execs.iter().any(|e| {
                Some(&e.id) == run.exec_id.as_ref()
                    && e.run_id == run.id
                    && e.state == ExecState::Exited
                    && e.ended_at.is_some()
            })
        {
            return Err(integrity(
                "conflict execution has no current input or confirmed exit",
            ));
        }
        let reply: Reply = serde_json::from_slice(
            &self
                .artifacts
                .read_mission_body(
                    &run.mission_id,
                    run.result_ref
                        .as_ref()
                        .ok_or_else(|| integrity("missing conflict result"))?,
                    LIMIT,
                )
                .map_err(|(c, m)| error(c, &m))?,
        )
        .map_err(|_| integrity("invalid conflict result"))?;
        if reply.input_sha256 != launch.input_ref.sha256 {
            return Err(integrity("conflict result input changed"));
        }
        let produced = reply
            .result
            .map_err(|_| integrity("integration did not produce a conflict"))?;
        validate_result(&launch.input.plan, &produced)?;
        let (_, conflict_paths) = produced
            .outcome
            .conflict
            .ok_or_else(|| integrity("integration did not report a conflict"))?;
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|w| {
                w.id == launch.input.workspace_id
                    && w.state == WorkspaceState::Retained
                    && w.writer_run_id.is_none()
            })
            .cloned()
            .ok_or_else(|| {
                error(
                    MissionErrorCode::WorkspaceBusy,
                    "conflict workspace is not released",
                )
            })?;
        if snapshot
            .runs
            .iter()
            .any(|r| r.workspace_id.as_ref() == Some(&workspace.id) && r.holds_execution_slot())
        {
            return Err(error(
                MissionErrorCode::WorkspaceBusy,
                "conflict workspace still has an owned execution",
            ));
        }
        let source_ids: Vec<_> = launch
            .input
            .plan
            .sources
            .iter()
            .flat_map(|s| s.source_run_ids.iter())
            .collect();
        let mut allowed_paths: Vec<_> = snapshot
            .tasks
            .iter()
            .filter(|t| {
                snapshot
                    .runs
                    .iter()
                    .any(|r| r.task_id == t.id && source_ids.contains(&&r.id))
            })
            .flat_map(|t| t.contract.allowed_paths.iter().cloned())
            .collect();
        allowed_paths.sort();
        allowed_paths.dedup();
        if allowed_paths.is_empty() {
            return Err(integrity("conflict has no writer scope"));
        }
        let resume = Resume {
            applied_count: produced.outcome.sources.len() + 1,
            conflict_paths,
            resolution_run_ids: launch
                .input
                .resume
                .map_or_else(Vec::new, |r| r.resolution_run_ids),
            allowed_paths,
        };
        Ok(ConflictInput {
            workspace,
            plan: launch.input.plan,
            resume,
        })
    }

    pub(super) fn schedule_integration(
        &self,
        snapshot: &workflow::MissionEntities,
        sources: &[(Id, Vec<Id>)],
    ) -> Result<(), MissionRpcError> {
        let plan = Plan {
            kind: "integration_plan".into(),
            version: 1,
            mission_id: snapshot.mission.id.clone(),
            repository: snapshot.mission.repository_path.clone(),
            base_oid: snapshot.mission.base_oid.clone(),
            prior_candidate_id: snapshot.mission.candidate_id.clone(),
            sources: workflow::freeze_integration_sources(snapshot, sources)?,
        };
        let objective = workflow::store_artifact(
            &self.artifacts,
            &snapshot.mission.id,
            "application/json",
            &serde_json::to_vec(&plan).expect("integration plan"),
        )?;
        let mut task = super::pipeline::task(
            &snapshot.mission,
            &snapshot.tasks,
            TaskKind::Integrate,
            Some(Role::Integrator),
            "Integrate captured candidates".into(),
            objective.clone(),
            snapshot
                .mission
                .role_bindings
                .iter()
                .find(|r| r.role == Role::Integrator)
                .or_else(|| {
                    snapshot
                        .mission
                        .role_bindings
                        .iter()
                        .find(|r| r.role == Role::Builder)
                })
                .map(|r| r.primary_binding_id.clone()),
        );
        if task.binding_id.is_none() {
            return Err(error(
                MissionErrorCode::ModelUnavailable,
                "configure an integrator binding before integration",
            ));
        }
        task.integration = Some(IntegrationTask {
            plan_ref: objective,
            step: IntegrationStep::Automatic,
        });
        task.contract.expected_outputs = vec![ExpectedOutput::Patch];
        task.contract.input_artifact_ids = snapshot
            .candidates
            .iter()
            .filter(|c| plan.sources.iter().any(|s| s.candidate_id == c.id))
            .map(|c| c.manifest_ref.id.clone())
            .collect();
        let mut mission = snapshot.mission.clone();
        mission.phase = Phase::Integrating;
        self.commit_actor(
            mission,
            "engine.plan_integration",
            vec![Entity::Task(Box::new(task))],
            vec![],
        )
    }

    pub(super) fn prepare_integration(
        &self,
        intent: &StoredOutbox,
        root: &Path,
        executor: &Executor,
    ) -> Result<Option<Job>, MissionRpcError> {
        let snapshot = workflow::load_entities(&self.storage, &intent.mission_id)?;
        if snapshot.mission.state != MissionState::Running
            || snapshot.mission.phase != Phase::Integrating
        {
            return Ok(None);
        }
        let Some(mut run) = snapshot
            .runs
            .iter()
            .find(|r| Some(&r.id) == intent.run_id.as_ref())
            .cloned()
        else {
            return Ok(None);
        };
        let Some(mut task) = snapshot.tasks.iter().find(|t| t.id == run.task_id).cloned() else {
            return Ok(None);
        };
        if !task.is_deterministic_integration() {
            return Ok(None);
        }
        if run.state != RunState::Prepared
            || run.dispatch_state != RunDispatchState::Unsent
            || run.fencing_token.get() != intent.fencing_token
            || task.active_run_id.as_ref() != Some(&run.id)
        {
            return Ok(None);
        }
        let plan: Plan = serde_json::from_slice(
            &self
                .artifacts
                .read_mission_body(
                    &run.mission_id,
                    task.integration
                        .as_ref()
                        .map_or(&task.contract.objective_ref, |i| &i.plan_ref),
                    LIMIT,
                )
                .map_err(|(c, m)| MissionRpcError::new(c, m))?,
        )
        .map_err(|_| integrity("invalid integration plan"))?;
        if !plan.matches(&snapshot) || plan.prior_candidate_id != snapshot.mission.candidate_id {
            return Err(integrity("integration plan changed before dispatch"));
        }
        let program = executor
            .supervisor
            .helper_program()
            .ok_or_else(|| {
                error(
                    MissionErrorCode::CapabilityUnsupported,
                    "integration requires a durable native helper",
                )
            })?
            .canonicalize()
            .map_err(|_| integrity("integration helper executable is unavailable"))?;
        let resumed = match task.integration.as_ref().map(|i| &i.step) {
            Some(IntegrationStep::Continuing {
                conflict_run_id,
                resolution_run_id,
            }) => {
                let mut input =
                    self.integration_conflict_input(&snapshot, &task, conflict_run_id)?;
                if serde_json::to_vec(&input.plan).ok() != serde_json::to_vec(&plan).ok() {
                    return Err(integrity("continuation changed its integration plan"));
                }
                let resolution = snapshot
                    .runs
                    .iter()
                    .find(|r| {
                        &r.id == resolution_run_id
                            && r.task_id == task.id
                            && r.state == RunState::Succeeded
                            && r.ended_at.is_some()
                            && r.binding_snapshot.is_some()
                            && r.workspace_id.as_ref() == Some(&input.workspace.id)
                    })
                    .ok_or_else(|| integrity("continuation has no completed integrator result"))?;
                if resolution.exec_id.as_ref().is_some_and(|id| {
                    !snapshot.execs.iter().any(|e| {
                        &e.id == id
                            && e.run_id == resolution.id
                            && e.state == ExecState::Exited
                            && e.ended_at.is_some()
                    })
                }) {
                    return Err(integrity("integrator process has not ended"));
                }
                input
                    .resume
                    .resolution_run_ids
                    .push(resolution_run_id.clone());
                Some(input)
            }
            _ => None,
        };
        let workspace_id = resumed
            .as_ref()
            .map_or_else(Id::generate, |i| i.workspace.id.clone());
        let path = resumed.as_ref().map_or_else(
            || {
                root.join(run.mission_id.as_str())
                    .join("workspaces")
                    .join(format!("integration-{workspace_id}"))
            },
            |i| PathBuf::from(&i.workspace.path),
        );
        let head_oid = resumed
            .as_ref()
            .map_or_else(|| plan.base_oid.clone(), |i| i.workspace.head_oid.clone());
        let input = Input {
            plan,
            task_id: task.id.clone(),
            run_id: run.id.clone(),
            workspace_id: workspace_id.clone(),
            worktree: path.to_string_lossy().into_owned(),
            resume: resumed.map(|i| i.resume),
        };
        let input_ref = workflow::store_artifact(
            &self.artifacts,
            &run.mission_id,
            "application/json",
            &serde_json::to_vec(&input).expect("integration input"),
        )?;
        let input_path = self
            .artifacts
            .execution_body_path(&run.mission_id, &input_ref, LIMIT)
            .map_err(|(c, m)| MissionRpcError::new(c, m))?;
        let launch = Launch {
            kind: "integration_exec".into(),
            version: 1,
            input,
            input_ref,
            input_path: input_path.to_string_lossy().into_owned(),
            program: program.to_string_lossy().into_owned(),
            timeout_ms: snapshot.mission.policy.run_time_limit_ms.get(),
            resource_policy: LaunchPolicy {
                reservation_bytes: U64String::new(512 << 20).unwrap(),
                cpu_slots: 1,
                enforcement: Enforcement::Observe,
                memory_max_bytes: None,
                cpu_max_cores: None,
                pids_max: None,
            },
        };
        run.context_ref = workflow::store_artifact(
            &self.artifacts,
            &run.mission_id,
            "application/json",
            &serde_json::to_vec(&launch).expect("integration launch"),
        )?;
        run.workspace_id = Some(workspace_id.clone());
        run.state = RunState::Starting;
        run.dispatch_state = RunDispatchState::MayHaveSent;
        task.workspace_id = Some(workspace_id.clone());
        let workspace = Workspace {
            id: workspace_id,
            mission_id: run.mission_id.clone(),
            path: launch.input.worktree.clone(),
            kind: WorkspaceKind::Integration,
            base_oid: launch.input.plan.base_oid.clone(),
            head_oid,
            writer_run_id: Some(run.id.clone()),
            lease_token: run.fencing_token.clone(),
            state: WorkspaceState::Busy,
            owned_by_daemon: true,
        };
        let job = Job {
            launch,
            workspace: workspace.clone(),
            token: run.fencing_token.get(),
        };
        self.commit_actor(
            snapshot.mission,
            "engine.claim_integration",
            vec![
                Entity::Run(Box::new(run)),
                Entity::Task(Box::new(task)),
                Entity::Workspace(Box::new(workspace)),
            ],
            vec![OutboxUpdate {
                id: intent.id.clone(),
                expected_state: OutboxState::Prepared,
                state: OutboxState::Sending,
                fencing_token: intent.fencing_token,
            }],
        )?;
        Ok(Some(job))
    }
}

pub(super) struct Job {
    launch: Launch,
    pub workspace: Workspace,
    pub token: u64,
}
impl Job {
    pub(super) fn run_id(&self) -> &Id {
        &self.launch.input.run_id
    }
    pub(super) fn execute(
        self,
        service: &MissionService,
        executor: &Executor,
        cancel: &AtomicBool,
    ) -> Result<(), MissionRpcError> {
        let launch = &self.launch;
        let spawn = SpawnRequest {
            exec_id: Id::generate(),
            mission_id: launch.input.plan.mission_id.clone(),
            run_id: self.run_id().clone(),
            owner_daemon_id: service.owner_daemon_id.clone(),
            program: PathBuf::from(&launch.program),
            argv: launch.argv(),
            cwd: PathBuf::from(&launch.input.plan.repository),
            env_overrides: BTreeMap::new(),
            env_clear: false,
            stdin: None,
            resource_policy: launch.resource_policy.clone(),
            spool_bytes: DEFAULT_SPOOL_BYTES,
            redactor: None,
            sink: Arc::new(|_, _| {}),
            validate_path: None,
        };
        let raw = executor.run(spawn, launch.timeout_ms, cancel)?;
        if raw.cancelled || raw.timed_out {
            return Err(error(
                MissionErrorCode::BudgetExceeded,
                "integration was cancelled or exceeded its time limit",
            ));
        }
        if raw.stdout_total > LIMIT as u64 || raw.stdout_tail.len() > LIMIT {
            return Err(integrity("integration output exceeds its byte budget"));
        }
        let reply: Reply = serde_json::from_str(&raw.stdout_tail)
            .map_err(|_| integrity("integration helper returned invalid output"))?;
        if reply.input_sha256 != launch.input_ref.sha256 {
            return Err(integrity("integration result belongs to another input"));
        }
        let produced = reply.result?;
        if raw.exit_code != Some(0) {
            return Err(integrity("integration helper did not exit successfully"));
        }
        validate_result(&launch.input.plan, &produced)?;
        let expected_resolutions = launch.input.resume.as_ref().map(|r| &r.resolution_run_ids);
        if produced
            .manifest
            .as_ref()
            .is_some_and(|m| match expected_resolutions {
                Some(ids) => m["resolution_run_ids"] != serde_json::json!(ids),
                None => m.get("resolution_run_ids").is_some(),
            })
        {
            return Err(integrity("integration resolution provenance changed"));
        }
        let result_ref = workflow::store_artifact_with_retry(
            &service.artifacts,
            &launch.input.plan.mission_id,
            "application/json",
            raw.stdout_tail.as_bytes(),
            true,
        )?;
        loop {
            let result = (|| {
                let mission = service.read_mission(&launch.input.plan.mission_id)?;
                workflow::publish_integration(
                    service,
                    &service.artifacts,
                    mission,
                    Path::new(&launch.input.worktree),
                    &launch.input.plan.sources,
                    produced.clone(),
                    Some(&Completion {
                        job: &self,
                        result_ref: &result_ref,
                        cancel,
                        head_oid: produced.workspace_head_oid.as_deref(),
                    }),
                )
                .map(|_| ())
            })();
            match result {
                Err(e) if e.code == MissionErrorCode::StorageUnavailable => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                other => return other,
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    input_sha256: String,
    result: Result<workflow::ProducedIntegration, MissionRpcError>,
}

fn validate_result(
    plan: &Plan,
    produced: &workflow::ProducedIntegration,
) -> Result<(), MissionRpcError> {
    if let Some(head) = &produced.workspace_head_oid {
        crate::workspace::git::validate_oid(head)
            .map_err(|_| integrity("invalid integration workspace head"))?;
        if produced
            .integrated
            .as_ref()
            .is_some_and(|c| &c.commit_oid != head)
        {
            return Err(integrity(
                "integrated candidate does not match workspace head",
            ));
        }
    }
    if let Some(candidate) = &produced.integrated {
        if produced.outcome.conflict.is_some()
            || produced.outcome.sources != plan.sources
            || candidate.sources != plan.sources
            || candidate.base_oid != plan.base_oid
            || produced.manifest.as_ref().is_none_or(|m| {
                m["base_oid"] != plan.base_oid
                    || m["commit_oid"] != candidate.commit_oid
                    || m["tree_oid"] != candidate.tree_oid
                    || m["sources"] != serde_json::json!(plan.sources)
            })
        {
            return Err(integrity("integration result changed its frozen sources"));
        }
        crate::workspace::git::validate_oid(&candidate.commit_oid)
            .map_err(|_| integrity("invalid integrated commit"))?;
        crate::workspace::git::validate_oid(&candidate.tree_oid)
            .map_err(|_| integrity("invalid integrated tree"))?;
    } else {
        let count = produced.outcome.sources.len();
        if produced.manifest.is_some()
            || count >= plan.sources.len()
            || produced.outcome.sources != plan.sources[..count]
            || produced.outcome.conflict.as_ref().map(|(id, _)| id)
                != Some(&plan.sources[count].candidate_id)
        {
            return Err(integrity("integration conflict changed its frozen sources"));
        }
    }
    Ok(())
}

fn produce_continuation(
    input: &Input,
    resume: &Resume,
) -> Result<workflow::ProducedIntegration, MissionRpcError> {
    use crate::workspace::{self, git};
    let worktree = Path::new(&input.worktree);
    let repository = Path::new(&input.plan.repository);
    let git_error = |e: workspace::GitError| {
        error(
            MissionErrorCode::IntegrityFailed,
            &format!("integration workspace: {e}"),
        )
    };
    let identity = workspace::repository_identity(worktree).map_err(git_error)?;
    let source = workspace::repository_identity(repository).map_err(git_error)?;
    if identity.common_dir != source.common_dir
        || worktree.canonicalize().ok().as_ref() != Some(&identity.canonical_path)
    {
        return Err(integrity("integration workspace repository changed"));
    }
    if resume.applied_count == 0
        || resume.applied_count > input.plan.sources.len()
        || resume.resolution_run_ids.is_empty()
    {
        return Err(integrity("invalid integration continuation position"));
    }
    workspace::integration::validate_sources(repository, &input.plan.sources, &input.plan.base_oid)
        .map_err(|e| error(MissionErrorCode::IntegrityFailed, &e.to_string()))?;
    // A provider's Patch report cannot silently stage Git's unresolved text.
    // Read only recorded conflict paths, without following a symlink.
    for path in &resume.conflict_paths {
        if !workspace::capture::path_allowed(path, &resume.allowed_paths) {
            return Err(integrity("conflict path exceeds the original writer scope"));
        }
        // Check each component before reading: symlink_metadata on the leaf
        // alone still follows a symlink in an ancestor directory.
        let mut checked = worktree.to_path_buf();
        for component in Path::new(path).components() {
            if !matches!(component, std::path::Component::Normal(_)) {
                return Err(integrity("invalid conflict path component"));
            }
            checked.push(component);
            match std::fs::symlink_metadata(&checked) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(integrity("conflict resolution cannot follow a symlink"));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                Err(_) => return Err(integrity("conflict path cannot be inspected")),
            }
        }
        let path = worktree.join(path);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Ok(m) if m.is_file() => {}
            _ => {
                return Err(integrity(
                    "conflict resolution requires a regular file or an explicit deletion",
                ))
            }
        }
        let mut file =
            std::fs::File::open(&path).map_err(|_| integrity("resolved file cannot be read"))?;
        let mut buffer = [0; 16384];
        let (mut marker, mut count, mut prefix) = (0u8, 0usize, true);
        loop {
            let n = file
                .read(&mut buffer)
                .map_err(|_| integrity("resolved file cannot be read"))?;
            if n == 0 {
                break;
            }
            for &byte in &buffer[..n] {
                if byte == b'\n' {
                    marker = 0;
                    count = 0;
                    prefix = true;
                } else if prefix {
                    if count == 0 && matches!(byte, b'<' | b'>') {
                        marker = byte;
                        count = 1;
                    } else if count > 0 && byte == marker {
                        count = count.saturating_add(1);
                    } else {
                        if byte == b' ' && count >= 7 {
                            return Err(error(
                                MissionErrorCode::ResultInvalid,
                                "conflict markers remain; integrator result was not adopted",
                            ));
                        }
                        prefix = false;
                    }
                }
            }
        }
    }
    workspace::capture(
        worktree,
        &input.plan.mission_id,
        resume.resolution_run_ids.clone(),
        &resume.allowed_paths,
        &input.plan.base_oid,
    )
    .map_err(|e| error(MissionErrorCode::ResultInvalid, &e.to_string()))?;
    let (outcome, integrated) = workspace::integration::continue_integration(
        repository,
        worktree,
        &input.plan.mission_id,
        &input.plan.sources,
        &input.plan.base_oid,
        resume.applied_count,
    )
    .map_err(|e| error(MissionErrorCode::ResultInvalid, &e.to_string()))?;
    let manifest = integrated
        .as_ref()
        .map(|candidate| {
            let mut manifest = workflow::manifest_document(
                worktree,
                &input.plan.base_oid,
                &candidate.commit_oid,
                &candidate.tree_oid,
                &candidate.sources,
            )?;
            manifest["resolution_run_ids"] = serde_json::json!(resume.resolution_run_ids);
            Ok::<_, MissionRpcError>(manifest)
        })
        .transpose()?;
    Ok(workflow::ProducedIntegration {
        outcome,
        integrated,
        manifest,
        workspace_head_oid: Some(git::rev_parse(worktree, "HEAD").map_err(git_error)?),
    })
}

/// Internal daemon mode; input bytes are bound by the persisted launch argv.
pub fn helper(path: &Path, expected_sha256: &str) -> i32 {
    let result = (|| {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(LIMIT as u64 + 1).read_to_end(&mut bytes))
            .map_err(|_| integrity("integration input cannot be read"))?;
        if bytes.len() > LIMIT || format!("{:x}", Sha256::digest(&bytes)) != expected_sha256 {
            return Err(integrity(
                "integration input hash does not match its launch",
            ));
        }
        let input: Input = serde_json::from_slice(&bytes)
            .map_err(|_| integrity("integration input is invalid"))?;
        if input.plan.kind != "integration_plan"
            || input.plan.version != 1
            || !Path::new(&input.worktree).is_absolute()
            || !Path::new(&input.plan.repository).is_absolute()
        {
            return Err(integrity("unsupported integration input"));
        }
        let worktree = Path::new(&input.worktree);
        std::fs::create_dir_all(
            worktree
                .parent()
                .ok_or_else(|| integrity("missing integration workspace parent"))?,
        )
        .map_err(|_| integrity("integration workspace parent cannot be created"))?;
        if let Some(resume) = &input.resume {
            produce_continuation(&input, resume)
        } else {
            workflow::produce_integration(
                Path::new(&input.plan.repository),
                worktree,
                &input.plan.mission_id,
                &input.plan.base_oid,
                &input.plan.sources,
            )
        }
    })();
    let success = result.is_ok();
    let reply = Reply {
        input_sha256: expected_sha256.into(),
        result,
    };
    let Ok(bytes) = serde_json::to_vec(&reply) else {
        return 1;
    };
    let mut output = std::io::stdout().lock();
    if bytes.len() >= LIMIT
        || output
            .write_all(&bytes)
            .and_then(|_| output.write_all(b"\n"))
            .and_then(|_| output.flush())
            .is_err()
    {
        return 1;
    }
    if success {
        0
    } else {
        1
    }
}

pub(super) struct Completion<'a> {
    job: &'a Job,
    result_ref: &'a ArtifactRef,
    cancel: &'a AtomicBool,
    head_oid: Option<&'a str>,
}
impl Completion<'_> {
    pub(super) fn run_id(&self) -> Id {
        self.job.run_id().clone()
    }

    pub(super) fn commit(
        &self,
        service: &MissionService,
        proposed: Mission,
        entities: Vec<Entity>,
    ) -> Result<(), MissionRpcError> {
        let input = &self.job.launch.input;
        loop {
            let result = (|| {
                let snapshot = workflow::load_entities(&service.storage, &input.plan.mission_id)?;
                let mut run = snapshot
                    .runs
                    .iter()
                    .find(|r| r.id == input.run_id)
                    .cloned()
                    .ok_or_else(|| integrity("integration run is missing"))?;
                if run.state.is_terminal() && run.result_ref.as_ref() == Some(self.result_ref) {
                    return Ok(());
                }
                if run.fencing_token.get() != self.job.token
                    || run.state.is_terminal()
                    || !snapshot.execs.iter().any(|e| {
                        Some(&e.id) == run.exec_id.as_ref()
                            && e.run_id == run.id
                            && e.state == ExecState::Exited
                            && e.ended_at.is_some()
                    })
                {
                    return Err(integrity(
                        "integration lost its completed execution ownership",
                    ));
                }
                let mut task = snapshot
                    .tasks
                    .iter()
                    .find(|t| {
                        t.id == input.task_id
                            && t.is_internal_integration()
                            && t.active_run_id.as_ref() == Some(&run.id)
                    })
                    .cloned()
                    .ok_or_else(|| integrity("integration task ownership changed"))?;
                let mut workspace = snapshot
                    .workspaces
                    .iter()
                    .find(|w| {
                        w.id == input.workspace_id
                            && w.writer_run_id.as_ref() == Some(&run.id)
                            && w.lease_token.get() == self.job.token
                    })
                    .cloned()
                    .ok_or_else(|| integrity("integration workspace ownership changed"))?;
                let cancelled = self.cancel.load(Ordering::Acquire)
                    || run.state == RunState::Stopping
                    || task.state == TaskState::Cancelled
                    || matches!(
                        snapshot.mission.state,
                        MissionState::Stopping | MissionState::Cancelled
                    );
                if !cancelled && !self.job.launch.permits_release(&snapshot) {
                    return Err(integrity(
                        "integration candidate changed before publication",
                    ));
                }
                let mut mission = snapshot.mission;
                let conflict = entities
                    .iter()
                    .any(|e| matches!(e, Entity::Decision(d) if d.kind == DecisionKind::Conflict));
                let mut upserts = if cancelled {
                    vec![]
                } else {
                    entities
                        .iter()
                        .filter(|e| !matches!(e, Entity::Workspace(_)))
                        .cloned()
                        .collect::<Vec<_>>()
                };
                if !cancelled {
                    mission.phase = proposed.phase;
                    mission.candidate_id = proposed.candidate_id.clone();
                    mission.open_decision_count += entities
                        .iter()
                        .filter(|e| matches!(e, Entity::Decision(_)))
                        .count() as u32;
                }
                if let Some(head) = self.head_oid {
                    workspace.head_oid = head.into();
                }
                workspace.state = WorkspaceState::Retained;
                workspace.writer_run_id = None;
                task.state = if cancelled {
                    TaskState::Cancelled
                } else if conflict {
                    TaskState::Failed
                } else {
                    TaskState::Succeeded
                };
                task.active_run_id = None;
                task.updated_at = term_storage::time::now_iso8601();
                run.state = if cancelled {
                    RunState::Cancelled
                } else if conflict {
                    RunState::Failed
                } else {
                    RunState::Succeeded
                };
                run.dispatch_state = RunDispatchState::Acknowledged;
                run.ended_at = Some(task.updated_at.clone());
                run.result_ref = Some(self.result_ref.clone());
                if conflict && !cancelled {
                    run.failure_code = Some(MissionErrorCode::ResultInvalid);
                    task.blocked_code = Some("integration_conflict".into());
                }
                let updates = service
                    .storage
                    .mission_outbox()
                    .map_err(MissionService::store_error)?
                    .into_iter()
                    .filter(|i| {
                        i.run_id.as_ref() == Some(&run.id)
                            && matches!(
                                i.operation,
                                OutboxOperation::Start | OutboxOperation::Cancel
                            )
                            && i.state == OutboxState::Sending
                    })
                    .map(|i| OutboxUpdate {
                        id: i.id,
                        expected_state: OutboxState::Sending,
                        state: OutboxState::Acknowledged,
                        fencing_token: i.fencing_token,
                    })
                    .collect();
                upserts.extend([
                    Entity::Run(Box::new(run)),
                    Entity::Task(Box::new(task)),
                    Entity::Workspace(Box::new(workspace)),
                ]);
                service.commit_actor(mission, "engine.integration_result", upserts, updates)
            })();
            match result {
                Err(e) if e.code == MissionErrorCode::RevisionConflict => continue,
                Err(e) if e.code == MissionErrorCode::StorageUnavailable => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                other => return other,
            }
        }
    }
}
