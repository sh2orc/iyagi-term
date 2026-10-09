//! Local IPC wire format: length-prefixed JSON frames and the RPC envelope
//! (spec `01-contracts.md` §3–4).

use std::io::Read;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::RpcError;
use crate::ids::ConnectionId;

pub const PROTOCOL_VERSION: u32 = 1;
/// `u32 LE length + UTF-8 JSON`, hard maximum 65,536 bytes including prefix.
pub const MAX_FRAME_BYTES: usize = 65_536;
/// Nesting depth cap for decoded JSON.
pub const MAX_JSON_DEPTH: usize = 32;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame length {0} exceeds {1}")]
    TooLarge(u32, u32),
    #[error("frame is not valid UTF-8")]
    NotUtf8,
    #[error("invalid JSON: {0}")]
    InvalidJson(String),
    #[error("JSON nesting deeper than {0}")]
    TooDeep(usize),
    #[error("unexpected end of stream while reading header/body")]
    Truncated,
    #[error("I/O error: {0}")]
    Io(String),
}

impl From<std::io::Error> for FrameError {
    fn from(value: std::io::Error) -> Self {
        FrameError::Io(value.to_string())
    }
}

/// Encode a JSON value as one wire frame. Whole-frame budget (including JSON
/// escaping and metadata) is applied first: an envelope whose encoded frame
/// would exceed the cap is rejected before it is ever sent.
pub fn encode_frame(value: &serde_json::Value) -> Result<Vec<u8>, FrameError> {
    let mut body = serde_json::to_vec(value).map_err(|e| FrameError::InvalidJson(e.to_string()))?;
    let total = 4usize + body.len();
    if total > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(total as u32, MAX_FRAME_BYTES as u32));
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.append(&mut body);
    Ok(out)
}

/// Read exactly one frame from `stream`.
pub fn decode_frame(stream: &mut impl Read) -> Result<serde_json::Value, FrameError> {
    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .map_err(|_| FrameError::Truncated)?;
    let len = u32::from_le_bytes(header);
    if len as usize + 4 > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len, MAX_FRAME_BYTES as u32));
    }
    let mut body = vec![0u8; len as usize];
    stream
        .read_exact(&mut body)
        .map_err(|_| FrameError::Truncated)?;
    let text = std::str::from_utf8(&body).map_err(|_| FrameError::NotUtf8)?;
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| FrameError::InvalidJson(e.to_string()))?;
    check_depth(&value, 0).map_err(FrameError::TooDeep)?;
    Ok(value)
}

fn check_depth(value: &serde_json::Value, depth: usize) -> Result<(), usize> {
    if depth > MAX_JSON_DEPTH {
        return Err(depth);
    }
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                check_depth(item, depth + 1)?;
            }
            Ok(())
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                check_depth(item, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// One decoded frame, envelope-level.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Request(RpcRequest),
    Response(RpcResponse),
    Event(RpcEvent),
}

impl Frame {
    pub fn from_json(value: serde_json::Value) -> Result<Self, FrameError> {
        if value.get("method").is_some() {
            Ok(Frame::Request(
                serde_json::from_value(value)
                    .map_err(|e| FrameError::InvalidJson(e.to_string()))?,
            ))
        } else if value.get("event").is_some() {
            Ok(Frame::Event(
                serde_json::from_value(value)
                    .map_err(|e| FrameError::InvalidJson(e.to_string()))?,
            ))
        } else {
            Ok(Frame::Response(
                serde_json::from_value(value)
                    .map_err(|e| FrameError::InvalidJson(e.to_string()))?,
            ))
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Frame::Request(r) => serde_json::to_value(r).expect("request envelope serializes"),
            Frame::Response(r) => serde_json::to_value(r).expect("response envelope serializes"),
            Frame::Event(e) => serde_json::to_value(e).expect("event envelope serializes"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcRequest {
    pub v: u32,
    pub id: String,
    pub method: String,
    pub params: serde_json::Value,
}

impl RpcRequest {
    pub fn new(id: impl Into<String>, method: &str, params: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcResponse {
    pub v: u32,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    pub fn ok(id: impl Into<String>, result: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: id.into(),
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: impl Into<String>, error: RpcError) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: id.into(),
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcEvent {
    pub v: u32,
    pub event: RpcEventKind,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum RpcEventKind {
    #[serde(rename = "workload.changed")]
    WorkloadChanged,
    #[serde(rename = "queue.changed")]
    QueueChanged,
    #[serde(rename = "resource.snapshot")]
    ResourceSnapshot,
    #[serde(rename = "session.output")]
    SessionOutput,
    #[serde(rename = "session.resize_applied")]
    SessionResizeApplied,
    #[serde(rename = "session.flow_blocked")]
    SessionFlowBlocked,
    #[serde(rename = "session.exited")]
    SessionExited,
    #[serde(rename = "session.owner_changed")]
    SessionOwnerChanged,
    /// A view fell behind a rolling journal's retained head: the daemon
    /// detached it and the client must attach again (replay restarts at
    /// `first_seq`). Payload: `{session_id, view_id, epoch, first_seq}`.
    #[serde(rename = "session.replay_required")]
    SessionReplayRequired,
    #[serde(rename = "remote.state_changed")]
    RemoteStateChanged,
    #[serde(rename = "intervention.reported")]
    InterventionReported,
    /// O1 mission projection moved (hint only, never proof — clients refetch
    /// a snapshot or the event tail). Payload: `{mission_id, latest_seq}`.
    #[serde(rename = "mission.changed")]
    MissionChanged,
}

/// First control-connection message (must arrive within 2s).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HelloParams {
    pub client_id: String,
    /// One-shot secret; redacted from every log surface.
    pub token: String,
    pub role: HelloRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum HelloRole {
    Control,
    Data,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HelloResult {
    pub daemon_id: String,
    pub protocol: u32,
    pub connection_id: ConnectionId,
    /// 1-use token bound to the control connection, TTL 5s.
    pub data_token: String,
    pub capabilities: crate::snapshot::Capabilities,
    /// app+daemon build id for the outdated-daemon check
    /// (`format!("{CARGO_PKG_VERSION} ({IYAGI_GIT_SHA})")`). `serde(default)`
    /// keeps backward compat: an OLD daemon predates this field, so it
    /// deserializes to `""` here — which never matches the app's build id and
    /// is therefore correctly read as "outdated".
    #[serde(default)]
    pub daemon_version: String,
}

/// Well-known method names.
pub mod methods {
    pub const HELLO: &str = "hello";
    pub const SYSTEM_SNAPSHOT: &str = "system.snapshot";
    pub const WORKLOAD_LAUNCH: &str = "workload.launch";
    pub const WORKLOAD_CANCEL: &str = "workload.cancel";
    pub const WORKLOAD_REPRIORITIZE: &str = "workload.reprioritize";
    pub const WORKLOAD_UPDATE_POLICY: &str = "workload.update_policy";
    pub const WORKLOAD_PROCESSES: &str = "workload.processes";
    pub const SESSION_ATTACH: &str = "session.attach";
    pub const SESSION_DETACH: &str = "session.detach";
    pub const SESSION_INPUT: &str = "session.input";
    pub const SESSION_RESIZE: &str = "session.resize";
    pub const SESSION_ACK: &str = "session.ack";
    pub const SESSION_TAKE_CONTROL: &str = "session.take_control";
    /// 창이 지금 보고 있는 세션 보고(압력 완화 §1).
    pub const SESSION_FOCUS: &str = "session.focus";
    /// 세션 단위 완화 수동 조작(양보/복원/보호, 압력 완화 §2).
    pub const SESSION_RELIEF: &str = "session.relief";
    /// 데몬 전체 완화 정책 변경(압력 완화 §2 `relief.auto_yield`).
    pub const RELIEF_SET_POLICY: &str = "relief.set_policy";
    pub const WORKLOAD_SUSPEND: &str = "workload.suspend";
    pub const WORKLOAD_RESUME: &str = "workload.resume";
    pub const GUARD_SET_POLICY: &str = "guard.set_policy";
    pub const RETENTION_SET_LIMIT: &str = "retention.set_limit";
    pub const REMOTE_LIST_HOSTS: &str = "remote.list_hosts";
    pub const REMOTE_UPSERT_HOST: &str = "remote.upsert_host";
    pub const REMOTE_REMOVE_HOST: &str = "remote.remove_host";
    pub const REMOTE_TEST_CONNECTION: &str = "remote.test_connection";
    pub const INTERVENTION_REPORT: &str = "intervention.report";
    pub const INTERVENTION_LIST: &str = "intervention.list";
    pub const AGENT_SESSION_REPORT: &str = "agent_session.report";
    pub const AGENT_SESSION_LIST: &str = "agent_session.list";
    pub const AGENT_SESSION_FORGET: &str = "agent_session.forget";
    pub const SESSION_SEARCH: &str = "session.search";
    pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(value: serde_json::Value) -> Frame {
        let bytes = encode_frame(&value).unwrap();
        let mut cursor = std::io::Cursor::new(bytes);
        let decoded = decode_frame(&mut cursor).unwrap();
        Frame::from_json(decoded).unwrap()
    }

    #[test]
    fn frame_roundtrip_preserves_envelope() {
        let req = RpcRequest::new("req-1", methods::SYSTEM_SNAPSHOT, serde_json::json!({}));
        let frame = roundtrip(serde_json::to_value(&req).unwrap());
        assert_eq!(frame, Frame::Request(req));

        let ev = RpcEvent {
            v: 1,
            event: RpcEventKind::QueueChanged,
            payload: serde_json::json!({"revision": 7}),
        };
        let frame = roundtrip(serde_json::to_value(&ev).unwrap());
        assert_eq!(frame, Frame::Event(ev));
    }

    #[test]
    fn oversized_frame_rejected_on_encode_and_decode() {
        let big = serde_json::json!({"data": "x".repeat(70_000)});
        assert!(matches!(
            encode_frame(&big),
            Err(FrameError::TooLarge(_, _))
        ));

        let mut poison = 70_000u32.to_le_bytes().to_vec();
        poison.extend_from_slice(&[0u8; 64]);
        let mut cursor = std::io::Cursor::new(poison);
        assert!(decode_frame(&mut cursor).is_err());
    }

    #[test]
    fn depth_limit_rejects_deep_nesting() {
        let mut value = serde_json::json!(1);
        for _ in 0..40 {
            value = serde_json::json!({ "a": value });
        }
        // encode is fine (size is small); decode must reject the depth.
        let bytes = encode_frame(&value).unwrap();
        let mut cursor = std::io::Cursor::new(bytes);
        assert!(matches!(
            decode_frame(&mut cursor),
            Err(FrameError::TooDeep(_))
        ));
    }

    #[test]
    fn truncated_stream_is_an_error() {
        let value = serde_json::json!({"ok": true});
        let mut bytes = encode_frame(&value).unwrap();
        bytes.truncate(6);
        let mut cursor = std::io::Cursor::new(bytes);
        assert!(matches!(
            decode_frame(&mut cursor),
            Err(FrameError::Truncated)
        ));
    }

    #[test]
    fn hello_result_round_trips_with_daemon_version() {
        let hello = HelloResult {
            daemon_id: "daemon-1".into(),
            protocol: PROTOCOL_VERSION,
            connection_id: ConnectionId::generate(),
            data_token: "tok".into(),
            capabilities: crate::snapshot::Capabilities::observe_only("test"),
            daemon_version: "0.1.0 (abc1234)".into(),
        };
        let json = serde_json::to_string(&hello).unwrap();
        let back: HelloResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, hello);
        assert_eq!(back.daemon_version, "0.1.0 (abc1234)");
    }

    #[test]
    fn hello_result_from_old_daemon_defaults_daemon_version_to_empty() {
        // An OLD daemon predates `daemon_version` and never sends it. It must
        // still deserialize (serde default) so the app connects and reads the
        // empty build id as "outdated" instead of failing the handshake.
        let legacy = serde_json::json!({
            "daemon_id": "old-daemon",
            "protocol": PROTOCOL_VERSION,
            "connection_id": ConnectionId::generate(),
            "data_token": "tok",
            "capabilities": serde_json::to_value(
                crate::snapshot::Capabilities::observe_only("test")
            ).unwrap(),
        });
        let hello: HelloResult = serde_json::from_value(legacy).unwrap();
        assert_eq!(hello.daemon_version, "");
    }
}
