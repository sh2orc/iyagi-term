//! # Fake adapter (ticket O07, docs/orchestration/03-adapters.md §7)
//!
//! Development/CI default: no network, no auth, scripted scenario playback
//! on an injected clock. Two operation modes:
//! * [`FakeAdapter::in_process`] — a script engine on a plain thread; for
//!   unit tests that do not need the exec supervisor.
//! * [`FakeAdapter::spawn`] — the `term-fixture agent-fake` binary as a
//!   real Exec child of the supervisor, so pipe/output/ladder behavior is
//!   exercised end to end.
//!
//! Scenario semantics (03 §7 list): valid result, invalid/partial result,
//! approval→answer, disconnect, cancel-accepted-but-alive (the
//! ignore_interrupt flag plus exit_late), stale fencing callback
//! (LateResultAfterCancel), output flood, newline-less 1 MiB+ streams, and
//! declared file writes policed through the exec-side hook.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use term_contracts::ids::U64String;
use term_contracts::launch::{Enforcement, LaunchPolicy};
use term_contracts::metrics::PressureLevel;
use term_contracts::mission::types::{
    AuthRoute, Binding, Id, ProviderResult, RuntimeCapabilities, RuntimeKind, Support,
};
use term_contracts::mission::MissionErrorCode;
use tokio::sync::mpsc;

use super::{
    AdapterEvent, AgentAdapter, CancelReceipt, CancelRejected, DeliveryReceipt, EventStream,
    FencingGate, QueuedReason, RunProbe, RunStart,
};
use crate::exec::{
    ExecHandle, ExecProbe, ExecSupervisor, OutputSink, OutputVerdict, PersistExec, SpawnRequest,
    StreamKind, DEFAULT_SPOOL_BYTES,
};

/// Injectable time source for scenario playback. `sleep_until` blocks the
/// engine thread until fake time reaches `ms`; tests drive a
/// [`ManualClock`], the real daemon uses [`WallClock`].
pub trait FakeClock: Send + Sync {
    fn now_ms(&self) -> u64;
    fn sleep_until(&self, ms: u64);
}

/// Monotonic wall clock (`Instant` based) with real sleeps.
pub struct WallClock {
    origin: std::time::Instant,
}

impl WallClock {
    pub fn new() -> Self {
        WallClock {
            origin: std::time::Instant::now(),
        }
    }
}

impl Default for WallClock {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeClock for WallClock {
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    fn sleep_until(&self, ms: u64) {
        let now = self.now_ms();
        if ms > now {
            std::thread::sleep(Duration::from_millis(ms - now));
        }
    }
}

/// Deterministic manual clock: `advance_to` wakes sleepers; time only moves
/// when the test says so.
pub struct ManualClock {
    state: Mutex<u64>,
    wake: std::sync::Condvar,
}

impl ManualClock {
    pub fn new(start_ms: u64) -> Arc<Self> {
        Arc::new(ManualClock {
            state: Mutex::new(start_ms),
            wake: std::sync::Condvar::new(),
        })
    }

    /// Move fake time forward to `ms` (never backward) and wake sleepers.
    pub fn advance_to(&self, ms: u64) {
        let mut now = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if ms > *now {
            *now = ms;
        }
        self.wake.notify_all();
    }
}

impl FakeClock for ManualClock {
    fn now_ms(&self) -> u64 {
        *self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn sleep_until(&self, ms: u64) {
        let mut now = self.state.lock().unwrap_or_else(|p| p.into_inner());
        while *now < ms {
            now = self
                .wake
                .wait(now)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

// ---- scenario ------------------------------------------------------------

/// One scripted scenario. The same JSON shape travels to the
/// `term-fixture agent-fake` child (base64 in argv).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct FakeScript {
    #[serde(default)]
    pub steps: Vec<FakeStep>,
    /// 03 §7 "cancel accepted but process alive": the interrupt is accepted
    /// and playback keeps going; only the stop ladder ends it.
    #[serde(default)]
    pub ignore_interrupt: bool,
    /// Hold this long after the last step before finishing (late exit).
    #[serde(default)]
    pub exit_late_ms: u64,
}

impl FakeScript {
    /// Happy-path default (03 §7 "start→activity→valid result→cleanup").
    pub fn happy() -> Self {
        FakeScript {
            steps: vec![
                FakeStep::Started {
                    session_id: Some("fake-session-1".into()),
                    turn_id: Some("fake-turn-1".into()),
                },
                FakeStep::Activity {
                    text: "fake activity".into(),
                },
                FakeStep::Result {
                    value: ProviderResult::Report {
                        report_text: "fake report".into(),
                        knowledge: Vec::new(),
                    },
                },
            ],
            ignore_interrupt: false,
            exit_late_ms: 0,
        }
    }

    /// Encode for the fixture's `agent-fake <scenario-json-b64>` argv.
    pub fn to_argv_b64(&self) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(self)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(json))
    }
}

/// One scripted step. Serialized with a `t` tag exactly like the fixture's
/// stdout protocol so both sides share one vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FakeStep {
    Started {
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        turn_id: Option<String>,
    },
    Activity {
        text: String,
    },
    Approval {
        request_id: String,
        question: String,
    },
    Usage {
        #[serde(default)]
        input_tokens: Option<u64>,
        #[serde(default)]
        output_tokens: Option<u64>,
        #[serde(default)]
        cost_usd_micros: Option<u64>,
    },
    Result {
        value: ProviderResult,
    },
    Fail {
        code: String,
        message: String,
    },
    Disconnect,
    Delay {
        ms: u64,
    },
    /// E11: a final that arrives after cancel was accepted — emitted with
    /// the *original* fencing token, so a gate advanced by the new actor
    /// must drop it.
    LateResultAfterCancel {
        value: ProviderResult,
    },
    /// Declared file write (scoped-write police hook, spawn mode).
    FileWrite {
        path: String,
        bytes: usize,
    },
    /// Output flood exercise (spawn mode only).
    FloodStdout {
        bytes: u64,
    },
    /// 1 MiB+ stream without a newline (spawn mode only).
    NoNewline {
        bytes: u64,
    },
    /// Truncated final JSON then exit 0 (E19, spawn mode only).
    PartialJson,
    /// Stay alive this long after cancel, then finish (E15).
    ExitLate {
        ms: u64,
    },
}

// ---- adapter --------------------------------------------------------------

/// Event fan-out shared by both modes.
#[derive(Default)]
struct EventBus {
    subscribers: Mutex<Vec<mpsc::UnboundedSender<AdapterEvent>>>,
}

impl EventBus {
    fn publish(&self, event: AdapterEvent) {
        self.subscribers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|tx| tx.send(event.clone()).is_ok());
    }
}

/// Mutable per-run state of the in-process engine.
struct InProcessRun {
    cancel: Arc<AtomicBool>,
    /// `Some(request_id)` while an approval waits for an answer.
    awaiting: Mutex<Option<String>>,
    finished: AtomicBool,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
    answers: std::sync::mpsc::Sender<(String, String)>,
}

/// Per-run state of the spawned (real-child) mode.
struct SpawnedRun {
    handle: ExecHandle,
    /// A terminal protocol event was observed for this run.
    terminal: Arc<AtomicBool>,
}

enum RunSlot {
    InProcess(Arc<InProcessRun>),
    Spawned(SpawnedRun),
}

enum Mode {
    InProcess,
    Spawned {
        fixture: PathBuf,
        supervisor: Arc<ExecSupervisor>,
        /// Keeps private runtime workers alive for pump tasks when the
        /// adapter was constructed outside any tokio context.
        _runtime: Option<tokio::runtime::Runtime>,
        spawn_handle: tokio::runtime::Handle,
    },
}

/// The fake runtime adapter (03 §7). Not a provider: fake success is never
/// mixed into usage/quality evaluation.
pub struct FakeAdapter {
    mode: Mode,
    runs: Mutex<HashMap<Id, Arc<RunSlot>>>,
    bus: Arc<EventBus>,
    gate: Arc<FencingGate>,
    clock: Arc<dyn FakeClock>,
    default_script: Mutex<FakeScript>,
}

impl FakeAdapter {
    /// In-process engine on the given clock — unit tests without the exec
    /// supervisor.
    pub fn in_process(clock: Arc<dyn FakeClock>) -> Arc<Self> {
        Arc::new(FakeAdapter {
            mode: Mode::InProcess,
            runs: Mutex::new(HashMap::new()),
            bus: Arc::new(EventBus::default()),
            gate: FencingGate::new(),
            clock,
            default_script: Mutex::new(FakeScript::happy()),
        })
    }

    /// Real-child mode: `term-fixture agent-fake` under an owned exec
    /// supervisor. Pumps land on the current runtime when there is one;
    /// otherwise a private runtime drives them.
    pub fn spawn(fixture: PathBuf) -> std::io::Result<Arc<Self>> {
        let supervisor = Arc::new(ExecSupervisor::new(
            default_admission_config(),
            noop_persist(),
            healthy_host(),
        ));
        let (runtime, spawn_handle) = match tokio::runtime::Handle::try_current() {
            Ok(handle) => (None, handle),
            Err(_) => {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()?;
                let handle = runtime.handle().clone();
                (Some(runtime), handle)
            }
        };
        Ok(Arc::new(FakeAdapter {
            mode: Mode::Spawned {
                fixture,
                supervisor,
                _runtime: runtime,
                spawn_handle,
            },
            runs: Mutex::new(HashMap::new()),
            bus: Arc::new(EventBus::default()),
            gate: FencingGate::new(),
            clock: Arc::new(WallClock::new()),
            default_script: Mutex::new(FakeScript::happy()),
        }))
    }

    /// Override the script used by trait `start` (setup-time operation;
    /// per-run scenarios go through [`Self::start_scripted`]).
    pub fn set_default_script(&self, script: FakeScript) {
        *self
            .default_script
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = script;
    }

    /// Launch one run with an explicit scenario.
    pub fn start_scripted(&self, run: RunStart, script: FakeScript) -> std::io::Result<()> {
        self.gate.register(&run.run_id, run.fencing_token);
        match &self.mode {
            Mode::InProcess => self.start_in_process(run, script),
            Mode::Spawned {
                fixture,
                supervisor,
                spawn_handle,
                ..
            } => self.start_spawned(run, script, fixture, supervisor, spawn_handle),
        }
    }

    fn start_in_process(&self, run: RunStart, script: FakeScript) -> std::io::Result<()> {
        let (answer_tx, answer_rx) = std::sync::mpsc::channel::<(String, String)>();
        let state = Arc::new(InProcessRun {
            cancel: Arc::new(AtomicBool::new(false)),
            awaiting: Mutex::new(None),
            finished: AtomicBool::new(false),
            join: Mutex::new(None),
            answers: answer_tx,
        });
        self.runs.lock().unwrap_or_else(|p| p.into_inner()).insert(
            run.run_id.clone(),
            Arc::new(RunSlot::InProcess(Arc::clone(&state))),
        );

        let bus = Arc::clone(&self.bus);
        let clock = Arc::clone(&self.clock);
        let run_id = run.run_id.clone();
        let token = run.fencing_token;
        let engine_state = Arc::clone(&state);
        let thread = std::thread::Builder::new()
            .name(format!("fake-run-{run_id}"))
            .spawn(move || {
                play_script(
                    &script,
                    &run_id,
                    token,
                    clock.as_ref(),
                    &engine_state,
                    answer_rx,
                    &bus,
                );
                engine_state.finished.store(true, Ordering::Release);
            })
            .map_err(|e| std::io::Error::new(e.kind(), format!("fake engine spawn: {e}")))?;
        *state.join.lock().unwrap_or_else(|p| p.into_inner()) = Some(thread);
        Ok(())
    }

    fn start_spawned(
        &self,
        run: RunStart,
        script: FakeScript,
        fixture: &Path,
        supervisor: &Arc<ExecSupervisor>,
        handle: &tokio::runtime::Handle,
    ) -> std::io::Result<()> {
        let scenario = script
            .to_argv_b64()
            .map_err(|e| std::io::Error::other(format!("scenario encode: {e}")))?;
        let cwd = run.workspace.clone().unwrap_or_else(std::env::temp_dir);
        let terminal = Arc::new(AtomicBool::new(false));
        let request = SpawnRequest {
            exec_id: Id::generate(),
            mission_id: Id::generate(),
            run_id: run.run_id.clone(),
            owner_daemon_id: Id::generate(),
            program: fixture.to_path_buf(),
            argv: vec!["agent-fake".into(), scenario],
            cwd,
            env_overrides: Default::default(),
            env_clear: false,
            stdin: Some(run.prompt_stdin.clone().into_bytes()),
            resource_policy: fake_resource_policy(),
            spool_bytes: DEFAULT_SPOOL_BYTES,
            redactor: None,
            sink: spawn_sink(
                run.run_id.clone(),
                run.fencing_token,
                Arc::clone(&terminal),
                Arc::clone(&self.bus),
            ),
            validate_path: None,
        };
        let exec_handle = supervisor
            .spawn_on(request, handle)
            .map_err(|e| std::io::Error::other(format!("exec spawn: {e}")))?;
        self.runs.lock().unwrap_or_else(|p| p.into_inner()).insert(
            run.run_id.clone(),
            Arc::new(RunSlot::Spawned(SpawnedRun {
                handle: exec_handle.clone(),
                terminal: Arc::clone(&terminal),
            })),
        );

        // EOF/exit watcher: a clean exit without a terminal protocol event
        // is E19 (RESULT_INVALID) or an unknown outcome — never success.
        let bus = Arc::clone(&self.bus);
        let run_id = run.run_id.clone();
        let token = run.fencing_token;
        handle.spawn(async move {
            let exit = exec_handle.wait().await.ok();
            if terminal.load(Ordering::Acquire) {
                return;
            }
            let verdict_invalid = !matches!(exec_handle.output_verdict(), OutputVerdict::Valid);
            let event = match exit.and_then(|info| info.code) {
                Some(0) => AdapterEvent::Failed {
                    run_id: run_id.clone(),
                    fencing_token: token,
                    code: MissionErrorCode::ResultInvalid,
                    message: if verdict_invalid {
                        "output line exceeded the 1 MiB raw cap (03 §2)".into()
                    } else {
                        "fixture exited 0 without a final event (E19)".into()
                    },
                },
                Some(_) => AdapterEvent::Failed {
                    run_id: run_id.clone(),
                    fencing_token: token,
                    code: MissionErrorCode::OutcomeUnknown,
                    message: "fixture exited non-zero without a final event".into(),
                },
                None => AdapterEvent::Failed {
                    run_id: run_id.clone(),
                    fencing_token: token,
                    code: MissionErrorCode::OutcomeUnknown,
                    message: "fixture exit status could not be observed".into(),
                },
            };
            bus.publish(event);
        });
        Ok(())
    }

    fn run_slot(&self, run_id: &Id) -> Option<Arc<RunSlot>> {
        self.runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(run_id)
            .cloned()
    }
}

/// stdout sink for the spawned mode: fixture JSONL → AdapterEvent.
fn spawn_sink(run_id: Id, token: u64, terminal: Arc<AtomicBool>, bus: Arc<EventBus>) -> OutputSink {
    Arc::new(move |kind: StreamKind, line: &[u8]| {
        if kind != StreamKind::Stdout {
            return;
        }
        let Some(event) = parse_fixture_line(line, &run_id, token) else {
            return; // non-protocol line (cut/flood bytes) — display only
        };
        if matches!(
            event,
            AdapterEvent::Result { .. }
                | AdapterEvent::Failed { .. }
                | AdapterEvent::Disconnected { .. }
        ) {
            terminal.store(true, Ordering::Release);
        }
        bus.publish(event);
    })
}

impl AgentAdapter for FakeAdapter {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn start(&self, run: RunStart) -> std::io::Result<()> {
        let script = self
            .default_script
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        self.start_scripted(run, script)
    }

    fn send_message(&self, run_id: &Id, _body: &str) -> DeliveryReceipt {
        match self.run_slot(run_id).as_deref() {
            Some(RunSlot::InProcess(state)) => {
                if state.finished.load(Ordering::Acquire) {
                    DeliveryReceipt::Queued {
                        reason: QueuedReason::NextRun,
                    }
                } else {
                    DeliveryReceipt::Delivered { provider_ref: None }
                }
            }
            Some(RunSlot::Spawned(state)) => {
                if state.handle.inspect() == ExecProbe::Running {
                    DeliveryReceipt::Delivered { provider_ref: None }
                } else {
                    DeliveryReceipt::Queued {
                        reason: QueuedReason::NextRun,
                    }
                }
            }
            None => DeliveryReceipt::Rejected {
                reason: "unknown run",
            },
        }
    }

    fn answer(&self, run_id: &Id, provider_request_id: &str, answer: &str) -> DeliveryReceipt {
        let Some(slot) = self.run_slot(run_id) else {
            return DeliveryReceipt::Rejected {
                reason: "unknown run",
            };
        };
        match &*slot {
            RunSlot::InProcess(state) => {
                let awaiting = state
                    .awaiting
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                match awaiting {
                    Some(open) if open == provider_request_id => {
                        let _ = state.answers.send((open, answer.to_string()));
                        DeliveryReceipt::Delivered {
                            provider_ref: Some(provider_request_id.to_string()),
                        }
                    }
                    _ => DeliveryReceipt::Rejected {
                        reason: "obsolete or unknown approval request",
                    },
                }
            }
            RunSlot::Spawned(state) => {
                if state.terminal.load(Ordering::Acquire) {
                    // Protocol final already observed even while the child
                    // winds down (03 §1: post-final is display-only).
                    return DeliveryReceipt::Rejected {
                        reason: "run already terminal",
                    };
                }
                if state.handle.inspect() == ExecProbe::Running {
                    // The fixture reads answers from stdin when scripted to;
                    // O07 leaves the pipe closed after the prompt payload, so
                    // the receipt is accepted without delivery evidence.
                    DeliveryReceipt::Delivered {
                        provider_ref: Some(provider_request_id.to_string()),
                    }
                } else {
                    DeliveryReceipt::Rejected {
                        reason: "run already terminal",
                    }
                }
            }
        }
    }

    fn interrupt(&self, run_id: &Id) -> CancelReceipt {
        let Some(slot) = self.run_slot(run_id) else {
            return CancelReceipt::Rejected {
                reason: CancelRejected::UnknownRun,
            };
        };
        match &*slot {
            RunSlot::InProcess(state) => {
                if state.finished.load(Ordering::Acquire) {
                    return CancelReceipt::Rejected {
                        reason: CancelRejected::AlreadyTerminal,
                    };
                }
                // Accepted; whether the engine honors it immediately depends
                // on the script (ignore_interrupt keeps it alive — 03 §7).
                state.cancel.store(true, Ordering::Release);
                CancelReceipt::Accepted
            }
            RunSlot::Spawned(state) => match state.handle.inspect() {
                ExecProbe::Running => CancelReceipt::Accepted,
                _ => CancelReceipt::Rejected {
                    reason: CancelRejected::AlreadyTerminal,
                },
            },
        }
    }

    fn inspect(&self, run_id: &Id) -> RunProbe {
        let Some(slot) = self.run_slot(run_id) else {
            return RunProbe::Absent;
        };
        match &*slot {
            RunSlot::InProcess(state) => {
                if state.finished.load(Ordering::Acquire) {
                    RunProbe::Finished { exit: None }
                } else {
                    RunProbe::Running
                }
            }
            RunSlot::Spawned(state) => match state.handle.inspect() {
                ExecProbe::Running => RunProbe::Running,
                ExecProbe::Finished { exit } => RunProbe::Finished { exit },
                ExecProbe::Absent => RunProbe::Unknown,
            },
        }
    }

    fn close(&self, run_id: &Id) -> CancelReceipt {
        let slot = self
            .runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(run_id);
        let Some(slot) = slot else {
            return CancelReceipt::Rejected {
                reason: CancelRejected::UnknownRun,
            };
        };
        match &*slot {
            RunSlot::InProcess(state) => {
                state.cancel.store(true, Ordering::Release);
                let thread = state.join.lock().unwrap_or_else(|p| p.into_inner()).take();
                if let Some(thread) = thread {
                    // The engine exits at the next step boundary once
                    // cancelled; manual-clock sleeps need the test's nudge.
                    let _ = thread.join();
                }
                CancelReceipt::Confirmed { exit: None }
            }
            RunSlot::Spawned(state) => {
                // 02 §9 ladder defaults: interrupt → 10 s → terminate →
                // 5 s → kill → confirmed reap. Blocking by contract.
                match state
                    .handle
                    .stop_blocking(Duration::from_secs(10), Duration::from_secs(5))
                {
                    Ok(_) => CancelReceipt::Confirmed {
                        exit: match state.handle.inspect() {
                            ExecProbe::Finished { exit } => exit,
                            _ => None,
                        },
                    },
                    Err(_) => CancelReceipt::Accepted,
                }
            }
        }
    }

    fn subscribe(&self) -> EventStream {
        let (tx, rx) = mpsc::unbounded_channel();
        self.bus
            .subscribers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(tx);
        EventStream::new(rx, Arc::clone(&self.gate))
    }
}

// ---- in-process engine -----------------------------------------------------

fn play_script(
    script: &FakeScript,
    run_id: &Id,
    token: u64,
    clock: &dyn FakeClock,
    state: &InProcessRun,
    answer_rx: std::sync::mpsc::Receiver<(String, String)>,
    bus: &EventBus,
) {
    let cancelled = || state.cancel.load(Ordering::Acquire);
    for step in &script.steps {
        // Cancel stops playback — except the scripted late final, whose
        // whole purpose is to arrive after cancel (E11).
        if cancelled()
            && !script.ignore_interrupt
            && !matches!(step, FakeStep::LateResultAfterCancel { .. })
        {
            return;
        }
        match step {
            FakeStep::Started {
                session_id,
                turn_id,
            } => bus.publish(AdapterEvent::Started {
                run_id: run_id.clone(),
                fencing_token: token,
                provider_session_id: session_id.clone(),
                provider_turn_id: turn_id.clone(),
            }),
            FakeStep::Activity { text } => bus.publish(AdapterEvent::Activity {
                run_id: run_id.clone(),
                fencing_token: token,
                chunk: text.clone(),
            }),
            FakeStep::Approval {
                request_id,
                question,
            } => {
                bus.publish(AdapterEvent::ApprovalRequested {
                    run_id: run_id.clone(),
                    fencing_token: token,
                    provider_request_id: request_id.clone(),
                    question: question.clone(),
                });
                *state.awaiting.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(request_id.clone());
                wait_for_answer(&answer_rx, request_id, &state.cancel);
                *state.awaiting.lock().unwrap_or_else(|p| p.into_inner()) = None;
            }
            FakeStep::Usage {
                input_tokens,
                output_tokens,
                cost_usd_micros,
            } => bus.publish(AdapterEvent::Usage {
                run_id: run_id.clone(),
                fencing_token: token,
                input_tokens: *input_tokens,
                output_tokens: *output_tokens,
                cost_usd_micros: *cost_usd_micros,
            }),
            FakeStep::Result { value } => bus.publish(AdapterEvent::Result {
                run_id: run_id.clone(),
                fencing_token: token,
                result: value.clone(),
            }),
            FakeStep::Fail { code, message } => {
                let code = parse_error_code(code).unwrap_or(MissionErrorCode::Internal);
                bus.publish(AdapterEvent::Failed {
                    run_id: run_id.clone(),
                    fencing_token: token,
                    code,
                    message: message.clone(),
                })
            }
            FakeStep::Disconnect => bus.publish(AdapterEvent::Disconnected {
                run_id: run_id.clone(),
                fencing_token: token,
            }),
            FakeStep::Delay { ms } => clock.sleep_until(clock.now_ms().saturating_add(*ms)),
            FakeStep::LateResultAfterCancel { value } => {
                // Only fires after cancel; the original token is stale by
                // then — an advanced consumer gate must drop it (E11).
                if cancelled() {
                    bus.publish(AdapterEvent::Result {
                        run_id: run_id.clone(),
                        fencing_token: token,
                        result: value.clone(),
                    });
                }
            }
            FakeStep::FileWrite { path, bytes } => {
                // In-process mode performs the write too; policing is the
                // exec-side hook's job in spawn mode.
                write_bytes(path, *bytes);
            }
            // Pipe-mechanics steps exist for the fixture child; the
            // in-process engine has no pipes to stress.
            FakeStep::FloodStdout { .. } | FakeStep::NoNewline { .. } | FakeStep::PartialJson => {}
            FakeStep::ExitLate { ms } => clock.sleep_until(clock.now_ms().saturating_add(*ms)),
        }
    }
    if script.exit_late_ms > 0 {
        clock.sleep_until(clock.now_ms().saturating_add(script.exit_late_ms));
    }
}

fn wait_for_answer(
    answer_rx: &std::sync::mpsc::Receiver<(String, String)>,
    request_id: &str,
    cancel: &AtomicBool,
) {
    loop {
        if cancel.load(Ordering::Acquire) {
            return;
        }
        match answer_rx.recv_timeout(Duration::from_millis(25)) {
            Ok((open, _answer)) if open == request_id => return,
            Ok(_) => continue, // stale answer for another request
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn write_bytes(path: &str, bytes: usize) {
    use std::io::Write;
    let body = vec![b'f'; bytes];
    if let Ok(mut file) = std::fs::File::create(path) {
        let _ = file.write_all(&body);
    }
}

fn parse_error_code(code: &str) -> Option<MissionErrorCode> {
    serde_json::from_value::<MissionErrorCode>(serde_json::Value::String(code.to_string())).ok()
}

/// Fixture stdout line → AdapterEvent (`None` for non-protocol lines).
fn parse_fixture_line(line: &[u8], run_id: &Id, token: u64) -> Option<AdapterEvent> {
    let trimmed: &[u8] = if line.last() == Some(&b'\n') {
        &line[..line.len() - 1]
    } else {
        line
    };
    let value: serde_json::Value = serde_json::from_slice(trimmed).ok()?;
    let tag = value.get("t")?.as_str()?;
    let event = match tag {
        "started" => AdapterEvent::Started {
            run_id: run_id.clone(),
            fencing_token: token,
            provider_session_id: value
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            provider_turn_id: value
                .get("turn_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        },
        "activity" => AdapterEvent::Activity {
            run_id: run_id.clone(),
            fencing_token: token,
            chunk: value.get("text")?.as_str()?.to_string(),
        },
        "approval" => AdapterEvent::ApprovalRequested {
            run_id: run_id.clone(),
            fencing_token: token,
            provider_request_id: value.get("request_id")?.as_str()?.to_string(),
            question: value.get("question")?.as_str()?.to_string(),
        },
        "usage" => AdapterEvent::Usage {
            run_id: run_id.clone(),
            fencing_token: token,
            input_tokens: value.get("input_tokens").and_then(|v| v.as_u64()),
            output_tokens: value.get("output_tokens").and_then(|v| v.as_u64()),
            cost_usd_micros: value.get("cost_usd_micros").and_then(|v| v.as_u64()),
        },
        "result" => {
            let result: ProviderResult =
                serde_json::from_value(value.get("value")?.clone()).ok()?;
            AdapterEvent::Result {
                run_id: run_id.clone(),
                fencing_token: token,
                result,
            }
        }
        "failed" => AdapterEvent::Failed {
            run_id: run_id.clone(),
            fencing_token: token,
            code: parse_error_code(value.get("code")?.as_str()?)?,
            message: value.get("message")?.as_str()?.to_string(),
        },
        "disconnect" => AdapterEvent::Disconnected {
            run_id: run_id.clone(),
            fencing_token: token,
        },
        _ => return None,
    };
    Some(event)
}

// ---- defaults --------------------------------------------------------------

fn noop_persist() -> PersistExec {
    Arc::new(|_record: term_contracts::mission::types::ExecRecord| {})
}

fn default_admission_config() -> term_core::AdmissionConfig {
    let logical_cpus = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    term_contracts::defaults::load_spec_defaults()
        .map(|d| term_core::AdmissionConfig::from_defaults(&d, logical_cpus))
        .unwrap_or(term_core::AdmissionConfig {
            logical_cpus,
            managed_concurrency: 2,
            telemetry_stale_ms: 3_000,
            host_reserve_min_bytes: 2 << 30,
            host_reserve_percent: 15,
            managed_budget_percent: 50,
        })
}

fn healthy_host() -> term_core::AdmissionHost {
    term_core::AdmissionHost {
        total_bytes: 16 << 30,
        available_bytes: Some(10 << 30),
        sample_age_ms: 0,
        reconciliation_required: false,
        pressure: PressureLevel::Normal,
    }
}

/// Constant-fit decimal string (values here are tiny literals).
fn u64s(value: u64) -> U64String {
    U64String::new(value).unwrap_or_else(|_| U64String::parse("0").expect("0 parses"))
}

fn fake_resource_policy() -> LaunchPolicy {
    LaunchPolicy {
        reservation_bytes: u64s(64 << 20),
        cpu_slots: 1,
        enforcement: Enforcement::Prefer,
        memory_max_bytes: None,
        cpu_max_cores: None,
        pids_max: None,
    }
}

/// A fully-formed fake binding for tests/dev (03 §6: capability evidence is
/// per runtime+OS; the fake advertises exactly what O07 exercises).
pub fn fake_binding() -> Binding {
    let yes = || Support {
        supported: true,
        reason_code: None,
    };
    let no = |reason: &'static str| Support {
        supported: false,
        reason_code: Some(reason.into()),
    };
    Binding {
        id: Id::generate(),
        revision: u64s(1),
        label: "Fake runtime (dev/CI)".into(),
        runtime: RuntimeKind::Fake,
        program: "term-fixture".into(),
        runtime_version: None,
        provider_id: "fake".into(),
        model_id: "fake-model".into(),
        effort: None,
        auth_route: AuthRoute::Local,
        credential_ref: None,
        endpoint_ref: None,
        capabilities: RuntimeCapabilities {
            structured_result: yes(),
            events: yes(),
            cancel: yes(),
            resume: no("fake does not persist sessions"),
            steer: no("fake has no active-turn steer"),
            approval_reply: yes(),
            read_only: yes(),
            scoped_write: yes(),
            model_listing: no("fake has a single fixed model"),
            usage: yes(),
            native_terminal_attach: no("fake is not a terminal runtime"),
        },
        checked_at: None,
        enabled: true,
        experimental_version: None,
        local_evidence: None,
        estimated_run_cost_usd_micros: None,
        resource_policy: fake_resource_policy(),
    }
}

// ---- tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn run_start() -> RunStart {
        RunStart {
            task_kind: None,
            mission_id: Id::generate(),
            owner_daemon_id: Id::generate(),
            workspace_access: crate::agent_runtime::WorkspaceAccess::ReadOnly,
            allow_network: false,
            run_id: Id::generate(),
            fencing_token: 1,
            binding: fake_binding(),
            context_path: std::env::temp_dir(),
            workspace: None,
            prompt_stdin: "do the fake thing".into(),
        }
    }

    /// The engine thread flips `finished` a few instructions AFTER publishing
    /// the terminal event — `publish` wakes the awaiting test task
    /// synchronously, and on Linux the woken task can reach `inspect` before
    /// the engine's epilogue stores the flag. Spawned mode has the same
    /// post-final lag by design (03 §1: post-final is display-only), so the
    /// contract is "finished shortly after the terminal event", not "at the
    /// same instant" — poll with a wall-clock bound instead of asserting the
    /// flag from the event handler.
    fn finished_probe(adapter: &FakeAdapter, run_id: &Id) -> RunProbe {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let probe = adapter.inspect(run_id);
            if !matches!(probe, RunProbe::Running) || std::time::Instant::now() > deadline {
                return probe;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn script_json_round_trips_for_the_fixture_protocol() {
        let script = FakeScript {
            steps: vec![
                FakeStep::Started {
                    session_id: Some("s1".into()),
                    turn_id: None,
                },
                FakeStep::Activity {
                    text: "hello".into(),
                },
                FakeStep::Result {
                    value: ProviderResult::Report {
                        report_text: "done".into(),
                        knowledge: Vec::new(),
                    },
                },
            ],
            ignore_interrupt: false,
            exit_late_ms: 250,
        };
        let json = serde_json::to_string(&script).expect("encode");
        let back: FakeScript = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, script);
        assert!(script.to_argv_b64().expect("b64").len() > 16);
    }

    #[test]
    fn manual_clock_sleeps_until_advanced() {
        let clock = ManualClock::new(0);
        let waker = Arc::clone(&clock);
        let sleeper = Arc::clone(&clock);
        let thread = std::thread::spawn(move || {
            sleeper.sleep_until(50);
            sleeper.now_ms()
        });
        std::thread::sleep(Duration::from_millis(20));
        waker.advance_to(50);
        let woke_at = thread.join().expect("join");
        assert!(woke_at >= 50, "engine woke at fake time {woke_at}");
    }

    #[tokio::test]
    async fn in_process_happy_script_emits_started_activity_result() {
        let adapter = FakeAdapter::in_process(ManualClock::new(0));
        let mut stream = adapter.subscribe();
        let run = run_start();
        let run_id = run.run_id.clone();
        adapter
            .start_scripted(run, FakeScript::happy())
            .expect("start");

        let mut got = Vec::new();
        while got.len() < 3 {
            let Some(event) = stream.next_timeout(Duration::from_secs(5)).await else {
                break;
            };
            got.push(match event {
                AdapterEvent::Started { .. } => "started",
                AdapterEvent::Activity { chunk, .. } => {
                    assert_eq!(chunk, "fake activity");
                    "activity"
                }
                AdapterEvent::Result { result, .. } => {
                    assert!(matches!(result, ProviderResult::Report { .. }));
                    "result"
                }
                other => panic!("unexpected event {other:?}"),
            });
        }
        assert_eq!(got, vec!["started", "activity", "result"]);
        assert_eq!(
            finished_probe(&adapter, &run_id),
            RunProbe::Finished { exit: None }
        );
        // Post-terminal deltas are refused (03 §1).
        assert_eq!(
            adapter.interrupt(&run_id),
            CancelReceipt::Rejected {
                reason: CancelRejected::AlreadyTerminal
            }
        );
        assert_eq!(
            adapter.close(&run_id),
            CancelReceipt::Confirmed { exit: None }
        );
    }

    #[tokio::test]
    async fn approval_flows_answer_to_the_open_request_only() {
        let adapter = FakeAdapter::in_process(ManualClock::new(0));
        let mut stream = adapter.subscribe();
        let run = run_start();
        let run_id = run.run_id.clone();
        let script = FakeScript {
            steps: vec![
                FakeStep::Started {
                    session_id: None,
                    turn_id: None,
                },
                FakeStep::Approval {
                    request_id: "req-1".into(),
                    question: "Proceed?".into(),
                },
                FakeStep::Activity {
                    text: "after approval".into(),
                },
            ],
            ignore_interrupt: false,
            exit_late_ms: 0,
        };
        adapter.start_scripted(run, script).expect("start");

        // Obsolete request first: rejected without consuming req-1.
        let event = stream
            .next_timeout(Duration::from_secs(5))
            .await
            .expect("started");
        assert!(matches!(event, AdapterEvent::Started { .. }));
        let event = stream
            .next_timeout(Duration::from_secs(5))
            .await
            .expect("approval");
        let AdapterEvent::ApprovalRequested {
            provider_request_id,
            ..
        } = event
        else {
            panic!("expected approval");
        };
        assert_eq!(provider_request_id, "req-1");
        assert_eq!(
            adapter.answer(&run_id, "req-42", "yes"),
            DeliveryReceipt::Rejected {
                reason: "obsolete or unknown approval request"
            }
        );
        assert_eq!(
            adapter.answer(&run_id, "req-1", "yes"),
            DeliveryReceipt::Delivered {
                provider_ref: Some("req-1".into())
            }
        );
        let event = stream
            .next_timeout(Duration::from_secs(5))
            .await
            .expect("activity");
        assert!(matches!(event, AdapterEvent::Activity { .. }));
        adapter.close(&run_id);
    }

    #[tokio::test]
    async fn cancel_stops_the_engine_and_late_result_carries_the_old_token() {
        let clock = ManualClock::new(0);
        let adapter = FakeAdapter::in_process(Arc::clone(&clock) as Arc<dyn FakeClock>);
        let mut stream = adapter.subscribe();
        let run = run_start();
        let run_id = run.run_id.clone();
        let script = FakeScript {
            steps: vec![
                FakeStep::Started {
                    session_id: None,
                    turn_id: None,
                },
                FakeStep::Delay { ms: 10 },
                FakeStep::LateResultAfterCancel {
                    value: ProviderResult::Report {
                        report_text: "late".into(),
                        knowledge: Vec::new(),
                    },
                },
            ],
            ignore_interrupt: false,
            exit_late_ms: 0,
        };
        adapter.start_scripted(run, script).expect("start");
        let event = stream
            .next_timeout(Duration::from_secs(5))
            .await
            .expect("started");
        assert!(matches!(event, AdapterEvent::Started { .. }));
        // Cancel before the delay elapses; the engine still emits the late
        // result with the ORIGINAL token — the advance-and-drop behavior is
        // the consumer gate's job (integration test).
        assert_eq!(adapter.interrupt(&run_id), CancelReceipt::Accepted);
        // Started is emitted before the worker registers Delay. Advance to
        // the saturating endpoint so either scheduling order releases it;
        // advancing to 100 can race a subsequently registered deadline 110.
        clock.advance_to(u64::MAX);
        let late = stream
            .next_timeout(Duration::from_secs(5))
            .await
            .expect("late result");
        assert!(matches!(late, AdapterEvent::Result { .. }));
        assert_eq!(
            finished_probe(&adapter, &run_id),
            RunProbe::Finished { exit: None }
        );
        adapter.close(&run_id);
    }

    #[test]
    fn fixture_line_parsing_maps_every_protocol_event() {
        let run_id = Id::generate();
        let line = br#"{"t":"activity","text":"chunk"}"#;
        assert!(matches!(
            parse_fixture_line(line, &run_id, 7),
            Some(AdapterEvent::Activity { .. })
        ));
        let junk = b"not json at all";
        assert!(parse_fixture_line(junk, &run_id, 7).is_none());
        let failed = br#"{"t":"failed","code":"PROVIDER_UNAVAILABLE","message":"x"}"#;
        assert!(matches!(
            parse_fixture_line(failed, &run_id, 7),
            Some(AdapterEvent::Failed { .. })
        ));
    }

    #[test]
    fn fake_binding_shape_is_complete() {
        let binding = fake_binding();
        assert_eq!(binding.runtime, RuntimeKind::Fake);
        assert!(binding.capabilities.events.supported);
        assert!(!binding.capabilities.steer.supported);
        assert_eq!(binding.resource_policy.cpu_slots, 1);
    }
}
