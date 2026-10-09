//! RPC routing (spec `01-contracts.md` §4 — the complete method table).
//!
//! Control connections may call anything; data connections carry ONLY
//! `session.output` events and the `session.ack` method (a second violation
//! closes them).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use serde_json::json;
use term_contracts::error::ErrorCode;
use term_contracts::ids::{ConnectionId, RequestId, SessionId, ViewId, WorkloadId};
use term_contracts::launch::{Enforcement, LaunchPolicy, LaunchRequest, Priority};
use term_contracts::remote::ExecutorChoice;
use term_contracts::rpc::{methods, RpcRequest, RpcResponse};
use term_contracts::session::{
    AttachAccess, AttachParams, AttachResult, GuardPolicyParams, InputParams, InputResult,
    ReliefPolicyParams, ResizeParams, ResizeResult, SessionAck, SessionFocusParams,
    SessionFocusResult, SessionReliefParams, SessionReliefResult, WorkloadGuardResult,
    WorkloadSuspendParams,
};
use term_contracts::state::WorkloadState;
use term_contracts::RpcError;
use term_contracts::U64String;
use term_pty::actor::{ActorError, InputReply};

use crate::orchestrator::{self, LaunchOutcome};
use crate::sessions::{self, SessionEntry, ViewEntry};
use crate::state::{ConnRole, DaemonState};

/// What the connection loop should do after routing one request.
pub enum Outcome {
    /// Send this encoded response frame.
    Reply(serde_json::Value),
    /// A resize was validated and submitted in wire order. Only its
    /// completion waits independently, so other panes can resize together.
    Deferred(std::pin::Pin<Box<dyn std::future::Future<Output = serde_json::Value> + Send>>),
    /// No response (`session.ack` on success — spec §4).
    None,
    /// Send (optional) response, then close the connection.
    Close(Option<serde_json::Value>),
}

/// Route one RPC request. `linked_control` is set for data connections;
/// `violations` tracks this connection's data-role violations.
pub async fn route(
    state: Arc<DaemonState>,
    conn: &ConnectionId,
    role: ConnRole,
    linked_control: Option<ConnectionId>,
    request: &RpcRequest,
    violations: &mut u32,
) -> Outcome {
    // Data connections: ack only. Repeated violations close (spec §3).
    if role == ConnRole::Data && request.method != methods::SESSION_ACK {
        *violations += 1;
        let response = RpcResponse::err(
            request.id.clone(),
            RpcError::new(
                ErrorCode::InvalidArgument,
                "data connections carry session.ack only",
            ),
        );
        let value = serde_json::to_value(&response).unwrap_or_default();
        return if *violations >= 2 {
            Outcome::Close(Some(value))
        } else {
            Outcome::Reply(value)
        };
    }

    // O1 mission methods carry their own error vocabulary with structured
    // details (01 §7); they bypass the R1 RpcError wrapper below and answer
    // with a fully formed response frame. Gate off still answers honestly
    // (CAPABILITY_UNSUPPORTED), never an empty fake result.
    if crate::mission::service::is_mission_method(&request.method) {
        return dispatch_mission(Arc::clone(&state), conn.clone(), request).await;
    }

    let result: Result<Option<serde_json::Value>, RpcError> = match request.method.as_str() {
        methods::HELLO => Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "hello is only valid as the first message",
        )),
        methods::SYSTEM_SNAPSHOT => system_snapshot(&state).map(Some),
        methods::WORKLOAD_LAUNCH => {
            let state = Arc::clone(&state);
            let params = request.params.clone();
            run_blocking(move || {
                let req: LaunchRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::new(ErrorCode::InvalidArgument, e.to_string()))?;
                // R2 remote execution is not implemented. Running a remote
                // request here would execute on the local host while the RPC
                // reports success (fail-open placement) — reject before the
                // orchestrator records or spawns anything. Clients that omit
                // `executor` deserialize to the Local default and pass.
                if req.executor != ExecutorChoice::Local {
                    return Err(RpcError::new(
                        ErrorCode::InvalidArgument,
                        "remote executor is not supported; executor must be local",
                    ));
                }
                orchestrator::launch(state, req).map(|o| Some(launch_result(o)))
            })
            .await
        }
        methods::WORKLOAD_CANCEL => {
            let state = Arc::clone(&state);
            let params = request.params.clone();
            run_blocking(move || {
                let workload_id: WorkloadId = parse_id(&params, "workload_id")?;
                let force = match params.get("force") {
                    None => false,
                    Some(value) => value.as_bool().ok_or_else(|| {
                        RpcError::new(ErrorCode::InvalidArgument, "force must be a boolean")
                    })?,
                };
                let state = orchestrator::cancel_workload_with_force(state, &workload_id, force)?;
                Ok(Some(json!({ "state": state })))
            })
            .await
        }
        methods::WORKLOAD_REPRIORITIZE => workload_reprioritize(&state, &request.params).map(Some),
        methods::WORKLOAD_UPDATE_POLICY => {
            workload_update_policy(&state, &request.params).map(Some)
        }
        methods::WORKLOAD_PROCESSES => workload_processes(&state, &request.params).map(Some),
        methods::SESSION_ATTACH => {
            let state = Arc::clone(&state);
            let conn = conn.clone();
            let params = request.params.clone();
            run_blocking(move || session_attach(&state, &conn, &params).map(Some)).await
        }
        methods::SESSION_DETACH => session_detach(&state, conn, &request.params).map(Some),
        methods::SESSION_INPUT => {
            // Split in two: validation (session/epoch/ownership/guard checks
            // and the `input_in_flight` reservation — a handful of mutex
            // locks, never blocks) runs INLINE in wire order; only the PTY
            // write (up to 750 ms on a stalled program) defers to the
            // blocking pool. Deferring the whole thing let a later
            // `session.take_control`/`session.detach` on the same connection
            // execute first, so this input failed NOT_INPUT_OWNER /
            // STALE_EPOCH and the client answered with a full pane reattach
            // (screen clear + journal replay). Awaiting the write inline,
            // in turn, parked a runtime worker AND the connection's read
            // loop — one pane whose program stopped reading delayed every
            // other pane's keys on the same control connection.
            match session_input_begin(&state, conn, &request.params) {
                Ok(pending) => {
                    let state = Arc::clone(&state);
                    let id = request.id.clone();
                    return Outcome::Deferred(Box::pin(async move {
                        let result =
                            run_blocking(move || session_input_finish(&state, pending).map(Some))
                                .await
                                .map(Option::unwrap_or_default);
                        let response = match result {
                            Ok(value) => RpcResponse::ok(id, value),
                            Err(error) => RpcResponse::err(id, error),
                        };
                        serde_json::to_value(response).unwrap_or_default()
                    }));
                }
                Err(error) => Err(error),
            }
        }
        methods::SESSION_RESIZE => match session_resize(&state, conn, &request.params) {
            Ok(pending) => {
                let state = Arc::clone(&state);
                let conn = conn.clone();
                let id = request.id.clone();
                return Outcome::Deferred(Box::pin(async move {
                    let response = match pending.complete(&state, &conn).await {
                        Ok(value) => RpcResponse::ok(id, value),
                        Err(error) => RpcResponse::err(id, error),
                    };
                    serde_json::to_value(response).unwrap_or_default()
                }));
            }
            Err(error) => Err(error),
        },
        methods::SESSION_ACK => session_ack(&state, linked_control, &request.params),
        // session.search는 저널 수십 MiB를 읽는 블로킹 스캔(스토리지 조회
        // 포함) — async 연결 루프의 워커를 붙잡지 않게 blocking worker로
        // 보낸다(launch/attach와 같은 패턴).
        methods::SESSION_SEARCH => {
            let state = Arc::clone(&state);
            let params = request.params.clone();
            run_blocking(move || session_search(&state, &params).map(Some)).await
        }
        methods::SESSION_TAKE_CONTROL => {
            session_take_control(&state, conn, &request.params).map(Some)
        }
        methods::SESSION_FOCUS => session_focus(&state, conn, &request.params).map(Some),
        // 08 §2: 수동 완화는 OS 호출(프로세스 트리 관측 + setpriority)을
        // 포함한다 — `ResourcePlatform`은 블로킹 계약이므로 async executor가
        // 아니라 blocking worker에서 돈다.
        methods::SESSION_RELIEF => {
            let state = Arc::clone(&state);
            let params = request.params.clone();
            run_blocking(move || session_relief(&state, &params).map(Some)).await
        }
        methods::RELIEF_SET_POLICY => relief_set_policy(&state, &request.params).map(Some),
        methods::WORKLOAD_SUSPEND => {
            let state = Arc::clone(&state);
            let params = request.params.clone();
            run_blocking(move || workload_guard(&state, &params, true).map(Some)).await
        }
        methods::WORKLOAD_RESUME => {
            let state = Arc::clone(&state);
            let params = request.params.clone();
            run_blocking(move || workload_guard(&state, &params, false).map(Some)).await
        }
        methods::GUARD_SET_POLICY => guard_set_policy(&state, &request.params).map(Some),
        methods::INTERVENTION_REPORT => intervention_report(&state, &request.params).map(Some),
        methods::INTERVENTION_LIST => intervention_list(&state).map(Some),
        // 에이전트 세션 식별(02 §8): 모두 스토리지를 건드리므로 blocking
        // worker에서 돈다(launch/cancel과 같은 패턴).
        methods::AGENT_SESSION_REPORT => {
            let state = Arc::clone(&state);
            let params = request.params.clone();
            run_blocking(move || agent_session_report(&state, &params).map(Some)).await
        }
        methods::AGENT_SESSION_LIST => {
            let state = Arc::clone(&state);
            let params = request.params.clone();
            run_blocking(move || agent_session_list(&state, &params).map(Some)).await
        }
        methods::AGENT_SESSION_FORGET => {
            let state = Arc::clone(&state);
            let params = request.params.clone();
            run_blocking(move || agent_session_forget(&state, &params).map(Some)).await
        }
        methods::RETENTION_SET_LIMIT => retention_set_limit(&state, &request.params).map(Some),
        methods::DAEMON_SHUTDOWN => daemon_shutdown(&state, &request.params).map(Some),
        other => Err(RpcError::new(
            ErrorCode::InvalidArgument,
            format!("unknown method {other:?}"),
        )),
    };

    match result {
        Ok(Some(value)) => {
            let response = RpcResponse::ok(request.id.clone(), value);
            Outcome::Reply(serde_json::to_value(&response).unwrap_or_default())
        }
        // session.ack success: no response frame.
        Ok(None) => Outcome::None,
        Err(e) => {
            let response = RpcResponse::err(request.id.clone(), e);
            Outcome::Reply(serde_json::to_value(&response).unwrap_or_default())
        }
    }
}

/// Run a blocking RPC body (launch/cancel do PTY + storage work).
async fn run_blocking<F>(work: F) -> Result<Option<serde_json::Value>, RpcError>
where
    F: FnOnce() -> Result<Option<serde_json::Value>, RpcError> + Send + 'static,
{
    tokio::task::spawn_blocking(work).await.unwrap_or_else(|e| {
        Err(RpcError::new(
            ErrorCode::DaemonUnavailable,
            format!("rpc worker panicked: {e}"),
        ))
    })
}

fn launch_result(outcome: LaunchOutcome) -> serde_json::Value {
    json!({
        "workload_id": outcome.workload_id,
        "session_id": outcome.session_id,
        "state": outcome.state,
        "effective_policy": outcome.effective_policy,
        "missing_capabilities": outcome.missing_capabilities,
    })
}

/// UUID-v4 id parsing via the contract newtypes' `parse`.
trait ParseId {
    fn parse_text(s: &str) -> Result<Self, RpcError>
    where
        Self: Sized;
}

macro_rules! impl_parse_id {
    ($($t:ty),*) => {
        $(impl ParseId for $t {
            fn parse_text(s: &str) -> Result<Self, RpcError> {
                <$t>::parse(s).map_err(|_| {
                    RpcError::new(
                        ErrorCode::InvalidArgument,
                        concat!(stringify!($t), " must be a UUID v4"),
                    )
                })
            }
        })*
    };
}

impl_parse_id!(RequestId, SessionId, ViewId, WorkloadId);

fn parse_id<T: ParseId>(params: &serde_json::Value, field: &str) -> Result<T, RpcError> {
    let text = params
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, format!("{field} missing")))?;
    T::parse_text(text)
}

// ---------------------------------------------------------------------------
// Methods

/// `session.search` (W2): 저널 전체 substring 검색. 스캔은 바이트 예산으로
/// 유계다 — 검색이 데몬의 다른 역할을 굶기지 않게 한다.
fn session_search(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    use term_contracts::session::search_limits as lim;
    use term_contracts::session::{SessionSearchParams, SessionSearchResult};

    let params: SessionSearchParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new(ErrorCode::InvalidArgument, format!("search payload: {e}")))?;
    // 검증과 검색 모두 트림된 질의로 — 앞뒤 공백이 매칭을 흐리지 않게.
    let query = params.query.trim().to_string();
    if query.is_empty() || query.len() > lim::QUERY_MAX {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            format!("query must be 1..={} chars", lim::QUERY_MAX),
        ));
    }
    let needle = if params.case_sensitive {
        query.clone()
    } else {
        query.to_lowercase()
    };

    // 스캔 대상: 살아 있는 세션(먼저) → 종료 세션(스토리지 created_at 역순).
    // live 세션과 저널 경로가 같으므로 스토리지 순회에서는 live에 이미
    // 있는 id는 건너뛴다(중복 스캔 방지). session_id 지정 여부와 무관하게
    // 종료 세션도 항상 후보가 된다 — 특정 세션 검색이 live 여부와
    // 무관하게 동작해야 한다.
    let mut targets: Vec<(term_contracts::ids::SessionId, std::path::PathBuf)> = Vec::new();
    let mut live_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
        for (id, entry) in sessions.iter() {
            if params.session_id.as_ref() == Some(id) || params.session_id.is_none() {
                live_ids.insert(id.to_string());
                targets.push((id.clone(), entry.journal_path.clone()));
            }
        }
    }
    if let Ok(records) = state.storage.sessions() {
        for record in records.into_iter().rev() {
            if record.replay_status == "deleted" || live_ids.contains(record.id.as_str()) {
                continue;
            }
            if params
                .session_id
                .as_ref()
                .is_some_and(|want| *want != record.id)
            {
                continue;
            }
            let path = state.paths.journal(record.id.as_str());
            if path.is_file() {
                targets.push((record.id, path));
            }
        }
    }

    let mut matches: Vec<term_contracts::SessionSearchMatch> = Vec::new();
    let mut truncated = false;
    let mut total_scanned: u64 = 0;
    let mut scanned = 0u32;
    for (session_id, path) in targets {
        if total_scanned >= lim::TOTAL_SCAN_BYTES {
            truncated = true;
            break;
        }
        scanned += 1;
        let outcome = crate::search_scan::scan_journal(&path, &params, &needle);
        total_scanned += outcome.scanned_bytes;
        if outcome.truncated {
            truncated = true;
        }
        matches.extend(crate::search_scan::matches_for_session(
            &session_id,
            outcome.matches,
        ));
        if matches.len() >= lim::MAX_MATCHES {
            truncated = true;
            break;
        }
    }

    // 세션 내 seq 내림차순(최근 것부터), 상한 초과분은 자른다.
    matches.sort_by(|a, b| {
        a.session_id
            .as_str()
            .cmp(b.session_id.as_str())
            .then(b.seq.cmp(&a.seq))
    });
    if matches.len() > lim::MAX_MATCHES {
        matches.truncate(lim::MAX_MATCHES);
        truncated = true;
    }

    let result = SessionSearchResult {
        matches,
        truncated,
        sessions_scanned: scanned,
    };
    serde_json::to_value(&result)
        .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, format!("search encode: {e}")))
}

fn intervention_report(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let report: term_contracts::InterventionReport = serde_json::from_value(params.clone())
        .map_err(|e| {
            RpcError::new(
                ErrorCode::InvalidArgument,
                format!("intervention payload: {e}"),
            )
        })?;
    match state.report_intervention(report)? {
        Some(notice) => serde_json::to_value(notice).map_err(|e| {
            RpcError::new(
                ErrorCode::DaemonUnavailable,
                format!("intervention encode: {e}"),
            )
        }),
        None => Ok(serde_json::json!({"deduplicated": true})),
    }
}

// ---------------------------------------------------------------------------
// 에이전트 세션 식별·복구 (spec `02-runner.md` §8)

/// `agent_session.report`: CLI 공식 hook이 보낸 세션 수명 이벤트.
///
/// hook 등록은 **전역**이라 iyagi 밖에서 실행된 CLI의 신호도 여기로
/// 온다. 어느 pane인지 확정하지 못하면 `recorded: false`로 조용히 무시한다
/// — 남의 실행을 우리 목록에 넣지 않는다.
fn agent_session_report(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    use term_contracts::agent_session::{
        AgentSessionEvent, AgentSessionReport, AgentSessionReportResult, AgentSessionSource,
    };

    let report: AgentSessionReport = serde_json::from_value(params.clone()).map_err(|e| {
        RpcError::new(
            ErrorCode::InvalidArgument,
            format!("agent session payload: {e}"),
        )
    })?;
    report
        .validate()
        .map_err(|message| RpcError::new(ErrorCode::InvalidArgument, message))?;

    let Some((workload_id, matched_pid)) = match_hook_workload(state, &report) else {
        return encode(&AgentSessionReportResult {
            workload_id: None,
            recorded: false,
        });
    };
    let Some(entry) = state.workload_entry(&workload_id) else {
        return encode(&AgentSessionReportResult {
            workload_id: None,
            recorded: false,
        });
    };

    // `clear`/`startup`은 새 id가 발급된 것이므로 관찰로 알아낸 값도
    // 갈아엎는다. 그 밖의 이벤트는 관찰(registry/lock_file)이 이미 다른
    // id를 확정해 뒀으면 건드리지 않는다 — 관찰이 hook보다 확실하다.
    let replaces_resolved = matches!(
        report.event,
        AgentSessionEvent::Clear | AgentSessionEvent::Start
    );
    // 끝났다는 보고는 배지에 세션을 **싣지 않는다**. 예전엔 End도 일단
    // `session_id`를 찍어 둬서, 감시 루프의 다음 틱이 고쳐 줄 때까지
    // 목록이 방금 끝난 대화를 `active: true`로 보여 주고 UI가 복구 대신
    // "이 pane으로 이동"을 권했다.
    let ends_session = report.event == AgentSessionEvent::End;
    let (previous_session, cwd, pty_session_id) = {
        let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        let previous = guard
            .agent
            .as_ref()
            .and_then(|status| status.session_id.clone());
        // A new CLI may report before the process watcher notices the switch.
        // Never attach an OpenCode ID to a stale Claude/Codex badge.
        if !ends_session
            && guard
                .agent
                .as_ref()
                .is_some_and(|status| status.agent != report.agent)
        {
            guard.agent = None;
        }
        match guard.agent.as_mut() {
            // 보고된 id가 지금 싣고 있는 그것이면 세션 칸만 비운다 —
            // 에이전트·pid는 그대로다(프로세스는 아직 살아 있을 수 있고,
            // 다음 관찰이 새 대화를 채운다).
            Some(status) if ends_session => {
                if status.agent == report.agent
                    && status.session_id.as_deref() == Some(report.session_id.as_str())
                {
                    status.session_id = None;
                    status.session_source = None;
                    status.session_name = None;
                    status.session_status = None;
                }
            }
            Some(status) => {
                let resolved_by_observation = matches!(
                    status.session_source,
                    Some(AgentSessionSource::Registry) | Some(AgentSessionSource::LockFile)
                );
                let differs = status.session_id.as_deref() != Some(report.session_id.as_str());
                if !resolved_by_observation || !differs || replaces_resolved {
                    if matched_pid != 0 {
                        status.pid = matched_pid;
                    }
                    status.session_id = Some(report.session_id.clone());
                    status.session_source = Some(AgentSessionSource::Hook);
                }
            }
            // 끝났다는 보고로 배지를 새로 만들 이유는 없다.
            None if ends_session => {}
            None => {
                guard.agent = Some(term_contracts::snapshot::AgentStatus {
                    agent: report.agent.clone(),
                    pid: matched_pid,
                    detected_at_ms: state.now_ms(),
                    session_id: Some(report.session_id.clone()),
                    session_name: None,
                    session_source: Some(AgentSessionSource::Hook),
                    session_status: None,
                    model: None,
                    effort: None,
                    model_source: None,
                });
            }
        }
        (previous, guard.cwd.clone(), guard.session_id.clone())
    };

    match report.event {
        AgentSessionEvent::End => {
            if let Err(error) = state.storage.end_agent_session(
                &workload_id,
                &report.agent,
                &report.session_id,
                "hook_end",
            ) {
                tracing::warn!(workload = %workload_id, %error, "hook session end failed");
            }
        }
        event => {
            // `clear`는 새 id 발급이다 — 직전 대화를 먼저 닫는다.
            if event == AgentSessionEvent::Clear {
                if let Some(previous) = previous_session.filter(|p| *p != report.session_id) {
                    if let Err(error) = state.storage.end_agent_session(
                        &workload_id,
                        &report.agent,
                        &previous,
                        "replaced",
                    ) {
                        tracing::warn!(workload = %workload_id, %error, "hook session replace failed");
                    }
                }
            }
            let upsert = term_storage::AgentSessionUpsert {
                workload_id: workload_id.clone(),
                pty_session_id: Some(pty_session_id),
                agent: report.agent.clone(),
                agent_session_id: report.session_id.clone(),
                cwd: report.cwd.clone().unwrap_or(cwd),
                title: None,
                program: None,
                source: AgentSessionSource::Hook,
            };
            if let Err(error) = state.storage.upsert_agent_session(upsert) {
                tracing::warn!(workload = %workload_id, %error, "hook session upsert failed");
            }
        }
    }

    state.workload_state_changed(&workload_id);
    encode(&AgentSessionReportResult {
        workload_id: Some(workload_id),
        recorded: true,
    })
}

/// hook 보고를 어느 워크로드에 붙일지. 근거가 강한 순:
/// (1) 보고가 들고 온 `workload_id`가 살아 있는 워크로드일 때,
/// (2) `pty_session_id`가 어떤 워크로드의 PTY 세션과 같을 때,
/// (3) 조상 pid 사슬(가까운 순)이 RUNNING 워크로드의 에이전트 pid나 셸
///     pid와 맞을 때. 직접 연결된 보고도 알려진 에이전트/플러그인 pid를 보존한다.
fn match_hook_workload(
    state: &Arc<DaemonState>,
    report: &term_contracts::agent_session::AgentSessionReport,
) -> Option<(WorkloadId, u32)> {
    if let Some(id) = &report.workload_id {
        if let Ok(parsed) = WorkloadId::parse(id) {
            if let Some(entry) = state.workload_entry(&parsed) {
                let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
                return Some((parsed, hook_agent_pid(guard.agent.as_ref(), report)));
            }
        }
    }

    let registry: Vec<_> = state
        .workloads
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .cloned()
        .collect();

    if let Some(session_id) = &report.pty_session_id {
        for entry in &registry {
            let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
            if guard.session_id.as_str() == session_id {
                return Some((
                    guard.workload_id.clone(),
                    hook_agent_pid(guard.agent.as_ref(), report),
                ));
            }
        }
    }

    // 가까운 조상부터 — hook은 CLI의 자식이고 CLI는 셸의 자손이다.
    for pid in &report.ancestor_pids {
        for entry in &registry {
            let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
            if guard.state != WorkloadState::Running {
                continue;
            }
            let agent_pid = guard.agent.as_ref().map(|a| a.pid);
            if agent_pid == Some(*pid) || guard.shell_pid == Some(*pid) {
                return Some((guard.workload_id.clone(), *pid));
            }
        }
    }
    None
}

fn hook_agent_pid(
    current: Option<&term_contracts::snapshot::AgentStatus>,
    report: &term_contracts::agent_session::AgentSessionReport,
) -> u32 {
    current
        .filter(|status| {
            status.agent == report.agent
                && (report.ancestor_pids.is_empty() || report.ancestor_pids.contains(&status.pid))
        })
        .map(|status| status.pid)
        // Our OpenCode plugin spawns the hook directly, without an intervening
        // shell. Bind early reports to its PID so the first watcher tick keeps
        // them instead of discarding a placeholder pid=0.
        .or_else(|| {
            (report.agent == "opencode")
                .then(|| report.ancestor_pids.first().copied())
                .flatten()
        })
        .unwrap_or(0)
}

/// `agent_session.list`: 복구 후보 목록. 지금 살아 있는 pane에서 관찰
/// 중인 대화는 `active`로 표시해 UI가 "복구"가 아니라 "이동"을 권하게 한다.
fn agent_session_list(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    use term_contracts::agent_session::AgentSessionListParams;

    let params: AgentSessionListParams = if params.is_null() {
        AgentSessionListParams::default()
    } else {
        serde_json::from_value(params.clone()).map_err(|e| {
            RpcError::new(
                ErrorCode::InvalidArgument,
                format!("agent session list payload: {e}"),
            )
        })?
    };
    let mut records = state
        .storage
        .list_agent_sessions_filtered(
            params.effective_limit(),
            params.cwd.as_deref(),
            params.workload_id.as_ref(),
            params.pty_session_id.as_ref(),
        )
        .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))?;

    // 살아 있는 관찰: RUNNING 워크로드의 에이전트가 이 세션을 붙들고 있나.
    // 대상 조회가 이전 워크로드의 행을 반환해도, 다른 pane에서 이미 재개한
    // 같은 대화를 다시 실행 가능하다고 표시하지 않는다.
    let live: std::collections::HashSet<(String, String)> = state
        .workloads
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .filter_map(|entry| {
            let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
            if guard.state != WorkloadState::Running {
                return None;
            }
            let agent = guard.agent.as_ref()?;
            Some((agent.agent.clone(), agent.session_id.clone()?))
        })
        .collect();
    for record in &mut records {
        record.active = live.contains(&(record.agent.clone(), record.agent_session_id.clone()));
    }
    // 개수 상한(`limit`)만으로는 프레임 예산을 지킬 수 없다 — cwd·제목·
    // program이 긴 기록 200건이면 64 KiB를 넘는다. 바이트로 한 번 더 깎는다.
    let requested = records.len();
    let bytes = bound_agent_session_list(&mut records);
    if records.len() < requested {
        tracing::warn!(
            requested,
            returned = records.len(),
            bytes,
            "agent session list trimmed to the frame budget"
        );
    }
    encode(&records)
}

/// `agent_session.list` 응답 배열의 바이트 예산: 프레임 64 KiB
/// (`rpc::MAX_FRAME_BYTES`) − envelope 여유분. 응답 프레임에는 배열 말고도
/// `v`/`id`/`result` 래퍼와 JSON 이스케이프가 함께 들어간다.
const AGENT_SESSION_LIST_BUDGET_BYTES: usize = 48 * 1024;

/// 인코딩 길이가 예산 안에 드는 만큼만 남기고 목록 **끝**을 잘라 낸다
/// (목록은 `last_seen_at` 내림차순이므로 가장 오래된 것이 먼저 빠진다).
/// 돌려주는 값은 남은 배열의 인코딩 바이트 수다.
///
/// 깎지 않으면 `ipc`가 프레임 인코딩에서 `TooLarge`를 받고 **연결을 닫는다**
/// — 목록 한 번 때문에 UI의 컨트롤 연결이 통째로 끊기는 것보다 오래된
/// 몇 건을 빼고 보내는 것이 낫다.
///
/// 한 건씩 빼고 전체를 다시 인코딩하면 비용이 건수의 제곱으로 늘어난다.
/// 배열 인코딩은 공백이 없으므로 길이가 `대괄호 2 + 원소 길이 합 +
/// 쉼표(n-1)`로 정확히 쪼개진다 — 원소를 한 번씩만 인코딩해 들어가는
/// 개수를 세면 결과가 같으면서 비용은 선형이다.
fn bound_agent_session_list(
    records: &mut Vec<term_contracts::agent_session::AgentSessionRecord>,
) -> usize {
    let mut total = "[]".len();
    let mut fits = 0usize;
    for record in records.iter() {
        // 문자열·bool·Option뿐이라 직렬화가 실패할 수 없지만, 실패하면
        // 거기서 멈춘다(짧은 목록이 끊긴 연결보다 낫다).
        let Ok(encoded) = serde_json::to_vec(record) else {
            break;
        };
        let separator = usize::from(fits > 0);
        let with_record = total + separator + encoded.len();
        if with_record > AGENT_SESSION_LIST_BUDGET_BYTES {
            break;
        }
        total = with_record;
        fits += 1;
    }
    records.truncate(fits);
    total
}

/// `agent_session.forget`: 목록에서 한 건을 영구히 지운다(사용자 동작).
/// 에이전트 자신의 기록 파일은 건드리지 않는다 — 우리 색인만 지운다.
fn agent_session_forget(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    use term_contracts::agent_session::{AgentSessionForgetParams, AgentSessionForgetResult};

    let params: AgentSessionForgetParams = serde_json::from_value(params.clone()).map_err(|e| {
        RpcError::new(
            ErrorCode::InvalidArgument,
            format!("agent session forget payload: {e}"),
        )
    })?;
    let forgotten = state
        .storage
        .forget_agent_session(&params.id)
        .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))?;
    encode(&AgentSessionForgetResult { forgotten })
}

fn encode<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, format!("encode: {e}")))
}

/// `intervention.list` (W1-5): 최근 개입 알림을 오래된 순으로.
fn intervention_list(state: &Arc<DaemonState>) -> Result<serde_json::Value, RpcError> {
    let recent = state
        .interventions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .recent();
    serde_json::to_value(&recent).map_err(|e| {
        RpcError::new(
            ErrorCode::DaemonUnavailable,
            format!("intervention encode: {e}"),
        )
    })
}

fn system_snapshot(state: &Arc<DaemonState>) -> Result<serde_json::Value, RpcError> {
    let snapshot = state.build_snapshot();
    let value = serde_json::to_value(&snapshot).map_err(|e| {
        RpcError::new(
            ErrorCode::DaemonUnavailable,
            format!("snapshot encode: {e}"),
        )
    })?;
    // Whole-frame budget (01 §2: JSON escaping 포함 64 KiB가 먼저 적용된다).
    // Terminal workloads already omit usage; if a pathological registry still
    // overflows, degrade to summaries without any usage, and only then fail
    // with a retryable error — never a silently oversized frame.
    //
    // The result value travels inside an RpcResponse envelope
    // ({"v":1,"id":"<uuid>","result":...}), so the value budget reserves the
    // prefix plus envelope slack. Sizing the check to the bare value let a
    // 65_521-byte result pass here and then fail frame encode in ipc, which
    // surfaced as client-side CONNECTION_LOST (O01 regression b25).
    const ENVELOPE_RESERVE: usize = 4 + 128;
    let fits = |value: &serde_json::Value| -> Result<bool, RpcError> {
        Ok(serde_json::to_vec(value)
            .map_err(|e| {
                RpcError::new(
                    ErrorCode::DaemonUnavailable,
                    format!("snapshot encode: {e}"),
                )
            })?
            .len()
            + ENVELOPE_RESERVE
            <= term_contracts::rpc::MAX_FRAME_BYTES)
    };
    let encoded_len = |value: &serde_json::Value| -> Result<usize, RpcError> {
        serde_json::to_vec(value)
            .map(|bytes| bytes.len())
            .map_err(|e| {
                RpcError::new(
                    ErrorCode::DaemonUnavailable,
                    format!("snapshot encode: {e}"),
                )
            })
    };
    if fits(&value)? {
        return Ok(value);
    }
    let mut degraded = value;
    if let Some(list) = degraded.get_mut("workloads").and_then(|w| w.as_array_mut()) {
        for summary in list.iter_mut() {
            if let Some(object) = summary.as_object_mut() {
                object.remove("usage");
            }
        }
    }
    if !fits(&degraded)? {
        // Second rung: non-active summaries (QUEUED + terminal) drop their
        // longest strings (cwd/program with escaped separators). Queued work
        // stays fully visible through the queue array + title; active
        // workloads keep everything.
        if let Some(list) = degraded.get_mut("workloads").and_then(|w| w.as_array_mut()) {
            for summary in list.iter_mut() {
                let active = matches!(
                    summary.get("state").and_then(|s| s.as_str()),
                    Some("STARTING" | "RUNNING" | "STOPPING" | "DRAINING")
                );
                if !active {
                    if let Some(object) = summary.as_object_mut() {
                        object.insert("cwd".into(), serde_json::json!(""));
                        object.insert("program".into(), serde_json::json!(""));
                    }
                }
            }
        }
    }
    if fits(&degraded)? {
        tracing::warn!(
            bytes = encoded_len(&degraded)?,
            "snapshot exceeded frame budget; usage metrics omitted"
        );
        return Ok(degraded);
    }
    Err(RpcError::new(
        ErrorCode::Busy,
        format!(
            "snapshot ({} workloads, {} bytes) exceeds the frame budget; query workload.processes pages",
            degraded
                .get("workloads")
                .and_then(|w| w.as_array())
                .map_or(0, Vec::len),
            encoded_len(&degraded)?
        ),
    ))
}

fn workload_reprioritize(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let workload_id: WorkloadId = parse_id(params, "workload_id")?;
    let priority = params
        .get("priority")
        .and_then(|v| v.as_u64())
        .and_then(|v| u8::try_from(v).ok())
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "priority must be 0..=2"))?;
    if priority > 2 {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "priority must be 0..=2",
        ));
    }
    let priority = Priority(priority);

    let entry = state
        .workload_entry(&workload_id)
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "workload not found"))?;
    let (current_state, request_id) = {
        let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        (guard.state, guard.request_id.clone())
    };
    // 실행 중 작업에는 적용하지 않음 (spec §4).
    if current_state != WorkloadState::Queued {
        return Err(RpcError::new(
            ErrorCode::InvalidState,
            "only QUEUED workloads can be reprioritized",
        ));
    }
    {
        let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        guard.priority = priority;
    }
    // Queue re-insert under the new priority (the queue API has no mutate;
    // documented deviation: aging restarts).
    state.queue.cancel(&workload_id);
    if let Some(request_id) = request_id {
        let _ = state
            .queue
            .enqueue(workload_id.clone(), request_id, priority);
    }
    state.broadcast_queue_changed();
    let queue = state.queue.snapshot();
    Ok(json!({ "revision": state.revision(), "queue": queue }))
}

fn workload_update_policy(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let workload_id: WorkloadId = parse_id(params, "workload_id")?;
    let policy: LaunchPolicy = params
        .get("policy")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "policy missing or invalid"))?;
    if policy.cpu_slots == 0 || policy.cpu_slots > 1024 {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "cpu_slots out of 1..=1024",
        ));
    }
    if policy.reservation_bytes.get() == 0 {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "reservation_bytes must be positive",
        ));
    }

    let entry = state
        .workload_entry(&workload_id)
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "workload not found"))?;
    let current = entry.lock().unwrap_or_else(|p| p.into_inner()).state;
    if current != WorkloadState::Queued {
        return Err(RpcError::new(
            ErrorCode::InvalidState,
            "policy changes are QUEUED-only",
        ));
    }

    // Capability re-check for `require`.
    let request_like = LaunchRequest {
        request_id: RequestId::generate(),
        profile_id: String::new(),
        cwd: String::new(),
        program: String::new(),
        argv: Vec::new(),
        env_overrides: Default::default(),
        mode: term_contracts::launch::LaunchMode::Managed,
        executor: term_contracts::remote::ExecutorChoice::Local,
        cols: 80,
        rows: 24,
        priority: Priority(1),
        policy: policy.clone(),
        claude_provider: None,
    };
    let missing = orchestrator::missing_capabilities(state, &request_like);
    if !missing.is_empty() && policy.enforcement == Enforcement::Require {
        return Err(RpcError::new(
            ErrorCode::CapabilityUnavailable,
            "requested limits cannot be enforced on this platform",
        )
        .with_details(json!({ "missing": missing })));
    }

    // Admission-condition re-check (budget/slots) against the live host.
    let host = state.admission_host();
    let admission = state.config.admission_config(state.logical_cpus);
    let budget = admission.managed_budget_bytes(host.total_bytes);
    let slots = admission.cpu_slot_capacity();
    if policy.reservation_bytes.get() > budget || policy.cpu_slots > slots {
        return Err(RpcError::new(
            ErrorCode::ResourceUnschedulable,
            "policy exceeds the managed budget or CPU slot capacity",
        ));
    }

    {
        let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        guard.policy = policy.clone();
        if let Some(descriptor) = guard.descriptor.as_mut() {
            descriptor.policy = policy.clone();
        }
    }
    state.workload_state_changed(&workload_id);
    Ok(json!({ "workload_id": workload_id, "policy": policy }))
}

fn workload_processes(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let workload_id: WorkloadId = parse_id(params, "workload_id")?;
    let cursor = params.get("cursor").and_then(|v| v.as_u64()).unwrap_or(0);
    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(100)
        .clamp(1, 100);

    let entry = state
        .workload_entry(&workload_id)
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "workload not found"))?;
    let (group, shell_pid) = {
        let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        (guard.group.clone(), guard.shell_pid)
    };
    let members = match group {
        Some(group) => state
            .platform
            .member_identities(&group)
            .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))?,
        // 직접 셸에는 자원 그룹이 없다(02-runner §3). 관측 단위는 PTY 루트
        // pid의 프로세스 트리이며(08 §1.2, 04-ui의 "direct shell workload는
        // session 단위 자원 관측"), `usage`가 이미 그 트리를 쓴다. 여기서만
        // 빈 목록을 돌려주면 셸 pane의 프로세스 표가 영영 비어 있게 된다.
        None => match shell_pid {
            Some(root) => crate::telemetry_loop::scan_shell_tree(root),
            // 대기 중이거나 이미 끝난 워크로드: 볼 프로세스가 없다.
            None => Vec::new(),
        },
    };
    let start = (cursor as usize).min(members.len());
    let end = (start + limit as usize).min(members.len());
    let page: Vec<serde_json::Value> = members[start..end]
        .iter()
        .map(|identity| {
            json!({
                "identity": {
                    "pid": identity.pid,
                    "start_token": identity.start_token,
                    "boot_id": identity.boot_id,
                },
                // Process names are not collected by the identity probe;
                // `null` per the metrics contract (never a fake value).
                "name": serde_json::Value::Null,
                "measured": false,
            })
        })
        .collect();
    let next_cursor = if end < members.len() {
        Some(end as u64)
    } else {
        None
    };
    Ok(json!({ "processes": page, "next_cursor": next_cursor }))
}

fn session_attach(
    state: &Arc<DaemonState>,
    conn: &ConnectionId,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let params: AttachParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new(ErrorCode::InvalidArgument, e.to_string()))?;
    let session = crate::session_recovery::load(state, &params.session_id)?;

    // Attach limit: 2 views per session (spec §4).
    {
        let views = session.views.lock().unwrap_or_else(|p| p.into_inner());
        if !views.contains_key(&params.view_id)
            && views.len() >= term_contracts::session::limits::VIEWS_PER_SESSION
        {
            return Err(RpcError::new(
                ErrorCode::InvalidState,
                "session already has the maximum number of views",
            ));
        }
    }

    // Writer exclusivity: a new writer attach demotes the existing writer
    // and emits owner_changed (spec §4: one writer; new writer wins).
    let mut old_owner: Option<ViewId> = None;
    if params.access == AttachAccess::Writer {
        let current = session
            .owner_view
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(owner) = current {
            if owner != params.view_id {
                old_owner = Some(owner);
            }
        }
    }

    let epoch = uuid::Uuid::new_v4().to_string();
    let last_seq = session.last_seq.load(Ordering::Acquire);
    let (cols, rows) = *session.size.lock().unwrap_or_else(|p| p.into_inner());
    // 롤링 저널(02-runner §5): 재생은 보존된 첫 레코드(세그먼트 머리의 크기
    // 레코드)부터다. 잘린 바이트는 UI가 "앞부분 N MiB 지워짐"으로 알린다.
    let head = session.journal_segments.snapshot();
    let retained_from = head.first_seq.max(1);
    // 스냅샷 복원: UI가 이미 `resume_from_seq - 1`까지의 화면을 그렸다. 그 다음
    // 레코드가 아직 보존돼 있으면(헤드 이후, 마지막 레코드 + 1 이하) 거기서부터만
    // 재생한다. 범위를 벗어나면 조용히 무시하고 헤드부터 재생한다 — UI는
    // `replay_from_seq`가 요청값과 같을 때만 스냅샷을 쓴다.
    let replay_from = params
        .resume_from_seq
        .as_ref()
        .map(U64String::get)
        .filter(|seq| *seq >= retained_from && *seq <= last_seq.saturating_add(1))
        .unwrap_or(retained_from);
    // 재생 바이트 예산(AttachParams.max_replay_bytes): 보존된 저널이 예산보다
    // 크면 뒤쪽 세그먼트 머리부터만 재생한다. 건너뛴 앞부분은 잘린 헤드처럼
    // 알린다(replay_dropped_bytes) — UI가 live 전환 때 크기를 흔들어 TUI가
    // 화면을 다시 그리고, 그보다 앞선 스냅샷은 replay_from_seq 불일치로 스스로
    // 버린다. 예산 안의 재개 지점은 그대로 존중한다.
    let (replay_from, skipped_bytes) = match params.max_replay_bytes.as_ref().map(U64String::get) {
        Some(budget) if budget > 0 => crate::sessions::bounded_replay_start(
            &session.journal_path,
            &head,
            &session.journal_offsets,
            replay_from,
            budget,
        ),
        _ => (replay_from, 0),
    };

    {
        let mut views = session.views.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(old) = &old_owner {
            if let Some(view) = views.get_mut(old) {
                view.access = AttachAccess::Reader;
            }
        }
        views.insert(
            params.view_id.clone(),
            ViewEntry {
                view_id: params.view_id.clone(),
                access: params.access,
                conn: conn.clone(),
                epoch: epoch.clone(),
                // 보존된 헤드부터 재생한다(잘리지 않았으면 seq 1, spec §5).
                next_seq: replay_from,
                input_in_flight: false,
            },
        );
    }
    if params.access == AttachAccess::Writer {
        *session.owner_view.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(params.view_id.clone());
    }
    {
        let mut flow = session.flow.lock().unwrap_or_else(|p| p.into_inner());
        flow.attach_view(params.view_id.clone(), epoch.clone());
        // 스냅샷 재개: UI는 이미 `replay_from - 1`까지의 화면을 갖고 있어 그
        // 자리를 ACK한다(아무것도 보내기 전에도). 원장에 밑받침해 두면 그 ACK는
        // "beyond the last sent seq" 오류 없이 중복으로 무시된다.
        if replay_from > 1 {
            flow.seed_view_acked(&params.view_id, &epoch, replay_from - 1);
        }
    }
    // 회수(retire)와의 경쟁: 보관 링에서 밀려난 종료 세션은 뷰가 0인 동안
    // 레지스트리에서 사라질 수 있다. 뷰를 넣은 뒤 한 번 더 확인한다 —
    // 이미 사라졌다면 이 항목은 아무도 도달할 수 없는 쓰레기이므로 알 수
    // 없는 세션과 똑같은 오류로 정직하게 답한다. 방금 넣은 뷰를 도로 빼지
    // 않으면 이 에폭의 원장 없는 뷰가 세션 뷰 수 상한만 차지하고 남는다.
    if state.session(&params.session_id).is_none() {
        session
            .views
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&params.view_id);
        session
            .flow
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .detach_view(&params.view_id);
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "session not found",
        ));
    }
    // A late attach to a finished session (all views detached earlier, pump
    // exited) must restart delivery so the journal replay actually streams
    // (spec 02 §5: 종료 세션 재생; B20/B22).
    crate::sessions::ensure_pump(Arc::clone(state), Arc::clone(&session));
    session.wake();
    if let Some(entry) = state.workload_entry(&session.workload_id) {
        entry.lock().unwrap_or_else(|p| p.into_inner()).connection =
            term_contracts::state::TerminalConnection::Attached;
    }
    if let Some(old) = old_owner {
        state.bump_revision();
        state.broadcast_control(
            term_contracts::rpc::RpcEventKind::SessionOwnerChanged,
            json!({
                "session_id": params.session_id,
                "old_owner": old,
                "new_owner": params.view_id,
                "epoch": epoch,
            }),
        );
    }

    let result = AttachResult {
        epoch,
        replay_from_seq: U64String::new(replay_from).expect("seq fits i64"),
        last_seq: U64String::new(last_seq).expect("fits"),
        cols,
        rows,
        replay_dropped_bytes: (head.dropped_bytes.saturating_add(skipped_bytes) > 0).then(|| {
            U64String::new(head.dropped_bytes.saturating_add(skipped_bytes)).expect("bytes fit i64")
        }),
        exited: Some(session.actor_finalized.load(Ordering::Acquire)),
    };
    serde_json::to_value(&result)
        .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))
}

fn session_detach(
    state: &Arc<DaemonState>,
    conn: &ConnectionId,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let session_id: SessionId = parse_id(params, "session_id")?;
    let view_id: ViewId = parse_id(params, "view_id")?;
    let session = state
        .session(&session_id)
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "session not found"))?;
    let removed = {
        let mut views = session.views.lock().unwrap_or_else(|p| p.into_inner());
        match views.get(&view_id) {
            Some(view) if &view.conn == conn => views.remove(&view_id),
            _ => {
                return Err(RpcError::new(
                    ErrorCode::InvalidArgument,
                    "view not attached by this connection",
                ))
            }
        }
    };
    if removed.is_some_and(|v| v.access == AttachAccess::Writer) {
        let mut owner = session.owner_view.lock().unwrap_or_else(|p| p.into_inner());
        if owner.as_ref() == Some(&view_id) {
            *owner = None;
        }
    }
    session
        .flow
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .detach_view(&view_id);
    state.bump_revision();
    if session
        .views
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty()
    {
        if let Some(entry) = state.workload_entry(&session.workload_id) {
            entry.lock().unwrap_or_else(|p| p.into_inner()).connection =
                term_contracts::state::TerminalConnection::Detached;
        }
        // 마지막 뷰가 떠났다: 이미 종료했고 보관 링 밖인 세션은 여기서
        // 회수한다(살아 있는 세션은 절대 건드리지 않는다).
        state.retire_session_if_cold(&session_id);
    }
    Ok(json!({ "detached": true }))
}

/// Detach every view a closing control connection owned (slow-reader / lost
/// client semantics: only the connection goes away, spec §4).
pub fn detach_all_views_of(state: &Arc<DaemonState>, conn: &ConnectionId) {
    let sessions: Vec<Arc<SessionEntry>> = state
        .sessions
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .cloned()
        .collect();
    for session in sessions {
        let mine = DaemonState::views_for_conn(&session, conn);
        if mine.is_empty() {
            continue;
        }
        {
            let mut views = session.views.lock().unwrap_or_else(|p| p.into_inner());
            for id in &mine {
                views.remove(id);
            }
            let mut owner = session.owner_view.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(owner_id) = owner.clone() {
                if mine.contains(&owner_id) {
                    *owner = None;
                }
            }
        }
        {
            let mut flow = session.flow.lock().unwrap_or_else(|p| p.into_inner());
            for id in &mine {
                flow.detach_view(id);
            }
        }
        state.bump_revision();
        // 연결이 닫히며 마지막 뷰가 사라진 종료 세션도 회수 대상이다.
        if session
            .views
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
        {
            state.retire_session_if_cold(&session.session_id);
        }
    }
}

/// Locate the (at most one) view on `conn` for `session` matching `epoch`.
fn find_view(
    session: &Arc<SessionEntry>,
    conn: &ConnectionId,
    epoch: &str,
) -> Result<Option<ViewId>, RpcError> {
    let views = session.views.lock().unwrap_or_else(|p| p.into_inner());
    let exact: Vec<ViewId> = views
        .iter()
        .filter(|(_, v)| &v.conn == conn && v.epoch == epoch)
        .map(|(id, _)| id.clone())
        .collect();
    match exact.len() {
        0 => Ok(None),
        1 => Ok(Some(exact[0].clone())),
        _ => Err(RpcError::new(ErrorCode::InvalidArgument, "ambiguous epoch")),
    }
}

/// `session.input`의 인라인 검증 통과분. [`session_input_begin`]은 락 몇 개로
/// 즉시 끝나는 검증과 `input_in_flight` 예약만 하고(연결 루프가 wire 순서대로
/// 부른다), [`session_input_finish`]가 최대 750ms 블로킹될 수 있는 PTY 쓰기를
/// blocking worker에서 이어서 마무리한다.
struct PendingInput {
    session: Arc<SessionEntry>,
    view_id: ViewId,
    workload_id: WorkloadId,
    input_id: String,
    data: Vec<u8>,
    actor: Arc<term_pty::actor::SessionActorHandle>,
}

/// `session.input`의 검증 절반 — 에포크·소유권·가드·in-flight 예약. 절대
/// 블로킹되지 않으므로 연결 루프에서 인라인으로 돌아, 뒤따르는
/// `session.take_control`/`session.detach`와의 순서가 뒤집히지 않는다.
fn session_input_begin(
    state: &Arc<DaemonState>,
    conn: &ConnectionId,
    params: &serde_json::Value,
) -> Result<PendingInput, RpcError> {
    let params: InputParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new(ErrorCode::InvalidArgument, e.to_string()))?;
    let session = state
        .session(&params.session_id)
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "session not found"))?;

    // 08 §5: 자원 가드로 일시정지 중인 세션은 입력을 받지 않는다. 정지된
    // 자식은 PTY 슬레이브를 읽지 않아 tty 큐가 차면 writer가 막히고, 막힌
    // writer는 취소 처리까지 밀어낸다. 재개하면 다시 받는다.
    if state.guard_view(&session.workload_id).0.is_suspended() {
        return Err(RpcError::new(
            ErrorCode::InvalidState,
            "session is suspended by the resource guard; resume it first",
        )
        .with_details(json!({ "reason_code": REASON_GUARD_SUSPENDED })));
    }

    let view_id = find_view(&session, conn, &params.epoch)?.ok_or_else(|| {
        RpcError::new(
            ErrorCode::StaleEpoch,
            "epoch does not match any attached view",
        )
    })?;
    let is_owner = {
        let owner = session
            .owner_view
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        owner == Some(view_id.clone())
    };
    if !is_owner {
        return Err(RpcError::new(
            ErrorCode::NotInputOwner,
            "input requires the session writer view",
        ));
    }

    let data = base64::engine::general_purpose::STANDARD
        .decode(&params.data_b64)
        .map_err(|_| RpcError::new(ErrorCode::InvalidArgument, "data_b64 is not valid base64"))?;
    if data.is_empty() {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "input chunk is empty",
        ));
    }
    if data.len() > term_contracts::session::limits::INPUT_CHUNK_BYTES {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "input chunk exceeds 4 KiB",
        ));
    }

    // One outstanding input per writer (spec §4). The reservation is taken
    // in wire order here, so a second input from the same view is refused
    // before the first one's write even starts.
    {
        let mut views = session.views.lock().unwrap_or_else(|p| p.into_inner());
        let Some(view) = views.get_mut(&view_id) else {
            return Err(RpcError::new(ErrorCode::StaleEpoch, "view detached"));
        };
        if view.input_in_flight {
            return Err(RpcError::new(
                ErrorCode::Busy,
                "one outstanding input per writer",
            ));
        }
        view.input_in_flight = true;
    }

    let Some(actor) = state
        .workload_entry(&session.workload_id)
        .and_then(|e| e.lock().unwrap_or_else(|p| p.into_inner()).actor.clone())
    else {
        // Leaving `input_in_flight` set here turned every later key into
        // BUSY "one outstanding input per writer" until a reattach.
        clear_in_flight(&session, &view_id);
        return Err(RpcError::new(
            ErrorCode::InvalidState,
            "session has no live actor",
        ));
    };

    Ok(PendingInput {
        workload_id: session.workload_id.clone(),
        session,
        view_id,
        input_id: params.input_id,
        data,
        actor,
    })
}

/// `session.input`의 쓰기 절반 — blocking worker 전용. 입력은 이미 검증을
/// 지났으므로 여기서 하는 일은 PTY 쓰기와 응답 조립뿐이다.
fn session_input_finish(
    state: &Arc<DaemonState>,
    pending: PendingInput,
) -> Result<serde_json::Value, RpcError> {
    let PendingInput {
        session,
        view_id,
        workload_id,
        input_id,
        data,
        actor,
    } = pending;
    // W2(ADR-5 해소): 쓰기 완료까지 기다련 응답한다. 정상 쓰기는
    // 마이크로초 단위로 끝나고, 막힌 PTY에서만 상한(750ms) 후 정직한
    // Queued로 떨어진다.
    let reply =
        match actor.write_input_await(&input_id, &data, std::time::Duration::from_millis(750)) {
            Ok(reply) => reply,
            Err(ActorError::Input(term_pty::input::InputQueueError::QueueFull { .. })) => {
                clear_in_flight(&session, &view_id);
                let error = RpcError::new(ErrorCode::Busy, "input queue is full");
                // A full queue behind a stalled write is the same "program is
                // not reading" condition: say so, so the pane can show it. A
                // full queue with a healthy program is a client-side burst —
                // its own reason keeps the "stalled" badge off.
                return Err(match actor.input_blocked_for() {
                    Some(blocked) => error.with_details(json!({
                        "reason_code": REASON_INPUT_STALLED,
                        "retry_after_ms": 1_000,
                        "blocked_ms": blocked.as_millis() as u64,
                    })),
                    None => error.with_details(json!({
                        "reason_code": REASON_INPUT_QUEUE_FULL,
                    })),
                });
            }
            Err(ActorError::InputStalled { blocked_ms }) => {
                clear_in_flight(&session, &view_id);
                return Err(RpcError::new(
                    ErrorCode::Busy,
                    "the program in this terminal is not reading input",
                )
                .with_details(json!({
                    "reason_code": REASON_INPUT_STALLED,
                    "retry_after_ms": 1_000,
                    "blocked_ms": blocked_ms,
                })));
            }
            Err(ActorError::Input(term_pty::input::InputQueueError::ChunkTooLarge { .. })) => {
                clear_in_flight(&session, &view_id);
                return Err(RpcError::new(
                    ErrorCode::InvalidArgument,
                    "input chunk exceeds 4 KiB",
                ));
            }
            Err(e) => {
                clear_in_flight(&session, &view_id);
                return Err(RpcError::new(ErrorCode::InvalidState, e.to_string()));
            }
        };
    clear_in_flight(&session, &view_id);
    // Codex `/model`의 전역 설정 저장을 이 pane에 귀속할 근거(model_watch).
    crate::model_watch::note_input(state.now_ms(), &workload_id);

    let accepted = match reply {
        InputReply::Queued { bytes } => bytes,
        InputReply::Written { bytes } => bytes,
        InputReply::Replayed { outcome } => match outcome {
            term_pty::input::InputOutcome::Accepted(bytes) => bytes,
            term_pty::input::InputOutcome::Unknown => {
                return Err(RpcError::new(
                    ErrorCode::InputOutcomeUnknown,
                    "previous write outcome unknown; do not auto-retry",
                ))
            }
            term_pty::input::InputOutcome::InFlight => data.len() as u32,
        },
    };
    // W2의 750 ms Queued는 아직 tty에 쓰지 않은 입력이다 — 클라이언트가
    // '전달됨'(막힘 배지 해제)과 구분하게 싣는다(구 데몬은 생략).
    let queued = match &reply {
        InputReply::Written { .. } => false,
        InputReply::Queued { .. } => true,
        InputReply::Replayed { outcome } => {
            !matches!(outcome, term_pty::input::InputOutcome::Accepted(_))
        }
    };
    let result = InputResult {
        input_id,
        accepted_bytes: accepted,
        queued: Some(queued),
    };
    serde_json::to_value(&result)
        .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))
}

/// `details.reason_code` of a `session.input` refused because the resource
/// guard suspended the session (code `INVALID_STATE`).
pub const REASON_GUARD_SUSPENDED: &str = "GUARD_SUSPENDED";
/// `details.reason_code` of a `session.input` refused because the program
/// has not read its tty input past the stall threshold (code `BUSY`).
pub const REASON_INPUT_STALLED: &str = "INPUT_STALLED";
/// `details.reason_code` of a `session.input` refused because the actor's
/// input queue is full while the program is **healthy** (code `BUSY`) — a
/// client-side burst, not a hung program, so the pane must NOT show the
/// "input stalled" badge for it. Old daemons sent no reason for either
/// state, which is why the client also keeps a message-sniffing fallback.
pub const REASON_INPUT_QUEUE_FULL: &str = "INPUT_QUEUE_FULL";
/// `details.reason_code` of a `session.input`/`session.resize` refused by
/// the per-connection pending-completion budget (code `BUSY`, ipc.rs). The
/// request was rejected **before dispatch** — nothing reached the session —
/// so the client may safely retry the same input after `retry_after_ms`.
pub const REASON_PENDING_BUDGET: &str = "PENDING_BUDGET";

fn clear_in_flight(session: &Arc<SessionEntry>, view_id: &ViewId) {
    if let Some(view) = session
        .views
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_mut(view_id)
    {
        view.input_in_flight = false;
    }
}

fn session_resize(
    state: &Arc<DaemonState>,
    conn: &ConnectionId,
    params: &serde_json::Value,
) -> Result<PendingResize, RpcError> {
    let params: ResizeParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new(ErrorCode::InvalidArgument, e.to_string()))?;
    let session = state
        .session(&params.session_id)
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "session not found"))?;

    // 08 §5: 자원 가드로 일시정지 중인 세션은 입력을 받지 않는다. 정지된
    // 자식은 PTY 슬레이브를 읽지 않아 tty 큐가 차면 writer가 막히고, 막힌
    // writer는 취소 처리까지 밀어낸다. 재개하면 다시 받는다.
    if state.guard_view(&session.workload_id).0.is_suspended() {
        return Err(RpcError::new(
            ErrorCode::InvalidState,
            "session is suspended by the resource guard; resume it first",
        ));
    }

    let view_id = find_view(&session, conn, &params.epoch)?.ok_or_else(|| {
        RpcError::new(
            ErrorCode::StaleEpoch,
            "epoch does not match any attached view",
        )
    })?;
    let is_owner = {
        let owner = session
            .owner_view
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        owner == Some(view_id)
    };
    if !is_owner {
        return Err(RpcError::new(
            ErrorCode::NotInputOwner,
            "resize requires the session writer view",
        ));
    }
    if !(2..=1000).contains(&params.cols) || !(2..=1000).contains(&params.rows) {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "cols/rows out of 2..=1000",
        ));
    }

    // One outstanding resize per session. Other sessions never share this
    // guard; ownership checks and actor submissions still happen in wire order.
    let pending = PendingResize::begin(session, params)?;
    let actor = state
        .workload_entry(&pending.session.workload_id)
        .and_then(|e| e.lock().unwrap_or_else(|p| p.into_inner()).actor.clone())
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidState, "session has no live actor"))?;
    actor
        .resize(pending.params.cols, pending.params.rows)
        .map_err(|e| RpcError::new(ErrorCode::Busy, e.to_string()))?;
    *pending
        .session
        .size
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = (pending.params.cols, pending.params.rows);
    Ok(pending)
}

struct PendingResize {
    session: Arc<SessionEntry>,
    params: ResizeParams,
    known_seq: u64,
    allow_existing: bool,
}

impl PendingResize {
    fn begin(session: Arc<SessionEntry>, params: ResizeParams) -> Result<Self, RpcError> {
        session
            .resize_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| RpcError::new(ErrorCode::Busy, "session resize already in flight"))?;
        let known_seq = session.last_seq.load(Ordering::Acquire);
        let allow_existing =
            *session.size.lock().unwrap_or_else(|p| p.into_inner()) == (params.cols, params.rows);
        Ok(Self {
            session,
            params,
            known_seq,
            allow_existing,
        })
    }

    async fn applied_seq(&self) -> Result<u64, RpcError> {
        let wait = async {
            loop {
                // Register before checking the journal so an actor notification
                // between the check and await cannot be lost.
                let notified = self.session.resize_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let applied = {
                    let ring = self
                        .session
                        .recent_resizes
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    ring.iter().rev().find_map(|&(seq, cols, rows, _)| {
                        (cols == self.params.cols
                            && rows == self.params.rows
                            && (seq > self.known_seq
                                || (self.allow_existing
                                    && ring.last().is_some_and(|last| last.0 == seq))))
                        .then_some(seq)
                    })
                };
                if let Some(seq) = applied {
                    return seq;
                }
                notified.await;
            }
        };
        tokio::time::timeout(Duration::from_millis(600), wait)
            .await
            .map_err(|_| {
                RpcError::new(
                    ErrorCode::Busy,
                    "resize was not applied before the deadline",
                )
            })
    }

    async fn complete(
        self,
        state: &Arc<DaemonState>,
        conn: &ConnectionId,
    ) -> Result<serde_json::Value, RpcError> {
        let seq = self.applied_seq().await?;
        // A detach/owner change can now run while we wait. Do not publish a
        // completion for a writer that has since lost its view or epoch.
        let view = find_view(&self.session, conn, &self.params.epoch)?;
        if view.is_none()
            || *self
                .session
                .owner_view
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                != view
        {
            return Err(RpcError::new(
                ErrorCode::StaleEpoch,
                "resize writer changed while awaiting completion",
            ));
        }
        let result = ResizeResult {
            resize_id: self.params.resize_id.clone(),
            applied_seq: U64String::new(seq).expect("fits"),
        };
        state.broadcast_control(
            term_contracts::rpc::RpcEventKind::SessionResizeApplied,
            json!({
                "session_id": self.params.session_id,
                "epoch": self.params.epoch,
                "seq": result.applied_seq,
                "cols": self.params.cols,
                "rows": self.params.rows,
            }),
        );
        serde_json::to_value(result)
            .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))
    }
}

impl Drop for PendingResize {
    fn drop(&mut self) {
        self.session
            .resize_in_flight
            .store(false, Ordering::Release);
    }
}

/// Data-connection ACK: applies to the linked control connection's views.
/// Returns `Ok(None)` — no response frame on success (spec §4).
fn session_ack(
    state: &Arc<DaemonState>,
    linked_control: Option<ConnectionId>,
    params: &serde_json::Value,
) -> Result<Option<serde_json::Value>, RpcError> {
    let ack: SessionAck = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new(ErrorCode::InvalidArgument, e.to_string()))?;
    let Some(control) = linked_control else {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "ack requires a linked control connection",
        ));
    };
    let session = state
        .session(&ack.session_id)
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "session not found"))?;
    sessions::apply_ack(state, &session, &control, &ack.epoch, ack.through_seq.get())?;
    Ok(None)
}

/// `session.focus` — 이 창(컨트롤 연결)이 지금 보고 있는 세션 보고
/// (spec `08-pressure-relief.md` §1). 압력 완화는 보이는 pane을 건드리지
/// 않으므로 데몬이 포커스 집합을 알아야 한다. 연결당 최대 하나이며,
/// `session_id`가 없으면 "이 창은 아무것도 보고 있지 않다"는 뜻이다.
fn session_focus(
    state: &Arc<DaemonState>,
    conn: &ConnectionId,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let params: SessionFocusParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new(ErrorCode::InvalidArgument, e.to_string()))?;
    if let Some(session_id) = &params.session_id {
        // 살아 있는 워크로드의 세션만 포커스가 된다 — 종료된(또는 모르는)
        // 세션을 포커스로 광고하면 완화 로직이 유령을 보호하게 된다.
        let live = state.session(session_id).is_some_and(|session| {
            state.workload_entry(&session.workload_id).is_some_and(|w| {
                !w.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .state
                    .is_terminal()
            })
        });
        if !live {
            return Err(RpcError::new(
                ErrorCode::InvalidArgument,
                "session not found",
            ));
        }
    }
    // 같은 보고를 반복해도 revision은 움직이지 않는다(UI가 1초마다 다시
    // 보고해도 스냅샷이 매번 무효화되지 않게).
    if state.set_focused_session(conn, params.session_id) {
        state.bump_revision();
    }
    let result = SessionFocusResult {
        focused_session_ids: state.focused_session_ids(),
    };
    serde_json::to_value(&result)
        .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))
}

/// `session.relief` — 세션 하나의 완화를 사용자가 직접 조작한다
/// (spec `08-pressure-relief.md` §2). 수동은 항상 자동보다 우선한다(§0-2):
/// 수동 양보는 압력이 풀려도 자동 복원되지 않고, 압력 중 수동 복원한 세션은
/// NORMAL로 돌아갈 때까지 다시 양보되지 않는다.
///
/// 블로킹: 계획한 조작을 이 자리에서 OS에 걸고 결과까지 기록한 뒤 답한다.
fn session_relief(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let params: SessionReliefParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new(ErrorCode::InvalidArgument, e.to_string()))?;
    // `session.focus`와 같은 검증: 살아 있는 워크로드의 세션만 대상이다 —
    // 끝난 세션의 완화를 조작하면 유령 기록이 남는다.
    let session = state
        .session(&params.session_id)
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "session not found"))?;
    let workload_id = session.workload_id.clone();
    let live = state.workload_entry(&workload_id).is_some_and(|entry| {
        !entry
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .is_terminal()
    });
    if !live {
        return Err(RpcError::new(
            ErrorCode::InvalidState,
            "workload already finished",
        ));
    }

    let now = state.now_ms();
    let cpu_level = *state
        .cpu_pressure_level
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let ops = {
        let mut relief = state.relief.lock().unwrap_or_else(|p| p.into_inner());
        relief.manual(&workload_id, params.action, cpu_level)
    };
    for op in ops {
        let outcome = crate::relief::apply(state, &op);
        let mut relief = state.relief.lock().unwrap_or_else(|p| p.into_inner());
        relief.record(&op, &outcome, now);
    }
    // 보호 표시만 바뀌어도 요약은 달라진다 — revision을 올리고
    // `workload.changed`로 알린다.
    state.workload_state_changed(&workload_id);

    let (relief, protected) = state.relief_view(&workload_id);
    let result = SessionReliefResult { relief, protected };
    serde_json::to_value(&result)
        .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))
}

/// `relief.set_policy` — 데몬 전체의 자동 완화 스위치(08 §2).
/// 끄는 것만으로는 이미 양보 중인 세션이 복원되지 않는다: NORMAL 회복
/// 경로나 수동 복원이 되돌린다.
fn relief_set_policy(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let params: ReliefPolicyParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new(ErrorCode::InvalidArgument, e.to_string()))?;
    let policy = term_contracts::snapshot::ReliefPolicy {
        auto_yield: params.auto_yield,
    };
    let changed = state
        .relief
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_policy(policy);
    if changed {
        state.bump_revision();
    }
    serde_json::to_value(policy)
        .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))
}

/// `workload.suspend` / `workload.resume` — 자원 가드의 수동 조작(08 §5).
/// 블로킹: 이 자리에서 OS에 걸고 결과까지 기록한 뒤 답한다. 수동 정지는
/// 자동 재개 대상이 아니다.
fn workload_guard(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
    suspend: bool,
) -> Result<serde_json::Value, RpcError> {
    let params: WorkloadSuspendParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new(ErrorCode::InvalidArgument, e.to_string()))?;
    let live = state
        .workload_entry(&params.workload_id)
        .is_some_and(|entry| {
            !entry
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .state
                .is_terminal()
        });
    if !live {
        return Err(RpcError::new(
            ErrorCode::InvalidState,
            "workload already finished",
        ));
    }
    let now = state.now_ms();
    let ops = {
        let mut guard = state.guard.lock().unwrap_or_else(|p| p.into_inner());
        guard.manual(&params.workload_id, suspend)
    };
    for op in ops {
        let outcome = crate::guard::apply(state, &op);
        let mut guard = state.guard.lock().unwrap_or_else(|p| p.into_inner());
        guard.record(&op, &outcome, now);
    }
    state.workload_state_changed(&params.workload_id);
    let (guard, _) = state.guard_view(&params.workload_id);
    let result = WorkloadGuardResult { guard };
    serde_json::to_value(&result)
        .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))
}

/// `guard.set_policy` — 데몬 전체의 자원 가드 정책(08 §5).
fn guard_set_policy(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let params: GuardPolicyParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::new(ErrorCode::InvalidArgument, e.to_string()))?;
    let policy = params.policy;
    let changed = state
        .guard
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_policy(policy.clone());
    if changed {
        state.bump_revision();
    }
    serde_json::to_value(policy)
        .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))
}

fn session_take_control(
    state: &Arc<DaemonState>,
    conn: &ConnectionId,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let session_id: SessionId = parse_id(params, "session_id")?;
    let view_id: ViewId = parse_id(params, "view_id")?;
    let expected_owner: Option<ViewId> = match params.get("expected_owner") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => {
            let text = v.as_str().ok_or_else(|| {
                RpcError::new(
                    ErrorCode::InvalidArgument,
                    "expected_owner must be a view id or null",
                )
            })?;
            Some(ViewId::parse_text(text)?)
        }
    };
    let session = state
        .session(&session_id)
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "session not found"))?;

    // CAS on the current owner.
    let current = session
        .owner_view
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if current != expected_owner {
        return Err(RpcError::new(
            ErrorCode::InvalidState,
            "expected_owner does not match the current owner",
        ));
    }
    if current == Some(view_id.clone()) {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "view already owns the session",
        ));
    }
    {
        let views = session.views.lock().unwrap_or_else(|p| p.into_inner());
        let view = views
            .get(&view_id)
            .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "view not attached"))?;
        if &view.conn != conn {
            return Err(RpcError::new(
                ErrorCode::InvalidArgument,
                "view not attached by this connection",
            ));
        }
    }

    // Swap: new writer, old writer demoted; fresh epoch for the new writer.
    let new_epoch = uuid::Uuid::new_v4().to_string();
    let old_owner = current;
    {
        let mut views = session.views.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(old) = &old_owner {
            if let Some(view) = views.get_mut(old) {
                view.access = AttachAccess::Reader;
            }
        }
        if let Some(view) = views.get_mut(&view_id) {
            view.access = AttachAccess::Writer;
            view.epoch = new_epoch.clone();
        }
    }
    *session.owner_view.lock().unwrap_or_else(|p| p.into_inner()) = Some(view_id.clone());
    session
        .flow
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .attach_view(view_id.clone(), new_epoch.clone());
    session.wake();
    state.bump_revision();
    state.broadcast_control(
        term_contracts::rpc::RpcEventKind::SessionOwnerChanged,
        json!({
            "session_id": session_id,
            "old_owner": old_owner,
            "new_owner": view_id,
            "epoch": new_epoch,
        }),
    );
    Ok(json!({ "epoch": new_epoch, "owner": view_id }))
}

fn retention_set_limit(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let session_id: SessionId = parse_id(params, "session_id")?;
    let max_bytes = params
        .get("max_bytes")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "max_bytes missing"))?;
    if max_bytes == 0 || max_bytes > i64::MAX as u64 {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "max_bytes out of range",
        ));
    }
    let session = state
        .session(&session_id)
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidArgument, "session not found"))?;

    // Disk-space check: free bytes on the journal's volume must cover the
    // new cap (02-runner §5: 상한 증가는 여유 디스크 확인 후 명시적으로).
    {
        let host = state
            .host
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .0
            .clone();
        let free: Option<u64> = host.and_then(|h| {
            h.disks
                .iter()
                .filter(|d| !d.mount.is_empty())
                .filter_map(|d| {
                    d.free_bytes
                        .value
                        .as_ref()
                        .map(|v| (v.get(), d.mount.clone()))
                })
                .filter(|(_, mount)| session.journal_path.starts_with(mount))
                .map(|(free, _)| free)
                .max()
                .or_else(|| {
                    h.disks
                        .iter()
                        .filter_map(|d| d.free_bytes.value.as_ref().map(|v| v.get()))
                        .max()
                })
        });
        if let Some(free) = free {
            if free < max_bytes {
                return Err(RpcError::new(
                    ErrorCode::DiskFull,
                    "insufficient disk space for the requested journal limit",
                ));
            }
        }
    }

    // 상한은 살아 있는 writer에 걸려야 한다. cap에 걸려 멈춘 actor는
    // 세션은 살아 있어도 reader 정지가 고정(sticky)이라 출력 수집을 되살릴
    // 수 없다 — 성공을 가장하지 않고 INVALID_STATE로 답한다(상한은 cap에
    // 닿기 전에 올려야 한다).
    let journal_error = state
        .workload_entry(&session.workload_id)
        .and_then(|entry| {
            entry
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .actor
                .clone()
        })
        .and_then(|actor| actor.status().journal_error);
    if journal_error.is_some() || session.actor_finalized.load(Ordering::Acquire) {
        return Err(RpcError::new(
            ErrorCode::InvalidState,
            "session journal is already stopped; raise the limit before the cap is reached",
        ));
    }
    let inner = session
        .journal_inner
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let Some(inner) = inner else {
        return Err(RpcError::new(
            ErrorCode::InvalidState,
            "session has no live journal writer",
        ));
    };
    inner
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_session_limit(max_bytes);
    session.journal_limit.store(max_bytes, Ordering::Release);
    Ok(json!({ "session_id": session_id, "max_bytes": max_bytes }))
}

fn daemon_shutdown(
    state: &Arc<DaemonState>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let stop_workloads = params
        .get("stop_workloads")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if stop_workloads {
        state
            .stop_workloads_on_shutdown
            .store(true, Ordering::Release);
    }
    let _ = state.shutdown.send(true);
    Ok(json!({ "shutting_down": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 모든 문자열 필드를 계약 상한까지 채운 기록 한 건(최악의 크기).
    fn fat_agent_session_record(index: usize) -> term_contracts::agent_session::AgentSessionRecord {
        use term_contracts::agent_session::{limits, AgentSessionRecord, AgentSessionSource};
        AgentSessionRecord {
            id: uuid::Uuid::new_v4().to_string(),
            workload_id: WorkloadId::generate(),
            pty_session_id: Some(SessionId::generate()),
            agent: "x".repeat(limits::AGENT_MAX),
            agent_session_id: format!("{index:0>width$}", width = limits::SESSION_ID_MAX),
            cwd: format!("/{}", "c".repeat(limits::PATH_MAX - 1)),
            title: Some("t".repeat(limits::TITLE_MAX)),
            program: Some(format!("/{}", "p".repeat(limits::PATH_MAX - 1))),
            source: AgentSessionSource::Registry,
            first_seen_at: "2026-09-13T00:00:00.000Z".into(),
            last_seen_at: "2026-09-13T00:00:01.000Z".into(),
            ended_at: Some("2026-09-13T00:00:02.000Z".into()),
            end_reason: Some("workload_exited".into()),
            active: false,
        }
    }

    /// 제일 큰 기록 500건이라도 응답은 컨트롤 프레임(64 KiB) 안에 든다 —
    /// 프레임을 넘기면 `ipc`가 인코딩 실패로 연결을 닫는다.
    #[test]
    fn agent_session_list_is_bounded_by_bytes_not_by_count() {
        let mut records: Vec<_> = (0..500).map(fat_agent_session_record).collect();
        let bytes = bound_agent_session_list(&mut records);
        assert!(
            bytes <= AGENT_SESSION_LIST_BUDGET_BYTES,
            "{bytes} bytes exceeds the budget"
        );
        assert!(
            bytes + 4 <= term_contracts::rpc::MAX_FRAME_BYTES,
            "{bytes} bytes + length prefix must fit one frame"
        );
        assert!(!records.is_empty(), "at least one record must survive");
        assert!(records.len() < 500, "fat records must have been trimmed");
        // 돌려준 길이는 실제 인코딩 길이와 정확히 같아야 한다.
        assert_eq!(bytes, serde_json::to_vec(&records).unwrap().len());
        // 남은 것은 앞쪽(가장 최근) 기록이다.
        assert_eq!(
            records[0].agent_session_id,
            format!(
                "{:0>width$}",
                0,
                width = term_contracts::agent_session::limits::SESSION_ID_MAX
            )
        );

        // 예산 안에 드는 목록은 한 건도 깎지 않는다.
        let mut small = vec![fat_agent_session_record(1)];
        let bytes = bound_agent_session_list(&mut small);
        assert_eq!(small.len(), 1);
        assert!(bytes <= AGENT_SESSION_LIST_BUDGET_BYTES);

        let mut empty: Vec<term_contracts::agent_session::AgentSessionRecord> = Vec::new();
        assert_eq!(bound_agent_session_list(&mut empty), 2);
    }

    #[test]
    fn routing_table_covers_the_full_spec_method_set() {
        let all = [
            methods::HELLO,
            methods::SYSTEM_SNAPSHOT,
            methods::WORKLOAD_LAUNCH,
            methods::WORKLOAD_CANCEL,
            methods::WORKLOAD_REPRIORITIZE,
            methods::WORKLOAD_UPDATE_POLICY,
            methods::WORKLOAD_PROCESSES,
            methods::SESSION_ATTACH,
            methods::SESSION_DETACH,
            methods::SESSION_INPUT,
            methods::SESSION_RESIZE,
            methods::SESSION_ACK,
            methods::SESSION_TAKE_CONTROL,
            methods::SESSION_FOCUS,
            methods::SESSION_RELIEF,
            methods::RELIEF_SET_POLICY,
            methods::RETENTION_SET_LIMIT,
            methods::AGENT_SESSION_REPORT,
            methods::AGENT_SESSION_LIST,
            methods::AGENT_SESSION_FORGET,
            methods::DAEMON_SHUTDOWN,
        ];
        assert_eq!(all.len(), 21, "every spec method is routed");
    }
}

#[cfg(test)]
mod search_tests {
    use crate::search_scan::context_line;

    #[test]
    fn context_line_extracts_the_matching_line() {
        let text = "first line\nDo you want to proceed? (y/n)\nlast line";
        let at = text.find("proceed").unwrap();
        assert_eq!(context_line(text, at, 7), "Do you want to proceed? (y/n)");
    }

    #[test]
    fn context_line_never_panics_on_non_boundary_indices() {
        // 대소문자 접기로 인해 원문과 길이가 달라지는 예: 'İ'(U+0130).
        let folded = "İstem".to_lowercase();
        assert_ne!(folded.len(), "İstem".len());
        let text = "İstem kontrolü";
        // folded 인덱스를 원문에 그대로 적용해도 경계 방어로 근사한다.
        let _ = context_line(text, 2, 3);
        let _ = context_line(text, 0, text.len() + 10);
        let _ = context_line("짧", 5, 9);
    }

    #[test]
    fn context_line_bounds_long_lines_with_marker() {
        let long = format!("x{}y", "a".repeat(600));
        let at = long.find('y').unwrap();
        let line = context_line(&long, at, 1);
        assert!(line.starts_with('…'));
        assert!(line.len() <= term_contracts::session::search_limits::LINE_CONTEXT + 4);
    }
}

/// Mission RPC arm: feature gate, blocking execution, `mission.changed`
/// broadcast after commit, and verbatim MissionRpcError responses.
async fn dispatch_mission(
    state: Arc<DaemonState>,
    conn: ConnectionId,
    request: &RpcRequest,
) -> Outcome {
    use term_contracts::mission::error::{MissionErrorCode, MissionRpcError};

    let unsupported = |message: &str| {
        let error = MissionRpcError::new(MissionErrorCode::CapabilityUnsupported, message);
        let response = serde_json::json!({
            "v": term_contracts::rpc::PROTOCOL_VERSION,
            "id": request.id,
            "error": error,
        });
        Outcome::Reply(response)
    };
    let Some(service) = state.missions.clone() else {
        return unsupported("mission orchestration is disabled on this daemon");
    };
    let params = request.params.clone();
    let method = request.method.clone();
    let handled = tokio::task::spawn_blocking(move || service.handle(&conn, &method, &params))
        .await
        .unwrap_or_else(|join| {
            Err(MissionRpcError::new(
                MissionErrorCode::Internal,
                format!("mission handler panicked: {join}"),
            ))
        });
    match handled {
        Ok(handled) => {
            if let Some((mission_id, latest_seq)) = handled.notify {
                state.broadcast_control(
                    term_contracts::rpc::RpcEventKind::MissionChanged,
                    serde_json::json!({
                        "mission_id": mission_id.to_string(),
                        "latest_seq": latest_seq.to_string(),
                    }),
                );
            }
            let response = RpcResponse::ok(request.id.clone(), handled.result);
            Outcome::Reply(serde_json::to_value(&response).unwrap_or_default())
        }
        Err(error) => {
            let response = serde_json::json!({
                "v": term_contracts::rpc::PROTOCOL_VERSION,
                "id": request.id,
                "error": error,
            });
            Outcome::Reply(response)
        }
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Arc<SessionEntry>) {
        let dir = tempfile::tempdir().unwrap();
        let (session, _, _) = crate::sessions::journal_release_tests::session_entry(
            dir.path(),
            &SessionId::generate(),
        );
        (dir, session)
    }

    fn request(session: &Arc<SessionEntry>, cols: u16) -> PendingResize {
        PendingResize::begin(
            Arc::clone(session),
            ResizeParams {
                session_id: session.session_id.clone(),
                epoch: session.current_epoch(),
                resize_id: "test-resize".into(),
                cols,
                rows: 24,
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn resize_completion_before_or_after_wait_is_never_lost() {
        let (_dir, session) = fixture();
        {
            let pending = request(&session, 100);
            session.record_resize(1, 100, 24, 0);
            assert_eq!(pending.applied_seq().await.unwrap(), 1);
        }
        let pending = request(&session, 120);
        let wait = pending.applied_seq();
        tokio::pin!(wait);
        tokio::select! {
            biased;
            _ = &mut wait => panic!("must wait for the actual journal record"),
            _ = tokio::task::yield_now() => {}
        }
        session.record_resize(2, 120, 24, 0);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), wait)
                .await
                .unwrap()
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn unchanged_size_resolves_existing_record_without_waiting() {
        let (_dir, session) = fixture();
        session.record_resize(1, 80, 24, 0);
        session.last_seq.store(2, Ordering::Release); // later output
        let pending = request(&session, 80);
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(100), pending.applied_seq())
                .await
                .unwrap()
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn historical_size_is_not_reported_as_a_new_application() {
        let (_dir, session) = fixture();
        session.record_resize(1, 100, 24, 0);
        session.record_resize(2, 80, 24, 0);
        session.last_seq.store(2, Ordering::Release);
        let pending = request(&session, 100);
        // No matching new record: timeout is an error, never a made-up seq.
        let error = pending.applied_seq().await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Busy);
    }

    #[tokio::test]
    async fn cancelled_wait_releases_only_its_own_session() {
        let (_a, session_a) = fixture();
        let (_b, session_b) = fixture();
        let pending_a = request(&session_a, 100);
        let pending_b = request(&session_b, 120);
        assert!(session_a.resize_in_flight.load(Ordering::Acquire));
        let duplicate = PendingResize::begin(Arc::clone(&session_a), pending_a.params.clone());
        assert_eq!(duplicate.err().unwrap().code, ErrorCode::Busy);
        let task = tokio::spawn(async move { pending_a.applied_seq().await });
        task.abort();
        let _ = task.await;
        assert!(!session_a.resize_in_flight.load(Ordering::Acquire));
        assert!(session_b.resize_in_flight.load(Ordering::Acquire));
        let _next = request(&session_a, 140);
        drop(pending_b);
        assert!(!session_b.resize_in_flight.load(Ordering::Acquire));
    }
}
