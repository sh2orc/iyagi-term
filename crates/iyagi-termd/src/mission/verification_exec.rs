//! Frozen deterministic-command contracts on the shared durable supervisor.
use super::{
    service::MissionService,
    workflow::{self, MissionEntities, VerificationRequest},
};
use crate::exec::{
    ExecError, ExecProbe, ExecSupervisor, SpawnRequest, StreamKind, DEFAULT_SPOOL_BYTES,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use term_contracts::{
    ids::U64String,
    launch::{Enforcement, LaunchPolicy},
    mission::{types::*, MissionErrorCode, MissionRpcError},
};

#[derive(Clone)]
pub(super) struct Executor {
    pub supervisor: Arc<ExecSupervisor>,
    pub runtime: tokio::runtime::Handle,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Launch {
    kind: String,
    version: u32,
    run_id: Id,
    task_id: Id,
    workspace_id: Id,
    candidate_id: Id,
    #[serde(default)]
    candidate_commit_oid: Option<String>,
    command: VerificationCommand,
    timeout_ms: u64,
    program: String,
    cwd: String,
    pub resource_policy: LaunchPolicy,
    #[serde(default)]
    isolation: Option<super::verification_isolation::Isolation>,
}

pub(super) struct Executed {
    pub raw: workflow::RawRun,
    pub environment: serde_json::Value,
    pub input_integrity: InputIntegrity,
}

fn error(code: MissionErrorCode, message: &str) -> MissionRpcError {
    MissionRpcError::new(code, message)
}

pub(super) fn load(
    service: &MissionService,
    snapshot: &MissionEntities,
    run: &Run,
) -> Result<Launch, MissionRpcError> {
    let body = service
        .artifacts
        .read_mission_body(&run.mission_id, &run.context_ref, 256 * 1024)
        .map_err(|(code, message)| MissionRpcError::new(code, message))?;
    let launch: Launch = serde_json::from_slice(&body).map_err(|_| {
        error(
            MissionErrorCode::IntegrityFailed,
            "invalid verification launch contract",
        )
    })?;
    if launch.kind != "verification_exec"
        || !matches!(launch.version, 1 | 2)
        || (launch.version == 1 && launch.isolation.is_some())
        || (launch.version == 2
            && !snapshot.workspaces.iter().any(|w| {
                w.id == launch.workspace_id
                    && launch.isolation.as_ref().is_some_and(|i| {
                        Path::new(&w.path)
                            .canonicalize()
                            .ok()
                            .is_some_and(|root| i.valid(&root, launch.command.allowed_network))
                    })
            }))
        || (launch.version == 2
            && !snapshot.candidates.iter().any(|c| {
                c.id == launch.candidate_id
                    && launch.candidate_commit_oid.as_ref() == Some(&c.commit_oid)
            }))
        || run.binding_snapshot.is_some()
        || launch.run_id != run.id
        || launch.task_id != run.task_id
        || Some(&launch.workspace_id) != run.workspace_id.as_ref()
        || launch.command.repository_id != snapshot.mission.repository_id
        || !snapshot.tasks.iter().any(|t| {
            t.id == run.task_id
                && t.kind == TaskKind::Verify
                && t.binding_id.is_none()
                && t.contract.verification_ids == [launch.command.id.clone()]
        })
        || !snapshot.workspaces.iter().any(|w| {
            w.id == launch.workspace_id
                && w.kind == WorkspaceKind::Verification
                && w.owned_by_daemon
        })
        || !snapshot
            .candidates
            .iter()
            .any(|c| c.id == launch.candidate_id)
    {
        return Err(error(
            MissionErrorCode::IntegrityFailed,
            "verification launch lost its task, candidate or workspace binding",
        ));
    }
    Ok(launch)
}

impl Launch {
    pub(super) fn matches(&self, record: &ExecRecord, manifest: &serde_json::Value) -> bool {
        if let Some(isolation) = &self.isolation {
            // Exact argv equality stays deterministic across restarts: the
            // seatbelt profile is rebuilt only from the frozen contract (the
            // credential denies are recorded at prepare, never re-read from
            // the recovering daemon's HOME or filesystem).
            let (program, argv) = isolation.launch(&self.program, &self.command.argv);
            return record.resource_policy == self.resource_policy
                && manifest["program"] == serde_json::json!(program)
                && manifest["cwd"].as_str() == Some(&self.cwd)
                && manifest["argv"] == serde_json::json!(argv)
                && manifest["env_keys"]
                    == serde_json::json!(isolation.env.keys().collect::<Vec<_>>())
                && manifest["env_clear"] == true;
        }
        record.resource_policy == self.resource_policy
            && manifest["program"].as_str() == Some(&self.program)
            && manifest["cwd"].as_str() == Some(&self.cwd)
            && manifest["argv"] == serde_json::json!(self.command.argv)
            && manifest["env_keys"] == serde_json::json!([])
            && manifest.get("env_clear").is_none_or(|v| v == false)
    }
    pub(super) fn permits_release(&self, snapshot: &MissionEntities) -> bool {
        snapshot.mission.candidate_id.as_ref() == Some(&self.candidate_id)
            && snapshot
                .mission
                .policy
                .allowed_verification_ids
                .contains(&self.command.id)
            && (!self.command.allowed_network || snapshot.mission.policy.allow_network)
    }
}

fn resolve_program(program: &str) -> Result<PathBuf, MissionRpcError> {
    let input = Path::new(program);
    let candidates: Vec<_> = if input.is_absolute() {
        vec![input.to_path_buf()]
    } else if program.contains('/') || program.contains('\\') {
        return Err(error(
            MissionErrorCode::InvalidArgument,
            "verification executable must be absolute or a bare PATH name",
        ));
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter(|p| p.is_absolute())
            .flat_map(|p| {
                let mut names = vec![p.join(program)];
                if cfg!(windows) && input.extension().is_none() {
                    names.push(p.join(format!("{program}.exe")));
                }
                names
            })
            .collect()
    };
    for candidate in candidates {
        let Ok(metadata) = std::fs::metadata(&candidate) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        if let Ok(path) = candidate.canonicalize() {
            return Ok(path);
        }
    }
    Err(error(
        MissionErrorCode::InvalidArgument,
        "verification executable is unavailable",
    ))
}

fn freeze(
    service: &MissionService,
    request: &VerificationRequest<'_>,
    cwd: &Path,
    isolation: &mut Option<super::verification_isolation::Isolation>,
) -> Result<Launch, MissionRpcError> {
    let program = resolve_program(&request.command.program)?;
    let root = request.worktree.canonicalize().map_err(|_| {
        error(
            MissionErrorCode::InvalidArgument,
            "verification workspace is unavailable",
        )
    })?;
    let cwd = cwd.canonicalize().map_err(|_| {
        error(
            MissionErrorCode::InvalidArgument,
            "verification cwd is unavailable",
        )
    })?;
    if !cwd.starts_with(&root) {
        return Err(error(
            MissionErrorCode::PolicyDenied,
            "verification cwd escapes its owned workspace",
        ));
    }
    let snapshot = workflow::load_entities(&service.storage, request.mission_id)?;
    let mut run = snapshot
        .runs
        .iter()
        .find(|r| &r.id == request.verify_run_id)
        .cloned()
        .ok_or_else(|| error(MissionErrorCode::NotFound, "verification Run is missing"))?;
    let mut workspace = snapshot
        .workspaces
        .iter()
        .find(|w| Some(&w.id) == run.workspace_id.as_ref())
        .cloned()
        .ok_or_else(|| {
            error(
                MissionErrorCode::PolicyDenied,
                "verification has no reserved workspace",
            )
        })?;
    if snapshot.mission.state != MissionState::Running
        || snapshot.mission.candidate_id.as_ref() != Some(request.candidate_id)
        || run.state != RunState::Starting
        || run.exec_id.is_some()
        || workspace.state != WorkspaceState::Preparing
        || workspace.kind != WorkspaceKind::Verification
        || workspace.writer_run_id.as_ref() != Some(&run.id)
        || workspace.lease_token != run.fencing_token
        || Path::new(&workspace.path).canonicalize().ok().as_ref() != Some(&root)
    {
        return Err(error(
            MissionErrorCode::InvalidState,
            "verification changed before launch preparation",
        ));
    }
    if request.command.env_profile_ref.is_some() {
        return Err(error(
            MissionErrorCode::CapabilityUnsupported,
            "verification environment profiles are not configured",
        ));
    }
    if isolation.is_none() {
        *isolation = Some(super::verification_isolation::Isolation::prepare(
            &root,
            request.command.allowed_network,
        )?);
    }
    let launch = Launch {
        kind: "verification_exec".into(),
        version: 2,
        run_id: run.id.clone(),
        task_id: run.task_id.clone(),
        workspace_id: workspace.id.clone(),
        candidate_id: request.candidate_id.clone(),
        candidate_commit_oid: Some(
            snapshot
                .candidates
                .iter()
                .find(|c| c.id == *request.candidate_id)
                .ok_or_else(|| {
                    error(
                        MissionErrorCode::IntegrityFailed,
                        "verification candidate disappeared",
                    )
                })?
                .commit_oid
                .clone(),
        ),
        command: request.command.clone(),
        timeout_ms: request
            .command
            .timeout_ms
            .min(snapshot.mission.policy.run_time_limit_ms.get()),
        program: program.to_string_lossy().into_owned(),
        cwd: cwd.to_string_lossy().into_owned(),
        isolation: isolation.clone(),
        // A transparent admission estimate, with no new implicit hard cap.
        resource_policy: LaunchPolicy {
            reservation_bytes: U64String::new(512 << 20).unwrap(),
            cpu_slots: 1,
            enforcement: Enforcement::Observe,
            memory_max_bytes: None,
            cpu_max_cores: None,
            pids_max: None,
        },
    };
    if !launch.permits_release(&snapshot) {
        return Err(error(
            MissionErrorCode::PolicyDenied,
            "verification launch is outside current policy",
        ));
    }
    run.context_ref = workflow::store_artifact(
        &service.artifacts,
        &run.mission_id,
        "application/json",
        &serde_json::to_vec(&launch).expect("verification contract"),
    )?;
    workspace.state = WorkspaceState::Busy;
    service.commit_actor(
        snapshot.mission,
        "engine.verify_exec_contract",
        vec![
            Entity::Run(Box::new(run)),
            Entity::Workspace(Box::new(workspace)),
        ],
        vec![],
    )?;
    Ok(launch)
}

impl Executor {
    pub(super) fn execute(
        &self,
        service: &MissionService,
        request: &VerificationRequest<'_>,
        cwd: &Path,
        cancel: &AtomicBool,
    ) -> Result<Executed, MissionRpcError> {
        let mut isolation = None;
        let launch = loop {
            match freeze(service, request, cwd, &mut isolation) {
                Err(e) if e.code == MissionErrorCode::RevisionConflict => continue,
                result => break result?,
            }
        };
        let isolation = launch
            .isolation
            .as_ref()
            .expect("new verification launch is isolated");
        let (program, argv) = isolation.launch(&launch.program, &launch.command.argv);
        let spawn = SpawnRequest {
            exec_id: Id::generate(),
            mission_id: request.mission_id.clone(),
            run_id: request.verify_run_id.clone(),
            owner_daemon_id: service.owner_daemon_id.clone(),
            program,
            argv,
            cwd: PathBuf::from(&launch.cwd),
            env_overrides: isolation.env.clone(),
            env_clear: true,
            stdin: None,
            resource_policy: launch.resource_policy.clone(),
            spool_bytes: DEFAULT_SPOOL_BYTES,
            redactor: None,
            sink: Arc::new(|_, _| {}),
            validate_path: None,
        };
        let raw = self.run(spawn, launch.timeout_ms, cancel)?;
        // sandbox-exec returns success only after applying its profile and
        // successfully executing the command. Failed starts are not proof.
        let input_check = crate::workspace::verification::validate(
            request.worktree,
            launch
                .candidate_commit_oid
                .as_deref()
                .expect("new verification candidate is frozen"),
        );
        let input_integrity = if raw.exit_code == Some(0)
            && !raw.cancelled
            && !raw.timed_out
            && input_check.is_ok()
        {
            InputIntegrity::Enforced
        } else {
            InputIntegrity::Unknown
        };
        Ok(Executed {
            raw,
            input_integrity,
            environment: serde_json::json!({
                "cwd":launch.cwd,"worktree":request.worktree,"input_integrity":input_integrity,
                "integrity_basis":"macos_seatbelt_v1 denies file writes except the separate owned output directory",
                "isolation":isolation,"env_clear":true,"input_matches_candidate":input_check.is_ok(),
                "input_error":input_check.err().map(|e|e.to_string()),
            }),
        })
    }

    pub(super) fn run(
        &self,
        spawn: SpawnRequest,
        timeout_ms: u64,
        cancel: &AtomicBool,
    ) -> Result<workflow::RawRun, MissionRpcError> {
        let handle = loop {
            if cancel.load(Ordering::Acquire) {
                let mut cancelled = error(
                    MissionErrorCode::InvalidState,
                    "deterministic command was cancelled before launch",
                );
                cancelled.details.reason_code =
                    Some("deterministic_cancelled_before_launch".into());
                return Err(cancelled);
            }
            match self.supervisor.spawn_on(spawn.clone(), &self.runtime) {
                Ok(handle) => break handle,
                Err(ExecError::AdmissionDenied { .. }) => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(MissionRpcError::new(
                        MissionErrorCode::ProviderUnavailable,
                        format!("deterministic command launch: {e}"),
                    ))
                }
            }
        };
        let started = Instant::now();
        let timeout = Duration::from_millis(timeout_ms);
        let mut timed_out = false;
        let mut cancelled = false;
        let exit_code = loop {
            if let ExecProbe::Finished { exit } = handle.inspect() {
                break exit;
            }
            cancelled |= cancel.load(Ordering::Acquire);
            timed_out |= started.elapsed() >= timeout;
            if cancelled || timed_out {
                // Keep the worker/lease until the final Exec commit succeeds.
                match handle.stop_blocking(Duration::from_secs(10), Duration::from_secs(5)) {
                    Ok(_) => {}
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        Ok(workflow::RawRun {
            exit_code,
            timed_out,
            cancelled,
            stdout_total: handle.stream_stats(StreamKind::Stdout).0,
            stderr_total: handle.stream_stats(StreamKind::Stderr).0,
            stdout_tail: handle.take_output(),
            stderr_tail: handle.take_diagnostics(),
        })
    }
}
