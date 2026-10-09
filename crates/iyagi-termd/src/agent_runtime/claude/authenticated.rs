use super::{
    auth::{AuthScope, PrivateDirectory},
    ExecChildSource, PrintStreamSource,
};
use crate::exec::{input::ExecInput, output::InboxError, MAX_LINE_BYTES};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use term_contracts::mission::{types::Id, MissionErrorCode};

pub(super) struct AuthenticatedSource {
    pub inner: ExecChildSource,
    pub input: ExecInput,
    pub scope: AuthScope,
    pub prompt: Option<Vec<u8>>,
    pub private: Option<PrivateDirectory>,
    pub runtime: tokio::runtime::Handle,
    pub failure: Option<(MissionErrorCode, &'static str)>,
    pub initialized: bool,
}
impl AuthenticatedSource {
    fn fail(&mut self, code: MissionErrorCode, message: &'static str) -> bool {
        self.failure = Some((code, message));
        self.input.close();
        false
    }
    fn send(&mut self, value: &Value) -> bool {
        // This is bounded before any bytes enter the supervised stdin queue.
        let mut bytes = match serde_json::to_vec(value) {
            Ok(bytes) if bytes.len() < MAX_LINE_BYTES => bytes,
            _ => {
                return self.fail(
                    MissionErrorCode::ResultInvalid,
                    "Claude input frame exceeds its byte limit",
                )
            }
        };
        bytes.push(b'\n');
        if self.inner.cancel.load(std::sync::atomic::Ordering::Acquire) {
            self.input.close();
            return false;
        }
        if self.input.write_blocking(&bytes).is_err() {
            return self.fail(
                MissionErrorCode::OutcomeUnknown,
                "Claude input delivery was not confirmed",
            );
        }
        true
    }
    /// The returned account is metadata from this exact process. No second
    /// CLI process checks one configuration while another receives the task.
    fn authenticate(&mut self) -> bool {
        let id = Id::generate().to_string();
        if !self.send(&json!({"type":"control_request","request_id":id,"request":{"subtype":"initialize","hooks":null}})) { return false; }
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut messages = 0;
        loop {
            if self.inner.cancel.load(std::sync::atomic::Ordering::Acquire) {
                self.input.close();
                return false;
            }
            if Instant::now() >= deadline {
                return self.fail(
                    MissionErrorCode::ProviderUnavailable,
                    "Claude authentication initialization timed out",
                );
            }
            if matches!(
                self.inner.handle.output_verdict(),
                crate::exec::OutputVerdict::Invalid { .. }
            ) {
                return self.fail(
                    MissionErrorCode::ResultInvalid,
                    "Claude initialization output exceeded its limit",
                );
            }
            let line = match self.inner.lines.recv_timeout(Duration::from_millis(25)) {
                Ok(line) => line,
                Err(InboxError::Timeout) if !self.inner.handle.stdout_done() => continue,
                _ => {
                    return self.fail(
                        MissionErrorCode::ProviderUnavailable,
                        "Claude initialization stream ended",
                    )
                }
            };
            messages += 1;
            if messages > 256 {
                return self.fail(
                    MissionErrorCode::ResultInvalid,
                    "Claude initialization produced too many messages",
                );
            }
            let value: Value = match serde_json::from_slice(&line) {
                Ok(value) => value,
                Err(_) => {
                    return self.fail(
                        MissionErrorCode::ResultInvalid,
                        "Claude initialization returned invalid JSON",
                    )
                }
            };
            if value["type"] != "control_response" {
                if value["type"] == "control_request" {
                    return self.fail(
                        MissionErrorCode::PolicyDenied,
                        "Claude requested an unsupported initialization action",
                    );
                }
                continue;
            }
            let response = &value["response"];
            if response["request_id"] != id {
                return self.fail(
                    MissionErrorCode::ResultInvalid,
                    "Claude initialization response ID did not match",
                );
            }
            if response["subtype"] != "success" {
                return self.fail(
                    MissionErrorCode::AuthRequired,
                    "Claude initialization was rejected",
                );
            }
            let result = &response["response"];
            if !self.scope.verify_account(result) {
                return self.fail(
                    MissionErrorCode::AuthRequired,
                    "Claude authentication does not match the binding",
                );
            }
            if !self.scope.verify_permissions(result) {
                return self.fail(
                    MissionErrorCode::PolicyDenied,
                    "Claude permissions do not match this task",
                );
            }
            return true;
        }
    }
}
impl PrintStreamSource for AuthenticatedSource {
    fn initialize(&mut self) -> Result<(), (MissionErrorCode, &'static str)> {
        if self.initialized {
            return Ok(());
        }
        if self.failure.is_none() && self.authenticate() {
            self.initialized = true;
            Ok(())
        } else {
            Err(self.failure.unwrap_or((
                MissionErrorCode::OutcomeUnknown,
                "Claude initialization was cancelled",
            )))
        }
    }
    fn next_line(&mut self) -> Option<Vec<u8>> {
        if self.initialize().is_err() {
            return None;
        }
        if self.inner.cancel.load(std::sync::atomic::Ordering::Acquire) {
            self.input.close();
            return None;
        }
        if let Some(prompt) = self.prompt.take() {
            let Ok(prompt) = String::from_utf8(prompt) else {
                self.fail(
                    MissionErrorCode::ResultInvalid,
                    "Claude prompt is not UTF-8",
                );
                return None;
            };
            let sent = self.send(&json!({"type":"user","message":{"role":"user","content":prompt},"parent_tool_use_id":null,"session_id":""}));
            // No SDK hooks, tools or approvals are served by this one-turn
            // path. EOF lets the CLI exit once its final result is written.
            self.input.close();
            if !sent {
                return None;
            }
        }
        let line = self.inner.next_line()?;
        if let Ok(value) = serde_json::from_slice::<Value>(&line) {
            if value["type"] == "control_request" {
                self.fail(
                    MissionErrorCode::PolicyDenied,
                    "Claude requested an unsupported interactive action",
                );
                return None;
            }
        }
        Some(line)
    }
    fn exit_code(&mut self) -> Option<i32> {
        self.inner.exit_code()
    }
    fn failure(&self) -> Option<(MissionErrorCode, &'static str)> {
        self.failure
    }
    fn task_not_submitted(&self) -> bool {
        self.prompt.is_some()
    }
    fn requires_structured_result(&self) -> bool {
        true
    }
}
impl Drop for AuthenticatedSource {
    fn drop(&mut self) {
        self.input.close();
        let exec = self.inner.handle.clone();
        let private = self.private.clone();
        self.runtime.spawn_blocking(move || {
            while exec
                .stop_blocking(Duration::from_secs(10), Duration::from_secs(5))
                .is_err()
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            if let Some(private) = private {
                private.lock().unwrap_or_else(|p| p.into_inner()).take();
            }
        });
    }
}
