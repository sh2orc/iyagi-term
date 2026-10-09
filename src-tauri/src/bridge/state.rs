//! Bridge session state: the control/data connection pair, the revision
//! cache, and the channel fan-out toward the frontend.
//!
//! Lifecycle (`02-runner.md` §1): resolve data dir → spawn daemon if its
//! runtime files are missing → poll endpoint/token (100 ms, ≤ 10 s) →
//! connect + hello (control) → `bridge_open_data` consumes the single-use
//! `data_token` (TTL 5 s, `01-contracts.md` §3).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::ipc::Channel;
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use term_contracts::error::ErrorCode;
use term_contracts::ids::SessionId;
use term_contracts::rpc::{Frame, HelloResult, HelloRole, RpcEventKind};

use super::connection::{BridgeError, Connection, OsTransport, Transport};
use super::daemon_manager::{self, DaemonSpawner, OsDaemonSpawner, READY_POLL, READY_TIMEOUT};

/// Frontend channel for control-connection events (`{event, payload}`).
pub type EventChannel = Channel<Value>;
type EventChannels = std::sync::Mutex<Vec<EventChannel>>;
/// session_id → view_id → output channel (data connection routing).
///
/// Keyed by view on purpose: a re-attach of the same view REPLACES its
/// channel instead of appending. Dropping the superseded Rust `Channel`
/// is what makes Tauri eval `{end:true}` in the webview, which unregisters
/// the matching JS callback — without that, every re-attach pinned one more
/// channel on both heaps and every output record was serialized once per
/// stale channel (the "grows forever while running" leak).
type SessionChannels = std::sync::Mutex<HashMap<String, HashMap<String, EventChannel>>>;

/// Grace before an exited session's channels are dropped bridge-side. The
/// data socket may still carry the tail of its output when the control
/// socket delivers `session.exited` (two sockets, no cross ordering); the
/// daemon drains output for at most `exit_drain` (2 s), so 10 s is ample.
/// The frontend normally unsubscribes explicitly when the pane goes away —
/// this is the backstop for panes left open on an exited session.
const EXITED_SESSION_GRACE: Duration = Duration::from_secs(10);

/// Daemon data root as resolved at `connect`, shared between `BridgeInner`
/// (writer) and [`BridgeState::effective_data_dir`] (reader). Lives outside
/// the async mutex like `revision`: a Settings read of the Z.ai key must not
/// queue behind a connect that is still waiting for the daemon to come up.
pub type ResolvedDataDir = Arc<std::sync::Mutex<Option<PathBuf>>>;

/// Managed Tauri state. `revision` sits outside the async mutex so
/// `bridge_revision` never contends with connection work.
pub struct BridgeState {
    pub revision: Arc<AtomicU64>,
    /// `None` until the first `connect` resolved a dir
    /// (see [`BridgeState::effective_data_dir`]).
    data_dir: ResolvedDataDir,
    pub inner: AsyncMutex<BridgeInner>,
}

impl BridgeState {
    pub fn new() -> Self {
        Self::with_io(
            Arc::new(OsTransport),
            Arc::new(OsDaemonSpawner),
            Arc::new(daemon_manager::locate_daemon_binary),
        )
    }

    pub fn with_io(
        transport: Arc<dyn Transport>,
        spawner: Arc<dyn DaemonSpawner>,
        locate: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>,
    ) -> Self {
        let revision = Arc::new(AtomicU64::new(0));
        let inner = BridgeInner::new(
            transport,
            spawner,
            locate,
            Arc::new(EventChannels::default()),
            Arc::new(SessionChannels::default()),
            Arc::clone(&revision),
        );
        Self {
            revision,
            data_dir: inner.resolved_data_dir(),
            inner: AsyncMutex::new(inner),
        }
    }

    /// The daemon's data root: the dir the bridge resolved at `connect` (the
    /// `--data-dir` the daemon was spawned with, or the override the
    /// frontend passed), falling back to the platform default before any
    /// connect. `<data_dir>/secrets` (the Z.ai key, which the daemon reads
    /// back for `claude_provider` routing) and `<data_dir>/data` (daemon-
    /// written caches) both hang off it, so commands touching either resolve
    /// through here instead of calling `default_data_dir` themselves.
    /// Never waits on `inner`.
    pub fn effective_data_dir(&self) -> Result<PathBuf, BridgeError> {
        let resolved = self
            .data_dir
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        match resolved {
            Some(dir) => Ok(dir),
            None => daemon_manager::default_data_dir(),
        }
    }
}

impl Default for BridgeState {
    fn default() -> Self {
        Self::new()
    }
}

/// Single-use data token minted by the control hello (`01-contracts.md` §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataToken {
    Minted(String),
    Consumed,
}

impl DataToken {
    /// Consume the token exactly once; `None` afterwards forever.
    pub fn take(&mut self) -> Option<String> {
        match std::mem::replace(self, DataToken::Consumed) {
            DataToken::Minted(token) => Some(token),
            DataToken::Consumed => None,
        }
    }

    fn mint(token: String) -> Self {
        DataToken::Minted(token)
    }
}

pub struct BridgeInner {
    transport: Arc<dyn Transport>,
    spawner: Arc<dyn DaemonSpawner>,
    locate: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>,
    ready_timeout: Duration,
    control: Option<Arc<Connection>>,
    data: Option<Arc<Connection>>,
    token: DataToken,
    hello: Option<HelloResult>,
    data_dir: PathBuf,
    /// Mirror of `data_dir` published for [`BridgeState::effective_data_dir`].
    resolved_data_dir: ResolvedDataDir,
    event_channels: Arc<EventChannels>,
    session_channels: Arc<SessionChannels>,
    revision: Arc<AtomicU64>,
    /// Build/version handshake: the connected daemon's build id differs from
    /// this app's (or is empty — an OLD daemon that predates the field).
    /// Recorded at connect and surfaced to the UI via `connection_status`;
    /// it never blocks the connection.
    daemon_outdated: bool,
    /// daemon_id of the current control connection. A change means a new
    /// daemon took over, which resets the snapshot revision baseline the UI
    /// tracks (OOM postmortem 2026-09-15 defect #2: the new daemon restarts
    /// revisions at 1, and the bridge's `fetch_max` cache would otherwise
    /// keep the retired daemon's higher baseline).
    last_daemon_id: Option<String>,
}

impl BridgeInner {
    fn new(
        transport: Arc<dyn Transport>,
        spawner: Arc<dyn DaemonSpawner>,
        locate: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>,
        event_channels: Arc<EventChannels>,
        session_channels: Arc<SessionChannels>,
        revision: Arc<AtomicU64>,
    ) -> Self {
        Self {
            transport,
            spawner,
            locate,
            ready_timeout: READY_TIMEOUT,
            control: None,
            data: None,
            token: DataToken::Consumed,
            hello: None,
            data_dir: PathBuf::new(),
            resolved_data_dir: Arc::new(std::sync::Mutex::new(None)),
            event_channels,
            session_channels,
            revision,
            daemon_outdated: false,
            last_daemon_id: None,
        }
    }

    /// Test-only: shrink the spawn/ready budget.
    #[cfg(test)]
    fn with_ready_timeout(mut self, timeout: Duration) -> Self {
        self.ready_timeout = timeout;
        self
    }

    /// Handle to the resolved-data-dir slot `BridgeState` reads without the
    /// async mutex.
    fn resolved_data_dir(&self) -> ResolvedDataDir {
        Arc::clone(&self.resolved_data_dir)
    }

    pub fn control(&self) -> Option<Arc<Connection>> {
        self.control.clone()
    }

    pub fn data(&self) -> Option<Arc<Connection>> {
        self.data.clone()
    }

    pub fn connection_status(&self) -> (bool, bool) {
        let control_alive = self
            .control
            .as_ref()
            .is_some_and(|connection| !connection.is_closed());
        let data_alive = self
            .data
            .as_ref()
            .is_some_and(|connection| !connection.is_closed());
        (control_alive, data_alive)
    }

    /// Whether the connected daemon's build id differs from this app's
    /// (`true` also for an OLD daemon that never reported one). Set at connect,
    /// read by `bridge_connection_status` for the frontend outdated banner.
    pub fn daemon_outdated(&self) -> bool {
        self.daemon_outdated
    }

    /// Establish the control connection. Returns the cached hello while the
    /// control connection stays alive. Never logs the token.
    pub async fn connect(
        &mut self,
        data_dir_override: Option<&str>,
    ) -> Result<HelloResult, BridgeError> {
        if let Some(conn) = &self.control {
            if !conn.is_closed() {
                if let Some(hello) = &self.hello {
                    return Ok(hello.clone());
                }
            } else {
                self.shutdown();
            }
        }

        let data_dir = daemon_manager::resolve_data_dir(data_dir_override)?;
        self.data_dir = data_dir.clone();
        // Published before the spawn/ready wait: even if the daemon never
        // comes up, the key store must follow the dir it was asked to use.
        *self
            .resolved_data_dir
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(data_dir.clone());
        let deadline = Instant::now() + self.ready_timeout;
        let mut spawn_attempted = false;
        let mut connect_failures = 0usize;
        let mut respawn_budget = 1usize;

        loop {
            let files_present = daemon_manager::endpoint_file(&data_dir).is_file()
                && daemon_manager::token_file(&data_dir).is_file();
            if !files_present {
                if !spawn_attempted {
                    spawn_attempted = true;
                    daemon_manager::spawn_daemon_with(
                        &*self.locate,
                        self.spawner.as_ref(),
                        &data_dir,
                    )?;
                    if !daemon_manager::wait_ready(&data_dir, deadline).await {
                        return Err(BridgeError::daemon_unavailable(
                            "daemon did not become ready within the timeout",
                        ));
                    }
                } else {
                    if Instant::now() >= deadline {
                        return Err(BridgeError::daemon_unavailable(
                            "daemon runtime files did not appear within the timeout",
                        ));
                    }
                    tokio::time::sleep(READY_POLL).await;
                    continue;
                }
            }

            let endpoint =
                daemon_manager::read_runtime_file(&daemon_manager::endpoint_file(&data_dir));
            let token = daemon_manager::read_runtime_file(&daemon_manager::token_file(&data_dir));
            if let (Some(endpoint), Some(token)) = (endpoint, token) {
                match self.transport.connect(&endpoint).await {
                    Ok(stream) => {
                        match Connection::open(stream, &token, HelloRole::Control).await {
                            Ok((conn, hello, event_rx)) => {
                                let hello = hello.expect("control handshake returns HelloResult");
                                // OOM postmortem defect #2: a new daemon
                                // (new daemon_id) restarts revisions at 1, so
                                // reset the bridge's revision baseline on
                                // takeover — otherwise `fetch_max` keeps the
                                // retired daemon's higher value and discards
                                // the new authoritative snapshots.
                                if self.last_daemon_id.as_deref() != Some(hello.daemon_id.as_str())
                                {
                                    self.revision.store(0, Ordering::SeqCst);
                                    self.last_daemon_id = Some(hello.daemon_id.clone());
                                }
                                // Build/version handshake (never fatal): record
                                // whether this daemon predates the app build so
                                // the UI can offer a restart.
                                // 두 기준과 비교한다: 이 앱의 빌드 id, 그리고 디스크의
                                // (다음에 띄울) 데몬 바이너리가 보고하는 빌드 id. 후자는
                                // 데몬만 다시 빌드하고 옛 프로세스가 계속 도는 개발 함정을
                                // 잡는다 — 앱 id와 같아도 바이너리가 더 새로우면 오래됐다.
                                let app_outdated = super::connection::daemon_is_outdated(
                                    &super::connection::app_build_version(),
                                    &hello.daemon_version,
                                );
                                let binary_outdated = match (self.locate)() {
                                    Some(binary) => {
                                        daemon_manager::binary_build_version(&binary)
                                            .await
                                            .is_some_and(|on_disk| {
                                                super::connection::daemon_is_outdated(
                                                    &on_disk,
                                                    &hello.daemon_version,
                                                )
                                            })
                                    }
                                    None => false,
                                };
                                self.daemon_outdated = app_outdated || binary_outdated;
                                spawn_control_forwarder(
                                    event_rx,
                                    Arc::clone(&self.event_channels),
                                    Arc::clone(&self.session_channels),
                                    Arc::clone(&self.revision),
                                    EXITED_SESSION_GRACE,
                                );
                                self.control = Some(Arc::new(conn));
                                self.token = DataToken::mint(hello.data_token.clone());
                                self.hello = Some(hello.clone());
                                return Ok(hello);
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, "bridge: control connect attempt failed");
                                connect_failures += 1;
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "bridge: transport connect attempt failed");
                        connect_failures += 1;
                    }
                }
                // Stale-endpoint recovery: after repeated failures with files
                // present, respawn once (singleton-safe, 02 §1-2).
                if connect_failures >= 5 && respawn_budget > 0 {
                    respawn_budget -= 1;
                    let _ = daemon_manager::spawn_daemon_with(
                        &*self.locate,
                        self.spawner.as_ref(),
                        &data_dir,
                    );
                }
            }

            if Instant::now() >= deadline {
                return Err(BridgeError::daemon_unavailable(
                    "daemon endpoint was not reachable within the timeout",
                ));
            }
            tokio::time::sleep(READY_POLL).await;
        }
    }

    /// Open the data connection by consuming the single-use data token.
    /// Idempotent while alive; the token can never be reused afterwards.
    /// The TTL is 5 s — callers must open the data connection promptly
    /// after `bridge_connect` returns (`01-contracts.md` §3).
    pub async fn open_data(&mut self) -> Result<(), BridgeError> {
        if let Some(conn) = &self.data {
            if !conn.is_closed() {
                return Ok(());
            }
            // `is_closed` is set by EITHER loop. If only the writer died the
            // reader (and with it the data forwarder holding this map) would
            // outlive the handle and duplicate every record once a new data
            // connection opens. Abort both before letting the handle go.
            conn.close();
            self.data = None;
        }
        let control_alive = self
            .control
            .as_ref()
            .map(|c| !c.is_closed())
            .unwrap_or(false);
        if !control_alive {
            return Err(BridgeError::Rpc(term_contracts::error::RpcError::new(
                ErrorCode::InvalidState,
                "control connection required before opening the data connection",
            )));
        }
        let token_consumed = || {
            BridgeError::Rpc(term_contracts::error::RpcError::new(
                ErrorCode::InvalidState,
                "data token already consumed — reconnect via bridge_disconnect + bridge_connect",
            ))
        };
        if !matches!(self.token, DataToken::Minted(_)) {
            return Err(token_consumed());
        }
        let endpoint =
            daemon_manager::read_runtime_file(&daemon_manager::endpoint_file(&self.data_dir))
                .ok_or_else(|| BridgeError::daemon_unavailable("daemon endpoint file missing"))?;
        let stream = self.transport.connect(&endpoint).await?;
        // The daemon burns the token on the data hello, so consume it only
        // once the transport is up: a transient connect failure must leave
        // the minted token usable by the next bridge_open_data.
        let token = self.token.take().ok_or_else(token_consumed)?;
        let (conn, _hello, event_rx) = Connection::open(stream, &token, HelloRole::Data).await?;
        spawn_data_forwarder(
            event_rx,
            Arc::clone(&self.session_channels),
            Arc::clone(&self.event_channels),
        );
        self.data = Some(Arc::new(conn));
        Ok(())
    }

    /// Route `session.output` frames for `session_id` to `channel` on behalf
    /// of `view_id`. Re-subscribing the same view replaces (and drops) the
    /// previous channel — see `SessionChannels`.
    pub fn subscribe_session(
        &self,
        session_id: &str,
        view_id: &str,
        channel: EventChannel,
    ) -> Result<(), BridgeError> {
        let id = SessionId::parse(session_id)
            .map_err(|_| BridgeError::invalid_argument("session_id must be a UUID v4 string"))?;
        let view = SessionId::parse(view_id)
            .map_err(|_| BridgeError::invalid_argument("view_id must be a UUID v4 string"))?;
        let superseded = self
            .session_channels
            .lock()
            .expect("session channel lock")
            .entry(String::from(id))
            .or_default()
            .insert(String::from(view), channel);
        // Drop outside the lock: the Channel's on_drop evals into the webview.
        drop(superseded);
        Ok(())
    }

    /// Forget one view's channel (pane closed / retried / disposed). Unknown
    /// ids are a no-op — the frontend may unsubscribe after a reconnect
    /// already cleared the map.
    pub fn unsubscribe_session(&self, session_id: &str, view_id: &str) {
        let removed = {
            let mut guard = self.session_channels.lock().expect("session channel lock");
            let removed = guard
                .get_mut(session_id)
                .and_then(|views| views.remove(view_id));
            if guard.get(session_id).is_some_and(HashMap::is_empty) {
                guard.remove(session_id);
            }
            removed
        };
        drop(removed);
    }

    /// Number of registered output channels across all sessions (시험용).
    #[cfg(test)]
    pub fn session_channel_count(&self) -> usize {
        self.session_channels
            .lock()
            .expect("session channel lock")
            .values()
            .map(HashMap::len)
            .sum()
    }

    pub fn push_event_channel(&self, channel: EventChannel) {
        self.event_channels
            .lock()
            .expect("event channel lock")
            .push(channel);
    }

    /// Close both connections and drop frontend channels. A reconnect always
    /// re-subscribes before `session.attach`; retaining old WebView channels
    /// would duplicate every replay record after HMR. The daemon itself stays
    /// alive (02 §1-5).
    pub fn shutdown(&mut self) {
        if let Some(conn) = &self.control {
            conn.close();
        }
        if let Some(conn) = &self.data {
            conn.close();
        }
        self.event_channels
            .lock()
            .expect("event channel lock")
            .clear();
        self.session_channels
            .lock()
            .expect("session channel lock")
            .clear();
        self.control = None;
        self.data = None;
        self.token = DataToken::Consumed;
        self.hello = None;
        // Cleared so a disconnected bridge never reports "outdated"; it is
        // recomputed on the next control hello. `last_daemon_id` is kept on
        // purpose so a reconnect to the SAME daemon does not look like a
        // takeover and needlessly reset the revision baseline.
        self.daemon_outdated = false;
    }
}

/// Control-connection forwarder: events → all frontend event channels,
/// revision-bearing payloads → revision cache, `session.exited` → the
/// session's output channels are released after a grace period (backstop
/// for panes the frontend never explicitly unsubscribes).
fn spawn_control_forwarder(
    mut events: mpsc::UnboundedReceiver<Frame>,
    channels: Arc<EventChannels>,
    session_channels: Arc<SessionChannels>,
    revision: Arc<AtomicU64>,
    exited_grace: Duration,
) {
    tokio::spawn(async move {
        while let Some(frame) = events.recv().await {
            let Frame::Event(event) = frame else { continue };
            let Ok(kind) = serde_json::to_value(event.event) else {
                continue;
            };
            if let Some(rev) = event.payload.get("revision").and_then(|v| v.as_u64()) {
                revision.fetch_max(rev, Ordering::SeqCst);
            }
            if event.event == RpcEventKind::SessionExited {
                if let Some(session_id) = event.payload.get("session_id").and_then(|v| v.as_str()) {
                    schedule_session_release(
                        Arc::clone(&session_channels),
                        session_id.to_string(),
                        exited_grace,
                    );
                }
            }
            dispatch_to_channels(
                &channels,
                json!({ "event": kind, "payload": event.payload }),
            );
        }
    });
}

/// Drop every channel of `session_id` once `grace` has elapsed. A view that
/// re-subscribes in the meantime (retry → new session id) is unaffected
/// because the new attach targets a different session id.
fn schedule_session_release(sessions: Arc<SessionChannels>, session_id: String, grace: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(grace).await;
        let removed = sessions
            .lock()
            .expect("session channel lock")
            .remove(&session_id);
        drop(removed);
    });
}

/// Data-connection forwarder: `session.output` → the per-session channel
/// registered by `bridge_subscribe_session`; anything else (not expected on
/// the data connection, 01 §3) is forwarded to the global event channels.
fn spawn_data_forwarder(
    mut events: mpsc::UnboundedReceiver<Frame>,
    session_channels: Arc<SessionChannels>,
    event_channels: Arc<EventChannels>,
) {
    tokio::spawn(async move {
        const MAX_RECORDS: usize = 128;
        const MAX_BYTES: usize = 64 * 1024;
        let mut frames = Vec::with_capacity(MAX_RECORDS);
        while events.recv_many(&mut frames, MAX_RECORDS).await > 0 {
            // A history redraw is hundreds of small PTY records. One WKWebView
            // eval per record serializes their delivery over many UI turns.
            // Let the socket reader finish its ready burst, without a timer
            // that would delay an isolated keystroke or resize.
            tokio::task::yield_now().await;
            while frames.len() < MAX_RECORDS {
                let Ok(frame) = events.try_recv() else { break };
                frames.push(frame);
            }
            let mut batch: HashMap<String, Vec<Value>> = HashMap::new();
            let mut bytes = 0;
            for frame in frames.drain(..) {
                let Frame::Event(event) = frame else { continue };
                if event.event == RpcEventKind::SessionOutput {
                    let Some(session_id) = event.payload.get("session_id").and_then(|v| v.as_str())
                    else {
                        continue;
                    };
                    let session_id = session_id.to_owned();
                    let raw_len = event
                        .payload
                        .get("raw_len")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize;
                    if bytes + raw_len > MAX_BYTES {
                        flush_session_batch(&session_channels, &mut batch);
                        bytes = 0;
                    }
                    bytes += raw_len;
                    batch.entry(session_id).or_default().push(event.payload);
                } else {
                    // Keep non-output notifications behind preceding records.
                    flush_session_batch(&session_channels, &mut batch);
                    bytes = 0;
                    let Ok(kind) = serde_json::to_value(event.event) else {
                        continue;
                    };
                    dispatch_to_channels(
                        &event_channels,
                        json!({ "event": kind, "payload": event.payload }),
                    );
                }
            }
            flush_session_batch(&session_channels, &mut batch);
        }
    });
}

fn flush_session_batch(sessions: &Arc<SessionChannels>, batch: &mut HashMap<String, Vec<Value>>) {
    for (session_id, mut records) in batch.drain() {
        let payload = if records.len() == 1 {
            json!({ "event": "session.output", "payload": records.pop().unwrap() })
        } else {
            json!({ "event": "session.output.batch", "payload": records })
        };
        route_session_output(sessions, &session_id, payload);
    }
}

fn dispatch_to_channels(channels: &Arc<EventChannels>, payload: Value) {
    let mut guard = channels.lock().expect("event channel lock");
    guard.retain(|channel| channel.send(payload.clone()).is_ok());
}

fn route_session_output(sessions: &Arc<SessionChannels>, session_id: &str, payload: Value) {
    let mut guard = sessions.lock().expect("session channel lock");
    if let Some(views) = guard.get_mut(session_id) {
        views.retain(|_, channel| channel.send(payload.clone()).is_ok());
        if views.is_empty() {
            guard.remove(session_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    use tokio::io::DuplexStream;

    use super::super::connection::BoxedStream;
    use crate::bridge::daemon_manager::{endpoint_file, token_file};

    const DATA_TOKEN: &str = "integration-data-token";

    #[derive(Default)]
    struct FakeTransport {
        endpoints: std::sync::Mutex<VecDeque<BoxedStream>>,
    }

    impl FakeTransport {
        fn push(&self, stream: DuplexStream) {
            self.endpoints.lock().unwrap().push_back(Box::new(stream));
        }
    }

    impl Transport for FakeTransport {
        fn connect<'a>(
            &'a self,
            _endpoint: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<BoxedStream, BridgeError>> + Send + 'a>,
        > {
            Box::pin(async move {
                self.endpoints
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or_else(|| BridgeError::daemon_unavailable("no fake endpoint queued"))
            })
        }
    }

    /// Spawner standing in for the real daemon: writes runtime files and
    /// queues one control + one data connection handled by an in-process
    /// fake daemon speaking the term-contracts wire protocol.
    struct FakeDaemonSpawner {
        transport: Arc<FakeTransport>,
        acks: Arc<AsyncMutex<Vec<Value>>>,
    }

    impl DaemonSpawner for FakeDaemonSpawner {
        fn spawn_detached(
            &self,
            _binary: &std::path::Path,
            data_dir: &std::path::Path,
        ) -> std::io::Result<()> {
            std::fs::create_dir_all(data_dir.join("runtime"))?;
            std::fs::write(endpoint_file(data_dir), "fake://daemon").unwrap();
            std::fs::write(token_file(data_dir), "runtime-auth-token").unwrap();

            for role in ["control", "data"] {
                let (client_end, mut daemon_end) = tokio::io::duplex(16 * 1024);
                self.transport.push(client_end);
                let acks = Arc::clone(&self.acks);
                let is_control = role == "control";
                tokio::spawn(async move {
                    // hello
                    let request = read_json_frame(&mut daemon_end).await.unwrap();
                    assert_eq!(request["method"], "hello");
                    let token_ok = if is_control {
                        request["params"]["token"] == json!("runtime-auth-token")
                            && request["params"]["role"] == json!("control")
                    } else {
                        request["params"]["token"] == json!(DATA_TOKEN)
                            && request["params"]["role"] == json!("data")
                    };
                    if token_ok {
                        write_json_frame(
                            &mut daemon_end,
                            json!({"v": 1, "id": request["id"], "result": if is_control { hello_result() } else { json!({"connection_id": uuid::Uuid::new_v4().to_string(), "role": "data"}) }}),
                        )
                        .await
                        .unwrap();
                    } else {
                        write_json_frame(
                            &mut daemon_end,
                            json!({"v": 1, "id": request["id"], "error": {
                                "code": "INVALID_ARGUMENT", "message": "auth failed", "retryable": false
                            }}),
                        )
                        .await
                        .unwrap();
                        return;
                    }
                    // request loop
                    while let Some(request) = read_json_frame(&mut daemon_end).await {
                        match request["method"].as_str().unwrap_or_default() {
                            "session.ack" => {
                                acks.lock().await.push(request["params"].clone());
                                // 01 §4: no response for ack.
                            }
                            "workload.launch" => {
                                // Push a revision-bearing event, then answer.
                                write_json_frame(
                                    &mut daemon_end,
                                    json!({"v": 1, "event": "queue.changed",
                                           "payload": {"queue": [], "revision": 7}}),
                                )
                                .await
                                .unwrap();
                                write_json_frame(
                                    &mut daemon_end,
                                    json!({"v": 1, "id": request["id"], "result": {
                                        "workload_id": "w-1", "session_id": null,
                                        "state": "QUEUED"}}),
                                )
                                .await
                                .unwrap();
                            }
                            other => {
                                write_json_frame(
                                    &mut daemon_end,
                                    json!({"v": 1, "id": request["id"], "result": {"method": other}}),
                                )
                                .await
                                .unwrap();
                            }
                        }
                    }
                });
            }
            Ok(())
        }
    }

    async fn read_json_frame(stream: &mut DuplexStream) -> Option<Value> {
        use tokio::io::AsyncReadExt;
        let mut header = [0u8; 4];
        stream.read_exact(&mut header).await.ok()?;
        let mut body = vec![0u8; u32::from_le_bytes(header) as usize];
        stream.read_exact(&mut body).await.ok()?;
        serde_json::from_slice(&body).ok()
    }

    async fn write_json_frame(stream: &mut DuplexStream, value: Value) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        let bytes = term_contracts::rpc::encode_frame(&value).unwrap();
        stream.write_all(&bytes).await
    }

    fn hello_result() -> Value {
        json!({
            "daemon_id": "fake-daemon",
            "protocol": 1,
            "connection_id": "00000000-0000-4000-8000-000000000001",
            "data_token": DATA_TOKEN,
            "capabilities": {
                "memory_limit_kind": {"support": "unsupported", "reason": "fake"},
                "cpu_quota": {"support": "supported"},
                "process_count_limit": {"support": "supported"},
                "tree_accounting": {"support": "supported"},
                "reattach": {"support": "supported"},
                "resume": {"support": "unsupported", "reason": "fake"},
                "platform": "fake"
            }
        })
    }

    fn bridge_with_fake(
        transport: Arc<FakeTransport>,
        spawner: Arc<FakeDaemonSpawner>,
    ) -> BridgeState {
        BridgeState::with_io(
            transport,
            spawner,
            Arc::new(|| Some(PathBuf::from("fake-iyagi-termd"))),
        )
    }

    #[test]
    fn data_token_is_single_use() {
        let mut token = DataToken::mint("secret".into());
        assert_eq!(token.take().as_deref(), Some("secret"));
        assert!(token.take().is_none());
        assert_eq!(token, DataToken::Consumed);
        // Re-mint only happens with a fresh control hello.
        token = DataToken::mint("next".into());
        assert_eq!(token.take().as_deref(), Some("next"));
    }

    /// Build iyagi-termd first and set IYAGI_TERMD_TEST_BINARY to its absolute path.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires a built daemon binary and native PTY support"]
    async fn native_daemon_launches_shell_and_delivers_output() {
        use std::process::{Child, Command, Stdio};
        use tauri::ipc::InvokeResponseBody;

        struct DaemonProcess(Child);
        impl Drop for DaemonProcess {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let binary = std::env::var_os("IYAGI_TERMD_TEST_BINARY")
            .expect("set IYAGI_TERMD_TEST_BINARY to the built daemon");
        // Keep the Unix socket path below macOS's length limit.
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let _daemon = DaemonProcess(
            Command::new(binary)
                .arg("--data-dir")
                .arg(dir.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        assert!(daemon_manager::wait_ready(dir.path(), Instant::now() + READY_TIMEOUT).await);

        let state = BridgeState::new();
        state
            .inner
            .lock()
            .await
            .connect(dir.path().to_str())
            .await
            .unwrap();
        state.inner.lock().await.open_data().await.unwrap();
        let conn = state.inner.lock().await.control().unwrap();
        let launched = conn
            .call(
                "launch-smoke",
                "workload.launch",
                json!({
                    "request_id": uuid::Uuid::new_v4().to_string(),
                    "profile_id": "shell", "mode": "shell", "executor": {"kind": "local"},
                    "cwd": dir.path(), "program": "/bin/sh",
                    "argv": ["-c", "printf iyagi-smoke-ok"], "env_overrides": {},
                    "cols": 80, "rows": 24, "priority": 1,
                    "policy": {
                        "reservation_bytes": "2147483648", "cpu_slots": 1,
                        "enforcement": "observe", "memory_max_bytes": null,
                        "cpu_max_cores": null, "pids_max": null
                    }
                }),
            )
            .await
            .unwrap();
        let session_id = launched["session_id"].as_str().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let channel = Channel::new(move |body| {
            if let InvokeResponseBody::Json(text) = body {
                let _ = tx.send(serde_json::from_str(&text).unwrap());
            }
            Ok(())
        });
        let view_id = uuid::Uuid::new_v4().to_string();
        state
            .inner
            .lock()
            .await
            .subscribe_session(session_id, &view_id, channel)
            .unwrap();
        let attached = conn
            .call(
                "attach-smoke",
                "session.attach",
                json!({
                    "session_id": session_id, "view_id": view_id,
                    "access": "writer"
                }),
            )
            .await
            .unwrap();
        assert!(attached["epoch"].is_string());
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = rx.recv().await {
                if event["payload"]["data_b64"] == "bW9haS1zbW9rZS1vaw==" {
                    return;
                }
            }
            panic!("output channel closed before shell output arrived");
        })
        .await
        .expect("shell output must reach the frontend channel");
        state.inner.lock().await.shutdown();
    }

    #[tokio::test]
    async fn connect_spawns_and_hellos_then_open_data_routes_acks() {
        let transport = Arc::new(FakeTransport::default());
        let spawner = Arc::new(FakeDaemonSpawner {
            transport: Arc::clone(&transport),
            acks: Arc::new(AsyncMutex::new(Vec::new())),
        });
        let acks = Arc::clone(&spawner.acks);
        let state = bridge_with_fake(Arc::clone(&transport), Arc::clone(&spawner));

        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_string_lossy().to_string();

        // 1. connect spawns the fake daemon (files missing) and performs hello.
        let hello = state
            .inner
            .lock()
            .await
            .connect(Some(&data_dir))
            .await
            .unwrap();
        assert_eq!(hello.protocol, 1);
        assert_eq!(hello.data_token, DATA_TOKEN);

        // 2. second connect with a live control connection returns the cached
        //    hello without consuming another transport endpoint.
        let cached = state
            .inner
            .lock()
            .await
            .connect(Some(&data_dir))
            .await
            .unwrap();
        assert_eq!(cached.data_token, DATA_TOKEN);

        // 3. open_data consumes the token, helloing with role=data.
        state.inner.lock().await.open_data().await.unwrap();

        // 4. RPC over control; the fake daemon pushes a revision event first.
        let conn = state.inner.lock().await.control().unwrap();
        let result = conn
            .call("req-launch", "workload.launch", json!({"mode": "managed"}))
            .await
            .unwrap();
        assert_eq!(result["workload_id"], "w-1");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(state.revision.load(Ordering::SeqCst), 7);

        // 5. ACK over the data connection reaches the daemon.
        let session_id = uuid::Uuid::new_v4().to_string();
        let data_conn = state.inner.lock().await.data().unwrap();
        data_conn
            .send_only(term_contracts::rpc::RpcRequest::new(
                uuid::Uuid::new_v4().to_string(),
                "session.ack",
                json!({"session_id": session_id, "epoch": "e-1", "through_seq": "3"}),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let acks = acks.lock().await;
        assert_eq!(acks.len(), 1);
        assert_eq!(acks[0]["through_seq"], "3");
    }

    #[tokio::test]
    async fn effective_data_dir_follows_connect_and_survives_disconnect() {
        let transport = Arc::new(FakeTransport::default());
        let spawner = Arc::new(FakeDaemonSpawner {
            transport: Arc::clone(&transport),
            acks: Arc::new(AsyncMutex::new(Vec::new())),
        });
        let state = bridge_with_fake(Arc::clone(&transport), Arc::clone(&spawner));
        // Before any connect: the platform default, exactly what a daemon
        // started without `--data-dir` would use.
        assert_eq!(
            state.effective_data_dir().ok(),
            daemon_manager::default_data_dir().ok()
        );

        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_string_lossy().to_string();
        state
            .inner
            .lock()
            .await
            .connect(Some(&data_dir))
            .await
            .unwrap();
        assert_eq!(state.effective_data_dir().unwrap(), dir.path());

        // A disconnect keeps the resolved dir: the Z.ai key store must not
        // hop back to the default while the daemon (and its
        // `claude_provider` key lookup) still lives under the override.
        state.inner.lock().await.shutdown();
        assert_eq!(state.effective_data_dir().unwrap(), dir.path());
    }

    #[tokio::test]
    async fn data_token_cannot_open_twice_after_consumption() {
        let transport = Arc::new(FakeTransport::default());
        let spawner = Arc::new(FakeDaemonSpawner {
            transport: Arc::clone(&transport),
            acks: Arc::new(AsyncMutex::new(Vec::new())),
        });
        let state = bridge_with_fake(Arc::clone(&transport), Arc::clone(&spawner));
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_string_lossy().to_string();

        let mut inner = state.inner.lock().await;
        inner.connect(Some(&data_dir)).await.unwrap();
        inner.open_data().await.unwrap();
        assert_eq!(inner.connection_status(), (true, true));
        let first_data = inner.data().unwrap();

        // Kill the data connection, then try to open again: token consumed.
        first_data.close();
        assert_eq!(inner.connection_status(), (true, false));
        let err = inner.open_data().await.unwrap_err().into_rpc();
        assert_eq!(err.code, ErrorCode::InvalidState);
        assert!(inner.data().is_none());
    }

    #[tokio::test]
    async fn subscribe_session_validates_uuid_shape() {
        let transport = Arc::new(FakeTransport::default());
        let spawner = Arc::new(FakeDaemonSpawner {
            transport: Arc::clone(&transport),
            acks: Arc::new(AsyncMutex::new(Vec::new())),
        });
        let state = bridge_with_fake(transport, spawner);
        let inner = state.inner.lock().await;
        let view_id = uuid::Uuid::new_v4().to_string();

        let err = inner
            .subscribe_session("not-a-uuid", &view_id, capture_channel())
            .unwrap_err();
        assert_eq!(err.into_rpc().code, ErrorCode::InvalidArgument);

        let ok_id = uuid::Uuid::new_v4().to_string();
        assert!(inner
            .subscribe_session(&ok_id, &view_id, capture_channel())
            .is_ok());
        // v1-shaped UUID rejected too.
        let err = inner.subscribe_session(
            "e2f5c8e0-6b1a-11d0-a08c-0020af31e880",
            &view_id,
            capture_channel(),
        );
        assert!(err.is_err());
        // The view id is validated with the same shape rule.
        let err = inner.subscribe_session(&ok_id, "view-1", capture_channel());
        assert_eq!(err.unwrap_err().into_rpc().code, ErrorCode::InvalidArgument);
    }

    /// The leak this guards against: every attach used to append a channel
    /// and nothing ever removed it, so a long-running app pinned one Rust
    /// channel + one webview callback per attach forever.
    #[tokio::test]
    async fn resubscribe_replaces_and_unsubscribe_releases_channels() {
        let transport = Arc::new(FakeTransport::default());
        let spawner = Arc::new(FakeDaemonSpawner {
            transport: Arc::clone(&transport),
            acks: Arc::new(AsyncMutex::new(Vec::new())),
        });
        let state = bridge_with_fake(transport, spawner);
        let inner = state.inner.lock().await;
        let session = uuid::Uuid::new_v4().to_string();
        let view_a = uuid::Uuid::new_v4().to_string();
        let view_b = uuid::Uuid::new_v4().to_string();
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counting = |dropped: &Arc<std::sync::atomic::AtomicUsize>| -> EventChannel {
            struct DropCounter(Arc<std::sync::atomic::AtomicUsize>);
            impl Drop for DropCounter {
                fn drop(&mut self) {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
            let guard = DropCounter(Arc::clone(dropped));
            Channel::new(move |_body| {
                let _keep = &guard;
                Ok(())
            })
        };

        // Re-attaching the same view three times keeps ONE channel and drops
        // the two superseded ones (Tauri's on_drop is what tells the webview
        // to unregister its callback).
        for _ in 0..3 {
            inner
                .subscribe_session(&session, &view_a, counting(&dropped))
                .unwrap();
        }
        assert_eq!(inner.session_channel_count(), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 2);

        // A second view of the same session is independent.
        inner
            .subscribe_session(&session, &view_b, counting(&dropped))
            .unwrap();
        assert_eq!(inner.session_channel_count(), 2);

        inner.unsubscribe_session(&session, &view_a);
        assert_eq!(inner.session_channel_count(), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 3);
        // Unknown ids are a harmless no-op (post-reconnect unsubscribe).
        inner.unsubscribe_session(&session, &view_a);
        inner.unsubscribe_session("nope", &view_b);
        assert_eq!(inner.session_channel_count(), 1);

        inner.unsubscribe_session(&session, &view_b);
        assert_eq!(inner.session_channel_count(), 0);
        assert_eq!(dropped.load(Ordering::SeqCst), 4);
        assert!(
            inner.session_channels.lock().unwrap().is_empty(),
            "empty session entries are removed too"
        );
    }

    #[tokio::test]
    async fn session_exited_releases_output_channels_after_the_grace_period() {
        let sessions: Arc<SessionChannels> = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let events: Arc<EventChannels> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let revision = Arc::new(AtomicU64::new(0));
        let session_id = uuid::Uuid::new_v4().to_string();
        let other_id = uuid::Uuid::new_v4().to_string();
        for id in [&session_id, &other_id] {
            sessions.lock().unwrap().insert(
                id.clone(),
                HashMap::from([(uuid::Uuid::new_v4().to_string(), capture_channel())]),
            );
        }
        let (tx, rx) = mpsc::unbounded_channel();
        spawn_control_forwarder(
            rx,
            Arc::clone(&events),
            Arc::clone(&sessions),
            revision,
            Duration::from_millis(30),
        );
        tx.send(Frame::Event(term_contracts::rpc::RpcEvent {
            v: 1,
            event: RpcEventKind::SessionExited,
            payload: json!({"session_id": session_id, "exit_code": 0,
                            "descendants_remaining": false, "reason": "exited"}),
        }))
        .unwrap();
        // Still routable during the grace (the data socket may lag).
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(sessions.lock().unwrap().contains_key(&session_id));
        tokio::time::sleep(Duration::from_millis(80)).await;
        let guard = sessions.lock().unwrap();
        assert!(!guard.contains_key(&session_id), "exited session released");
        assert!(
            guard.contains_key(&other_id),
            "unrelated sessions untouched"
        );
    }

    /// A Channel capturing deliveries into a shared buffer (the tauri
    /// Channel works headless: it just invokes the registered callback).
    fn capture_channel() -> EventChannel {
        Channel::new(|body| {
            let _ = body;
            Ok(())
        })
    }

    /// A channel whose sends always fail, standing in for a dead webview.
    fn dead_channel() -> EventChannel {
        Channel::new(|_body| Err(tauri::Error::AssetNotFound("dead-channel".into())))
    }

    #[tokio::test]
    async fn data_forwarder_batches_bursts_with_bounded_size_and_per_session_order() {
        use tauri::ipc::InvokeResponseBody;
        const RECORDS: usize = 230;
        let delivered = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let done = Arc::new(tokio::sync::Notify::new());
        let sessions = Arc::new(SessionChannels::default());
        for session_id in ["left", "right"] {
            let sink = Arc::clone(&delivered);
            let count = Arc::clone(&count);
            let done = Arc::clone(&done);
            let channel = Channel::new(move |body| {
                if let InvokeResponseBody::Json(text) = body {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    let size = value["payload"].as_array().map_or(1, Vec::len);
                    sink.lock().unwrap().push(value);
                    if count.fetch_add(size, Ordering::SeqCst) + size == RECORDS {
                        done.notify_one();
                    }
                }
                Ok(())
            });
            sessions
                .lock()
                .unwrap()
                .insert(session_id.into(), HashMap::from([("view".into(), channel)]));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let mut expected = HashMap::<String, Vec<Value>>::new();
        for index in 0..RECORDS {
            let session_id = if index % 2 == 0 { "left" } else { "right" };
            let resize = index % 23 == 0;
            let payload = json!({
                "session_id": session_id, "epoch": if index < 140 { "e1" } else { "e2" },
                "seq": (index + 1).to_string(), "kind": if resize { "resize" } else { "output" },
                "data_b64": "", "raw_len": if resize { 0 } else if index % 11 == 0 { 16384 } else { 1024 },
                "cols": 90, "rows": 30,
            });
            expected
                .entry(session_id.into())
                .or_default()
                .push(payload.clone());
            tx.send(Frame::Event(term_contracts::rpc::RpcEvent {
                v: 1,
                event: RpcEventKind::SessionOutput,
                payload,
            }))
            .unwrap();
        }
        drop(tx);
        spawn_data_forwarder(rx, sessions, Arc::new(EventChannels::default()));
        tokio::time::timeout(Duration::from_secs(2), done.notified())
            .await
            .unwrap();
        let delivered = delivered.lock().unwrap();
        assert!(
            delivered.len() < RECORDS / 2,
            "bursts must avoid a WebView call per record"
        );
        let mut actual = HashMap::<String, Vec<Value>>::new();
        for envelope in delivered.iter() {
            let records = if envelope["event"] == "session.output.batch" {
                envelope["payload"].as_array().unwrap().clone()
            } else {
                vec![envelope["payload"].clone()]
            };
            assert!(records.len() <= 128);
            assert!(
                records
                    .iter()
                    .map(|r| r["raw_len"].as_u64().unwrap())
                    .sum::<u64>()
                    <= 64 * 1024
            );
            for record in records {
                actual
                    .entry(record["session_id"].as_str().unwrap().into())
                    .or_default()
                    .push(record);
            }
        }
        assert_eq!(
            actual, expected,
            "output, resize and epoch boundaries must retain their order"
        );
    }

    #[tokio::test]
    async fn data_forwarder_routes_session_output_by_session() {
        use tauri::ipc::InvokeResponseBody;

        let delivered: Arc<std::sync::Mutex<Vec<Value>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&delivered);
        let channel: EventChannel = Channel::new(move |body| {
            if let InvokeResponseBody::Json(text) = body {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    sink.lock().unwrap().push(value);
                }
            }
            Ok(())
        });

        let sessions: Arc<SessionChannels> = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let events: Arc<EventChannels> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let session_id = uuid::Uuid::new_v4().to_string();
        sessions.lock().unwrap().insert(
            session_id.clone(),
            HashMap::from([(uuid::Uuid::new_v4().to_string(), channel)]),
        );
        // A dead channel for another session: pruned once that session gets
        // traffic (send fails → entry removed).
        let dead_session_id = uuid::Uuid::new_v4().to_string();
        sessions.lock().unwrap().insert(
            dead_session_id.clone(),
            HashMap::from([(uuid::Uuid::new_v4().to_string(), dead_channel())]),
        );

        let (tx, rx) = mpsc::unbounded_channel();
        spawn_data_forwarder(rx, Arc::clone(&sessions), Arc::clone(&events));

        let output_payload = json!({
            "session_id": session_id, "epoch": "e1", "seq": "2", "kind": "output",
            "data_b64": "aGk=", "raw_len": 2
        });
        tx.send(Frame::Event(term_contracts::rpc::RpcEvent {
            v: 1,
            event: RpcEventKind::SessionOutput,
            payload: output_payload.clone(),
        }))
        .unwrap();
        // Output for the dead session prunes its entry.
        tx.send(Frame::Event(term_contracts::rpc::RpcEvent {
            v: 1,
            event: RpcEventKind::SessionOutput,
            payload: json!({"session_id": dead_session_id, "epoch": "e1", "seq": "1",
                            "kind": "resize", "data_b64": "", "raw_len": 0,
                            "cols": 80, "rows": 24}),
        }))
        .unwrap();
        // Non-output events on the data connection go to the global channels.
        tx.send(Frame::Event(term_contracts::rpc::RpcEvent {
            v: 1,
            event: RpcEventKind::WorkloadChanged,
            payload: json!({"workload_id": "w-9", "state": "RUNNING"}),
        }))
        .unwrap();
        drop(tx);
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        let got = delivered.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["event"], "session.output");
        assert_eq!(got[0]["payload"], output_payload);
        drop(got);
        // Dead channel entry was pruned, live one kept.
        let sessions = sessions.lock().unwrap();
        assert!(sessions.contains_key(&session_id));
        assert_eq!(sessions.len(), 1);
    }

    #[tokio::test]
    async fn connect_fails_cleanly_when_daemon_never_becomes_ready() {
        struct NeverReadySpawner;
        impl DaemonSpawner for NeverReadySpawner {
            fn spawn_detached(
                &self,
                _binary: &std::path::Path,
                data_dir: &std::path::Path,
            ) -> std::io::Result<()> {
                std::fs::create_dir_all(data_dir.join("runtime"))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_string_lossy().to_string();
        let mut inner = BridgeInner::new(
            Arc::new(FakeTransport::default()),
            Arc::new(NeverReadySpawner),
            Arc::new(|| Some(PathBuf::from("fake-iyagi-termd"))),
            Arc::new(std::sync::Mutex::new(Vec::new())),
            Arc::new(std::sync::Mutex::new(HashMap::new())),
            Arc::new(AtomicU64::new(0)),
        )
        .with_ready_timeout(Duration::from_millis(250));
        let err = inner.connect(Some(&data_dir)).await.unwrap_err().into_rpc();
        assert_eq!(err.code, ErrorCode::DaemonUnavailable);
        assert!(err.retryable);
        assert!(!err.message.contains("token"));
        // The dir is published even though the daemon never came up, so a
        // key saved meanwhile lands where the next successful connect looks.
        assert_eq!(
            inner.resolved_data_dir().lock().unwrap().as_deref(),
            Some(dir.path())
        );
    }
}
