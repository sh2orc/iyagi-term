//! # Codex app-server adapter (ticket O08, docs/orchestration/03-adapters.md §3)
//!
//! Drives `codex app-server` over newline-delimited JSON-RPC stdio (framing
//! decision + protocol field names are pinned by the generated schemas in
//! `fixtures/` from codex-cli 0.153.4 — see `peer.rs` for the framing note).
//! Flow per 03 §3: `initialize` (clientInfo + minimal capabilities) →
//! `initialized` → `account/read` + `model/list` validation → `thread/start`
//! with explicit model/cwd/approvalPolicy/sandbox → `turn/start` with input
//! and the ProviderResult `outputSchema` → notifications → `turn/completed`
//! status + structured result assembly. `turn/steer` sends mid-turn input,
//! `turn/interrupt` cancels; `thread/resume` is only allowed for threads this
//! adapter recorded (ownership ledger below). Hidden reasoning is never
//! captured or surfaced.
//!
//! [`ProtocolPeer`] abstracts recorded JSONL transcripts and process transports.
//! The daemon uses [`SupervisedPeer`] through a shared durable ExecSupervisor:
//! its bounded stdin remains open for steering, interrupt, and approval replies,
//! and protocol completion is separate from native/durable process cleanup.
//! [`LivePeer`] remains a standalone legacy transport, outside mission dispatch.
//!
//! Capabilities come from the recorded live-evidence registry for the exact
//! binding and installed version. Offline transcripts never establish support.

mod approvals;
pub mod auth;
mod daemon_isolation;
mod models;
mod peer;
mod result_schema;
mod supervised;

pub(crate) use self::daemon_isolation::argv_prefix;
pub use self::models::{collect as collect_models, list as list_models};
pub use self::peer::{
    load_transcript, LivePeer, PeerError, PeerEvent, ProtocolPeer, RecordedPeer, TranscriptLine,
};
pub use self::result_schema::RESULT_KINDS;
pub use self::supervised::SupervisedPeer;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use term_contracts::mission::types::{AuthRoute, Binding, Id, RuntimeCapabilities, RuntimeKind};
use term_contracts::mission::MissionErrorCode;
use tokio::sync::mpsc;

#[cfg(test)]
use self::result_schema::provider_result_output_schema;
pub(crate) use self::result_schema::{parse_flat_result, task_result_output_schema};
use super::{
    AdapterEvent, AgentAdapter, CancelReceipt, CancelRejected, DeliveryReceipt, EventStream,
    FencingGate, QueuedReason, RunProbe, RunStart,
};

/// `codex app-server` argv verified against the installed 0.153.4 help
/// output (no `--stdio` flag — 03 §3 forbids blindly reusing usage argv).
const APP_SERVER_ARGV: [&str; 1] = ["app-server"];

/// Server→client request methods this adapter answers with approval
/// decisions (fixtures/ServerRequest.json). Anything else gets a
/// `-32601` error response so the server is never left hanging.
const APPROVAL_METHODS: [&str; 4] = [
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "execCommandApproval",
    "applyPatchApproval",
];

/// Approval traffic is untrusted app-server output, so what this adapter
/// stores and re-emits per run stays bounded (same shape as the OpenCode
/// adapter's approval caps): per-entry id/question sizes, the pending entry
/// count, and the pending set's bytes (ids, methods, and the re-emitted
/// question bytes). Per-entry overflow is rejected with the same `-32601`
/// response as unsupported methods; pending-set overflow is protocol abuse
/// and fails the run once (03 §2 caps, like the raw line cap).
///
/// The question cap must cover a real `item/fileChange/requestApproval`:
/// its question embeds the pretty-printed change evidence that
/// `approvals.rs` deliberately retains up to 64 KiB, and the thread runs
/// `approvalPolicy: untrusted`, so every patch asks. 128 KiB is that
/// evidence plus envelope overhead. The set cap still admits a handful of
/// maximum-size approvals while bounding a flood of them.
const MAX_PENDING_APPROVALS: usize = 256;
const MAX_APPROVAL_ID_BYTES: usize = 4 * 1024;
const MAX_APPROVAL_QUESTION_BYTES: usize = 128 * 1024;
const MAX_PENDING_APPROVAL_BYTES: usize = 1024 * 1024;

/// Builds the per-run transport. Tests inject recorded transcripts; the live
/// constructor spawns the app-server child.
pub type PeerFactory =
    Arc<dyn Fn(&RunStart) -> Result<Arc<dyn ProtocolPeer>, String> + Send + Sync>;
type IoPeerFactory = Arc<dyn Fn(&RunStart) -> std::io::Result<Arc<dyn ProtocolPeer>> + Send + Sync>;

// ---- binding ---------------------------------------------------------------

/// Codex-specific view of a [`Binding`], derived inside the adapter because
/// the O08 `RunStart` carries no codex fields (thread/start's explicit
/// model/cwd/approval/sandbox all resolve from here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexBindingConfig {
    pub program: String,
    pub provider_id: String,
    pub model_id: String,
    pub effort: Option<String>,
    pub auth_route: AuthRoute,
    /// ThreadStartParams.sandbox (SandboxMode enum).
    pub sandbox: &'static str,
    pub allow_network: bool,
    /// ThreadStartParams.approvalPolicy — untrusted commands surface as
    /// Decisions (03 §3 step 6).
    pub approval_policy: &'static str,
    /// Whether the binding evidences `turn/steer` (capability-gated).
    pub steer_supported: bool,
}

impl CodexBindingConfig {
    /// A binding identifies the provider, not a task's write authorization.
    /// Actual starts use from_run to apply the daemon-issued task access.
    pub fn from_binding(binding: &Binding) -> Result<Self, String> {
        if binding.runtime != RuntimeKind::Codex {
            return Err(format!(
                "binding runtime is {:?}, not codex",
                binding.runtime
            ));
        }
        if binding.program.trim().is_empty() {
            return Err("binding program is empty".into());
        }
        if binding.model_id.trim().is_empty() {
            return Err("binding model_id is empty".into());
        }
        if binding.provider_id.trim().is_empty() {
            return Err("binding provider_id is empty".into());
        }
        Ok(CodexBindingConfig {
            program: binding.program.clone(),
            provider_id: binding.provider_id.clone(),
            model_id: binding.model_id.clone(),
            effort: binding.effort.clone().filter(|e| !e.trim().is_empty()),
            auth_route: binding.auth_route,
            sandbox: "read-only",
            allow_network: false,
            approval_policy: "untrusted",
            steer_supported: binding.capabilities.steer.supported,
        })
    }
    pub fn from_run(run: &RunStart) -> Result<Self, String> {
        if run.workspace_access == crate::agent_runtime::WorkspaceAccess::Write
            && !run
                .workspace
                .as_ref()
                .is_some_and(|path| path.is_absolute() && path.is_dir())
        {
            return Err(
                "Codex write tasks require an existing absolute workspace directory".into(),
            );
        }
        let mut cfg = Self::from_binding(&run.binding)?;
        cfg.sandbox = match run.workspace_access {
            crate::agent_runtime::WorkspaceAccess::ReadOnly => "read-only",
            crate::agent_runtime::WorkspaceAccess::Write => "workspace-write",
        };
        // The sandbox is the boundary of a mission run, not the approval
        // prompt: the daemon issues the workspace and the write authority, and
        // the run cannot widen either. `untrusted` on top of that turns every
        // command the provider has not pre-trusted into a blocking Decision —
        // a Lead that opens with `pwd`, `git status` and two `rg` calls stops
        // four times before it has read a single file, and each answer only
        // buys the next command. So the policy follows the authority the task
        // was actually given:
        //
        // * read-only (plan/research/review) — nothing is writable and the
        //   network is off, so there is no escalation left to approve.
        // * workspace-write (implement/integrate) — writes are already
        //   confined to this run's worktree, and `on-request` leaves the
        //   provider free to ask when it wants something beyond that.
        cfg.approval_policy = match run.workspace_access {
            crate::agent_runtime::WorkspaceAccess::ReadOnly => "never",
            crate::agent_runtime::WorkspaceAccess::Write => "on-request",
        };
        cfg.allow_network = run.allow_network;
        Ok(cfg)
    }

    fn sandbox_policy(&self, cwd: &Path) -> Value {
        if self.sandbox == "workspace-write" {
            json!({"type":"workspaceWrite","writableRoots":[cwd.to_string_lossy()],"networkAccess":self.allow_network,
                "excludeSlashTmp":true,"excludeTmpdirEnvVar":true})
        } else {
            json!({"type":"readOnly","networkAccess":self.allow_network})
        }
    }
}

// ---- per-run state ----------------------------------------------------------

/// One approval awaiting a daemon answer (`answer` consumes it exactly once).
#[derive(Debug, Clone)]
struct PendingApproval {
    /// The server request id EXACTLY as the wire carried it.
    wire_id: Value,
    method: String,
    /// Bytes of the question re-emitted for this entry; counted against
    /// the pending-set byte cap while the approval stays open.
    question_bytes: usize,
}

/// State shared between the engine thread and the trait surface.
struct RunShared {
    provider_thread_id: Mutex<Option<String>>,
    provider_turn_id: Mutex<Option<String>>,
    provider_started_turn: Mutex<Option<(String, String)>>,
    turn_active: AtomicBool,
    terminal: AtomicBool,
    interrupt_requested: AtomicBool,
    interrupt_sent: AtomicBool,
    requested_model: String,
    observed_model: Mutex<Option<String>>,
    pending_approvals: Mutex<HashMap<String, PendingApproval>>,
    pending_steers: Mutex<HashMap<u64, (String, std::sync::mpsc::SyncSender<DeliveryReceipt>)>>,
    next_request_id: AtomicU64,
    exit_code: Mutex<Option<i32>>,
    steer_supported: bool,
}

impl RunShared {
    fn new(cfg: &CodexBindingConfig) -> Self {
        RunShared {
            provider_thread_id: Mutex::new(None),
            provider_turn_id: Mutex::new(None),
            provider_started_turn: Mutex::new(None),
            turn_active: AtomicBool::new(false),
            terminal: AtomicBool::new(false),
            interrupt_requested: AtomicBool::new(false),
            interrupt_sent: AtomicBool::new(false),
            requested_model: cfg.model_id.clone(),
            observed_model: Mutex::new(None),
            pending_approvals: Mutex::new(HashMap::new()),
            pending_steers: Mutex::new(HashMap::new()),
            next_request_id: AtomicU64::new(1),
            exit_code: Mutex::new(None),
            steer_supported: cfg.steer_supported,
        }
    }

    fn next_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::AcqRel)
    }

    fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::Acquire)
    }

    fn mark_terminal(&self) {
        self.terminal.store(true, Ordering::Release);
        self.turn_active.store(false, Ordering::Release);
        for (_, (_, sender)) in self
            .pending_steers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
        {
            let _ = sender.send(DeliveryReceipt::Unknown {
                reason: "turn ended before the steer acknowledgement",
            });
        }
    }
}

struct RunSlot {
    peer: Arc<dyn ProtocolPeer>,
    shared: Arc<RunShared>,
}

// ---- bounded event fan-out (F1) ----------------------------------------------

/// Delivery quota window for coalescable events ([`BoundedFanout`]).
/// Admission is bounded in events AND bytes per window: at most 24 events
/// (192/s), and once 256 KiB of payload went out in a window the rest
/// stages (the first send of a window may be one oversized entry, itself
/// bounded by the 1 MiB protocol line cap). The consumer's drain rate is
/// not assumed: the mission actor pauses draining while a start is in
/// flight and ticks slower under git/DB work, so this byte bound is what
/// caps how fast a stalled subscriber queue can grow.
const FANOUT_WINDOW: Duration = Duration::from_millis(125);
const FANOUT_WINDOW_EVENTS: u32 = 24;
const FANOUT_WINDOW_BYTES: usize = 256 * 1024;
/// Byte and entry bounds for staged Activity text. The byte budget mirrors
/// the session flow-control watermark (256 KiB); older staged chunks are
/// shed with one marker per episode once it is exhausted.
const FANOUT_STAGED_BYTES: usize = 256 * 1024;
const FANOUT_STAGED_ENTRIES: usize = 256;
/// Quiet-period flush cadence for staged events when no publish arrives.
const FANOUT_FLUSH_TICK: Duration = Duration::from_millis(50);

/// Event fan-out with publish-side bounds (same role as the fake adapter's
/// bus; shared by the Claude and OpenCode adapters). `EventStream` still
/// wraps a tokio unbounded receiver — its type lives in the shared port
/// module — so the bound is enforced here, ahead of the channel:
/// order-critical kinds (Started, ApprovalRequested, Result/Failed/…,
/// Disconnected) flush anything staged and are delivered immediately;
/// Activity text and latest-wins observations (Usage, RateLimited,
/// ModelObserved) stage behind the per-window event/byte quota once it is
/// exhausted. Staged Activity coalesces per run+token up to the staged byte
/// budget (display-only concatenation) and is shed oldest-first above that
/// budget. Shedding never affects a run's
/// outcome: terminal results and errors ride order-critical kinds, and the
/// consumer resolves Usage/RateLimited/ModelObserved with max/latest-wins.
pub(crate) struct BoundedFanout {
    subscribers: Mutex<Vec<mpsc::UnboundedSender<AdapterEvent>>>,
    staging: Mutex<StagedEvents>,
}

impl Default for BoundedFanout {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct StagedEvents {
    /// Staged Activity chunks, oldest first; consecutive chunks of one
    /// run+token merge into the tail entry (display-only concatenation)
    /// while it stays within the staged byte budget.
    activity: VecDeque<AdapterEvent>,
    activity_bytes: usize,
    /// Newest pending Usage/RateLimited/ModelObserved per run+token+kind.
    latest: HashMap<(Id, u64, u8), AdapterEvent>,
    /// Shedding bookkeeping for one drop marker per episode.
    dropped_chunks: usize,
    dropped_bytes: usize,
    marker_run: Option<(Id, u64)>,
    window_start: Option<Instant>,
    sent_in_window: u32,
    /// Payload bytes sent in the current window (see [`event_bytes`]).
    window_bytes: usize,
}

impl BoundedFanout {
    pub(crate) fn new() -> Self {
        BoundedFanout {
            subscribers: Mutex::new(Vec::new()),
            staging: Mutex::new(StagedEvents::default()),
        }
    }

    /// Quiet-period flusher: staged events normally drain on the next
    /// publish or ahead of an order-critical event; this thread covers a
    /// staged tail that would otherwise wait when the CLI goes silent
    /// mid-run. It holds only a weak handle, upgraded for one flush at a
    /// time so the thread never owns the fan-out across its sleep, and
    /// exits once the last adapter or engine drops the fan-out. A failed
    /// spawn merely removes that coverage — publishes still drain staging.
    pub(crate) fn start_flusher(fanout: &Arc<Self>) {
        let weak = Arc::downgrade(fanout);
        let spawned = std::thread::Builder::new()
            .name("agent-fanout-flush".into())
            .spawn(move || {
                while let Some(fanout) = weak.upgrade() {
                    fanout.flush_tick();
                    // Release before sleeping: this thread must never be
                    // the fan-out's last owner across the pause.
                    drop(fanout);
                    std::thread::sleep(FANOUT_FLUSH_TICK);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(
                %error,
                "adapter event flusher did not start; staged events drain on the next publish"
            );
        }
    }

    pub(crate) fn add_subscriber(&self, tx: mpsc::UnboundedSender<AdapterEvent>) {
        self.subscribers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(tx);
    }

    pub(crate) fn publish(&self, event: AdapterEvent) {
        // Lock order is always subscribers → staging (flush_tick matches),
        // and both are released before returning: sends on unbounded
        // channels never block, so no publisher can be held here.
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|p| p.into_inner());
        let mut staging = self.staging.lock().unwrap_or_else(|p| p.into_inner());
        let coalescable =
            matches!(event, AdapterEvent::Activity { .. }) || latest_slot(&event).is_some();
        if coalescable {
            if staging.is_empty() && staging.has_quota() {
                send_all(&mut subscribers, &event);
                staging.count_send(&event);
            } else {
                staging.stage(event);
                staging.flush(&mut subscribers, false);
            }
        } else {
            // Order-critical: deliver everything staged first so relative
            // order (e.g. Usage ahead of a terminal Result) is preserved,
            // then deliver immediately. Bypassing the quota is safe here:
            // these kinds are structurally bounded per run (handshake
            // events, the capped approval set, one terminal event), never
            // a firehose.
            staging.flush(&mut subscribers, true);
            send_all(&mut subscribers, &event);
            staging.saturate_window();
        }
    }

    /// Flusher-thread entry: drains staged events under the same quota.
    fn flush_tick(&self) {
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|p| p.into_inner());
        let mut staging = self.staging.lock().unwrap_or_else(|p| p.into_inner());
        staging.flush(&mut subscribers, false);
    }
}

impl StagedEvents {
    fn is_empty(&self) -> bool {
        // A pending drop marker also blocks the direct-send fast path so it
        // can never surface after newer text.
        self.activity.is_empty() && self.latest.is_empty() && self.dropped_chunks == 0
    }

    fn has_quota(&mut self) -> bool {
        self.roll_window();
        self.within_quota()
    }

    /// Both window bounds hold: events sent and payload bytes sent.
    fn within_quota(&self) -> bool {
        self.sent_in_window < FANOUT_WINDOW_EVENTS && self.window_bytes < FANOUT_WINDOW_BYTES
    }

    fn roll_window(&mut self) {
        if self
            .window_start
            .is_none_or(|start| start.elapsed() >= FANOUT_WINDOW)
        {
            self.window_start = Some(Instant::now());
            self.sent_in_window = 0;
            self.window_bytes = 0;
        }
    }

    fn count_send(&mut self, event: &AdapterEvent) {
        self.sent_in_window = self.sent_in_window.saturating_add(1);
        self.window_bytes = self.window_bytes.saturating_add(event_bytes(event));
    }

    /// Critical events bypass the quota once; saturating the window keeps
    /// the staged kinds behind them from riding the bypass.
    fn saturate_window(&mut self) {
        self.roll_window();
        self.sent_in_window = FANOUT_WINDOW_EVENTS;
    }

    fn stage(&mut self, event: AdapterEvent) {
        if let Some(slot) = latest_slot(&event) {
            match self.latest.get_mut(&slot) {
                Some(pending) => merge_latest(pending, &event),
                None => {
                    self.latest.insert(slot, event);
                }
            }
            return;
        }
        let AdapterEvent::Activity {
            run_id,
            fencing_token,
            chunk,
        } = event
        else {
            return; // order-critical kinds never stage
        };
        let bytes = chunk.len();
        // Merging stops at the staged byte budget, so the never-shed newest
        // entry stays one budget or one capped protocol line in size.
        let merge_tail = match self.activity.back() {
            Some(AdapterEvent::Activity {
                run_id: tail_run,
                fencing_token: tail_token,
                chunk: tail,
            }) => {
                tail_run == &run_id
                    && *tail_token == fencing_token
                    && tail.len().saturating_add(bytes) <= FANOUT_STAGED_BYTES
            }
            _ => false,
        };
        if merge_tail {
            if let Some(AdapterEvent::Activity { chunk: tail, .. }) = self.activity.back_mut() {
                tail.push_str(&chunk);
                self.activity_bytes = self.activity_bytes.saturating_add(bytes);
            }
        } else {
            self.activity_bytes = self.activity_bytes.saturating_add(bytes);
            self.activity.push_back(AdapterEvent::Activity {
                run_id,
                fencing_token,
                chunk,
            });
        }
        self.shed_to_budget();
    }

    /// Shed oldest staged chunks above the byte/entry budget. The newest
    /// entry is never shed; because merging stops at the budget, it may
    /// transiently exceed the budget by at most one capped protocol line
    /// (1 MiB).
    fn shed_to_budget(&mut self) {
        while (self.activity_bytes > FANOUT_STAGED_BYTES
            || self.activity.len() > FANOUT_STAGED_ENTRIES)
            && self.activity.len() > 1
        {
            match self.activity.pop_front() {
                Some(AdapterEvent::Activity {
                    run_id,
                    fencing_token,
                    chunk,
                }) => {
                    self.activity_bytes = self.activity_bytes.saturating_sub(chunk.len());
                    self.dropped_chunks = self.dropped_chunks.saturating_add(1);
                    self.dropped_bytes = self.dropped_bytes.saturating_add(chunk.len());
                    if self.marker_run.is_none() {
                        self.marker_run = Some((run_id, fencing_token));
                    }
                }
                Some(_) => {}
                None => break,
            }
        }
    }

    /// One drop marker per shedding episode, delivered before the chunks
    /// that survived it. It rides Activity — display-only text into the
    /// run's activity log, like the shed chunks it reports.
    fn take_marker(&mut self) -> Option<AdapterEvent> {
        if self.dropped_chunks == 0 {
            return None;
        }
        let chunks = self.dropped_chunks;
        let bytes = self.dropped_bytes;
        let (run_id, fencing_token) = self.marker_run.clone()?;
        self.dropped_chunks = 0;
        self.dropped_bytes = 0;
        self.marker_run = None;
        Some(AdapterEvent::Activity {
            run_id,
            fencing_token,
            chunk: format!(
                "[iyagi: shed {chunks} activity chunk(s) ({bytes} bytes) above the \
                 {} KiB adapter event budget; display text only, the run result is \
                 unaffected]",
                FANOUT_STAGED_BYTES / 1024,
            ),
        })
    }

    fn take_next(&mut self) -> Option<AdapterEvent> {
        if let Some(event) = self.activity.pop_front() {
            if let AdapterEvent::Activity { chunk, .. } = &event {
                self.activity_bytes = self.activity_bytes.saturating_sub(chunk.len());
            }
            return Some(event);
        }
        let key = self.latest.keys().next().cloned()?;
        self.latest.remove(&key)
    }

    /// Send staged events, oldest first. `force` (ahead of an
    /// order-critical event) ignores the quota for this one flush and then
    /// saturates the window.
    fn flush(&mut self, subscribers: &mut Vec<mpsc::UnboundedSender<AdapterEvent>>, force: bool) {
        self.roll_window();
        loop {
            if !force && !self.within_quota() {
                break;
            }
            let Some(event) = self.take_marker().or_else(|| self.take_next()) else {
                break;
            };
            send_all(subscribers, &event);
            self.count_send(&event);
        }
        if force {
            self.sent_in_window = FANOUT_WINDOW_EVENTS;
        }
    }
}

/// Latest-wins observation kinds that may stage and merge: the consumer
/// keeps the maximum usage values, the longest rate-limit horizon, and the
/// newest observed model, so merging an unflushed older observation into
/// its replacement changes nothing the consumer would conclude. The slot
/// includes the fencing token: observations under different tokens never
/// merge, so a newer token's value is never delivered under a stale one.
fn latest_slot(event: &AdapterEvent) -> Option<(Id, u64, u8)> {
    let tag = match event {
        AdapterEvent::Usage { .. } => 0,
        AdapterEvent::RateLimited { .. } => 1,
        AdapterEvent::ModelObserved { .. } => 2,
        _ => return None,
    };
    Some((event.run_id().clone(), event.fencing_token(), tag))
}

fn merge_latest(pending: &mut AdapterEvent, fresh: &AdapterEvent) {
    let replace = match (&mut *pending, fresh) {
        (
            AdapterEvent::Usage {
                input_tokens: old_input,
                output_tokens: old_output,
                cost_usd_micros: old_cost,
                ..
            },
            AdapterEvent::Usage {
                input_tokens: fresh_input,
                output_tokens: fresh_output,
                cost_usd_micros: fresh_cost,
                ..
            },
        ) => {
            *old_input = max_optional(*old_input, *fresh_input);
            *old_output = max_optional(*old_output, *fresh_output);
            *old_cost = max_optional(*old_cost, *fresh_cost);
            false
        }
        (
            AdapterEvent::RateLimited {
                observation: old, ..
            },
            AdapterEvent::RateLimited {
                observation: new, ..
            },
        ) => {
            // The consumer ignores non-extending horizons; keep the longer.
            new.resets_at_unix_ms.get() > old.resets_at_unix_ms.get()
        }
        _ => true,
    };
    if replace {
        *pending = fresh.clone();
    }
}

fn max_optional(old: Option<u64>, fresh: Option<u64>) -> Option<u64> {
    match (old, fresh) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

/// Payload bytes one event spends from the window's byte quota. The
/// latest-wins observations other than a model name are small fixed-size
/// records and count as zero; their event count still spends the quota.
fn event_bytes(event: &AdapterEvent) -> usize {
    match event {
        AdapterEvent::Activity { chunk, .. } => chunk.len(),
        AdapterEvent::ModelObserved { model, .. } => model.len(),
        _ => 0,
    }
}

/// Fan one event out to live subscribers, pruning closed channels.
fn send_all(subscribers: &mut Vec<mpsc::UnboundedSender<AdapterEvent>>, event: &AdapterEvent) {
    subscribers.retain(|tx| tx.send(event.clone()).is_ok());
}

// ---- adapter ----------------------------------------------------------------

/// The Codex runtime adapter (03 §3). One instance serves many runs; each
/// run gets its own peer, engine thread, and normalized event flow.
pub struct CodexAdapter {
    peer_factory: IoPeerFactory,
    runs: Mutex<HashMap<Id, Arc<RunSlot>>>,
    bus: Arc<BoundedFanout>,
    gate: Arc<FencingGate>,
    claimed_runs: Mutex<HashSet<Id>>,
}

impl Drop for CodexAdapter {
    fn drop(&mut self) {
        for slot in self.runs.lock().unwrap_or_else(|p| p.into_inner()).values() {
            slot.peer.close();
        }
    }
}

impl CodexAdapter {
    pub fn authenticated(
        supervisor: Arc<crate::exec::ExecSupervisor>,
        runtime: tokio::runtime::Handle,
        connections: Option<Arc<crate::connections::ConnectionStore>>,
        config_root: PathBuf,
    ) -> Arc<Self> {
        Self::with_io_peer_factory(Arc::new(move |run| {
            SupervisedPeer::spawn_authenticated(
                run,
                &supervisor,
                &runtime,
                connections.as_deref(),
                &config_root,
            )
            .map(|peer| peer as Arc<dyn ProtocolPeer>)
        }))
    }
    /// Shared process lifecycle without production authentication setup.
    /// Fixtures and legacy callers use this; the daemon uses authenticated.
    pub fn supervised(
        supervisor: Arc<crate::exec::ExecSupervisor>,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        Self::with_peer_factory(Arc::new(move |run| {
            CodexBindingConfig::from_run(run)?;
            SupervisedPeer::spawn(run, &supervisor, &runtime)
                .map(|peer| peer as Arc<dyn ProtocolPeer>)
                .map_err(|e| e.0)
        }))
    }
    /// Full control over transport construction (tests: recorded peers).
    pub fn with_peer_factory(peer_factory: PeerFactory) -> Arc<Self> {
        Self::with_io_peer_factory(Arc::new(move |run| {
            peer_factory(run).map_err(std::io::Error::other)
        }))
    }
    fn with_io_peer_factory(peer_factory: IoPeerFactory) -> Arc<Self> {
        let bus = Arc::new(BoundedFanout::new());
        BoundedFanout::start_flusher(&bus);
        Arc::new(CodexAdapter {
            peer_factory,
            runs: Mutex::new(HashMap::new()),
            bus,
            gate: FencingGate::new(),
            claimed_runs: Mutex::new(HashSet::new()),
        })
    }

    /// Offline constructor: every run replays the same recorded transcript.
    pub fn recorded_transcript(path: PathBuf) -> Arc<Self> {
        Self::with_peer_factory(Arc::new(move |_| {
            RecordedPeer::from_file(&path).map(|peer| peer as Arc<dyn ProtocolPeer>)
        }))
    }

    /// Standalone legacy constructor without a mission persistence context.
    /// Daemon dispatch uses [`Self::supervised`] and the shared native gate.
    pub fn live() -> Arc<Self> {
        Self::with_peer_factory(Arc::new(|run| {
            let cfg = CodexBindingConfig::from_run(&run)?;
            let cwd = run.workspace.clone().unwrap_or_else(std::env::temp_dir);
            let mut argv = self::daemon_isolation::argv_prefix(Path::new(&cfg.program));
            argv.extend(APP_SERVER_ARGV.iter().map(|s| s.to_string()));
            LivePeer::spawn(Path::new(&cfg.program), &argv, &cwd)
                .map(|peer| peer as Arc<dyn ProtocolPeer>)
                .map_err(|e| e.0)
        }))
    }

    fn slot(&self, run_id: &Id) -> Option<Arc<RunSlot>> {
        self.runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(run_id)
            .cloned()
    }

    /// 03 §2: requested vs observed model, both recorded. `None` when the
    /// run is unknown or never reached `thread/start`.
    pub fn model_observation(&self, run_id: &Id) -> Option<(String, Option<String>)> {
        let slot = self.slot(run_id)?;
        let observed = slot
            .shared
            .observed_model
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        Some((slot.shared.requested_model.clone(), observed))
    }

    /// Installation/version/auth-route metadata plus the O18 live-evidence
    /// registry: capabilities flip ONLY when the parsed CLI version matches
    /// the recorded live-tested version (03 §6: new versions reset to
    /// unknown). The probe itself still performs no inference.
    pub fn probe(binding: &Binding) -> CodexProbe {
        let version = read_cli_version(&binding.program);
        CodexProbe {
            provider_id: binding.provider_id.clone(),
            model_id: binding.model_id.clone(),
            auth_route: binding.auth_route,
            program: binding.program.clone(),
            installed: program_exists(&binding.program),
            capabilities: super::capability_evidence::capabilities_for_binding(
                binding,
                std::env::consts::OS,
                version.as_deref(),
            ),
            version,
        }
    }
}

/// Run `<program> --version` and parse it — local metadata only, no
/// inference, no auth (03 §3). `None` when the program is missing or the
/// output does not parse.
pub fn read_cli_version(program: &str) -> Option<String> {
    super::installation::version(program, term_contracts::mission::types::RuntimeKind::Codex).ok()
}

/// Probe metadata result (03 §1 `probe`, capability mapping recorded in
/// `done/orchestration/O08.md`).
#[derive(Debug, Clone, PartialEq)]
pub struct CodexProbe {
    pub provider_id: String,
    pub model_id: String,
    pub auth_route: AuthRoute,
    pub program: String,
    pub installed: bool,
    pub version: Option<String>,
    pub capabilities: RuntimeCapabilities,
}

/// Parse `codex --version` output (`codex-cli 0.153.4` → `0.153.4`).
/// Pure so tests never spawn the CLI.
pub fn parse_cli_version(output: &str) -> Option<String> {
    let first = output.lines().next()?;
    let token = first.split_whitespace().last()?;
    let version_shaped = token.starts_with(|c: char| c.is_ascii_digit())
        && token.contains('.')
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if version_shaped {
        Some(token.to_string())
    } else {
        None
    }
}

/// Program existence without execution: absolute/relative paths stat
/// directly; bare names scan `PATH` (plus `.exe` on Windows).
pub fn program_exists(program: &str) -> bool {
    let path = Path::new(program);
    if program.contains('/') || program.contains('\\') {
        return path.is_file();
    }
    let Ok(path_var) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| {
        dir.join(program).is_file()
            || (cfg!(windows) && dir.join(format!("{program}.exe")).is_file())
    })
}

impl AgentAdapter for CodexAdapter {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn start(&self, run: RunStart) -> std::io::Result<()> {
        let cfg = CodexBindingConfig::from_run(&run)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        if !self
            .claimed_runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(run.run_id.clone())
        {
            return Err(std::io::Error::other(
                "Codex run was already claimed; create a new attempt",
            ));
        }
        let peer = (self.peer_factory)(&run).map_err(super::retry::mark_before_submission)?;
        let shared = Arc::new(RunShared::new(&cfg));
        self.gate.register(&run.run_id, run.fencing_token);
        self.runs.lock().unwrap_or_else(|p| p.into_inner()).insert(
            run.run_id.clone(),
            Arc::new(RunSlot {
                peer: Arc::clone(&peer),
                shared: Arc::clone(&shared),
            }),
        );

        let engine = Engine {
            run_id: run.run_id.clone(),
            token: run.fencing_token,
            cfg,
            peer,
            shared,
            bus: Arc::clone(&self.bus),
            streamed_items: HashSet::new(),
            activity_at_line_start: true,
            file_changes: approvals::FileChanges::default(),
            task_kind: run.task_kind,
            cwd: run.workspace.clone().unwrap_or_else(std::env::temp_dir),
            prompt: run.prompt_stdin.clone(),
            context_path: run.context_path.clone(),
            authenticated: false,
            task_submitted: false,
        };
        std::thread::Builder::new()
            .name(format!("codex-run-{}", run.run_id))
            .spawn(move || engine.drive())
            .map_err(|e| std::io::Error::new(e.kind(), format!("codex engine spawn: {e}")))?;
        Ok(())
    }

    fn send_message(&self, run_id: &Id, body: &str) -> DeliveryReceipt {
        let Some(slot) = self.slot(run_id) else {
            return DeliveryReceipt::Rejected {
                reason: "unknown run",
            };
        };
        let shared = &slot.shared;
        if shared.is_terminal() || !shared.turn_active.load(Ordering::Acquire) {
            // No active turn: the body joins the next run's context (02 §8).
            return DeliveryReceipt::Queued {
                reason: QueuedReason::NextRun,
            };
        }
        if !shared.steer_supported {
            return DeliveryReceipt::Queued {
                reason: QueuedReason::SteerUnsupported,
            };
        }
        let (thread_id, turn_id) = match current_thread_and_turn(shared) {
            Some(pair) => pair,
            None => {
                return DeliveryReceipt::Queued {
                    reason: QueuedReason::NextRun,
                }
            }
        };
        let id = shared.next_id();
        let (sender, receipt) = std::sync::mpsc::sync_channel(1);
        {
            let mut pending = shared
                .pending_steers
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if !pending.is_empty() || shared.is_terminal() {
                return DeliveryReceipt::Queued {
                    reason: QueuedReason::NextRun,
                };
            }
            pending.insert(id, (turn_id.clone(), sender));
        }
        let message = json!({
            "id": id,
            "method": "turn/steer",
            "params": {
                "threadId": thread_id,
                "expectedTurnId": turn_id,
                "input": [ { "type": "text", "text": body } ],
            },
        });
        if slot.peer.send(&message).is_err() {
            shared
                .pending_steers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
            return DeliveryReceipt::Unknown {
                reason: "app-server message write was not confirmed; do not resend automatically",
            };
        }
        let result =
            receipt
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or(DeliveryReceipt::Unknown {
                    reason: "steer acknowledgement was not confirmed; do not resend automatically",
                });
        shared
            .pending_steers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        result
    }

    fn answer(&self, run_id: &Id, provider_request_id: &str, answer: &str) -> DeliveryReceipt {
        let Some(slot) = self.slot(run_id) else {
            return DeliveryReceipt::Rejected {
                reason: "unknown run",
            };
        };
        let shared = &slot.shared;
        if shared.is_terminal() {
            return DeliveryReceipt::Rejected {
                reason: "run already terminal",
            };
        }
        let pending = shared
            .pending_approvals
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(provider_request_id);
        let Some(pending) = pending else {
            return DeliveryReceipt::Rejected {
                reason: "unknown or obsolete approval request",
            };
        };
        let Some(decision) = map_approval_decision(&pending.method, answer) else {
            // Not consumed: put it back so a corrected answer can retry.
            shared
                .pending_approvals
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(provider_request_id.to_string(), pending);
            return DeliveryReceipt::Rejected {
                reason: "unrecognized approval answer",
            };
        };
        let response = json!({ "id": pending.wire_id, "result": { "decision": decision } });
        match slot.peer.send(&response) {
            Ok(()) => DeliveryReceipt::Delivered {
                provider_ref: Some(provider_request_id.to_string()),
            },
            Err(_) => DeliveryReceipt::Unknown {
                reason: "approval write was not confirmed; do not resend automatically",
            },
        }
    }

    fn interrupt(&self, run_id: &Id) -> CancelReceipt {
        let Some(slot) = self.slot(run_id) else {
            return CancelReceipt::Rejected {
                reason: CancelRejected::UnknownRun,
            };
        };
        let shared = &slot.shared;
        if shared.is_terminal() {
            return CancelReceipt::Rejected {
                reason: CancelRejected::AlreadyTerminal,
            };
        }
        let Some(_) = current_thread_and_turn(shared) else {
            return CancelReceipt::Rejected {
                reason: CancelRejected::Other("codex turn not established yet".into()),
            };
        };
        // The provider can respond on stdout before the stdin writer's
        // acknowledgment returns. Record our intent before sending.
        shared.interrupt_requested.store(true, Ordering::Release);
        match flush_interrupt(shared, slot.peer.as_ref()) {
            // Accepted records the cancellation intent. If turn/start has
            // replied before turn/started, the engine sends it on that event.
            // Only protocol/native cleanup can confirm termination.
            Ok(()) => CancelReceipt::Accepted,
            Err(e) => CancelReceipt::Rejected {
                reason: CancelRejected::Other(e.0),
            },
        }
    }

    fn inspect(&self, run_id: &Id) -> RunProbe {
        let Some(slot) = self.slot(run_id) else {
            return RunProbe::Absent;
        };
        if slot.shared.is_terminal() && slot.peer.cleanup_confirmed() {
            RunProbe::Finished {
                exit: *slot
                    .shared
                    .exit_code
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()),
            }
        } else {
            RunProbe::Running
        }
    }

    fn close(&self, run_id: &Id) -> CancelReceipt {
        let slot = self.slot(run_id);
        let Some(slot) = slot else {
            return CancelReceipt::Rejected {
                reason: CancelRejected::UnknownRun,
            };
        };
        slot.peer.close();
        if !slot.peer.cleanup_confirmed() {
            return CancelReceipt::Accepted;
        }
        self.runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(run_id);
        let exit = *slot
            .shared
            .exit_code
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        CancelReceipt::Confirmed { exit }
    }

    fn subscribe(&self) -> EventStream {
        let (tx, rx) = mpsc::unbounded_channel();
        self.bus.add_subscriber(tx);
        EventStream::new(rx, Arc::clone(&self.gate))
    }
}

fn flush_interrupt(shared: &RunShared, peer: &dyn ProtocolPeer) -> Result<(), PeerError> {
    if !shared.interrupt_requested.load(Ordering::Acquire) || shared.is_terminal() {
        return Ok(());
    }
    let Some((thread_id, turn_id)) = current_thread_and_turn(shared) else {
        return Ok(());
    };
    if shared
        .provider_started_turn
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        != Some(&(thread_id.clone(), turn_id.clone()))
        || shared.interrupt_sent.swap(true, Ordering::AcqRel)
    {
        return Ok(());
    }
    peer.send(&json!({"id":shared.next_id(),"method":"turn/interrupt","params":{"threadId":thread_id,"turnId":turn_id}}))
}

fn current_thread_and_turn(shared: &RunShared) -> Option<(String, String)> {
    let thread = shared
        .provider_thread_id
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()?;
    let turn = shared
        .provider_turn_id
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()?;
    Some((thread, turn))
}

/// Daemon answer vocabulary → the decision enum each approval method's
/// response schema defines (unified: accept/decline/cancel, legacy:
/// approved/denied). `None` = unrecognized answer.
fn map_approval_decision(method: &str, answer: &str) -> Option<Value> {
    let normalized = answer.trim().to_ascii_lowercase();
    let unified = matches!(
        method,
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
    );
    match normalized.as_str() {
        "accept" | "approve" | "approved" | "yes" | "y" | "true" => {
            Some(json!(if unified { "accept" } else { "approved" }))
        }
        "decline" | "deny" | "denied" | "no" | "n" | "false" => {
            Some(json!(if unified { "decline" } else { "denied" }))
        }
        "cancel" | "abort" if unified => Some(json!("cancel")),
        _ => None,
    }
}

// ---- engine -----------------------------------------------------------------

/// One whole activity line, framed for a tail that is concatenated verbatim:
/// it opens a line when the tail is mid-sentence and always closes the line it
/// wrote. Empty bodies produce nothing rather than a blank line.
fn activity_line(body: &str, at_line_start: bool) -> String {
    let body = body.trim_end_matches('\n');
    if body.is_empty() {
        return String::new();
    }
    let mut chunk = String::with_capacity(body.len() + 2);
    if !at_line_start {
        chunk.push('\n');
    }
    chunk.push_str(body);
    chunk.push('\n');
    chunk
}

/// One run's protocol state machine on its own thread.
struct Engine {
    authenticated: bool,
    task_submitted: bool,
    run_id: Id,
    token: u64,
    cfg: CodexBindingConfig,
    peer: Arc<dyn ProtocolPeer>,
    shared: Arc<RunShared>,
    bus: Arc<BoundedFanout>,
    /// agentMessage items already streamed as deltas (item/completed must
    /// not duplicate them).
    streamed_items: HashSet<String>,
    /// Whether the activity tail currently ends on a line boundary. Chunks are
    /// concatenated verbatim downstream (`BoundedFanout::stage_activity`
    /// merges them with `push_str`), so a whole-line chunk has to delimit
    /// itself or it fuses with the text before it.
    activity_at_line_start: bool,
    file_changes: approvals::FileChanges,
    task_kind: Option<term_contracts::mission::types::TaskKind>,
    cwd: PathBuf,
    prompt: String,
    context_path: PathBuf,
}

impl Engine {
    fn emit(&self, event: AdapterEvent) {
        self.bus.publish(event);
    }

    /// A fragment of the line being written: never broken up, never padded.
    fn emit_activity_delta(&mut self, chunk: String) {
        if chunk.is_empty() {
            return;
        }
        self.activity_at_line_start = chunk.ends_with('\n');
        self.emit(AdapterEvent::Activity {
            run_id: self.run_id.clone(),
            fencing_token: self.token,
            chunk,
        });
    }

    /// A complete line (a lifecycle note, or a message that never streamed).
    /// It opens a line if the tail is mid-sentence and closes the one it
    /// wrote, so `[toolCall] completed` can never fuse onto the end of the
    /// model's last sentence the way `… 나누겠습니다.[userMessage] completed`
    /// did before.
    fn emit_activity_line(&mut self, body: &str) {
        let chunk = activity_line(body, self.activity_at_line_start);
        if chunk.is_empty() {
            return;
        }
        self.activity_at_line_start = true;
        self.emit(AdapterEvent::Activity {
            run_id: self.run_id.clone(),
            fencing_token: self.token,
            chunk,
        });
    }

    fn fail(&self, code: MissionErrorCode, message: impl Into<String>) {
        self.emit(AdapterEvent::Failed {
            run_id: self.run_id.clone(),
            fencing_token: self.token,
            code,
            message: message.into(),
        });
        self.shared.mark_terminal();
    }

    fn transport_failure(&self, message: String) {
        self.emit(if self.task_submitted {
            AdapterEvent::Disconnected {
                run_id: self.run_id.clone(),
                fencing_token: self.token,
            }
        } else {
            super::retry::failure(
                self.run_id.clone(),
                self.token,
                MissionErrorCode::ProviderUnavailable,
                message,
            )
        });
        self.shared.mark_terminal();
    }

    /// Initialize, verify effective configuration, authenticate, then select
    /// the model and start a turn. Failure emits a normalized terminal event.
    fn handshake(&mut self) -> bool {
        // (1) initialize — clientInfo + default capabilities only.
        let init_id = self.shared.next_id();
        let params = json!({
            "clientInfo": {
                "name": "iyagi",
                "title": null,
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {},
        });
        let Some(response) = self.request(init_id, "initialize", params) else {
            return false;
        };
        if response.get("error").is_some() {
            self.fail(
                MissionErrorCode::ProviderUnavailable,
                "initialize was rejected",
            );
            return false;
        }
        if !response.get("result").is_some_and(Value::is_object) {
            self.fail(
                MissionErrorCode::ProviderUnavailable,
                "initialize response result is not an object (v1/InitializeResponse.json)",
            );
            return false;
        }
        if response["result"]
            .get("userAgent")
            .and_then(Value::as_str)
            .is_none()
        {
            self.fail(
                MissionErrorCode::ProviderUnavailable,
                "initialize response lacks userAgent",
            );
            return false;
        }
        if self
            .peer
            .auth_scope()
            .is_some_and(|scope| !scope.verify_initialize(&response["result"]))
        {
            self.fail(
                MissionErrorCode::PolicyDenied,
                "Codex did not use the selected authentication home",
            );
            return false;
        }

        // (2) initialized notification (ClientNotification.json).
        if self.notify("initialized", json!({})) {
            return false;
        }

        let mut thread_config = None;
        if let Some(scope) = self.peer.auth_scope() {
            let id = self.shared.next_id();
            let Some(response) = self.request(
                id,
                "config/read",
                json!({"includeLayers":false,"cwd":self.cwd}),
            ) else {
                return false;
            };
            if response.get("error").is_some()
                || !scope.verify_config(&response["result"]["config"])
            {
                self.fail(
                    MissionErrorCode::PolicyDenied,
                    "Codex effective authentication or automation settings differ from the binding",
                );
                return false;
            }
            thread_config = Some(auth::AuthScope::thread_config(
                &response["result"]["config"],
                self.cfg.allow_network,
            ));
        }
        if let Some(key) = self.peer.take_api_key() {
            if self.cfg.auth_route != AuthRoute::ApiKey {
                self.fail(
                    MissionErrorCode::PolicyDenied,
                    "API credentials cannot be used for this authentication route",
                );
                return false;
            }
            let id = self.shared.next_id();
            let Some(response) = self.request(
                id,
                "account/login/start",
                json!({"type":"apiKey","apiKey":key.as_str()}),
            ) else {
                return false;
            };
            if response.get("error").is_some() || response["result"]["type"] != "apiKey" {
                self.fail(
                    MissionErrorCode::AuthRequired,
                    "Codex API key login was not confirmed",
                );
                return false;
            }
        }

        // (3) account/read — auth stays codex-owned; we only observe.
        let account_id = self.shared.next_id();
        let Some(response) = self.request(account_id, "account/read", json!({})) else {
            return false;
        };
        if response.get("error").is_some() {
            self.fail(MissionErrorCode::AuthRequired, "account/read was rejected");
            return false;
        }
        let account = response["result"].get("account");
        if !matches!(account, Some(Value::Object(_))) {
            self.fail(
                MissionErrorCode::AuthRequired,
                "app-server reports no signed-in account (account/read result.account is null)",
            );
            return false;
        }
        let expected = match self.cfg.auth_route {
            AuthRoute::Subscription => "chatgpt",
            AuthRoute::ApiKey => "apiKey",
            _ => "unsupported",
        };
        if response["result"]["account"]["type"].as_str() != Some(expected)
            || self.peer.auth_scope().is_some() && response["result"]["requiresOpenaiAuth"] != true
        {
            self.fail(
                MissionErrorCode::AuthRequired,
                "Codex account authentication does not match the binding",
            );
            return false;
        }
        self.authenticated = true;

        // (4) model/list — the binding's model must be advertised.
        let models_id = self.shared.next_id();
        let Some(response) = self.request(models_id, "model/list", json!({})) else {
            return false;
        };
        if response.get("error").is_some() {
            self.fail(
                MissionErrorCode::ModelUnavailable,
                "model/list was rejected",
            );
            return false;
        }
        let listed = response["result"]["data"]
            .as_array()
            .is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry.get("model").and_then(Value::as_str) == Some(self.cfg.model_id.as_str())
                        || entry.get("id").and_then(Value::as_str)
                            == Some(self.cfg.model_id.as_str())
                })
            });
        if !listed {
            self.fail(
                MissionErrorCode::ModelUnavailable,
                format!(
                    "model/list does not advertise the bound model {:?}",
                    self.cfg.model_id
                ),
            );
            return false;
        }

        // (5) thread/start — model/cwd/approvalPolicy/sandbox explicit
        // (ThreadStartParams; only schema-fixed fields, nothing guessed).
        let thread_id = self.shared.next_id();
        let mut thread_params = json!({
            "model": self.cfg.model_id,
            "cwd": self.cwd.to_string_lossy(),
            "approvalPolicy": self.cfg.approval_policy,
            "sandbox": self.cfg.sandbox,
            "modelProvider": self.cfg.provider_id,
        });
        if let Some(config) = thread_config {
            thread_params["config"] = config;
        }
        let Some(response) = self.request(thread_id, "thread/start", thread_params) else {
            return false;
        };
        if response.get("error").is_some() {
            self.fail(
                MissionErrorCode::ProviderUnavailable,
                "thread/start was rejected",
            );
            return false;
        }
        let Some(thread) = response["result"]["thread"]["id"].as_str() else {
            self.fail(
                MissionErrorCode::ProviderUnavailable,
                "thread/start response lacks result.thread.id",
            );
            return false;
        };
        if self.peer.auth_scope().is_some()
            && response["result"]["modelProvider"].as_str() != Some(self.cfg.provider_id.as_str())
        {
            self.fail(
                MissionErrorCode::PolicyDenied,
                "Codex thread selected a different provider",
            );
            return false;
        }
        // 10 §5: repository instruction files never reach an automated run.
        // Launch sets project_doc_max_bytes=0; confirm from what the thread
        // reports before any prompt is sent.
        if let Some(source) =
            workspace_instruction_source(&response["result"]["instructionSources"], &self.cwd)
        {
            self.fail(
                MissionErrorCode::PolicyDenied,
                format!("Codex loaded repository instructions for an automated run: {source}"),
            );
            return false;
        }
        *self
            .shared
            .provider_thread_id
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(thread.to_string());
        // 03 §2: requested vs observed model, both recorded.
        if let Some(observed) = response["result"].get("model").and_then(Value::as_str) {
            *self
                .shared
                .observed_model
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(observed.to_string());
            self.emit(AdapterEvent::ModelObserved {
                run_id: self.run_id.clone(),
                fencing_token: self.token,
                model: observed.into(),
            });
            if observed != self.cfg.model_id {
                self.fail(
                    MissionErrorCode::ModelUnavailable,
                    "Codex selected a different model than the binding",
                );
                return false;
            }
        } else if self.authenticated {
            self.fail(
                MissionErrorCode::ModelUnavailable,
                "Codex did not confirm the selected model",
            );
            return false;
        }

        if self.peer.auth_scope().is_some() {
            let request_id = self.shared.next_id();
            let Some(inventory) = self.request(
                request_id,
                "mcpServerStatus/list",
                json!({"threadId":thread,"limit":100}),
            ) else {
                return false;
            };
            let disabled = inventory.get("error").is_none()
                && inventory["result"]["nextCursor"].is_null()
                && inventory["result"]["data"]
                    .as_array()
                    .is_some_and(|servers| {
                        servers.iter().all(|server| {
                            server["runtimeStatus"] == "disabled"
                                && server["tools"]
                                    .as_object()
                                    .is_some_and(|tools| tools.is_empty())
                        })
                    });
            if !disabled {
                self.fail(
                    MissionErrorCode::PolicyDenied,
                    "Codex thread has unverified external tool connections",
                );
                return false;
            }
        }

        // (6) turn/start — input + the ProviderResult outputSchema; effort
        // rides the schema-pinned TurnStartParams.effort field only.
        let turn_req_id = self.shared.next_id();
        let mut turn_params = json!({
            "threadId": thread,
            "input": [
                { "type": "text", "text": self.prompt },
                {
                    "type": "text",
                    "text": format!(
                        "Context bundle artifact (daemon-issued): {}",
                        self.context_path.display()
                    ),
                },
            ],
            "outputSchema": task_result_output_schema(self.task_kind),
            "approvalPolicy": self.cfg.approval_policy,
            "sandboxPolicy": self.cfg.sandbox_policy(&self.cwd),
        });
        if let Some(effort) = self.cfg.effort.as_deref() {
            turn_params["effort"] = json!(effort);
        }
        // Set before any write: a failed/partial write may have submitted it.
        self.task_submitted = true;
        let Some(response) = self.request(turn_req_id, "turn/start", turn_params) else {
            return false;
        };
        if response.get("error").is_some() {
            self.fail(
                MissionErrorCode::ProviderUnavailable,
                "turn/start was rejected",
            );
            return false;
        }
        let Some(turn) = response["result"]["turn"]["id"].as_str() else {
            self.fail(
                MissionErrorCode::ProviderUnavailable,
                "turn/start response lacks result.turn.id",
            );
            return false;
        };
        *self
            .shared
            .provider_turn_id
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(turn.to_string());
        self.shared.turn_active.store(true, Ordering::Release);
        if let Err(error) = flush_interrupt(&self.shared, self.peer.as_ref()) {
            self.transport_failure(error.to_string());
            return false;
        }

        // Started carries the provider's exact ids (03 §1).
        self.emit(AdapterEvent::Started {
            run_id: self.run_id.clone(),
            fencing_token: self.token,
            provider_session_id: Some(thread.to_string()),
            provider_turn_id: Some(turn.to_string()),
        });
        true
    }

    /// Send a request and wait for its response, dispatching everything else
    /// inbound in the meantime. `None` = run already failed.
    fn request(&mut self, id: u64, method: &str, params: Value) -> Option<Value> {
        let message = json!({ "id": id, "method": method, "params": params });
        if self.peer.send(&message).is_err() {
            self.transport_failure(format!("{method} transport failed"));
            return None;
        }
        loop {
            match self.peer.recv() {
                PeerEvent::Message(value) => {
                    if is_response_to(&value, id) {
                        return Some(value);
                    }
                    if !self.dispatch_inbound(value) {
                        return None;
                    }
                }
                PeerEvent::Eof => {
                    self.transport_failure("app-server ended while awaiting a response".into());
                    return None;
                }
                PeerEvent::ConnectionLost => {
                    self.transport_failure(
                        "app-server connection lost while awaiting a response".into(),
                    );
                    return None;
                }
                PeerEvent::Overcap => {
                    self.fail(
                        MissionErrorCode::ResultInvalid,
                        "raw protocol line exceeded the 1 MiB cap (03 §2)",
                    );
                    return None;
                }
            }
        }
    }

    /// Send a notification; `true` = run already failed.
    fn notify(&mut self, method: &str, params: Value) -> bool {
        let message = json!({ "method": method, "params": params });
        if self.peer.send(&message).is_err() {
            self.transport_failure(format!("{method} transport failed"));
            return true;
        }
        false
    }

    /// Post-Started read loop until a terminal condition.
    fn read_loop(&mut self) {
        loop {
            match self.peer.recv() {
                PeerEvent::Message(value) => {
                    if !self.dispatch_inbound(value) {
                        return;
                    }
                }
                PeerEvent::Eof => {
                    if !self.shared.is_terminal() {
                        // Clean exit without turn/completed is never success.
                        self.fail(
                            MissionErrorCode::ResultInvalid,
                            "app-server exited 0 without turn/completed (E19)",
                        );
                    }
                    return;
                }
                PeerEvent::ConnectionLost => {
                    if !self.shared.is_terminal() {
                        self.emit(AdapterEvent::Disconnected {
                            run_id: self.run_id.clone(),
                            fencing_token: self.token,
                        });
                        self.shared.mark_terminal();
                    }
                    return;
                }
                PeerEvent::Overcap => {
                    if !self.shared.is_terminal() {
                        self.fail(
                            MissionErrorCode::ResultInvalid,
                            "raw protocol line exceeded the 1 MiB cap (03 §2)",
                        );
                    }
                    return;
                }
            }
        }
    }

    /// Route one inbound message. `false` = run terminal (stop reading).
    fn dispatch_inbound(&mut self, value: Value) -> bool {
        if value.get("id").is_some()
            && (value.get("result").is_some() || value.get("error").is_some())
        {
            if let Some(id) = value["id"].as_u64() {
                if let Some((turn, sender)) = self
                    .shared
                    .pending_steers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id)
                {
                    let receipt = if value.get("error").is_some() {
                        DeliveryReceipt::Rejected {
                            reason: "provider rejected the steer request",
                        }
                    } else if value["result"]["turnId"].as_str() == Some(turn.as_str()) {
                        DeliveryReceipt::Delivered {
                            provider_ref: Some(turn),
                        }
                    } else {
                        DeliveryReceipt::Unknown {
                            reason: "steer acknowledgement did not identify the expected turn",
                        }
                    };
                    let _ = sender.send(receipt);
                }
            }
            return !self.shared.is_terminal();
        }
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            return true; // unrecognized frame — display-only, keep reading
        };
        if value.get("id").is_some() {
            return self.handle_server_request(value);
        }
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        self.handle_notification(method, params)
    }

    /// Server→client request: approvals surface as Decision material with
    /// the EXACT provider request id; anything else is declined with a
    /// `-32601` response so the server never blocks on us.
    fn handle_server_request(&mut self, value: Value) -> bool {
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let wire_id = value.get("id").cloned().unwrap_or(Value::Null);
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        if APPROVAL_METHODS.contains(&method.as_str()) {
            // The id, params, and derived question are all untrusted
            // app-server output: bound each entry before it is stored or
            // re-emitted (03 §2 caps).
            let wire_id_bytes = wire_id.to_string().len();
            let question = if method == "item/fileChange/requestApproval" {
                self.file_changes.question(&params)
            } else {
                approval_question(&method, &params)
            };
            if let Some(message) = approval_entry_rejection(wire_id_bytes, question.len()) {
                // Answering keeps a legitimate server from waiting on a
                // decision we refuse to track; the provider sees an errored
                // request, never an approval.
                let _ = self.peer.send(&json!({
                    "id": wire_id,
                    "error": {
                        "code": -32601,
                        "message": message,
                    },
                }));
                return !self.shared.is_terminal();
            }
            let provider_request_id = wire_id_to_string(&wire_id);
            let entry_bytes =
                provider_request_id.len() + wire_id_bytes + method.len() + question.len();
            let mut pending = self
                .shared
                .pending_approvals
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if pending.contains_key(&provider_request_id) {
                // Duplicate id while still pending: no growth and no second
                // Decision event for the same request.
                return true;
            }
            if approval_set_overflow(&pending, entry_bytes) {
                drop(pending);
                self.fail(
                    MissionErrorCode::ResultInvalid,
                    "app-server approval requests exceeded the pending tracking cap (03 §2)",
                );
                return false;
            }
            pending.insert(
                provider_request_id.clone(),
                PendingApproval {
                    wire_id,
                    method,
                    question_bytes: question.len(),
                },
            );
            drop(pending);
            self.emit(AdapterEvent::ApprovalRequested {
                run_id: self.run_id.clone(),
                fencing_token: self.token,
                provider_request_id,
                question,
            });
            return !self.shared.is_terminal();
        }
        let _ = self.peer.send(&json!({
            "id": wire_id,
            "error": {
                "code": -32601,
                "message": format!("iyagi does not support server request {method}"),
            },
        }));
        true
    }

    fn handle_notification(&mut self, method: &str, params: Value) -> bool {
        match method {
            "account/rateLimits/updated" => {
                if let Some(observation) =
                    super::rate_limits::codex(&params, super::rate_limits::unix_millis())
                {
                    self.emit(AdapterEvent::RateLimited {
                        run_id: self.run_id.clone(),
                        fencing_token: self.token,
                        observation,
                    });
                }
                true
            }
            "account/updated" if self.authenticated => {
                let expected = if self.cfg.auth_route == AuthRoute::ApiKey {
                    "apikey"
                } else {
                    "chatgpt"
                };
                if params["authMode"].as_str() != Some(expected) {
                    self.fail(
                        MissionErrorCode::AuthRequired,
                        "Codex authentication changed during the run",
                    );
                    return false;
                }
                true
            }
            "turn/started" => {
                if let (Some(thread), Some(turn)) =
                    (params["threadId"].as_str(), params["turn"]["id"].as_str())
                {
                    *self
                        .shared
                        .provider_started_turn
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = Some((thread.into(), turn.into()));
                    if let Err(error) = flush_interrupt(&self.shared, self.peer.as_ref()) {
                        self.transport_failure(error.to_string());
                        return false;
                    }
                }
                true
            }
            "turn/completed" => {
                let turn = params.get("turn").cloned().unwrap_or(Value::Null);
                self.handle_turn_completed(&turn)
            }
            "item/agentMessage/delta" => {
                let Some(delta) = params.get("delta").and_then(Value::as_str) else {
                    return true;
                };
                if let Some(item) = params.get("itemId").and_then(Value::as_str) {
                    self.streamed_items.insert(item.to_string());
                }
                self.emit_activity_delta(delta.to_string());
                true
            }
            "item/started" => {
                if params["item"]["type"] == "fileChange" {
                    self.file_changes.observe(&params, Some(&params["item"]));
                }
                true
            }
            "item/fileChange/patchUpdated" => {
                self.file_changes.observe(&params, None);
                true
            }
            "item/completed" => {
                self.file_changes.completed(&params);
                let item = params.get("item").cloned().unwrap_or(Value::Null);
                let item_type = item.get("type").and_then(Value::as_str).unwrap_or("?");
                let item_id = item.get("id").and_then(Value::as_str).unwrap_or("?");
                if item_type == "agentMessage" && !self.streamed_items.contains(item_id) {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        self.emit_activity_line(text);
                    }
                } else if item_type != "agentMessage" {
                    // Compact lifecycle line only — tool payloads are
                    // artifact material, not main-IPC bodies (03 §2).
                    self.emit_activity_line(&format!("[{item_type}] completed"));
                }
                true
            }
            "thread/tokenUsage/updated" => {
                let last = &params["tokenUsage"]["last"];
                self.emit(AdapterEvent::Usage {
                    run_id: self.run_id.clone(),
                    fencing_token: self.token,
                    input_tokens: positive_u64(last.get("inputTokens")),
                    output_tokens: positive_u64(last.get("outputTokens")),
                    // Codex reports no dollar cost here — never fabricate.
                    cost_usd_micros: None,
                });
                true
            }
            "model/rerouted" => {
                if let Some(to) = params.get("toModel").and_then(Value::as_str) {
                    *self
                        .shared
                        .observed_model
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = Some(to.to_string());
                    self.emit(AdapterEvent::ModelObserved {
                        run_id: self.run_id.clone(),
                        fencing_token: self.token,
                        model: to.into(),
                    });
                    if to != self.cfg.model_id {
                        self.fail(
                            MissionErrorCode::ModelUnavailable,
                            "Codex rerouted away from the bound model",
                        );
                        return false;
                    }
                }
                true
            }
            "error" => {
                let will_retry = params
                    .get("willRetry")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if will_retry {
                    self.emit(AdapterEvent::Activity {
                        run_id: self.run_id.clone(),
                        fencing_token: self.token,
                        chunk: "[provider error, will retry]".into(),
                    });
                    true
                } else {
                    let code = map_codex_error(params["error"].get("codexErrorInfo"));
                    self.fail(code, "Codex provider request failed");
                    false
                }
            }
            "thread/status/changed" => {
                if params
                    .get("status")
                    .and_then(|s| s.get("type"))
                    .and_then(Value::as_str)
                    == Some("systemError")
                    && !self.shared.is_terminal()
                {
                    self.fail(
                        MissionErrorCode::ProviderUnavailable,
                        "thread entered systemError state",
                    );
                    return false;
                }
                true
            }
            // item/reasoning/*: hidden reasoning is never captured (03 §3).
            // Everything else (fs/changed, thread/name/updated, …) is
            // display-only for this adapter.
            _ => true,
        }
    }

    /// turn/completed classification (03 §3 step 7: status + structured
    /// result together; partial text tails never infer completion).
    fn handle_turn_completed(&mut self, turn: &Value) -> bool {
        let status = turn.get("status").and_then(Value::as_str).unwrap_or("?");
        match status {
            "completed" => {
                let items = turn.get("items").and_then(Value::as_array);
                let final_text = items.and_then(|items| {
                    items
                        .iter()
                        .rev()
                        .filter(|item| {
                            item.get("type").and_then(Value::as_str) == Some("agentMessage")
                        })
                        .find(|item| {
                            // Prefer an explicit final_answer phase; fall back
                            // to the last agentMessage (phase unknown is
                            // legitimate per MessagePhase docs).
                            item.get("phase").and_then(Value::as_str) != Some("commentary")
                        })
                        .and_then(|item| item.get("text").and_then(Value::as_str))
                });
                let Some(final_text) = final_text else {
                    self.emit(AdapterEvent::InvalidResult {
                        run_id: self.run_id.clone(),
                        fencing_token: self.token,
                        code: MissionErrorCode::ResultInvalid,
                        message: "turn/completed carried no final assistant message".into(),
                        rejected_result: None,
                    });
                    self.shared.mark_terminal();
                    return false;
                };
                match serde_json::from_str(final_text).and_then(super::parse_provider_result) {
                    Ok(result) => {
                        self.emit(AdapterEvent::Result {
                            run_id: self.run_id.clone(),
                            fencing_token: self.token,
                            result,
                        });
                        self.shared.mark_terminal();
                        false
                    }
                    Err(_) => {
                        self.emit(AdapterEvent::InvalidResult {
                            run_id: self.run_id.clone(),
                            fencing_token: self.token,
                            code: MissionErrorCode::ResultInvalid,
                            message: "final assistant message is not a valid ProviderResult".into(),
                            rejected_result: Some(final_text.to_owned()),
                        });
                        self.shared.mark_terminal();
                        false
                    }
                }
            }
            "failed" => {
                let code = map_codex_error(turn["error"].get("codexErrorInfo"));
                self.fail(code, "Codex turn failed");
                false
            }
            "interrupted" => {
                if self.shared.interrupt_requested.load(Ordering::Acquire) {
                    // Our own interrupt confirmed at protocol level; the
                    // cancel-owning actor already settled the run state.
                    self.shared.mark_terminal();
                } else {
                    self.fail(
                        MissionErrorCode::OutcomeUnknown,
                        "provider interrupted the turn without a cancel request",
                    );
                }
                false
            }
            _ => true, // inProgress or unknown — keep reading
        }
    }

    fn drive(mut self) {
        struct ClosePeer(Arc<dyn ProtocolPeer>);
        impl Drop for ClosePeer {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _owner = ClosePeer(self.peer.clone());
        if self.handshake() {
            self.read_loop();
        }
    }
}

/// A JSON-RPC response addressed to our request `id`?
fn is_response_to(value: &Value, id: u64) -> bool {
    (value.get("result").is_some() || value.get("error").is_some())
        && match value.get("id") {
            Some(Value::Number(number)) => {
                number.as_u64() == Some(id) || number.as_i64() == Some(id as i64)
            }
            _ => false,
        }
}

/// The EXACT provider request id as a stable string key (numbers decimal,
/// strings verbatim).
fn wire_id_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Per-entry bound for one approval request from untrusted app-server
/// output: `Some(message)` = reject with the `-32601` protocol error (03 §2
/// caps). Ids and questions beyond the bound are never stored or re-emitted.
fn approval_entry_rejection(wire_id_bytes: usize, question_bytes: usize) -> Option<String> {
    if wire_id_bytes > MAX_APPROVAL_ID_BYTES {
        Some(format!(
            "iyagi rejects approval request ids over {MAX_APPROVAL_ID_BYTES} bytes"
        ))
    } else if question_bytes > MAX_APPROVAL_QUESTION_BYTES {
        Some(format!(
            "iyagi rejects approval questions over {MAX_APPROVAL_QUESTION_BYTES} bytes"
        ))
    } else {
        None
    }
}

/// Pending-set bound: `true` when a fresh entry of `entry_bytes` (id, wire
/// id, method, and question bytes) may not be tracked because the count or
/// byte cap is exhausted (03 §2 caps; the caller fails the run once, like
/// the raw line cap).
fn approval_set_overflow(pending: &HashMap<String, PendingApproval>, entry_bytes: usize) -> bool {
    if pending.len() >= MAX_PENDING_APPROVALS {
        return true;
    }
    let stored: usize = pending
        .iter()
        .map(|(key, entry)| {
            key.len() + entry.wire_id.to_string().len() + entry.method.len() + entry.question_bytes
        })
        .sum();
    stored + entry_bytes > MAX_PENDING_APPROVAL_BYTES
}

/// Positive integer as u64; negatives/null stay `None` (never fabricated).
fn positive_u64(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_i64)
        .and_then(|v| u64::try_from(v).ok())
}

/// codexErrorInfo → MissionErrorCode (camelCase string forms and the
/// object forms both map conservatively).
fn map_codex_error(info: Option<&Value>) -> MissionErrorCode {
    match info.and_then(Value::as_str) {
        Some("unauthorized") => MissionErrorCode::AuthRequired,
        Some("usageLimitExceeded") | Some("rateLimitExceeded") => {
            MissionErrorCode::ProviderRateLimited
        }
        Some("sessionBudgetExceeded") => MissionErrorCode::BudgetExceeded,
        Some("contextWindowExceeded") => MissionErrorCode::ContextTooLarge,
        _ => MissionErrorCode::ProviderUnavailable,
    }
}

/// First thread instruction source inside the run workspace (10 §5). With
/// `project_doc_max_bytes=0` Codex must report none there; user-level
/// sources outside the workspace are allowed. Paths are compared as reported
/// and canonicalized so aliases such as `/var` → `/private/var` still match.
fn workspace_instruction_source(sources: &Value, workspace: &Path) -> Option<String> {
    let canonical_workspace = std::fs::canonicalize(workspace).ok();
    sources
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find(|source| {
            let path = Path::new(source);
            path.starts_with(workspace)
                || canonical_workspace.as_ref().is_some_and(|root| {
                    std::fs::canonicalize(path)
                        .map_or_else(|_| path.starts_with(root), |real| real.starts_with(root))
                })
        })
        .map(str::to_owned)
}

/// Human question for an approval request, built only from schema-guaranteed
/// params fields.
fn approval_question(method: &str, params: &Value) -> String {
    match method {
        "item/commandExecution/requestApproval" => {
            let command = params
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("(unknown command)");
            let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or("?");
            let kind = params
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("command");
            format!("command approval ({kind}): {command} @ {cwd}")
        }
        "item/fileChange/requestApproval" => {
            let reason = params
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("file change");
            format!("file change approval: {reason}")
        }
        "execCommandApproval" => {
            let command = params
                .get("command")
                .and_then(Value::as_array)
                .map(|argv| {
                    argv.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_else(|| "(unknown command)".into());
            format!("exec approval: {command}")
        }
        "applyPatchApproval" => {
            let count = params
                .get("fileChanges")
                .and_then(Value::as_object)
                .map(|map| map.len())
                .unwrap_or(0);
            format!("patch approval: {count} file change(s)")
        }
        _ => format!("approval request ({method})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_version_parses_the_installed_shape() {
        assert_eq!(
            parse_cli_version("codex-cli 0.153.4\n"),
            Some("0.153.4".into())
        );
        assert_eq!(
            parse_cli_version("codex-cli 1.2.3-rc.1\n"),
            Some("1.2.3-rc.1".into())
        );
        assert_eq!(parse_cli_version("garbage"), None);
        assert_eq!(parse_cli_version(""), None);
    }

    #[test]
    fn repository_instruction_sources_inside_the_workspace_are_detected() {
        let root = tempfile::tempdir().expect("temp");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let inside = workspace.join("AGENTS.md");
        std::fs::write(&inside, "repository rules").expect("write");
        let outside = root.path().join("user-AGENTS.md");
        std::fs::write(&outside, "user rules").expect("write");
        let inside_text = inside.to_string_lossy().to_string();
        let outside_text = outside.to_string_lossy().to_string();

        assert_eq!(workspace_instruction_source(&json!([]), &workspace), None);
        assert_eq!(workspace_instruction_source(&Value::Null, &workspace), None);
        assert_eq!(
            workspace_instruction_source(&json!([outside_text.clone()]), &workspace),
            None
        );
        assert_eq!(
            workspace_instruction_source(&json!([outside_text, inside_text.clone()]), &workspace),
            Some(inside_text)
        );
        // A canonical alias of the workspace path still matches.
        let canonical = std::fs::canonicalize(&inside)
            .expect("canonical")
            .to_string_lossy()
            .to_string();
        assert_eq!(
            workspace_instruction_source(&json!([canonical.clone()]), &workspace),
            Some(canonical)
        );
    }

    #[test]
    fn program_exists_stats_paths_without_execution() {
        let file = tempfile::NamedTempFile::new().expect("temp");
        let path = file.path().to_string_lossy().to_string();
        assert!(program_exists(&path));
        assert!(!program_exists(&format!("{path}.missing")));
        assert!(!program_exists("definitely-not-a-real-program-xyz"));
    }

    #[test]
    fn binding_config_derives_conservative_sandbox() {
        let mut binding = crate::agent_runtime::fake::fake_binding();
        binding.runtime = RuntimeKind::Codex;
        binding.program = "C:/tools/codex.exe".into();
        binding.provider_id = "openai".into();
        binding.model_id = "gpt-5.1-codex".into();
        binding.capabilities.scoped_write.supported = false;
        let cfg = CodexBindingConfig::from_binding(&binding).expect("derives");
        assert_eq!(cfg.sandbox, "read-only");
        assert_eq!(cfg.approval_policy, "untrusted");
        binding.capabilities.scoped_write.supported = true;
        let cfg = CodexBindingConfig::from_binding(&binding).expect("derives");
        assert_eq!(
            cfg.sandbox, "read-only",
            "capability evidence cannot grant write access"
        );
        binding.runtime = RuntimeKind::Fake;
        assert!(CodexBindingConfig::from_binding(&binding).is_err());
    }

    /// Activity chunks are concatenated verbatim, so a lifecycle note landing
    /// right after a streamed message used to read as one word:
    /// `… 나누겠습니다.[userMessage] completed`. A whole line opens its own.
    #[test]
    fn a_whole_activity_line_never_fuses_onto_the_text_before_it() {
        assert_eq!(
            activity_line("[userMessage] completed", false),
            "\n[userMessage] completed\n"
        );
        assert_eq!(
            activity_line("[userMessage] completed", true),
            "[userMessage] completed\n"
        );
        // A message that already ends its line is not padded with a blank one.
        assert_eq!(activity_line("plan ready\n", true), "plan ready\n");
        // Nothing to say stays nothing — never a bare newline.
        assert_eq!(activity_line("\n", false), "");
        assert_eq!(activity_line("", true), "");
    }

    /// The approval policy follows the authority the task was given, not the
    /// binding: on a read-only run there is nothing left to approve, and a
    /// per-command prompt there stops the Lead four times (`pwd`, `git
    /// status`, two `rg` calls) before it has read a single file.
    #[test]
    fn run_config_takes_its_approval_policy_from_the_task_authority() {
        let mut binding = crate::agent_runtime::fake::fake_binding();
        binding.runtime = RuntimeKind::Codex;
        binding.program = "/usr/local/bin/codex".into();
        binding.provider_id = "openai".into();
        binding.model_id = "gpt-5.1-codex".into();
        let dir = tempfile::tempdir().expect("workspace");
        let mut run = RunStart {
            task_kind: None,
            mission_id: Id::generate(),
            owner_daemon_id: Id::generate(),
            run_id: Id::generate(),
            fencing_token: 1,
            binding,
            workspace_access: crate::agent_runtime::WorkspaceAccess::ReadOnly,
            allow_network: false,
            context_path: dir.path().into(),
            workspace: Some(dir.path().into()),
            prompt_stdin: "prompt".into(),
        };
        let read_only = CodexBindingConfig::from_run(&run).expect("read-only run");
        assert_eq!(read_only.sandbox, "read-only");
        assert_eq!(read_only.approval_policy, "never");
        run.workspace_access = crate::agent_runtime::WorkspaceAccess::Write;
        let write = CodexBindingConfig::from_run(&run).expect("write run");
        assert_eq!(write.sandbox, "workspace-write");
        assert_eq!(write.approval_policy, "on-request");
    }

    #[test]
    fn probe_claims_no_capability_without_live_evidence() {
        let mut binding = crate::agent_runtime::fake::fake_binding();
        binding.runtime = RuntimeKind::Codex;
        binding.provider_id = "openai".into();
        let missing = std::env::temp_dir().join("iyagi-o08-no-codex.exe");
        let _ = std::fs::remove_file(&missing);
        binding.program = missing.to_string_lossy().into_owned();
        let probe = CodexAdapter::probe(&binding);
        assert!(!probe.installed);
        assert_eq!(
            probe.version, None,
            "no --version spawn for a missing program"
        );
        assert!(!probe.capabilities.events.supported);
        assert!(!probe.capabilities.cancel.supported);
        assert_eq!(
            probe.capabilities.events.reason_code.as_deref(),
            Some("no_compatibility_evidence")
        );
    }

    #[test]
    fn approval_decisions_map_both_vocabularies() {
        assert_eq!(
            map_approval_decision("item/commandExecution/requestApproval", "accept"),
            Some(json!("accept"))
        );
        assert_eq!(
            map_approval_decision("execCommandApproval", "approve"),
            Some(json!("approved"))
        );
        assert_eq!(
            map_approval_decision("applyPatchApproval", "deny"),
            Some(json!("denied"))
        );
        assert_eq!(
            map_approval_decision("item/fileChange/requestApproval", "maybe"),
            None
        );
    }

    #[test]
    fn approval_entry_caps_reject_oversized_ids_and_questions() {
        let fat_id = json!("x".repeat(MAX_APPROVAL_ID_BYTES + 1));
        assert!(
            approval_entry_rejection(fat_id.to_string().len(), "exec approval: ls".len()).is_some()
        );
        let fat_question = "x".repeat(MAX_APPROVAL_QUESTION_BYTES + 1);
        assert!(approval_entry_rejection(json!(7).to_string().len(), fat_question.len()).is_some());
        assert_eq!(
            approval_entry_rejection(
                json!(7).to_string().len(),
                "file change approval: edit".len()
            ),
            None
        );
    }

    /// Regression: a 4 KiB question cap auto-rejected nearly every real
    /// `item/fileChange/requestApproval` (the question embeds the retained
    /// diff evidence), so writer runs under `approvalPolicy: untrusted`
    /// could not apply non-trivial patches.
    #[test]
    fn file_change_approval_with_a_multi_kilobyte_diff_is_not_rejected() {
        let diff: String = (0..400)
            .map(|i| format!("+    let generated_line_{i} = {i};\n"))
            .collect();
        assert!(diff.len() > 10 * 1024, "fixture diff is ~10+ KiB");
        // Near the 64 KiB evidence bound approvals.rs retains per item.
        let largest = "+x\n".repeat(15_000);
        for diff in [diff, largest] {
            let started = json!({
                "threadId": "thread",
                "turnId": "turn",
                "item": {
                    "id": "patch-1",
                    "type": "fileChange",
                    "changes": [{"path": "src/login.rs", "kind": {"type": "update"}, "diff": diff}]
                }
            });
            let request = json!({
                "threadId": "thread",
                "turnId": "turn",
                "itemId": "patch-1",
                "reason": "apply the login patch",
                "grantRoot": null
            });
            let mut changes = approvals::FileChanges::default();
            changes.observe(&started, Some(&started["item"]));
            let question = changes.question(&request);
            let details: Value = serde_json::from_str(&question).expect("question JSON");
            assert_eq!(details["details_available"], true, "evidence retained");
            assert_eq!(details["changes"][0]["diff"], diff.as_str());
            assert!(question.len() > 4 * 1024);
            assert_eq!(
                approval_entry_rejection(json!(41).to_string().len(), question.len()),
                None,
                "a real file-change approval must surface as a Decision"
            );
        }
    }

    #[test]
    fn approval_set_cap_trips_on_count_and_stored_bytes() {
        let mut pending = HashMap::new();
        for i in 0..MAX_PENDING_APPROVALS {
            pending.insert(
                i.to_string(),
                PendingApproval {
                    wire_id: json!(i),
                    method: "execCommandApproval".into(),
                    question_bytes: "exec approval: ls".len(),
                },
            );
        }
        let fresh = "255".len() + json!(255).to_string().len() + "execCommandApproval".len();
        assert!(approval_set_overflow(&pending, fresh));
        pending.remove("0");
        assert!(!approval_set_overflow(&pending, fresh));

        // Question bytes count toward the byte cap: a handful of
        // maximum-size approvals stay trackable, a flood of them does not.
        let method = "item/fileChange/requestApproval";
        let fresh_max = "999".len() + json!(999).to_string().len() + method.len();
        let fresh_max = fresh_max + MAX_APPROVAL_QUESTION_BYTES;
        let mut fat = HashMap::new();
        while !approval_set_overflow(&fat, fresh_max) {
            let key = fat.len().to_string();
            fat.insert(
                key.clone(),
                PendingApproval {
                    wire_id: json!(key),
                    method: method.into(),
                    question_bytes: MAX_APPROVAL_QUESTION_BYTES,
                },
            );
            assert!(fat.len() < MAX_PENDING_APPROVALS, "byte cap must trip");
        }
        assert!(fat.len() >= 4, "{} max-size approvals fit", fat.len());
        assert!(fat.len() < 16, "{} max-size approvals fit", fat.len());
        assert!(approval_set_overflow(&fat, fresh_max));
    }

    /// Pin the staging window so a slow test machine cannot roll it
    /// mid-test: a future anchor never elapses and the quota stays spent.
    fn pin_exhausted_window(fanout: &BoundedFanout) {
        let mut staging = fanout.staging.lock().unwrap_or_else(|p| p.into_inner());
        staging.window_start = Some(Instant::now() + Duration::from_secs(3_600));
        staging.sent_in_window = FANOUT_WINDOW_EVENTS;
    }

    #[test]
    fn fanout_stages_bursts_and_flushes_before_critical_events() {
        let fanout = Arc::new(BoundedFanout::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        fanout.add_subscriber(tx);
        let run = Id::generate();
        // Far above the per-window quota: the excess stages (coalesced per
        // run) instead of queueing one subscriber event per chunk.
        let mut published = 0;
        for i in 0..1_000 {
            fanout.publish(AdapterEvent::Activity {
                run_id: run.clone(),
                fencing_token: 7,
                chunk: format!("chunk-{i} "),
            });
            published += 1;
            if published == FANOUT_WINDOW_EVENTS {
                pin_exhausted_window(&fanout);
            }
        }
        fanout.publish(AdapterEvent::Failed {
            run_id: run.clone(),
            fencing_token: 7,
            code: MissionErrorCode::ProviderUnavailable,
            message: "end".into(),
        });
        let mut text = String::new();
        let mut failed = false;
        let mut events: usize = 0;
        while let Some(event) = rx.try_recv().ok() {
            events += 1;
            match event {
                AdapterEvent::Activity { chunk, .. } => text.push_str(&chunk),
                AdapterEvent::Failed { .. } => failed = true,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(failed, "the order-critical event force-flushes and follows");
        // Quota sends one event each; the staged remainder coalesces into a
        // single Activity for the run; the Failed event closes the stream.
        assert_eq!(events, FANOUT_WINDOW_EVENTS as usize + 2);
        for i in 0..1_000 {
            assert!(
                text.contains(&format!("chunk-{i} ")),
                "chunk {i} missing ({} bytes delivered)",
                text.len()
            );
        }
    }

    #[test]
    fn fanout_sheds_oldest_activity_above_the_byte_budget_with_a_marker() {
        let fanout = Arc::new(BoundedFanout::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        fanout.add_subscriber(tx);
        let run_a = Id::generate();
        let run_b = Id::generate();
        pin_exhausted_window(&fanout);
        let big = "x".repeat(200_000);
        fanout.publish(AdapterEvent::Activity {
            run_id: run_a.clone(),
            fencing_token: 3,
            chunk: big.clone(),
        });
        // A different run cannot merge into run_a's tail entry, so staging
        // both exceeds the 256 KiB budget and sheds the oldest.
        fanout.publish(AdapterEvent::Activity {
            run_id: run_b.clone(),
            fencing_token: 3,
            chunk: big.clone(),
        });
        fanout.publish(AdapterEvent::Disconnected {
            run_id: run_b.clone(),
            fencing_token: 3,
        });
        let mut received = Vec::new();
        while let Some(event) = rx.try_recv().ok() {
            received.push(event);
        }
        let marker_index = received
            .iter()
            .position(|event| {
                matches!(event, AdapterEvent::Activity { chunk, .. } if chunk.contains("shed 1 activity chunk"))
            })
            .expect("one drop marker for the episode");
        let survivor_index = received
            .iter()
            .position(|event| {
                matches!(event, AdapterEvent::Activity { run_id, chunk, .. }
                    if run_id == &run_b && chunk.len() == big.len())
            })
            .expect("the newest chunk survives");
        let disconnected_index = received
            .iter()
            .position(|event| matches!(event, AdapterEvent::Disconnected { .. }))
            .expect("terminal event delivered");
        assert_eq!(received.len(), 3);
        assert!(marker_index < survivor_index);
        assert!(survivor_index < disconnected_index);
    }

    #[test]
    fn fanout_merges_staged_usage_by_maximum() {
        let fanout = Arc::new(BoundedFanout::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        fanout.add_subscriber(tx);
        let run = Id::generate();
        pin_exhausted_window(&fanout);
        fanout.publish(AdapterEvent::Usage {
            run_id: run.clone(),
            fencing_token: 1,
            input_tokens: Some(1_000),
            output_tokens: Some(50),
            cost_usd_micros: None,
        });
        fanout.publish(AdapterEvent::Usage {
            run_id: run.clone(),
            fencing_token: 1,
            input_tokens: Some(500),
            output_tokens: Some(60),
            cost_usd_micros: Some(200),
        });
        fanout.publish(AdapterEvent::Disconnected {
            run_id: run.clone(),
            fencing_token: 1,
        });
        let mut usage: Option<(Option<u64>, Option<u64>, Option<u64>)> = None;
        let mut usage_events = 0;
        let mut disconnected = false;
        while let Some(event) = rx.try_recv().ok() {
            match event {
                AdapterEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cost_usd_micros,
                    ..
                } => {
                    usage_events += 1;
                    usage = Some((input_tokens, output_tokens, cost_usd_micros));
                }
                AdapterEvent::Disconnected { .. } => disconnected = true,
                AdapterEvent::Activity { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(disconnected, "terminal event force-flushes the merge");
        assert_eq!(usage_events, 1, "staged usage coalesces into one event");
        assert_eq!(usage, Some((Some(1_000), Some(60), Some(200))));
    }

    #[test]
    fn fanout_direct_sends_are_bounded_in_bytes_per_window() {
        let fanout = Arc::new(BoundedFanout::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        fanout.add_subscriber(tx);
        let run = Id::generate();
        {
            // Pin a fresh window open: the event quota stays untouched.
            let mut staging = fanout.staging.lock().unwrap_or_else(|p| p.into_inner());
            staging.window_start = Some(Instant::now() + Duration::from_secs(3_600));
        }
        let chunk = "y".repeat(100 * 1024);
        for _ in 0..4 {
            fanout.publish(AdapterEvent::Activity {
                run_id: run.clone(),
                fencing_token: 5,
                chunk: chunk.clone(),
            });
        }
        let mut direct = 0;
        while let Ok(event) = rx.try_recv() {
            assert!(matches!(event, AdapterEvent::Activity { .. }));
            direct += 1;
        }
        // Far below the 24-event quota, bytes decide: the third send
        // crosses the 256 KiB window, so the fourth chunk stages.
        assert_eq!(direct, 3);
        fanout.publish(AdapterEvent::Disconnected {
            run_id: run.clone(),
            fencing_token: 5,
        });
        let mut staged_bytes = 0;
        let mut disconnected = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                AdapterEvent::Activity { chunk, .. } => staged_bytes += chunk.len(),
                AdapterEvent::Disconnected { .. } => disconnected = true,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(disconnected, "staged text flushes first");
        assert_eq!(staged_bytes, chunk.len());
    }

    #[test]
    fn fanout_stops_merging_at_the_staged_budget_so_old_text_sheds() {
        let fanout = Arc::new(BoundedFanout::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        fanout.add_subscriber(tx);
        let run = Id::generate();
        pin_exhausted_window(&fanout);
        // Consecutive chunks of one run must not merge into a single
        // never-shed entry that grows past the budget while the quota is
        // spent.
        for _ in 0..8 {
            fanout.publish(AdapterEvent::Activity {
                run_id: run.clone(),
                fencing_token: 2,
                chunk: "z".repeat(100 * 1024),
            });
        }
        {
            let staging = fanout.staging.lock().unwrap_or_else(|p| p.into_inner());
            assert!(staging.activity_bytes <= FANOUT_STAGED_BYTES);
            assert!(staging.dropped_chunks > 0, "older text sheds");
        }
        fanout.publish(AdapterEvent::Disconnected {
            run_id: run.clone(),
            fencing_token: 2,
        });
        let received: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        match received.first() {
            Some(AdapterEvent::Activity { chunk, .. }) => {
                assert!(chunk.contains("activity chunk(s)"), "got {chunk}")
            }
            other => panic!("expected the drop marker first, got {other:?}"),
        }
        assert!(matches!(
            received.last(),
            Some(AdapterEvent::Disconnected { .. })
        ));
    }

    #[test]
    fn fanout_never_merges_observations_across_fencing_tokens() {
        let fanout = Arc::new(BoundedFanout::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        fanout.add_subscriber(tx);
        let run = Id::generate();
        pin_exhausted_window(&fanout);
        for token in [1, 2] {
            fanout.publish(AdapterEvent::Usage {
                run_id: run.clone(),
                fencing_token: token,
                input_tokens: Some(token * 100),
                output_tokens: None,
                cost_usd_micros: None,
            });
        }
        fanout.publish(AdapterEvent::Disconnected {
            run_id: run.clone(),
            fencing_token: 2,
        });
        let mut tokens = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event {
                AdapterEvent::Usage {
                    fencing_token,
                    input_tokens,
                    ..
                } => {
                    assert_eq!(input_tokens, Some(fencing_token * 100));
                    tokens.push(fencing_token);
                }
                other => assert!(matches!(other, AdapterEvent::Disconnected { .. })),
            }
        }
        tokens.sort_unstable();
        assert_eq!(tokens, [1, 2], "each token keeps its own observation");
    }

    #[test]
    fn output_schema_is_requested_with_every_turn() {
        let schema = provider_result_output_schema();
        assert_eq!(
            schema["properties"]["result"]["anyOf"]
                .as_array()
                .map(Vec::len),
            Some(result_schema::RESULT_KINDS.len())
        );
    }
}
