//! AI 코딩 에이전트 감시(자동 감지 베이스): 셸 세션의 PTY 루트(셸)에서
//! 프로세스 트리를 주기적으로 관찰해 claude/codex/opencode 실행을 알아낸다.
//!
//! 감지는 출력 스캐닝이 아니라 프로세스 관찰로 한다 — 테마·버전·언어와
//! 무관하게 동작한다. 결과는 `WorkloadSummary.agent`에 실려 기존
//! `workload.changed` 이벤트 흐름을 그대로 탄다(별도 이벤트·RPC 없음).
//! 관리(managed) 실행은 트리 감시가 필요 없다 — 런치 시점에 명령 서명만
//! 매칭해 찍는다([`stamp_command_agent`]).
//!
//! 감지된 프로세스마다 에이전트 **자체 세션 id**도 알아내
//! ([`crate::agent_session`]) `AgentStatus.session_*`에 싣고
//! `agent_sessions` 테이블에 남긴다(spec `02-runner.md` §8). 저장은 이
//! 감시 스레드에서 동기 스토리지 API로 한다 — 실패는 경고 한 줄이고
//! 감시는 계속 돈다(세션 기록은 편의 기능이지 실행의 전제가 아니다).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use term_contracts::agent_session::{valid_session_id, AgentSessionSource};
use term_contracts::ids::WorkloadId;
use term_contracts::launch::LaunchMode;
use term_contracts::snapshot::AgentStatus;
use term_contracts::state::WorkloadState;
use term_platform::proc_scan::{scan_process_trees, ProcessBrief};
use term_storage::AgentSessionUpsert;

use crate::agent_session::{self, Resolved};
use crate::state::DaemonState;

/// 감시 틱 주기. 1초면 배지 반응이 즉각적이고 sysinfo 열거(cmd/exe만)
/// 비용도 무시할 만하다.
const TICK: Duration = Duration::from_secs(1);

/// Codex 세션 재확인 간격(틱). Claude는 600바이트 레지스트리 한 번 읽기라
/// 매 틱 다시 읽어 이름·상태를 살아 있게 유지하지만, Codex는 fd 테이블
/// 열거라 값이 비싸고 한 번 잡으면 잘 바뀌지 않는다.
const CODEX_RERESOLVE_EVERY: u32 = 3;

/// 세션을 연속으로 이만큼 다시 알아내지 못하면 알던 값을 버린다. 레지스트리
/// 파일이 지워졌는데도(대화 종료) 예전 id를 계속 싣고 저장하면 목록에 유령
/// 대화가 `active`로 남는다. 버리고 나면 `should_reresolve`가 곧바로 다시
/// 알아보게 한다 — 잠깐 읽기가 막힌 것이라면 다음 틱에 회복한다.
const RESOLVE_FAILURE_LIMIT: u32 = 5;

/// `agent_sessions.end_reason` 코드(§8). 계약 문서의 집합과 같다.
pub(crate) const END_WORKLOAD_EXITED: &str = "workload_exited";
const END_REPLACED: &str = "replaced";

/// 한 에이전트의 감지 서명. `binaries`는 실행 파일/명령 이름 정확 일치,
/// `path_markers`는 argv·경로에 들어 있는 패키지 경로 단편(npm 설치 형태).
struct AgentSignature {
    id: &'static str,
    binaries: &'static [&'static str],
    path_markers: &'static [&'static str],
}

/// 감지 대상 서명 테이블. 새 에이전트는 여기 한 줄만 추가하면 된다.
const SIGNATURES: &[AgentSignature] = &[
    AgentSignature {
        id: "claude",
        binaries: &["claude"],
        path_markers: &["@anthropic-ai/claude-code"],
    },
    AgentSignature {
        id: "codex",
        binaries: &["codex"],
        path_markers: &["@openai/codex"],
    },
    AgentSignature {
        id: "opencode",
        binaries: &["opencode"],
        path_markers: &["@opencode-ai/opencode", "/opencode-ai/"],
    },
];

/// 경로의 마지막 요소. `/`와 `\` 둘 다 구분자로 본다(Windows 경로가 그대로
/// 들어온다). Windows 네이티브 설치본은 `claude.exe`이므로 `.exe`(대소문자
/// 무관)만 벗긴다 — 그 밖의 확장자는 남겨 `claude.md`는 `claude`와 다른
/// 이름으로 취급해 오탐(vim claude.md 등)을 막는다.
fn file_name(path: &str) -> &str {
    exe_name(path).0
}

/// [`file_name`]과 `.exe`를 벗겼는지 여부.
fn exe_name(path: &str) -> (&str, bool) {
    let normalized = path.trim_end_matches(['/', '\\']);
    let name = match normalized.rsplit_once(['/', '\\']) {
        Some((_, name)) => name,
        None => normalized,
    };
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.eq_ignore_ascii_case("exe") => (stem, true),
        _ => (name, false),
    }
}

/// `path`가 실행 파일 `binary`를 가리키는가. Windows 실행 파일(`.exe`)의
/// 이름은 대소문자를 가리지 않는다(`CLAUDE.EXE`) — 그 밖에는 정확히 같아야 한다.
fn names_binary(path: &str, binary: &str) -> bool {
    let (name, exe) = exe_name(path);
    name == binary || (exe && name.eq_ignore_ascii_case(binary))
}

/// 서명 하나가 이 프로세스와 일치하는가.
fn signature_matches(sig: &AgentSignature, process: &ProcessBrief) -> bool {
    if let Some(exe) = &process.exe {
        if sig.binaries.iter().any(|b| names_binary(exe, b)) {
            return true;
        }
    }
    for (index, arg) in process.cmd.iter().enumerate() {
        if index == 0 && sig.binaries.iter().any(|b| names_binary(arg, b)) {
            return true;
        }
        // 경로 구분자를 정규화해 Windows npm 경로도 잡는다.
        let normalized = arg.replace('\\', "/");
        if sig.path_markers.iter().any(|m| normalized.contains(m)) {
            return true;
        }
    }
    false
}

/// 트리에서 첫 번째로 일치하는 에이전트를 찾는다. 트리는 루트에서 가까운
/// 순(BFS)으로 들어 있으므로 "사용자가 실행한 것"이 먼저 잡힌다 —
/// claude가 보조 도구로 다른 에이전트를 띄워도 주인은 claude다.
pub fn detect_agent_in_tree(tree: &[ProcessBrief]) -> Option<(&'static str, u32)> {
    for process in tree {
        for sig in SIGNATURES {
            if signature_matches(sig, process) {
                return Some((sig.id, process.pid));
            }
        }
    }
    None
}

/// 명령행(관리 실행의 program/argv)에서 에이전트를 찾는다.
pub fn detect_agent_in_command(program: &str, argv: &[String]) -> Option<&'static str> {
    let mut cmd = vec![program.to_string()];
    cmd.extend(argv.iter().cloned());
    let brief = ProcessBrief {
        pid: 0,
        ppid: 0,
        name: file_name(program).to_string(),
        exe: Some(program.to_string()),
        cmd,
    };
    SIGNATURES
        .iter()
        .find(|sig| signature_matches(sig, &brief))
        .map(|sig| sig.id)
}

/// 실행 인수에 **선지정된** 에이전트 세션 id(`claude --resume <id>`,
/// `claude --session-id <id>`, `codex resume <id>`, `opencode --session <id>`). 판정은 계약의
/// [`valid_session_id`] 하나만 쓴다 — 그 규칙이 첫 글자를 영숫자로 못 박고
/// 있으므로 값이 아니라 다음 플래그인 경우(예: `codex resume --last`)도
/// 여기서 함께 걸린다.
pub fn session_id_from_argv(agent: &str, argv: &[String]) -> Option<String> {
    // Fork creates a different conversation; the plugin will report its new ID.
    if agent == "opencode"
        && argv
            .iter()
            .take_while(|arg| arg.as_str() != "--")
            .any(|arg| arg == "--fork" || arg == "--fork=true")
    {
        return None;
    }
    let takes_value = |flag: &str| -> bool {
        match flag {
            "--session-id" => agent == "claude" || agent == "codex",
            "--session" | "-s" => agent == "opencode",
            "--resume" | "-r" => agent == "claude",
            "resume" => agent == "codex",
            _ => false,
        }
    };
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        // `--flag=value` 형태도 받는다.
        if let Some((flag, value)) = arg.split_once('=') {
            if takes_value(flag) && valid_session_id(value) {
                return Some(value.to_string());
            }
            continue;
        }
        if !takes_value(arg) {
            continue;
        }
        // 플래그가 argv의 마지막이면 값이 없다 — 더 볼 것도 없다.
        let value = args.next()?;
        if valid_session_id(value) {
            return Some(value.clone());
        }
    }
    None
}

/// 관리 실행: 런치 확정 시점에 명령 서명으로 에이전트를 찍어 둔다.
/// `pid`는 Unix에서 exec 후 목표와 같은 PTY 첫 자식이다(Windows 헬퍼는
/// 목표를 기다리기만 해서 0을 전달한다).
///
/// 인수로 세션을 선지정했으면(`--resume`/`--session-id`/`resume`) 그 id를
/// 출처 `launch`로 곧바로 싣고 기록한다 — 관찰을 기다릴 필요가 없다(§8).
pub fn stamp_command_agent(
    state: &Arc<DaemonState>,
    workload_id: &WorkloadId,
    program: &str,
    argv: &[String],
    pid: u32,
) {
    let Some(agent) = detect_agent_in_command(program, argv) else {
        return;
    };
    let launched_session = session_id_from_argv(agent, argv);
    let Some(entry) = state.workload_entry(workload_id) else {
        return;
    };
    let (cwd, pty_session_id) = {
        let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        if guard.agent.as_ref().is_none_or(|a| a.agent != agent) {
            guard.agent = Some(AgentStatus {
                agent: agent.to_string(),
                pid,
                detected_at_ms: state.now_ms(),
                session_id: launched_session.clone(),
                session_name: None,
                session_source: launched_session
                    .as_ref()
                    .map(|_| AgentSessionSource::Launch),
                session_status: None,
                model: None,
                effort: None,
                model_source: None,
            });
        }
        (guard.cwd.clone(), guard.session_id.clone())
    };

    if let Some(session_id) = launched_session {
        persist_upsert(
            state,
            AgentSessionUpsert {
                workload_id: workload_id.clone(),
                pty_session_id: Some(pty_session_id),
                agent: agent.to_string(),
                agent_session_id: session_id,
                cwd,
                title: None,
                program: Some(program.to_string()),
                source: AgentSessionSource::Launch,
            },
        );
    }
}

/// 감시 루프: 1초마다 살아 있는 셸 세션의 트리를 훑고 변화가 있을 때만
/// `workload.changed`를 쏜다.
/// Supervised loop body (`supervisor::spawn_supervised`).
pub fn run(state: Arc<DaemonState>) {
    // 종료 신호 수신기와 관찰 캐시는 루프 밖에서 한 번만 만든다(재시작마다
    // 새로 만든다 — 캐시는 다음 틱에 다시 채워진다).
    let shutdown = state.shutdown.subscribe();
    let mut watched: HashMap<WorkloadId, Watched> = HashMap::new();
    loop {
        if *shutdown.borrow() {
            return;
        }
        tick(&state, &mut watched);
        std::thread::sleep(TICK);
    }
}

/// 한 워크로드에서 지금 보고 있는 에이전트(틱 사이에 살아남는 상태).
struct Watched {
    agent: String,
    pid: u32,
    /// 마지막으로 알아낸 세션. 아직 못 알아냈으면 `None`.
    resolved: Option<Resolved>,
    /// [`resolved`](Self::resolved)가 있는데 재확인이 연속으로 실패한 횟수.
    /// [`RESOLVE_FAILURE_LIMIT`]에 닿으면 알던 값을 버린다.
    resolve_failures: u32,
    /// 이 (agent, pid)를 처음 본 시각. (agent, pid)가 그대로인 한 유지해
    /// 표시용 경과 시간이 흔들리지 않게 한다.
    detected_at_ms: u64,
    /// 재확인 간격 카운터.
    ticks: u32,
    /// 마지막으로 저장한 (agent, 세션 id).
    persisted: Option<(String, String)>,
    /// 현재 모델·effort 추적(`model_watch`).
    model: crate::model_watch::ModelWatch,
}

/// 출처가 **프로세스 관찰**인가(registry/lock_file). 관찰 값은 재확인이
/// 연속 실패하면 버려야 하고, hook/launch 값은 관찰이 비어 있어도 유지해야
/// 한다 — 관찰은 hook이 본 세션을 볼 수 없다.
fn observed_source(source: Option<AgentSessionSource>) -> bool {
    matches!(
        source,
        Some(AgentSessionSource::Registry) | Some(AgentSessionSource::LockFile)
    )
}

/// 재확인 실패 한 번을 센다. 연속 실패가 [`RESOLVE_FAILURE_LIMIT`]에 닿으면
/// 카운터를 되돌리고 `true`(= 알던 세션을 버려라)를 돌려준다. 한 번의 실패로
/// 버리지 않는 이유는 레지스트리 쓰기 중간·일시적인 권한 오류로도 실패하기
/// 때문이다.
fn note_resolve_failure(failures: &mut u32) -> bool {
    *failures = failures.saturating_add(1);
    if *failures >= RESOLVE_FAILURE_LIMIT {
        *failures = 0;
        return true;
    }
    false
}

/// 이번 틱에 세션을 다시 알아볼 것인가. 아직 모르면 언제나 다시 본다.
fn should_reresolve(agent: &str, resolved: bool, ticks: u32) -> bool {
    if !resolved {
        return true;
    }
    match agent {
        // 레지스트리 한 번 읽기 — 이름·상태를 살아 있게 유지한다.
        "claude" => true,
        _ => ticks.is_multiple_of(CODEX_RERESOLVE_EVERY),
    }
}

/// `detected_at_ms`를 뺀 나머지가 같은가. 감지 시각은 (agent, pid)가
/// 그대로인 한 유지되므로 브로드캐스트 판정에서 제외한다.
fn agent_status_eq(a: &AgentStatus, b: &AgentStatus) -> bool {
    a.agent == b.agent
        && a.pid == b.pid
        && a.session_id == b.session_id
        && a.session_name == b.session_name
        && a.session_source == b.session_source
        && a.session_status == b.session_status
        && a.model == b.model
        && a.effort == b.effort
        && a.model_source == b.model_source
}

fn tick(state: &Arc<DaemonState>, watched: &mut HashMap<WorkloadId, Watched>) {
    // 감시 대상: RUNNING 셸 워크로드 + PTY 루트(셸) pid.
    let watchers: Vec<(WorkloadId, u32)> = {
        let registry = state.workloads.lock().unwrap_or_else(|p| p.into_inner());
        registry
            .values()
            .filter_map(|entry| {
                let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
                if guard.mode == LaunchMode::Shell && guard.state == WorkloadState::Running {
                    Some((guard.workload_id.clone(), guard.shell_pid?))
                } else {
                    None
                }
            })
            .collect()
    };
    // 감시 집합에서 빠진 워크로드(종료·셸 아님)는 세션을 한 번 닫고 잊는다.
    let live: HashSet<&WorkloadId> = watchers.iter().map(|(id, _)| id).collect();
    let gone: Vec<WorkloadId> = watched
        .keys()
        .filter(|id| !live.contains(id))
        .cloned()
        .collect();
    for workload_id in gone {
        crate::model_watch::forget_input(&workload_id);
        let entry = watched.remove(&workload_id);
        if entry.is_some_and(|e| e.persisted.is_some()) {
            end_workload_sessions(state, &workload_id);
        }
    }
    if watchers.is_empty() {
        return;
    }

    let roots: Vec<u32> = watchers.iter().map(|(_, pid)| *pid).collect();
    let trees: HashMap<u32, Vec<ProcessBrief>> = scan_process_trees(&roots);
    let now = state.now_ms();

    // Codex `/model`은 세션 기록보다 먼저 전역 config.toml에 저장된다 — 바뀌었으면
    // 마지막으로 입력받은 Codex pane에 귀속한다(model_watch 머리말 참고).
    let model_context = crate::model_watch::ModelContext::current(state);
    let codex_workloads: Vec<WorkloadId> = watched
        .iter()
        .filter(|(_, w)| w.agent == "codex")
        .map(|(id, _)| id.clone())
        .collect();
    if let Some(target) =
        crate::model_watch::codex_config_target(&model_context, now, &codex_workloads)
    {
        if let Some(watch) = watched.get_mut(&target) {
            watch.model.apply_codex_config_change(&model_context);
        }
    }

    for (workload_id, root) in watchers {
        let tree = trees.get(&root);
        let detected = tree
            .and_then(|tree| detect_agent_in_tree(tree))
            .map(|(agent, pid): (&str, u32)| (agent.to_string(), pid));

        let Some((agent, pid)) = detected else {
            // 에이전트가 사라졌다: 배지를 내리고 세션을 닫는다.
            if let Some(previous) = watched.remove(&workload_id) {
                if previous.persisted.is_some() {
                    end_workload_sessions(state, &workload_id);
                }
            }
            clear_agent_status(state, &workload_id);
            continue;
        };

        // (agent, pid)가 바뀌면 새 프로세스다 — 이전 세션을 닫고 처음부터.
        let fresh = watched
            .get(&workload_id)
            .is_none_or(|w| w.agent != agent || w.pid != pid);
        if fresh {
            if let Some(previous) = watched.remove(&workload_id) {
                if previous.persisted.is_some() {
                    end_workload_sessions(state, &workload_id);
                }
            }
            watched.insert(
                workload_id.clone(),
                Watched {
                    agent: agent.clone(),
                    pid,
                    resolved: None,
                    resolve_failures: 0,
                    detected_at_ms: now,
                    ticks: 0,
                    persisted: None,
                    model: crate::model_watch::ModelWatch::new(),
                },
            );
        }
        let Some(watch) = watched.get_mut(&workload_id) else {
            continue;
        };

        let brief = tree.and_then(|tree| tree.iter().find(|p| p.pid == pid));
        if let Some(brief) = brief {
            if should_reresolve(&agent, watch.resolved.is_some(), watch.ticks) {
                match agent_session::resolve(&agent, pid, brief) {
                    Some(resolved) => {
                        watch.resolved = Some(resolved);
                        watch.resolve_failures = 0;
                    }
                    // 연속 실패가 쌓이면 알던 세션을 버린다 — 대화가 끝났는데도
                    // (레지스트리 파일이 사라졌는데도) 예전 id를 계속 브로드캐스트
                    // ·저장하면 목록에 유령 대화가 `active`로 남는다.
                    None => {
                        if note_resolve_failure(&mut watch.resolve_failures) {
                            watch.resolved = None;
                        }
                    }
                }
            }
        }
        watch.ticks = watch.ticks.wrapping_add(1);
        crate::model_watch::refresh(
            &model_context,
            &agent,
            brief,
            watch.resolved.as_ref(),
            &mut watch.model,
        );

        let Some(entry) = state.workload_entry(&workload_id) else {
            continue;
        };
        let (broadcast, persist) = {
            let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
            let previous = guard.agent.as_ref();
            let mut status = AgentStatus {
                agent: agent.clone(),
                pid,
                detected_at_ms: watch.detected_at_ms,
                session_id: None,
                session_name: None,
                session_source: None,
                session_status: None,
                model: None,
                effort: None,
                model_source: None,
            };
            match &watch.resolved {
                Some(resolved) => {
                    status.session_id = Some(resolved.session_id.clone());
                    status.session_name = resolved.name.clone();
                    status.session_source = Some(resolved.source);
                    status.session_status = resolved.status.clone();
                }
                // 관찰로 못 알아냈으면 이미 아는 값(hook/launch)을 지우지
                // 않는다 — 같은 프로세스인 한 그 값이 여전히 최선이다.
                // 반대로 **관찰로 얻은** 값은 들고 가지 않는다: 여기 올 때
                // `resolved`가 비어 있다는 것은 재확인이 연속 실패해 방금
                // 버렸다는 뜻이고(RESOLVE_FAILURE_LIMIT), 다시 실으면 버린
                // 의미가 없다.
                None => {
                    if let Some(previous) = previous.filter(|p| {
                        p.agent == agent && p.pid == pid && !observed_source(p.session_source)
                    }) {
                        status.session_id = previous.session_id.clone();
                        status.session_name = previous.session_name.clone();
                        status.session_source = previous.session_source;
                        status.session_status = previous.session_status.clone();
                    }
                }
            }
            match watch.model.current() {
                Some((observation, source)) => {
                    status.model = observation.model.clone();
                    status.effort = observation.effort.clone();
                    status.model_source = Some(*source);
                }
                // 아직 못 알아냈으면 같은 프로세스가 이미 가진 값을 지우지 않는다.
                None => {
                    if let Some(previous) = previous.filter(|p| p.agent == agent && p.pid == pid) {
                        status.model = previous.model.clone();
                        status.effort = previous.effort.clone();
                        status.model_source = previous.model_source;
                    }
                }
            }
            let broadcast = !previous.is_some_and(|p| agent_status_eq(p, &status));
            let persist = status.session_id.clone().map(|session_id| {
                (
                    session_id,
                    guard.cwd.clone(),
                    guard.session_id.clone(),
                    status.session_source,
                )
            });
            if broadcast {
                guard.agent = Some(status);
            }
            (broadcast, persist)
        };
        if broadcast {
            state.workload_state_changed(&workload_id);
        }

        // 저장: 세션 id가 처음 잡혔거나 바뀌었을 때만.
        let Some((session_id, cwd, pty_session_id, source)) = persist else {
            continue;
        };
        let key = (agent.clone(), session_id.clone());
        if watched
            .get(&workload_id)
            .is_some_and(|w| w.persisted.as_ref() == Some(&key))
        {
            continue;
        }
        if let Some(watch) = watched.get(&workload_id) {
            if let Some((prev_agent, prev_session)) = watch.persisted.clone() {
                if let Err(error) = state.storage.end_agent_session(
                    &workload_id,
                    &prev_agent,
                    &prev_session,
                    END_REPLACED,
                ) {
                    tracing::warn!(workload = %workload_id, %error, "agent session end failed");
                }
            }
        }
        let resolved = watched.get(&workload_id).and_then(|w| w.resolved.clone());
        let program = brief.and_then(|brief| agent_session::observed_program(brief, &agent));
        let ok = persist_upsert(
            state,
            AgentSessionUpsert {
                workload_id: workload_id.clone(),
                pty_session_id: Some(pty_session_id),
                agent: agent.clone(),
                agent_session_id: session_id.clone(),
                cwd: resolved.as_ref().and_then(|r| r.cwd.clone()).unwrap_or(cwd),
                title: resolved.as_ref().and_then(|r| r.title.clone()),
                program,
                source: source.unwrap_or(AgentSessionSource::Registry),
            },
        );
        if ok {
            if let Some(watch) = watched.get_mut(&workload_id) {
                watch.persisted = Some(key);
            }
        }
    }
}

/// 배지를 내린다(에이전트가 사라졌을 때). 이미 없으면 아무것도 하지 않는다.
fn clear_agent_status(state: &Arc<DaemonState>, workload_id: &WorkloadId) {
    let Some(entry) = state.workload_entry(workload_id) else {
        return;
    };
    let cleared = {
        let mut guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        guard.agent.take().is_some()
    };
    if cleared {
        state.workload_state_changed(workload_id);
    }
}

fn end_workload_sessions(state: &Arc<DaemonState>, workload_id: &WorkloadId) {
    if let Err(error) = state
        .storage
        .end_agent_sessions_for_workload(workload_id, END_WORKLOAD_EXITED)
    {
        tracing::warn!(workload = %workload_id, %error, "agent session close failed");
    }
}

/// 저장 실패는 경고 한 줄이다 — 감시는 계속 돈다.
fn persist_upsert(state: &Arc<DaemonState>, upsert: AgentSessionUpsert) -> bool {
    let workload_id = upsert.workload_id.clone();
    match state.storage.upsert_agent_session(upsert) {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(workload = %workload_id, %error, "agent session upsert failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_resume_flags_identify_the_exact_session() {
        for args in [
            vec!["--session", "ses_123abc"],
            vec!["-s", "ses_123abc"],
            vec!["--session=ses_123abc"],
        ] {
            let args: Vec<_> = args.into_iter().map(str::to_string).collect();
            assert_eq!(
                session_id_from_argv("opencode", &args).as_deref(),
                Some("ses_123abc")
            );
            assert_eq!(session_id_from_argv("codex", &args), None);
        }
        for args in [
            vec!["--session", "--auto"],
            vec!["--continue"],
            vec!["--session-id", "ses_123abc"],
            vec!["--session", "ses_123abc", "--fork"],
            vec!["--", "--session", "ses_123abc"],
        ] {
            let args: Vec<_> = args.into_iter().map(str::to_string).collect();
            assert_eq!(session_id_from_argv("opencode", &args), None);
        }
    }

    fn brief(pid: u32, ppid: u32, exe: Option<&str>, cmd: &[&str]) -> ProcessBrief {
        ProcessBrief {
            pid,
            ppid,
            name: exe
                .and_then(|e| e.rsplit_once('/').map(|(_, n)| n))
                .unwrap_or_default()
                .to_string(),
            exe: exe.map(str::to_string),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn direct_binary_match_via_exe_and_argv0() {
        let tree = vec![
            brief(1, 0, Some("/bin/zsh"), &["-zsh"]),
            brief(2, 1, Some("/Users/x/.local/bin/claude"), &["claude"]),
        ];
        assert_eq!(detect_agent_in_tree(&tree), Some(("claude", 2)));

        let tree = vec![
            brief(1, 0, Some("/bin/zsh"), &["-zsh"]),
            // exe를 못 얻은 경우 argv[0] 이름으로도 잡는다.
            brief(3, 1, None, &["codex"]),
        ];
        assert_eq!(detect_agent_in_tree(&tree), Some(("codex", 3)));
    }

    #[test]
    fn npm_package_marker_matches_any_arg() {
        let tree = vec![
            brief(1, 0, Some("/bin/zsh"), &["-zsh"]),
            brief(
                2,
                1,
                Some("/usr/local/bin/node"),
                &[
                    "node",
                    "/Users/x/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                ],
            ),
        ];
        assert_eq!(detect_agent_in_tree(&tree), Some(("claude", 2)));

        let windows_style = brief(
            2,
            1,
            Some("C:\\node.exe"),
            &[
                "C:\\node.exe",
                "C:\\x\\node_modules\\@openai\\codex\\bin\\codex.js",
            ],
        );
        assert_eq!(detect_agent_in_tree(&[windows_style]), Some(("codex", 2)));
    }

    #[test]
    fn file_like_arguments_do_not_false_positive() {
        // vim claude.md / cat opencode-notes.txt 같은 인자는 잡지 않는다.
        let tree = vec![
            brief(1, 0, Some("/bin/zsh"), &["-zsh"]),
            brief(2, 1, Some("/usr/bin/vim"), &["vim", "claude.md"]),
            brief(3, 1, Some("/usr/bin/cat"), &["cat", "notes/opencode.md"]),
        ];
        assert_eq!(detect_agent_in_tree(&tree), None);
    }

    #[test]
    fn shallowest_match_wins() {
        // 셸 바로 아래 claude, 그 아래(깊이 2) codex가 있으면 claude가 주인.
        let tree = vec![
            brief(1, 0, Some("/bin/zsh"), &["-zsh"]),
            brief(2, 1, Some("/bin/claude"), &["claude"]),
            brief(3, 2, Some("/bin/codex"), &["codex"]),
        ];
        assert_eq!(detect_agent_in_tree(&tree), Some(("claude", 2)));
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn launch_arguments_pin_a_session_id_when_one_is_given() {
        const ID: &str = "7db2598e-c360-48fe-a2d5-0240993c9f7a";
        assert_eq!(
            session_id_from_argv("claude", &argv(&["--resume", ID])),
            Some(ID.to_string())
        );
        assert_eq!(
            session_id_from_argv("claude", &argv(&["-r", ID])),
            Some(ID.to_string())
        );
        assert_eq!(
            session_id_from_argv("claude", &argv(&["--session-id", ID, "--verbose"])),
            Some(ID.to_string())
        );
        assert_eq!(
            session_id_from_argv("claude", &argv(&[&format!("--session-id={ID}")])),
            Some(ID.to_string())
        );
        assert_eq!(
            session_id_from_argv("codex", &argv(&["resume", ID])),
            Some(ID.to_string())
        );
    }

    #[test]
    fn launch_arguments_without_a_real_id_pin_nothing() {
        const ID: &str = "7db2598e-c360-48fe-a2d5-0240993c9f7a";
        // 값이 아니라 다음 플래그다.
        assert_eq!(
            session_id_from_argv("codex", &argv(&["resume", "--last"])),
            None
        );
        // 값이 아예 없다.
        assert_eq!(session_id_from_argv("claude", &argv(&["--resume"])), None);
        // 경로 문자가 든 값은 세션 id가 아니다.
        assert_eq!(
            session_id_from_argv("claude", &argv(&["--resume", "../../etc/passwd"])),
            None
        );
        // 에이전트마다 받는 플래그가 다르다.
        assert_eq!(
            session_id_from_argv("codex", &argv(&["--resume", ID])),
            None
        );
        assert_eq!(session_id_from_argv("claude", &argv(&["resume", ID])), None);
        // 평범한 실행에는 아무것도 없다.
        assert_eq!(session_id_from_argv("claude", &argv(&["--continue"])), None);
        assert_eq!(session_id_from_argv("claude", &[]), None);
        // 플래그처럼 보이는 값은 계약 규칙(첫 글자 영숫자)에서 걸린다 —
        // 통과하면 `codex resume <id>`에 그대로 끼워져 플래그가 된다.
        for planted in [
            "--dangerously-bypass-approvals-and-sandbox",
            "-r",
            ".",
            "..",
        ] {
            assert_eq!(
                session_id_from_argv("codex", &argv(&["resume", planted])),
                None,
                "{planted:?}"
            );
            assert_eq!(
                session_id_from_argv("claude", &argv(&[&format!("--session-id={planted}")])),
                None,
                "{planted:?}"
            );
        }
    }

    /// 재확인이 연속으로 실패하면 알던 세션을 버린다 — 그러지 않으면 대화가
    /// 끝난 뒤에도 예전 id가 계속 브로드캐스트·저장된다.
    #[test]
    fn resolve_failures_drop_the_cached_session_after_the_limit() {
        let mut failures = 0;
        for attempt in 1..RESOLVE_FAILURE_LIMIT {
            assert!(
                !note_resolve_failure(&mut failures),
                "{attempt}번째 실패로는 버리지 않는다"
            );
            assert_eq!(failures, attempt);
        }
        assert!(note_resolve_failure(&mut failures), "상한에 닿으면 버린다");
        // 카운터는 되돌아간다 — 다음 주기를 처음부터 센다.
        assert_eq!(failures, 0);
        // 성공 한 번이 끼면 카운터는 호출자가 0으로 되돌린다(tick 참고).
        assert!(!note_resolve_failure(&mut failures));
    }

    /// 버린 뒤에 "이미 아는 값"으로 되살아나지 않게, 관찰 출처와 hook/launch
    /// 출처를 구분한다.
    #[test]
    fn only_observed_sources_are_dropped_on_resolve_failure() {
        assert!(observed_source(Some(AgentSessionSource::Registry)));
        assert!(observed_source(Some(AgentSessionSource::LockFile)));
        assert!(!observed_source(Some(AgentSessionSource::Hook)));
        assert!(!observed_source(Some(AgentSessionSource::Launch)));
        assert!(!observed_source(None));
    }

    #[test]
    fn reresolve_cadence_is_eager_until_known_then_agent_specific() {
        // 아직 모르면 언제나 다시 본다.
        for ticks in 0..5 {
            assert!(should_reresolve("claude", false, ticks));
            assert!(should_reresolve("codex", false, ticks));
        }
        // claude는 매 틱(레지스트리 한 번 읽기 — 상태/이름이 살아 있어야 한다).
        for ticks in 0..5 {
            assert!(should_reresolve("claude", true, ticks));
        }
        // codex는 3틱마다(fd 열거는 비싸다).
        assert!(should_reresolve("codex", true, 0));
        assert!(!should_reresolve("codex", true, 1));
        assert!(!should_reresolve("codex", true, 2));
        assert!(should_reresolve("codex", true, 3));
    }

    #[test]
    fn broadcast_comparison_ignores_only_the_detection_timestamp() {
        let base = AgentStatus {
            agent: "claude".into(),
            pid: 7,
            detected_at_ms: 1_000,
            session_id: Some("abc".into()),
            session_name: Some("name".into()),
            session_source: Some(AgentSessionSource::Registry),
            session_status: Some("idle".into()),
            model: Some("Opus 5 (1M context)".into()),
            effort: Some("xhigh".into()),
            model_source: Some(term_contracts::snapshot::AgentModelSource::StatusLine),
        };
        let mut later = base.clone();
        later.detected_at_ms = 99_000;
        assert!(agent_status_eq(&base, &later), "감지 시각은 비교에서 뺀다");

        for mutate in [
            (|s: &mut AgentStatus| s.pid = 8) as fn(&mut AgentStatus),
            |s| s.agent = "codex".into(),
            |s| s.session_id = Some("other".into()),
            |s| s.session_name = None,
            |s| s.session_source = Some(AgentSessionSource::Hook),
            |s| s.session_status = Some("busy".into()),
            |s| s.model = Some("Sonnet 5".into()),
            |s| s.effort = None,
            |s| s.model_source = Some(term_contracts::snapshot::AgentModelSource::Transcript),
        ] {
            let mut changed = base.clone();
            mutate(&mut changed);
            assert!(!agent_status_eq(&base, &changed), "{changed:?}");
        }
    }

    #[test]
    fn command_matching_covers_bare_name_and_path() {
        assert_eq!(
            detect_agent_in_command("claude", &["--continue".into()]),
            Some("claude")
        );
        assert_eq!(
            detect_agent_in_command("/opt/homebrew/bin/opencode", &["run".into()]),
            Some("opencode")
        );
        assert_eq!(
            detect_agent_in_command("python3", &["script.py".into()]),
            None
        );
        // Windows 네이티브 설치본: 역슬래시 경로 + `.exe`(대소문자 무관).
        assert_eq!(
            detect_agent_in_command(r"C:\Users\x\.local\bin\claude.exe", &[]),
            Some("claude")
        );
        assert_eq!(
            detect_agent_in_command(r"C:\npm\claude.exe", &["--continue".into()]),
            Some("claude")
        );
        assert_eq!(
            detect_agent_in_command(r"C:\Program Files\Claude\CLAUDE.EXE", &[]),
            Some("claude")
        );
        assert_eq!(detect_agent_in_command("claude.exe", &[]), Some("claude"));
        // `.exe`만 벗긴다 — 다른 확장자는 여전히 다른 이름이다.
        assert_eq!(detect_agent_in_command("/x/claude.md", &[]), None);
        assert_eq!(detect_agent_in_command(r"C:\x\claude.cmd", &[]), None);
        assert_eq!(detect_agent_in_command(".exe", &[]), None);
    }

    #[test]
    fn windows_native_binary_is_detected_in_the_tree() {
        let tree = vec![
            brief(1, 0, Some(r"C:\Windows\System32\cmd.exe"), &["cmd.exe"]),
            brief(
                2,
                1,
                Some(r"C:\Users\x\.local\bin\claude.exe"),
                &[r"C:\Users\x\.local\bin\claude.exe", "--continue"],
            ),
        ];
        assert_eq!(detect_agent_in_tree(&tree), Some(("claude", 2)));
        // exe를 못 얻어도 argv[0]의 `.exe` 이름으로 잡는다.
        let tree = vec![brief(3, 1, None, &["codex.exe", "resume"])];
        assert_eq!(detect_agent_in_tree(&tree), Some(("codex", 3)));
    }
}
