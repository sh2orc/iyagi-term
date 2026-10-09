//! # Exec supervisor (ticket O07)
//!
//! PTY-less pipe-process lifecycle for mission runs (00 §4, 02 §9, 09 §3):
//! `tokio::process` children with piped stdin/stdout/stderr, explicit cwd +
//! argv (never a shell string), bounded output spools, PID/start-token/boot
//! identity, and the cancellation ladder that always confirms the reap
//! before the reservation is released.
//!
//! Layering: the supervisor never touches `term-storage` — every
//! `orch_execs`-shaped [`ExecRecord`] transition is handed to the injected
//! fallible [`persistence::ExecPersistence`] store. Production launches wait
//! behind a helper gate until prepared ownership and native identity commit.
//! Admission runs on a private [`ExecLedger`] with `term-core` admission
//! semantics: the same process is counted exactly once because the R1
//! workload ledger and this ledger are separate instances (00 §4).

pub mod gated;
#[cfg(target_os = "macos")]
mod guardian;
pub mod input;
mod native_recovery;
pub mod output;
pub mod ownership;
pub mod persistence;
pub mod process;
mod recovery;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::json;
use sha2::{Digest, Sha256};
use term_contracts::ids::{ProcessIdentity, U64String};
use term_contracts::launch::LaunchPolicy;
use term_contracts::mission::types::{
    ArtifactRef, ExecGroupKind, ExecRecord, ExecState, Id, Timestamp,
};
use term_contracts::snapshot::QueueReason;
use term_core::{AdmissionConfig, AdmissionHost, AdmissionInput, AdmissionRequest};
use tokio::process::Command;

pub use output::{
    BoundedSpool, FnRedactor, NoRedactor, OutputVerdict, PathVerdict, Redactor, StreamKind,
    DEFAULT_SPOOL_BYTES, MAX_LINE_BYTES,
};
pub use ownership::ConfirmedExit;
pub use process::ExitInfo as ExecExit;

use crate::exec::output::{PathViolation, StreamTap};

/// Persist hook: the mission service stores the row; the supervisor only
/// reports transitions (Prepared → Spawned → Exited, Stopping on cancel).
pub type PersistExec = Arc<dyn Fn(ExecRecord) + Send + Sync>;

/// Path-policing hook for the O07 fake child's declared writes (O06 replaces
/// this with real workspace capture).
pub type PathValidator = Arc<dyn Fn(&str, u64) -> PathVerdict + Send + Sync>;

/// Artifact-like sink: `(stream, line)` with newline included (cut lines
/// carry no newline). The sink's storage policy (O05) bounds persistence,
/// not this callback.
pub type OutputSink = Arc<dyn Fn(StreamKind, &[u8]) + Send + Sync>;

/// Supervision errors. Never a bare bool (03 §1: reasons ride along).
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// Admission refused the launch; the reason mirrors `term-core`.
    #[error("admission denied: {reason:?}")]
    AdmissionDenied { reason: QueueReason },
    #[error("exec {exec_id} already holds a reservation")]
    DuplicateReservation { exec_id: Id },
    #[error("spawn rejected: {0}")]
    InvalidSpawn(String),
    #[error("spawn failed: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("stop ladder worker failed: {0}")]
    StopWorker(String),
    /// The stop (or a failed launch's cleanup) completed for the root child,
    /// but owned group members survived the bounded force drain, or an
    /// escaped member still holds the output pipes open after the group was
    /// observed empty. A surviving group is pinned with native recovery,
    /// which keeps retrying Force. The reservation stays held and Exited is
    /// not committed until termination is confirmed: by the group watcher's
    /// finalize, or for a failed launch by native recovery itself
    /// (02-engine.md: no lease release before the confirmed exit).
    #[error(
        "exec {exec_id} stopped with stranded group members (is_empty errors: {empty_errors}, force errors: {force_errors}); the reservation is retained until their exit is confirmed"
    )]
    StopStranded {
        exec_id: Id,
        empty_errors: u32,
        force_errors: u32,
    },
    #[error("exec persistence failed: {0}")]
    Persistence(String),
}

/// Launch request for one pipe child.
#[derive(Clone)]
pub struct SpawnRequest {
    pub exec_id: Id,
    pub mission_id: Id,
    pub run_id: Id,
    /// Daemon instance that owns this exec (ExecRecord.owner_daemon_id).
    pub owner_daemon_id: Id,
    /// Absolute executable path — never shell text.
    pub program: PathBuf,
    /// Arguments excluding the program itself.
    pub argv: Vec<String>,
    /// Existing absolute working directory.
    pub cwd: PathBuf,
    /// Explicit environment overrides on top of the daemon environment;
    /// values are never logged or persisted (09 §3 sanitized manifest).
    pub env_overrides: BTreeMap<String, String>,
    /// Clear inherited variables before applying overrides. Runtime auth
    /// isolation must opt in so ambient provider credentials cannot win.
    pub env_clear: bool,
    /// Payload written to stdin before the pipe closes (e.g. the run prompt).
    pub stdin: Option<Vec<u8>>,
    /// Admission + limits policy (Binding.resource_policy verbatim, 09 §3).
    pub resource_policy: LaunchPolicy,
    /// Retained tail per stream (default 1 MiB, 03 §2).
    pub spool_bytes: usize,
    /// Redaction before stdout/stderr sinks and retained diagnostic tails.
    pub redactor: Option<Arc<dyn Redactor>>,
    /// Artifact-like sink for every (possibly cut) line.
    pub sink: OutputSink,
    /// O07 fake-child write policing hook.
    pub validate_path: Option<PathValidator>,
}

/// Probe result for `inspect` (03 §1: Running|Finished|Absent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecProbe {
    Running,
    Finished { exit: Option<i32> },
    Absent,
}

struct SupervisorInner {
    ledger: ExecLedger,
    persistence: Arc<dyn persistence::ExecPersistence>,
    gate: Option<gated::GateConfig>,
    host: Mutex<AdmissionHost>,
    live: Mutex<HashMap<Id, Weak<ExecHandleInner>>>,
    recovery_guard: Mutex<()>,
    native_recovery: Mutex<native_recovery::NativeRecovery>,
}

/// Supervisor owning pipe children + their admission ledger (09 §3: the
/// exec admission instance is separate from the R1 workload ledger).
#[derive(Clone)]
pub struct ExecSupervisor {
    inner: Arc<SupervisorInner>,
}

impl ExecSupervisor {
    pub fn new(config: AdmissionConfig, persist: PersistExec, host: AdmissionHost) -> Self {
        ExecSupervisor {
            inner: Arc::new(SupervisorInner {
                ledger: ExecLedger::new(config),
                persistence: Arc::new(persistence::Observer(persist)),
                gate: None,
                host: Mutex::new(host),
                live: Mutex::new(HashMap::new()),
                recovery_guard: Mutex::new(()),
                native_recovery: Mutex::new(Default::default()),
            }),
        }
    }

    /// Production constructor: durable preparation and a launch gate are
    /// mandatory together. Admission stays closed until `refresh_recovery`
    /// restores prior reservations. The observer constructor is for deterministic
    /// adapters and legacy callers being migrated to this path.
    pub fn persistent(
        config: AdmissionConfig,
        persistence: Arc<dyn persistence::ExecPersistence>,
        host: AdmissionHost,
        gate: gated::GateConfig,
    ) -> Self {
        let ledger = ExecLedger::new(config);
        ledger.require_recovery();
        Self {
            inner: Arc::new(SupervisorInner {
                ledger,
                persistence,
                gate: Some(gate),
                host: Mutex::new(host),
                live: Mutex::new(HashMap::new()),
                recovery_guard: Mutex::new(()),
                native_recovery: Mutex::new(Default::default()),
            }),
        }
    }

    /// Refresh host telemetry facts used by admission (the daemon samples
    /// these from its telemetry loop; tests inject directly).
    pub fn update_host(&self, host: AdmissionHost) {
        *self.inner.host.lock().unwrap_or_else(|p| p.into_inner()) = host;
    }

    pub fn ledger(&self) -> &ExecLedger {
        &self.inner.ledger
    }

    pub(crate) fn helper_program(&self) -> Option<PathBuf> {
        self.inner
            .gate
            .as_ref()
            .map(|gate| gate.helper_program.clone())
    }

    /// Probe a tracked exec; `Absent` for unknown ids.
    pub fn inspect(&self, exec_id: &Id) -> ExecProbe {
        let weak = self
            .inner
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(exec_id)
            .cloned();
        match weak.and_then(|w| w.upgrade()) {
            Some(handle) => handle.inspect(),
            None => ExecProbe::Absent,
        }
    }

    /// 09 §3 order: reservation → prepared Exec commit → waiting helper →
    /// native group and identity commit → release. Blocking gate/storage
    /// work runs off the async executor; pipes use the current runtime.
    pub async fn spawn(&self, request: SpawnRequest) -> Result<ExecHandle, ExecError> {
        let owner = self.clone();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || owner.spawn_on(request, &runtime))
            .await
            .map_err(|e| ExecError::StopWorker(e.to_string()))?
    }

    /// Sync core of [`Self::spawn`] with an explicit runtime handle — used
    /// by sync trait surfaces (the fake adapter) that may not `await`.
    pub fn spawn_on(
        &self,
        request: SpawnRequest,
        handle: &tokio::runtime::Handle,
    ) -> Result<ExecHandle, ExecError> {
        self.spawn_mode(request, handle, false)
    }

    /// Retain stdin for an ordered bidirectional protocol. The same
    /// admission, persistence, native gate, and cleanup apply as to print.
    pub fn spawn_interactive_on(
        &self,
        request: SpawnRequest,
        handle: &tokio::runtime::Handle,
    ) -> Result<ExecHandle, ExecError> {
        self.spawn_mode(request, handle, true)
    }

    fn spawn_mode(
        &self,
        request: SpawnRequest,
        handle: &tokio::runtime::Handle,
        interactive: bool,
    ) -> Result<ExecHandle, ExecError> {
        if interactive && request.stdin.is_some() {
            return Err(ExecError::InvalidSpawn(
                "interactive stdin cannot also have a one-shot payload".into(),
            ));
        }
        if !request.cwd.is_absolute() || !request.cwd.is_dir() {
            return Err(ExecError::InvalidSpawn(format!(
                "cwd must be an existing absolute directory: {}",
                request.cwd.display()
            )));
        }
        if !request.program.is_absolute() {
            return Err(ExecError::InvalidSpawn(format!(
                "program must be an absolute path: {}",
                request.program.display()
            )));
        }

        if let Some(gate) = &self.inner.gate {
            gated::validate(gate, &request)?;
        }

        // (1) Admission against the current host facts.
        let host = *self.inner.host.lock().unwrap_or_else(|p| p.into_inner());
        let exec_id = request.exec_id.clone();
        let admission = AdmissionRequest {
            reservation_bytes: request.resource_policy.reservation_bytes.get(),
            cpu_slots: request.resource_policy.cpu_slots,
        };
        self.inner
            .ledger
            .try_admit_and_reserve(&host, &exec_id, admission)?;

        // (2) Prepared commit with the sanitized launch manifest (no env
        // values — 09 §3).
        let mut base = RecordBase::new(&request);
        match self
            .inner
            .persistence
            .prepare(base.record(ExecState::Prepared), &base.manifest_body)
        {
            Ok(reference) => base.launch_manifest_ref = reference,
            Err(error) => {
                self.inner.ledger.release(&exec_id);
                return Err(ExecError::Persistence(error.to_string()));
            }
        }

        let (mut child, identity, owned_group, ownership_record) =
            if let Some(gate) = &self.inner.gate {
                match gated::launch(
                    self,
                    gate,
                    &request,
                    &base,
                    self.inner.persistence.as_ref(),
                    handle,
                ) {
                    Ok(launched) => (
                        launched.child,
                        Some(launched.identity),
                        Some(launched.group),
                        launched.record,
                    ),
                    Err(error) => {
                        // A stranded launch cleanup left live members pinned
                        // with native recovery, which releases this
                        // reservation only after their verified exit and
                        // the durable Exited commit.
                        if !matches!(error, ExecError::StopStranded { .. }) {
                            self.inner.ledger.release(&exec_id);
                        }
                        return Err(error);
                    }
                }
            } else {
                // The observer-only test path preserves its direct pipe child.
                let mut command = Command::new(&request.program);
                if request.env_clear {
                    command.env_clear();
                }
                command
                    .args(&request.argv)
                    .kill_on_drop(true)
                    .current_dir(&request.cwd)
                    .envs(&request.env_overrides)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                #[cfg(unix)]
                command.process_group(0);
                let child = {
                    let _entered = handle.enter();
                    match command.spawn() {
                        Ok(child) => child,
                        Err(error) => {
                            self.inner.ledger.release(&exec_id);
                            let mut record = base.record(ExecState::Exited);
                            record.ended_at = Some(now_iso8601());
                            self.inner
                                .persistence
                                .update(record)
                                .map_err(|e| ExecError::Persistence(e.to_string()))?;
                            return Err(ExecError::Spawn(error));
                        }
                    }
                };
                let pid = child.id();
                let identity = pid.and_then(ownership::capture_identity);
                let mut record = base.record(ExecState::Spawned);
                record.identity = identity.clone();
                record.group_kind = Some(ExecGroupKind::ObservedTree);
                record.group_reference = pid.map(|p| p.to_string());
                record.started_at = Some(now_iso8601());
                // Observer persistence is infallible; no production callback can
                // take this ungated path.
                self.inner
                    .persistence
                    .update(record.clone())
                    .map_err(|e| ExecError::Persistence(e.to_string()))?;
                (child, identity, None, record)
            };
        let mut stdin_pipe = child.stdin.take();
        let input = if interactive {
            stdin_pipe
                .take()
                .map(|pipe| input::ExecInput::start(pipe, handle))
        } else {
            None
        };
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let violations = Arc::new(Mutex::new(Vec::new()));
        let stdout_tap = Arc::new(StreamTap::new(
            output::StreamKind::Stdout,
            request.spool_bytes,
            Arc::clone(&request.sink),
            request.redactor.clone(),
            request.validate_path.clone(),
            Arc::clone(&violations),
        ));
        let stderr_tap = Arc::new(StreamTap::new(
            output::StreamKind::Stderr,
            request.spool_bytes,
            Arc::clone(&request.sink),
            request.redactor.clone(),
            None,
            Arc::clone(&violations),
        ));
        let inner = Arc::new(ExecHandleInner {
            exec_id: exec_id.clone(),
            supervisor: Arc::clone(&self.inner),
            slot: Arc::new(process::ChildSlot::new(child)),
            identity: identity.clone(),
            stdout_tap,
            stderr_tap,
            violations,
            ownership_record,
            owned_group,
            finalize_guard: Mutex::new(()),
            finalized: std::sync::atomic::AtomicBool::new(false),
            stranded_registered: std::sync::atomic::AtomicBool::new(false),
            stop_escalated: std::sync::atomic::AtomicBool::new(false),
            streams_pending: std::sync::atomic::AtomicBool::new(false),
            input,
        });

        // (5) Bounded spools registered before the pipes drain (09 §3).
        if let Some(stdout_reader) = stdout_pipe {
            let tap = Arc::clone(&inner.stdout_tap);
            handle.spawn(process::pipe_pump(stdout_reader, tap));
        }
        if let Some(stderr_reader) = stderr_pipe {
            let tap = Arc::clone(&inner.stderr_tap);
            handle.spawn(process::pipe_pump(stderr_reader, tap));
        }

        // Prompt payload then close stdin (print-adapter style runs).
        if let Some(payload) = request.stdin {
            handle.spawn(async move {
                if let Some(mut pipe) = stdin_pipe {
                    use tokio::io::AsyncWriteExt;
                    let _ = pipe.write_all(&payload).await;
                    let _ = pipe.shutdown().await;
                }
            });
        } else if let Some(pipe) = stdin_pipe {
            // No prompt: close immediately so children see EOF.
            drop(pipe);
        }

        self.inner
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(exec_id, Arc::downgrade(&inner));

        let exec = ExecHandle { inner };
        if exec.inner.owned_group.is_some() {
            let owner = exec.clone();
            handle.spawn_blocking(move || owner.watch_owned_group());
        }
        Ok(exec)
    }
}

/// Bound for the post-grace force drain of an owned group's remaining
/// members ([`ExecHandle::stop_blocking`] and the gated launch-cleanup
/// sibling). A member in uninterruptible sleep or a backend with persistent
/// per-member identity errors never reports the group empty; past this
/// deadline the pinned group is registered with native recovery (which
/// retries Force on the reconciliation worker) instead of wedging the stop
/// worker's blocking thread forever. The same bound applies to a stop's
/// wait for the output pipes to close after the group was observed empty
/// (an escaped member, e.g. a reparented setuid-both child, can hold them).
const STRANDED_DRAIN_TIMEOUT: Duration = Duration::from_secs(45);

/// One supervisor-owned exec child. Cloning shares the child.
#[derive(Clone)]
pub struct ExecHandle {
    inner: Arc<ExecHandleInner>,
}

struct ExecHandleInner {
    exec_id: Id,
    supervisor: Arc<SupervisorInner>,
    slot: Arc<process::ChildSlot>,
    identity: Option<ProcessIdentity>,
    stdout_tap: Arc<StreamTap>,
    stderr_tap: Arc<StreamTap>,
    violations: Arc<Mutex<Vec<PathViolation>>>,
    ownership_record: ExecRecord,
    owned_group: Option<gated::OwnedGroup>,
    finalize_guard: Mutex<()>,
    finalized: std::sync::atomic::AtomicBool,
    /// Set once the owned group was handed to native recovery as stranded;
    /// later stops skip the drain so retry loops stay cheap until a stop
    /// observes the group empty again (native recovery finished) and takes
    /// the normal finalize path. Cleared by finalize, which also drops the
    /// stop-path pin so an unobservable group cannot hold a pin slot.
    stranded_registered: std::sync::atomic::AtomicBool,
    /// A stop had to escalate (terminate/kill, remaining members, or a
    /// stranded drain): every later stop reports [`ConfirmedExit::Killed`].
    stop_escalated: std::sync::atomic::AtomicBool,
    /// A stop's bounded wait for the output pipes expired after the group
    /// was observed empty; later stops re-check the pipes without waiting.
    streams_pending: std::sync::atomic::AtomicBool,
    input: Option<input::ExecInput>,
}

impl ExecHandleInner {
    fn inspect(&self) -> ExecProbe {
        match self.slot.poll_exit() {
            Some(info)
                if self.owned_group.is_none()
                    || self.finalized.load(std::sync::atomic::Ordering::Acquire) =>
            {
                ExecProbe::Finished { exit: info.code }
            }
            Some(_) => ExecProbe::Running,
            None => ExecProbe::Running,
        }
    }
}

impl ExecHandle {
    /// Present only for `spawn_interactive_on`. Acknowledgments confirm
    /// pipe writes, not provider acceptance of the enclosed request.
    pub fn input(&self) -> Option<input::ExecInput> {
        self.inner.input.clone()
    }
    pub fn exec_id(&self) -> &Id {
        &self.inner.exec_id
    }

    pub fn identity(&self) -> Option<&ProcessIdentity> {
        self.inner.identity.as_ref()
    }

    /// Running|Finished probe (03 §1). Never blocks.
    pub fn inspect(&self) -> ExecProbe {
        self.inner.inspect()
    }

    /// EOF/error is independent of the retained sink's lifetime. Channel
    /// adapters use this after draining queued stdout to detect completion.
    pub fn stdout_done(&self) -> bool {
        self.inner
            .stdout_tap
            .done
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Wait for a natural exit **and** both pipes drained to EOF, then
    /// finalize exactly once (ledger release + Exited commit, 09 §3).
    pub async fn wait(&self) -> Result<process::ExitInfo, ExecError> {
        loop {
            if let Some(info) = self.inner.slot.poll_exit() {
                if self.streams_done()
                    && (self
                        .inner
                        .finalized
                        .load(std::sync::atomic::Ordering::Acquire)
                        || (self.inner.owned_group.is_none() && self.finalize(info).is_ok()))
                {
                    return Ok(info);
                }
                // The process has exited, but ownership remains reserved
                // while its durable completion cannot be recorded.
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }

    /// Cancellation ladder (02 §9): interrupt → `grace_interrupt` →
    /// terminate → `grace_kill` → kill → unbounded reap. The reservation is
    /// released only after the confirmed exit (never before).
    pub async fn stop(
        &self,
        grace_interrupt: Duration,
        grace_kill: Duration,
    ) -> Result<ConfirmedExit, ExecError> {
        let owned = self.clone();
        tokio::task::spawn_blocking(move || owned.stop_blocking(grace_interrupt, grace_kill))
            .await
            .map_err(|e| ExecError::StopWorker(e.to_string()))?
    }

    /// Sync ladder bridge for sync trait surfaces (e.g. the fake adapter's
    /// `close`): same 02 §9 semantics, blocking the calling thread.
    ///
    /// The post-grace drain of remaining group members is bounded by
    /// [`STRANDED_DRAIN_TIMEOUT`]: past the deadline the pinned group is
    /// registered with native recovery (which keeps retrying Force on the
    /// reconciliation worker) and this call fails with
    /// [`ExecError::StopStranded`] so the blocking thread is released. The
    /// reservation is still released only after the confirmed exit: once
    /// reconciliation observes the group empty, a later stop (caller retry
    /// loop or the group watcher's last-owner branch) re-checks emptiness
    /// first and completes the normal streams_done + finalize path that
    /// commits Exited and releases the reservation. The wait for the output
    /// pipes after the group was observed empty is bounded the same way.
    /// A stop of an already finalized exec returns at once.
    pub fn stop_blocking(
        &self,
        grace_interrupt: Duration,
        grace_kill: Duration,
    ) -> Result<ConfirmedExit, ExecError> {
        use std::sync::atomic::Ordering;
        let stopping_at = std::time::Instant::now();
        // Exited is already durable, the reservation released and the group
        // retired (by a racing stop, `wait`, or the group watcher). Nothing
        // is left to confirm: never re-observe, re-drain or re-strand, so
        // caller retry loops terminate even while later observations fail.
        if self.inner.finalized.load(Ordering::Acquire) {
            let exit = self.inner.slot.poll_exit();
            let killed = self.inner.stop_escalated.load(Ordering::Acquire)
                || exit.is_some_and(|info| info.killed);
            return Ok(if killed {
                ConfirmedExit::Killed {
                    elapsed_ms: stopping_at.elapsed().as_millis(),
                }
            } else {
                ConfirmedExit::Exited {
                    code: exit.and_then(|info| info.code),
                }
            });
        }
        let escalated_before = self.inner.stop_escalated.load(Ordering::Acquire);
        let streams_pending = self.inner.streams_pending.load(Ordering::Acquire);
        // A previous stop already noted Stopping durably and hit the drain
        // deadline. Retries cannot progress the stranded members and must
        // stay cheap (no re-observation, no re-drain) so caller retry loops
        // never wedge another blocking thread.
        let mut stranded = self.inner.owned_group.is_some()
            && self.inner.stranded_registered.load(Ordering::Acquire);
        if stranded {
            // Native recovery may have emptied the group since the flag was
            // set (the root is already reaped at this point, so the ladder
            // and drain below are quick no-ops). Take the normal path so
            // this stop reaches streams_done + finalize (which clears the
            // flag and drops the pin): caller retry loops and the group
            // watcher's last-owner branch then observe success instead of
            // StopStranded forever, and the reservation/live-map entry are
            // released. A group that appears non-empty again simply
            // re-drains and re-registers below.
            if let Some(group) = &self.inner.owned_group {
                if group.is_empty().unwrap_or(false) {
                    stranded = false;
                }
            }
        }
        let mut strand_counts: Option<(u32, u32)> = None;
        if let Some(input) = &self.inner.input {
            input.close();
        }
        let mut stopped_remaining_members = false;
        if self.inner.owned_group.is_some() && !stranded && !streams_pending {
            // The cancellation intent already exists in the mission outbox.
            // A failed Stopping observation never prevents native cleanup;
            // the final Exited commit remains mandatory before release.
            self.note_stopping();
        }
        let confirmed = if let Some(group) = &self.inner.owned_group {
            self.inner.slot.stop_sync_with_group(
                grace_interrupt,
                grace_kill,
                || {
                    let _ = group.stop(term_platform::group::StopPhase::Grace);
                },
                || {
                    let _ = group.stop(term_platform::group::StopPhase::Force);
                },
            )
        } else {
            self.inner.slot.stop_sync(grace_interrupt, grace_kill)
        };
        if !stranded {
            if let Some(group) = &self.inner.owned_group {
                // Root exit can precede descendant exit. Give the remaining
                // owned members their TERM window before escalation.
                if !group.is_empty().unwrap_or(false) {
                    stopped_remaining_members = true;
                    let _ = group.stop(term_platform::group::StopPhase::Grace);
                    let deadline = std::time::Instant::now() + grace_kill;
                    while !group.is_empty().unwrap_or(false) && std::time::Instant::now() < deadline
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                if let Some(counts) = self.force_drain_or_strand(group) {
                    strand_counts = Some(counts);
                    stranded = true;
                }
            }
        }

        let confirmed = if escalated_before
            || stranded
            || stopped_remaining_members
            || matches!(confirmed, ConfirmedExit::Killed { .. })
        {
            // Once a stop escalated, every later stop of this exec reports
            // the kill, even after the root was reaped by that earlier stop.
            self.inner.stop_escalated.store(true, Ordering::Release);
            ConfirmedExit::Killed {
                elapsed_ms: stopping_at.elapsed().as_millis(),
            }
        } else {
            confirmed
        };

        if stranded {
            // The pinned group is registered with native recovery, whose
            // worker keeps retrying Force; the group watcher retries
            // finalize (streams drained + Exited commit + reservation
            // release) once the group empties. Release this stop worker
            // with the distinct outcome instead of spinning here.
            return Err(ExecError::StopStranded {
                exec_id: self.inner.exec_id.clone(),
                empty_errors: strand_counts.map(|(empty, _)| empty).unwrap_or(0),
                force_errors: strand_counts.map(|(_, force)| force).unwrap_or(0),
            });
        }

        let (code, killed) = match &confirmed {
            ConfirmedExit::Exited { code } => (*code, false),
            ConfirmedExit::Killed { .. } => {
                (self.inner.slot.poll_exit().and_then(|info| info.code), true)
            }
        };
        // The group was observed empty, but an escaped member (e.g. a
        // setuid-both child reparented away from the observed tree) can
        // still hold the inherited output pipes. Bound this wait like the
        // drain: the reservation stays held (no finalize), and the group
        // watcher finalizes once the pipes close. Later stops only re-check.
        let streams_wait = if streams_pending {
            Duration::ZERO
        } else {
            STRANDED_DRAIN_TIMEOUT
        };
        let streams_deadline = std::time::Instant::now() + streams_wait;
        while !self.streams_done() {
            if self.inner.owned_group.is_some() && std::time::Instant::now() >= streams_deadline {
                if !self.inner.streams_pending.swap(true, Ordering::AcqRel) {
                    tracing::warn!(
                        exec_id=%self.inner.exec_id,
                        "stop confirmed the owned group empty, but its output pipes stay open; reservation retained until they close"
                    );
                }
                return Err(ExecError::StopStranded {
                    exec_id: self.inner.exec_id.clone(),
                    empty_errors: 0,
                    force_errors: 0,
                });
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.finalize(process::ExitInfo { code, killed })?;
        Ok(confirmed)
    }

    /// Bounded post-grace force drain of the owned group's remaining
    /// members. Returns `None` when the group emptied; `Some((empty_errors,
    /// force_errors))` once the [`STRANDED_DRAIN_TIMEOUT`] deadline expires,
    /// with the pinned group registered for native-recovery reconciliation.
    /// `is_empty` and force errors are counted and surfaced, never silently
    /// discarded: they distinguish members that will not die (uninterruptible
    /// sleep) from a backend that cannot observe them.
    fn force_drain_or_strand(&self, group: &gated::OwnedGroup) -> Option<(u32, u32)> {
        let deadline = std::time::Instant::now() + STRANDED_DRAIN_TIMEOUT;
        let mut empty_errors = 0u32;
        let mut force_errors = 0u32;
        let mut last_error: Option<std::io::Error> = None;
        loop {
            match group.is_empty() {
                Ok(true) => return None,
                Ok(false) => {}
                Err(error) => {
                    empty_errors += 1;
                    last_error = Some(error);
                }
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            if let Err(error) = group.stop(term_platform::group::StopPhase::Force) {
                force_errors += 1;
                last_error = Some(error);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        tracing::warn!(
            exec_id=%self.inner.exec_id, empty_errors, force_errors, last_error=?last_error,
            "stop force drain deadline exceeded; recording the workload as stopped with stranded group members"
        );
        // `register_stranded_group` is an `ExecSupervisor` inherent method
        // (native_recovery); rebuild the supervisor from this exec's pinned
        // inner Arc to reach it.
        let supervisor = ExecSupervisor {
            inner: Arc::clone(&self.inner.supervisor),
        };
        if supervisor.register_stranded_group(&self.inner.exec_id, &group.handle, None) {
            self.inner
                .stranded_registered
                .store(true, std::sync::atomic::Ordering::Release);
        } else {
            tracing::warn!(
                exec_id=%self.inner.exec_id,
                "stranded-group registry is full; the owned-group watcher still polls emptiness"
            );
        }
        Some((empty_errors, force_errors))
    }

    /// Retained stdout tail (lossy UTF-8).
    pub fn take_output(&self) -> String {
        self.inner
            .stdout_tap
            .spool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take_tail()
    }

    /// Retained stderr tail — redacted diagnostics for failure reporting.
    pub fn take_diagnostics(&self) -> String {
        self.inner
            .stderr_tap
            .spool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take_tail()
    }
    /// Stream stats: `(total_bytes, retained_bytes, cut_lines)`.
    pub fn stream_stats(&self, kind: StreamKind) -> (u64, usize, u32) {
        let tap = match kind {
            StreamKind::Stdout => &self.inner.stdout_tap,
            StreamKind::Stderr => &self.inner.stderr_tap,
        };
        (
            tap.total_bytes.load(std::sync::atomic::Ordering::Relaxed),
            tap.spool
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .retained_bytes(),
            tap.cuts.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// 03 §2 verdict: any line cut at the 1 MiB cap invalidates the run
    /// (`RESULT_INVALID`) — success is never inferred from partial output.
    pub fn output_verdict(&self) -> OutputVerdict {
        let cuts = self
            .inner
            .stdout_tap
            .cuts
            .load(std::sync::atomic::Ordering::Relaxed)
            + self
                .inner
                .stderr_tap
                .cuts
                .load(std::sync::atomic::Ordering::Relaxed);
        if cuts == 0 {
            OutputVerdict::Valid
        } else {
            OutputVerdict::Invalid { cuts }
        }
    }

    /// Write-police findings (O07 fake-child declarations; O06 captures the
    /// real diff).
    pub fn take_violations(&self) -> Vec<PathViolation> {
        std::mem::take(
            &mut *self
                .inner
                .violations
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        )
    }

    fn streams_done(&self) -> bool {
        self.inner
            .stdout_tap
            .done
            .load(std::sync::atomic::Ordering::Acquire)
            && self
                .inner
                .stderr_tap
                .done
                .load(std::sync::atomic::Ordering::Acquire)
    }

    fn note_stopping(&self) {
        let _serial = self
            .inner
            .finalize_guard
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if self
            .inner
            .finalized
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        let mut record = self.inner.ownership_record.clone();
        record.state = ExecState::Stopping;
        if let Err(error) = self.inner.supervisor.persistence.update(record) {
            tracing::warn!(exec_id=%self.inner.exec_id,error=%error,"exec stopping observation could not be persisted");
        }
    }

    fn watch_owned_group(self) {
        loop {
            if self
                .inner
                .finalized
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return;
            }
            let group = self
                .inner
                .owned_group
                .as_ref()
                .expect("owned group watcher");
            // Refresh verified descendant identities before polling/reaping
            // the root. Native backends own the group's membership.
            let _ = group.config.platform.member_identities(&group.handle);
            if Arc::strong_count(&self.inner) == 1 {
                // Last external owner was dropped: the watcher still owns
                // cleanup and must not orphan the process tree.
                let _ = self.stop_blocking(Duration::from_millis(100), Duration::from_millis(100));
            } else if let Some(info) = self.inner.slot.poll_exit() {
                if self.streams_done() && self.finalize(info).is_ok() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Exactly-once finalized guard (09 §3): ledger release + Exited commit.
    fn finalize(&self, info: process::ExitInfo) -> Result<(), ExecError> {
        let _serial = self
            .inner
            .finalize_guard
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if self
            .inner
            .finalized
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(());
        }
        if let Some(group) = &self.inner.owned_group {
            if !group.is_empty()? {
                return Err(ExecError::StopWorker("owned group has not exited".into()));
            }
        }
        let mut record = self.inner.ownership_record.clone();
        record.state = ExecState::Exited;
        if let Some(input) = &self.inner.input {
            input.close();
        }
        record.ended_at = Some(now_iso8601());
        record.exit_code = info.code;
        self.inner
            .supervisor
            .persistence
            .update(record)
            .map_err(|e| ExecError::Persistence(e.to_string()))?;
        self.inner
            .finalized
            .store(true, std::sync::atomic::Ordering::Release);
        self.inner.supervisor.ledger.release(&self.inner.exec_id);
        // The empty group and durable Exited record are confirmed above.
        // Release the stop-path recovery pin only after those guarantees hold.
        // Only a registered pin needs the native-recovery lock.
        if self
            .inner
            .stranded_registered
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            ExecSupervisor {
                inner: Arc::clone(&self.inner.supervisor),
            }
            .drop_stranded_pin(&self.inner.exec_id);
        }
        if self.inner.ownership_record.group_identity.is_some() {
            if let Some(group) = &self.inner.owned_group {
                if let Err(error) = group.config.platform.retire_recovered_group(&group.handle) {
                    tracing::warn!(exec_id=%self.inner.exec_id, %error,
                        "persisted execution left an empty native group");
                }
            }
        }
        self.inner
            .supervisor
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.inner.exec_id);
        Ok(())
    }
}

// ---- admission -----------------------------------------------------------

/// One live exec reservation as admission math sees it. `resident_bytes`
/// stays `None` in O07 (no per-exec telemetry yet) so the whole reservation
/// remains pending headroom — conservative, like an unsampled workload.
#[derive(Debug, Clone, Copy)]
struct ReservedExec {
    reservation_bytes: u64,
    cpu_slots: u32,
}

/// Exec admission ledger: decide + reserve atomically (term-core admission
/// semantics, `AdmissionConfig::decide` inside the same critical section as
/// the insert). Deliberately a *separate instance* from the R1 workload
/// ledger — an exec process is counted here exactly once and never twice
/// (00 §4).
pub struct ExecLedger {
    config: AdmissionConfig,
    state: Mutex<LedgerState>,
}

#[derive(Default)]
struct LedgerState {
    reservations: HashMap<Id, ReservedExec>,
    recovered: HashMap<Id, ExecRecord>,
    recovery_ready: bool,
}

/// Info about a live exec reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecReservation {
    pub exec_id: Id,
    pub reservation_bytes: u64,
    pub cpu_slots: u32,
}

impl ExecLedger {
    pub fn new(config: AdmissionConfig) -> Self {
        ExecLedger {
            config,
            state: Mutex::new(LedgerState {
                recovery_ready: true,
                ..Default::default()
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Atomic decide + reserve: nothing is reserved on denial, and the first
    /// failing [`QueueReason`] is reported so the UI can explain the wait.
    pub fn try_admit_and_reserve(
        &self,
        host: &AdmissionHost,
        exec_id: &Id,
        request: AdmissionRequest,
    ) -> Result<ExecReservation, ExecError> {
        let mut state = self.lock();
        if !state.recovery_ready {
            return Err(ExecError::AdmissionDenied {
                reason: QueueReason::WaitTelemetry,
            });
        }
        if state.reservations.contains_key(exec_id) {
            return Err(ExecError::DuplicateReservation {
                exec_id: exec_id.clone(),
            });
        }
        let active: Vec<term_core::ActiveWorkload> = state
            .reservations
            .values()
            .map(|slot| term_core::ActiveWorkload {
                reservation_bytes: slot.reservation_bytes,
                resident_bytes: None,
                cpu_slots: slot.cpu_slots,
            })
            .collect();
        let input = AdmissionInput {
            total_bytes: host.total_bytes,
            available_bytes: host.available_bytes,
            sample_age_ms: host.sample_age_ms,
            reconciliation_required: host.reconciliation_required,
            pressure: host.pressure,
            active,
            request,
        };
        match self.config.decide(&input) {
            term_contracts::snapshot::QueueReason::Admit => {
                state.reservations.insert(
                    exec_id.clone(),
                    ReservedExec {
                        reservation_bytes: request.reservation_bytes,
                        cpu_slots: request.cpu_slots,
                    },
                );
                Ok(ExecReservation {
                    exec_id: exec_id.clone(),
                    reservation_bytes: request.reservation_bytes,
                    cpu_slots: request.cpu_slots,
                })
            }
            reason => Err(ExecError::AdmissionDenied { reason }),
        }
    }

    /// Release exactly once per reservation (finalized guard, 09 §3).
    pub fn release(&self, exec_id: &Id) -> bool {
        let mut state = self.lock();
        // A reconstructed reservation has no current child handle whose
        // destructor can establish termination. Only recovery evidence releases it.
        if state.recovered.contains_key(exec_id) {
            return false;
        }
        state.reservations.remove(exec_id).is_some()
    }

    pub fn is_active(&self, exec_id: &Id) -> bool {
        self.lock().reservations.contains_key(exec_id)
    }

    pub fn active_count(&self) -> usize {
        self.lock().reservations.len()
    }
}

// ---- record template -------------------------------------------------------

/// Immutable row fields fixed at spawn time.
struct RecordBase {
    id: Id,
    mission_id: Id,
    run_id: Id,
    owner_daemon_id: Id,
    resource_policy: LaunchPolicy,
    launch_manifest_ref: ArtifactRef,
    manifest_body: Vec<u8>,
}

impl RecordBase {
    fn new(request: &SpawnRequest) -> Self {
        let manifest = json!({
            "program": request.program.to_string_lossy(),
            "argv": request.argv,
            "cwd": request.cwd.to_string_lossy(),
            // Sanitized: env VALUES never travel (09 §3).
            "env_keys": request.env_overrides.keys().collect::<Vec<_>>(),
            "env_clear": request.env_clear,
        });
        let body = serde_json::to_vec(&manifest).unwrap_or_default();
        let digest: String = Sha256::digest(&body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        RecordBase {
            id: request.exec_id.clone(),
            mission_id: request.mission_id.clone(),
            run_id: request.run_id.clone(),
            owner_daemon_id: request.owner_daemon_id.clone(),
            resource_policy: request.resource_policy.clone(),
            manifest_body: body.clone(),
            launch_manifest_ref: ArtifactRef {
                id: Id::generate(),
                sha256: digest,
                bytes: clamped_u64_string(body.len() as u64),
                media_type: "application/json".into(),
            },
        }
    }

    fn record(&self, state: ExecState) -> ExecRecord {
        ExecRecord {
            id: self.id.clone(),
            mission_id: self.mission_id.clone(),
            run_id: self.run_id.clone(),
            state,
            identity: None,
            group_kind: None,
            group_reference: None,
            group_identity: None,
            resource_policy: self.resource_policy.clone(),
            launch_manifest_ref: self.launch_manifest_ref.clone(),
            owner_daemon_id: self.owner_daemon_id.clone(),
            started_at: None,
            ended_at: None,
            exit_code: None,
        }
    }
}

/// `U64String::new` clamped to the SQLite i64 bound; the min() makes the
/// constructor infallible and the fallback literal is a static decimal.
fn clamped_u64_string(value: u64) -> U64String {
    U64String::new(value.min(U64String::MAX))
        .unwrap_or_else(|_| U64String::parse("0").expect("static decimal literal parses"))
}

/// UTC ISO-8601 wall clock for ExecRecord columns. Mirrors
/// `term_storage::time::now_iso8601` (the supervisor must not depend on
/// term-storage); fixed width so lexicographic order is chronological.
pub(crate) fn now_iso8601() -> Timestamp {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    iso8601_from_unix(dur.as_secs() as i64, dur.subsec_millis())
}

fn iso8601_from_unix(secs: i64, millis: u32) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60
    )
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_contracts::metrics::PressureLevel;
    use term_core::AdmissionConfig;

    fn healthy_host() -> AdmissionHost {
        AdmissionHost {
            total_bytes: 16 << 30,
            available_bytes: Some(10 << 30),
            sample_age_ms: 0,
            reconciliation_required: false,
            pressure: PressureLevel::Normal,
        }
    }

    fn config() -> AdmissionConfig {
        AdmissionConfig {
            logical_cpus: 8,
            managed_concurrency: 2,
            telemetry_stale_ms: 3_000,
            host_reserve_min_bytes: 2 << 30,
            host_reserve_percent: 15,
            managed_budget_percent: 50,
        }
    }

    #[test]
    fn iso8601_matches_the_storage_format() {
        assert_eq!(iso8601_from_unix(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            iso8601_from_unix(1_700_000_000, 7),
            "2023-11-14T22:13:20.007Z"
        );
    }

    #[test]
    fn ledger_admits_reserves_and_releases_exactly_once() {
        let ledger = ExecLedger::new(config());
        let id = Id::generate();
        ledger
            .try_admit_and_reserve(
                &healthy_host(),
                &id,
                AdmissionRequest {
                    reservation_bytes: 2 << 30,
                    cpu_slots: 1,
                },
            )
            .expect("empty host admits");
        assert_eq!(ledger.active_count(), 1);
        let duplicate = ledger
            .try_admit_and_reserve(
                &healthy_host(),
                &id,
                AdmissionRequest {
                    reservation_bytes: 1,
                    cpu_slots: 1,
                },
            )
            .unwrap_err();
        assert!(matches!(duplicate, ExecError::DuplicateReservation { .. }));
        assert!(ledger.release(&id));
        assert!(!ledger.release(&id), "release happens exactly once");
        assert_eq!(ledger.active_count(), 0);
    }

    #[test]
    fn ledger_denial_carries_the_first_failing_reason() {
        let ledger = ExecLedger::new(config());
        let err = ledger
            .try_admit_and_reserve(
                &healthy_host(),
                &Id::generate(),
                AdmissionRequest {
                    reservation_bytes: (8 << 30) + 1,
                    cpu_slots: 1,
                },
            )
            .unwrap_err();
        assert!(matches!(
            err,
            ExecError::AdmissionDenied {
                reason: QueueReason::ResourceUnschedulable
            }
        ));
        assert_eq!(ledger.active_count(), 0, "denial reserves nothing");
    }

    /// Hand-built owned-group handle over the mock platform: the root is a
    /// short-lived shell and both streams are already at EOF, so only the
    /// stop/finalize logic is exercised (no helper gate, no group watcher).
    #[cfg(unix)]
    fn owned_mock_handle(
        runtime: &tokio::runtime::Runtime,
        platform: &Arc<term_platform::group::mock::MockPlatform>,
        group: term_platform::GroupHandle,
    ) -> ExecHandle {
        let gate = gated::GateConfig {
            helper_program: PathBuf::from("/nonexistent/iyagi-launch-helper"),
            directory: std::env::temp_dir(),
            platform: platform.clone(),
            timeout: Duration::from_secs(1),
        };
        let supervisor = ExecSupervisor::persistent(
            config(),
            Arc::new(persistence::Observer(Arc::new(|_record: ExecRecord| {}))),
            healthy_host(),
            gate.clone(),
        );
        let child = {
            let _entered = runtime.enter();
            Command::new("/bin/sh")
                .args(["-c", "exit 0"])
                .kill_on_drop(true)
                .spawn()
                .expect("spawn shell")
        };
        let violations = Arc::new(Mutex::new(Vec::new()));
        let sink: OutputSink = Arc::new(|_kind: StreamKind, _line: &[u8]| {});
        let stdout_tap = Arc::new(StreamTap::new(
            StreamKind::Stdout,
            DEFAULT_SPOOL_BYTES,
            Arc::clone(&sink),
            None,
            None,
            Arc::clone(&violations),
        ));
        let stderr_tap = Arc::new(StreamTap::new(
            StreamKind::Stderr,
            DEFAULT_SPOOL_BYTES,
            sink,
            None,
            None,
            Arc::clone(&violations),
        ));
        for tap in [&stdout_tap, &stderr_tap] {
            tap.done.store(true, std::sync::atomic::Ordering::Release);
        }
        let ownership_record = ExecRecord {
            id: Id::generate(),
            mission_id: Id::generate(),
            run_id: Id::generate(),
            state: ExecState::Spawned,
            identity: None,
            group_kind: Some(ExecGroupKind::ObservedTree),
            group_reference: Some(group.reference.clone()),
            group_identity: None,
            resource_policy: crate::agent_runtime::fake::fake_binding().resource_policy,
            launch_manifest_ref: ArtifactRef {
                id: Id::generate(),
                sha256: "a".repeat(64),
                bytes: clamped_u64_string(2),
                media_type: "application/json".into(),
            },
            owner_daemon_id: Id::generate(),
            started_at: None,
            ended_at: None,
            exit_code: None,
        };
        ExecHandle {
            inner: Arc::new(ExecHandleInner {
                exec_id: ownership_record.id.clone(),
                supervisor: Arc::clone(&supervisor.inner),
                slot: Arc::new(process::ChildSlot::new(child)),
                identity: None,
                stdout_tap,
                stderr_tap,
                violations,
                ownership_record,
                owned_group: Some(gated::OwnedGroup {
                    config: gate,
                    handle: group,
                }),
                finalize_guard: Mutex::new(()),
                finalized: std::sync::atomic::AtomicBool::new(false),
                stranded_registered: std::sync::atomic::AtomicBool::new(false),
                stop_escalated: std::sync::atomic::AtomicBool::new(false),
                streams_pending: std::sync::atomic::AtomicBool::new(false),
                input: None,
            }),
        }
    }

    /// Once finalized, a stop never re-observes, re-drains or re-strands, so
    /// caller retry loops terminate even while observations would claim live
    /// members; and a stop that had to escalate keeps reporting the kill.
    #[cfg(unix)]
    #[test]
    fn stop_after_finalize_returns_at_once_with_the_escalated_outcome() {
        use term_platform::group::mock::{MockMember, MockPlatform};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let platform = Arc::new(MockPlatform::new(
            term_contracts::snapshot::Capabilities::observe_only("mock"),
        ));
        let group = platform.create_anonymous_group().expect("mock group");
        // A descendant outlives the root: the first stop must escalate.
        platform.set_members(
            &group.reference,
            vec![MockMember::alive(ProcessIdentity {
                pid: 4129,
                start_token: "start".into(),
                boot_id: "boot".into(),
            })],
        );
        let handle = owned_mock_handle(&runtime, &platform, group.clone());
        let first = handle
            .stop_blocking(Duration::from_millis(2_000), Duration::from_millis(20))
            .expect("first stop");
        assert!(matches!(first, ConfirmedExit::Killed { .. }), "{first:?}");
        assert!(handle
            .inner
            .finalized
            .load(std::sync::atomic::Ordering::Acquire));
        let signals = platform.terminate_log().len();

        // Observations now claim live members: only the finalized fast path
        // can return promptly (a re-drain would spin for the full deadline).
        platform.script_is_empty(&group.reference, vec![false; 10_000]);
        let started = std::time::Instant::now();
        let again = handle
            .stop_blocking(Duration::from_millis(2_000), Duration::from_millis(20))
            .expect("a finalized exec stops at once");
        assert!(matches!(again, ConfirmedExit::Killed { .. }), "{again:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(platform.terminate_log().len(), signals, "no re-drain");
    }
}
