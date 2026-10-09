//! # iyagi-termd (library)
//!
//! User-privileged execution daemon: local IPC server, gated launch
//! orchestration, lifecycle (spec `01-contracts.md`, `02-runner.md`).
//! The binary in `main.rs` is a thin clap wrapper over [`Daemon`].

pub mod agent_model;
pub mod agent_runtime;
pub mod agent_session;
pub mod agent_watch;
pub mod auth;
pub mod claude_provider;
pub mod claude_usage;
pub mod config;
pub mod connections;
pub mod dispatch;
pub mod exec;
pub mod gate_listener;
pub mod guard;
pub mod helper;
pub mod hook;
pub mod ipc;
pub mod lifecycle;
pub mod mission;
pub mod model_watch;
mod opencode_integration;
pub mod orchestrator;
pub mod paths;
pub mod relief;
pub mod retention;
pub mod scheduler;
pub mod search_scan;
mod session_recovery;
pub mod sessions;
pub mod state;
pub mod supervisor;
pub mod telemetry_loop;
pub mod workspace;

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use term_core::{
    CpuPressureTracker, MonotonicClock, PressureTracker, ReservationLedger, WorkloadQueue,
};
use term_platform::telemetry::{SystemClock, TelemetrySampler};
use term_platform::ResourcePlatform;

use crate::config::DaemonConfig;
use crate::paths::{Paths, SingletonOutcome};
use crate::state::DaemonState;

/// Exit code used for "another daemon owns this data dir".
pub const EXIT_HELD: i32 = 3;

pub struct Daemon {
    pub state: Arc<DaemonState>,
}

impl Daemon {
    /// Bring the daemon up: paths → singleton lock → token → storage
    /// (migrate + reconcile) → platform → state → loops. Returns `Err` with
    /// an exit code when startup must not proceed.
    pub fn start(
        data_dir: std::path::PathBuf,
        runtime: tokio::runtime::Handle,
    ) -> Result<Daemon, i32> {
        let config = DaemonConfig::load();
        let paths = Paths::init(&data_dir).map_err(|e| {
            eprintln!("iyagi-termd: data dir init failed: {e}");
            1
        })?;

        match paths::acquire_singleton_lock(&paths) {
            Ok(SingletonOutcome::Acquired) => {}
            Ok(SingletonOutcome::HeldByLiveDaemon) => {
                eprintln!("iyagi-termd: another daemon already owns this data dir");
                return Err(EXIT_HELD);
            }
            Err(e) => {
                eprintln!("iyagi-termd: singleton lock failed: {e}");
                return Err(1);
            }
        }

        paths.write_token().map_err(|e| {
            eprintln!("iyagi-termd: token write failed: {e}");
            1
        })?;

        // DB migration → crash reconciliation (02-runner §1.4 order).
        let storage = Arc::new(term_storage::Storage::open(paths.db()).map_err(|e| {
            eprintln!("iyagi-termd: storage open failed: {e}");
            1
        })?);
        let report = storage.take_reconciliation_report().unwrap_or_default();
        let daemon_id = uuid::Uuid::new_v4().to_string();

        let platform: Arc<dyn ResourcePlatform> = Arc::from(term_platform::group::select_backend());
        // O1 gate: the platform backend never decides mission support; the
        // daemon stamps its own declaration so absent always means "locked".
        let caps = platform
            .capabilities()
            .with_mission_protocol(config.missions_enabled.then_some(1))
            // `LaunchRequest.claude_provider` is resolved by this daemon's
            // launch path (orchestrator); older daemons omit the flag and
            // the UI refuses routed launches against them.
            .with_claude_provider_routing(true);
        // 08 §2: 양보 capability는 백엔드 탐지(cgroup 위임 조사 등)를 포함하므로
        // 기동에 한 번만 읽고 데몬이 사는 내내 재사용한다.
        let scheduling_yield_supported =
            caps.scheduling_yield.support == term_contracts::snapshot::LimitSupport::Supported;
        // 08 §5: 일시정지 capability도 같은 규율로 기동에 한 번만 읽는다.
        let suspend_resume_supported =
            caps.suspend_resume.support == term_contracts::snapshot::LimitSupport::Supported;
        let boot_id_reliable = term_platform::identity::boot_id_reliable();
        let logical_cpus = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1);

        let clock = MonotonicClock::new();
        let queue = WorkloadQueue::new(config.queue_config(), MonotonicClock::new());
        let ledger = ReservationLedger::new(config.admission_config(logical_cpus));
        let pressure = PressureTracker::new(config.pressure_config(), MonotonicClock::new());
        let cpu_pressure =
            CpuPressureTracker::new(config.cpu_pressure_config(), MonotonicClock::new());
        let relief_config = config.relief_config();
        let guard_policy = config.guard_policy();
        // O1 mission service: only while the feature gate advertises it
        // (O01). The gate off means no mission RPC state exists at all.
        let missions = config.missions_enabled.then(|| {
            Arc::new(
                crate::mission::MissionService::new(
                    Arc::clone(&storage),
                    crate::mission::artifacts::ArtifactStore::new(
                        Arc::clone(&storage),
                        paths.missions_dir(),
                    ),
                )
                .with_daemon_id(
                    term_contracts::mission::types::Id::parse(&daemon_id).expect("daemon UUID"),
                ),
            )
        });
        // O14: recover pending mission intents before anything dispatches —
        // unknown outcomes are never auto-resent (02 §7).
        if let Some(missions) = &missions {
            match missions.recover_on_startup() {
                Ok(report) => tracing::info!(
                    dispatch = report.dispatch,
                    held = report.held,
                    inspect = report.inspect,
                    "mission outbox recovery"
                ),
                Err(error) => tracing::warn!(%error, "mission outbox recovery scan failed"),
            }
        }
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);

        // The global journal budget is on-disk bytes, but a fresh budget
        // starts at zero: journals left by previous daemon generations stay
        // on disk for `journal_retention_days` (default 7) before retention
        // deletes them, so without seeding the cap would not see them and
        // real usage could exceed the limit. Seed with what is on disk now
        // (metadata sizes of the journal files retention manages); retention
        // releases the on-disk bytes of exactly those files as it deletes
        // them, and `release` saturates at zero, so the two accounting paths
        // cannot underflow. A seed at or near the cap is relieved below,
        // before any session can open.
        let journal_budget = {
            let mut budget =
                term_pty::journal::GlobalJournalBudget::new(config.journal_global_bytes());
            let on_disk = retention::journal_bytes_on_disk(&paths.journals_dir());
            budget.seed_used(on_disk);
            if on_disk > 0 {
                tracing::info!(
                    bytes = on_disk,
                    "journal budget seeded from journals on disk"
                );
            }
            Arc::new(Mutex::new(budget))
        };

        let state = Arc::new(DaemonState {
            daemon_id: daemon_id.clone(),
            config,
            paths,
            storage,
            missions,
            platform,
            caps: Mutex::new(caps),
            boot_id_reliable,
            logical_cpus,
            revision: AtomicU64::new(1),
            clock,
            queue,
            ledger,
            telemetry: Mutex::new(TelemetrySampler::new(Arc::new(SystemClock::new()))),
            pressure: Mutex::new(pressure),
            cpu_pressure: Mutex::new(cpu_pressure),
            host: Mutex::new((None, 0)),
            pressure_level: Mutex::new(term_contracts::metrics::PressureLevel::Normal),
            cpu_pressure_level: Mutex::new(term_contracts::metrics::PressureLevel::Normal),
            reconciliation_required: AtomicBool::new(false),
            db_vacuumed: AtomicBool::new(false),
            connections: Mutex::new(std::collections::HashMap::new()),
            workloads: Mutex::new(std::collections::HashMap::new()),
            sessions: Mutex::new(std::collections::HashMap::new()),
            finalized_sessions: Mutex::new(std::collections::VecDeque::new()),
            finished_workloads: Mutex::new(std::collections::VecDeque::new()),
            interventions: Mutex::new(crate::state::InterventionRing::new()),
            journal_budget,
            flow_budget: term_pty::flow::GlobalOutputBudget::shared(),
            data_tokens: auth::DataTokens::new(DaemonConfig::load().data_token_ttl()),
            usage_cache: Mutex::new(std::collections::HashMap::new()),
            focused_sessions: Mutex::new(std::collections::HashMap::new()),
            relief: Mutex::new(crate::relief::ReliefController::new(
                relief_config.policy,
                relief_config.release_interval_ms,
            )),
            guard: Mutex::new(crate::guard::GuardController::new(guard_policy)),
            scheduling_yield_supported,
            suspend_resume_supported,
            runtime,
            shutdown: shutdown_tx,
            stop_workloads_on_shutdown: AtomicBool::new(false),
            last_activity: Mutex::new(Instant::now()),
        });

        lifecycle::assess_reconciliation(&state, report);

        // Journal space pressure, before IPC can open a session. Earlier
        // daemon generations each started their budget at zero, so the
        // journals they left can put the seed at or above the cap; then
        // every new journal fails its 20-byte header reservation and every
        // launch fails with JOURNAL_LIMIT. Evict the oldest finished,
        // unpinned journals (and unreachable leftovers) down to the
        // low-water mark regardless of their retention window.
        retention::relieve_space_pressure(&state);

        // Seed telemetry immediately so the first launch does not sit in
        // WAIT_TELEMETRY waiting for the 1 s loop (02-runner §1: ready
        // implies a working snapshot).
        {
            let now = state.now_ms();
            let sample = state
                .telemetry
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .poll_host(now);
            let level = state
                .pressure
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .update(sample.total_bytes().unwrap_or(0), sample.available_bytes());
            let cpu_level = state
                .cpu_pressure
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .update(
                    telemetry_loop::measured_cpu_cores(&sample),
                    sample.logical_cpu_count,
                );
            // 스냅샷이 광고하는 cpu_pressure는 히스테리시스를 거친 값이다
            // (샘플러는 이력이 없어 NORMAL만 채운다).
            let mut sample = sample;
            sample.cpu_pressure = cpu_level;
            *state.host.lock().unwrap_or_else(|p| p.into_inner()) = (Some(sample), now);
            *state
                .pressure_level
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = level;
            *state
                .cpu_pressure_level
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = cpu_level;
        }

        Ok(Daemon { state })
    }

    /// Serve until shutdown is signaled. Returns the process exit code.
    pub async fn run(self) -> i32 {
        let state = self.state;
        // Subscribe before anything that can signal shutdown is spawned.
        // `watch::Sender::send` stores nothing while no receiver exists, and
        // a receiver created later treats the current value as already
        // seen, so an IPC bind failure that fired first would leave the
        // wait below hanging: a zombie holding the singleton lock with no
        // endpoint. `wait_for` also checks the current value first.
        let mut shutdown_rx = state.shutdown.subscribe();
        supervisor::spawn_supervised(&state, "scheduler", scheduler::run);
        supervisor::spawn_supervised(&state, "telemetry", telemetry_loop::run);
        supervisor::spawn_supervised(&state, "agent-watch", agent_watch::run);
        supervisor::spawn_supervised(&state, "journal-retention", retention::run);
        supervisor::spawn_supervised(&state, "idle-watcher", lifecycle::run_idle_watcher);
        let mission_actor = mission::actor::spawn(Arc::clone(&state));

        let ipc_state = Arc::clone(&state);
        let ipc_failed = Arc::new(AtomicBool::new(false));
        let ipc_failed_task = Arc::clone(&ipc_failed);
        let ipc_task = tokio::spawn(async move {
            if let Err(e) = ipc::serve(Arc::clone(&ipc_state)).await {
                eprintln!("iyagi-termd: ipc listener failed: {e}");
                ipc_failed_task.store(true, std::sync::atomic::Ordering::Release);
                // A daemon whose endpoint could not be bound (e.g. a
                // squatted Windows named pipe) would otherwise zombie here
                // holding the singleton lock: take the normal shutdown path.
                let _ = ipc_state.shutdown.send(true);
            }
        });
        #[cfg(unix)]
        spawn_signal_handlers(Arc::clone(&state));

        // Ready line for operators/tests tailing stderr.
        eprintln!(
            "iyagi-termd: ready daemon_id={} endpoint={}",
            state.daemon_id,
            state.paths.main_endpoint()
        );

        let _ = shutdown_rx.wait_for(|stop| *stop).await;
        tracing::info!("shutdown requested");
        ipc_task.abort();
        if let Some(actor) = mission_actor {
            // Keep storage and the daemon runtime alive until owned mission
            // processes are closed and their final projections are recorded.
            let _ = tokio::task::spawn_blocking(move || actor.join()).await;
        }

        // 08 §0-4: 모든 완화는 데몬 종료 경로에서 해제된다. 남겨 두면
        // 사용자가 손댈 수 없는 background 프로세스가 그대로 남는다.
        restore_yielded_on_shutdown(&state);

        // Give in-flight terminal writes + events a moment to land.
        if state
            .stop_workloads_on_shutdown
            .load(std::sync::atomic::Ordering::Acquire)
        {
            orchestrator::stop_all_workloads(&state);
            lifecycle::wait_for_terminal(&state, std::time::Duration::from_secs(3));
        } else {
            lifecycle::wait_for_winding_down(&state, std::time::Duration::from_secs(3));
        }
        stop_session_pumps(&state);
        eprintln!("iyagi-termd: exiting");
        if ipc_failed.load(std::sync::atomic::Ordering::Acquire) {
            // The IPC endpoint never came up; distinguish it from a clean
            // shutdown for supervisors reading the exit code.
            return 1;
        }
        0
    }
}

/// SIGTERM/SIGINT/SIGHUP take the same graceful path as
/// `daemon.shutdown { stop_workloads: false }`: logind session teardown,
/// `kill`/`pkill`, or a terminal hangup end the daemon with its journals
/// flushed instead of the default immediate termination. Workloads are
/// kept, as on any daemon restart (02-runner §1).
#[cfg(unix)]
fn spawn_signal_handlers(state: Arc<DaemonState>) {
    use tokio::signal::unix::{signal, SignalKind};
    let kinds = [
        (SignalKind::terminate(), "SIGTERM"),
        (SignalKind::interrupt(), "SIGINT"),
        (SignalKind::hangup(), "SIGHUP"),
    ];
    for (kind, name) in kinds {
        let mut sig = match signal(kind) {
            Ok(sig) => sig,
            Err(e) => {
                tracing::warn!(signal = name, error = %e, "signal handler not installed");
                continue;
            }
        };
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if sig.recv().await.is_some() {
                tracing::info!(
                    signal = name,
                    "os signal: graceful shutdown, workloads kept"
                );
                state
                    .stop_workloads_on_shutdown
                    .store(false, std::sync::atomic::Ordering::Release);
                let _ = state.shutdown.send(true);
            }
        });
    }
}

/// Undo every scheduling yield before the daemon exits (08 §0-4). Best
/// effort: a workload whose processes already left simply has nothing to
/// restore, and a failure here must never block shutdown.
fn restore_yielded_on_shutdown(state: &Arc<DaemonState>) {
    let ops = state
        .relief
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .shutdown_restore_ops();
    for op in ops {
        if let Err(error) = relief::apply(state, &op) {
            tracing::debug!(workload = %op.workload_id(), %error, "shutdown relief restore failed");
        }
    }
    // 08 §5: 가드가 일시정시킨 워크로드도 나가기 전에 재개한다(불변 4).
    let ops = state
        .guard
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .shutdown_resume_ops();
    for op in ops {
        if let Err(error) = guard::apply(state, &op) {
            tracing::debug!(workload = %op.workload_id(), %error, "shutdown guard resume failed");
        }
    }
}

/// Stop every session's delivery pump on the way out. The pump only exits by
/// itself once the actor finalized AND the last view detached, so a shutdown
/// with live (or detached-but-live) sessions used to leave one spinning
/// thread per session holding an `Arc<DaemonState>`.
fn stop_session_pumps(state: &Arc<DaemonState>) {
    let sessions: Vec<_> = state
        .sessions
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .cloned()
        .collect();
    for session in sessions {
        session
            .pump_stop
            .store(true, std::sync::atomic::Ordering::Release);
        session.wake();
    }
}
