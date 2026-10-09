//! OpenCode's session/async-prompt/SSE protocol, pinned by the captured
//! OpenAPI fixture. A fresh session owns exactly one user message. Transport
//! loss never permits another prompt POST; only a correlated, completed,
//! persisted assistant message can supply the structured result.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use term_contracts::mission::error::{MissionErrorCode as Code, MissionRpcError};
use term_contracts::mission::types::{Binding, Id};

use crate::agent_runtime::{
    AdapterEvent, CancelReceipt, CancelRejected, DeliveryReceipt, QueuedReason, RunProbe, RunStart,
    WorkspaceAccess,
};

pub mod http;
pub mod models;
pub mod runtime;
pub mod server;

const MAX_MESSAGES: usize = 256;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_RESULT_BYTES: usize = 2 * 1024 * 1024;

/// The live implementation uses a run-owned, authenticated loopback server.
/// A false callback result ends this subscription without implying process
/// termination. Neither this interface nor its implementations retry POSTs.
pub trait OpencodeTransport: Send + Sync {
    fn post(&self, route: &str, body: &Value) -> Result<Value, String>;
    fn get(&self, route: &str) -> Result<Value, String>;
    fn event_stream(
        &self,
        route: &str,
        on_event: &mut dyn FnMut(Value) -> bool,
    ) -> Result<(), String>;
    fn close(&self);
    /// Only owned child termination can make this true, not an HTTP reply.
    fn process_probe(&self) -> RunProbe {
        RunProbe::Unknown
    }
}

#[derive(Default)]
struct RunState {
    task_kind: Option<term_contracts::mission::types::TaskKind>,
    session_id: Option<String>,
    user_message_id: String,
    turn_id: Option<String>,
    token: u64,
    claimed: bool,
    terminal: bool,
    interrupted: bool,
    approvals: HashSet<String>,
    seen_approvals: HashSet<String>,
    // Complete snapshots keyed by assistant id prevent repeated SSE updates
    // from charging the same tokens/cost again.
    messages: HashMap<String, Value>,
}

pub struct OpencodeAdapter {
    binding: Binding,
    transport: Arc<dyn OpencodeTransport>,
    state: Mutex<HashMap<Id, RunState>>,
}

impl OpencodeAdapter {
    pub fn with_transport(binding: Binding, transport: Arc<dyn OpencodeTransport>) -> Self {
        Self {
            binding,
            transport,
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn start(
        &self,
        start: &RunStart,
        mut emit: impl FnMut(AdapterEvent),
    ) -> Result<(), MissionRpcError> {
        if self.binding.provider_id.is_empty()
            || self.binding.model_id.is_empty()
            || self.binding != start.binding
        {
            return Err(error(
                Code::InvalidArgument,
                "OpenCode launch binding does not match the adapter",
            ));
        }
        {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            // A transport owns one server/session/run, even after an uncertain
            // session-create response. Starting again could duplicate work.
            if !guard.is_empty() {
                return Err(error(
                    Code::InvalidState,
                    "OpenCode transport already owns a run",
                ));
            }
            guard.insert(
                start.run_id.clone(),
                RunState {
                    task_kind: start.task_kind,
                    token: start.fencing_token,
                    user_message_id: format!("msg_{}", Id::generate().as_str().replace('-', "")),
                    ..Default::default()
                },
            );
        }
        let body = json!({
            "model": {"providerID": self.binding.provider_id, "id": self.binding.model_id},
            "title": format!("iyagi run {}", start.run_id),
            "permission": permissions(start),
        });
        let session = self.transport.post("/session", &body).map_err(|_| {
            self.disconnect(&start.run_id, start.fencing_token, &mut emit);
            error(
                Code::OutcomeUnknown,
                "OpenCode session creation response was not confirmed",
            )
        })?;
        let Some(session_id) = session["id"].as_str().filter(|id| valid_id(id, "ses")) else {
            self.disconnect(&start.run_id, start.fencing_token, &mut emit);
            return Err(error(
                Code::OutcomeUnknown,
                "OpenCode session creation returned an invalid id",
            ));
        };
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(&start.run_id)
            .unwrap()
            .session_id = Some(session_id.to_owned());
        emit(AdapterEvent::Started {
            run_id: start.run_id.clone(),
            fencing_token: start.fencing_token,
            provider_session_id: Some(session_id.to_owned()),
            provider_turn_id: None,
        });
        Ok(())
    }

    pub fn drive_turn(
        &self,
        run_id: &Id,
        prompt: &str,
        mut emit: impl FnMut(AdapterEvent),
    ) -> Result<(), MissionRpcError> {
        let (session, user_message, token, task_kind) = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let run = guard
                .get_mut(run_id)
                .ok_or_else(|| error(Code::NotFound, "run not started"))?;
            if run.claimed || run.terminal || run.interrupted {
                return Err(error(
                    Code::InvalidState,
                    "OpenCode turn cannot be submitted twice",
                ));
            }
            let session = run
                .session_id
                .clone()
                .ok_or_else(|| error(Code::OutcomeUnknown, "session creation was not confirmed"))?;
            run.claimed = true;
            (
                session,
                run.user_message_id.clone(),
                run.token,
                run.task_kind,
            )
        };
        let body = json!({
            "messageID": user_message,
            "model": {"providerID": self.binding.provider_id, "modelID": self.binding.model_id},
            "parts": [{"type":"text", "text":prompt}],
            "format": {"type":"json_schema", "schema":super::codex::task_result_output_schema(task_kind), "retryCount":0},
        });
        let mut submitted = false;
        let mut terminal = false;
        let mut text_parts = HashMap::<String, String>::new();
        let mut retained_bytes = 0usize;
        let stream = self.transport.event_stream("/event", &mut |event| {
            if terminal {
                return false;
            }
            let kind = event["type"].as_str().unwrap_or("");
            if kind == "server.connected" {
                if !submitted {
                    if self.is_interrupted(run_id) {
                        return false;
                    }
                    // Subscribe BEFORE the one POST: events that arrive during
                    // its HTTP response remain buffered on the SSE connection.
                    submitted = true;
                    if self
                        .transport
                        .post(&format!("/session/{session}/prompt_async"), &body)
                        .is_err()
                    {
                        self.disconnect(run_id, token, &mut emit);
                        terminal = true;
                        return false;
                    }
                }
                return true;
            }
            let props = &event["properties"];
            if !submitted || props["sessionID"].as_str() != Some(&session) {
                return true;
            }
            if self.is_interrupted(run_id) {
                return false;
            }
            match kind {
                "message.updated" => {
                    let info = &props["info"];
                    if !owned_message(info, &session, &user_message) {
                        return true;
                    }
                    if !self.matches_model(info) {
                        self.fail(
                            run_id,
                            token,
                            Code::ModelUnavailable,
                            "OpenCode used a different provider or model",
                            &mut emit,
                        );
                        terminal = true;
                        return false;
                    }
                    let id = info["id"].as_str().unwrap().to_owned();
                    let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                    let run = guard.get_mut(run_id).unwrap();
                    if !run.messages.contains_key(&id) && run.messages.len() >= MAX_MESSAGES {
                        drop(guard);
                        self.fail(
                            run_id,
                            token,
                            Code::ContextTooLarge,
                            "OpenCode assistant message limit exceeded",
                            &mut emit,
                        );
                        terminal = true;
                        return false;
                    }
                    let changed = !run.messages.contains_key(&id);
                    if changed {
                        run.turn_id = Some(id.clone());
                    }
                    run.messages.insert(id.clone(), usage_snapshot(info));
                    drop(guard);
                    if changed {
                        emit(AdapterEvent::Started {
                            run_id: run_id.clone(),
                            fencing_token: token,
                            provider_session_id: Some(session.clone()),
                            provider_turn_id: Some(id),
                        });
                    }
                    if info.get("error").is_some_and(|e| !e.is_null()) {
                        self.fail_provider(run_id, token, &info["error"], &mut emit);
                        terminal = true;
                    }
                }
                "message.part.updated" | "message.part.delta" => {
                    let part = if kind == "message.part.updated" {
                        &props["part"]
                    } else {
                        props
                    };
                    let Some(message_id) = part["messageID"].as_str() else {
                        return true;
                    };
                    if !self.owns_assistant(run_id, message_id) {
                        return true;
                    }
                    if kind == "message.part.updated"
                        && (part["sessionID"].as_str() != Some(&session) || part["type"] != "text")
                    {
                        return true;
                    }
                    if kind == "message.part.delta" && part["field"] != "text" {
                        return true;
                    }
                    let id_key = if kind == "message.part.updated" {
                        "id"
                    } else {
                        "partID"
                    };
                    let Some(part_id) = part[id_key].as_str().filter(|id| valid_id(id, "prt"))
                    else {
                        return true;
                    };
                    let Some(text) = part[if kind == "message.part.updated" {
                        "text"
                    } else {
                        "delta"
                    }]
                    .as_str() else {
                        return true;
                    };
                    let key = format!("{message_id}/{part_id}");
                    let old = text_parts.get(&key).map(String::as_str).unwrap_or("");
                    let next_len = if kind == "message.part.delta" {
                        old.len().saturating_add(text.len())
                    } else {
                        text.len()
                    };
                    let total = retained_bytes
                        .saturating_sub(old.len())
                        .saturating_add(next_len);
                    if total > MAX_TEXT_BYTES
                        || (!text_parts.contains_key(&key) && text_parts.len() >= MAX_MESSAGES)
                    {
                        self.fail(
                            run_id,
                            token,
                            Code::ContextTooLarge,
                            "OpenCode activity limit exceeded",
                            &mut emit,
                        );
                        terminal = true;
                        return false;
                    }
                    // starts_with establishes a UTF-8 boundary. Equal byte
                    // lengths with a different prefix are replacements.
                    let chunk = if kind == "message.part.delta" {
                        text
                    } else {
                        text.strip_prefix(old).unwrap_or(text)
                    }
                    .to_owned();
                    let next = if kind == "message.part.delta" {
                        format!("{old}{text}")
                    } else {
                        text.to_owned()
                    };
                    text_parts.insert(key, next);
                    retained_bytes = total;
                    if !chunk.is_empty() {
                        emit(AdapterEvent::Activity {
                            run_id: run_id.clone(),
                            fencing_token: token,
                            chunk,
                        });
                    }
                }
                "permission.asked" => {
                    let Some(id) = props["id"].as_str().filter(|id| valid_id(id, "per")) else {
                        return true;
                    };
                    // A provider permission may have no tool link; it must
                    // still belong to this owned session. A present link must
                    // point to this turn's assistant, not a different message.
                    if let Some(tool) = props.get("tool") {
                        if !tool["messageID"]
                            .as_str()
                            .is_some_and(|id| self.owns_assistant(run_id, id))
                        {
                            return true;
                        }
                    }
                    let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                    let run = guard.get_mut(run_id).unwrap();
                    if run.seen_approvals.len() >= MAX_MESSAGES {
                        return true;
                    }
                    let fresh = run.seen_approvals.insert(id.to_owned());
                    if fresh {
                        run.approvals.insert(id.to_owned());
                    }
                    drop(guard);
                    if fresh {
                        let question =
                            json!({"permission":props["permission"], "patterns":props["patterns"]})
                                .to_string();
                        if question.len() > MAX_TEXT_BYTES {
                            self.fail(
                                run_id,
                                token,
                                Code::ContextTooLarge,
                                "OpenCode permission request too large",
                                &mut emit,
                            );
                            terminal = true;
                        } else {
                            emit(AdapterEvent::ApprovalRequested {
                                run_id: run_id.clone(),
                                fencing_token: token,
                                provider_request_id: id.to_owned(),
                                question,
                            });
                        }
                    }
                }
                "session.error" => {
                    self.fail_provider(run_id, token, &props["error"], &mut emit);
                    terminal = true;
                }
                "session.idle" | "session.status"
                    if kind == "session.idle" || props["status"]["type"] == "idle" =>
                {
                    let latest = {
                        let guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                        let run = guard.get(run_id).unwrap();
                        run.turn_id
                            .as_ref()
                            .and_then(|id| run.messages.get(id))
                            .filter(|info| completed(info))
                            .map(|info| info["id"].as_str().unwrap().to_owned())
                    };
                    // Initial idle and intermediate tool turns are not proof
                    // of a completed answer. Re-read persisted final evidence.
                    if let Some(id) = latest {
                        match self
                            .transport
                            .get(&format!("/session/{session}/message/{id}"))
                        {
                            Ok(saved) => self.finish_saved(
                                run_id,
                                token,
                                &session,
                                &user_message,
                                &id,
                                saved,
                                &mut emit,
                            ),
                            Err(_) => self.disconnect(run_id, token, &mut emit),
                        }
                        terminal = true;
                    }
                }
                _ => {}
            }
            !terminal
        });
        if !terminal {
            // EOF is unknown even if HTTP returned 200 or the stream ended
            // cleanly. Do not resubmit this run after subscription failure.
            self.disconnect(run_id, token, &mut emit);
            return Err(error(
                Code::OutcomeUnknown,
                if stream.is_err() {
                    "OpenCode event subscription disconnected"
                } else {
                    "OpenCode stream ended without final evidence"
                },
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_saved(
        &self,
        run_id: &Id,
        token: u64,
        session: &str,
        user_message: &str,
        id: &str,
        saved: Value,
        emit: &mut dyn FnMut(AdapterEvent),
    ) {
        let info = &saved["info"];
        if !owned_message(info, session, user_message) || info["id"] != id || !completed(info) {
            self.disconnect(run_id, token, emit);
            return;
        }
        if !self.matches_model(info) {
            self.fail(
                run_id,
                token,
                Code::ModelUnavailable,
                "OpenCode final model does not match the binding",
                emit,
            );
            return;
        }
        if info.get("error").is_some_and(|e| !e.is_null()) {
            self.fail_provider(run_id, token, &info["error"], emit);
            return;
        }
        if self.is_interrupted(run_id) {
            self.disconnect(run_id, token, emit);
            return;
        }
        let final_value = info.get("structured").cloned().unwrap_or(Value::Null);
        if final_value.to_string().len() > MAX_RESULT_BYTES {
            self.fail(
                run_id,
                token,
                Code::ContextTooLarge,
                "OpenCode structured result too large",
                emit,
            );
            return;
        }
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(run_id)
            .unwrap()
            .messages
            .insert(id.to_owned(), usage_snapshot(info));
        let (input_tokens, output_tokens, cost_usd_micros) = self.usage(run_id);
        emit(AdapterEvent::Usage {
            run_id: run_id.clone(),
            fencing_token: token,
            input_tokens,
            output_tokens,
            cost_usd_micros,
        });
        match super::parse_provider_result(final_value.clone()) {
            Ok(result) => {
                self.mark_terminal(run_id);
                emit(AdapterEvent::Result {
                    run_id: run_id.clone(),
                    fencing_token: token,
                    result,
                });
            }
            Err(_) => {
                self.mark_terminal(run_id);
                emit(AdapterEvent::InvalidResult {
                    run_id: run_id.clone(),
                    fencing_token: token,
                    code: Code::ResultInvalid,
                    message: "OpenCode final message lacks a valid structured result".into(),
                    rejected_result: Some(final_value.to_string()),
                });
            }
        }
    }

    fn matches_model(&self, info: &Value) -> bool {
        info["providerID"].as_str() == Some(&self.binding.provider_id)
            && info["modelID"].as_str() == Some(&self.binding.model_id)
    }

    fn owns_assistant(&self, run_id: &Id, id: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(run_id)
            .is_some_and(|r| r.messages.contains_key(id))
    }

    fn is_interrupted(&self, run_id: &Id) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(run_id)
            .is_some_and(|r| r.interrupted)
    }

    fn mark_terminal(&self, run_id: &Id) {
        if let Some(run) = self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(run_id)
        {
            run.terminal = true;
            run.approvals.clear();
        }
    }

    fn disconnect(&self, run_id: &Id, token: u64, emit: &mut dyn FnMut(AdapterEvent)) {
        self.mark_terminal(run_id);
        emit(AdapterEvent::Disconnected {
            run_id: run_id.clone(),
            fencing_token: token,
        });
    }

    fn fail(
        &self,
        run_id: &Id,
        token: u64,
        code: Code,
        message: &str,
        emit: &mut dyn FnMut(AdapterEvent),
    ) {
        self.mark_terminal(run_id);
        emit(AdapterEvent::Failed {
            run_id: run_id.clone(),
            fencing_token: token,
            code,
            message: message.to_owned(),
        });
    }

    fn fail_provider(
        &self,
        run_id: &Id,
        token: u64,
        provider_error: &Value,
        emit: &mut dyn FnMut(AdapterEvent),
    ) {
        // Provider bodies and headers can contain credentials; report only
        // normalized, fixed diagnostics, never raw upstream text.
        if let Some(observation) =
            super::rate_limits::opencode(provider_error, super::rate_limits::unix_millis())
        {
            emit(AdapterEvent::RateLimited {
                run_id: run_id.clone(),
                fencing_token: token,
                observation,
            });
        }
        let code = match provider_error["name"].as_str() {
            Some("ProviderAuthError") => Code::AuthRequired,
            Some("ContextOverflowError" | "MessageOutputLengthError") => Code::ContextTooLarge,
            Some("StructuredOutputError") => Code::ResultInvalid,
            Some("MessageAbortedError") => Code::OutcomeUnknown,
            Some("APIError") if provider_error["data"]["statusCode"] == 429 => {
                Code::ProviderRateLimited
            }
            _ => Code::ProviderUnavailable,
        };
        if code == Code::OutcomeUnknown {
            self.disconnect(run_id, token, emit);
        } else {
            self.fail(
                run_id,
                token,
                code,
                "OpenCode reported a provider error",
                emit,
            );
        }
    }

    pub fn interrupt(&self, run_id: &Id) -> CancelReceipt {
        let session = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let Some(run) = guard.get_mut(run_id) else {
                return CancelReceipt::Rejected {
                    reason: CancelRejected::UnknownRun,
                };
            };
            run.interrupted = true;
            run.approvals.clear();
            run.session_id.clone()
        };
        let Some(session) = session else {
            return CancelReceipt::Accepted;
        };
        match self
            .transport
            .post(&format!("/session/{session}/abort"), &json!({}))
        {
            Ok(value) if value == true => CancelReceipt::Accepted,
            _ => CancelReceipt::Rejected {
                reason: CancelRejected::Other("OpenCode abort was not acknowledged".into()),
            },
        }
    }

    pub fn send_message(&self, _run_id: &Id, _body: &str) -> DeliveryReceipt {
        DeliveryReceipt::Queued {
            reason: QueuedReason::SteerUnsupported,
        }
    }

    pub fn answer(&self, run_id: &Id, request_id: &str, answer: &str) -> DeliveryReceipt {
        let reply = match answer {
            "approve" | "accept" | "once" => "once",
            "reject" | "deny" => "reject",
            _ => {
                return DeliveryReceipt::Rejected {
                    reason: "unsupported permission answer",
                }
            }
        };
        {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let Some(run) = guard.get_mut(run_id) else {
                return DeliveryReceipt::Rejected {
                    reason: "unknown run",
                };
            };
            if run.terminal || run.interrupted || !run.approvals.remove(request_id) {
                return DeliveryReceipt::Rejected {
                    reason: "permission request is not pending for this run",
                };
            }
        }
        // Claim before posting; no retry after an ambiguous HTTP response.
        match self.transport.post(
            &format!("/permission/{request_id}/reply"),
            &json!({"reply":reply}),
        ) {
            Ok(value) if value == true => DeliveryReceipt::Delivered {
                provider_ref: Some(request_id.to_owned()),
            },
            Ok(value) if value == false => DeliveryReceipt::Rejected {
                reason: "permission reply was rejected",
            },
            _ => DeliveryReceipt::Unknown {
                reason: "permission response was not confirmed",
            },
        }
    }

    pub fn provider_ids(&self, run_id: &Id) -> (Option<String>, Option<String>) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(run_id)
            .map(|r| (r.session_id.clone(), r.turn_id.clone()))
            .unwrap_or((None, None))
    }

    pub fn usage(&self, run_id: &Id) -> (Option<u64>, Option<u64>, Option<u64>) {
        let guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(run) = guard.get(run_id).filter(|r| !r.messages.is_empty()) else {
            return (None, None, None);
        };
        let sum = |field: &str| {
            run.messages.values().try_fold(0u64, |total, v| {
                total.checked_add(v["tokens"][field].as_u64()?)
            })
        };
        let cost = run.messages.values().try_fold(0u64, |total, v| {
            let micros = v["cost"].as_f64()? * 1_000_000.0;
            if !micros.is_finite() || micros < 0.0 || micros.round() >= u64::MAX as f64 {
                return None;
            }
            total.checked_add(micros.round() as u64)
        });
        (sum("input"), sum("output"), cost)
    }

    /// Server/process liveness is separate from result validity. GET 200,
    /// session idle and a terminal protocol event do not prove child exit.
    pub fn inspect(&self, run_id: &Id) -> RunProbe {
        if self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(run_id)
        {
            self.transport.process_probe()
        } else {
            RunProbe::Absent
        }
    }

    pub fn close_run(&self, run_id: &Id) {
        if self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(run_id)
        {
            self.transport.close();
        }
    }

    pub fn probe_metadata(binding: &Binding) -> OpencodeProbe {
        let caps = super::capability_evidence::capabilities_for_binding(
            binding,
            std::env::consts::OS,
            binding.runtime_version.as_deref(),
        );
        OpencodeProbe {
            provider_id: binding.provider_id.clone(),
            model_id: binding.model_id.clone(),
            auth_route: binding.auth_route,
            supported: caps.model_listing.supported,
            reason_code: (!caps.model_listing.supported)
                .then(|| "no_compatibility_evidence".into()),
        }
    }

    pub fn provider_models(&self) -> Result<Value, String> {
        self.transport.get("/config/providers")
    }
}

fn error(code: Code, text: &str) -> MissionRpcError {
    MissionRpcError::new(code, text)
}

fn valid_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && value.len() <= 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn owned_message(info: &Value, session: &str, parent: &str) -> bool {
    info["role"] == "assistant"
        && info["sessionID"] == session
        && info["parentID"] == parent
        && info["id"].as_str().is_some_and(|id| valid_id(id, "msg"))
}

fn completed(info: &Value) -> bool {
    info["time"]["completed"].as_u64().is_some()
        && info["finish"]
            .as_str()
            .is_some_and(|f| f != "tool-calls" && f != "unknown")
}

fn usage_snapshot(info: &Value) -> Value {
    // Retain scalar facts only. Stream snapshots can contain the complete
    // structured answer and arbitrary provider metadata on every update.
    json!({
        "id":info["id"].as_str(),
        "time":{"completed":info["time"]["completed"].as_u64()},
        "finish":info["finish"].as_str().filter(|s|s.len()<=64),
        "tokens":{"input":info["tokens"]["input"].as_u64(),"output":info["tokens"]["output"].as_u64()},
        "cost":info["cost"].as_f64(),
    })
}

fn permissions(start: &RunStart) -> Vec<Value> {
    let rule = |permission: &str, action: &str| json!({"permission":permission,"pattern":"*","action":action});
    let mut rules = vec![rule("*", "deny")];
    for tool in ["read", "glob", "grep"] {
        rules.push(rule(tool, "allow"));
    }
    // No arbitrary shell or delegated agents: native permission rules do
    // not provide an OS sandbox for them. Verification belongs to the daemon.
    if start.workspace_access == WorkspaceAccess::Write {
        rules.push(rule("edit", "allow"));
    }
    if start.allow_network {
        for tool in ["webfetch", "websearch"] {
            rules.push(rule(tool, "allow"));
        }
    }
    rules.push(rule("external_directory", "deny"));
    rules
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpencodeProbe {
    pub provider_id: String,
    pub model_id: String,
    pub auth_route: term_contracts::mission::types::AuthRoute,
    pub supported: bool,
    pub reason_code: Option<String>,
}

pub struct LiveServerPlan {
    pub program: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub startup_timeout: Duration,
}

impl LiveServerPlan {
    pub fn for_binding(binding: &Binding, workdir: PathBuf) -> Self {
        Self {
            program: binding.program.clone(),
            argv: vec![
                "serve".into(),
                "--pure".into(),
                "--port".into(),
                "0".into(),
                "--hostname".into(),
                "127.0.0.1".into(),
            ],
            cwd: workdir,
            startup_timeout: Duration::from_secs(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_match_openapi() {
        let spec: Value = serde_json::from_str(include_str!("fixtures/openapi.json")).unwrap();
        let body = |route: &str| {
            spec["paths"][route]["post"]["requestBody"]["content"]["application/json"]["schema"]
                .clone()
        };
        let create = body("/session");
        assert!(create["properties"]["model"]["properties"]
            .get("id")
            .is_some());
        let prompt = body("/session/{sessionID}/prompt_async");
        assert!(prompt["required"]
            .as_array()
            .unwrap()
            .contains(&json!("parts")));
        assert!(prompt["properties"].get("format").is_some());
        assert_eq!(
            spec["paths"]["/permission/{requestID}/reply"]["post"]["operationId"],
            "permission.reply"
        );
    }
}
