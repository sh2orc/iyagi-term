//! Non-blocking AgentAdapter bridge. Session creation and SSE run on an
//! owned worker; the mission actor can cancel during server startup or a
//! stalled HTTP request. The transport factory must clean up any partially
//! launched child before returning an error.

use super::{OpencodeAdapter, OpencodeTransport};
use crate::agent_runtime::{
    AdapterEvent, AgentAdapter, CancelReceipt, CancelRejected, DeliveryReceipt, EventStream,
    FencingGate, QueuedReason, RunProbe, RunStart,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use term_contracts::mission::types::Id;
use term_contracts::mission::MissionErrorCode;
use tokio::sync::mpsc;

pub type TransportFactory = Arc<
    dyn Fn(&RunStart, &AtomicBool) -> std::io::Result<Arc<dyn OpencodeTransport>> + Send + Sync,
>;

struct RunSlot {
    cancelled: AtomicBool,
    done: AtomicBool,
    core: Mutex<Option<Arc<OpencodeAdapter>>>,
    transport: Mutex<Option<Arc<dyn OpencodeTransport>>>,
}

/// Event fan-out for this adapter's runs. Delegates to the bounded fan-out
/// shared with the other runtime adapters (F1): Activity text and
/// latest-wins observations stage behind a per-window delivery quota with
/// oldest-first shedding above a byte budget, while order-critical kinds
/// (approvals, terminal results, errors) are always delivered immediately.
#[derive(Default)]
struct Bus {
    inner: Arc<crate::agent_runtime::codex::BoundedFanout>,
}
impl Bus {
    fn emit(&self, event: AdapterEvent) {
        self.inner.publish(event);
    }
    fn add_subscriber(&self, tx: mpsc::UnboundedSender<AdapterEvent>) {
        self.inner.add_subscriber(tx);
    }
    /// See [`crate::agent_runtime::codex::BoundedFanout::start_flusher`].
    fn start_flusher(bus: &Arc<Self>) {
        crate::agent_runtime::codex::BoundedFanout::start_flusher(&bus.inner);
    }
}

pub struct OpenCodeRuntimeAdapter {
    factory: TransportFactory,
    runs: Mutex<HashMap<Id, Arc<RunSlot>>>,
    bus: Arc<Bus>,
    gate: Arc<FencingGate>,
}

impl OpenCodeRuntimeAdapter {
    pub fn supervised(
        supervisor: Arc<crate::exec::ExecSupervisor>,
        runtime: tokio::runtime::Handle,
        connections: Arc<crate::connections::ConnectionStore>,
        config_root: std::path::PathBuf,
    ) -> Arc<Self> {
        Self::with_factory(Arc::new(move |start, cancelled| {
            let connection = connections.resolve_opencode(&start.binding)?;
            super::server::spawn_resolved(
                start,
                &supervisor,
                &runtime,
                connection,
                &config_root,
                cancelled,
            )
        }))
    }
    pub fn with_factory(factory: TransportFactory) -> Arc<Self> {
        let bus = Arc::new(Bus::default());
        Bus::start_flusher(&bus);
        Arc::new(Self {
            factory,
            runs: Mutex::new(HashMap::new()),
            bus,
            gate: FencingGate::new(),
        })
    }
    fn run(&self, id: &Id) -> Option<Arc<RunSlot>> {
        self.runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .cloned()
    }
}

impl AgentAdapter for OpenCodeRuntimeAdapter {
    fn name(&self) -> &'static str {
        "opencode"
    }
    fn start(&self, start: RunStart) -> std::io::Result<()> {
        let slot = Arc::new(RunSlot {
            cancelled: AtomicBool::new(false),
            done: AtomicBool::new(false),
            core: Mutex::new(None),
            transport: Mutex::new(None),
        });
        {
            let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
            if runs.contains_key(&start.run_id) {
                return Err(std::io::Error::other("OpenCode run already owns a worker"));
            }
            runs.insert(start.run_id.clone(), slot.clone());
        }
        self.gate.register(&start.run_id, start.fencing_token);
        let id = start.run_id.clone();
        let factory = self.factory.clone();
        let bus = self.bus.clone();
        let worker_slot = slot.clone();
        let spawned = std::thread::Builder::new()
            .name("opencode-run".into())
            .spawn(move || {
                let slot = worker_slot;
                let transport = match factory(&start, &slot.cancelled) {
                    Ok(transport) => transport,
                    Err(error) => {
                        let error = crate::agent_runtime::retry::mark_before_submission(error);
                        bus.emit(
                            if crate::agent_runtime::retry::has_submission_proof(&error) {
                                crate::agent_runtime::retry::failure(
                                    start.run_id,
                                    start.fencing_token,
                                    MissionErrorCode::ProviderUnavailable,
                                    "OpenCode server failed before task submission".into(),
                                )
                            } else {
                                AdapterEvent::Failed {
                                    run_id: start.run_id,
                                    fencing_token: start.fencing_token,
                                    code: match error.kind() {
                                        std::io::ErrorKind::PermissionDenied => {
                                            MissionErrorCode::AuthRequired
                                        }
                                        std::io::ErrorKind::InvalidInput => {
                                            MissionErrorCode::PolicyDenied
                                        }
                                        _ => MissionErrorCode::ProviderUnavailable,
                                    },
                                    message: "OpenCode server could not start".into(),
                                }
                            },
                        );
                        slot.done.store(true, Ordering::Release);
                        return;
                    }
                };
                *slot.transport.lock().unwrap_or_else(|p| p.into_inner()) = Some(transport.clone());
                let core = Arc::new(OpencodeAdapter::with_transport(
                    start.binding.clone(),
                    transport.clone(),
                ));
                *slot.core.lock().unwrap_or_else(|p| p.into_inner()) = Some(core.clone());
                let mut terminal = false;
                let mut emit = |event: AdapterEvent| {
                    terminal |= event.is_terminal();
                    bus.emit(event);
                };
                if !slot.cancelled.load(Ordering::Acquire) {
                    let result = core.start(&start, &mut emit).and_then(|_| {
                        core.drive_turn(&start.run_id, &start.prompt_stdin, &mut emit)
                    });
                    if let Err(error) = result {
                        if !terminal {
                            bus.emit(AdapterEvent::Failed {
                                run_id: start.run_id.clone(),
                                fencing_token: start.fencing_token,
                                code: error.code,
                                message: error.message,
                            });
                            terminal = true;
                        }
                    }
                }
                if !terminal {
                    bus.emit(AdapterEvent::Disconnected {
                        run_id: start.run_id.clone(),
                        fencing_token: start.fencing_token,
                    });
                }
                transport.close();
                // The protocol worker retains process ownership until the owner
                // confirms cleanup, including on terminal-result and error paths.
                while !matches!(transport.process_probe(), RunProbe::Finished { .. }) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                slot.done.store(true, Ordering::Release);
            });
        if let Err(error) = spawned {
            self.runs
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
            return Err(error);
        }
        Ok(())
    }
    fn send_message(&self, _id: &Id, _body: &str) -> DeliveryReceipt {
        DeliveryReceipt::Queued {
            reason: QueuedReason::SteerUnsupported,
        }
    }
    fn answer(&self, id: &Id, request: &str, answer: &str) -> DeliveryReceipt {
        let Some(slot) = self.run(id) else {
            return DeliveryReceipt::Rejected {
                reason: "unknown run",
            };
        };
        let core = slot.core.lock().unwrap_or_else(|p| p.into_inner()).clone();
        core.map(|core| core.answer(id, request, answer))
            .unwrap_or(DeliveryReceipt::Rejected {
                reason: "OpenCode session not ready",
            })
    }
    fn interrupt(&self, id: &Id) -> CancelReceipt {
        let Some(slot) = self.run(id) else {
            return CancelReceipt::Rejected {
                reason: CancelRejected::UnknownRun,
            };
        };
        if slot.done.load(Ordering::Acquire) {
            return CancelReceipt::Confirmed { exit: None };
        }
        if !slot.cancelled.swap(true, Ordering::AcqRel) {
            let core = slot.core.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let transport = slot
                .transport
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            let id = id.clone();
            // abort and child cleanup are separate operations; a blocked HTTP
            // abort must not keep the actor from cancelling the owned process.
            if let Some(core) = core {
                let _ = std::thread::Builder::new()
                    .name("opencode-abort".into())
                    .spawn(move || {
                        core.interrupt(&id);
                    });
            }
            if let Some(transport) = transport {
                transport.close();
            }
        }
        CancelReceipt::Accepted
    }
    fn inspect(&self, id: &Id) -> RunProbe {
        let Some(slot) = self.run(id) else {
            return RunProbe::Absent;
        };
        if slot.done.load(Ordering::Acquire) {
            RunProbe::Finished { exit: None }
        } else {
            RunProbe::Running
        }
    }
    fn close(&self, id: &Id) -> CancelReceipt {
        let Some(slot) = self.run(id) else {
            return CancelReceipt::Rejected {
                reason: CancelRejected::UnknownRun,
            };
        };
        if slot.done.load(Ordering::Acquire) {
            return CancelReceipt::Confirmed { exit: None };
        }
        slot.cancelled.store(true, Ordering::Release);
        if let Some(transport) = slot
            .transport
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            transport.close();
        }
        CancelReceipt::Accepted
    }
    fn subscribe(&self) -> EventStream {
        let (tx, rx) = mpsc::unbounded_channel();
        self.bus.add_subscriber(tx);
        EventStream::new(rx, self.gate.clone())
    }
}

impl Drop for OpenCodeRuntimeAdapter {
    fn drop(&mut self) {
        for slot in self.runs.lock().unwrap_or_else(|p| p.into_inner()).values() {
            slot.cancelled.store(true, Ordering::Release);
            if let Some(transport) = slot
                .transport
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
            {
                transport.close();
            }
        }
    }
}
