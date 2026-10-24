//! Wire 이벤트 해석(T03).
//!
//! payload 형식은 데몬 dispatch/state/sessions의 실제 broadcast를 근거로
//! 정의했다(TS 인터페이스가 아니라 — IMPLEMENTATION_PLAN §2.2-12).
//! 특히 `session.owner_changed`는 `{old_owner, new_owner, epoch}`이고
//! `take_control` 결과는 `{epoch, owner}`다(TS의 `owner_view_id`가 아님).
//!
//! 알 수 없는 이벤트 종류(미래 데몬)는 `Unknown`으로 원문을 보존해
//! 조용히 버리지 않는다.

use serde::Deserialize;
use serde_json::Value;
use term_contracts::ids::{SessionId, U64String, ViewId};
use term_contracts::intervention::InterventionNotice;
use term_contracts::metrics::HostSample;
use term_contracts::rpc::{Frame, RpcEventKind};
use term_contracts::session::{SessionExit, SessionOutput};
use term_contracts::snapshot::{QueueEntry, WorkloadSummary};

/// `session.resize_applied` payload(dispatch.rs `PendingResize::complete`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResizeApplied {
    pub session_id: SessionId,
    pub epoch: String,
    pub seq: U64String,
    pub cols: u16,
    pub rows: u16,
}

/// `session.flow_blocked` payload(sessions.rs B16). `blocked=false`는
/// 256 KiB 미소비가 풀렸다는 뜻이다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FlowBlocked {
    pub session_id: SessionId,
    pub view_id: ViewId,
    pub epoch: String,
    pub blocked: bool,
}

/// `session.owner_changed` payload(dispatch.rs writer 교체). raw wire 필드
/// 이름 그대로 — TS adapter의 `owner_view_id`와 다르다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OwnerChanged {
    pub session_id: SessionId,
    /// 강등된 이전 writer(없으면 null).
    #[serde(default)]
    pub old_owner: Option<ViewId>,
    /// 새 writer(take_control/attach writer 경로는 항상 있음).
    #[serde(default)]
    pub new_owner: Option<ViewId>,
    pub epoch: String,
}

/// `session.take_control` 결과 `{epoch, owner}`(dispatch.rs).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TakeControlResult {
    pub epoch: String,
    pub owner: ViewId,
}

/// `session.replay_required` payload(rolling journal가 view를 떼어냈다).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReplayRequired {
    pub session_id: SessionId,
    pub view_id: ViewId,
    pub epoch: String,
    pub first_seq: U64String,
}

/// `queue.changed` payload.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct QueueChanged {
    pub revision: u64,
    #[serde(default)]
    pub queue: Vec<QueueEntry>,
}

/// `resource.snapshot` payload(telemetry_loop). `host` 계약 타입을 그대로
/// 쓰고 압력 수준을 곁들인다.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ResourceSnapshot {
    #[serde(default)]
    pub revision: u64,
    pub host: HostSample,
}

/// `mission.changed` payload(O1 힌트 — 목록/테일 재조회용).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MissionChanged {
    pub mission_id: String,
    pub latest_seq: U64String,
}

/// 해석된 데몬 이벤트.
#[derive(Debug, Clone, PartialEq)]
pub enum DaemonEvent {
    WorkloadChanged(Box<WorkloadSummary>),
    QueueChanged(Box<QueueChanged>),
    ResourceSnapshot(Box<ResourceSnapshot>),
    SessionOutput(SessionOutput),
    ResizeApplied(ResizeApplied),
    FlowBlocked(FlowBlocked),
    Exited(SessionExit),
    OwnerChanged(OwnerChanged),
    ReplayRequired(ReplayRequired),
    InterventionReported(InterventionNotice),
    /// R1 범위 밝힘: remote.state_changed의 payload 스키마는 아직 형식화
    /// 대상이 아니다(SSH 후속 범위). 원문을 보존한다.
    RemoteStateChanged(Value),
    MissionChanged(MissionChanged),
    /// 예약 명령이 발사됐다(종착 결과 — 토스트·알림 센터가 소비).
    ScheduleFired(Box<term_contracts::schedule::ScheduleFiredPayload>),
    /// 네트워크 끊김 자동 continue 상태 변이(armed→fired|disarmed).
    AutoContinueChanged(Box<term_contracts::auto_continue::AutoContinuePayload>),
    /// 알 수 없는 이벤트(미래 데몬). 종류 문자열과 원문 payload 보존.
    Unknown {
        kind: String,
        payload: Value,
    },
}

/// 프레임을 해석한다. Response 프레임은 이벤트가 아니다(None).
pub fn parse(frame: &Frame) -> Option<DaemonEvent> {
    let Frame::Event(event) = frame else {
        return None;
    };
    let payload = &event.payload;
    Some(match event.event {
        RpcEventKind::WorkloadChanged => {
            DaemonEvent::WorkloadChanged(Box::new(from_payload(payload, "workload.changed")?))
        }
        RpcEventKind::QueueChanged => {
            DaemonEvent::QueueChanged(Box::new(from_payload(payload, "queue.changed")?))
        }
        RpcEventKind::ResourceSnapshot => {
            DaemonEvent::ResourceSnapshot(Box::new(from_payload(payload, "resource.snapshot")?))
        }
        RpcEventKind::SessionOutput => {
            DaemonEvent::SessionOutput(from_payload(payload, "session.output")?)
        }
        RpcEventKind::SessionResizeApplied => {
            DaemonEvent::ResizeApplied(from_payload(payload, "session.resize_applied")?)
        }
        RpcEventKind::SessionFlowBlocked => {
            DaemonEvent::FlowBlocked(from_payload(payload, "session.flow_blocked")?)
        }
        RpcEventKind::SessionExited => {
            DaemonEvent::Exited(from_payload(payload, "session.exited")?)
        }
        RpcEventKind::SessionOwnerChanged => {
            DaemonEvent::OwnerChanged(from_payload(payload, "session.owner_changed")?)
        }
        RpcEventKind::SessionReplayRequired => {
            DaemonEvent::ReplayRequired(from_payload(payload, "session.replay_required")?)
        }
        RpcEventKind::RemoteStateChanged => DaemonEvent::RemoteStateChanged(payload.clone()),
        RpcEventKind::InterventionReported => {
            DaemonEvent::InterventionReported(from_payload(payload, "intervention.reported")?)
        }
        RpcEventKind::MissionChanged => {
            DaemonEvent::MissionChanged(from_payload(payload, "mission.changed")?)
        }
        RpcEventKind::ScheduleFired => {
            DaemonEvent::ScheduleFired(Box::new(from_payload(payload, "schedule.fired")?))
        }
        RpcEventKind::AutoContinueChanged => DaemonEvent::AutoContinueChanged(Box::new(
            from_payload(payload, "auto_continue.changed")?,
        )),
    })
}

/// payload 해석 실패는 조용한 무시가 아니라 None으로 올리고 호출자가
/// 로그로 남긴다(원문 payload는 손실되지 않게 Unknown으로 감싸는 호출자
/// 보조도 둔다).
fn from_payload<T: serde::de::DeserializeOwned>(payload: &Value, kind: &str) -> Option<T> {
    match serde_json::from_value(payload.clone()) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::warn!(kind, error = %err, "term-client: malformed event payload");
            None
        }
    }
}

/// payload 해석 실패 프레임을 `Unknown`으로 감싸 보존한다.
pub fn parse_lossy(frame: &Frame) -> Option<DaemonEvent> {
    if let Some(event) = parse(frame) {
        return Some(event);
    }
    let Frame::Event(event) = frame else {
        return None;
    };
    let kind = serde_json::to_value(event.event)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    Some(DaemonEvent::Unknown {
        kind,
        payload: event.payload.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use term_contracts::rpc::RpcEvent;

    fn event_frame(kind: RpcEventKind, payload: Value) -> Frame {
        Frame::Event(RpcEvent {
            v: 1,
            event: kind,
            payload,
        })
    }

    #[test]
    fn owner_changed_uses_raw_wire_fields() {
        let session = SessionId::generate();
        let view = ViewId::generate();
        let frame = event_frame(
            RpcEventKind::SessionOwnerChanged,
            json!({
                "session_id": session,
                "old_owner": null,
                "new_owner": view,
                "epoch": "e-1",
            }),
        );
        let DaemonEvent::OwnerChanged(owner) = parse(&frame).unwrap() else {
            panic!("owner_changed");
        };
        assert_eq!(owner.old_owner, None);
        assert_eq!(owner.new_owner, Some(view));
        assert_eq!(owner.epoch, "e-1");
    }

    #[test]
    fn take_control_result_reads_epoch_and_owner() {
        let view = ViewId::generate();
        let result: TakeControlResult =
            serde_json::from_value(json!({"epoch": "e-2", "owner": view})).unwrap();
        assert_eq!(result.owner, view);
        assert_eq!(result.epoch, "e-2");
    }

    #[test]
    fn replay_required_carries_first_seq_as_string() {
        let frame = event_frame(
            RpcEventKind::SessionReplayRequired,
            json!({
                "session_id": SessionId::generate(),
                "view_id": ViewId::generate(),
                "epoch": "e",
                "first_seq": "42",
            }),
        );
        let DaemonEvent::ReplayRequired(replay) = parse(&frame).unwrap() else {
            panic!("replay_required");
        };
        assert_eq!(replay.first_seq.get(), 42);
    }

    #[test]
    fn malformed_payload_is_preserved_by_lossy_parse() {
        let frame = event_frame(
            RpcEventKind::SessionReplayRequired,
            json!({"session_id": "not-a-uuid"}),
        );
        // 엄격 해석은 실패(None).
        assert!(parse(&frame).is_none());
        // lossy는 종류와 원문을 보존한다.
        match parse_lossy(&frame) {
            Some(DaemonEvent::Unknown { kind, .. }) => {
                assert_eq!(kind, "session.replay_required")
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
