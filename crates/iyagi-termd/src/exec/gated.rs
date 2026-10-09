//! Pipe-mode use of the existing launch-helper gate and resource platform.
//! Target argv/env are sent over the private nonce-authenticated gate; the
//! target cannot run before the helper's group identity has been committed.

use super::{ExecError, ExecSupervisor, RecordBase, SpawnRequest};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use term_contracts::gate::GateTarget;
use term_contracts::ids::{ProcessIdentity, SessionId, WorkloadId};
use term_contracts::launch::Enforcement;
use term_contracts::mission::types::{ExecGroupKind, ExecRecord, ExecState};
use term_contracts::snapshot::LimitSupport;
use term_contracts::workload::{GroupKind, WorkloadDescriptor};
use term_platform::{GroupHandle, ResourcePlatform};
use term_pty::gate::{GateServer, StartOutcome};
use tokio::process::{Child, Command};

#[derive(Clone)]
pub struct GateConfig {
    pub helper_program: PathBuf,
    pub directory: PathBuf,
    pub platform: Arc<dyn ResourcePlatform>,
    pub timeout: Duration,
}

pub(super) struct OwnedGroup {
    pub config: GateConfig,
    pub handle: GroupHandle,
}
impl OwnedGroup {
    pub fn is_empty(&self) -> std::io::Result<bool> {
        self.config.platform.is_empty(&self.handle)
    }
    pub fn stop(&self, phase: term_platform::group::StopPhase) -> std::io::Result<()> {
        self.config.platform.terminate_owned(&self.handle, phase)
    }
}

pub(super) struct GatedChild {
    pub child: Child,
    pub identity: ProcessIdentity,
    pub group: OwnedGroup,
    pub record: ExecRecord,
}

pub(super) fn validate(config: &GateConfig, request: &SpawnRequest) -> Result<(), ExecError> {
    if !config.helper_program.is_absolute() || !config.helper_program.is_file() {
        return Err(ExecError::InvalidSpawn(
            "launch helper must be an existing absolute executable".into(),
        ));
    }
    if request.resource_policy.enforcement == Enforcement::Require {
        let capabilities = config.platform.capabilities();
        let unsupported = (request.resource_policy.memory_max_bytes.is_some()
            && capabilities.memory_limit_kind.support != LimitSupport::Supported)
            || (request.resource_policy.cpu_max_cores.is_some()
                && capabilities.cpu_quota.support != LimitSupport::Supported)
            || (request.resource_policy.pids_max.is_some()
                && capabilities.process_count_limit.support != LimitSupport::Supported);
        if unsupported {
            return Err(ExecError::InvalidSpawn(
                "required OS resource limits are unsupported".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn launch(
    supervisor: &ExecSupervisor,
    config: &GateConfig,
    request: &SpawnRequest,
    base: &RecordBase,
    persistence: &dyn super::persistence::ExecPersistence,
    runtime: &tokio::runtime::Handle,
) -> Result<GatedChild, ExecError> {
    let mut cleanup_record = base.record(ExecState::Prepared);
    let mut cleanup_group = None;
    let result = launch_inner(
        supervisor,
        config,
        request,
        base,
        persistence,
        runtime,
        &mut cleanup_record,
        &mut cleanup_group,
    );
    // A stranded cleanup is not finished: members outlived the bounded drain.
    // Native recovery owns its pinned group, cleanup record and reservation,
    // and commits Exited, releases and retires only after a verified empty
    // observation. Writing Exited here would drop the row from restart
    // recovery while the members still run.
    let stranded = matches!(result, Err(ExecError::StopStranded { .. }));
    if result.is_err() && !stranded {
        cleanup_record.state = ExecState::Exited;
        cleanup_record.ended_at = Some(super::now_iso8601());
        // No target can escape the cleanup path below. Keep the reservation
        // and the starting worker until that fact is durably recorded.
        let mut attempts = 0u64;
        while let Err(error) = persistence.update(cleanup_record.clone()) {
            if attempts.is_multiple_of(50) {
                tracing::warn!(exec_id=%cleanup_record.id, error=%error,
                    "launch cleanup is confirmed; durable completion pending, reservation retained");
            }
            attempts = attempts.saturating_add(1);
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Some(group) = cleanup_group {
            if let Err(error) = config.platform.retire_recovered_group(&group) {
                tracing::warn!(%error, "persisted launch cleanup left an empty native group");
            }
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn launch_inner(
    supervisor: &ExecSupervisor,
    config: &GateConfig,
    request: &SpawnRequest,
    base: &RecordBase,
    persistence: &dyn super::persistence::ExecPersistence,
    runtime: &tokio::runtime::Handle,
    cleanup_record: &mut ExecRecord,
    cleanup_group: &mut Option<GroupHandle>,
) -> Result<GatedChild, ExecError> {
    std::fs::create_dir_all(&config.directory)?;
    let private = tempfile::Builder::new()
        .prefix("e")
        .tempdir_in(&config.directory)?;
    let nonce = term_pty::gate::generate_nonce();
    #[cfg(unix)]
    let endpoint = private.path().join("g").to_string_lossy().into_owned();
    #[cfg(windows)]
    let endpoint = format!(r"\\.\pipe\iyagi-exec-{nonce}");
    #[cfg(not(any(unix, windows)))]
    return Err(ExecError::InvalidSpawn(
        "pipe launch gates are unsupported on this OS".into(),
    ));
    let listener = crate::gate_listener::GateListener::bind(&endpoint, runtime.clone())?;
    let mut policy = request.resource_policy.clone();
    if policy.enforcement == Enforcement::Observe {
        policy.memory_max_bytes = None;
        policy.cpu_max_cores = None;
        policy.pids_max = None;
    }
    let descriptor = WorkloadDescriptor {
        workload_id: WorkloadId::parse(request.exec_id.as_str())
            .map_err(|_| ExecError::InvalidSpawn("invalid exec id".into()))?,
        session_id: SessionId::generate(),
        cwd: request.cwd.to_string_lossy().into_owned(),
        program: request.program.to_string_lossy().into_owned(),
        argv: request.argv.clone(),
        env_overrides: Default::default(),
        cols: 80,
        rows: 24,
        policy,
    };
    let mut group = config.platform.create_group(&descriptor)?;
    #[cfg(target_os = "macos")]
    if config.platform.needs_observer_guardian() {
        group = super::guardian::launch(config, &descriptor.workload_id)?;
    }
    let group_identity = config
        .platform
        .recovery_identity(&group)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "native execution recovery identity unavailable");
            None
        });
    if group_identity.is_some() {
        config.platform.retain_group_until_exit(&mut group);
        *cleanup_group = Some(group.clone());
    }
    let mut command = Command::new(&config.helper_program);
    command
        .args(["--launch-helper", &endpoint, &nonce])
        .kill_on_drop(true)
        .current_dir(&request.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = {
        let _entered = runtime.enter();
        command.spawn()?
    };
    let mut attached = false;
    let result = (|| -> Result<(ProcessIdentity, ExecRecord), ExecError> {
        let pid = child
            .id()
            .ok_or_else(|| ExecError::InvalidSpawn("helper PID unavailable".into()))?;
        let identity = super::ownership::capture_identity(pid)
            .ok_or_else(|| ExecError::InvalidSpawn("helper identity unavailable".into()))?;
        cleanup_record.identity = Some(identity.clone());
        cleanup_record.group_reference = Some(group.reference.clone());
        cleanup_record.group_kind = Some(match group.kind {
            GroupKind::Cgroup => ExecGroupKind::Cgroup,
            GroupKind::Job => ExecGroupKind::Job,
            GroupKind::ObservedTree => ExecGroupKind::ObservedTree,
        });
        cleanup_record.group_identity = group_identity.clone();
        cleanup_record.started_at = Some(super::now_iso8601());
        let deadline = Instant::now() + config.timeout;
        let stream = listener.accept(deadline)?;
        let mut gate = GateServer::new(stream);
        gate.wait_hello(&nonce, &identity, deadline)
            .map_err(|_| ExecError::InvalidSpawn("helper handshake failed".into()))?;
        let mut argv = vec![request.program.to_string_lossy().into_owned()];
        argv.extend(request.argv.clone());
        gate.send_target(&GateTarget {
            program: request.program.to_string_lossy().into_owned(),
            argv,
            env_overrides: request.env_overrides.clone(),
            env_clear: request.env_clear,
            env_remove: Vec::new(),
            cwd: request.cwd.to_string_lossy().into_owned(),
        })
        .map_err(|_| ExecError::InvalidSpawn("helper target delivery failed".into()))?;
        config.platform.attach_waiting_helper(&group, &identity)?;
        attached = true;
        let mut record = base.record(ExecState::Spawned);
        record.identity = Some(identity.clone());
        record.group_reference = Some(group.reference.clone());
        record.group_kind = Some(match group.kind {
            GroupKind::Cgroup => ExecGroupKind::Cgroup,
            GroupKind::Job => ExecGroupKind::Job,
            GroupKind::ObservedTree => ExecGroupKind::ObservedTree,
        });
        record.group_identity = cleanup_record.group_identity.clone();
        record.started_at = cleanup_record.started_at.clone();
        *cleanup_record = record.clone();
        if let Err(error) = persistence.update(record.clone()) {
            let _ = gate.abort();
            return Err(ExecError::Persistence(error.to_string()));
        }
        gate.release(|| Ok(()))
            .map_err(|_| ExecError::InvalidSpawn("helper release was not confirmed".into()))?;
        match gate.await_started(deadline) {
            Ok(StartOutcome::Started) => Ok((identity, record)),
            // Unix exec closes the authenticated gate's CLOEXEC socket.
            // This is transport handoff, not a successful provider result:
            // the supervisor still owns/reaps the child and validates output.
            #[cfg(unix)]
            Err(term_pty::gate::GateError::Eof) => Ok((identity, record)),
            Ok(StartOutcome::StartFailed { .. }) => {
                Err(ExecError::InvalidSpawn("target could not start".into()))
            }
            Err(_) => Err(ExecError::InvalidSpawn(
                "target start was not confirmed".into(),
            )),
        }
    })();
    match result {
        Ok((identity, record)) => Ok(GatedChild {
            child,
            identity,
            group: OwnedGroup {
                config: config.clone(),
                handle: group,
            },
            record,
        }),
        Err(error) => {
            // The helper is ours, even if identity persistence failed. Abort
            // prevents target exec; native ownership handles contain any
            // target that raced a transport loss after RELEASE.
            let _ = config
                .platform
                .terminate_owned(&group, term_platform::group::StopPhase::Force);
            let _ = child.start_kill();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            // Before attach, the target never received RELEASE and only
            // the reaped helper existed. An observed-tree group has no root
            // at this point and cannot answer is_empty yet. The force drain
            // is bounded (STRANDED_DRAIN_TIMEOUT): members that never report
            // empty are handed to native recovery instead of wedging this
            // launch worker's thread forever.
            if attached {
                let mut deadline = Instant::now() + super::STRANDED_DRAIN_TIMEOUT;
                let mut empty_errors = 0u32;
                let mut force_errors = 0u32;
                loop {
                    match config.platform.is_empty(&group) {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(_) => empty_errors += 1,
                    }
                    if Instant::now() >= deadline {
                        // Not finished: keep the row non-exited (Stopping) so
                        // a later daemon generation still owns it, even if
                        // this observation cannot be persisted.
                        if cleanup_record.state != ExecState::Stopping {
                            cleanup_record.state = ExecState::Stopping;
                            if let Err(stopping) = persistence.update(cleanup_record.clone()) {
                                tracing::warn!(exec_id=%base.id, error=%stopping,
                                    "launch cleanup stopping observation could not be persisted");
                            }
                        }
                        // Pin every group, with or without a recovery
                        // identity: native recovery keeps forcing it, then
                        // commits Exited, releases the reservation (kept by
                        // spawn) and retires the group on a verified empty.
                        if supervisor.register_stranded_group(
                            &base.id,
                            &group,
                            Some(cleanup_record.clone()),
                        ) {
                            tracing::warn!(
                                exec_id=%base.id, empty_errors, force_errors, launch_error=%error,
                                "launch cleanup force drain deadline exceeded; native recovery owns the stranded members and the reservation"
                            );
                            return Err(ExecError::StopStranded {
                                exec_id: base.id.clone(),
                                empty_errors,
                                force_errors,
                            });
                        }
                        // No pin slot: nothing else would ever settle this
                        // launch, so keep draining here and retry the
                        // hand-off after another bounded period.
                        tracing::warn!(
                            exec_id=%base.id, empty_errors, force_errors,
                            "stranded-group registry full; launch cleanup keeps draining"
                        );
                        deadline = Instant::now() + super::STRANDED_DRAIN_TIMEOUT;
                    }
                    if config
                        .platform
                        .terminate_owned(&group, term_platform::group::StopPhase::Force)
                        .is_err()
                    {
                        force_errors += 1;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Err(error)
        }
    }
}
