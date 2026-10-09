//! # Claude Code print adapter (ticket O09, docs/orchestration/03-adapters.md §4)
//!
//! CLI evidence: `fixtures/claude-help.txt`, `fixtures/claude-print-help.txt`
//! and `fixtures/claude-version.txt` were captured from the installed
//! Claude Code CLI 2.1.263 (`claude --help`, `claude -p --help`,
//! `claude --version` — no model calls). Every baseline argv flag is
//! asserted against the captured help text by `tests/mission_claude.rs`.
//!
//! Baseline argv: `-p --output-format stream-json --verbose
//! --include-partial-messages` plus `--model <binding.model_id>` (and
//! `--effort`/`--resume` when the binding provides them). Permission
//! posture is `--permission-prompts none` — documented in the captured
//! help as "anything that would prompt is denied automatically". That is
//! a restriction, never a grant: all-permission bypass flags
//! (`--dangerously-skip-permissions`, `--allow-dangerously-skip-permissions`,
//! `bypassPermissions`, `--fallback-model`, `--bare`) are rejected by
//! [`guard_argv`]. The prompt travels over STDIN only.
//!
//! Production authentication uses an explicit environment and the CLI's
//! stream-input initialization response before sending a user prompt.
//! Saved keys/tokens use private HOME/CLAUDE_CONFIG_DIR trees; an existing
//! managed subscription uses the official CLI credential home. Safe mode,
//! restricted tools and host-owned routing keep project/user customizations
//! from changing the selected connection. See `auth.rs` and the recorded
//! 2.1.271 metadata checks; older text-input fixtures remain separate evidence.
//!
//! Stream normalization ([`PrintRunDriver`]): stream-json lines →
//! [`AdapterEvent`]s. Protocol shapes marked confirmed/assumed:
//! * confirmed from the CLI's own headless documentation — `system/init`
//!   carrying session metadata, `stream_event` with
//!   `event.delta.type == "text_delta"` text, `assistant`/`user` messages,
//!   `system/api_retry` with an `error` slug, and the final `result`
//!   envelope (`subtype`, `is_error`, `result`, `session_id`, `usage`
//!   with `input_tokens`/`output_tokens`/`cache_*`, `total_cost_usd`).
//! * assumed — exact `system/init` field names beyond `session_id`
//!   (e.g. `model`), `structured_output` mapping onto the O02
//!   ProviderResult DTO. Unknown JSON event kinds and malformed lines are
//!   counted and skipped, never fatal (see [`StreamStats`]).
//!
//! Success requires BOTH the final `result` envelope AND exit code 0: an
//! API transport disconnect is not success, exit 0 without a final result
//! is RESULT_INVALID (E19), and interrupted runs never emit a Result.
//! Usage nulls stay null — never coerced to zero.
//!
//! Interrupts on Windows: piped children have no reliable SIGINT path
//! (O07), so cancellation requests the exec supervisor stop ladder
//! (interrupt grace → terminate `taskkill /T /F` → kill → confirmed reap,
//! 02 §9). [`CancelReceipt::Accepted`] therefore means "interrupt
//! requested, termination unconfirmed" until `close` confirms.
//!
//! The daemon uses `authenticated`; `supervised`/`spawned` retain the
//! legacy text-input process seam for existing callers and fixtures.

use std::collections::{BTreeMap, HashMap, HashSet};
pub mod auth;
mod authenticated;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use term_contracts::ids::U64String;
use term_contracts::launch::{Enforcement, LaunchPolicy};
use term_contracts::metrics::PressureLevel;
use term_contracts::mission::types::{
    AuthRoute, Binding, Id, ProviderResult, RuntimeCapabilities, RuntimeKind,
};
use term_contracts::mission::MissionErrorCode;
use tokio::sync::mpsc;

use crate::agent_runtime::{
    AdapterEvent, AgentAdapter, CancelReceipt, CancelRejected, DeliveryReceipt, EventStream,
    FencingGate, QueuedReason, RunProbe, RunStart,
};
use crate::exec::{
    ExecHandle, ExecProbe, ExecSupervisor, SpawnRequest, DEFAULT_SPOOL_BYTES, MAX_LINE_BYTES,
};

// ---- captured CLI evidence -------------------------------------------------

/// `claude --help` captured from the installed CLI (see module docs).
pub const FIXTURE_HELP: &str = include_str!("fixtures/claude-help.txt");
/// `claude -p --help` (print-mode usage; subcommands omitted by the CLI).
pub const FIXTURE_PRINT_HELP: &str = include_str!("fixtures/claude-print-help.txt");
/// `claude --version` output of the verified CLI build.
pub const FIXTURE_VERSION: &str = include_str!("fixtures/claude-version.txt");
/// CLI version the fixtures were captured from.
pub const EVIDENCE_CLI_VERSION: &str = "2.1.263";

/// Baseline argv (03 §4): every item must appear in [`FIXTURE_HELP`].
pub const BASELINE_ARGV: [&str; 5] = [
    "-p",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
];

/// Capability reason slug for anything without a recorded live-run
/// compatibility test (03 §6: evidence is per runtime+OS+auth_route).
pub const NO_COMPATIBILITY_EVIDENCE: &str = "no_compatibility_evidence";

/// Argv substrings this adapter must never pass (03 §4 items 5–6:
/// no all-permission bypass, no implicit `--bare`, no silent model
/// fallback).
const FORBIDDEN_ARGV_SUBSTRINGS: [&str; 5] = [
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "bypassPermissions",
    "--fallback-model",
    "--bare",
];

/// Launch plan for one print run: argv/env/cwd/stdin plus the isolated
/// config dir. Deliberately no Debug implementation: authenticated plans
/// contain secrets. [`SpawnObservation`] records environment names only.
#[derive(Clone)]
pub struct LaunchPlan {
    pub program: PathBuf,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub stdin: Vec<u8>,
    pub config_dir: PathBuf,
    pub resume_session: Option<String>,
    pub env_clear: bool,
    pub(crate) auth_scope: Option<auth::AuthScope>,
    pub(crate) redactor: Option<Arc<crate::connections::SecretRedactor>>,
    pub(crate) private: Option<auth::PrivateDirectory>,
}

/// Build the launch plan from the binding (03 §4 item 1). The prompt is
/// stdin-only — never joined into argv.
pub fn build_launch_plan(
    run: &RunStart,
    resume_session: Option<&str>,
    config_root: &Path,
) -> std::io::Result<LaunchPlan> {
    let model = run.binding.model_id.trim();
    if model.is_empty() {
        return Err(invalid_input(
            "binding.model_id is empty — refusing to launch with an implicit CLI default model",
        ));
    }
    if model.starts_with('-') {
        return Err(invalid_input(
            "binding.model_id must be a model id, not an option",
        ));
    }
    if run.binding.program.trim().is_empty() {
        return Err(invalid_input("binding.program is empty"));
    }
    let mut argv: Vec<String> = BASELINE_ARGV.iter().map(|s| (*s).to_string()).collect();
    argv.extend([
        "--permission-prompts".to_string(),
        "none".to_string(),
        "--model".to_string(),
        model.to_string(),
    ]);
    if let Some(effort) = run
        .binding
        .effort
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        if effort.starts_with('-') {
            return Err(invalid_input(
                "binding.effort must be a level, not an option",
            ));
        }
        argv.extend(["--effort".to_string(), effort.to_string()]);
    }
    if let Some(session) = resume_session {
        if session.is_empty() || session.starts_with('-') {
            return Err(invalid_input("resume session id is empty or option-shaped"));
        }
        argv.extend(["--resume".to_string(), session.to_string()]);
    }
    guard_argv(&argv)?;

    let config_dir = config_root.join(format!("claude-config-{}", run.run_id));
    let home_value = config_dir.to_string_lossy().into_owned();
    let mut env = BTreeMap::new();
    env.insert("CLAUDE_CONFIG_DIR".to_string(), home_value.clone());
    let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env.insert(home_key.to_string(), home_value);

    Ok(LaunchPlan {
        program: PathBuf::from(&run.binding.program),
        argv,
        env,
        cwd: run.workspace.clone().unwrap_or_else(std::env::temp_dir),
        stdin: run.prompt_stdin.clone().into_bytes(),
        config_dir,
        resume_session: resume_session.map(str::to_string),
        env_clear: false,
        auth_scope: None,
        redactor: None,
        private: None,
    })
}

fn invalid_input(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

fn permission_denied(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, message.into())
}

/// Defensive guard: the constructed argv must never carry a bypass or
/// implicit-behavior flag (03 §4 items 5–6).
pub fn guard_argv(argv: &[String]) -> std::io::Result<()> {
    for arg in argv {
        for bad in FORBIDDEN_ARGV_SUBSTRINGS {
            if arg.contains(bad) {
                return Err(invalid_input(format!(
                    "claude argv must never contain '{bad}'"
                )));
            }
        }
    }
    Ok(())
}

// ---- stream-json protocol (tolerant) ----------------------------------------

/// Counters for tolerated non-fatal stream anomalies (03 §4 item 4:
/// unknown kinds are skipped with a diagnostic counter, never fatal).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub lines: u64,
    pub unknown_event_kinds: u64,
    pub malformed_lines: u64,
    pub over_cap_lines: u64,
    pub api_retry_events: u64,
    pub stream_event_deltas: u64,
    /// `system/init.model` when present (assumed field name — used for
    /// requested/observed model comparison, never for identity).
    pub observed_model: Option<String>,
}

/// Usage numbers of the final `result` envelope. Absent/null stays `None`
/// — a genuine zero arrives as `Some(0)` (03 §2: absent means unknown).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageFields {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

/// The final `result` envelope (last line of a healthy stream).
#[derive(Debug, Clone, Default)]
pub struct ResultEnvelope {
    pub subtype: Option<String>,
    pub is_error: bool,
    pub session_id: Option<String>,
    pub result_text: Option<String>,
    pub structured_output: Option<Value>,
    pub usage: UsageFields,
    pub total_cost_usd: Option<f64>,
}

impl ResultEnvelope {
    /// Success only when the envelope says so: `is_error` false and the
    /// subtype is not an `error_*` variant.
    fn is_success(&self) -> bool {
        !self.is_error
            && self
                .subtype
                .as_deref()
                .is_none_or(|s| !s.starts_with("error_"))
    }

    fn cost_usd_micros(&self) -> Option<u64> {
        self.total_cost_usd.map(|usd| {
            let micros = (usd * 1_000_000.0).round();
            if micros <= 0.0 {
                0
            } else if micros >= u64::MAX as f64 {
                u64::MAX
            } else {
                micros as u64
            }
        })
    }

    fn provider_result(&self) -> Result<ProviderResult, String> {
        if let Some(output) = &self.structured_output {
            return super::parse_provider_result(output.clone())
                .map_err(|e| format!("structured_output is not a ProviderResult DTO: {e}"));
        }
        Ok(ProviderResult::Report {
            report_text: self.result_text.clone().unwrap_or_default(),
            knowledge: Vec::new(),
        })
    }
}

fn parse_usage(value: Option<&Value>) -> UsageFields {
    let Some(value) = value else {
        return UsageFields::default();
    };
    let field = |name: &str| value.get(name).and_then(Value::as_u64);
    UsageFields {
        input_tokens: field("input_tokens"),
        output_tokens: field("output_tokens"),
        cache_creation_input_tokens: field("cache_creation_input_tokens"),
        cache_read_input_tokens: field("cache_read_input_tokens"),
    }
}

fn parse_result_envelope(value: &Value) -> ResultEnvelope {
    let text = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_string);
    ResultEnvelope {
        subtype: text("subtype"),
        is_error: value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        session_id: text("session_id"),
        result_text: text("result"),
        structured_output: value
            .get("structured_output")
            .filter(|v| !v.is_null())
            .cloned(),
        usage: parse_usage(value.get("usage")),
        total_cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
    }
}

/// Structured `system/api_retry.error` slugs (from the CLI's headless
/// documentation) → normalized codes. Field-based classification only —
/// never stderr/screen-string scraping.
fn classify_api_error(slug: &str) -> MissionErrorCode {
    match slug {
        "authentication_failed"
        | "oauth_org_not_allowed"
        | "account_on_hold"
        | "billing_error"
        | "cloud_credential_error" => MissionErrorCode::AuthRequired,
        "rate_limit" => MissionErrorCode::ProviderRateLimited,
        "model_not_found" => MissionErrorCode::ModelUnavailable,
        _ => MissionErrorCode::ProviderUnavailable,
    }
}

fn map_result_subtype(subtype: Option<&str>) -> MissionErrorCode {
    match subtype {
        Some("error_max_budget_usd") => MissionErrorCode::BudgetExceeded,
        _ => MissionErrorCode::ProviderUnavailable,
    }
}

// ---- run driver -------------------------------------------------------------

/// Event fan-out shared by the driver thread and `subscribe`. Delegates to
/// the bounded fan-out shared with the other runtime adapters (F1): Activity
/// text and latest-wins observations (Usage/RateLimited/ModelObserved)
/// stage behind a per-window delivery quota with oldest-first shedding
/// above a byte budget, while order-critical kinds (approvals, terminal
/// results, errors) are always delivered immediately.
#[derive(Default)]
pub struct FanOut {
    inner: Arc<super::codex::BoundedFanout>,
}

impl FanOut {
    pub fn publish(&self, event: AdapterEvent) {
        self.inner.publish(event);
    }

    pub(crate) fn add_subscriber(&self, tx: mpsc::UnboundedSender<AdapterEvent>) {
        self.inner.add_subscriber(tx);
    }

    /// See [`super::codex::BoundedFanout::start_flusher`].
    pub(crate) fn start_flusher(fanout: &Arc<Self>) {
        super::codex::BoundedFanout::start_flusher(&fanout.inner);
    }
}

/// Push-state machine turning stream-json lines into [`AdapterEvent`]s.
/// Feed stdout lines, then [`PrintRunDriver::finalize`] with the observed
/// exit code; the terminal event follows the 03 §4 rule that success
/// needs BOTH the envelope and exit 0.
pub struct PrintRunDriver {
    run_id: Id,
    fencing_token: u64,
    fanout: Arc<FanOut>,
    stats: StreamStats,
    started_emitted: bool,
    session_id: Option<String>,
    envelope: Option<ResultEnvelope>,
    duplicate_final: bool,
    requires_structured_result: bool,
    first_error: Option<(MissionErrorCode, String)>,
}

impl PrintRunDriver {
    pub fn new(run_id: Id, fencing_token: u64, fanout: Arc<FanOut>) -> Self {
        PrintRunDriver {
            run_id,
            fencing_token,
            fanout,
            stats: StreamStats::default(),
            started_emitted: false,
            session_id: None,
            envelope: None,
            duplicate_final: false,
            requires_structured_result: false,
            first_error: None,
        }
    }

    /// The provider session id observed so far (structured fields only).
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn stats(&self) -> &StreamStats {
        &self.stats
    }

    /// One stdout line (newline already stripped). Over-cap, non-JSON and
    /// unknown-kind lines are counted and skipped, never fatal.
    pub fn feed_line(&mut self, line: &[u8]) {
        self.stats.lines += 1;
        if line.len() > MAX_LINE_BYTES {
            self.stats.over_cap_lines += 1;
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            self.stats.malformed_lines += 1;
            return;
        };
        let Some(kind) = value.get("type").and_then(Value::as_str) else {
            self.stats.malformed_lines += 1;
            return;
        };
        match kind {
            "system" => self.handle_system(&value),
            "rate_limit_event" => {
                if let Some(observation) =
                    super::rate_limits::claude(&value, super::rate_limits::unix_millis())
                {
                    self.fanout.publish(AdapterEvent::RateLimited {
                        run_id: self.run_id.clone(),
                        fencing_token: self.fencing_token,
                        observation,
                    });
                }
            }
            "stream_event" => {
                let is_text_delta = value.pointer("/event/delta/type").and_then(Value::as_str)
                    == Some("text_delta");
                if is_text_delta {
                    if let Some(text) = value.pointer("/event/delta/text").and_then(Value::as_str) {
                        self.stats.stream_event_deltas += 1;
                        self.emit_activity(text);
                    }
                }
            }
            "assistant" => {
                // With the baseline's --include-partial-messages the deltas
                // already carry the text; re-emitting full assistant blocks
                // would duplicate it. The block fallback only fires when no
                // delta was ever seen.
                if self.stats.stream_event_deltas == 0 {
                    if let Some(blocks) =
                        value.pointer("/message/content").and_then(Value::as_array)
                    {
                        for block in blocks {
                            if block.get("type").and_then(Value::as_str) == Some("text") {
                                if let Some(text) = block.get("text").and_then(Value::as_str) {
                                    self.emit_activity(text);
                                }
                            }
                        }
                    }
                }
            }
            // Turn-complete user/tool messages carry no display text here.
            "user" => {}
            "result" => {
                if self.envelope.is_some() {
                    self.duplicate_final = true;
                    return;
                }
                self.envelope = Some(parse_result_envelope(&value));
            }
            _ => {
                self.stats.unknown_event_kinds += 1;
            }
        }
    }

    fn handle_system(&mut self, value: &Value) {
        let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
        match subtype {
            "init" => {
                if self.session_id.is_none() {
                    self.session_id = value
                        .get("session_id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                if self.stats.observed_model.is_none() {
                    self.stats.observed_model = value
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                if !self.started_emitted {
                    self.emit_started();
                }
            }
            "api_retry" => {
                self.stats.api_retry_events += 1;
                if self.first_error.is_none() {
                    if let Some(slug) = value.get("error").and_then(Value::as_str) {
                        self.first_error = Some((classify_api_error(slug), slug.to_string()));
                    }
                }
            }
            // Other system subtypes (plugin_install, hook events,
            // permission_denied, …) are known kinds; values we do not use
            // are ignored per the CLI's own guidance.
            _ => {}
        }
    }

    /// Terminal decision: exit code AND envelope both required for
    /// success (03 §4 item 4). Interrupted runs are never results.
    pub fn finalize(&mut self, exit: Option<i32>, interrupted: bool) {
        if self.session_id.is_none() {
            self.session_id = self.envelope.as_ref().and_then(|e| e.session_id.clone());
        }
        if !self.started_emitted {
            self.emit_started();
        }
        if interrupted {
            self.emit_failed(
                MissionErrorCode::OutcomeUnknown,
                "run interrupted before a final result; terminated via the exec stop ladder \
                 (no reliable SIGINT for Windows piped children)"
                    .into(),
            );
            return;
        }
        if self.stats.over_cap_lines > 0 {
            self.emit_failed(
                MissionErrorCode::ResultInvalid,
                format!(
                    "{} stdout lines exceeded the {}-byte raw cap (03 §2)",
                    self.stats.over_cap_lines, MAX_LINE_BYTES
                ),
            );
            return;
        }
        if self.duplicate_final {
            self.emit_failed(
                MissionErrorCode::ResultInvalid,
                "duplicate result envelope in one stream".into(),
            );
            return;
        }
        let Some(envelope) = self.envelope.take() else {
            if let Some((code, slug)) = &self.first_error {
                self.emit_failed(
                    *code,
                    format!("no final result envelope; structured api_retry error '{slug}'"),
                );
            } else if exit == Some(0) {
                self.emit_failed(
                    MissionErrorCode::ResultInvalid,
                    "exit 0 without a final result envelope (E19)".into(),
                );
            } else if exit.is_some() {
                self.emit_failed(
                    MissionErrorCode::OutcomeUnknown,
                    format!("exit {exit:?} without a final result envelope"),
                );
            } else {
                self.emit(AdapterEvent::Disconnected {
                    run_id: self.run_id.clone(),
                    fencing_token: self.fencing_token,
                });
            }
            return;
        };
        // Usage is observed from the envelope (nulls preserved) and must
        // land before the terminal event or the stream would drop it as a
        // late delta.
        self.emit(AdapterEvent::Usage {
            run_id: self.run_id.clone(),
            fencing_token: self.fencing_token,
            input_tokens: envelope.usage.input_tokens,
            output_tokens: envelope.usage.output_tokens,
            cost_usd_micros: envelope.cost_usd_micros(),
        });
        if envelope.is_success() && exit == Some(0) {
            let result = if self.requires_structured_result && envelope.structured_output.is_none()
            {
                Err("Claude returned no structured ProviderResult".into())
            } else {
                envelope.provider_result()
            };
            match result {
                Ok(result) => self.emit(AdapterEvent::Result {
                    run_id: self.run_id.clone(),
                    fencing_token: self.fencing_token,
                    result,
                }),
                Err(_) => self.emit(AdapterEvent::InvalidResult {
                    run_id: self.run_id.clone(),
                    fencing_token: self.fencing_token,
                    code: MissionErrorCode::ResultInvalid,
                    message: "Claude returned no valid structured ProviderResult".into(),
                    rejected_result: envelope.structured_output.map(|value| value.to_string()),
                }),
            }
        } else if !envelope.is_success() {
            self.emit_failed(
                map_result_subtype(envelope.subtype.as_deref()),
                format!(
                    "claude result reported error (subtype {}, is_error {})",
                    envelope.subtype.as_deref().unwrap_or("(absent)"),
                    envelope.is_error
                ),
            );
        } else {
            self.emit_failed(
                MissionErrorCode::OutcomeUnknown,
                format!(
                    "result envelope reported success but process exit was {exit:?} \
                     (transport disconnect is not success)"
                ),
            );
        }
    }

    fn emit_started(&mut self) {
        self.started_emitted = true;
        self.emit(AdapterEvent::Started {
            run_id: self.run_id.clone(),
            fencing_token: self.fencing_token,
            provider_session_id: self.session_id.clone(),
            provider_turn_id: None,
        });
    }

    fn emit_activity(&mut self, text: &str) {
        self.emit(AdapterEvent::Activity {
            run_id: self.run_id.clone(),
            fencing_token: self.fencing_token,
            chunk: text.to_string(),
        });
    }

    fn emit_failed(&mut self, code: MissionErrorCode, message: String) {
        self.emit(AdapterEvent::Failed {
            run_id: self.run_id.clone(),
            fencing_token: self.fencing_token,
            code,
            message,
        });
    }

    fn emit(&self, event: AdapterEvent) {
        self.fanout.publish(event);
    }
}

// ---- process seam (spawned child OR recorded reader) ------------------------

/// One source of stream-json stdout lines plus its exit code — a spawned
/// exec child or a recorded reader (tests never spawn the real CLI).
pub trait PrintStreamSource: Send {
    /// Metadata-only initialization. It must never send a user prompt.
    fn initialize(&mut self) -> Result<(), (MissionErrorCode, &'static str)> {
        Ok(())
    }
    /// Next stdout line without the trailing newline; `None` = EOF.
    fn next_line(&mut self) -> Option<Vec<u8>>;
    /// Exit code after EOF; `None` when it could not be observed.
    fn exit_code(&mut self) -> Option<i32>;
    fn failure(&self) -> Option<(MissionErrorCode, &'static str)> {
        None
    }
    /// Positive evidence from a source that has not attempted a user frame.
    fn task_not_submitted(&self) -> bool {
        false
    }
    fn requires_structured_result(&self) -> bool {
        false
    }
}

/// A recorded stream: header line `{"iyagi_recording":1,…}` followed by
/// raw stream-json lines (see `fixtures/streams/`).
#[derive(Debug, Clone)]
pub struct Recording {
    pub exit_code: Option<i32>,
    pub line_delay_ms: u64,
    pub lines: Vec<Vec<u8>>,
}

impl Recording {
    pub fn parse(text: &str) -> Option<Recording> {
        let mut lines = text.lines();
        let header: Value = serde_json::from_str(lines.next()?).ok()?;
        if header.get("iyagi_recording")?.as_u64()? != 1 {
            return None;
        }
        Some(Recording {
            exit_code: header
                .get("exit_code")
                .and_then(Value::as_i64)
                .map(|c| c as i32),
            line_delay_ms: header
                .get("line_delay_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            lines: lines.map(|l| l.as_bytes().to_vec()).collect(),
        })
    }
}

/// Recorded reader with cancel-aware pacing (the `line_delay_ms` budget
/// makes mid-stream interrupts deterministic in tests).
pub struct RecordedSource {
    recording: Recording,
    index: usize,
    cancel: Arc<AtomicBool>,
}

impl RecordedSource {
    pub fn new(recording: Recording, cancel: Arc<AtomicBool>) -> Self {
        RecordedSource {
            recording,
            index: 0,
            cancel,
        }
    }
}

impl PrintStreamSource for RecordedSource {
    fn next_line(&mut self) -> Option<Vec<u8>> {
        if self.cancel.load(Ordering::Acquire) {
            return None;
        }
        let line = self.recording.lines.get(self.index)?.clone();
        self.index += 1;
        let delay = Duration::from_millis(self.recording.line_delay_ms);
        let mut waited = Duration::ZERO;
        while waited < delay {
            if self.cancel.load(Ordering::Acquire) {
                return None;
            }
            let step = Duration::from_millis(10).min(delay - waited);
            std::thread::sleep(step);
            waited += step;
        }
        Some(line)
    }

    fn exit_code(&mut self) -> Option<i32> {
        self.recording.exit_code
    }
}

/// A live exec child behind the same seam: the supervisor's sink pushes
/// stdout lines into a channel; the exit is confirmed through the
/// inspect/stop-ladder path.
pub struct ExecChildSource {
    handle: ExecHandle,
    lines: crate::exec::output::LineInbox,
    overflow_reported: bool,
    cancel: Arc<AtomicBool>,
}

impl PrintStreamSource for ExecChildSource {
    fn next_line(&mut self) -> Option<Vec<u8>> {
        use crate::exec::output::InboxError;
        loop {
            if self.cancel.load(Ordering::Acquire) || self.overflow_reported {
                return None;
            }
            // Observe EOF before draining: every sink call precedes its
            // publication, so an empty queue after EOF is definitive.
            let ended = self.handle.stdout_done();
            let result = if matches!(
                self.handle.output_verdict(),
                crate::exec::OutputVerdict::Invalid { .. }
            ) {
                Err(InboxError::Overflow)
            } else {
                self.lines.recv_timeout(if ended {
                    Duration::ZERO
                } else {
                    Duration::from_millis(25)
                })
            };
            match result {
                Ok(line) => return Some(line),
                Err(InboxError::Overflow) => {
                    self.overflow_reported = true;
                    return Some(vec![b' '; MAX_LINE_BYTES + 1]);
                }
                Err(InboxError::Closed) => return None,
                Err(InboxError::Timeout) if ended => return None,
                Err(InboxError::Timeout) => {}
            }
        }
    }

    fn exit_code(&mut self) -> Option<i32> {
        // stdout EOF usually means the child is exiting; observe briefly,
        // then confirm through the ladder (fast no-op for an exited child).
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !self.cancel.load(Ordering::Acquire) {
            if let ExecProbe::Finished { exit } = self.handle.inspect() {
                return exit;
            }
            if std::time::Instant::now() >= deadline {
                // A store outage after OS exit keeps ownership pending.
                // Never convert that into a missing provider result or a
                // confirmed close; the same owner retries completion.
                let _ = self
                    .handle
                    .stop_blocking(Duration::from_secs(10), Duration::from_secs(5));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

/// What a source factory hands to one run: the stream source plus the
/// exec handle when a real child exists (ladder interrupts).
pub struct PrintProcess {
    pub source: Box<dyn PrintStreamSource>,
    pub exec: Option<ExecHandle>,
}

/// Factory constructing the per-run process behind the seam.
pub type SourceFactory = Arc<
    dyn Fn(&RunStart, &LaunchPlan, Arc<AtomicBool>) -> std::io::Result<PrintProcess> + Send + Sync,
>;

/// Injectable version probe (default runs `<program> --version`, a free
/// local invocation — never a model call).
pub type VersionProbeFn = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

fn probe_version_via_cli(program: &str) -> Option<String> {
    super::installation::version(program, term_contracts::mission::types::RuntimeKind::Claude)
        .ok()
        .map(|version| format!("{version} (Claude Code)"))
}

fn spawn_exec_child(
    run: &RunStart,
    plan: &LaunchPlan,
    supervisor: &Arc<ExecSupervisor>,
    runtime: &tokio::runtime::Handle,
    cancel: Arc<AtomicBool>,
) -> std::io::Result<PrintProcess> {
    let (raw_sink, lines) = crate::exec::output::bounded_stdout_inbox();
    let private = plan.private.clone();
    let sink = Arc::new(move |kind, line: &[u8]| {
        let _private = &private;
        raw_sink(kind, line);
    });
    let request = SpawnRequest {
        exec_id: Id::generate(),
        mission_id: run.mission_id.clone(),
        run_id: run.run_id.clone(),
        owner_daemon_id: run.owner_daemon_id.clone(),
        program: plan.program.clone(),
        argv: plan.argv.clone(),
        cwd: plan.cwd.clone(),
        env_overrides: plan.env.clone(),
        env_clear: plan.env_clear,
        stdin: if plan.auth_scope.is_some() {
            None
        } else {
            Some(plan.stdin.clone())
        },
        resource_policy: run.binding.resource_policy.clone(),
        spool_bytes: DEFAULT_SPOOL_BYTES,
        redactor: plan
            .redactor
            .clone()
            .map(|r| r as Arc<dyn crate::exec::output::Redactor>),
        sink,
        validate_path: None,
    };
    let handle = if plan.auth_scope.is_some() {
        supervisor.spawn_interactive_on(request, runtime)
    } else {
        supervisor.spawn_on(request, runtime)
    }
    .map_err(|_| std::io::Error::other("Claude process launch failed"))?;
    let inner = ExecChildSource {
        handle: handle.clone(),
        lines,
        overflow_reported: false,
        cancel,
    };
    let source: Box<dyn PrintStreamSource> = if let Some(scope) = &plan.auth_scope {
        Box::new(authenticated::AuthenticatedSource {
            inner,
            input: handle.input().expect("interactive Claude owns stdin"),
            scope: scope.clone(),
            prompt: Some(plan.stdin.clone()),
            private: plan.private.clone(),
            runtime: runtime.clone(),
            failure: None,
            initialized: false,
        })
    } else {
        Box::new(inner)
    };
    Ok(PrintProcess {
        source,
        exec: Some(handle),
    })
}

// ---- capability honesty ------------------------------------------------------

/// Capabilities this adapter advertises without a recorded live-run
/// compatibility test (03 §6): everything unverified is `false` with
/// `no_compatibility_evidence`; steer/approval_reply are false by print
/// mode design (03 §4 items 5–6). Live evidence lands with O18.
/// Capabilities from the O18 live-evidence registry when the caller's CLI
/// version matches the recorded live-tested version (2.1.263/Windows);
/// otherwise every claim resets to `no_compatibility_evidence` (03 §6).
pub fn claude_capabilities_for(version: Option<&str>) -> RuntimeCapabilities {
    super::capability_evidence::capabilities_for(
        term_contracts::mission::types::RuntimeKind::Claude,
        std::env::consts::OS,
        version,
    )
}

/// Back-compat entry for callers without a parsed version: fully unclaimed.
pub fn claude_capabilities() -> RuntimeCapabilities {
    claude_capabilities_for(None)
}

// ---- adapter -----------------------------------------------------------------

pub struct ClaudeAdapterConfig {
    /// Daemon-owned root for per-run `CLAUDE_CONFIG_DIR` trees.
    pub config_root: PathBuf,
    /// Daemon data root holding the local secret store — the Z.ai Coding
    /// Plan key the key-store route reads at start time. `None` in tests
    /// that never launch that route.
    pub secrets_root: Option<PathBuf>,
    pub source_factory: SourceFactory,
    pub version_probe: VersionProbeFn,
}

/// Sanitized launch observation (env keys only — values are never
/// persisted, 09 §3).
#[derive(Debug, Clone)]
pub struct SpawnObservation {
    pub run_id: Id,
    pub argv: Vec<String>,
    pub env_keys: Vec<String>,
    pub cwd: PathBuf,
    pub stdin_bytes: usize,
    pub config_dir: PathBuf,
    pub resume_session: Option<String>,
}

struct RunSlot {
    cancel: Arc<AtomicBool>,
    interrupt_requested: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    exec: Option<ExecHandle>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
    stats: Mutex<Option<StreamStats>>,
}

struct Inner {
    config: ClaudeAdapterConfig,
    authenticated: bool,
    connections: Option<Arc<crate::connections::ConnectionStore>>,
    claimed_runs: Mutex<HashSet<Id>>,
    bus: Arc<FanOut>,
    gate: Arc<FencingGate>,
    runs: Mutex<HashMap<Id, Arc<RunSlot>>>,
    /// Provider session ids this adapter observed (03 §4 item 5: only
    /// owned sessions may be resumed).
    owned_sessions: Arc<Mutex<HashSet<String>>>,
    spawn_log: Mutex<Vec<SpawnObservation>>,
    /// Keeps private pump workers alive when constructed off-runtime.
    _runtime: Option<tokio::runtime::Runtime>,
}

/// The Claude Code print adapter (03 §4).
pub struct ClaudePrintAdapter {
    inner: Arc<Inner>,
}

impl Drop for ClaudePrintAdapter {
    fn drop(&mut self) {
        for slot in self
            .inner
            .runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
        {
            slot.cancel.store(true, Ordering::Release);
        }
    }
}

impl ClaudePrintAdapter {
    /// Production auth-scoped stream input with metadata verification before
    /// sending the single task prompt. Capabilities still require evidence.
    pub fn authenticated(
        supervisor: Arc<ExecSupervisor>,
        runtime: tokio::runtime::Handle,
        config_root: PathBuf,
        secrets_root: Option<PathBuf>,
        connections: Option<Arc<crate::connections::ConnectionStore>>,
    ) -> Arc<Self> {
        Self::with_auth_config(
            ClaudeAdapterConfig {
                config_root,
                secrets_root,
                source_factory: Arc::new(move |run, plan, cancel| {
                    spawn_exec_child(run, plan, &supervisor, &runtime, cancel)
                }),
                version_probe: Arc::new(probe_version_via_cli),
            },
            true,
            connections,
        )
    }
    /// Legacy text-input seam using the shared process lifecycle. The daemon
    /// uses authenticated so environment and account checks are mandatory.
    pub fn supervised(
        supervisor: Arc<ExecSupervisor>,
        runtime: tokio::runtime::Handle,
        config_root: PathBuf,
    ) -> Arc<Self> {
        Self::with_config(ClaudeAdapterConfig {
            config_root,
            secrets_root: None,
            source_factory: Arc::new(move |run, plan, cancel| {
                spawn_exec_child(run, plan, &supervisor, &runtime, cancel)
            }),
            version_probe: Arc::new(probe_version_via_cli),
        })
    }
    /// Tests / offline replay: bring your own source factory and version
    /// probe — no CLI is ever spawned.
    pub fn with_config(config: ClaudeAdapterConfig) -> Arc<Self> {
        Self::with_auth_config(config, false, None)
    }
    fn with_auth_config(
        config: ClaudeAdapterConfig,
        authenticated: bool,
        connections: Option<Arc<crate::connections::ConnectionStore>>,
    ) -> Arc<Self> {
        let bus = Arc::new(FanOut::default());
        FanOut::start_flusher(&bus);
        Arc::new(ClaudePrintAdapter {
            inner: Arc::new(Inner {
                config,
                authenticated,
                connections,
                claimed_runs: Mutex::new(HashSet::new()),
                bus,
                gate: FencingGate::new(),
                runs: Mutex::new(HashMap::new()),
                owned_sessions: Arc::new(Mutex::new(HashSet::new())),
                spawn_log: Mutex::new(Vec::new()),
                _runtime: None,
            }),
        })
    }

    /// Standalone/legacy process seam with an observer-only supervisor.
    /// Daemon mission execution uses [`Self::authenticated`] with its shared
    /// durable store, launch helper, native groups, and live host sample.
    pub fn spawned(program: &str) -> std::io::Result<Arc<Self>> {
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
        let _ = program; // program comes from each run's binding
        let bus = Arc::new(FanOut::default());
        FanOut::start_flusher(&bus);
        Ok(Arc::new(ClaudePrintAdapter {
            inner: Arc::new(Inner {
                authenticated: false,
                connections: None,
                claimed_runs: Mutex::new(HashSet::new()),
                config: ClaudeAdapterConfig {
                    config_root: std::env::temp_dir().join("iyagi-claude-configs"),
                    secrets_root: None,
                    source_factory: Arc::new(move |run, plan, cancel| {
                        spawn_exec_child(run, plan, &supervisor, &spawn_handle, cancel)
                    }),
                    version_probe: Arc::new(probe_version_via_cli),
                },
                bus,
                gate: FencingGate::new(),
                runs: Mutex::new(HashMap::new()),
                owned_sessions: Arc::new(Mutex::new(HashSet::new())),
                spawn_log: Mutex::new(Vec::new()),
                _runtime: runtime,
            }),
        }))
    }

    /// Start one print run, optionally resuming a provider session this
    /// adapter recorded (03 §4 item 5). `--resume` is evidenced in the
    /// captured help ("-r, --resume [value] Resume a conversation by
    /// session ID").
    pub fn start_with_resume(
        &self,
        run: RunStart,
        resume_session: Option<&str>,
    ) -> std::io::Result<()> {
        if let Some(session) = resume_session {
            let owned = self
                .inner
                .owned_sessions
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if !owned.contains(session) {
                return Err(permission_denied(format!(
                    "resume refused: provider session '{session}' was not recorded by this adapter"
                )));
            }
        }
        let mut plan = build_launch_plan(&run, resume_session, &self.inner.config.config_root)?;
        if self.inner.authenticated {
            auth::configure(
                &run,
                &mut plan,
                self.inner.connections.as_deref(),
                self.inner.config.secrets_root.as_deref(),
                &self.inner.config.config_root,
            )?;
        } else {
            std::fs::create_dir_all(&plan.config_dir)?;
        }
        if !self
            .inner
            .claimed_runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(run.run_id.clone())
        {
            return Err(invalid_input("Claude run was already started"));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let interrupt_requested = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let process = (self.inner.config.source_factory)(&run, &plan, Arc::clone(&cancel))?;
        self.inner.gate.register(&run.run_id, run.fencing_token);

        let slot = Arc::new(RunSlot {
            cancel: Arc::clone(&cancel),
            interrupt_requested: Arc::clone(&interrupt_requested),
            finished: Arc::clone(&finished),
            exec: process.exec.clone(),
            join: Mutex::new(None),
            stats: Mutex::new(None),
        });
        self.inner
            .runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(run.run_id.clone(), Arc::clone(&slot));
        self.inner
            .spawn_log
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(SpawnObservation {
                run_id: run.run_id.clone(),
                argv: plan.argv.clone(),
                env_keys: plan.env.keys().cloned().collect(),
                cwd: plan.cwd.clone(),
                stdin_bytes: plan.stdin.len(),
                config_dir: plan.config_dir.clone(),
                resume_session: plan.resume_session.clone(),
            });

        let bus = Arc::clone(&self.inner.bus);
        let owned_sessions = Arc::clone(&self.inner.owned_sessions);
        let run_id = run.run_id.clone();
        let token = run.fencing_token;
        let thread_slot = Arc::clone(&slot);
        let mut source = process.source;
        let thread = std::thread::Builder::new()
            .name(format!("claude-run-{run_id}"))
            .spawn(move || {
                let mut driver = PrintRunDriver::new(run_id, token, bus);
                driver.requires_structured_result = source.requires_structured_result();
                let initialization = source.initialize();
                if initialization.is_ok() {
                    while let Some(line) = source.next_line() {
                        if thread_slot.cancel.load(Ordering::Acquire) {
                            break;
                        }
                        driver.feed_line(&line);
                    }
                }
                let interrupted = thread_slot.cancel.load(Ordering::Acquire)
                    || thread_slot.interrupt_requested.load(Ordering::Acquire);
                let exit = if interrupted {
                    // Interrupted runs are not normal results; the ladder
                    // (not the exit code) ends them.
                    None
                } else {
                    source.exit_code()
                };
                if let Some((code, message)) = source
                    .failure()
                    .or_else(|| initialization.err())
                    .filter(|_| !interrupted)
                {
                    if source.task_not_submitted() && code == MissionErrorCode::ProviderUnavailable
                    {
                        driver.emit(super::retry::failure(
                            driver.run_id.clone(),
                            driver.fencing_token,
                            code,
                            message.into(),
                        ));
                    } else {
                        driver.emit_failed(code, message.into());
                    }
                } else {
                    driver.finalize(exit, interrupted);
                }
                if let Some(session) = driver.session_id() {
                    owned_sessions
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(session.to_string());
                }
                *thread_slot.stats.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(driver.stats().clone());
                thread_slot.finished.store(true, Ordering::Release);
            })
            .map_err(|e| std::io::Error::other(format!("claude run thread spawn: {e}")))?;
        *slot.join.lock().unwrap_or_else(|p| p.into_inner()) = Some(thread);
        Ok(())
    }

    /// Offline probe (03 §4 item 1): program existence + version parse +
    /// auth-route metadata taken from the binding only. No model calls.
    pub fn probe(&self, binding: &Binding) -> ProbeReport {
        let version_raw = (self.inner.config.version_probe)(&binding.program);
        let version = version_raw.as_deref().and_then(parse_cli_version);
        let capabilities = super::capability_evidence::capabilities_for_binding(
            binding,
            std::env::consts::OS,
            version_raw
                .as_deref()
                .and_then(|raw| {
                    super::installation::parse_version(
                        term_contracts::mission::types::RuntimeKind::Claude,
                        raw,
                    )
                })
                .as_deref(),
        );
        ProbeReport {
            program: binding.program.clone(),
            program_exists: Path::new(&binding.program).is_file(),
            version_raw,
            version,
            auth_route: binding.auth_route,
            provider_id: binding.provider_id.clone(),
            model_id: binding.model_id.clone(),
            capabilities,
        }
    }

    /// Sanitized launch observations (env keys only) for evidence/tests.
    pub fn spawn_observations(&self) -> Vec<SpawnObservation> {
        self.inner
            .spawn_log
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Stream stats of a finished run (diagnostic counters).
    pub fn run_stats(&self, run_id: &Id) -> Option<StreamStats> {
        self.inner
            .runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(run_id)
            .and_then(|slot| slot.stats.lock().unwrap_or_else(|p| p.into_inner()).clone())
    }

    fn slot(&self, run_id: &Id) -> Option<Arc<RunSlot>> {
        self.inner
            .runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(run_id)
            .cloned()
    }
}

/// Offline probe result. `version`/`capabilities` never claim what a live
/// run has not evidenced.
#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub program: String,
    pub program_exists: bool,
    pub version_raw: Option<String>,
    pub version: Option<CliVersion>,
    pub auth_route: AuthRoute,
    pub provider_id: String,
    pub model_id: String,
    pub capabilities: RuntimeCapabilities,
}

/// Parsed `claude --version` triple, e.g. `2.1.263 (Claude Code)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

pub fn parse_cli_version(raw: &str) -> Option<CliVersion> {
    let token = raw.split_whitespace().next()?;
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(CliVersion {
        major,
        minor,
        patch,
    })
}

impl AgentAdapter for ClaudePrintAdapter {
    fn name(&self) -> &'static str {
        "claude-print"
    }

    fn start(&self, run: RunStart) -> std::io::Result<()> {
        self.start_with_resume(run, None)
    }

    fn send_message(&self, run_id: &Id, _body: &str) -> DeliveryReceipt {
        // Print mode has no active-turn injection (03 §4 item 5); queued
        // message semantics stay engine-side — the receipt says why.
        match self.slot(run_id) {
            Some(slot) => {
                let terminal = slot.finished.load(Ordering::Acquire)
                    || slot.interrupt_requested.load(Ordering::Acquire);
                if terminal {
                    DeliveryReceipt::Queued {
                        reason: QueuedReason::NextRun,
                    }
                } else {
                    DeliveryReceipt::Queued {
                        reason: QueuedReason::SteerUnsupported,
                    }
                }
            }
            None => DeliveryReceipt::Rejected {
                reason: "unknown run",
            },
        }
    }

    fn answer(&self, run_id: &Id, _provider_request_id: &str, _answer: &str) -> DeliveryReceipt {
        if self.slot(run_id).is_none() {
            return DeliveryReceipt::Rejected {
                reason: "unknown run",
            };
        }
        // With --permission-prompts none the CLI denies anything that
        // would prompt; there is no interactive reply channel to deliver
        // an answer through (03 §4 item 6).
        DeliveryReceipt::Rejected {
            reason: "print mode has no approval reply channel (permission prompts are denied)",
        }
    }

    fn interrupt(&self, run_id: &Id) -> CancelReceipt {
        let Some(slot) = self.slot(run_id) else {
            return CancelReceipt::Rejected {
                reason: CancelRejected::UnknownRun,
            };
        };
        if slot.finished.load(Ordering::Acquire) {
            return CancelReceipt::Rejected {
                reason: CancelRejected::AlreadyTerminal,
            };
        }
        slot.interrupt_requested.store(true, Ordering::Release);
        slot.cancel.store(true, Ordering::Release);
        if let Some(exec) = &slot.exec {
            // Windows piped child: no SIGINT — request the exec stop
            // ladder on its own thread (interrupt grace → terminate
            // taskkill /T /F → kill). Accepted until termination is
            // confirmed (02 §9).
            let exec = exec.clone();
            std::thread::spawn(move || {
                let _ = exec.stop_blocking(Duration::from_secs(10), Duration::from_secs(5));
            });
        }
        CancelReceipt::Accepted
    }

    fn inspect(&self, run_id: &Id) -> RunProbe {
        let Some(slot) = self.slot(run_id) else {
            return RunProbe::Absent;
        };
        if let Some(exec) = &slot.exec {
            return match exec.inspect() {
                ExecProbe::Running => RunProbe::Running,
                ExecProbe::Finished { exit } => RunProbe::Finished { exit },
                ExecProbe::Absent => RunProbe::Unknown,
            };
        }
        if slot.finished.load(Ordering::Acquire) {
            RunProbe::Finished { exit: None }
        } else {
            RunProbe::Running
        }
    }

    fn close(&self, run_id: &Id) -> CancelReceipt {
        let Some(slot) = self.slot(run_id) else {
            return CancelReceipt::Rejected {
                reason: CancelRejected::UnknownRun,
            };
        };
        slot.cancel.store(true, Ordering::Release);
        let exit = if let Some(exec) = &slot.exec {
            if exec
                .stop_blocking(Duration::from_secs(10), Duration::from_secs(5))
                .is_err()
            {
                // Keep the slot: the owner still retries its durable Exited
                // commit and the actor must not release the Run early.
                return CancelReceipt::Accepted;
            }
            match exec.inspect() {
                ExecProbe::Finished { exit } => exit,
                _ => return CancelReceipt::Accepted,
            }
        } else {
            None
        };
        let thread = slot.join.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
        self.inner
            .runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(run_id);
        CancelReceipt::Confirmed { exit }
    }

    fn subscribe(&self) -> EventStream {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner.bus.add_subscriber(tx);
        EventStream::new(rx, Arc::clone(&self.inner.gate))
    }
}

// ---- defaults (mirrors the fake adapter's private helpers) -------------------

fn noop_persist() -> crate::exec::PersistExec {
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

/// A test/dev binding for the Claude runtime (mirrors `fake_binding`).
pub fn claude_binding(program: &str, auth_route: AuthRoute) -> Binding {
    let policy = || LaunchPolicy {
        reservation_bytes: U64String::new(64 << 20).expect("fits"),
        cpu_slots: 1,
        enforcement: Enforcement::Prefer,
        memory_max_bytes: None,
        cpu_max_cores: None,
        pids_max: None,
    };
    Binding {
        id: Id::generate(),
        revision: U64String::new(1).expect("fits"),
        label: "Claude Code print adapter (O09)".into(),
        runtime: RuntimeKind::Claude,
        program: program.to_string(),
        runtime_version: None,
        provider_id: "anthropic".into(),
        model_id: "claude-test-model".into(),
        effort: None,
        auth_route,
        credential_ref: None,
        endpoint_ref: None,
        capabilities: claude_capabilities(),
        checked_at: None,
        enabled: true,
        experimental_version: None,
        local_evidence: None,
        estimated_run_cost_usd_micros: None,
        resource_policy: policy(),
    }
}
