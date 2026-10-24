//! 예약 명령 루프: 사용자가 맡겨 둔 one-shot PTY 입력을 만료 시각에
//! 세션에 쓴다. 데몬이 예약표를 소유하므로 창을 닫아도 살아 있고,
//! `session.input`의 view/epoch 검증과 무관하게 actor에 직접 쓴다
//! (dispatch `session_input_finish`가 actor를 찾는 같은 경로 —
//! 스케줄러는 데몬 수준 행위자다).
//!
//! 상태 전이는 스토리지가 원자적으로 지킨다(클레임 `pending→firing`,
//! 취소는 pending만). 이 루프는 클레임한 행을 반드시 종착점이나 재시도로
//! 마무리하고 다음 틱을 돈다.

use std::sync::Arc;
use std::time::Duration;

use term_contracts::ids::{SessionId, WorkloadId};
use term_contracts::rpc::RpcEventKind;
use term_contracts::schedule::limits;
use term_contracts::schedule::{ScheduleOutcome, ScheduledCommand, ScheduledCommandStatus};
use term_pty::actor::{ActorError, SessionActorHandle};
use term_pty::input::InputQueueError;

use crate::state::DaemonState;

/// 만료 확인 주기. 사람이 체감하는 예약 정확도(1초 미만 오차)에 충분하고
/// 유휴 틱은 락 없는 스토리지 조회 한 번으로 끝난다.
const TICK: Duration = Duration::from_millis(500);
/// PTY 쓰기 완료 대기 — `session.input`와 같은 상한(W2, ADR-5).
const WRITE_TIMEOUT: Duration = Duration::from_millis(750);

/// 벽시계 unix epoch ms. 예약 시각은 사용자가 고른 "몇 시" 그 자체라
/// 단조 시계(`DaemonState::now_ms`, 기동 후 경과)가 아니라 벽시계로 다룬다
/// (03-resources §2의 "샘플·순서는 단조" 원칙은 어기지 않는다 — 여기는
/// 순서가 아니라 달력이다). 시스템 시각이 바뀌면 만료 판정도 함께 움직인다.
pub(crate) fn wall_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("wall clock after unix epoch")
        .as_millis() as u64
}

/// Supervised loop body (`supervisor::spawn_supervised`).
pub fn run(state: Arc<DaemonState>) {
    // 종료 신호 수신기는 루프 밖에서 한 번만 만든다(재시작마다 새로 만든다).
    let shutdown = state.shutdown.subscribe();
    // 부팅 스윕: 데몬이 시작되기 전에 만들어진 pending/firing은 세션과
    // 함께 죽었다(재시작은 PTY를 죽인다). 결과 불명인 자동 재실행 대신
    // 정직하게 skip 처리한다.
    let boot_ms = wall_now_ms();
    if let Err(error) = state
        .storage
        .skip_stale_scheduled_commands(boot_ms, wall_now_ms())
    {
        tracing::warn!(%error, "scheduled-command boot sweep failed");
    }
    while !*shutdown.borrow() {
        tick(&state);
        std::thread::sleep(TICK);
    }
}

fn tick(state: &Arc<DaemonState>) {
    let claimed = match state.storage.claim_due_scheduled_commands(wall_now_ms()) {
        Ok(claimed) => claimed,
        Err(error) => {
            tracing::warn!(%error, "scheduled-command claim failed");
            return;
        }
    };
    for record in claimed {
        fire_one(Arc::clone(state), record);
    }
}

/// 클레임한 한 건을 종착시킨다. 세션 해석 → 가드 확인 → actor 경로 쓰기.
fn fire_one(state: Arc<DaemonState>, record: ScheduledCommand) {
    let Some(session) = state.session(&record.session_id) else {
        return finish(
            &state,
            &record,
            ScheduledCommandStatus::Skipped,
            Some("session-gone"),
        );
    };
    // 08 §5: 가드로 정지 중인 세션에는 쓰지 않는다. 정지된 자식은 PTY
    // 슬레이브를 읽지 않아 tty 큐가 차면 writer가 막히므로, dispatch의
    // session.input 거부와 같은 근거로 건너뛴다.
    if state.guard_view(&session.workload_id).0.is_suspended() {
        return finish(
            &state,
            &record,
            ScheduledCommandStatus::Skipped,
            Some("guard-suspended"),
        );
    }
    let Some(actor) = actor_for(&state, &session.workload_id) else {
        return finish(
            &state,
            &record,
            ScheduledCommandStatus::Skipped,
            Some("session-gone"),
        );
    };
    // 시도마다 새 input_id: actor의 멱등 dedup이 이전 시도를 재생하면
    // 안 되기 때문이다(실패한 시도는 dedup 자리를 돌려놓지만, 타임아웃된
    // 시도는 완료될 수 있어 재생 위험이 있다).
    let input_id = format!("sched-{}-a{}", record.schedule_id, record.attempts);
    let data = command_bytes(&record.command);
    match actor.write_input_await(&input_id, &data, WRITE_TIMEOUT) {
        // Written·Queued·Replayed 모두 "입력이 받아들여졌다"다. Queued는
        // 상한 안에 쓰기가 끝나지 않은 것(나중에 완료됨)이지 실패가
        // 아니고, Replayed는 같은 시도가 이미 반영된 것이다 — 어느 쪽도
        // 재전송하지 않는다(session.input와 같은 계약).
        Ok(_) => {
            crate::model_watch::note_input(state.now_ms(), &session.workload_id);
            finish(&state, &record, ScheduledCommandStatus::Fired, None);
        }
        Err(error) => match classify_write_error(&error) {
            FailureKind::Retryable => retry_or_fail(&state, &record),
            FailureKind::Fatal => finish(
                &state,
                &record,
                ScheduledCommandStatus::Failed,
                Some("write-failed"),
            ),
        },
    }
}

/// 세션의 actor 핸들 — `WorkloadEntry.actor`에서 가져온다(dispatch의
/// session.input와 같은 경로). 세션이 살아 있어도 actor가 이미 정리됐으면
/// 없는 것과 같다. auto_continue 모듈도 같은 경로를 쓴다.
pub(crate) fn actor_for(
    state: &DaemonState,
    workload_id: &WorkloadId,
) -> Option<Arc<SessionActorHandle>> {
    state.workload_entry(workload_id).and_then(|entry| {
        entry
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .actor
            .clone()
    })
}

/// 쓰기 실패의 재시도 가능성 분류. 재시도 대상은 "아무것도 writer에 닿지
/// 않은" 실패뿐이다 — 큐가 꽉 찼거나(try_enqueue 거부) PTY가 너무 오래
/// 멈춰 있어 새 입력을 거부당한 경우.
fn classify_write_error(error: &ActorError) -> FailureKind {
    match error {
        ActorError::Input(InputQueueError::QueueFull { .. }) => FailureKind::Retryable,
        ActorError::InputStalled { .. } => FailureKind::Retryable,
        _ => FailureKind::Fatal,
    }
}

enum FailureKind {
    Retryable,
    Fatal,
}

/// 재시도: 시도 상한 안이면 2초 뒤에 다시 만료시키고, 못 하면 실패 확정.
/// 재시도 중에도 행은 pending이므로 사용자가 취소할 수 있다.
fn retry_or_fail(state: &DaemonState, record: &ScheduledCommand) {
    if record.attempts < limits::MAX_ATTEMPTS {
        let next = wall_now_ms() + limits::RETRY_DELAY_MS;
        if let Err(error) = state
            .storage
            .reschedule_scheduled_attempt(&record.schedule_id, next)
        {
            tracing::warn!(%error, "scheduled-command reschedule failed");
        }
        return;
    }
    finish(
        state,
        record,
        ScheduledCommandStatus::Failed,
        Some("write-failed"),
    );
}

/// 행을 종착시키고 `schedule.fired` 이벤트를 흘린다. 종착 기록이 동시에
/// 건너뛰어진 행(세션 종료 훅과의 극히 좁은 경합)을 만나면 조용히 끝난다 —
/// 그 세션은 이미 없고 이벤트는 참고용이다.
fn finish(
    state: &DaemonState,
    record: &ScheduledCommand,
    status: ScheduledCommandStatus,
    reason_code: Option<&str>,
) {
    let now = wall_now_ms();
    if let Err(error) =
        state
            .storage
            .finish_scheduled_command(&record.schedule_id, status, now, reason_code)
    {
        tracing::warn!(%error, "scheduled-command finish failed");
    }
    broadcast_fired(state, record, status, reason_code, now);
}

/// 종착 결과를 `schedule.fired`로 방송한다(종착 기록은 이미 돼 있다).
fn broadcast_fired(
    state: &DaemonState,
    record: &ScheduledCommand,
    status: ScheduledCommandStatus,
    reason_code: Option<&str>,
    at_ms: u64,
) {
    let outcome = match status {
        ScheduledCommandStatus::Fired => ScheduleOutcome::Fired,
        ScheduledCommandStatus::Failed => ScheduleOutcome::Failed,
        ScheduledCommandStatus::Skipped => ScheduleOutcome::Skipped,
        _ => return,
    };
    state.broadcast_control(
        RpcEventKind::ScheduleFired,
        serde_json::json!({
            "schedule_id": record.schedule_id,
            "session_id": record.session_id,
            "outcome": outcome,
            "reason_code": reason_code,
            "command": record.command,
            "fired_at_ms": at_ms,
        }),
    );
}

/// 세션 종료 훅(orchestrator::finalize_session): 그 세션의 pending을
/// 건너뛰고 각 행을 `schedule.fired`로 방송한다. 방송 없이 조용히 두면
/// 목록이 다음 만료 시각까지 pending인 채로 남는다.
pub fn skip_pending_for_session(state: &DaemonState, session_id: &SessionId) {
    let now = wall_now_ms();
    let skipped =
        match state
            .storage
            .skip_pending_schedules_for_session(session_id, now, "session-exited")
        {
            Ok(skipped) => skipped,
            Err(error) => {
                tracing::warn!(%error, "schedule skip on session exit failed");
                return;
            }
        };
    for record in skipped {
        broadcast_fired(
            state,
            &record,
            ScheduledCommandStatus::Skipped,
            Some("session-exited"),
            now,
        );
    }
}

/// PTY에 보낼 최종 바이트: 줄바꿈은 `\r`로 바꾸고 실행 Enter를 보장한다
/// (터미널의 Enter == CR). `preparePaste`의 `\n`→`\r` 변환과 같은 규칙.
/// auto_continue 모듈도 같은 변환을 쓴다.
pub(crate) fn command_bytes(command: &str) -> Vec<u8> {
    let mut data: Vec<u8> = command
        .replace("\r\n", "\r")
        .replace('\n', "\r")
        .into_bytes();
    if data.last() != Some(&b'\r') {
        data.push(b'\r');
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_bytes_normalizes_newlines_and_guarantees_enter() {
        assert_eq!(command_bytes("ls -la"), b"ls -la\r".to_vec());
        assert_eq!(command_bytes("echo hi\n"), b"echo hi\r".to_vec());
        // 이미 Enter로 끝나면 하나만 둔다.
        assert_eq!(command_bytes("echo hi\r"), b"echo hi\r".to_vec());
        // CRLF·LF 혼합은 모두 CR로.
        assert_eq!(command_bytes("a\r\nb\nc"), b"a\rb\rc\r".to_vec());
    }

    #[test]
    fn command_bytes_stays_under_the_input_chunk_cap() {
        let longest = "x".repeat(limits::COMMAND_MAX_BYTES);
        assert!(command_bytes(&longest).len() <= 4_096);
    }

    #[test]
    fn queue_full_and_stalled_are_retryable_everything_else_is_fatal() {
        assert!(matches!(
            classify_write_error(&ActorError::Input(InputQueueError::QueueFull {
                used: 1,
                max: 2
            })),
            FailureKind::Retryable
        ));
        assert!(matches!(
            classify_write_error(&ActorError::InputStalled { blocked_ms: 1 }),
            FailureKind::Retryable
        ));
        // 종료 중인 큐 등 다른 입력 오류는 재전송하지 않는다.
        assert!(matches!(
            classify_write_error(&ActorError::Input(InputQueueError::ShuttingDown)),
            FailureKind::Fatal
        ));
    }
}
