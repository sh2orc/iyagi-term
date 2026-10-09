//! Launch pipeline, workload lifecycle, cancel, and terminal finalization.
//!
//! The managed sequence mirrors spec `02-runner.md` §3 exactly (see the
//! numbered comments in [`start_managed_admitted`]); shell mode skips the
//! gate/group (spec: "일반 셸은 task/attempt 없이 workload와 session만 가진다").

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use term_contracts::error::ErrorCode;
use term_contracts::gate::GateTarget;
use term_contracts::ids::{RequestId, SessionId, WorkloadId};
use term_contracts::launch::{
    launch_fingerprint, Enforcement, LaunchMode, LaunchPolicy, LaunchRequest, LaunchValidation,
    Priority,
};
use term_contracts::metrics::UsageCoverage;
use term_contracts::session::{ExitReason, SessionExit};
use term_contracts::snapshot::QueueReason;
use term_contracts::state::{TerminalConnection, WorkloadState};
use term_contracts::workload::{ProcessOwnership, WorkloadDescriptor};
use term_contracts::RpcError;
use term_core::{AdmissionRequest, CoreError};
use term_pty::actor::{SessionActorConfig, TeardownInfo};
use term_pty::gate::{GateServer, StartOutcome};
use term_storage::{AttemptId, TaskId};
use term_storage::{LaunchIntent, LaunchIntentOutcome, RequestMethod, RequestResolution};

use crate::claude_provider::ResolvedClaudeProvider;
use crate::connections::SecretRedactor;
use crate::gate_listener::GateListener;
use crate::sessions::{
    release_journal, spawn_journal_flusher, spawn_pump, GroupWatch, NotifySink, RedactingJournal,
    SessionEntry, SharedJournal,
};
use crate::state::{DaemonState, WorkloadEntry};

/// Result of an accepted launch — queued or running.
#[derive(Debug, Clone)]
pub struct LaunchOutcome {
    pub workload_id: WorkloadId,
    pub session_id: SessionId,
    pub state: WorkloadState,
    pub effective_policy: term_contracts::launch::LaunchPolicy,
    pub missing_capabilities: Vec<String>,
}

/// Effective outcome of persisting a launch intent.
#[derive(Debug, Clone)]
enum RecordOutcome {
    Created,
    /// Concurrent duplicate won the insert race.
    Existing {
        workload_id: WorkloadId,
        state: WorkloadState,
    },
}

/// `workload.launch` entry point: ordered validation (spec `01-contracts.md`
/// §2 ordering), duplicate resolution, then mode-specific admission.
pub fn launch(state: Arc<DaemonState>, request: LaunchRequest) -> Result<LaunchOutcome, RpcError> {
    // (1) Protocol/length/shape validation (pure).
    if let LaunchValidation::Invalid(reason) = request.validate() {
        return Err(RpcError::new(ErrorCode::InvalidArgument, reason));
    }
    let fingerprint = launch_fingerprint(&request);

    // (2) Duplicate request check BEFORE any filesystem access (spec §2:
    // 중복 요청은 실행 파일이 이후 삭제되었어도 기존 실행 결과를 반환).
    match state
        .storage
        .resolve_request(&request.request_id, &fingerprint)
    {
        Ok(RequestResolution::Existing {
            workload_id,
            state: ws,
            ..
        }) => {
            return existing_outcome(&state, &workload_id, ws);
        }
        Ok(RequestResolution::Conflict) => {
            return Err(RpcError::new(
                ErrorCode::RequestConflict,
                "request id already recorded with a different fingerprint",
            ));
        }
        Ok(RequestResolution::New) => {}
        Err(e) => return Err(storage_error(e)),
    }

    // (3) Filesystem checks: program existence, then cwd.
    if !std::path::Path::new(&request.program).is_file() {
        return Err(RpcError::new(
            ErrorCode::ProgramNotFound,
            "program file not found",
        ));
    }
    // Windows: `.cmd/.bat/.ps1` shim은 std가 `cmd.exe /c`로 감싸 실행한다 —
    // 인수 인용이 우리 손을 떠나고(02-runner §3: shim 직접 실행·`cmd /c`
    // 연결 금지) 실패는 StartFailed{-1}로만 보인다. npm이 설치한
    // `claude.cmd`/`codex.cmd`가 정확히 이 경우다.
    if cfg!(windows) && has_shell_shim_extension(&request.program) {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "program is a .cmd/.bat/.ps1 shim — register the native executable \
             (or the interpreter plus an argv prefix) instead",
        ));
    }
    // 셸 pane은 `/usr/bin/env -u … <셸> …`로 온다(UI가 상속된 색 변수를
    // 지운다). env는 늘 있으므로 감싼 셸이 지워졌으면 PTY가 뜨자마자 127로
    // 끝난 터미널만 남는다 — 감싼 실행 파일이 절대 경로면 여기서 같은
    // PROGRAM_NOT_FOUND로 거절해 UI가 기본 셸로 바꿔 열 수 있게 한다.
    if let Some(wrapped) = env_wrapped_program(&request.program, &request.argv) {
        if !std::path::Path::new(wrapped).is_file() {
            return Err(
                RpcError::new(ErrorCode::ProgramNotFound, "program file not found").with_details(
                    serde_json::json!({
                        "reason_code": "wrapped_program_missing",
                        "program": wrapped,
                    }),
                ),
            );
        }
    }
    let canonical_cwd = usable_cwd(&request.cwd)?;

    // A recorded runtime ID does not prove that Claude saved a conversation.
    // Check again here for old workspace markers and removed transcripts,
    // before spawning a process that would immediately fail its --resume.
    if let Some(id) = crate::agent_session::claude_resume_id(&request.program, &request.argv) {
        let home = request
            .env_overrides
            .get("CLAUDE_CONFIG_DIR")
            .filter(|dir| !dir.is_empty())
            .map(|dir| canonical_cwd.join(dir))
            .or_else(crate::agent_session::claude_home);
        if home.as_ref().is_some_and(|home| {
            !crate::agent_session::claude_transcript_exists(home, id, canonical_cwd.to_str())
        }) {
            return Err(RpcError::new(
                ErrorCode::InvalidArgument,
                "Claude's saved conversation file is missing. This runtime session ID cannot be resumed.",
            ).with_details(serde_json::json!({"reason_code": "claude_transcript_missing"})));
        }
    }

    // Z.ai 라우팅 해석(`claude_provider`). 실패는 workload 행이 생기기 전에
    // reason code로 끝나고, 요청은 절대 바꾸지 않는다 — fingerprint·DB·
    // title에 토큰이 들어갈 길이 없다. 토큰은 아래 env 맵으로만 흐른다.
    let claude_provider = crate::claude_provider::resolve(state.paths.root(), &request)?;

    match request.mode {
        LaunchMode::Shell => launch_shell(&state, &request, canonical_cwd, claude_provider),
        LaunchMode::Managed => launch_managed(&state, &request, canonical_cwd, claude_provider),
    }
}

/// Idempotent replay answer: current state of the existing workload (spec
/// §4: launch 중복 응답은 동일 workload의 현재 상태).
fn existing_outcome(
    state: &Arc<DaemonState>,
    workload_id: &WorkloadId,
    ws: WorkloadState,
) -> Result<LaunchOutcome, RpcError> {
    if let Some(entry_arc) = state.workload_entry(workload_id) {
        let entry = entry_arc.lock().unwrap_or_else(|p| p.into_inner());
        return Ok(LaunchOutcome {
            workload_id: entry.workload_id.clone(),
            session_id: entry.session_id.clone(),
            state: ws,
            effective_policy: entry.policy.clone(),
            missing_capabilities: entry.missing_capabilities.clone(),
        });
    }
    // Registry miss (daemon restarted): report from storage alone.
    let Some(record) = state
        .storage
        .workload_record(workload_id)
        .map_err(storage_error)?
        .filter(|r| r.id == *workload_id)
    else {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "workload not found",
        ));
    };
    let session_id = state
        .storage
        .sessions()
        .ok()
        .and_then(|sessions| {
            sessions
                .into_iter()
                .find(|s| s.workload_id == *workload_id)
                .map(|s| s.id)
        })
        .unwrap_or_else(SessionId::generate);
    Ok(LaunchOutcome {
        workload_id: record.id.clone(),
        session_id,
        state: ws,
        effective_policy: term_contracts::launch::LaunchPolicy {
            reservation_bytes: record.reservation_bytes.clone(),
            cpu_slots: record.cpu_slots,
            enforcement: record.enforcement,
            memory_max_bytes: record.memory_max_bytes.clone(),
            cpu_max_cores: record.cpu_max_cores,
            pids_max: record.pids_max,
        },
        missing_capabilities: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Shell mode

fn launch_shell(
    state: &Arc<DaemonState>,
    request: &LaunchRequest,
    canonical_cwd: std::path::PathBuf,
    claude_provider: Option<ResolvedClaudeProvider>,
) -> Result<LaunchOutcome, RpcError> {
    // Admission: session limit only (spec: shells spawn directly; no queue,
    // no resource group). Host memory pressure never blocks a shell launch:
    // a shell itself costs a few MB, and a host under pressure is exactly
    // when the user needs a terminal (to find and stop the offender). What
    // pressure does govern is the managed queue (WAIT_HOST_PRESSURE) and the
    // relief/guard loops over already-running workloads.
    if state.active_workload_count() as u32 >= state.config.session_limit() {
        return Err(RpcError::new(
            ErrorCode::SessionLimit,
            format!("session limit {} reached", state.config.session_limit()),
        ));
    }

    let (workload_id, session_id) = (WorkloadId::generate(), SessionId::generate());
    record_intent(state, request, &request.policy, &workload_id, &session_id)?;
    let entry = register_workload(
        state,
        request,
        request.policy.clone(),
        workload_id.clone(),
        session_id.clone(),
        None,
        claude_provider.as_ref(),
    );

    // STARTING commit, then direct PTY spawn (no gate/group).
    state
        .storage
        .mark_starting(&workload_id)
        .map_err(storage_error)?;
    set_entry_state(state, &workload_id, WorkloadState::Starting);
    state.workload_state_changed(&workload_id);

    let argv = full_argv(&request.program, &request.argv);
    let mut env = with_iyagi_identity(&request.env_overrides, &session_id, &workload_id);
    // 라우팅된 Claude pane: provider 변수와 토큰을 얹고, 상속된 provider/auth
    // 변수는 `env_remove`로 먼저 지운다(비라우팅은 빈 목록 = 예전과 동일).
    if let Some(routed) = &claude_provider {
        routed.apply(&mut env);
    }
    crate::opencode_integration::configure(state.paths.root(), &mut env);
    let env_remove = claude_provider
        .as_ref()
        .map(|routed| routed.env_remove.clone())
        .unwrap_or_default();
    let spawned = term_pty::pty::PtyHandle::spawn(
        request.cols,
        request.rows,
        &request.program,
        &argv,
        &env,
        &env_remove,
        canonical_cwd.to_str(),
    );
    // 자식이 환경을 복사해 갔다(성공이든 실패든). 라우팅 토큰이 든 우리 쪽
    // 사본은 여기서 지운다 — 이 맵은 더 읽지 않는다.
    crate::claude_provider::zeroize_env(&mut env);
    let pty = Arc::new(spawned.map_err(|e| {
        fail_workload(state, &workload_id, ErrorCode::SpawnFailed, &e.to_string());
        RpcError::new(ErrorCode::SpawnFailed, "pty spawn failed")
    })?);
    // 에이전트 감시 루프가 이 셸의 자손을 훑을 수 있게 루트 pid를 남긴다.
    {
        let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        guard.shell_pid = pty.pid();
    }
    // F1: 맨 pid만으로는 재사용을 못 걸러낸다 — 관측·완화가 이 pid를 닻으로
    // 쓸 수 있게 스폰 직후 신원(pid+start token+boot id)도 찍어 둔다. 관리
    // 실행이 게이트 악수에 쓰는 것(term_platform::identity)과 같은 규칙을
    // 셸 루트에도 적용한다. 신원을 못 읽으면(스폰 직후 이미 종료한 셸 등)
    // 기록하지 않는다: 신원 없는 pid는 절대 닻이 되지 않으며 그 셸은 관측·
    // 완화 대상에서 빠질 뿐이다. RUNNING 커밋보다 먼저 끝내야 첫 관측 틱이
    // 닻 없는 셸을 보지 않는다.
    if let Some(root_pid) = pty.pid().filter(|pid| *pid > 0) {
        match term_platform::identity::process_identity(root_pid) {
            Some(identity) => state.record_shell_identity(&workload_id, identity),
            None => tracing::debug!(
                workload = %workload_id,
                pid = root_pid,
                "shell root identity unavailable; shell observation stays off"
            ),
        }
    }

    if let Err(e) = state.storage.mark_running(&workload_id) {
        // The shell is already spawned and its root identity recorded. Fail
        // the workload first (terminal write + `note_workload_terminal`,
        // which forgets the identity), then kill and reap the child so it is
        // never left orphaned and unsupervised — as the managed path does.
        fail_workload(
            state,
            &workload_id,
            ErrorCode::DaemonUnavailable,
            "running commit failed",
        );
        kill_and_reap_shell(state, &workload_id, &pty);
        return Err(storage_error(e));
    }
    set_entry_state(state, &workload_id, WorkloadState::Running);
    state.workload_state_changed(&workload_id);

    if let Err(e) = start_session_actor(
        state,
        &workload_id,
        &session_id,
        request.cols,
        request.rows,
        Arc::clone(&pty),
        None,
        claude_provider
            .as_ref()
            .map(|routed| Arc::clone(&routed.redactor)),
    ) {
        // Journal-open failure already failed the workload inside
        // `start_session_actor`; the shell never got its supervisor, so kill
        // and reap it instead of answering RUNNING for a FAILED workload.
        kill_and_reap_shell(state, &workload_id, &pty);
        return Err(e);
    }
    state.touch_activity();

    let policy = entry
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .policy
        .clone();
    Ok(LaunchOutcome {
        workload_id,
        session_id,
        state: WorkloadState::Running,
        effective_policy: policy,
        missing_capabilities: Vec::new(),
    })
}

/// Kill a spawned shell whose launch failed after `PtyHandle::spawn` (the
/// workload is already FAILED), then reap it. The group step of `cleanup_pty`
/// is a no-op for shells. The reap is bounded: a stuck child must not block
/// the launch reply, but an unreaped one lingers as a zombie.
fn kill_and_reap_shell(
    state: &Arc<DaemonState>,
    workload_id: &WorkloadId,
    pty: &Arc<term_pty::pty::PtyHandle>,
) {
    cleanup_pty(state, workload_id, pty);
    let reap_deadline = Instant::now() + Duration::from_secs(2);
    while matches!(pty.poll_exit(), Ok(None)) && Instant::now() < reap_deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------------------
// Managed mode

fn launch_managed(
    state: &Arc<DaemonState>,
    request: &LaunchRequest,
    canonical_cwd: std::path::PathBuf,
    claude_provider: Option<ResolvedClaudeProvider>,
) -> Result<LaunchOutcome, RpcError> {
    if state.active_workload_count() as u32 >= state.config.session_limit() {
        return Err(RpcError::new(
            ErrorCode::SessionLimit,
            format!("session limit {} reached", state.config.session_limit()),
        ));
    }

    // 이 호스트가 절대 받을 수 없는 크기는 거절하지 않고 받을 수 있는 최대로
    // 줄여 받는다(`fit_policy_to_host`). 지문·중복 판정은 원래 요청 그대로다.
    let policy = fit_policy_to_host(state, &request.policy);
    if policy != request.policy {
        tracing::info!(
            requested_reservation = request.policy.reservation_bytes.get(),
            reservation = policy.reservation_bytes.get(),
            requested_cpu_slots = request.policy.cpu_slots,
            cpu_slots = policy.cpu_slots,
            "managed policy fitted to this host instead of RESOURCE_UNSCHEDULABLE"
        );
    }

    // Capability gate: `require` + a platform-unsupported requested limit
    // fails before anything is created (spec §2: require는 실행 전에 실패).
    let missing = missing_capabilities(state, request);
    if !missing.is_empty() && request.policy.enforcement == Enforcement::Require {
        return Err(RpcError::new(
            ErrorCode::CapabilityUnavailable,
            "requested limits cannot be enforced on this platform",
        )
        .with_details(serde_json::json!({ "missing": missing })));
    }

    let (workload_id, session_id) = (WorkloadId::generate(), SessionId::generate());
    let mut env = with_iyagi_identity(&request.env_overrides, &session_id, &workload_id);
    // 라우팅된 Claude pane: 토큰을 **뺀** provider 변수만 얹는다. 이 descriptor
    // 는 대기열에 남을 수 있으므로(`WorkloadEntry`) 토큰은 시작 시점에
    // `start_managed_admitted`가 저장소에서 다시 읽어 gate 프레임에만 넣고,
    // `env_remove`도 그때 엔트리의 `claude_provider`에서 다시 만든다.
    if let Some(routed) = &claude_provider {
        routed.apply_selector(&mut env);
    }
    crate::opencode_integration::configure(state.paths.root(), &mut env);
    let descriptor = WorkloadDescriptor {
        workload_id: workload_id.clone(),
        session_id: session_id.clone(),
        cwd: canonical_cwd.to_string_lossy().into_owned(),
        program: request.program.clone(),
        argv: request.argv.clone(),
        env_overrides: env,
        cols: request.cols,
        rows: request.rows,
        policy: policy.clone(),
    };

    // Admission decide + reserve atomically (term-core reservation ledger).
    let host = state.admission_host();
    let admission = AdmissionRequest {
        reservation_bytes: policy.reservation_bytes.get(),
        cpu_slots: policy.cpu_slots,
    };
    match state
        .ledger
        .try_admit_and_reserve(&host, workload_id.clone(), admission)
    {
        Ok(_) => {}
        Err(CoreError::AdmissionDenied {
            reason: QueueReason::ResourceUnschedulable,
        }) => {
            // Not a wait state: the request needs a policy/config change
            // (01 §7); nothing is created.
            return Err(RpcError::new(
                ErrorCode::ResourceUnschedulable,
                "request exceeds the managed budget or CPU slot capacity",
            ));
        }
        Err(CoreError::AdmissionDenied { reason }) => {
            // Wait state: create the workload QUEUED + queue entry; launch
            // returns the queue result immediately (spec §4). A concurrent
            // duplicate may have recorded this request in between — resolve
            // to the existing workload instead of a phantom queue entry.
            match record_intent(state, request, &policy, &workload_id, &session_id) {
                Ok(RecordOutcome::Created) => {}
                Ok(RecordOutcome::Existing {
                    workload_id: existing,
                    state: ws,
                }) => {
                    return existing_outcome(state, &existing, ws);
                }
                Err(e) => return Err(e),
            }
            register_workload(
                state,
                request,
                policy.clone(),
                workload_id.clone(),
                session_id.clone(),
                Some(descriptor),
                claude_provider.as_ref(),
            );
            enqueue_workload(
                state,
                &workload_id,
                &request.request_id,
                request.priority,
                reason,
            );
            return Ok(LaunchOutcome {
                workload_id,
                session_id,
                state: WorkloadState::Queued,
                effective_policy: policy,
                missing_capabilities: missing,
            });
        }
        Err(e) => return Err(core_error(e)),
    }

    // Admitted: persist intent FIRST (§7.1 invariant: intent commit before
    // STARTING commit before any child exists).
    match record_intent(state, request, &policy, &workload_id, &session_id) {
        Ok(RecordOutcome::Created) => {}
        Ok(RecordOutcome::Existing {
            workload_id: existing,
            state: ws,
        }) => {
            release_reservation(state, &workload_id);
            return existing_outcome(state, &existing, ws);
        }
        Err(e) => {
            release_reservation(state, &workload_id);
            return Err(e);
        }
    }
    let entry = register_workload(
        state,
        request,
        policy,
        workload_id.clone(),
        session_id.clone(),
        Some(descriptor),
        claude_provider.as_ref(),
    );
    {
        let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        guard.reserved = true;
        guard.missing_capabilities = missing.clone();
    }

    let outcome_state = start_managed_admitted(state, &workload_id)?;
    let policy = entry
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .policy
        .clone();
    Ok(LaunchOutcome {
        workload_id,
        session_id,
        state: outcome_state,
        effective_policy: policy,
        missing_capabilities: missing,
    })
}

/// The gated launch sequence (spec `02-runner.md` §3, normative order).
/// Runs on a blocking thread; every failure path cleans group + helper and
/// releases the reservation exactly once.
fn start_managed_admitted(
    state: &Arc<DaemonState>,
    workload_id: &WorkloadId,
) -> Result<WorkloadState, RpcError> {
    // (1) Storage: STARTING commit (the intent row already exists QUEUED).
    state.storage.mark_starting(workload_id).map_err(|e| {
        release_reservation(state, workload_id);
        storage_error(e)
    })?;
    set_entry_state(state, workload_id, WorkloadState::Starting);
    state.workload_state_changed(workload_id);

    // 라우팅 여부도 같은 잠금에서 읽는다: 대기열에서 깨어난 실행은
    // `ResolvedClaudeProvider`를 더는 갖고 있지 않고 엔트리만 남아 있다.
    let (descriptor, routed) = match state.workload_entry(workload_id) {
        Some(entry) => {
            let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
            (guard.descriptor.clone(), guard.claude_provider.is_some())
        }
        None => {
            return Err(RpcError::new(
                ErrorCode::InvalidArgument,
                "workload vanished",
            ))
        }
    };
    let Some(descriptor) = descriptor else {
        release_reservation(state, workload_id);
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "descriptor missing",
        ));
    };

    // 라우팅된 Claude pane: 토큰은 **지금** 읽는다. descriptor(엔트리)에는
    // 토큰이 없다 — 대기열에 있는 동안 Settings에서 키가 지워졌으면 여기서
    // `zai_key_missing`으로 실패하고, 바뀌었으면 새 키로 시작한다. redactor도
    // 이 키로 다시 만들어 엔트리에 둔다(런치 시점 것은 옛 키일 수 있다).
    let start_token = if routed {
        match crate::claude_provider::StartToken::read(state.paths.root()) {
            Ok(token) => Some(token),
            Err(error) => {
                fail_workload(
                    state,
                    workload_id,
                    ErrorCode::InvalidArgument,
                    crate::claude_provider::reason_code(&error).unwrap_or("claude_provider"),
                );
                return Err(error);
            }
        }
    } else {
        None
    };
    let redactor = start_token
        .as_ref()
        .map(|token| Arc::clone(&token.redactor));
    if let Some(redactor) = &redactor {
        if let Some(entry) = state.workload_entry(workload_id) {
            entry.lock().unwrap_or_else(|p| p.into_inner()).redactor = Some(Arc::clone(redactor));
        }
    }

    // (2) Platform: create resource group + apply policy limits.
    let group_descriptor = group_policy_descriptor(&descriptor);
    let group = state
        .platform
        .create_group(&group_descriptor)
        .map_err(|e| {
            fail_workload(
                state,
                workload_id,
                ErrorCode::GroupAttachFailed,
                &e.to_string(),
            );
            RpcError::new(
                ErrorCode::GroupAttachFailed,
                "resource group creation failed",
            )
        })?;
    {
        let entry = state.workload_entry(workload_id).expect("registered above");
        entry.lock().unwrap_or_else(|p| p.into_inner()).group = Some(group.clone());
    }

    // (3) Private gate endpoint + (4) PTY: spawn launch-helper as the first
    // child. The target is NOT created yet — the helper WAITS.
    let nonce = term_pty::gate::generate_nonce();
    let endpoint = state.paths.gate_endpoint(&nonce);
    let exe = std::env::current_exe()
        .map_err(|e| {
            fail_workload(
                state,
                workload_id,
                ErrorCode::SpawnFailed,
                &format!("own exe: {e}"),
            );
            RpcError::new(ErrorCode::SpawnFailed, "cannot resolve daemon executable")
        })?
        .to_string_lossy()
        .into_owned();
    let gate_deadline = Instant::now() + state.config.gate_timeout();
    let listener = GateListener::bind(&endpoint, state.runtime.clone()).map_err(|e| {
        fail_workload(
            state,
            workload_id,
            ErrorCode::SpawnFailed,
            &format!("gate bind: {e}"),
        );
        RpcError::new(ErrorCode::SpawnFailed, "gate endpoint bind failed")
    })?;
    let helper_argv = vec![
        exe.clone(),
        "--launch-helper".into(),
        endpoint.clone(),
        nonce.clone(),
    ];
    let pty = Arc::new(
        term_pty::pty::PtyHandle::spawn(
            descriptor.cols,
            descriptor.rows,
            &exe,
            &helper_argv,
            &BTreeMap::new(),
            &[],
            None,
        )
        .map_err(|e| {
            fail_workload(state, workload_id, ErrorCode::SpawnFailed, &e.to_string());
            RpcError::new(ErrorCode::SpawnFailed, "helper pty spawn failed")
        })?,
    );
    let Some(child_pid) = pty.pid().filter(|p| *p > 0) else {
        cleanup_pty(state, workload_id, &pty);
        fail_workload(
            state,
            workload_id,
            ErrorCode::SpawnFailed,
            "helper pid unavailable",
        );
        return Err(RpcError::new(
            ErrorCode::SpawnFailed,
            "helper pid unavailable",
        ));
    };
    let Some(expected_identity) = term_platform::identity::process_identity(child_pid) else {
        fail_workload(
            state,
            workload_id,
            ErrorCode::SpawnFailed,
            "helper identity unavailable",
        );
        cleanup_pty(state, workload_id, &pty);
        return Err(RpcError::new(
            ErrorCode::SpawnFailed,
            "helper identity unavailable",
        ));
    };

    // (4b) Start the session machinery NOW: the actor's reader drains the
    // PTY master during the gate handshake (ConPTY children stall when the
    // master is never read) and the journal captures the session from its
    // very first bytes (spec 02-runner §5). No target exists yet; the actor
    // only reads, journals and polls the helper's liveness.
    if let Err(e) = start_session_actor(
        state,
        workload_id,
        &descriptor.session_id,
        descriptor.cols,
        descriptor.rows,
        Arc::clone(&pty),
        Some(group.clone()),
        redactor,
    ) {
        // Journal-open failure already failed the workload (group teardown +
        // reservation release) inside `start_session_actor`; kill the helper
        // PTY and surface the real error instead of waiting out the gate
        // deadline for a handshake that can no longer happen.
        cleanup_pty(state, workload_id, &pty);
        return Err(e);
    }

    // (5) Helper hello: verify BOTH the one-time nonce and the identity.
    let stream = match listener.accept(gate_deadline) {
        Ok(stream) => stream,
        Err(e) => {
            fail_workload(
                state,
                workload_id,
                ErrorCode::SpawnFailed,
                &format!("gate accept: {e}"),
            );
            cleanup_pty(state, workload_id, &pty);
            return Err(RpcError::new(
                ErrorCode::SpawnFailed,
                "helper did not connect to the gate",
            ));
        }
    };
    let mut gate = GateServer::new(stream);
    let t_accept = std::time::Instant::now();
    tracing::debug!("gate accept done; waiting for helper hello");
    let hello = match gate.wait_hello(&nonce, &expected_identity, gate_deadline) {
        Ok(hello) => hello,
        Err(e) => {
            fail_workload(
                state,
                workload_id,
                ErrorCode::SpawnFailed,
                &format!("gate hello: {e}"),
            );
            cleanup_pty(state, workload_id, &pty);
            return Err(RpcError::new(
                ErrorCode::SpawnFailed,
                "helper gate handshake failed",
            ));
        }
    };

    // (6) Send the target spec (helper still creates nothing).
    let mut target = GateTarget {
        program: descriptor.program.clone(),
        argv: full_argv(&descriptor.program, &descriptor.argv),
        // 라우팅된 pane: 토큰은 이 프레임의 env에만 얹는다(엔트리의
        // descriptor에는 없다).
        env_overrides: match &start_token {
            Some(token) => token.env_with_token(&descriptor.env_overrides),
            None => descriptor.env_overrides.clone(),
        },
        env_clear: false,
        // 라우팅된 Claude pane만 상속 provider/auth 변수를 지운다(helper가
        // env_overrides를 얹기 **전에** 적용). 비라우팅은 빈 목록.
        env_remove: if routed {
            crate::claude_provider::routed_env_remove()
        } else {
            Vec::new()
        },
        cwd: descriptor.cwd.clone(),
    };
    // 프레임에 옮겼으니 Zeroizing 원본은 여기서 지운다(redactor는 Arc로 남는다).
    drop(start_token);
    tracing::debug!(
        elapsed_ms = t_accept.elapsed().as_millis() as u64,
        "gate hello ok"
    );
    let sent = gate.send_target(&target);
    // 프레임이 나갔다(성공이든 실패든). 토큰이 든 우리 쪽 사본은 지운다 —
    // 직렬화된 프레임 바이트와 helper 쪽 사본이 우리가 못 지우는 마지막이다.
    crate::claude_provider::zeroize_env(&mut target.env_overrides);
    sent.map_err(|e| {
        tracing::debug!(
            error = %e,
            elapsed_ms = t_accept.elapsed().as_millis() as u64,
            "gate send_target failed"
        );
        fail_workload(
            state,
            workload_id,
            ErrorCode::SpawnFailed,
            &format!("gate target: {e}"),
        );
        RpcError::new(ErrorCode::SpawnFailed, "gate target send failed")
    })?;
    if workload_is_terminal(state, workload_id) {
        cleanup_pty(state, workload_id, &pty);
        return Err(RpcError::new(
            ErrorCode::SpawnFailed,
            "gate target send failed",
        ));
    }

    // (7) Attach the WAITING helper to the OS group BEFORE release.
    if let Err(e) = state
        .platform
        .attach_waiting_helper(&group, &hello.identity)
    {
        let _ = gate.abort();
        fail_workload(
            state,
            workload_id,
            ErrorCode::GroupAttachFailed,
            &e.to_string(),
        );
        cleanup_pty(state, workload_id, &pty);
        return Err(RpcError::new(
            ErrorCode::GroupAttachFailed,
            "helper group attach failed",
        ));
    }

    // (8) Persist group/process identity BEFORE release (§6 ordering).
    let ownership = ProcessOwnership {
        workload_id: workload_id.clone(),
        identity: hello.identity.clone(),
        group_kind: group.kind,
        group_reference: Some(group.reference.clone()),
        // An observed tree never claims group coverage (03 §7).
        coverage: match group.kind {
            term_contracts::workload::GroupKind::ObservedTree => UsageCoverage::ObservedTree,
            _ => UsageCoverage::Group,
        },
    };
    if let Err(e) = state.storage.save_group_identity(ownership) {
        tracing::warn!(workload = %workload_id, error = %e, "saving ownership row failed");
    }

    // (9) Single-use RELEASE.
    gate.release(|| Ok(())).map_err(|e| {
        fail_workload(
            state,
            workload_id,
            ErrorCode::SpawnFailed,
            &format!("gate release: {e}"),
        );
        cleanup_pty(state, workload_id, &pty);
        RpcError::new(ErrorCode::SpawnFailed, "gate release failed")
    })?;

    // (10) Await the start report. Unix: a clean EOF right after RELEASE is
    // the exec-success signal (close-on-exec semantics).
    let started = match gate.await_started(gate_deadline) {
        Ok(StartOutcome::Started) => true,
        Ok(StartOutcome::StartFailed { code }) => {
            fail_workload(
                state,
                workload_id,
                ErrorCode::SpawnFailed,
                &format!("target failed to start (code {code})"),
            );
            cleanup_pty(state, workload_id, &pty);
            return Err(RpcError::new(
                ErrorCode::SpawnFailed,
                "target failed to start",
            ));
        }
        Err(term_pty::gate::GateError::Eof) if cfg!(unix) => true,
        Err(e) => {
            fail_workload(
                state,
                workload_id,
                ErrorCode::SpawnFailed,
                &format!("start report: {e}"),
            );
            cleanup_pty(state, workload_id, &pty);
            return Err(RpcError::new(
                ErrorCode::SpawnFailed,
                "no start report from helper",
            ));
        }
    };

    if started && cfg!(windows) {
        // Windows helpers wait for the target and report its exit through
        // the gate; watch it in the background.
        spawn_gate_exit_watcher(Arc::clone(state), workload_id.clone(), gate);
    }

    // (11) RUNNING commit + (12) session actor + telemetry.
    let cancelled_during_start = state
        .workload_entry(workload_id)
        .is_some_and(|e| e.lock().unwrap_or_else(|p| p.into_inner()).cancel_requested);
    if !cancelled_during_start {
        if let Err(e) = state.storage.mark_running(workload_id) {
            fail_workload(
                state,
                workload_id,
                ErrorCode::DaemonUnavailable,
                "running commit failed",
            );
            cleanup_pty(state, workload_id, &pty);
            release_reservation(state, workload_id);
            return Err(storage_error(e));
        }
        set_entry_state(state, workload_id, WorkloadState::Running);
        // 관리 실행은 트리 감시 대상이 아니므로 명령 서명으로 에이전트를
        // 미리 찍는다(Running 브로드캐스트에 함께 실린다).
        #[cfg(unix)]
        let agent_pid = child_pid;
        #[cfg(windows)]
        let agent_pid = 0; // Windows 헬퍼는 목표 pid를 보고하지 않는다.
        crate::agent_watch::stamp_command_agent(
            state,
            workload_id,
            &descriptor.program,
            &descriptor.argv,
            agent_pid,
        );
        state.workload_state_changed(workload_id);
    }

    state.touch_activity();

    if cancelled_during_start {
        request_stop(state, workload_id);
        return Ok(WorkloadState::Stopping);
    }
    Ok(WorkloadState::Running)
}

/// Descriptor the platform sees: `observe` enforcement never applies OS hard
/// caps (measurement only); `prefer`/`require` pass them through.
fn group_policy_descriptor(descriptor: &WorkloadDescriptor) -> WorkloadDescriptor {
    let mut d = descriptor.clone();
    if d.policy.enforcement == Enforcement::Observe {
        d.policy.memory_max_bytes = None;
        d.policy.cpu_max_cores = None;
        d.policy.pids_max = None;
    }
    d
}

/// Windows: read `Exited{code}` from the helper's gate stream.
fn spawn_gate_exit_watcher(
    state: Arc<DaemonState>,
    workload_id: WorkloadId,
    mut gate: GateServer<crate::gate_listener::GateIo>,
) {
    let _ = std::thread::Builder::new()
        .name(format!("gate-exit-{workload_id}"))
        .spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(24 * 3600);
            match gate.await_exited(deadline) {
                Ok(code) => {
                    if let Some(entry) = state.workload_entry(&workload_id) {
                        *entry
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .gate_exit_code
                            .lock()
                            .unwrap_or_else(|p| p.into_inner()) = Some(code);
                    }
                }
                Err(e) => {
                    tracing::debug!(workload = %workload_id, error = %e, "gate exit report missing");
                }
            }
        });
}

// ---------------------------------------------------------------------------
// Registry helpers

fn full_argv(program: &str, args: &[String]) -> Vec<String> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(program.to_string());
    argv.extend(args.iter().cloned());
    argv
}

/// PTY 세션/워크로드 id를 알리는 환경 변수 이름(spec `02-runner.md` §8).
/// CLI 공식 hook은 CLI의 환경을 물려받고, CLI는 PTY의 환경을 물려받으므로
/// hook 프로세스에서 이 값을 읽어 어느 pane이 보냈는지 확실히 말할 수 있다.
pub const ENV_SESSION_ID: &str = "IYAGI_SESSION_ID";
pub const ENV_WORKLOAD_ID: &str = "IYAGI_WORKLOAD_ID";

/// 요청의 env override에 우리 식별자를 얹는다. 충돌하면 **우리 값이
/// 이긴다** — 클라이언트가 흉내 낸 값으로 남의 pane에 기록이 붙으면 안
/// 된다. 실행 fingerprint는 요청에서 이미 계산됐으므로 영향을 받지 않는다.
fn with_iyagi_identity(
    env_overrides: &std::collections::BTreeMap<String, String>,
    session_id: &SessionId,
    workload_id: &WorkloadId,
) -> std::collections::BTreeMap<String, String> {
    let mut env = env_overrides.clone();
    env.insert(ENV_SESSION_ID.to_string(), session_id.as_str().to_string());
    env.insert(
        ENV_WORKLOAD_ID.to_string(),
        workload_id.as_str().to_string(),
    );
    env
}

fn title_of(request: &LaunchRequest) -> String {
    // The env launcher is transport plumbing, not the user's terminal name.
    let cleanup_args = [
        "-u",
        "NO_COLOR",
        "-u",
        "FORCE_COLOR",
        "-u",
        "CLICOLOR",
        "-u",
        "CLICOLOR_FORCE",
    ];
    if request.mode == LaunchMode::Shell
        && request.program == "/usr/bin/env"
        && request.argv.len() > cleanup_args.len()
        && request
            .argv
            .iter()
            .take(cleanup_args.len())
            .map(String::as_str)
            .eq(cleanup_args)
    {
        let program = &request.argv[cleanup_args.len()];
        return std::path::Path::new(program)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.clone());
    }
    let name = std::path::Path::new(&request.program)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| request.program.clone());
    if request.argv.is_empty() {
        name
    } else {
        format!("{name} {}", request.argv.join(" "))
    }
}

/// `policy`는 실제로 적용할 정책이다(관리 실행은 호스트에 맞춰 줄였을 수
/// 있다). 지문은 언제나 요청 원문으로 만든다 — 같은 요청의 재전송이 충돌로
/// 보이지 않게.
fn record_intent(
    state: &Arc<DaemonState>,
    request: &LaunchRequest,
    policy: &LaunchPolicy,
    workload_id: &WorkloadId,
    session_id: &SessionId,
) -> Result<RecordOutcome, RpcError> {
    let (task_id, attempt_id) = if request.mode == LaunchMode::Managed {
        (Some(TaskId::generate()), Some(AttemptId::generate()))
    } else {
        (None, None)
    };
    let intent = LaunchIntent {
        request_id: request.request_id.clone(),
        method: RequestMethod::Launch,
        fingerprint: launch_fingerprint(request),
        title: title_of(request),
        task_id,
        attempt_id,
        attempt_ordinal: 1,
        workload_id: workload_id.clone(),
        session_id: session_id.clone(),
        mode: request.mode,
        priority: request.priority,
        policy: policy.clone(),
        journal_relative_path: format!("journals/{session_id}.mtj"),
        journal_limit_bytes: state.config.journal_session_bytes(),
        cols: request.cols,
        rows: request.rows,
    };
    match state.storage.record_launch_intent(intent) {
        Ok(LaunchIntentOutcome::Created { .. }) => Ok(RecordOutcome::Created),
        Ok(LaunchIntentOutcome::Existing {
            workload_id: existing,
            state,
            ..
        }) => Ok(RecordOutcome::Existing {
            workload_id: existing,
            state,
        }),
        Err(term_storage::StorageError::RequestConflict { .. }) => Err(RpcError::new(
            ErrorCode::RequestConflict,
            "request id already recorded with a different fingerprint",
        )),
        Err(e) => Err(storage_error(e)),
    }
}

fn register_workload(
    state: &Arc<DaemonState>,
    request: &LaunchRequest,
    policy: LaunchPolicy,
    workload_id: WorkloadId,
    session_id: SessionId,
    descriptor: Option<WorkloadDescriptor>,
    claude_provider: Option<&ResolvedClaudeProvider>,
) -> Arc<Mutex<WorkloadEntry>> {
    let entry = Arc::new(Mutex::new(WorkloadEntry {
        workload_id: workload_id.clone(),
        session_id: session_id.clone(),
        request_id: Some(request.request_id.clone()),
        mode: request.mode,
        state: WorkloadState::Queued,
        title: title_of(request),
        cwd: request.cwd.clone(),
        program: request.program.clone(),
        policy,
        priority: request.priority,
        descriptor,
        queue_reason: None,
        cancel_requested: false,
        root_exited: false,
        exit_code: None,
        last_error_code: None,
        missing_capabilities: Vec::new(),
        group: None,
        actor: None,
        reservation_released: false,
        reserved: false,
        gate_exit_code: Mutex::new(None),
        connection: TerminalConnection::Detached,
        shell_pid: None,
        agent: None,
        finalized: false,
        // 선택자(비밀 아님)만 남긴다. redactor는 토큰 사본을 쥐므로 곧바로
        // spawn하는 셸 모드만 여기서 넣고, 관리 모드는 대기열에 남을 수 있어
        // 시작 시점(`start_managed_admitted`)에 다시 읽은 키로 만들어 넣는다.
        claude_provider: claude_provider.map(|routed| routed.provider.clone()),
        redactor: claude_provider
            .filter(|_| request.mode == LaunchMode::Shell)
            .map(|routed| Arc::clone(&routed.redactor)),
    }));
    state
        .workloads
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(workload_id, Arc::clone(&entry));
    state.bump_revision();
    entry
}

fn enqueue_workload(
    state: &Arc<DaemonState>,
    workload_id: &WorkloadId,
    request_id: &RequestId,
    priority: Priority,
    _reason: QueueReason,
) {
    if let Err(e) = state
        .queue
        .enqueue(workload_id.clone(), request_id.clone(), priority)
    {
        tracing::error!(workload = %workload_id, error = %e, "queue enqueue failed");
    }
    // The wait reason is NOT stamped here: the scheduler owns it (03 §3
    // "Last computed wait reason, when the scheduler evaluated it") and its
    // first pass broadcasts queue.changed with the reason attached.
    state.broadcast_queue_changed();
}

/// Names of requested hard limits the platform cannot enforce *right now* —
/// unsupported by the OS or permission-gated (Linux without a delegated
/// cgroup subtree). `require` fails preflight on any of them; observe/prefer
/// launch anyway and carry the list in the outcome (03 §5).
/// 이 호스트가 절대 받을 수 없는 관리 실행 요청(예약 > 관리 예산 B,
/// cpu_slots > 슬롯 수 C)을 받을 수 있는 최대로 줄인 정책. 예약은 승인
/// 회계일 뿐 강제 상한(`memory_max_bytes`)이 아니어서 줄여도 실행 자체는
/// 같다 — 설정을 고치라고 거절(RESOURCE_UNSCHEDULABLE)하는 대신 받아들이고,
/// 줄인 값은 `effective_policy`로 돌려준다. 호스트 전체 크기를 아직 모르면
/// 예산도 모르므로 바이트는 그대로 둔다.
fn fit_policy_to_host(state: &DaemonState, policy: &LaunchPolicy) -> LaunchPolicy {
    let admission = state.config.admission_config(state.logical_cpus);
    let total = state.admission_host().total_bytes;
    let mut fitted = policy.clone();
    if total > 0 {
        let budget = admission.managed_budget_bytes(total);
        if policy.reservation_bytes.get() > budget {
            if let Ok(bytes) = term_contracts::U64String::new(budget) {
                fitted.reservation_bytes = bytes;
            }
        }
    }
    fitted.cpu_slots = policy.cpu_slots.min(admission.cpu_slot_capacity());
    fitted
}

pub fn missing_capabilities(state: &Arc<DaemonState>, request: &LaunchRequest) -> Vec<String> {
    let caps = state.caps.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let unsupported = |c: &term_contracts::snapshot::LimitCapability| {
        c.support != term_contracts::snapshot::LimitSupport::Supported
    };
    let mut missing = Vec::new();
    if request.policy.memory_max_bytes.is_some() && unsupported(&caps.memory_limit_kind) {
        missing.push("memory_max_bytes".to_string());
    }
    if request.policy.cpu_max_cores.is_some() && unsupported(&caps.cpu_quota) {
        missing.push("cpu_max_cores".to_string());
    }
    if request.policy.pids_max.is_some() && unsupported(&caps.process_count_limit) {
        missing.push("pids_max".to_string());
    }
    missing
}

fn set_entry_state(state: &Arc<DaemonState>, workload_id: &WorkloadId, to: WorkloadState) {
    if let Some(entry) = state.workload_entry(workload_id) {
        let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        // Never overwrite a terminal state from a stale path.
        if guard.state.is_terminal() && !to.is_terminal() {
            return;
        }
        guard.state = to;
    }
}

/// Terminal FAILED transition for launch-time failures (STARTING -> FAILED):
/// group cleanup, reservation release, single terminal write.
fn fail_workload(
    state: &Arc<DaemonState>,
    workload_id: &WorkloadId,
    code: ErrorCode,
    message: &str,
) {
    cleanup_group(state, workload_id);
    release_reservation(state, workload_id);
    let _ = state.storage.mark_terminal(
        workload_id,
        WorkloadState::Failed,
        None,
        Some(code_as_str(code)),
    );
    if let Some(entry) = state.workload_entry(workload_id) {
        let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        guard.state = WorkloadState::Failed;
        guard.last_error_code = Some(code_as_str(code));
        // 실패한 런치는 argv/env 원문을 더 붙들 이유가 없다. redactor(토큰
        // 사본)도 같이 놓는다 — 액터가 자기 Arc를 따로 쥐고 있다.
        guard.descriptor = None;
        guard.redactor = None;
    }
    close_agent_sessions(state, workload_id);
    tracing::warn!(workload = %workload_id, code = code_as_str(code), message, "launch failed");
    state.workload_state_changed(workload_id);
    state.note_workload_terminal(workload_id);
}

/// 이 pane에서 아직 열려 있는 에이전트 세션 행을 모두 닫는다(spec §8의
/// `workload_exited`). **종료 상태를 확정하는 모든 지점**에서 부른다:
/// 감시 루프는 자기가 저장한 행만 닫으므로, hook 보고로 생긴 행이나 관리
/// 실행의 `launch` 선지정 행(관리 실행은 트리 감시 집합에 없다)은 여기서만
/// 마감된다. 열린 행만 갱신하므로 여러 번 불려도 멱등이다.
fn close_agent_sessions(state: &Arc<DaemonState>, workload_id: &WorkloadId) {
    if let Err(error) = state
        .storage
        .end_agent_sessions_for_workload(workload_id, crate::agent_watch::END_WORKLOAD_EXITED)
    {
        tracing::warn!(workload = %workload_id, %error, "agent session close failed");
    }
}

fn cleanup_group(state: &Arc<DaemonState>, workload_id: &WorkloadId) {
    if let Some(entry) = state.workload_entry(workload_id) {
        let group = entry.lock().unwrap_or_else(|p| p.into_inner()).group.take();
        if let Some(group) = group {
            let _ = state
                .platform
                .terminate_owned(&group, term_platform::StopPhase::Force);
            drop(group);
        }
    }
}

fn cleanup_pty(
    state: &Arc<DaemonState>,
    workload_id: &WorkloadId,
    pty: &Arc<term_pty::pty::PtyHandle>,
) {
    let _ = pty.kill();
    pty.close();
    cleanup_group(state, workload_id);
}

/// Release the managed reservation. `ReservationLedger::release` is exactly
/// once by itself (a second release is a no-op), so racing paths (cancel of
/// a QUEUED entry vs. the scheduler's admit) cannot double- or under-release.
pub fn release_reservation(state: &Arc<DaemonState>, workload_id: &WorkloadId) {
    if state.ledger.release(workload_id) {
        if let Some(entry) = state.workload_entry(workload_id) {
            entry
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .reservation_released = true;
        }
        tracing::debug!(workload = %workload_id, "reservation released");
    }
}

fn code_as_str(code: ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{code:?}"))
}

/// Map the actor's journal-stop reason onto the spec surface codes
/// (02-runner §5: 공간 부족 시 해당 세션 read를 중지하고 JOURNAL_LIMIT/DISK_FULL을
/// 표시). Unknown reasons surface nothing rather than a wrong code.
pub fn journal_error_code(error: &str) -> Option<&'static str> {
    if error.contains("cap reached") || error.contains("budget exhausted") {
        Some("JOURNAL_LIMIT")
    } else if error.contains("disk full") {
        Some("DISK_FULL")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Session actor wiring

/// Build the registry `SessionEntry`, spawn the journal flusher + delivery
/// pump, then start the actor with daemon-side adapters. A journal-open
/// failure fails the workload here (group teardown + reservation release)
/// and returns `Err` so the caller kills the already-spawned PTY child
/// instead of leaving it unsupervised.
fn start_session_actor(
    state: &Arc<DaemonState>,
    workload_id: &WorkloadId,
    session_id: &SessionId,
    cols: u16,
    rows: u16,
    pty: Arc<term_pty::pty::PtyHandle>,
    group: Option<term_platform::GroupHandle>,
    redactor: Option<Arc<SecretRedactor>>,
) -> Result<(), RpcError> {
    let open_journal = || {
        SharedJournal::open(
            state,
            session_id,
            Arc::clone(&state.journal_budget),
            state.config.journal_session_bytes(),
            state.config.journal_segment_bytes(),
        )
    };
    // 전역 저널 예산이 높은 수위를 넘었으면 새 저널을 열기 전에 끝난 세션의
    // 저널부터 비운다. retention 루프는 1분마다라, 그 사이에 열린 새 세션은
    // 헤더조차 못 쓰거나(JOURNAL_LIMIT) 첫 출력에서 예산에 막혀 멈춘다.
    // 수위 아래면 잠금 한 번으로 끝난다.
    crate::retention::relieve_space_pressure(state);
    let opened = open_journal().or_else(|first| {
        // 디스크가 찼거나 그사이 예산이 다시 찼다: 한 번 더 정리하고 다시 연다.
        tracing::warn!(session = %session_id, error = %first, "journal open failed; relieving space and retrying once");
        crate::retention::relieve_disk_headroom(state);
        crate::retention::relieve_space_pressure(state);
        open_journal()
    });
    let (journal, journal_inner, journal_segments) = match opened {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!(session = %session_id, error = %e, "journal open failed; session cannot start");
            fail_workload(
                state,
                workload_id,
                ErrorCode::JournalLimit,
                "journal open failed",
            );
            return Err(RpcError::new(
                ErrorCode::JournalLimit,
                "journal open failed",
            ));
        }
    };

    let session = Arc::new(SessionEntry {
        session_id: session_id.clone(),
        workload_id: workload_id.clone(),
        journal_path: state.paths.journal(session_id.as_str()),
        journal_limit: AtomicU64::new(state.config.journal_session_bytes()),
        epoch: Mutex::new(uuid::Uuid::new_v4().to_string()),
        owner_view: Mutex::new(None),
        views: Mutex::new(std::collections::HashMap::new()),
        last_seq: std::sync::atomic::AtomicU64::new(0),
        journal_inner: Mutex::new(Some(Arc::clone(&journal_inner))),
        journal_offsets: Mutex::new(std::collections::BTreeMap::new()),
        journal_segments,
        journal_read_failing_since: Mutex::new(None),
        recent_resizes: Mutex::new(Vec::new()),
        resize_notify: tokio::sync::Notify::new(),
        resize_in_flight: AtomicBool::new(false),
        flow: Mutex::new(term_pty::flow::FlowController::with_budget(Arc::clone(
            &state.flow_budget,
        ))),
        wake_tx: Mutex::new(()),
        wake_cv: std::sync::Condvar::new(),
        wake_pending: std::sync::atomic::AtomicBool::new(false),
        pump_stop: std::sync::atomic::AtomicBool::new(false),
        pump_alive: std::sync::atomic::AtomicBool::new(false),
        pump_ctl: std::sync::Mutex::new(()),
        size: Mutex::new((cols, rows)),
        actor_finalized: std::sync::atomic::AtomicBool::new(false),
    });
    state
        .sessions
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(session_id.clone(), Arc::clone(&session));

    spawn_journal_flusher(Arc::clone(state), Arc::clone(&session), journal_inner);
    spawn_pump(Arc::clone(state), Arc::clone(&session));

    let cleanup = Arc::new(SessionFinalizer {
        state: Arc::clone(state),
        workload_id: workload_id.clone(),
    });
    let descendants: Arc<dyn term_pty::actor::DescendantWatch> = match group {
        Some(group) => Arc::new(GroupWatch::new(Arc::clone(&state.platform), group)),
        None => Arc::new(term_pty::actor::NoDescendants),
    };

    let mut config = SessionActorConfig::new(session_id.clone(), cols, rows, pty);
    // 저널은 라이브 뷰·재생의 유일한 원천이므로(02-runner §4–5) 그 앞단
    // 한 곳에서 지우면 어디에도 토큰이 남지 않는다. 비라우팅 세션은 래퍼
    // 없이 예전 경로 그대로다(비용 0).
    let journal: Box<dyn term_pty::actor::JournalSink> = match redactor {
        Some(redactor) => Box::new(RedactingJournal::new(journal, redactor)),
        None => Box::new(journal),
    };
    config.journal = journal;
    config.sink = Box::new(NotifySink::new(Arc::clone(state), session_id.clone()));
    config.cleanup = cleanup;
    config.descendants = descendants;
    let (handle, join) = term_pty::actor::start(config);
    let handle = Arc::new(handle);
    if let Some(entry) = state.workload_entry(workload_id) {
        entry.lock().unwrap_or_else(|p| p.into_inner()).actor = Some(Arc::clone(&handle));
    }
    // Cancel already requested while STARTING: stop immediately.
    let cancel_flagged = state
        .workload_entry(workload_id)
        .is_some_and(|e| e.lock().unwrap_or_else(|p| p.into_inner()).cancel_requested);
    if cancel_flagged {
        handle.cancel();
    }
    let reap_id = workload_id.clone();
    std::thread::Builder::new()
        .name(format!("reap-{reap_id}"))
        .spawn(move || {
            if let Ok(status) = join.join() {
                tracing::debug!(workload = %reap_id, ?status.lifecycle, "session actor reaped");
            }
        })
        .expect("spawn reaper");
    Ok(())
}

/// Actor teardown hook: final storage transitions + events (exactly once).
struct SessionFinalizer {
    state: Arc<DaemonState>,
    workload_id: WorkloadId,
}

impl term_pty::actor::SessionCleanup for SessionFinalizer {
    fn teardown(&self, info: &TeardownInfo) {
        finalize_session(
            Arc::clone(&self.state),
            self.workload_id.clone(),
            info.clone(),
        );
    }
}

/// Terminal finalization: root-exit bookkeeping, DRAINING → terminal,
/// reservation release, sampler untrack, group teardown, events.
pub fn finalize_session(state: Arc<DaemonState>, workload_id: WorkloadId, info: TeardownInfo) {
    let Some(entry_arc) = state.workload_entry(&workload_id) else {
        // 워크로드가 이미 보관 링에서 밀려났어도(런치 실패 뒤 다른 64개가
        // 먼저 종료한 경우) 세션 쪽 마무리는 해야 한다 — 그러지 않으면
        // 펌프·flusher와 저널 핸들이 영영 남고 retention도 영원히 건너뛴다.
        if let Some(session) = state.session(&info.session_id) {
            session.actor_finalized.store(true, AtomicOrdering::Release);
            release_journal(&session);
            session.wake();
            state.note_session_finalized(&info.session_id);
        }
        return;
    };
    let (session_id, mode, reserved, already_terminal, journal_error) = {
        let mut guard = entry_arc.lock().unwrap_or_else(|p| p.into_inner());
        if guard.finalized {
            return;
        }
        guard.finalized = true;
        let already_terminal = guard.state.is_terminal();
        if info.exit_code.is_some() {
            guard.root_exited = true;
            let _ = state.storage.set_root_exited(&workload_id, true);
        }
        // B18: the actor's journal stop reason (session cap / global budget /
        // disk full) rides along so the terminal record can show it
        // (02-runner §5: JOURNAL_LIMIT/DISK_FULL 표시).
        let journal_error = guard
            .actor
            .as_ref()
            .and_then(|actor| actor.status().journal_error);
        (
            guard.session_id.clone(),
            guard.mode,
            guard.reserved,
            already_terminal,
            journal_error,
        )
    };
    if let Some(session) = state.session(&session_id) {
        session.actor_finalized.store(true, AtomicOrdering::Release);
        // 02-runner §7 teardown 순서: 예약 → writer → master → group handle.
        // 저널 쓰기 핸들을 여기서 놓아야 `JournalInner`의 Drop이 돌아
        // 버퍼가 flush되고 파일 서술자가 닫힌다. 재생은 파일을 직접 읽으므로
        // (`read_journal_range`) 이후 attach도 그대로 동작한다.
        release_journal(&session);
        // 펌프는 여기서 세우지 않는다: 아직 붙어 있는 뷰에 마지막 레코드를
        // 배달해야 하고, 종료 세션 재attach 재생(02-runner §5)도 살아 있어야
        // 한다. 뷰가 0이 되는 순간 펌프는 스스로 빠져나가고, 보관 링에서
        // 밀려나면 `retire_session_if_cold`가 정지 신호를 세운다.
        // 보관 링 등록은 워크로드가 종료 상태에 닿는 곳(finish_workload /
        // 아래 already_terminal 분기)에서 한다 — 루트만 나가고 소유 자손이
        // 살아 RUNNING인 세션이 링에서 밀려나 회수되면 안 된다.
        session.wake();
    }
    // 셸·관리 워크로드 모두 여기서 계량 등록이 풀린다(셸은 PTY 루트 트리를
    // 훑던 공급자, 관리 워크로드는 그룹 멤버 공급자).
    state
        .telemetry
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .untrack_workload(&workload_id);
    state
        .usage_cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&workload_id);
    // 끝난 세션은 더 이상 아무 창의 포커스가 아니다(08 §1).
    state.clear_focus_for_session(&session_id);
    if already_terminal {
        // A launch-failure path already recorded the external terminal
        // state (spec 01 §7: initial-failure cleanup passes through internal
        // steps but records the external terminal state exactly once).
        cleanup_group(&state, &workload_id);
        if reserved {
            release_reservation(&state, &workload_id);
        }
        state.note_session_finalized(&session_id);
        // Launch-failure path already recorded the external terminal state;
        // the exit cause lives in last_error_code, not in a process exit.
        let launch_error = state.workload_entry(&workload_id).and_then(|e| {
            e.lock()
                .unwrap_or_else(|p| p.into_inner())
                .last_error_code
                .clone()
        });
        state.broadcast_control(
            term_contracts::rpc::RpcEventKind::SessionExited,
            serde_json::to_value(&SessionExit {
                session_id: session_id.clone(),
                exit_code: None,
                descendants_remaining: false,
                reason: ExitReason::Unknown,
                detail: launch_error,
            })
            .unwrap_or(serde_json::Value::Null),
        );
        return;
    }

    // Preferred exit code: gate report (Windows managed) > pty poll.
    let exit_code = preferred_exit_code(&state, &workload_id, &info, mode);
    let journal_stop_code = journal_error.as_deref().and_then(journal_error_code);
    // OOM 증거는 그룹이 사라지기 전에 읽는다 — finish_workload가 그룹을
    // 정리하면 cgroup 파일도 함께 사라진다.
    let oom_kill_count = state
        .workload_entry(&workload_id)
        .and_then(|e| e.lock().unwrap_or_else(|p| p.into_inner()).group.clone())
        .and_then(|group| state.platform.oom_kill_count(&group));

    if info.descendants_remaining && !info.cancelled {
        // Root gone, owned descendants alive: keep RUNNING, flag
        // root_exited, watch the group until it empties (spec §5).
        {
            let mut guard = entry_arc.lock().unwrap_or_else(|p| p.into_inner());
            guard.exit_code = exit_code;
        }
        state.workload_state_changed(&workload_id);
        spawn_descendant_watcher(
            Arc::clone(&state),
            workload_id,
            exit_code,
            journal_stop_code,
            oom_kill_count,
        );
        return;
    }

    finish_workload(
        &state,
        &workload_id,
        &session_id,
        info.cancelled,
        exit_code,
        info.descendants_remaining,
        reserved,
        journal_stop_code.map(str::to_string),
        oom_kill_count,
    );
}

fn preferred_exit_code(
    state: &Arc<DaemonState>,
    workload_id: &WorkloadId,
    info: &TeardownInfo,
    mode: LaunchMode,
) -> Option<i32> {
    let read_gate_code = || {
        state.workload_entry(workload_id).and_then(|entry| {
            *entry
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .gate_exit_code
                .lock()
                .unwrap_or_else(|p| p.into_inner())
        })
    };
    if mode == LaunchMode::Managed && cfg!(windows) {
        // Give the gate exit report a moment to land.
        for _ in 0..20 {
            if let Some(code) = read_gate_code() {
                return Some(code);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    read_gate_code().or(info.exit_code)
}

/// 관측 오류(그룹 멤버를 읽을 수 없음)가 이만큼 계속되면(300ms 틱 × 100 =
/// 30초) 살아 있을지 모르는 멤버를 기록하고 워크로드를 마감한다. 오류를
/// "비었음"으로 치부하면 멤버가 살아 있는 채로 마감하고, 무한 재시기는
/// 종료 상태를 영원히 붙잡는다 — 둘 다 아니어야 한다.
const WATCHER_OBSERVATION_ERROR_LIMIT: u32 = 100;

/// 한 관측 오류 후에도 워크로드를 마감해도 되는가. 첫 오류에서 마감하는
/// 것(구현의 이전 버전)은 살아 있는 멤버를 두고 워크로드를 끝내는 버그다:
/// 오류는 재시도하고, 한계에 이른 streak만 마감한다.
fn observation_error_finalizes(error_streak: u32) -> bool {
    error_streak >= WATCHER_OBSERVATION_ERROR_LIMIT
}

fn spawn_descendant_watcher(
    state: Arc<DaemonState>,
    workload_id: WorkloadId,
    exit_code: Option<i32>,
    journal_stop_code: Option<&'static str>,
    oom_kill_count: Option<u64>,
) {
    std::thread::Builder::new()
        .name(format!("desc-watch-{workload_id}"))
        .spawn(move || {
            let journal_stop_code = journal_stop_code.map(str::to_string);
            let mut observation_errors: u32 = 0;
            // 종료 신호 수신기는 루프 밖에서 한 번만 만든다(틱마다
            // `watch::Receiver`를 새로 할당하던 낭비를 없앤다).
            let shutdown = state.shutdown.subscribe();
            loop {
                if *shutdown.borrow() {
                    if let Some(group) = state
                        .workload_entry(&workload_id)
                        .and_then(|e| e.lock().unwrap_or_else(|p| p.into_inner()).group.clone())
                    {
                        let _ = state
                            .platform
                            .terminate_owned(&group, term_platform::StopPhase::Force);
                    }
                    break;
                }
                let empty = state
                    .workload_entry(&workload_id)
                    .and_then(|e| e.lock().unwrap_or_else(|p| p.into_inner()).group.clone())
                    .map(|group| match state.platform.is_empty(&group) {
                        Ok(empty) => {
                            observation_errors = 0;
                            empty
                        }
                        Err(error) => {
                            observation_errors += 1;
                            let finalize = observation_error_finalizes(observation_errors);
                            if finalize {
                                tracing::warn!(
                                    workload = %workload_id,
                                    error_streak = observation_errors,
                                    %error,
                                    "descendant watcher cannot observe the workload group; finalizing"
                                );
                            }
                            finalize
                        }
                    })
                    .unwrap_or(true);
                if empty {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            if let Some(entry) = state.workload_entry(&workload_id) {
                let (session_id, reserved) = {
                    let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
                    (guard.session_id.clone(), guard.reserved)
                };
                finish_workload(
                    &state,
                    &workload_id,
                    &session_id,
                    false,
                    exit_code,
                    false,
                    reserved,
                    journal_stop_code,
                    oom_kill_count,
                );
            }
        })
        .expect("spawn descendant watcher");
}

/// Windows NTSTATUS for "allocation failed because the commit limit (job
/// memory cap) was reached" — job-object workloads die with this code.
const STATUS_COMMITMENT_LIMIT: u32 = 0xC000012D;

/// NTSTATUS 종료 코드는 최상위 비트가 서 있어 `i32`로는 음수다 —
/// `u32::try_from`은 그래서 언제나 실패했고 커밋 한도 kill이 OOM으로
/// 분류되지 않았다. 비트 패턴으로 비교한다.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_commitment_limit_exit(code: i32) -> bool {
    code as u32 == STATUS_COMMITMENT_LIMIT
}

/// 시작 경로 검사(01 §4 CWD_UNAVAILABLE). 실패에는 `reason_code`
/// (`cwd_missing`·`cwd_permission_denied`·`cwd_not_directory`·
/// `cwd_unavailable`)를 실어 UI가 사유를 알리며 가까운 상위 경로로 다시
/// 시도하게 한다(04-ui §2-4). 검색 권한(x)까지 여기서 본다 — 없으면 자식의
/// chdir이 SPAWN_FAILED로 끝나 경로 문제인 줄 모르고, 대체 경로도 못 탄다.
fn usable_cwd(cwd: &str) -> Result<std::path::PathBuf, RpcError> {
    let unavailable = |reason: &str, message: &'static str| {
        RpcError::new(ErrorCode::CwdUnavailable, message)
            .with_details(serde_json::json!({ "reason_code": reason }))
    };
    let canonical =
        std::fs::canonicalize(std::path::Path::new(cwd)).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                unavailable("cwd_missing", "cwd cannot be canonicalized")
            }
            // macOS 개인정보 보호 폴더(TCC)의 EPERM도 여기로 온다.
            std::io::ErrorKind::PermissionDenied => {
                unavailable("cwd_permission_denied", "cwd is not accessible")
            }
            _ => unavailable("cwd_unavailable", "cwd cannot be canonicalized"),
        })?;
    if !canonical.is_dir() {
        return Err(unavailable("cwd_not_directory", "cwd is not a directory"));
    }
    if !directory_searchable(&canonical) {
        return Err(unavailable(
            "cwd_permission_denied",
            "cwd is not accessible",
        ));
    }
    Ok(canonical)
}

/// 이 프로세스가 디렉터리로 chdir할 수 있는가(검색 권한).
#[cfg(unix)]
fn directory_searchable(dir: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: NUL로 끝나는 유효한 경로 문자열을 넘기고, 포인터는 호출 동안 산다.
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(not(unix))]
fn directory_searchable(_dir: &std::path::Path) -> bool {
    true
}

/// `program`이 `env`일 때 env가 실행할 실행 파일(argv에서 옵션·`NAME=값`
/// 다음의 첫 낱말). 판단할 수 없는 옵션을 만나거나 PATH 검색(상대 이름)이면
/// None — 그때는 아무것도 거절하지 않는다.
fn env_wrapped_program<'a>(program: &str, argv: &'a [String]) -> Option<&'a str> {
    if !cfg!(unix) || std::path::Path::new(program).file_name()? != "env" {
        return None;
    }
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-u" | "--unset" => {
                args.next()?;
            }
            "-i" | "--ignore-environment" | "-" => {}
            "--" => {
                return args
                    .next()
                    .map(String::as_str)
                    .filter(|p| p.starts_with('/'))
            }
            a if a.starts_with("--unset=") => {}
            a if a.starts_with('-') => return None,
            a if a.contains('=') => {}
            a => return Some(a).filter(|p| p.starts_with('/')),
        }
    }
    None
}

/// 실행 파일 확장자가 셸 shim(`.cmd/.bat/.ps1`)인지 — 대소문자 무시.
fn has_shell_shim_extension(program: &str) -> bool {
    std::path::Path::new(program)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            e.eq_ignore_ascii_case("cmd")
                || e.eq_ignore_ascii_case("bat")
                || e.eq_ignore_ascii_case("ps1")
        })
        .unwrap_or(false)
}

/// 종료 사유 한 줄 분류(SOTA_GAP_REVIEW W1-1). 우선순위: 사용자 취소 >
/// 저널 상한(기록 손실) > OOM kill > 정상 프로세스 종료 > 관측 불가.
/// `detail`은 현지화하지 않는 기술 근거다 — 문구는 UI가 reason enum에서
/// 만든다. 이 enum 값의 의미는 스키마로 동결한다.
fn classify_exit(
    cancelled: bool,
    exit_code: Option<i32>,
    journal_stop_code: Option<&str>,
    oom_kill_count: Option<u64>,
) -> (ExitReason, Option<String>) {
    if cancelled {
        return (ExitReason::Cancelled, None);
    }
    if let Some(code) = journal_stop_code {
        return (ExitReason::JournalLimit, Some(code.to_string()));
    }
    if let Some(count) = oom_kill_count {
        if count > 0 {
            return (
                ExitReason::OomKill,
                Some(format!("memory.events oom_kill={count}")),
            );
        }
    }
    if let Some(code) = exit_code {
        #[cfg(windows)]
        {
            if is_commitment_limit_exit(code) {
                return (
                    ExitReason::OomKill,
                    Some("exit 0xC000012D STATUS_COMMITMENT_LIMIT".to_string()),
                );
            }
        }
        #[cfg(not(windows))]
        let _code = code;
        return (ExitReason::ProcessExit, None);
    }
    (ExitReason::Unknown, None)
}

/// The storage reason code that rides with the terminal transition
/// (`lifecycle_events.reason_code` — 이미 쓰던 코드 체계를 잇는다).
fn storage_reason_code(reason: ExitReason, journal_stop_code: Option<&str>) -> Option<String> {
    match reason {
        ExitReason::Cancelled => Some("CANCELLED".to_string()),
        // 저널 상한은 기존 JOURNAL_LIMIT/DISK_FULL 코드를 보존한다.
        ExitReason::JournalLimit => journal_stop_code.map(str::to_string),
        ExitReason::OomKill => Some("OOM_KILL".to_string()),
        ExitReason::ProcessExit | ExitReason::Unknown => None,
    }
}

/// DRAINING → terminal transition + cleanup + events (runs once).
#[allow(clippy::too_many_arguments)]
fn finish_workload(
    state: &Arc<DaemonState>,
    workload_id: &WorkloadId,
    session_id: &SessionId,
    cancelled: bool,
    exit_code: Option<i32>,
    descendants_remaining: bool,
    reserved: bool,
    journal_stop_code: Option<String>,
    oom_kill_count: Option<u64>,
) {
    // A cancel that raced the natural teardown (cancel arriving while owned
    // descendants were still being watched, or after the actor finalized on
    // a journal stop) must still land on CANCELLED, never SUCCEEDED/FAILED
    // (spec 01 §5; B08/B18 acceptance).
    let cancel_requested = state
        .workload_entry(workload_id)
        .is_some_and(|e| e.lock().unwrap_or_else(|p| p.into_inner()).cancel_requested);

    // 종료 사유는 "실효 취소" 여부로 분류한다(W1-1).
    let (exit_reason, exit_detail) = classify_exit(
        cancelled || cancel_requested,
        exit_code,
        journal_stop_code.as_deref(),
        oom_kill_count,
    );
    let storage_reason = storage_reason_code(exit_reason, journal_stop_code.as_deref());

    // DRAINING first (legal from RUNNING/STOPPING); tolerate states that
    // cannot drain (e.g. a FAILED during STARTING).
    let current = state
        .workload_entry(workload_id)
        .map(|e| e.lock().unwrap_or_else(|p| p.into_inner()).state);
    if matches!(
        current,
        Some(WorkloadState::Running | WorkloadState::Stopping)
    ) {
        let _ = state
            .storage
            .transition_to(workload_id, WorkloadState::Draining);
        set_entry_state(state, workload_id, WorkloadState::Draining);
        state.workload_state_changed(workload_id);
    }

    let terminal = if cancelled || cancel_requested {
        WorkloadState::Cancelled
    } else if exit_code == Some(0) && !descendants_remaining {
        WorkloadState::Succeeded
    } else {
        WorkloadState::Failed
    };

    if let Err(e) = state
        .storage
        .mark_terminal(workload_id, terminal, exit_code, storage_reason)
    {
        tracing::warn!(workload = %workload_id, error = %e, "terminal transition rejected");
    }
    if let Some(entry) = state.workload_entry(workload_id) {
        let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        guard.state = terminal;
        guard.exit_code = exit_code;
        guard.actor = None;
        // 종료 후에는 아무도 descriptor를 읽지 않는다(런치 파이프라인이
        // 시작 시점에 복사해 간다). 전체 argv + env_overrides를 데몬 수명
        // 내내 붙들고 있을 이유가 없다 — spec §6의 "원문은 메모리에만"은
        // 실행 중에만 유효한 얘기다. 종료된 엔트리는 한동안 레지스트리에
        // 남으므로(FINISHED_WORKLOADS_RETAINED) 토큰 사본을 쥔 redactor도
        // 여기서 놓는다(저널 래퍼는 자기 Arc를 따로 쥐고 있다).
        guard.descriptor = None;
        guard.redactor = None;
        if journal_stop_code.is_some() {
            guard.last_error_code = journal_stop_code;
        }
    }

    // Group teardown AFTER terminal (spec §7: drain → reservation, writer,
    // master, group handle order).
    cleanup_group(state, workload_id);

    // Reservation release exactly once.
    if reserved {
        release_reservation(state, workload_id);
    }

    // pane이 끝났으니 이 워크로드의 에이전트 세션 행을 닫는다(§8).
    close_agent_sessions(state, workload_id);

    state.workload_state_changed(workload_id);
    state.broadcast_control(
        term_contracts::rpc::RpcEventKind::SessionExited,
        serde_json::to_value(&SessionExit {
            session_id: session_id.clone(),
            exit_code,
            descendants_remaining,
            reason: exit_reason,
            detail: exit_detail,
        })
        .unwrap_or(serde_json::Value::Null),
    );
    // 종료 워크로드 보관 링(스토리지가 이력의 진실 원본이다). 세션은
    // 워크로드가 종료 상태에 닿은 지금 보관 링에 든다.
    state.note_workload_terminal(workload_id);
    state.note_session_finalized(session_id);
    state.touch_activity();
}

// ---------------------------------------------------------------------------
// Cancel / stop

/// `workload.cancel` (idempotent): QUEUED → CANCELLED immediately; active →
/// STOPPING + Grace → grace window → Force.
pub fn cancel_workload(
    state: Arc<DaemonState>,
    workload_id: &WorkloadId,
) -> Result<WorkloadState, RpcError> {
    cancel_workload_with_force(state, workload_id, false)
}

/// Force cancellation skips the grace period and signals the entire owned group.
pub fn cancel_workload_with_force(
    state: Arc<DaemonState>,
    workload_id: &WorkloadId,
    force: bool,
) -> Result<WorkloadState, RpcError> {
    let Some(entry_arc) = state.workload_entry(workload_id) else {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            "workload not found",
        ));
    };
    let state_now = entry_arc.lock().unwrap_or_else(|p| p.into_inner()).state;

    if state_now.is_terminal() {
        return Ok(state_now);
    }
    let mut state_now = state_now;
    if state_now == WorkloadState::Queued {
        state.queue.cancel(workload_id);
        let _ = state.storage.set_cancel_requested(workload_id, true);
        // 스토리지의 종료 기록이 먼저다. 스케줄러가 방금 STARTING으로
        // 올렸으면(INVALID_STATE) 이 취소는 활성 경로로 넘어간다 — 예전엔
        // 레지스트리만 CANCELLED가 되고 진행 중인 런치의 예약을 풀어
        // 스토리지 행이 STARTING에 남았다.
        match state
            .storage
            .mark_terminal(workload_id, WorkloadState::Cancelled, None, None)
        {
            Ok(()) => {
                {
                    let mut guard = entry_arc.lock().unwrap_or_else(|p| p.into_inner());
                    guard.state = WorkloadState::Cancelled;
                    guard.cancel_requested = true;
                    guard.descriptor = None;
                    guard.redactor = None;
                }
                release_reservation(&state, workload_id);
                close_agent_sessions(&state, workload_id);
                state.broadcast_queue_changed();
                state.workload_state_changed(workload_id);
                state.note_workload_terminal(workload_id);
                return Ok(WorkloadState::Cancelled);
            }
            Err(term_storage::StorageError::InvalidState { .. }) => {
                tracing::debug!(
                    workload = %workload_id,
                    "cancel raced the scheduler admit; stopping the starting workload"
                );
                // 레지스트리가 아직 QUEUED를 보여도 스토리지는 STARTING 이상이다.
                let current = entry_arc.lock().unwrap_or_else(|p| p.into_inner()).state;
                if current.is_terminal() {
                    return Ok(current);
                }
                state_now = if current == WorkloadState::Queued {
                    WorkloadState::Starting
                } else {
                    current
                };
            }
            Err(e) => return Err(storage_error(e)),
        }
    }

    // Active (STARTING/RUNNING/STOPPING/DRAINING).
    let _ = state.storage.set_cancel_requested(workload_id, true);
    {
        let mut guard = entry_arc.lock().unwrap_or_else(|p| p.into_inner());
        guard.cancel_requested = true;
    }
    if matches!(state_now, WorkloadState::Starting | WorkloadState::Running) {
        let _ = state
            .storage
            .transition_to(workload_id, WorkloadState::Stopping);
        set_entry_state(&state, workload_id, WorkloadState::Stopping);
        state.workload_state_changed(workload_id);
    }
    if force {
        let group = entry_arc
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .group
            .clone();
        if let Some(group) = group {
            state
                .platform
                .terminate_owned(&group, term_platform::StopPhase::Force)
                .map_err(|e| RpcError::new(ErrorCode::DaemonUnavailable, e.to_string()))?;
        }
    }
    request_stop(&state, workload_id);
    Ok(WorkloadState::Stopping)
}

/// Fire the stop sequence: graceful signal, actor cancel, grace window,
/// then force.
///
/// Force is not one-shot: a tree can sit in uninterruptible sleep for a while
/// (a wedged exit, a full disk blocking teardown I/O), surviving SIGKILL
/// until the kernel call returns. The ladder re-forces on a backoff and only
/// then lands the workload in a terminal state — a workload parked in
/// STOPPING forever keeps holding admission and UI slots.
const STOP_FORCE_RETRY_ATTEMPTS: u32 = 3;
const STOP_FORCE_RETRY_INTERVAL: Duration = Duration::from_secs(10);

pub fn request_stop(state: &Arc<DaemonState>, workload_id: &WorkloadId) {
    let state = Arc::clone(state);
    let workload_id = workload_id.clone();
    std::thread::Builder::new()
        .name(format!("stop-{workload_id}"))
        .spawn(move || {
            let (group, actor) = match state.workload_entry(&workload_id) {
                Some(entry) => {
                    let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
                    (guard.group.clone(), guard.actor.clone())
                }
                None => return,
            };
            if let Some(group) = &group {
                // Windows R1: Grace is a documented no-op (02-runner §7).
                let _ = state
                    .platform
                    .terminate_owned(group, term_platform::StopPhase::Grace);
            }
            if let Some(actor) = &actor {
                actor.cancel();
            }
            let deadline = Instant::now() + state.config.stop_grace();
            while Instant::now() < deadline {
                if workload_is_terminal(&state, &workload_id) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if let Some(group) = &group {
                let _ = state
                    .platform
                    .terminate_owned(group, term_platform::StopPhase::Force);
            }
            if let Some(actor) = &actor {
                actor.cancel();
            }
            // Final wait: the actor drain is bounded (2 s timeout).
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if workload_is_terminal(&state, &workload_id) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            for attempt in 1..=STOP_FORCE_RETRY_ATTEMPTS {
                std::thread::sleep(STOP_FORCE_RETRY_INTERVAL);
                if workload_is_terminal(&state, &workload_id) {
                    return;
                }
                if let Some(group) = &group {
                    let _ = state
                        .platform
                        .terminate_owned(group, term_platform::StopPhase::Force);
                }
                tracing::warn!(
                    workload = %workload_id,
                    attempt,
                    "workload still not terminal after force stop"
                );
            }
            if workload_is_terminal(&state, &workload_id) {
                return;
            }
            // Bounded give-up: finalize as CANCELLED with stranded members.
            // Surviving processes are orphaned to the OS — nothing running as
            // this daemon can kill them — but the workload itself stops
            // occupying a stop/admission slot forever.
            let (session_id, reserved) = match state.workload_entry(&workload_id) {
                Some(entry) => {
                    let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
                    (guard.session_id.clone(), guard.reserved)
                }
                None => return,
            };
            tracing::warn!(
                workload = %workload_id,
                "workload did not reach terminal after force stop; finalizing with stranded members"
            );
            finish_workload(
                &state,
                &workload_id,
                &session_id,
                true,
                None,
                true,
                reserved,
                None,
                None,
            );
        })
        .expect("spawn stop task");
}

fn workload_is_terminal(state: &Arc<DaemonState>, workload_id: &WorkloadId) -> bool {
    state.workload_entry(workload_id).is_some_and(|e| {
        e.lock()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .is_terminal()
    })
}

/// Stop every active workload (daemon shutdown with stop_workloads).
pub fn stop_all_workloads(state: &Arc<DaemonState>) {
    let ids: Vec<WorkloadId> = state
        .workloads
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .filter_map(|e| {
            let guard = e.lock().unwrap_or_else(|p| p.into_inner());
            (!guard.state.is_terminal()).then_some(guard.workload_id.clone())
        })
        .collect();
    for id in ids {
        let _ = cancel_workload(Arc::clone(state), &id);
    }
    // Bound the wait so shutdown never hangs on a stubborn group.
    let deadline = Instant::now() + state.config.stop_grace() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let active = state
            .workloads
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .any(|e| {
                !e.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .state
                    .is_terminal()
            });
        if !active {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// Scheduler entry point

/// Start one QUEUED workload the scheduler admitted (the reservation is
/// already held by the ledger). Runs the post-admission pipeline on the
/// caller's blocking thread.
pub fn start_admitted(state: Arc<DaemonState>, workload_id: WorkloadId) {
    {
        let Some(entry) = state.workload_entry(&workload_id) else {
            return;
        };
        let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        if guard.state != WorkloadState::Queued || guard.descriptor.is_none() {
            drop(guard);
            release_reservation(&state, &workload_id);
            return;
        }
        guard.reserved = true;
    }
    state.queue.cancel(&workload_id);
    state.broadcast_queue_changed();
    match start_managed_admitted(&state, &workload_id) {
        Ok(new_state) => {
            tracing::info!(workload = %workload_id, ?new_state, "queued workload started")
        }
        Err(e) => {
            tracing::warn!(workload = %workload_id, error = %e, "queued workload failed to start")
        }
    }
}

// ---------------------------------------------------------------------------
// Error mapping

pub fn storage_error(e: term_storage::StorageError) -> RpcError {
    use term_storage::StorageError as S;
    match e {
        S::RequestConflict { .. } => RpcError::new(ErrorCode::RequestConflict, e.to_string()),
        S::InvalidState { .. } => RpcError::new(ErrorCode::InvalidState, e.to_string()),
        S::InvalidArgument(_) => RpcError::new(ErrorCode::InvalidArgument, e.to_string()),
        S::WorkloadNotFound { .. } | S::SessionNotFound { .. } => {
            RpcError::new(ErrorCode::InvalidArgument, e.to_string())
        }
        other => RpcError::new(ErrorCode::DaemonUnavailable, other.to_string()),
    }
}

pub fn core_error(e: CoreError) -> RpcError {
    RpcError::new(e.rpc_code(), e.to_string())
}

#[cfg(test)]
mod env_identity_tests {
    use super::*;

    #[test]
    fn identity_vars_are_added_and_always_win_over_the_request() {
        let session_id = SessionId::generate();
        let workload_id = WorkloadId::generate();
        let mut requested = std::collections::BTreeMap::new();
        requested.insert("EDITOR".to_string(), "vim".to_string());
        // 클라이언트가 흉내 낸 값은 덮어쓴다.
        requested.insert(ENV_SESSION_ID.to_string(), "spoofed".to_string());

        let env = with_iyagi_identity(&requested, &session_id, &workload_id);

        assert_eq!(env.get("EDITOR").map(String::as_str), Some("vim"));
        assert_eq!(
            env.get(ENV_SESSION_ID).map(String::as_str),
            Some(session_id.as_str())
        );
        assert_eq!(
            env.get(ENV_WORKLOAD_ID).map(String::as_str),
            Some(workload_id.as_str())
        );
        assert_eq!(env.len(), 3, "요청의 다른 키는 그대로 남는다");
        // 원본은 건드리지 않는다(fingerprint는 요청에서 계산된다).
        assert_eq!(
            requested.get(ENV_SESSION_ID).map(String::as_str),
            Some("spoofed")
        );
    }

    #[test]
    fn empty_overrides_still_get_both_identity_vars() {
        let env = with_iyagi_identity(
            &std::collections::BTreeMap::new(),
            &SessionId::generate(),
            &WorkloadId::generate(),
        );
        assert_eq!(env.len(), 2);
        assert!(env.contains_key(ENV_SESSION_ID) && env.contains_key(ENV_WORKLOAD_ID));
    }
}

#[cfg(test)]
mod launch_check_tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// UI가 보내는 셸 래퍼(`env -u … <셸> -l -i`)에서 셸 자리를 찾는다.
    #[cfg(unix)]
    #[test]
    fn env_wrapped_program_finds_the_shell_after_env_options() {
        let ui = argv(&[
            "-u",
            "NO_COLOR",
            "-u",
            "FORCE_COLOR",
            "/opt/homebrew/bin/fish",
            "-l",
        ]);
        assert_eq!(
            env_wrapped_program("/usr/bin/env", &ui),
            Some("/opt/homebrew/bin/fish")
        );
        let assigned = argv(&["-i", "LANG=C", "--unset=X", "--", "/bin/zsh"]);
        assert_eq!(
            env_wrapped_program("/usr/bin/env", &assigned),
            Some("/bin/zsh")
        );
        // PATH 검색·모르는 옵션·env가 아닌 실행 파일은 판단하지 않는다.
        assert_eq!(env_wrapped_program("/usr/bin/env", &argv(&["zsh"])), None);
        assert_eq!(
            env_wrapped_program("/usr/bin/env", &argv(&["-S", "zsh -l"])),
            None
        );
        assert_eq!(env_wrapped_program("/bin/zsh", &argv(&["/bin/sh"])), None);
        assert_eq!(env_wrapped_program("/usr/bin/env", &argv(&["-u"])), None);
    }

    fn reason(err: &RpcError) -> &str {
        err.details
            .as_ref()
            .and_then(|d| d["reason_code"].as_str())
            .unwrap_or_default()
    }

    /// 쓸 수 없는 시작 경로는 사유와 함께 CWD_UNAVAILABLE이다 — UI가 그
    /// 사유로 알리며 상위 경로로 다시 시도한다.
    #[test]
    fn usable_cwd_reports_why_a_directory_cannot_be_used() {
        let dir = tempfile::tempdir().unwrap();
        assert!(usable_cwd(dir.path().to_str().unwrap()).is_ok());

        let gone = dir.path().join("gone");
        let err = usable_cwd(gone.to_str().unwrap()).unwrap_err();
        assert_eq!(err.code, ErrorCode::CwdUnavailable);
        assert_eq!(reason(&err), "cwd_missing");

        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        let err = usable_cwd(file.to_str().unwrap()).unwrap_err();
        assert_eq!(reason(&err), "cwd_not_directory");
    }

    #[cfg(unix)]
    #[test]
    fn usable_cwd_rejects_a_directory_the_daemon_cannot_enter() {
        use std::os::unix::fs::PermissionsExt as _;
        // root는 권한 비트를 무시한다 — 그 환경에서는 볼 것이 없다.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
        let err = usable_cwd(locked.to_str().unwrap()).unwrap_err();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(err.code, ErrorCode::CwdUnavailable);
        assert_eq!(reason(&err), "cwd_permission_denied");
    }
}

#[cfg(test)]
mod exit_reason_tests {
    use super::*;

    /// 우선순위 계약(W1-1): 취소 > 저널 상한 > OOM > 정상 종료 > 관측 불가.
    /// 이 순서와 enum 의미는 스키마로 동결한다 — 변경 시 계약 업데이트 필요.
    #[test]
    fn classify_exit_priority_cancel_over_journal_over_oom() {
        // 취소가 무엇보다 앞선다(사용자 행동이 사유다).
        assert_eq!(
            classify_exit(true, Some(137), Some("JOURNAL_LIMIT"), Some(1)),
            (ExitReason::Cancelled, None)
        );
        // 저널 상한은 기록 손실 사실을 detail에 보존한다.
        assert_eq!(
            classify_exit(false, Some(0), Some("DISK_FULL"), Some(1)),
            (ExitReason::JournalLimit, Some("DISK_FULL".to_string()))
        );
        // OOM 카운터가 0이면 OOM이 아니다(None과 구분된다).
        assert_eq!(
            classify_exit(false, Some(1), None, Some(0)),
            (ExitReason::ProcessExit, None)
        );
        assert_eq!(
            classify_exit(false, Some(137), None, Some(2)),
            (
                ExitReason::OomKill,
                Some("memory.events oom_kill=2".to_string())
            )
        );
        // 코드 없이 관측 수단도 없으면 Unknown.
        assert_eq!(
            classify_exit(false, None, None, None),
            (ExitReason::Unknown, None)
        );
    }

    /// STATUS_COMMITMENT_LIMIT(0xC000012D)은 i32로 음수 — 비트 비교여야 잡힌다.
    #[test]
    fn commitment_limit_exit_code_is_recognized_as_negative_i32() {
        assert!(is_commitment_limit_exit(0xC000_012Du32 as i32));
        assert!(is_commitment_limit_exit(-1_073_741_523));
        assert!(!is_commitment_limit_exit(1));
        assert!(!is_commitment_limit_exit(-1));
        assert!(!is_commitment_limit_exit(0));
    }

    #[cfg(windows)]
    #[test]
    fn classify_exit_maps_commitment_limit_to_oom_kill() {
        assert_eq!(
            classify_exit(false, Some(0xC000_012Du32 as i32), None, None),
            (
                ExitReason::OomKill,
                Some("exit 0xC000012D STATUS_COMMITMENT_LIMIT".to_string())
            )
        );
    }

    #[test]
    fn shell_shim_extensions_are_detected_case_insensitively() {
        assert!(has_shell_shim_extension(
            r"C:\Users\me\AppData\Roaming\npm\claude.cmd"
        ));
        assert!(has_shell_shim_extension(r"D:\tools\run.BAT"));
        assert!(has_shell_shim_extension("script.Ps1"));
        assert!(!has_shell_shim_extension(
            r"C:\Program Files\nodejs\node.exe"
        ));
        assert!(!has_shell_shim_extension("/usr/local/bin/claude"));
        assert!(!has_shell_shim_extension("claude.cmd.exe"));
    }

    #[test]
    fn storage_reason_code_preserves_journal_codes() {
        assert_eq!(
            storage_reason_code(ExitReason::Cancelled, None),
            Some("CANCELLED".to_string())
        );
        assert_eq!(
            storage_reason_code(ExitReason::JournalLimit, Some("DISK_FULL")),
            Some("DISK_FULL".to_string())
        );
        assert_eq!(
            storage_reason_code(ExitReason::OomKill, None),
            Some("OOM_KILL".to_string())
        );
        assert_eq!(storage_reason_code(ExitReason::ProcessExit, None), None);
        assert_eq!(storage_reason_code(ExitReason::Unknown, None), None);
    }
}

/// 정지 정책(B1/B2): 강제 종료 사다리의 상한과 관측 오류 처리 계약.
// 이 모듈의 단언은 상수 그 자체를 고정하는 계약 테스트다.
#[cfg(test)]
#[allow(clippy::assertions_on_constants)]
mod stop_policy_tests {
    use super::*;

    /// 관측 오류 하나로 워크로드를 마감하면 안 된다 — 그룹 멤버가 살아
    /// 있는데 마감하는 버그였다. 오류 streak이 한계에 이르러야 마감한다.
    #[test]
    fn an_observation_error_never_finalizes_before_the_streak_limit() {
        assert!(
            !observation_error_finalizes(1),
            "the first observation error must keep the watcher waiting"
        );
        assert!(!observation_error_finalizes(
            WATCHER_OBSERVATION_ERROR_LIMIT - 1
        ));
        assert!(observation_error_finalizes(WATCHER_OBSERVATION_ERROR_LIMIT));
        assert!(observation_error_finalizes(
            WATCHER_OBSERVATION_ERROR_LIMIT + 1
        ));
    }

    /// 관측 오류 한계는 300ms 틱 기준 30초 안팎에 머문다: 마감이 너무
    /// 늦으면 워크로드가 STOPPING에 오래 머물고, 너무 빠르면 일시 오류에
    /// 마감해 버린다.
    #[test]
    fn the_observation_error_limit_stays_near_thirty_seconds() {
        assert!(
            WATCHER_OBSERVATION_ERROR_LIMIT >= 50,
            "too eager to finalize"
        );
        assert!(
            WATCHER_OBSERVATION_ERROR_LIMIT <= 200,
            "holds STOPPING too long"
        );
    }

    /// 강제 종료 재시도 예산은 유한하고 문서화된 상한(60초) 안에 머문다 —
    /// 사다리가 다시 무한 루프로 회귀하지 않는지 이 계약이 지킨다.
    #[test]
    fn the_force_stop_retry_budget_stays_bounded() {
        assert!(
            STOP_FORCE_RETRY_ATTEMPTS >= 1,
            "force must be retried at least once"
        );
        let worst_case = STOP_FORCE_RETRY_ATTEMPTS as u64 * STOP_FORCE_RETRY_INTERVAL.as_secs();
        assert!(
            worst_case <= 60,
            "force-stop retries must not hold the stop thread longer than a minute"
        );
    }
}
