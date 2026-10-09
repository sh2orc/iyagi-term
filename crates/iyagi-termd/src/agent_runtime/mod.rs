//! # Runtime/provider adapter port (ticket O07, docs/orchestration/03-adapters.md §1)
//!
//! Daemon-internal interface every runtime adapter (Codex/Claude/OpenCode/
//! Fake) implements. Adapters normalize provider traffic into the
//! [`AdapterEvent`] set and never touch DB/UI. The event stream enforces the
//! two no-state-corruption rules of 03 §1:
//! * every event carries the run's DB fencing token — events with a stale
//!   token are counted and DROPPED ([`FencingGate`], E11);
//! * deltas after a terminal event (Result/Failed/Disconnected) are counted
//!   and dropped, never applied.
//!
//! Receipts distinguish accepted vs confirmed (03 §1: `bool` returns are
//! forbidden — reasons ride with the enum).

pub mod capability_evidence;
pub mod claude;
pub mod codex;
pub mod detection;
pub mod fake;
pub mod installation;
pub mod installation_identity;
pub mod local_probe;
pub mod model_catalog;
pub mod opencode;
pub mod rate_limits;
pub mod retry;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use term_contracts::mission::types::{Binding, Id, ProviderResult};
use term_contracts::mission::MissionErrorCode;
use tokio::sync::mpsc;

/// Requested task access is separate from evidence about adapter support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceAccess {
    ReadOnly,
    Write,
}

/// Structured-output APIs require an object root, so the provider receives a
/// closed `result` envelope. Plan/Review use a versioned nullable envelope that
/// rejects inactive payloads before decoding the canonical DTO. Retained direct
/// DTOs and ordinary envelopes still pass the same strict ProviderResult parser.
pub(crate) fn parse_provider_result(
    value: serde_json::Value,
) -> serde_json::Result<ProviderResult> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Envelope {
        #[serde(default)]
        format: Option<String>,
        result: serde_json::Value,
    }
    if value.get("result").is_some() {
        let envelope: Envelope = serde_json::from_value(value)?;
        match envelope.format.as_deref() {
            None => serde_json::from_value(envelope.result),
            Some("iyagi-result-v2") => codex::parse_flat_result(envelope.result),
            Some(_) => Err(serde::de::Error::custom("unknown result envelope format")),
        }
    } else {
        serde_json::from_value(value)
    }
}

/// One run launch as the engine hands it to the adapter (03 §1 `start`).
#[derive(Debug, Clone)]
pub struct RunStart {
    /// Server-owned task kind; never inferred from model text.
    pub task_kind: Option<term_contracts::mission::types::TaskKind>,
    pub mission_id: Id,
    pub owner_daemon_id: Id,
    pub workspace_access: WorkspaceAccess,
    pub allow_network: bool,
    pub run_id: Id,
    /// DB fencing token of the starting actor (02 §7).
    pub fencing_token: u64,
    pub binding: Binding,
    /// Daemon-issued context artifact path (never raw provider text).
    pub context_path: PathBuf,
    pub workspace: Option<PathBuf>,
    /// Prompt body piped to the child's stdin (never a shell string).
    pub prompt_stdin: String,
}

/// Normalized adapter events (03 §1). Result/Failed/Disconnected are
/// terminal for the run; later deltas are display-log material only.
#[derive(Debug, Clone)]
pub enum AdapterEvent {
    Started {
        run_id: Id,
        fencing_token: u64,
        provider_session_id: Option<String>,
        provider_turn_id: Option<String>,
    },
    Activity {
        run_id: Id,
        fencing_token: u64,
        chunk: String,
    },
    ModelObserved {
        run_id: Id,
        fencing_token: u64,
        model: String,
    },
    ApprovalRequested {
        run_id: Id,
        fencing_token: u64,
        provider_request_id: String,
        question: String,
    },
    Usage {
        run_id: Id,
        fencing_token: u64,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cost_usd_micros: Option<u64>,
    },
    RateLimited {
        run_id: Id,
        fencing_token: u64,
        observation: term_contracts::mission::types::RateLimitObservation,
    },
    Result {
        run_id: Id,
        fencing_token: u64,
        result: ProviderResult,
    },
    Failed {
        run_id: Id,
        fencing_token: u64,
        code: MissionErrorCode,
        message: String,
    },
    // A completed provider answer rejected by the result schema/plan validator.
    // Transport, authentication and local preparation errors never use this.
    InvalidResult {
        run_id: Id,
        fencing_token: u64,
        code: MissionErrorCode,
        message: String,
        rejected_result: Option<String>,
    },
    // Only emitted at a boundary that has not submitted a task request.
    FailedBeforeSubmission {
        run_id: Id,
        fencing_token: u64,
        code: MissionErrorCode,
        message: String,
        observed_at_unix_ms: u64,
        retry_after_unix_ms: Option<u64>,
    },
    Disconnected {
        run_id: Id,
        fencing_token: u64,
    },
}

impl AdapterEvent {
    pub fn run_id(&self) -> &Id {
        match self {
            AdapterEvent::Started { run_id, .. }
            | AdapterEvent::ModelObserved { run_id, .. }
            | AdapterEvent::Activity { run_id, .. }
            | AdapterEvent::ApprovalRequested { run_id, .. }
            | AdapterEvent::Usage { run_id, .. }
            | AdapterEvent::RateLimited { run_id, .. }
            | AdapterEvent::Result { run_id, .. }
            | AdapterEvent::Failed { run_id, .. }
            | AdapterEvent::InvalidResult { run_id, .. }
            | AdapterEvent::FailedBeforeSubmission { run_id, .. }
            | AdapterEvent::Disconnected { run_id, .. } => run_id,
        }
    }

    pub fn fencing_token(&self) -> u64 {
        match self {
            AdapterEvent::Started { fencing_token, .. }
            | AdapterEvent::ModelObserved { fencing_token, .. }
            | AdapterEvent::Activity { fencing_token, .. }
            | AdapterEvent::ApprovalRequested { fencing_token, .. }
            | AdapterEvent::Usage { fencing_token, .. }
            | AdapterEvent::RateLimited { fencing_token, .. }
            | AdapterEvent::Result { fencing_token, .. }
            | AdapterEvent::Failed { fencing_token, .. }
            | AdapterEvent::InvalidResult { fencing_token, .. }
            | AdapterEvent::FailedBeforeSubmission { fencing_token, .. }
            | AdapterEvent::Disconnected { fencing_token, .. } => *fencing_token,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AdapterEvent::Result { .. }
                | AdapterEvent::Failed { .. }
                | AdapterEvent::InvalidResult { .. }
                | AdapterEvent::FailedBeforeSubmission { .. }
                | AdapterEvent::Disconnected { .. }
        )
    }
}

/// Delivery receipt for `send_message`/`answer` (03 §1). `Delivered` is an
/// accepted hand-off, not a provider acknowledgment of effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryReceipt {
    Delivered {
        provider_ref: Option<String>,
    },
    Queued {
        reason: QueuedReason,
    },
    Rejected {
        reason: &'static str,
    },
    /// The request may have reached the provider; never retry it blindly.
    Unknown {
        reason: &'static str,
    },
}

/// Why a message stayed queued (02 §8: queued is never "delivered now").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedReason {
    /// No steer capability on the active turn.
    SteerUnsupported,
    /// No active turn; the body joins the next run's context.
    NextRun,
}

/// Cancellation receipt (03 §1): accepted ≠ confirmed. `Confirmed` is only
/// reported after the owned process termination was verified (02 §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelReceipt {
    /// The provider/child accepted the interrupt; termination unconfirmed.
    Accepted,
    /// Termination confirmed (child reaped where one exists).
    Confirmed {
        exit: Option<i32>,
    },
    Rejected {
        reason: CancelRejected,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelRejected {
    UnknownRun,
    AlreadyTerminal,
    /// Free-form provider refusal text (recorded transports).
    Other(String),
}

/// Probe result (03 §1 `inspect`): `Unknown` covers unverifiable states —
/// never guessed as Running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunProbe {
    Running,
    Finished { exit: Option<i32> },
    Absent,
    Unknown,
}

/// The adapter port. `start` returns once the launch intent was accepted
/// (process spawned / session opened); events flow through [`EventStream`]
/// handles obtained from `subscribe`.
pub trait AgentAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn start(&self, run: RunStart) -> std::io::Result<()>;
    fn send_message(&self, run_id: &Id, body: &str) -> DeliveryReceipt;
    fn answer(&self, run_id: &Id, provider_request_id: &str, answer: &str) -> DeliveryReceipt;
    fn interrupt(&self, run_id: &Id) -> CancelReceipt;
    fn inspect(&self, run_id: &Id) -> RunProbe;
    fn close(&self, run_id: &Id) -> CancelReceipt;
    fn subscribe(&self) -> EventStream;
}

/// DB fencing enforcement for adapter callbacks (02 §7: a new actor bumps
/// the token and old callbacks become void). Tokens are tracked per run;
/// stale events are counted and dropped — never applied, never silently
/// ignored.
#[derive(Default)]
pub struct FencingGate {
    tokens: std::sync::Mutex<std::collections::HashMap<Id, u64>>,
    dropped_stale: AtomicU64,
    dropped_late: AtomicU64,
}

impl FencingGate {
    pub fn new() -> Arc<Self> {
        Arc::new(FencingGate::default())
    }

    /// Declare the starting actor's token for a run.
    pub fn register(&self, run_id: &Id, token: u64) {
        self.tokens
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(run_id.clone(), token);
    }

    /// The token currently entitled to mutate the run (`None` = unknown
    /// run, every event is stale).
    pub fn current(&self, run_id: &Id) -> Option<u64> {
        self.tokens
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(run_id)
            .copied()
    }

    /// A new actor takes over: the token only ever moves forward.
    pub fn advance(&self, run_id: &Id, to: u64) -> Option<u64> {
        let mut tokens = self.tokens.lock().unwrap_or_else(|p| p.into_inner());
        let slot = tokens.entry(run_id.clone()).or_insert(0);
        if to > *slot {
            *slot = to;
        }
        Some(*slot)
    }

    pub fn dropped_stale(&self) -> u64 {
        self.dropped_stale.load(Ordering::Acquire)
    }

    pub fn dropped_late(&self) -> u64 {
        self.dropped_late.load(Ordering::Acquire)
    }

    fn admit(&self, run_id: &Id, token: u64) -> bool {
        let matches = self.current(run_id) == Some(token);
        if !matches {
            self.dropped_stale.fetch_add(1, Ordering::AcqRel);
        }
        matches
    }

    fn note_late_delta(&self) {
        self.dropped_late.fetch_add(1, Ordering::AcqRel);
    }
}

/// mpsc-backed event stream with the 03 §1 guarantees: stale-token events
/// and post-terminal deltas are counted and dropped.
pub struct EventStream {
    rx: mpsc::UnboundedReceiver<AdapterEvent>,
    gate: Arc<FencingGate>,
    terminal_runs: std::sync::Mutex<HashSet<Id>>,
}

impl EventStream {
    pub(crate) fn new(rx: mpsc::UnboundedReceiver<AdapterEvent>, gate: Arc<FencingGate>) -> Self {
        EventStream {
            rx,
            gate,
            terminal_runs: std::sync::Mutex::new(HashSet::new()),
        }
    }

    pub fn gate(&self) -> &Arc<FencingGate> {
        &self.gate
    }

    /// Receive the next applicable event, or `None` at channel close.
    pub async fn next(&mut self) -> Option<AdapterEvent> {
        loop {
            let event = self.rx.recv().await?;
            if self.applicable(&event) {
                if event.is_terminal() {
                    self.terminal_runs
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(event.run_id().clone());
                }
                return Some(event);
            }
        }
    }

    /// Await the next applicable event for at most `timeout`.
    pub async fn next_timeout(&mut self, timeout: Duration) -> Option<AdapterEvent> {
        tokio::time::timeout(timeout, self.next())
            .await
            .ok()
            .flatten()
    }

    /// Non-blocking variant for manual-clock tests.
    pub fn try_next(&mut self) -> Option<AdapterEvent> {
        loop {
            let event = self.rx.try_recv().ok()?;
            if self.applicable(&event) {
                if event.is_terminal() {
                    self.terminal_runs
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(event.run_id().clone());
                }
                return Some(event);
            }
        }
    }

    fn applicable(&self, event: &AdapterEvent) -> bool {
        if !self.gate.admit(event.run_id(), event.fencing_token()) {
            return false;
        }
        if self
            .terminal_runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(event.run_id())
        {
            // 03 §1: deltas after Result/Failed never re-change state.
            self.gate.note_late_delta();
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_contracts::mission::types::ProviderResult;

    fn ev_for(run_id: &Id, token: u64) -> AdapterEvent {
        AdapterEvent::Activity {
            run_id: run_id.clone(),
            fencing_token: token,
            chunk: "x".into(),
        }
    }

    #[tokio::test]
    async fn stale_token_events_are_counted_and_dropped() {
        let gate = FencingGate::new();
        let run_id = Id::generate();
        gate.register(&run_id, 1);
        let (tx, rx) = mpsc::unbounded_channel();
        let mut stream = EventStream::new(rx, Arc::clone(&gate));
        tx.send(ev_for(&run_id, 1)).expect("send current");
        tx.send(ev_for(&run_id, 0)).expect("send stale");
        tx.send(ev_for(&run_id, 1)).expect("send current again");
        let a = stream.next().await.expect("current token passes");
        assert_eq!(a.fencing_token(), 1);
        let b = stream.next().await.expect("stale skipped");
        assert_eq!(b.fencing_token(), 1);
        assert_eq!(gate.dropped_stale(), 1);
        assert!(stream.try_next().is_none());
    }

    #[tokio::test]
    async fn advance_invalidates_the_previous_actor_only_forward() {
        let gate = FencingGate::new();
        let run = Id::generate();
        gate.register(&run, 3);
        assert_eq!(gate.advance(&run, 5), Some(5));
        assert_eq!(gate.advance(&run, 4), Some(5), "token never moves backward");
        assert_eq!(gate.current(&run), Some(5));
    }

    #[tokio::test]
    async fn deltas_after_terminal_are_dropped() {
        let gate = FencingGate::new();
        let (tx, rx) = mpsc::unbounded_channel();
        let mut stream = EventStream::new(rx, Arc::clone(&gate));
        let run_id = Id::generate();
        gate.register(&run_id, 1);
        let result = AdapterEvent::Result {
            run_id: run_id.clone(),
            fencing_token: 1,
            result: ProviderResult::Report {
                report_text: "done".into(),
                knowledge: Vec::new(),
            },
        };
        tx.send(result).expect("terminal");
        tx.send(AdapterEvent::Activity {
            run_id: run_id.clone(),
            fencing_token: 1,
            chunk: "late".into(),
        })
        .expect("late delta");
        let terminal = stream.next().await.expect("terminal passes");
        assert!(terminal.is_terminal());
        assert!(stream.try_next().is_none(), "late delta dropped");
        assert_eq!(gate.dropped_late(), 1);
    }
}
