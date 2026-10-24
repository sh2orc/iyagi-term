//! 네트워크 끊김 자동 continue: 세션 출력에서 API 연결 오류 문구를
//! 감지하면(`armed`) 30초마다 인터넷을 확인하고, 회복되면 그 세션에
//! `continue`를 입력한다(`fired`). 오류 한 번당 전송 한 번 — 회복 전송
//! 뒤 같은 문구가 다시 출력되어야 다시 무장한다.
//!
//! 구성이 두 갈래다:
//! * **감지**(`scan_output`)는 actor 루프 스레드에서 `NotifySink::emit`이
//!   넘겨 주는 살아 있는 출력 청크로 굴러간다(저널 재생 경로는 sink를
//!   거치지 않아 재접속 재감지 오발이 없다). 롤링 테일로 청크 경계에
//!   걸린 문구도 잡는다(프론트 intervention 감지와 같은 관례).
//! * **회복 루프**(`run`)는 감독 스레드에서 무장 상태가 있을 때만 프로브한다.
//!   blocking reqwest 한 번(연결 상한이 DNS+TCP+TLS을 함께 유계로 묶는다).
//!
//! 쓰기는 예약 명령과 같은 actor 직접 경로다(스케줄러는 데몬 수준
//! 행위자 — view/epoch 소유권과 무관하다). 사용자가 에러 뒤 직접 입력한
//! 세션에는 끼어들지 않는다(InputLog 비교).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use term_contracts::auto_continue::{AutoContinuePayload, AutoContinueState};
use term_contracts::ids::{SessionId, WorkloadId};
use term_contracts::rpc::RpcEventKind;

use crate::model_watch;
use crate::scheduled_commands::{actor_for, command_bytes, wall_now_ms};
use crate::sessions::SessionEntry;
use crate::state::DaemonState;

/// 감지 문구 — 괄호 안 오류 코드(ENOTFOUND/ECONNREFUSED/…) 변형 전부를
/// 덮는 안정 부분만 매칭한다. ANSI 이스케이프가 문구 사이에 끼면 v1은
/// 못 본다(프론트 intervention 감지와 같은 단순 매치 한계).
pub const PHRASE: &str = "Can't reach the API server";
/// 프로브 대상: 도달 자체가 "인터넷 됨"이다(HTTP 상태 코드는 무시한다 —
/// 4xx/5xx도 DNS·경로·TLS가 살아 있다는 뜻이다).
const PROBE_URL: &str = "https://api.anthropic.com";
/// 청크 경계에 걸린 문구를 위한 세션별 롤링 테일.
const TAIL_BYTES: usize = 512;
/// 프로브 주기(사용자 지정: 30초). 무장되는 즉시 즉시 1회 돈다.
const PROBE_INTERVAL: Duration = Duration::from_secs(30);
/// 프로브 하나의 상한(연결+읽기를 함께 묶는다).
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// 회복 루프 틱 — 무장·해제가 이 안에 반영된다.
const TICK: Duration = Duration::from_secs(1);
/// PTY 쓰기 완료 대기 — `session.input`와 같은 상한.
const WRITE_TIMEOUT: Duration = Duration::from_millis(750);
/// 회복 시 보내는 입력(한 단어로 고정).
const CONTINUE_COMMAND: &str = "continue";

/// 세션별 감지 상태. `tail`은 문구를 못 찾은 청크도 이어 붙인다(경계 대응).
struct Detection {
    workload_id: WorkloadId,
    armed: bool,
    /// 감지 시각(데몬 단조 ms) — "사용자가 그 뒤에 입력했는가" 비교용.
    detected_at_mono: u64,
    /// 감지 시각(unix epoch ms) — 이벤트 페이로드용.
    detected_at_ms: u64,
    tail: Vec<u8>,
}

static DETECTIONS: Mutex<Option<HashMap<SessionId, Detection>>> = Mutex::new(None);

fn detections() -> std::sync::MutexGuard<'static, Option<HashMap<SessionId, Detection>>> {
    DETECTIONS.lock().unwrap_or_else(|p| p.into_inner())
}

/// `NotifySink::emit`이 부르는 감지 입구(actor 루프 스레드 — 락 두 개와
/// 부분문자열 검색만 하고 끝낸다). 문구 hit이면 무장하고 이벤트를 흘린다.
pub fn scan_output(state: &DaemonState, session: &Arc<SessionEntry>, bytes: &[u8]) {
    if !state.config.auto_continue {
        return;
    }
    let mut armed_at: Option<(WorkloadId, u64)> = None;
    {
        let mut guard = detections();
        let map = guard.get_or_insert_with(HashMap::new);
        let entry = map
            .entry(session.session_id.clone())
            .or_insert_with(|| Detection {
                workload_id: session.workload_id.clone(),
                armed: false,
                detected_at_mono: 0,
                detected_at_ms: 0,
                tail: Vec::new(),
            });
        entry.tail.extend_from_slice(bytes);
        let overflow = entry.tail.len().saturating_sub(TAIL_BYTES);
        if overflow > 0 {
            entry.tail.drain(..overflow);
        }
        // 이미 무장했으면 중복 hit은 no-op — 오류 한 번당 continue 한 번.
        if !entry.armed && find_phrase(&entry.tail) {
            entry.armed = true;
            entry.detected_at_mono = state.now_ms();
            entry.detected_at_ms = wall_now_ms();
            armed_at = Some((entry.workload_id.clone(), entry.detected_at_ms));
        }
    }
    if let Some((workload_id, at_ms)) = armed_at {
        broadcast(
            state,
            &session.session_id,
            &workload_id,
            AutoContinueState::Armed,
            None,
            at_ms,
        );
    }
}

/// 테일 안에서 문구 찾기. ASCII 문구라 바이트 검색으로 족하다.
fn find_phrase(tail: &[u8]) -> bool {
    tail.windows(PHRASE.len()).any(|w| w == PHRASE.as_bytes())
}

/// 세션 종료 훅(orchestrator::finalize_session): 상태만 걷는다(방송 없음).
pub fn forget_session(session_id: &SessionId) {
    detections()
        .get_or_insert_with(HashMap::new)
        .remove(session_id);
}

/// Supervised loop body (`supervisor::spawn_supervised`).
pub fn run(state: Arc<DaemonState>) {
    // 종료 신호 수신기는 루프 밖에서 한 번만 만든다(재시작마다 새로 만든다).
    let shutdown = state.shutdown.subscribe();
    let mut next_probe_at: Option<Instant> = None;
    while !*shutdown.borrow() {
        if state.config.auto_continue {
            let armed = armed_snapshot();
            if armed.is_empty() {
                // 무장 0으로 돌아오면 예약을 지운다 — 다음 무장은 즉시 프로브.
                next_probe_at = None;
            } else {
                let now = Instant::now();
                if next_probe_at.is_none_or(|at| now >= at) {
                    let ok = probe_network();
                    next_probe_at = Some(now + PROBE_INTERVAL);
                    if ok {
                        recover_all(&state, &armed);
                    }
                }
            }
        }
        std::thread::sleep(TICK);
    }
}

/// 무장 항목 스냅샷(회복 판정은 락 밖에서 한다).
fn armed_snapshot() -> Vec<(SessionId, WorkloadId, u64, u64)> {
    detections()
        .get_or_insert_with(HashMap::new)
        .iter()
        .filter(|(_, entry)| entry.armed)
        .map(|(session_id, entry)| {
            (
                session_id.clone(),
                entry.workload_id.clone(),
                entry.detected_at_mono,
                entry.detected_at_ms,
            )
        })
        .collect()
}

/// 회복 판정(순수 — 단위 테스트가 조건표를 고정한다).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryAction {
    Fire,
    Disarm(&'static str),
}

fn decide(
    session_alive: bool,
    guard_suspended: bool,
    user_input_since_detection: bool,
) -> RecoveryAction {
    if !session_alive {
        return RecoveryAction::Disarm("session-gone");
    }
    // 08 §5: 가드로 정지 중인 세션에는 쓰지 않는다(dispatch의 session.input
    // 거부와 같은 근거). 멈춘 세션은 어차피 CLI도 멈춰 있다.
    if guard_suspended {
        return RecoveryAction::Disarm("guard-suspended");
    }
    // 사용자가 에러 뒤 직접 입력했다면 그 세션은 사용자가 다루고 있다.
    if user_input_since_detection {
        return RecoveryAction::Disarm("user-input");
    }
    RecoveryAction::Fire
}

/// 프로브 성공 뒤 무장 항목 전부를 판정·실행한다.
fn recover_all(state: &Arc<DaemonState>, armed: &[(SessionId, WorkloadId, u64, u64)]) {
    for (session_id, workload_id, detected_at_mono, detected_at_ms) in armed {
        let session_alive = state.session(session_id).is_some();
        let guard_suspended = state.guard_view(workload_id).0.is_suspended();
        let user_input_since =
            model_watch::last_input_ms(workload_id).is_some_and(|at| at > *detected_at_mono);
        match decide(session_alive, guard_suspended, user_input_since) {
            RecoveryAction::Disarm(reason) => {
                disarm(state, session_id, workload_id, reason, *detected_at_ms);
            }
            RecoveryAction::Fire => fire(state, session_id, workload_id, *detected_at_ms),
        }
    }
}

/// `continue`를 보내고 fired를 방송한다. 쓰기가 일시적으로 막힌 경우
/// (입력 큐가 참, PTY가 멈춰 있음)는 무장을 유지해 다음 프로브에 재시도한다
/// — 예약 명령의 재시도 분류와 같은 규칙.
fn fire(
    state: &Arc<DaemonState>,
    session_id: &SessionId,
    workload_id: &WorkloadId,
    detected_at_ms: u64,
) {
    let Some(actor) = actor_for(state, workload_id) else {
        disarm(
            state,
            session_id,
            workload_id,
            "session-gone",
            detected_at_ms,
        );
        return;
    };
    // 회복 전송마다 새 input_id: actor의 멱등 dedup이 이전 시도를 재생하면
    // 안 되기 때문이다(타임아웃된 시도는 나중에 완료될 수 있다).
    let input_id = format!("auto-continue-{}", session_id);
    match actor.write_input_await(&input_id, &command_bytes(CONTINUE_COMMAND), WRITE_TIMEOUT) {
        Ok(_) => {
            model_watch::note_input(state.now_ms(), workload_id);
            finish(
                state,
                session_id,
                workload_id,
                AutoContinueState::Fired,
                None,
                wall_now_ms(),
            );
        }
        Err(error) if retryable_write_error(&error) => {
            tracing::debug!(%error, session = %session_id, "auto-continue write blocked; stays armed");
        }
        Err(error) => {
            tracing::warn!(%error, session = %session_id, "auto-continue write failed");
            disarm(
                state,
                session_id,
                workload_id,
                "write-failed",
                detected_at_ms,
            );
        }
    }
}

/// 쓰기 실패 중 재시도 대상은 "아무것도 writer에 닿지 않은" 것뿐이다.
fn retryable_write_error(error: &term_pty::actor::ActorError) -> bool {
    matches!(
        error,
        term_pty::actor::ActorError::Input(term_pty::input::InputQueueError::QueueFull { .. })
            | term_pty::actor::ActorError::InputStalled { .. }
    )
}

/// 무장을 걷고 disarmed를 방송한다.
fn disarm(
    state: &DaemonState,
    session_id: &SessionId,
    workload_id: &WorkloadId,
    reason: &str,
    detected_at_ms: u64,
) {
    finish(
        state,
        session_id,
        workload_id,
        AutoContinueState::Disarmed,
        Some(reason),
        detected_at_ms,
    );
}

/// 항목을 지우고(다음 오류 문구가 새 감지가 된다) 결과를 방송한다.
fn finish(
    state: &DaemonState,
    session_id: &SessionId,
    workload_id: &WorkloadId,
    new_state: AutoContinueState,
    reason: Option<&str>,
    at_ms: u64,
) {
    detections()
        .get_or_insert_with(HashMap::new)
        .remove(session_id);
    broadcast(state, session_id, workload_id, new_state, reason, at_ms);
}

fn broadcast(
    state: &DaemonState,
    session_id: &SessionId,
    workload_id: &WorkloadId,
    new_state: AutoContinueState,
    reason: Option<&str>,
    at_ms: u64,
) {
    let payload = AutoContinuePayload {
        session_id: session_id.clone(),
        workload_id: workload_id.clone(),
        state: new_state,
        reason: reason.map(str::to_string),
        at_ms,
    };
    // 방송 실패(직렬화 불가·연결 없음)는 무해하다 — 상태 전이 자체가 진실이다.
    if let Ok(value) = serde_json::to_value(&payload) {
        state.broadcast_control(RpcEventKind::AutoContinueChanged, value);
    }
}

/// 인터넷 확인: blocking GET 한 번. 연결 상한이 DNS+TCP+TLS을 함께 묶는다
/// (`agent_runtime/opencode/http.rs` 관례 — 감독 루프는 일반 스레드다).
fn probe_network() -> bool {
    let Ok(client) = reqwest::blocking::Client::builder()
        .connect_timeout(PROBE_TIMEOUT)
        .timeout(PROBE_TIMEOUT)
        .build()
    else {
        return false;
    };
    // 응답 내용은 읽지 않는다 — `send()`가 돌아왔으면 도달한 것이다.
    client.get(PROBE_URL).send().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_is_found_across_chunk_boundaries_via_the_rolling_tail() {
        let mut tail = Vec::new();
        let phrase = b"... API Error: Can't reach the API server (ENOTFOUND) ...";
        let split = phrase.len() / 2;
        let mut hit = false;
        for chunk in [&phrase[..split], &phrase[split..]] {
            tail.extend_from_slice(chunk);
            let overflow = tail.len().saturating_sub(TAIL_BYTES);
            if overflow > 0 {
                tail.drain(..overflow);
            }
            hit = hit || find_phrase(&tail);
        }
        assert!(hit, "두 청크로 쪼개진 문구도 테일이 이어 붙인다");
        assert!(find_phrase(PHRASE.as_bytes()));
        assert!(!find_phrase(b"Can't reach the API serve"));
        assert!(!find_phrase(b"API Error: connection error (other)"));
    }

    #[test]
    fn tail_keeps_only_the_last_window() {
        let mut tail = vec![b'a'; TAIL_BYTES];
        tail.extend_from_slice(b"xyz");
        let overflow = tail.len().saturating_sub(TAIL_BYTES);
        tail.drain(..overflow);
        assert_eq!(tail.len(), TAIL_BYTES);
        assert!(tail.ends_with(b"xyz"));
    }

    #[test]
    fn decide_table_covers_every_branch() {
        assert_eq!(
            decide(false, false, false),
            RecoveryAction::Disarm("session-gone")
        );
        assert_eq!(
            decide(true, true, false),
            RecoveryAction::Disarm("guard-suspended")
        );
        assert_eq!(
            decide(true, false, true),
            RecoveryAction::Disarm("user-input")
        );
        assert_eq!(decide(true, false, false), RecoveryAction::Fire);
        // 세션 없음이 최우선(나머지 조건을 볼 필요가 없다).
        assert_eq!(
            decide(false, true, true),
            RecoveryAction::Disarm("session-gone")
        );
    }
}
