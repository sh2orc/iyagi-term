//! Codex's bidirectional JSONL transport on the shared Exec lifecycle.
use super::peer::{PeerError, PeerEvent, ProtocolPeer, MAX_LINE_BYTES};
use crate::agent_runtime::RunStart;
use crate::exec::input::ExecInput;
use crate::exec::output::{bounded_stdout_inbox, InboxError, LineInbox};
use crate::exec::{ExecHandle, ExecProbe, ExecSupervisor, SpawnRequest};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

pub struct SupervisedPeer {
    exec: ExecHandle,
    input: ExecInput,
    inbox: Mutex<LineInbox>,
    runtime: tokio::runtime::Handle,
    closed: AtomicBool,
    auth_scope: Option<super::auth::AuthScope>,
    api_key: Mutex<Option<zeroize::Zeroizing<String>>>,
    private: Option<super::auth::PrivateDirectory>,
}

impl SupervisedPeer {
    pub fn spawn(
        run: &RunStart,
        supervisor: &ExecSupervisor,
        runtime: &tokio::runtime::Handle,
    ) -> Result<Arc<Self>, PeerError> {
        Self::spawn_launch(run, supervisor, runtime, None)
    }

    pub fn spawn_authenticated(
        run: &RunStart,
        supervisor: &ExecSupervisor,
        runtime: &tokio::runtime::Handle,
        connections: Option<&crate::connections::ConnectionStore>,
        config_root: &std::path::Path,
    ) -> std::io::Result<Arc<Self>> {
        let launch = super::auth::AuthLaunch::prepare(run, connections, config_root)?;
        Self::spawn_launch(run, supervisor, runtime, Some(launch))
            .map_err(|_| std::io::Error::other("Codex app-server launch failed"))
    }

    fn spawn_launch(
        run: &RunStart,
        supervisor: &ExecSupervisor,
        runtime: &tokio::runtime::Handle,
        launch: Option<super::auth::AuthLaunch>,
    ) -> Result<Arc<Self>, PeerError> {
        let (raw_sink, inbox) = bounded_stdout_inbox();
        let private = launch.as_ref().and_then(|l| l.private.clone());
        let sink_private = private.clone();
        let sink: crate::exec::OutputSink = Arc::new(move |kind, bytes| {
            // Exec's stream taps retain this owner even if startup fails or
            // the external peer is dropped before durable cleanup completes.
            let _private = &sink_private;
            raw_sink(kind, bytes);
        });
        let argv = launch.as_ref().map(|l| l.argv.clone()).unwrap_or_else(|| {
            let mut argv = super::daemon_isolation::argv_prefix(std::path::Path::new(
                run.binding.program.as_str(),
            ));
            argv.push("app-server".into());
            argv
        });
        let exec = supervisor
            .spawn_interactive_on(
                SpawnRequest {
                    exec_id: term_contracts::mission::types::Id::generate(),
                    mission_id: run.mission_id.clone(),
                    run_id: run.run_id.clone(),
                    owner_daemon_id: run.owner_daemon_id.clone(),
                    program: run.binding.program.clone().into(),
                    argv,
                    cwd: run.workspace.clone().unwrap_or_else(std::env::temp_dir),
                    env_overrides: launch
                        .as_ref()
                        .map(|l| l.environment.clone())
                        .unwrap_or_default(),
                    env_clear: launch.is_some(),
                    stdin: None,
                    resource_policy: run.binding.resource_policy.clone(),
                    spool_bytes: 8 * 1024,
                    redactor: launch
                        .as_ref()
                        .and_then(|l| l.redactor.clone())
                        .map(|r| r as Arc<dyn crate::exec::output::Redactor>),
                    sink,
                    validate_path: None,
                },
                runtime,
            )
            .map_err(|_| PeerError("supervised app-server launch failed".into()))?;
        let input = exec.input().expect("interactive exec owns stdin");
        let (auth_scope, api_key) = launch
            .map(|l| (Some(l.scope), l.key))
            .unwrap_or((None, None));
        Ok(Arc::new(Self {
            exec,
            input,
            inbox: Mutex::new(inbox),
            runtime: runtime.clone(),
            closed: AtomicBool::new(false),
            auth_scope,
            api_key: Mutex::new(api_key),
            private,
        }))
    }

    pub fn exec_id(&self) -> &term_contracts::mission::types::Id {
        self.exec.exec_id()
    }
}

impl ProtocolPeer for SupervisedPeer {
    fn auth_scope(&self) -> Option<super::auth::AuthScope> {
        self.auth_scope.clone()
    }
    fn take_api_key(&self) -> Option<zeroize::Zeroizing<String>> {
        self.api_key
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }
    fn send(&self, message: &Value) -> Result<(), PeerError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(PeerError("app-server peer is closed".into()));
        }
        struct Frame(Vec<u8>);
        impl std::io::Write for Frame {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > MAX_LINE_BYTES.saturating_sub(1 + self.0.len()) {
                    return Err(std::io::Error::other("frame overcap"));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut frame = Frame(Vec::new());
        serde_json::to_writer(&mut frame, message)
            .map_err(|_| PeerError("outbound app-server frame exceeds its byte limit".into()))?;
        let mut line = frame.0;
        line.push(b'\n');
        self.input.write_blocking(&line).map_err(|error| {
            self.close();
            PeerError(error.to_string())
        })
    }

    fn recv(&self) -> PeerEvent {
        let inbox = self.inbox.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if self.closed.load(Ordering::Acquire) {
                return PeerEvent::Eof;
            }
            let ended = self.exec.stdout_done();
            if matches!(
                self.exec.output_verdict(),
                crate::exec::OutputVerdict::Invalid { .. }
            ) {
                return PeerEvent::Overcap;
            }
            match inbox.recv_timeout(if ended {
                Duration::ZERO
            } else {
                Duration::from_millis(25)
            }) {
                Ok(line) => {
                    return serde_json::from_slice(&line)
                        .map(PeerEvent::Message)
                        .unwrap_or(PeerEvent::ConnectionLost)
                }
                Err(InboxError::Overflow) => return PeerEvent::Overcap,
                Err(InboxError::Closed) => return PeerEvent::Eof,
                Err(InboxError::Timeout) if ended => return PeerEvent::Eof,
                Err(InboxError::Timeout) => {}
            }
        }
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.api_key
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        self.input.close();
        let exec = self.exec.clone();
        let private = self.private.clone();
        self.runtime.spawn_blocking(move || {
            // Bounded retries. The unbounded loop existed for transient
            // cleanup failures (Exited persistence unavailable), but a
            // permanently unkillable group member parked this thread in
            // ~60 s stop cycles forever — which kept the whole workload from
            // reaching a terminal state. After the cap the stop is stranded:
            // logged here, ownership stays with the exec layer's
            // stranded-group reconciliation, and this thread gets out of the
            // way. Never launch a replacement either way.
            const STOP_ATTEMPTS: u32 = 3;
            for attempt in 1..=STOP_ATTEMPTS {
                if exec
                    .stop_blocking(Duration::from_secs(10), Duration::from_secs(5))
                    .is_ok()
                {
                    break;
                }
                if attempt == STOP_ATTEMPTS {
                    tracing::warn!(
                        attempts = attempt,
                        "codex peer cleanup did not confirm; leaving the group to stranded reconciliation"
                    );
                } else {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            // The peer remains in the adapter's history after cleanup. Drop
            // the directory now, even while retained stream taps still exist.
            if let Some(private) = private {
                private.lock().unwrap_or_else(|p| p.into_inner()).take();
            }
        });
    }

    fn cleanup_confirmed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
            && matches!(self.exec.inspect(), ExecProbe::Finished { .. })
    }
}

impl Drop for SupervisedPeer {
    fn drop(&mut self) {
        self.close();
    }
}
