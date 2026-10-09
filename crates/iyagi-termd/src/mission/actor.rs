//! The daemon's mission pump. Adapters are polled independently; a quiet
//! provider never prevents another mission from making progress.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use term_contracts::mission::{types::*, MissionErrorCode, MissionRpcError};
use term_storage::mission::types::{OutboxOperation, OutboxState, OutboxUpdate};

use super::{service::MissionService, workflow};
use crate::agent_runtime::{
    AdapterEvent, AgentAdapter, CancelReceipt, DeliveryReceipt, EventStream, RunProbe, RunStart,
};

#[path = "delivery.rs"]
mod delivery;

struct DeliveryWorker {
    intent: term_storage::mission::types::StoredOutbox,
    handle: std::thread::JoinHandle<DeliveryReceipt>,
}

pub type AdapterFactory =
    Arc<dyn Fn(&RunStart) -> std::io::Result<Arc<dyn AgentAdapter>> + Send + Sync>;

enum StartFailure {
    Preflight(MissionRpcError),
    Provider(std::io::Error),
}

struct LiveRun {
    mission_id: Id,
    workspace: Workspace,
    adapter: Arc<dyn AgentAdapter>,
    stream: EventStream,
    pending: Option<AdapterEvent>,
    closed: bool,
    started: Instant,
    interrupted: Option<Instant>,
    token: u64,
    timeout: Duration,
    starting: Option<std::thread::JoinHandle<Result<(), StartFailure>>>,
    activity: RunActivity,
}

/// Coalesced display output for one live run (audit F1): per-token text
/// deltas accumulate here in memory, and one flush applies them through the
/// fenced activity path as a single bounded tail rewrite. Serialized by the
/// single actor thread — no process-global lock is involved.
#[derive(Default)]
struct RunActivity {
    pending: String,
    /// When the oldest unflushed byte was buffered (flush-window anchor).
    since: Option<Instant>,
    /// Passes a terminal event already waited for a failed final flush.
    terminal_deferrals: u32,
}

/// Flush a run's buffered display text once the oldest byte is this old.
/// Paired with the 250 ms scheduler tick: bounded latency, bounded rate.
const ACTIVITY_FLUSH_WINDOW: Duration = Duration::from_millis(400);
/// Memory bound for unflushed display text. The durable tail itself keeps
/// only the newest 1 MiB, so older buffered text is already droppable.
const ACTIVITY_BUFFER_BYTES: usize = 1024 * 1024;
/// Own-run Activity events buffered per actor pass. Buffering is O(1) and
/// memory-capped, so these do not spend the per-pass event budget; the
/// bound only guarantees a pass ends against a producer that bypasses the
/// adapters' bounded fan-out.
const ACTIVITY_DRAIN_EVENTS: usize = 4096;
/// Passes a terminal event waits for a failed final display flush before
/// it is applied anyway: display text must never hold a run's outcome, or
/// its execution slot, hostage to a persistent storage fault.
const ACTIVITY_TERMINAL_DEFERRALS: u32 = 8;

/// Display text a live run buffers itself: Activity for exactly this run
/// AND fencing token. Tokens repeat across missions (`revision + 1`), so a
/// token match alone could buffer another run's text into this run's tail;
/// anything else takes the fenced apply path, which routes by run id.
fn own_activity<'a>(event: &'a AdapterEvent, run_id: &Id, token: u64) -> Option<&'a str> {
    match event {
        AdapterEvent::Activity {
            run_id: owner,
            fencing_token,
            chunk,
        } if owner == run_id && *fencing_token == token => Some(chunk.as_str()),
        _ => None,
    }
}

impl RunActivity {
    /// Whether a terminal event should wait one more pass because the final
    /// display window failed to flush (text is still buffered). Applying it
    /// now would remove the run and discard that text while
    /// `mission.activity` already reports the tail complete. Bounded by
    /// [`ACTIVITY_TERMINAL_DEFERRALS`].
    fn defer_terminal(&mut self) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        if self.terminal_deferrals >= ACTIVITY_TERMINAL_DEFERRALS {
            tracing::warn!(
                bytes = self.pending.len(),
                "mission activity flush kept failing; terminal event applied without it"
            );
            return false;
        }
        self.terminal_deferrals += 1;
        true
    }

    fn buffer(&mut self, chunk: &str) {
        if self.since.is_none() {
            self.since = Some(Instant::now());
        }
        self.pending.push_str(chunk);
        if self.pending.len() > ACTIVITY_BUFFER_BYTES {
            let mut drop = self.pending.len() - ACTIVITY_BUFFER_BYTES;
            while !self.pending.is_char_boundary(drop) {
                drop += 1;
            }
            self.pending.drain(..drop);
        }
    }
}

struct VerificationWorker {
    mission_id: Id,
    workspace: Workspace,
    token: u64,
    cancel: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<Result<workflow::VerificationRunResult, MissionRpcError>>,
}

struct IntegrationWorker {
    mission_id: Id,
    workspace: Workspace,
    token: u64,
    cancel: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<Result<(), MissionRpcError>>,
}

fn deterministic_failure(id: Id, token: u64, error: MissionRpcError) -> AdapterEvent {
    if error.details.reason_code.as_deref() == Some("deterministic_cancelled_before_launch") {
        AdapterEvent::FailedBeforeSubmission {
            run_id: id,
            fencing_token: token,
            code: error.code,
            message: error.message,
            observed_at_unix_ms: crate::agent_runtime::rate_limits::unix_millis(),
            retry_after_unix_ms: None,
        }
    } else {
        AdapterEvent::Failed {
            run_id: id,
            fencing_token: token,
            code: error.code,
            message: error.message,
        }
    }
}

impl IntegrationWorker {
    fn finish(self, id: Id) -> Option<PendingCompletion> {
        let event = match self.handle.join() {
            Ok(Ok(())) => return None,
            Ok(Err(error)) => deterministic_failure(id, self.token, error),
            // A panic does not prove that a native process or its descendants
            // ended. Retain uncertain ownership for explicit reconciliation.
            Err(_) => AdapterEvent::Disconnected {
                run_id: id,
                fencing_token: self.token,
            },
        };
        Some(PendingCompletion {
            mission_id: self.mission_id,
            workspace: self.workspace,
            event,
        })
    }
}

struct PendingCompletion {
    mission_id: Id,
    workspace: Workspace,
    event: AdapterEvent,
}

pub struct MissionActor {
    service: Arc<MissionService>,
    root: PathBuf,
    factory: AdapterFactory,
    live: HashMap<Id, LiveRun>,
    verifications: HashMap<Id, VerificationWorker>,
    integrations: HashMap<Id, IntegrationWorker>,
    deliveries: HashMap<Id, (term_storage::mission::types::StoredOutbox, DeliveryReceipt)>,
    delivering: HashMap<Id, DeliveryWorker>,
    completions: HashMap<Id, PendingCompletion>,
    dispatch_permitted: bool,
    verification_executor: Option<super::verification_exec::Executor>,
    integration_executor: Option<super::verification_exec::Executor>,
}

impl MissionActor {
    pub fn new(service: Arc<MissionService>, root: PathBuf, factory: AdapterFactory) -> Self {
        Self {
            service,
            root,
            factory,
            live: HashMap::new(),
            verifications: HashMap::new(),
            integrations: HashMap::new(),
            deliveries: HashMap::new(),
            delivering: HashMap::new(),
            completions: HashMap::new(),
            dispatch_permitted: true,
            verification_executor: None,
            integration_executor: None,
        }
    }

    pub fn with_verification_exec(
        mut self,
        supervisor: Arc<crate::exec::ExecSupervisor>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        self.verification_executor = Some(super::verification_exec::Executor {
            supervisor,
            runtime,
        });
        self
    }

    /// Production wiring: all deterministic commands share native ownership.
    pub fn with_deterministic_exec(
        mut self,
        supervisor: Arc<crate::exec::ExecSupervisor>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let executor = super::verification_exec::Executor {
            supervisor,
            runtime,
        };
        self.integration_executor = Some(executor.clone());
        self.verification_executor = Some(executor);
        self
    }

    pub fn live_count(&self) -> usize {
        self.live.len() + self.verifications.len() + self.integrations.len()
    }

    /// Admission recovery can block new work while existing providers,
    /// cancellation, terminal persistence, and control settlement still run.
    pub fn set_dispatch_permitted(&mut self, permitted: bool) {
        self.dispatch_permitted = permitted;
    }

    pub fn tick(&mut self) -> Result<(), MissionRpcError> {
        match self.tick_inner() {
            // User control and verification workers legitimately win CAS
            // races. Recompute on the next pass; never stop the actor.
            Err(error) if error.code == MissionErrorCode::RevisionConflict => Ok(()),
            result => result,
        }
    }

    fn tick_inner(&mut self) -> Result<(), MissionRpcError> {
        self.service.checkpoint_time()?;
        self.collect_deliveries();
        // Process ownership can end before a concurrent user mutation wins
        // CAS. Keep the terminal evidence until its projection is committed.
        for id in self.completions.keys().cloned().collect::<Vec<_>>() {
            let completion = &self.completions[&id];
            match self.service.apply_adapter_event_outcome(
                &completion.mission_id,
                &completion.event,
                Some(&completion.workspace),
            ) {
                Ok(outcome) => {
                    // Only once the projection is committed (11 §7): a lost
                    // CAS above must not leave an observation of a transition
                    // that never happened.
                    if let Some(update) = &outcome.run_evidence {
                        self.service.record_run_evidence(update);
                    }
                    self.completions.remove(&id);
                }
                Err(e) if e.code == MissionErrorCode::RevisionConflict => {}
                Err(e) => return Err(e),
            }
        }
        let delivery_ids: Vec<_> = self.deliveries.keys().cloned().collect();
        for id in delivery_ids {
            let (intent, receipt) = &self.deliveries[&id];
            match self.service.record_delivery(intent, receipt) {
                Ok(()) => {
                    self.deliveries.remove(&id);
                }
                Err(e) if e.code == MissionErrorCode::RevisionConflict => {}
                Err(e) => return Err(e),
            }
        }
        self.service.reconcile_unknown_runs()?;
        self.service.reconcile_task_failures()?;
        self.service.route_messages()?;
        self.dispatch_deliveries()?;
        // Consume a bounded number per actor pass. Pending events stay in
        // memory until their DB commit succeeds, including CAS conflicts.
        let ids: Vec<Id> = self.live.keys().cloned().collect();
        for id in ids {
            let mut remove = false;
            if let Some(live) = self.live.get_mut(&id) {
                if live
                    .starting
                    .as_ref()
                    .is_some_and(|worker| worker.is_finished())
                {
                    let result = live.starting.take().expect("finished starter").join();
                    match result {
                        Ok(Ok(())) => {
                            // Cancellation can arrive before the adapter has
                            // registered its process. Reissue it now that
                            // start completed; the durable gate also rejects
                            // a release after the Run was cancelled.
                            if live.interrupted.is_some() {
                                let _ = live.adapter.interrupt(&id);
                            }
                        }
                        Ok(Err(StartFailure::Preflight(error))) => {
                            // No provider start was attempted. The local CLI
                            // version helper has already completed cleanup.
                            live.closed = true;
                            live.pending = Some(AdapterEvent::FailedBeforeSubmission {
                                run_id: id.clone(),
                                fencing_token: live.token,
                                code: error.code,
                                message: error.message,
                                observed_at_unix_ms: crate::agent_runtime::rate_limits::unix_millis(
                                ),
                                retry_after_unix_ms: None,
                            });
                        }
                        Ok(Err(StartFailure::Provider(error))) => {
                            live.closed = matches!(live.adapter.inspect(&id), RunProbe::Absent)
                                || matches!(
                                    live.adapter.close(&id),
                                    CancelReceipt::Confirmed { .. }
                                );
                            live.pending = Some(if live.closed {
                                if crate::agent_runtime::retry::has_submission_proof(&error) {
                                    crate::agent_runtime::retry::failure(
                                        id.clone(),
                                        live.token,
                                        MissionErrorCode::ProviderUnavailable,
                                        "provider failed before the task request was submitted"
                                            .into(),
                                    )
                                } else {
                                    AdapterEvent::Failed {
                                    run_id: id.clone(),
                                    fencing_token: live.token,
                                    code: match error.kind() {
                                        std::io::ErrorKind::PermissionDenied => {
                                            MissionErrorCode::AuthRequired
                                        }
                                        std::io::ErrorKind::InvalidInput => {
                                            MissionErrorCode::PolicyDenied
                                        }
                                        _ => MissionErrorCode::ProviderUnavailable,
                                    },
                                    message:
                                        "provider could not start with the selected configuration"
                                            .into(),
                                }
                                }
                            } else {
                                AdapterEvent::Disconnected {
                                    run_id: id.clone(),
                                    fencing_token: live.token,
                                }
                            });
                        }
                        Err(_) => {
                            // A panic is not absence evidence. Keep ownership
                            // until close confirms what happened externally.
                            live.pending = Some(AdapterEvent::Disconnected {
                                run_id: id.clone(),
                                fencing_token: live.token,
                            });
                        }
                    }
                }
                // Own-run Activity only appends to the memory-capped buffer,
                // so it does not spend this pass's event budget: a burst of
                // deltas cannot starve the approval or terminal event queued
                // behind it, nor leave the subscriber queue growing.
                let mut budget = if live.starting.is_none() { 64 } else { 0 };
                let mut activity_budget = ACTIVITY_DRAIN_EVENTS;
                let mut deferred_terminal = false;
                while budget > 0 {
                    if live.pending.is_none() {
                        live.pending = live.stream.try_next();
                    }
                    let Some(event) = live.pending.take() else {
                        break;
                    };
                    // Display deltas buffer in memory and are applied once
                    // per flush window; they carry no state transition, so
                    // the mission snapshot is not materialized per token.
                    if let Some(chunk) = own_activity(&event, &id, live.token) {
                        if activity_budget == 0 {
                            live.pending = Some(event);
                            break;
                        }
                        activity_budget -= 1;
                        live.activity.buffer(chunk);
                        continue;
                    }
                    budget -= 1;
                    // Any non-Activity event (notably a terminal one) orders
                    // the buffered display text ahead of the state change it
                    // observes, so the durable tail is complete before the
                    // run's final projection commits. A failed final flush
                    // holds the terminal event for a bounded number of passes
                    // rather than discarding the retained text on removal.
                    MissionActor::flush_activity(&self.service, live, &id);
                    let terminal = event.is_terminal();
                    if terminal && live.activity.defer_terminal() {
                        deferred_terminal = true;
                        live.pending = Some(event);
                        break;
                    }
                    if terminal && !live.closed {
                        if matches!(live.adapter.close(&id), CancelReceipt::Confirmed { .. }) {
                            live.closed = true;
                        } else {
                            live.pending = Some(event);
                            break;
                        }
                    }
                    match self.service.apply_adapter_event_outcome(
                        &live.mission_id,
                        &event,
                        Some(&live.workspace),
                    ) {
                        Ok(outcome) => {
                            // What this Run proved about the installed CLI,
                            // recorded outside the transaction that just
                            // committed and never able to fail it (11 §7).
                            if let Some(update) = &outcome.run_evidence {
                                self.service.record_run_evidence(update);
                            }
                            if terminal {
                                remove = true;
                                break;
                            }
                        }
                        Err(error) if error.code == MissionErrorCode::RevisionConflict => {
                            live.pending = Some(event);
                            break;
                        }
                        Err(error)
                            if terminal && error.code != MissionErrorCode::StorageUnavailable =>
                        {
                            // Invalid model output is a recorded failure; never
                            // silently adopt a partially validated result.
                            live.pending = Some(
                                if error.details.reason_code.as_deref()
                                    == Some(super::plan_repair::FORMAT_REJECTED)
                                    && matches!(event, AdapterEvent::Result { .. })
                                {
                                    let AdapterEvent::Result { result, .. } = event else {
                                        unreachable!()
                                    };
                                    AdapterEvent::InvalidResult {
                                        run_id: id.clone(),
                                        fencing_token: live.token,
                                        code: error.code,
                                        message: error.message,
                                        rejected_result: Some(
                                            serde_json::to_string(&result)
                                                .expect("provider result"),
                                        ),
                                    }
                                } else {
                                    AdapterEvent::Failed {
                                        run_id: id.clone(),
                                        fencing_token: live.token,
                                        code: error.code,
                                        message: error.message,
                                    }
                                },
                            );
                            break;
                        }
                        Err(error) => {
                            // Pending events stay in memory until their commit
                            // succeeds: a terminal or approval event that hit
                            // StorageUnavailable is retried on the next pass,
                            // never dropped (the adapter will not resend it).
                            live.pending = Some(event);
                            return Err(error);
                        }
                    }
                }
                if !remove && live.started.elapsed() >= live.timeout && live.interrupted.is_none() {
                    let _ = live.adapter.interrupt(&id);
                    live.interrupted = Some(Instant::now());
                }
                // A terminal event held this pass for its final display flush
                // is real provider evidence; never replace it with the
                // synthetic time-limit failure.
                if !remove
                    && !deferred_terminal
                    && live
                        .interrupted
                        .is_some_and(|at| at.elapsed() >= Duration::from_secs(10))
                    && matches!(live.adapter.close(&id), CancelReceipt::Confirmed { .. })
                {
                    live.closed = true;
                    live.pending = Some(AdapterEvent::Failed {
                        run_id: id.clone(),
                        fencing_token: live.token,
                        code: MissionErrorCode::BudgetExceeded,
                        message: "run exceeded its time limit or was cancelled".into(),
                    });
                }
                // A run that went quiet flushes its last partial window on a
                // later tick; an active stream flushes at most once per
                // window (audit F1).
                if live
                    .activity
                    .since
                    .is_some_and(|at| at.elapsed() >= ACTIVITY_FLUSH_WINDOW)
                {
                    MissionActor::flush_activity(&self.service, live, &id);
                }
            }
            if remove {
                self.live.remove(&id);
            }
        }

        // Deterministic commands run outside the pump. Polling a quiet
        // verifier must not block provider events or mission control.
        let finished: Vec<Id> = self
            .verifications
            .iter()
            .filter(|(_, w)| w.handle.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in finished {
            let worker = self.verifications.remove(&id).expect("finished worker");
            let result = worker.handle.join().unwrap_or_else(|_| {
                Err(MissionRpcError::new(
                    MissionErrorCode::Internal,
                    "verification worker panicked",
                ))
            });
            if let Err(error) = result {
                self.completions.insert(
                    id.clone(),
                    PendingCompletion {
                        mission_id: worker.mission_id,
                        workspace: worker.workspace,
                        event: deterministic_failure(id, worker.token, error),
                    },
                );
            }
        }
        let finished: Vec<_> = self
            .integrations
            .iter()
            .filter(|(_, worker)| worker.handle.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in finished {
            let worker = self
                .integrations
                .remove(&id)
                .expect("finished integration worker");
            if let Some(completion) = worker.finish(id.clone()) {
                self.completions.insert(id, completion);
            }
        }
        for intent in self
            .service
            .storage
            .mission_outbox()
            .map_err(MissionService::store_error)?
        {
            if intent.operation != OutboxOperation::Cancel || intent.state != OutboxState::Prepared
            {
                continue;
            }
            let Some(run_id) = &intent.run_id else {
                continue;
            };
            if !self.live.contains_key(run_id)
                && !self.verifications.contains_key(run_id)
                && !self.integrations.contains_key(run_id)
            {
                continue;
            }
            let mission = self.service.read_mission(&intent.mission_id)?;
            self.service.commit_actor(
                mission,
                "engine.claim_cancel",
                vec![],
                vec![OutboxUpdate {
                    id: intent.id,
                    expected_state: OutboxState::Prepared,
                    state: OutboxState::Sending,
                    fencing_token: intent.fencing_token,
                }],
            )?;
            if let Some(live) = self.live.get_mut(run_id) {
                if live.interrupted.is_none() {
                    let _ = live.adapter.interrupt(run_id);
                    live.interrupted = Some(Instant::now());
                }
            }
            if let Some(worker) = self.verifications.get(run_id) {
                worker.cancel.store(true, Ordering::Release);
            }
            if let Some(worker) = self.integrations.get(run_id) {
                worker.cancel.store(true, Ordering::Release);
            }
        }
        self.settle_controls()?;
        self.service.reconcile_task_failures()?;
        self.service.route_messages()?;
        if !self.dispatch_permitted {
            return Ok(());
        }
        self.service
            .advance_workflows(&self.root, self.integration_executor.is_some())?;
        self.service.dispatch_tick()?;
        let intents = self
            .service
            .storage
            .mission_outbox()
            .map_err(MissionService::store_error)?;
        for intent in intents {
            if intent.state != OutboxState::Prepared {
                continue;
            }
            if intent.operation == OutboxOperation::Verify {
                let job = match self.service.prepare_verification(
                    &intent,
                    &self.root,
                    self.verification_executor.is_some(),
                ) {
                    Ok(Some(job)) => job,
                    Ok(None) => continue,
                    Err(e) if e.code == MissionErrorCode::RevisionConflict => continue,
                    Err(e) if e.code == MissionErrorCode::StorageUnavailable => return Err(e),
                    Err(e) => {
                        self.fail_unsent(&intent, &e)?;
                        continue;
                    }
                };
                let cancel = Arc::new(AtomicBool::new(false));
                let worker_cancel = cancel.clone();
                let service = self.service.clone();
                let mission_id = job.mission_id.clone();
                let workspace = job.workspace.clone();
                let id = job.run_id.clone();
                let token = job.token;
                let executor = self.verification_executor.clone();
                let handle = std::thread::spawn(move || {
                    let path = std::path::Path::new(&job.workspace.path);
                    std::fs::create_dir_all(path.parent().expect("workspaces parent")).map_err(
                        |e| MissionRpcError::new(MissionErrorCode::Internal, e.to_string()),
                    )?;
                    workflow::run_verification_cancellable(
                        &service,
                        &service.artifacts,
                        &workflow::VerificationRequest {
                            mission_id: &job.mission_id,
                            command: &job.command,
                            candidate_id: &job.candidate_id,
                            repository: &job.repository,
                            worktree: path,
                            verify_task_id: &job.task_id,
                            verify_run_id: &job.run_id,
                            requirement_ids: job.requirement_ids,
                        },
                        &worker_cancel,
                        executor.as_ref(),
                    )
                });
                self.verifications.insert(
                    id,
                    VerificationWorker {
                        mission_id,
                        workspace,
                        token,
                        cancel,
                        handle,
                    },
                );
                continue;
            }
            if intent.operation != OutboxOperation::Start {
                continue;
            }
            if let Some(executor) = self.integration_executor.clone() {
                match self
                    .service
                    .prepare_integration(&intent, &self.root, &executor)
                {
                    Ok(Some(job)) => {
                        let id = job.run_id().clone();
                        let mission_id = job.workspace.mission_id.clone();
                        let workspace = job.workspace.clone();
                        let token = job.token;
                        let cancel = Arc::new(AtomicBool::new(false));
                        let worker_cancel = cancel.clone();
                        let service = self.service.clone();
                        let handle = std::thread::spawn(move || {
                            job.execute(&service, &executor, &worker_cancel)
                        });
                        self.integrations.insert(
                            id,
                            IntegrationWorker {
                                mission_id,
                                workspace,
                                token,
                                cancel,
                                handle,
                            },
                        );
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) if e.code == MissionErrorCode::RevisionConflict => continue,
                    Err(e) if e.code == MissionErrorCode::StorageUnavailable => return Err(e),
                    Err(e) => {
                        self.fail_unsent(&intent, &e)?;
                        continue;
                    }
                }
            }
            if intent
                .run_id
                .as_ref()
                .is_some_and(|id| self.live.contains_key(id))
            {
                continue;
            }
            let prepared = match self.service.prepare_run(&intent, &self.root) {
                Ok(Some(prepared)) => prepared,
                Ok(None) => continue,
                Err(error) if error.code == MissionErrorCode::RevisionConflict => continue,
                Err(error) if error.code == MissionErrorCode::StorageUnavailable => {
                    return Err(error)
                }
                Err(error) => {
                    self.fail_unsent(&intent, &error)?;
                    continue;
                }
            };
            let id = prepared.start.run_id.clone();
            let snapshot = workflow::load_entities(&self.service.storage, &prepared.mission_id)?;
            let timeout = Duration::from_millis(snapshot.mission.policy.run_time_limit_ms.get());
            let adapter = match (self.factory)(&prepared.start) {
                Ok(adapter) => adapter,
                Err(error) => {
                    self.completions.insert(
                        id.clone(),
                        PendingCompletion {
                            mission_id: prepared.mission_id,
                            workspace: prepared.workspace,
                            event: AdapterEvent::Failed {
                                run_id: id,
                                fencing_token: prepared.start.fencing_token,
                                code: MissionErrorCode::CapabilityUnsupported,
                                message: error.to_string(),
                            },
                        },
                    );
                    continue;
                }
            };
            let stream = adapter.subscribe();
            let token = prepared.start.fencing_token;
            let starter = adapter.clone();
            let start_service = self.service.clone();
            let worker = std::thread::Builder::new()
                .name(format!("mission-start-{id}"))
                .spawn(move || {
                    start_service
                        .validate_runtime_before_start(&prepared.start)
                        .map_err(StartFailure::Preflight)?;
                    starter
                        .start(prepared.start)
                        .map_err(StartFailure::Provider)
                });
            let (starting, pending, closed) = match worker {
                Ok(worker) => (Some(worker), None, false),
                Err(_) => (
                    None,
                    Some(AdapterEvent::Failed {
                        run_id: id.clone(),
                        fencing_token: token,
                        code: MissionErrorCode::ProviderUnavailable,
                        message: "provider start worker could not be created".into(),
                    }),
                    true,
                ),
            };
            self.live.insert(
                id.clone(),
                LiveRun {
                    mission_id: prepared.mission_id,
                    workspace: prepared.workspace,
                    adapter,
                    stream,
                    pending,
                    closed,
                    started: Instant::now(),
                    interrupted: None,
                    token,
                    timeout,
                    starting,
                    activity: RunActivity::default(),
                },
            );
        }
        Ok(())
    }

    /// Apply one run's coalesced display buffer through the fenced activity
    /// path. A CAS conflict with a concurrent user mutation is transient, so
    /// one immediate retry protects a terminal boundary's final window; any
    /// remaining failure retains the buffered text for the next tick —
    /// display output must never abort the actor's pass — and a terminal
    /// event behind it waits a bounded number of passes
    /// ([`RunActivity::defer_terminal`]). `apply_activity`
    /// re-checks the run fence and terminal state, so late text of a
    /// finished run is dropped exactly as per-delta application did before
    /// coalescing.
    fn flush_activity(service: &Arc<MissionService>, live: &mut LiveRun, run_id: &Id) {
        if live.activity.pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut live.activity.pending);
        live.activity.since = None;
        let mut conflict_retries = 1;
        loop {
            match service.apply_activity(&live.mission_id, run_id, live.token, &pending) {
                Ok(_) => return,
                Err(error)
                    if error.code == MissionErrorCode::RevisionConflict && conflict_retries > 0 =>
                {
                    conflict_retries -= 1;
                }
                Err(error) => {
                    tracing::warn!(code = ?error.code, run_id = %run_id,
                        "mission activity flush deferred; buffered text retained");
                    live.activity.buffer(&pending);
                    return;
                }
            }
        }
    }

    fn fail_unsent(
        &self,
        intent: &term_storage::mission::types::StoredOutbox,
        error: &MissionRpcError,
    ) -> Result<(), MissionRpcError> {
        let snapshot = workflow::load_entities(&self.service.storage, &intent.mission_id)?;
        let Some(mut run) = snapshot
            .runs
            .iter()
            .find(|r| Some(&r.id) == intent.run_id.as_ref())
            .cloned()
        else {
            return Ok(());
        };
        if run.state != RunState::Prepared || run.dispatch_state != RunDispatchState::Unsent {
            return Ok(());
        }
        let Some(mut task) = snapshot.tasks.iter().find(|t| t.id == run.task_id).cloned() else {
            return Ok(());
        };
        run.state = RunState::Failed;
        run.failure_code = Some(error.code);
        run.ended_at = Some(term_storage::time::now_iso8601());
        run.result_ref = Some(workflow::store_artifact(
            &self.service.artifacts,
            &intent.mission_id,
            "text/plain",
            error.message.as_bytes(),
        )?);
        task.state = TaskState::Failed;
        task.active_run_id = None;
        task.blocked_code = Some(format!("{:?}", error.code));
        self.service.commit_actor(
            snapshot.mission,
            "engine.prepare_failed",
            vec![Entity::Run(Box::new(run)), Entity::Task(Box::new(task))],
            vec![OutboxUpdate {
                id: intent.id.clone(),
                expected_state: OutboxState::Prepared,
                state: OutboxState::Failed,
                fencing_token: intent.fencing_token,
            }],
        )
    }

    fn settle_controls(&self) -> Result<(), MissionRpcError> {
        let intents = self
            .service
            .storage
            .mission_outbox()
            .map_err(MissionService::store_error)?;
        let mut cursor = None;
        loop {
            let (missions, next) = self
                .service
                .storage
                .mission_list(cursor, 50, false)
                .map_err(MissionService::store_error)?;
            for mission in missions {
                let snapshot = workflow::load_entities(&self.service.storage, &mission.id)?;
                let mut updates = Vec::new();
                for intent in intents
                    .iter()
                    .filter(|i| i.mission_id == mission.id && i.state == OutboxState::Prepared)
                {
                    let Some(run) = snapshot
                        .runs
                        .iter()
                        .find(|r| Some(&r.id) == intent.run_id.as_ref())
                    else {
                        continue;
                    };
                    if run.holds_execution_slot() {
                        continue;
                    }
                    if matches!(
                        intent.operation,
                        OutboxOperation::Start | OutboxOperation::Verify
                    ) {
                        updates.push(OutboxUpdate {
                            id: intent.id.clone(),
                            expected_state: OutboxState::Prepared,
                            state: OutboxState::Failed,
                            fencing_token: intent.fencing_token,
                        });
                    } else if intent.operation == OutboxOperation::Cancel {
                        for (expected_state, state) in [
                            (OutboxState::Prepared, OutboxState::Sending),
                            (OutboxState::Sending, OutboxState::Acknowledged),
                        ] {
                            updates.push(OutboxUpdate {
                                id: intent.id.clone(),
                                expected_state,
                                state,
                                fencing_token: intent.fencing_token,
                            });
                        }
                    }
                }
                let exec_cleanup_pending = super::failure_repair::exec_cleanup_pending(&snapshot);
                let mut mission = snapshot.mission;
                // A deterministic step commits its result just before its worker
                // returns. Wait until the actor has joined that worker too;
                // otherwise Paused can race the last owned execution frame.
                let workers_owned = self.live.values().any(|r| r.mission_id == mission.id)
                    || self
                        .integrations
                        .values()
                        .any(|r| r.mission_id == mission.id)
                    || self
                        .verifications
                        .values()
                        .any(|r| r.mission_id == mission.id)
                    || self
                        .completions
                        .values()
                        .any(|r| r.mission_id == mission.id);
                let settle = matches!(
                    mission.state,
                    MissionState::Pausing | MissionState::Stopping
                ) && !workers_owned
                    && (mission.state != MissionState::Stopping || !exec_cleanup_pending)
                    && !snapshot.runs.iter().any(|r| {
                        r.holds_execution_slot()
                            && !(mission.state == MissionState::Pausing
                                && r.state == RunState::Prepared
                                && r.dispatch_state == RunDispatchState::Unsent)
                    });
                if settle {
                    mission.state = if mission.state == MissionState::Pausing {
                        MissionState::Paused
                    } else if mission.failure_code.is_some() {
                        MissionState::Failed
                    } else {
                        MissionState::Cancelled
                    };
                }
                if !settle && updates.is_empty() {
                    continue;
                }
                self.service
                    .commit_actor(mission, "engine.control_settled", vec![], updates)?;
            }
            if next.is_none() {
                break;
            }
            cursor = next;
        }
        Ok(())
    }

    pub fn shutdown(&mut self) {
        for (id, live) in &mut self.live {
            if let Some(starter) = live.starting.take() {
                let _ = live.adapter.interrupt(id);
                let _ = starter.join();
            }
            let confirmed = matches!(live.adapter.close(id), CancelReceipt::Confirmed { .. });
            let event = if confirmed {
                AdapterEvent::Failed {
                    run_id: id.clone(),
                    fencing_token: live.token,
                    code: MissionErrorCode::ProviderUnavailable,
                    message:
                        "daemon shut down during execution; retry requires an explicit decision"
                            .into(),
                }
            } else {
                AdapterEvent::Disconnected {
                    run_id: id.clone(),
                    fencing_token: live.token,
                }
            };
            // Flush buffered display text before the terminal projection so
            // a graceful shutdown persists the final window (audit F1).
            MissionActor::flush_activity(&self.service, live, id);
            let _ =
                self.service
                    .apply_adapter_event(&live.mission_id, &event, Some(&live.workspace));
        }
        self.live.clear();
        for (_, worker) in self.delivering.drain() {
            let receipt = worker.handle.join().unwrap_or(DeliveryReceipt::Unknown {
                reason: "delivery worker ended without a receipt",
            });
            self.deliveries
                .insert(worker.intent.id.clone(), (worker.intent, receipt));
        }
        for (_, (intent, receipt)) in self.deliveries.drain() {
            let _ = self.service.record_delivery(&intent, &receipt);
        }
        for worker in self.verifications.values() {
            worker.cancel.store(true, Ordering::Release);
        }
        for worker in self.integrations.values() {
            worker.cancel.store(true, Ordering::Release);
        }
        for (_, worker) in self.verifications.drain() {
            let _ = worker.handle.join();
        }
        for (id, worker) in self.integrations.drain() {
            if let Some(completion) = worker.finish(id) {
                if let Err(error) = self.service.apply_adapter_event(
                    &completion.mission_id,
                    &completion.event,
                    Some(&completion.workspace),
                ) {
                    tracing::warn!(code=?error.code, "integration shutdown result awaits recovery");
                }
            }
        }
        if let Err(error) = self.service.flush_time() {
            tracing::warn!(code=?error.code,"mission time flush failed during shutdown");
        }
    }
}

/// Start the mission actor with the daemon. Provider execution lives on
/// its own thread, separate from terminal admission and telemetry.
pub fn spawn(state: Arc<crate::state::DaemonState>) -> Option<std::thread::JoinHandle<()>> {
    let service = state.missions.clone()?;
    let supervisor = Arc::new(crate::exec::ExecSupervisor::persistent(
        state.config.admission_config(state.logical_cpus),
        service.exec_persistence(),
        state.admission_host(),
        crate::exec::gated::GateConfig {
            helper_program: std::env::current_exe().expect("running daemon executable"),
            directory: state.paths.socket_dir().to_path_buf(),
            platform: state.platform.clone(),
            timeout: state.config.gate_timeout(),
        },
    ));
    let shared_exec = supervisor.clone();
    let runtime = state.runtime.clone();
    let claude_configs = state.paths.missions_dir().join("runtime/claude");
    let codex_configs = state.paths.missions_dir().join("runtime/codex");
    let opencode_configs = state.paths.missions_dir().join("runtime/opencode");
    // Constructing this store does not read keys. Resolution happens on the
    // adapter worker, outside the mission actor and all DB transactions.
    let connections =
        crate::connections::ConnectionStore::production(state.paths.root()).map(Arc::new);
    // 팩토리 클로저는 `move`라 `state`를 통째로 잡으면 아래 actor 스레드가 쓸 수
    // 없다 — Claude 어댑터가 필요로 하는 데이터 루트만 미리 떼어 둔다.
    let data_root = state.paths.root().to_path_buf();
    let factory: AdapterFactory = Arc::new(move |run| {
        if !matches!(
            run.binding.runtime,
            RuntimeKind::Opencode | RuntimeKind::Codex | RuntimeKind::Claude
        ) && (run.binding.credential_ref.is_some() || run.binding.endpoint_ref.is_some())
        {
            return Err(std::io::Error::other(
                "binding credential/endpoint reference has no daemon resolver",
            ));
        }
        match run.binding.runtime {
            RuntimeKind::Codex => Ok(crate::agent_runtime::codex::CodexAdapter::authenticated(
                shared_exec.clone(),
                runtime.clone(),
                connections.as_ref().ok().cloned(),
                codex_configs.clone(),
            )),
            RuntimeKind::Claude => Ok(
                crate::agent_runtime::claude::ClaudePrintAdapter::authenticated(
                    shared_exec.clone(),
                    runtime.clone(),
                    claude_configs.clone(),
                    Some(data_root.clone()),
                    connections.as_ref().ok().cloned(),
                ),
            ),
            RuntimeKind::Fake => Err(std::io::Error::other(
                "fake runtime requires an explicitly injected test adapter",
            )),
            RuntimeKind::Opencode => Ok(
                crate::agent_runtime::opencode::runtime::OpenCodeRuntimeAdapter::supervised(
                    shared_exec.clone(),
                    runtime.clone(),
                    connections
                        .as_ref()
                        .map_err(|_| std::io::Error::other("connection store is unavailable"))?
                        .clone(),
                    opencode_configs.clone(),
                ),
            ),
        }
    });
    Some(std::thread::Builder::new().name("mission-actor".into()).spawn(move || {
        let mut actor=MissionActor::new(service.clone(),state.paths.missions_dir(),factory).with_deterministic_exec(supervisor.clone(), state.runtime.clone());
        let shutdown=state.shutdown.subscribe();
        // Git preparation/integration can occupy this actor for longer than a
        // checkpoint interval. Keep time persistence independent of that work.
        let clock_shutdown = state.shutdown.subscribe();
        let clock_service = service.clone();
        let clock_worker = std::thread::Builder::new().name("mission-clock".into()).spawn(move || {
            while !*clock_shutdown.borrow() {
                if let Err(error) = clock_service.checkpoint_time() {
                    tracing::warn!(code=?error.code,"mission time checkpoint failed");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }).expect("mission clock thread");
        let recovery_shutdown = state.shutdown.subscribe();
        let recovery_supervisor = supervisor.clone();
        let recovery_worker = std::thread::Builder::new().name("mission-native-recovery".into()).spawn(move || {
            let mut failures: HashMap<Id, (String, Instant)> = HashMap::new();
            while !*recovery_shutdown.borrow() {
                // Keep errors across rotating batches, including directory
                // cleanup after the reservation is already released.
                failures.retain(|_, (_, seen)| seen.elapsed() < Duration::from_secs(300));
                for (id, error) in recovery_supervisor.reconcile_native_recovery() {
                    let message = error.to_string();
                    if failures.get(&id).map(|(message, _)| message) != Some(&message) {
                        tracing::warn!(exec_id=%id, %error, "native execution recovery remains unresolved");
                    }
                    failures.insert(id, (message, Instant::now()));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }).expect("mission native recovery thread");
        let mut seen=HashMap::new();
        let mut recovery_failed = false;
        let mut recovered_count = None;
        while !*shutdown.borrow() {
            supervisor.update_host(state.admission_host());
            match supervisor.refresh_recovery() {
                Ok(count) => {
                    if recovery_failed || recovered_count != Some(count) {
                        tracing::info!(restored_execs=count,"mission execution reservations reconciled");
                    }
                    recovery_failed = false;
                    recovered_count = Some(count);
                }
                Err(error) => {
                    if !recovery_failed {
                        tracing::warn!(%error,"mission execution reservation recovery failed; new dispatch held");
                    }
                    recovery_failed = true;
                }
            }
            actor.set_dispatch_permitted(supervisor.ledger().recovery_ready());
            if let Err(error)=actor.tick(){tracing::warn!(code=?error.code,message=%error.message,"mission tick failed");}
            // Notifications carry only a revision hint, never result bodies.
            let mut cursor=None;
            loop {
                let Ok((missions,next))=service.storage.mission_list(cursor,50,false) else {break;};
                for mission in missions {
                    if seen.insert(mission.id.clone(),mission.revision.get()) != Some(mission.revision.get()) {
                        state.broadcast_control(term_contracts::rpc::RpcEventKind::MissionChanged,serde_json::json!({"mission_id":mission.id,"latest_seq":mission.revision}));
                    }
                }
                if next.is_none(){break;}cursor=next;
            }
            std::thread::sleep(state.config.scheduler_interval);
        }
        actor.shutdown();
        let _ = recovery_worker.join();
        let _ = clock_worker.join();
    }).expect("mission actor thread"))
}

#[cfg(test)]
mod activity_buffer_tests {
    use super::*;

    #[test]
    fn only_this_runs_activity_is_buffered_even_when_tokens_collide() {
        let run = Id::generate();
        let other = Id::generate();
        let activity = |run_id: &Id, fencing_token| AdapterEvent::Activity {
            run_id: run_id.clone(),
            fencing_token,
            chunk: "text".into(),
        };
        assert_eq!(own_activity(&activity(&run, 3), &run, 3), Some("text"));
        // Tokens are `revision + 1` per mission, so another mission's run
        // can share this token; its text must not land in this run's tail.
        assert_eq!(own_activity(&activity(&other, 3), &run, 3), None);
        assert_eq!(own_activity(&activity(&run, 4), &run, 3), None);
        let disconnected = AdapterEvent::Disconnected {
            run_id: run.clone(),
            fencing_token: 3,
        };
        assert_eq!(own_activity(&disconnected, &run, 3), None);
    }

    #[test]
    fn terminal_waits_a_bounded_number_of_passes_for_a_failed_final_flush() {
        let mut activity = RunActivity::default();
        assert!(!activity.defer_terminal(), "nothing buffered");
        // A failed flush re-buffers its text; the terminal event waits.
        activity.buffer("final window");
        for _ in 0..ACTIVITY_TERMINAL_DEFERRALS {
            assert!(activity.defer_terminal());
        }
        assert!(
            !activity.defer_terminal(),
            "a persistent storage fault cannot hold the run's outcome"
        );
    }

    #[test]
    fn buffered_display_text_keeps_only_the_newest_window_bytes() {
        let mut activity = RunActivity::default();
        assert!(activity.pending.is_empty());
        activity.buffer(&"x".repeat(512));
        assert_eq!(activity.pending.len(), 512);
        assert!(
            activity.since.is_some(),
            "first byte anchors the flush window"
        );
        activity.buffer(&"y".repeat(ACTIVITY_BUFFER_BYTES + 8));
        // Oldest bytes drop on a char boundary; the cap is never exceeded.
        assert_eq!(activity.pending.len(), ACTIVITY_BUFFER_BYTES);
        assert!(activity.pending.ends_with('y'));
        assert!(
            !activity.pending.contains('x'),
            "dropped bytes are the oldest"
        );
    }
}
