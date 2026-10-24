//! control/data 연결 쌍과 typed RPC 표면(T03/T04).
//!
//! * data token은 control hello가 발급한 **일회용**(TTL 5s)이다 — 재사용하지
//!   않고, control 연결 직후 즉시 소비한다(§2.2-4/5).
//! * `daemon_id`가 바뀌면 revision 기준을 초기화한다(§5.2) — 재연결
//!   호출자가 `Client::daemon_id`로 감지한다.
//! * 일반 RPC timeout(5s)의 `Busy` 하나로 연결을 끊지 않는다(§5.2).
//! * 이벤트는 두 연결에서 하나의 채널로 합쳐진다. 소비가 느려 예산이 넘치면
//!   이벤트를 버리지 않고 연결 사망(`ControlDied`/`DataDied`)으로 알린다.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use term_contracts::agent_session::{
    AgentSessionForgetParams, AgentSessionForgetResult, AgentSessionListParams, AgentSessionRecord,
};
use term_contracts::error::ErrorCode;
use term_contracts::gateway::{GatewayPolicy, GatewayPolicyParams, GatewayStatus};
use term_contracts::ids::{RequestId, SessionId, ViewId, WorkloadId};
use term_contracts::intervention::InterventionNotice;
use term_contracts::launch::{LaunchPolicy, LaunchRequest};
use term_contracts::rpc::{Frame, HelloResult, HelloRole, RpcRequest};
use term_contracts::session::{
    AttachAccess, AttachParams, AttachResult, GuardPolicyParams, InputParams, InputResult,
    ReliefAction, ReliefPolicyParams, ResizeParams, ResizeResult, SessionAck, SessionFocusParams,
    SessionFocusResult, SessionReliefParams, SessionReliefResult, SessionSearchParams,
    SessionSearchResult, WorkloadGuardResult, WorkloadSuspendParams,
};
use term_contracts::snapshot::{GuardPolicy, QueueEntry, ReliefPolicy, Snapshot, WorkloadSummary};
use term_contracts::state::WorkloadState;
use term_contracts::U64String;

use crate::connection::{BridgeError, Connection, EventDelivery, EventPolicy};
use crate::events::{self, DaemonEvent, TakeControlResult};
use crate::launcher::{self, DaemonSpawner, OsDaemonSpawner};
use crate::transport::{OsTransport, Transport};

/// 이벤트 채널 용량(항목 수) — control은 알림 위주, data는 출력 기록 위주.
const CONTROL_EVENT_CAPACITY: usize = 256;
const DATA_EVENT_CAPACITY: usize = 512;
/// 큐에 남아 있어도 되는 총 바이트(전송 예산, §9.3). 항목 수만 제한하지 않게.
const CONTROL_EVENT_BYTES: usize = 4 * 1024 * 1024;
const DATA_EVENT_BYTES: usize = 16 * 1024 * 1024;

/// 연결 실패 원인 구분(T04 완료 기준: protocol mismatch, 데몬 없음, 인증
/// 실패, unsupported를 구분해서 표시).
#[derive(Debug)]
pub enum ConnectError {
    /// RPC/전송 오류. `summary()`가 코드별로 구분한다(PROTOCOL_MISMATCH,
    /// INVALID_ARGUMENT=token 거부, DAEMON_UNAVAILABLE 등).
    Bridge(BridgeError),
    /// 시작할 데몬 바이너리를 찾지 못했다(메시지에 설치 위치 안내 포함).
    DaemonNotFound(String),
    /// 시작했지만 ready 예산 안에 runtime 파일이 나타나지 않았다.
    DaemonNotReady(String),
    /// `--no-start-daemon` 실행에서 실행 중 데몬이 없다.
    NotRunning(String),
}

impl From<BridgeError> for ConnectError {
    fn from(value: BridgeError) -> Self {
        ConnectError::Bridge(value)
    }
}

impl ConnectError {
    /// 오류를 사람이 읽는 요약으로. 토큰 원문은 절대 포함하지 않는다.
    pub fn summary(&self) -> String {
        match self {
            ConnectError::Bridge(err) => {
                let (code, _retryable, message) = err.rpc_status();
                match code {
                    ErrorCode::ProtocolMismatch => format!("protocol mismatch: {message}"),
                    ErrorCode::InvalidArgument => format!("auth failed: {message}"),
                    _ => format!("daemon unavailable: {message}"),
                }
            }
            ConnectError::DaemonNotFound(m)
            | ConnectError::DaemonNotReady(m)
            | ConnectError::NotRunning(m) => m.clone(),
        }
    }

    /// 재시도(재연결) 의미가 있는 실패인가.
    pub fn retryable(&self) -> bool {
        match self {
            ConnectError::Bridge(err) => err.rpc_status().1,
            ConnectError::NotRunning(_) => true,
            _ => false,
        }
    }
}

/// `workload.launch` 결과(dispatch.rs `launch_result`).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct LaunchOutcome {
    pub workload_id: WorkloadId,
    /// 큐 대기 중에는 PTY가 없다(null) — 정상 상태다(U14).
    #[serde(default)]
    pub session_id: Option<SessionId>,
    pub state: WorkloadState,
    pub effective_policy: LaunchPolicy,
    #[serde(default)]
    pub missing_capabilities: Vec<String>,
}

/// `workload.cancel` 결과.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct CancelResult {
    pub state: WorkloadState,
}

/// `workload.reprioritize` 결과.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ReprioritizeResult {
    #[serde(default)]
    pub queue: Vec<QueueEntry>,
}

/// `workload.update_policy` 결과(갱신된 workload 요약).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct UpdatePolicyResult {
    pub workload: WorkloadSummary,
}

/// `workload.processes`의 구성원 한 줄(dispatch 투영 — name은 identity
/// 프로브가 수집하지 않아 null이다. `measured=false`와 함께 표시).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ProcessSummary {
    pub identity: term_contracts::ids::ProcessIdentity,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub measured: bool,
}

/// `workload.processes` 페이지.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ProcessesPage {
    #[serde(default)]
    pub processes: Vec<ProcessSummary>,
    #[serde(default)]
    pub next_cursor: Option<u64>,
}

/// `retention.set_limit` 결과(dispatch `{session_id, max_bytes}`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RetentionLimitResult {
    pub session_id: SessionId,
    pub max_bytes: U64String,
}

/// 클라이언트가 소비자에게 흘리는 이벤트.
#[derive(Debug)]
pub enum ClientEvent {
    /// 데몬 이벤트(해석 성공 또는 알 수 없는 종류의 원문 보존).
    Daemon(DaemonEvent),
    /// control 연결이 끊겼다(overflow=true면 전송 예산 초과).
    ControlDied { overflow: bool },
    /// data 연결이 끊겼다.
    DataDied { overflow: bool },
}

/// 연결된 클라이언트 핸들.
pub struct Client {
    pub(crate) control: Arc<Connection>,
    data: Arc<Connection>,
    hello: HelloResult,
    /// 재시작 제안용: 디스크 바이너리 기준으로 실행 중 데몬이 오래됐는가.
    daemon_outdated: bool,
    revision: Arc<AtomicU64>,
    request_seq: AtomicU64,
}

impl Client {
    /// 데몬이 밝힌 자기 id(daemon_id 변경 감지용).
    pub fn daemon_id(&self) -> &str {
        &self.hello.daemon_id
    }

    pub fn capabilities(&self) -> &term_contracts::snapshot::Capabilities {
        &self.hello.capabilities
    }

    pub fn hello(&self) -> &HelloResult {
        &self.hello
    }

    /// 재시작이 다른 데몬을 띄울 것인지(실행 중 데몬이 오래됐는지).
    pub fn daemon_outdated(&self) -> bool {
        self.daemon_outdated
    }

    /// 지금까지 본 이벤트 revision 최댓값.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    pub fn is_alive(&self) -> bool {
        !self.control.is_closed() && !self.data.is_closed()
    }

    pub fn data_overflowed(&self) -> bool {
        self.data.overflowed()
    }

    fn next_request_id(&self, label: &str) -> String {
        let seq = self.request_seq.fetch_add(1, Ordering::Relaxed);
        format!("{label}-{seq}-{}", Uuid::new_v4())
    }

    async fn call(&self, label: &str, method: &str, params: Value) -> Result<Value, BridgeError> {
        self.control
            .call(&self.next_request_id(label), method, params)
            .await
    }

    /// 두 연결을 닫는다. 데몬과 워크로드는 유지된다(§7.3).
    pub fn shutdown(&self) {
        self.control.close();
        self.data.close();
    }

    // ------------------------------------------------------------ 조회 RPC

    pub async fn system_snapshot(&self) -> Result<Snapshot, BridgeError> {
        let value = self
            .call(
                "snapshot",
                term_contracts::rpc::methods::SYSTEM_SNAPSHOT,
                Value::Null,
            )
            .await?;
        from_result(value, "system.snapshot")
    }

    pub async fn gateway_status(&self) -> Result<GatewayStatus, BridgeError> {
        let value = self
            .call(
                "gateway",
                term_contracts::rpc::methods::GATEWAY_STATUS,
                Value::Null,
            )
            .await?;
        from_result(value, "gateway.status")
    }

    pub async fn intervention_list(&self) -> Result<Vec<InterventionNotice>, BridgeError> {
        let value = self
            .call(
                "intervention",
                term_contracts::rpc::methods::INTERVENTION_LIST,
                Value::Null,
            )
            .await?;
        from_result(value, "intervention.list")
    }

    pub async fn agent_session_list(
        &self,
        params: AgentSessionListParams,
    ) -> Result<Vec<AgentSessionRecord>, BridgeError> {
        let value = self
            .call(
                "agents",
                term_contracts::rpc::methods::AGENT_SESSION_LIST,
                serde_json::to_value(params).expect("list params serialize"),
            )
            .await?;
        from_result(value, "agent_session.list")
    }

    pub async fn agent_session_forget(
        &self,
        params: AgentSessionForgetParams,
    ) -> Result<AgentSessionForgetResult, BridgeError> {
        let value = self
            .call(
                "agents",
                term_contracts::rpc::methods::AGENT_SESSION_FORGET,
                serde_json::to_value(params).expect("forget params serialize"),
            )
            .await?;
        from_result(value, "agent_session.forget")
    }

    pub async fn session_search(
        &self,
        params: SessionSearchParams,
    ) -> Result<SessionSearchResult, BridgeError> {
        let value = self
            .call(
                "search",
                term_contracts::rpc::methods::SESSION_SEARCH,
                serde_json::to_value(params).expect("search params serialize"),
            )
            .await?;
        from_result(value, "session.search")
    }

    // ------------------------------------------------------------ 실행/제어

    pub async fn workload_launch(
        &self,
        request: LaunchRequest,
    ) -> Result<LaunchOutcome, BridgeError> {
        let value = self
            .call(
                "launch",
                term_contracts::rpc::methods::WORKLOAD_LAUNCH,
                serde_json::to_value(&request).expect("launch request serializes"),
            )
            .await?;
        from_result(value, "workload.launch")
    }

    pub async fn workload_cancel(
        &self,
        request_id: &RequestId,
        workload_id: &WorkloadId,
        force: bool,
    ) -> Result<CancelResult, BridgeError> {
        let params = serde_json::json!({
            "request_id": request_id,
            "workload_id": workload_id,
            "force": force,
        });
        let value = self
            .call(
                "cancel",
                term_contracts::rpc::methods::WORKLOAD_CANCEL,
                params,
            )
            .await?;
        from_result(value, "workload.cancel")
    }

    /// `workload.reprioritize`(대기 중 작업만 — 실행 중은 INVALID_STATE).
    pub async fn workload_reprioritize(
        &self,
        workload_id: &WorkloadId,
        priority: u8,
    ) -> Result<ReprioritizeResult, BridgeError> {
        let params = serde_json::json!({
            "workload_id": workload_id,
            "priority": priority,
        });
        let value = self
            .call(
                "repri",
                term_contracts::rpc::methods::WORKLOAD_REPRIORITIZE,
                params,
            )
            .await?;
        from_result(value, "workload.reprioritize")
    }

    /// `workload.update_policy`(실행 중 자원 정책 변경 — 입찰 예산만,
    /// 이미 배정된 OS 한도는 그대로).
    pub async fn workload_update_policy(
        &self,
        workload_id: &WorkloadId,
        policy: &LaunchPolicy,
    ) -> Result<UpdatePolicyResult, BridgeError> {
        let params = serde_json::json!({
            "workload_id": workload_id,
            "policy": policy,
        });
        let value = self
            .call(
                "policy",
                term_contracts::rpc::methods::WORKLOAD_UPDATE_POLICY,
                params,
            )
            .await?;
        from_result(value, "workload.update_policy")
    }

    /// `workload.processes`(소유 트리 구성원 페이지 — 표시용).
    pub async fn workload_processes(
        &self,
        workload_id: &WorkloadId,
        cursor: Option<u64>,
        limit: u32,
    ) -> Result<ProcessesPage, BridgeError> {
        let params = serde_json::json!({
            "workload_id": workload_id,
            "cursor": cursor,
            "limit": limit,
        });
        let value = self
            .call(
                "procs",
                term_contracts::rpc::methods::WORKLOAD_PROCESSES,
                params,
            )
            .await?;
        from_result(value, "workload.processes")
    }

    pub async fn workload_suspend(
        &self,
        request_id: &RequestId,
        workload_id: &WorkloadId,
    ) -> Result<WorkloadGuardResult, BridgeError> {
        let params = serde_json::to_value(WorkloadSuspendParams {
            request_id: request_id.clone(),
            workload_id: workload_id.clone(),
        })
        .expect("suspend params serialize");
        let value = self
            .call(
                "suspend",
                term_contracts::rpc::methods::WORKLOAD_SUSPEND,
                params,
            )
            .await?;
        from_result(value, "workload.suspend")
    }

    pub async fn workload_resume(
        &self,
        request_id: &RequestId,
        workload_id: &WorkloadId,
    ) -> Result<WorkloadGuardResult, BridgeError> {
        let params = serde_json::to_value(WorkloadSuspendParams {
            request_id: request_id.clone(),
            workload_id: workload_id.clone(),
        })
        .expect("resume params serialize");
        let value = self
            .call(
                "resume",
                term_contracts::rpc::methods::WORKLOAD_RESUME,
                params,
            )
            .await?;
        from_result(value, "workload.resume")
    }

    // ------------------------------------------------------------ 세션

    pub async fn session_attach(
        &self,
        session_id: &SessionId,
        view_id: &ViewId,
        access: AttachAccess,
        max_replay_bytes: Option<u64>,
    ) -> Result<AttachResult, BridgeError> {
        let params = AttachParams {
            session_id: session_id.clone(),
            view_id: view_id.clone(),
            access,
            // M1은 디스크 화면 스냅샷이 없다 — resume_from_seq를 보내지 않는다.
            resume_from_seq: None,
            max_replay_bytes: max_replay_bytes
                .filter(|bytes| *bytes > 0)
                .and_then(|bytes| U64String::new(bytes).ok()),
        };
        let value = self
            .call(
                "attach",
                term_contracts::rpc::methods::SESSION_ATTACH,
                serde_json::to_value(&params).expect("attach params serialize"),
            )
            .await?;
        from_result(value, "session.attach")
    }

    pub async fn session_detach(
        &self,
        session_id: &SessionId,
        view_id: &ViewId,
    ) -> Result<Value, BridgeError> {
        let params = serde_json::json!({
            "session_id": session_id,
            "view_id": view_id,
        });
        self.call(
            "detach",
            term_contracts::rpc::methods::SESSION_DETACH,
            params,
        )
        .await
    }

    pub async fn session_input(
        &self,
        session_id: &SessionId,
        epoch: &str,
        input_id: &str,
        data_b64: &str,
    ) -> Result<InputResult, BridgeError> {
        let params = InputParams {
            session_id: session_id.clone(),
            epoch: epoch.to_string(),
            input_id: input_id.to_string(),
            data_b64: data_b64.to_string(),
        };
        let value = self
            .call(
                "input",
                term_contracts::rpc::methods::SESSION_INPUT,
                serde_json::to_value(&params).expect("input params serialize"),
            )
            .await?;
        from_result(value, "session.input")
    }

    pub async fn session_resize(
        &self,
        session_id: &SessionId,
        epoch: &str,
        resize_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<ResizeResult, BridgeError> {
        let params = ResizeParams {
            session_id: session_id.clone(),
            epoch: epoch.to_string(),
            resize_id: resize_id.to_string(),
            cols,
            rows,
        };
        let value = self
            .call(
                "resize",
                term_contracts::rpc::methods::SESSION_RESIZE,
                serde_json::to_value(&params).expect("resize params serialize"),
            )
            .await?;
        from_result(value, "session.resize")
    }

    /// data 연결의 send-only ACK(정상 처리 시 별도 응답 없음 — 01 §4).
    /// ACK는 "터미널 상태에 연속 적용 완료"한 지점만 보낸다(§6.3).
    pub async fn session_ack(
        &self,
        session_id: &SessionId,
        epoch: &str,
        through_seq: u64,
    ) -> Result<(), BridgeError> {
        let ack = SessionAck {
            session_id: session_id.clone(),
            epoch: epoch.to_string(),
            // 저널 seq는 i64 경계 안이다(계약). 잘못된 값이 섞이면 여기서
            // 명시적으로 실패하는 편이 조용한 왜곡보다 낫다.
            through_seq: U64String::new(through_seq).expect("ack seq fits the contract range"),
        };
        let request = RpcRequest::new(
            self.next_request_id("ack"),
            term_contracts::rpc::methods::SESSION_ACK,
            serde_json::to_value(&ack).expect("ack serializes"),
        );
        self.data.send_only(request).await
    }

    /// CAS 제어권 획득(§7.1). `expected_owner`가 현재 owner와 다르면
    /// INVALID_STATE로 거절된다. 현재 owner를 모르면 먼저 확보해야 한다 —
    /// `expected_owner=None`은 "owner 없음"을 뜻하며, 다른 writer가 있으면
    /// 실패한다(이 실패를 무한 재시도하지 않는다).
    pub async fn session_take_control(
        &self,
        session_id: &SessionId,
        view_id: &ViewId,
        expected_owner: Option<&ViewId>,
    ) -> Result<TakeControlResult, BridgeError> {
        let params = serde_json::json!({
            "session_id": session_id,
            "view_id": view_id,
            "expected_owner": expected_owner,
        });
        let value = self
            .call(
                "take_control",
                term_contracts::rpc::methods::SESSION_TAKE_CONTROL,
                params,
            )
            .await?;
        from_result(value, "session.take_control")
    }

    /// 이 창이 보고 있는 세션 보고(§7.2). `None`은 아무 세션도 안 본다.
    pub async fn session_focus(
        &self,
        session_id: Option<&SessionId>,
    ) -> Result<SessionFocusResult, BridgeError> {
        let params = SessionFocusParams {
            session_id: session_id.map(|id| id.clone()),
        };
        let value = self
            .call(
                "focus",
                term_contracts::rpc::methods::SESSION_FOCUS,
                serde_json::to_value(&params).expect("focus params serialize"),
            )
            .await?;
        from_result(value, "session.focus")
    }

    /// `retention.set_limit`(세션 저널 상한 상향 — 디스크 여유 확인 후,
    /// 살아 있는 writer에만. cap에 걸린 저널은 올릴 수 없다).
    pub async fn retention_set_limit(
        &self,
        session_id: &SessionId,
        max_bytes: u64,
    ) -> Result<RetentionLimitResult, BridgeError> {
        let params = serde_json::json!({
            "session_id": session_id,
            "max_bytes": max_bytes,
        });
        let value = self
            .call(
                "retention",
                term_contracts::rpc::methods::RETENTION_SET_LIMIT,
                params,
            )
            .await?;
        from_result(value, "retention.set_limit")
    }

    pub async fn session_relief(
        &self,
        session_id: &SessionId,
        action: ReliefAction,
    ) -> Result<SessionReliefResult, BridgeError> {
        let params = SessionReliefParams {
            session_id: session_id.clone(),
            action,
        };
        let value = self
            .call(
                "relief",
                term_contracts::rpc::methods::SESSION_RELIEF,
                serde_json::to_value(&params).expect("relief params serialize"),
            )
            .await?;
        from_result(value, "session.relief")
    }

    pub async fn relief_set_policy(&self, auto_yield: bool) -> Result<ReliefPolicy, BridgeError> {
        let params = ReliefPolicyParams { auto_yield };
        let value = self
            .call(
                "relief_policy",
                term_contracts::rpc::methods::RELIEF_SET_POLICY,
                serde_json::to_value(params).expect("relief policy serializes"),
            )
            .await?;
        from_result(value, "relief.set_policy")
    }

    pub async fn guard_set_policy(&self, policy: GuardPolicy) -> Result<GuardPolicy, BridgeError> {
        let params = GuardPolicyParams { policy };
        let value = self
            .call(
                "guard_policy",
                term_contracts::rpc::methods::GUARD_SET_POLICY,
                serde_json::to_value(&params).expect("guard policy serializes"),
            )
            .await?;
        from_result(value, "guard.set_policy")
    }

    pub async fn gateway_set_policy(
        &self,
        params: GatewayPolicyParams,
    ) -> Result<GatewayPolicy, BridgeError> {
        let value = self
            .call(
                "gateway_policy",
                term_contracts::rpc::methods::GATEWAY_SET_POLICY,
                serde_json::to_value(&params).expect("gateway policy serializes"),
            )
            .await?;
        from_result(value, "gateway.set_policy")
    }
}

/// 결과 해석: 계약 타입으로 못 바꾸면 ProtocolMismatch로 명시적으로 실패.
pub(crate) fn from_result<T: serde::de::DeserializeOwned>(
    value: Value,
    method: &str,
) -> Result<T, BridgeError> {
    serde_json::from_value(value).map_err(|e| {
        BridgeError::Rpc(term_contracts::RpcError::new(
            ErrorCode::ProtocolMismatch,
            format!("{method} result malformed: {e}"),
        ))
    })
}

/// 연결 구성(빌더). transport/spawner 주입으로 테스트는 실제 데몬 없이
/// 동작한다.
pub struct ClientBuilder {
    transport: Arc<dyn Transport>,
    spawner: Arc<dyn DaemonSpawner>,
    locate: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>,
    explicit_binary: Option<PathBuf>,
    auto_start: bool,
    /// hello `daemon_version`과 비교할 이 앱의 build id(§2.2-3 주입 규약).
    app_build_version: String,
    /// 디스크 바이너리 `--version` 조사로 재시작 판정을 정확히 할지.
    probe_binary_version: bool,
    control_policy: EventPolicy,
    data_policy: EventPolicy,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self {
            transport: Arc::new(OsTransport),
            spawner: Arc::new(OsDaemonSpawner),
            locate: Arc::new(|| None),
            explicit_binary: None,
            auto_start: true,
            app_build_version: String::new(),
            probe_binary_version: true,
            control_policy: EventPolicy::bounded(CONTROL_EVENT_CAPACITY, CONTROL_EVENT_BYTES),
            data_policy: EventPolicy::bounded(DATA_EVENT_CAPACITY, DATA_EVENT_BYTES),
        }
    }

    pub fn transport(mut self, transport: Arc<dyn Transport>) -> Self {
        self.transport = transport;
        self
    }

    pub fn spawner(mut self, spawner: Arc<dyn DaemonSpawner>) -> Self {
        self.spawner = spawner;
        self
    }

    pub fn locate(mut self, locate: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>) -> Self {
        self.locate = locate;
        self
    }

    /// `--daemon-bin` 명시 경로(절대 경로).
    pub fn explicit_binary(mut self, path: Option<PathBuf>) -> Self {
        self.explicit_binary = path;
        self
    }

    /// 자동 시작 허용 여부(`--no-start-daemon` → false).
    pub fn auto_start(mut self, allowed: bool) -> Self {
        self.auto_start = allowed;
        self
    }

    /// 이 클라이언트 앱의 build id(빈 값이면 outdated 비교를 하지 않는다).
    pub fn app_build_version(mut self, version: impl Into<String>) -> Self {
        self.app_build_version = version.into();
        self
    }

    pub fn probe_binary_version(mut self, enabled: bool) -> Self {
        self.probe_binary_version = enabled;
        self
    }

    /// data-dir을 아는 호출자용 탐색 주입(GUI 안정 복사본 위치 포함).
    pub fn locate_with_data_dir(self, data_dir: Arc<Path>) -> Self {
        let explicit = self.explicit_binary.clone();
        self.locate(Arc::new(move || {
            launcher::locate(explicit.as_deref(), Some(data_dir.as_ref()))
        }))
    }

    /// 연결한다. 반환된 receiver는 소비자가 계속 읽어야 한다(예산 초과 방지).
    pub async fn connect(
        &self,
        data_dir: &Path,
    ) -> Result<(Client, mpsc::Receiver<ClientEvent>), ConnectError> {
        // 1) runtime 파일이 없으면(필요하고 허용되면) 데몬을 분리 시작한다.
        if !crate::discovery::runtime_files_present(data_dir) {
            if !self.auto_start {
                return Err(ConnectError::NotRunning(format!(
                    "실행 중인 데몬이 없습니다({}). --no-start-daemon 상태에서는 시작하지 않습니다",
                    crate::discovery::runtime_dir(data_dir).display()
                )));
            }
            let binary = (self.locate)()
                .or_else(|| launcher::locate(self.explicit_binary.as_deref(), Some(data_dir)));
            let Some(binary) = binary else {
                return Err(ConnectError::DaemonNotFound(
                    "iyagi-termd 바이너리를 찾을 수 없습니다 — --daemon-bin 으로 절대 경로를 지정하거나 PATH에 설치하세요".into(),
                ));
            };
            launcher::spawn_binary(self.spawner.as_ref(), &binary, data_dir)
                .map_err(ConnectError::from)?;
            let deadline = Instant::now() + launcher::READY_TIMEOUT;
            if !launcher::wait_ready(data_dir, deadline).await {
                return Err(ConnectError::DaemonNotReady(
                    "데몬이 준비 예산(10s) 안에 runtime 파일을 만들지 않았습니다".into(),
                ));
            }
        }

        // 2) endpoint/token 읽기 → control hello → 즉시 data hello.
        let deadline = Instant::now() + launcher::READY_TIMEOUT;
        loop {
            let endpoint =
                crate::discovery::read_runtime_file(&crate::discovery::endpoint_file(data_dir));
            let token =
                crate::discovery::read_runtime_file(&crate::discovery::token_file(data_dir));
            if let (Some(endpoint), Some(token)) = (endpoint, token) {
                // 소켓 연결 실패만으로 runtime lock/token을 지우지 않는다(§5.2).
                match self.transport.connect(&endpoint).await {
                    Ok(stream) => {
                        match Connection::open_with_policy(
                            stream,
                            &token,
                            HelloRole::Control,
                            self.control_policy,
                        )
                        .await
                        {
                            Ok((control, hello, control_events)) => {
                                let hello = hello.expect("control hello returns HelloResult");

                                // 일회용 data token을 즉시 소비한다(TTL 5s).
                                // transport 실패로 data hello가 못 나가면 연결
                                // 쌍 전체를 다시 handshake한다(소비된 token
                                // 재사용 금지 — §5.2).
                                let data_stream = match self.transport.connect(&endpoint).await {
                                    Ok(stream) => stream,
                                    Err(err) => {
                                        control.close();
                                        return Err(ConnectError::from(err));
                                    }
                                };
                                let (data, _ack, data_events) = match Connection::open_with_policy(
                                    data_stream,
                                    // hello가 발급한 일회용 토큰: 재시도 없이 한 번.
                                    &hello.data_token,
                                    HelloRole::Data,
                                    self.data_policy,
                                )
                                .await
                                {
                                    Ok(pair) => pair,
                                    Err(err) => {
                                        control.close();
                                        return Err(ConnectError::from(err));
                                    }
                                };

                                // outdated 판정(안내용, 연결을 끊지 않는다).
                                let daemon_outdated = self.compute_outdated(&hello).await;

                                let control = Arc::new(control);
                                let data = Arc::new(data);
                                let revision = Arc::new(AtomicU64::new(0));
                                let (tx, rx) = mpsc::channel::<ClientEvent>(1024);
                                spawn_forwarder(
                                    control_events,
                                    Arc::clone(&control),
                                    Arc::clone(&revision),
                                    tx.clone(),
                                    true,
                                );
                                spawn_forwarder(
                                    data_events,
                                    Arc::clone(&data),
                                    Arc::clone(&revision),
                                    tx,
                                    false,
                                );
                                return Ok((
                                    Client {
                                        control,
                                        data,
                                        hello,
                                        daemon_outdated,
                                        revision,
                                        request_seq: AtomicU64::new(0),
                                    },
                                    rx,
                                ));
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, "term-client: control connect attempt failed");
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "term-client: transport connect attempt failed");
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(ConnectError::Bridge(BridgeError::daemon_unavailable(
                    "daemon endpoint was not reachable within the timeout",
                )));
            }
            tokio::time::sleep(launcher::READY_POLL).await;
        }
    }

    async fn compute_outdated(&self, hello: &HelloResult) -> bool {
        if self.app_build_version.is_empty() {
            return false;
        }
        let on_disk = if self.probe_binary_version {
            match (self.locate)() {
                Some(binary) => launcher::binary_build_version(&binary).await,
                None => None,
            }
        } else {
            None
        };
        crate::version::restart_would_update(
            &self.app_build_version,
            on_disk.as_deref(),
            &hello.daemon_version,
        )
    }
}

/// 연결의 이벤트를 하나의 채널로 합친다. 연결이 닫히면 Died를 보내고
/// 끝난다. revision-bearing payload로 revision 캐시를 갱신한다(§5.1-9).
fn spawn_forwarder(
    mut events: EventDelivery,
    conn: Arc<Connection>,
    revision: Arc<AtomicU64>,
    tx: mpsc::Sender<ClientEvent>,
    control: bool,
) {
    tokio::spawn(async move {
        while let Some(frame) = events.recv().await {
            if let Some(rev) = frame_revision(&frame) {
                revision.fetch_max(rev, Ordering::SeqCst);
            }
            if let Some(event) = events::parse_lossy(&frame) {
                if tx.send(ClientEvent::Daemon(event)).await.is_err() {
                    return;
                }
            }
        }
        let overflow = conn.overflowed();
        let _ = tx
            .send(if control {
                ClientEvent::ControlDied { overflow }
            } else {
                ClientEvent::DataDied { overflow }
            })
            .await;
    });
}

/// payload의 `revision` 숫자를 꺼낸다(모든 이벤트에 있는 것은 아니다).
fn frame_revision(frame: &Frame) -> Option<u64> {
    let Frame::Event(event) = frame else {
        return None;
    };
    event.payload.get("revision").and_then(|v| v.as_u64())
}
