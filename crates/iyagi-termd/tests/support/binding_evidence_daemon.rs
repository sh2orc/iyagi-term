//! Real IPC/native-process harness with explicitly injected protocol evidence.
//! This is test code: no RPC, config flag, or shipped binary can enable it.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use iyagi_termd_lib::{
    agent_runtime::{
        capability_evidence, codex::CodexAdapter, fake::fake_binding, installation::ProbeFailure,
    },
    exec::{gated::GateConfig, ExecSupervisor},
    mission::{
        actor::{AdapterFactory, MissionActor},
        artifacts::ArtifactStore,
        MissionService,
    },
    Daemon,
};
use term_contracts::mission::types::{AuthRoute, Id, RuntimeKind};

pub struct EvidenceDaemon {
    pub endpoint: String,
    pub token: String,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    _directory: tempfile::TempDir,
}

impl EvidenceDaemon {
    pub fn spawn() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        let mut daemon = Daemon::start(directory.path().into(), runtime.handle().clone()).unwrap();
        let state = Arc::get_mut(&mut daemon.state).expect("daemon has not started workers");
        let fixture = super::common::fixture_bin();
        let probe_program = fixture.clone();
        let service = Arc::new(
            MissionService::new(
                state.storage.clone(),
                ArtifactStore::new(state.storage.clone(), state.paths.missions_dir()),
            )
            .with_daemon_id(Id::parse(&state.daemon_id).unwrap())
            .with_binding_evidence(
                move |program, kind| {
                    if program == probe_program && kind == RuntimeKind::Codex {
                        Ok("protocol-fixture-v1".into())
                    } else {
                        Err(ProbeFailure::NotFound)
                    }
                },
                move |binding, _, version| {
                    if binding.program == fixture
                        && binding.runtime == RuntimeKind::Codex
                        && binding.provider_id == "openai"
                        && binding.auth_route == AuthRoute::Subscription
                        && binding.credential_ref.is_none()
                        && binding.endpoint_ref.is_none()
                        && version == Some("protocol-fixture-v1")
                    {
                        let mut caps = fake_binding().capabilities;
                        caps.steer.supported = true;
                        caps.steer.reason_code = None;
                        caps
                    } else {
                        capability_evidence::unclaimed()
                    }
                },
            ),
        );
        service.recover_on_startup().unwrap();
        state.missions = Some(service.clone());
        let state = daemon.state;
        let supervisor = Arc::new(ExecSupervisor::persistent(
            state.config.admission_config(state.logical_cpus),
            service.exec_persistence(),
            state.admission_host(),
            GateConfig {
                helper_program: super::common::daemon_bin().into(),
                directory: state.paths.socket_dir().into(),
                platform: state.platform.clone(),
                timeout: state.config.gate_timeout(),
            },
        ));
        let exec = supervisor.clone();
        let handle = runtime.handle().clone();
        let configs = state.paths.missions_dir().join("runtime/codex");
        let factory: AdapterFactory = Arc::new(move |_| {
            Ok(CodexAdapter::authenticated(
                exec.clone(),
                handle.clone(),
                None,
                configs.clone(),
            ))
        });
        let mut actor = MissionActor::new(service.clone(), state.paths.missions_dir(), factory)
            .with_deterministic_exec(supervisor.clone(), runtime.handle().clone());
        let endpoint = state.paths.main_endpoint();
        let token = std::fs::read_to_string(state.paths.socket_dir().join("token"))
            .unwrap()
            .trim()
            .into();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            let _entered = runtime.enter();
            iyagi_termd_lib::supervisor::spawn_supervised(
                &state,
                "telemetry",
                iyagi_termd_lib::telemetry_loop::run,
            );
            let ipc = runtime.spawn(iyagi_termd_lib::ipc::serve(state.clone()));
            while !stopped.load(Ordering::Acquire) {
                supervisor.update_host(state.admission_host());
                supervisor.refresh_recovery().unwrap();
                actor.set_dispatch_permitted(supervisor.ledger().recovery_ready());
                actor.tick().unwrap();
                std::thread::sleep(Duration::from_millis(25));
            }
            actor.shutdown();
            let _ = state.shutdown.send(true);
            ipc.abort();
            drop(_entered);
            runtime.shutdown_background();
        });
        Self {
            endpoint,
            token,
            stop,
            worker: Some(worker),
            _directory: directory,
        }
    }

    pub fn kill(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
impl Drop for EvidenceDaemon {
    fn drop(&mut self) {
        self.kill();
    }
}
