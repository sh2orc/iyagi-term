//! `scheduled_commands`(migration 0007) 저장 규칙: 클레임의 원자성과
//! attempts 증가, 취소는 pending만, 재시도 되돌리기, 세션 종료 스킵,
//! 부팅 스윕, 목록 정렬.

mod common;

use common::open;
use term_contracts::ids::{ScheduleId, SessionId};
use term_contracts::schedule::{ScheduledCommand, ScheduledCommandStatus};

fn pending(session_id: &SessionId, command: &str, fire_at_ms: u64) -> ScheduledCommand {
    ScheduledCommand {
        schedule_id: ScheduleId::generate(),
        session_id: session_id.clone(),
        command: command.to_string(),
        fire_at_ms,
        status: ScheduledCommandStatus::Pending,
        attempts: 0,
        created_at_ms: 1_000,
        fired_at_ms: None,
        reason_code: None,
    }
}

#[test]
fn claim_is_due_scoped_atomic_and_counts_attempts() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = open(&dir.path().join("schedules.db3")).unwrap();
    let session = SessionId::generate();
    let due = pending(&session, "echo due", 2_000);
    let later = pending(&session, "echo later", 60_000);
    storage.insert_scheduled_command(due.clone()).unwrap();
    storage.insert_scheduled_command(later).unwrap();

    // 만료 안 된 행은 클레임하지 않는다.
    assert!(storage
        .claim_due_scheduled_commands(1_000)
        .unwrap()
        .is_empty());
    // 만료된 행만 클레임되고 attempts가 1이 된다.
    let claimed = storage.claim_due_scheduled_commands(2_000).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].schedule_id, due.schedule_id);
    assert_eq!(claimed[0].status, ScheduledCommandStatus::Firing);
    assert_eq!(claimed[0].attempts, 1);
    // 같은 행을 두 번 클레임할 수 없다(firing은 재클레인 대상이 아니다).
    assert!(storage
        .claim_due_scheduled_commands(3_000)
        .unwrap()
        .is_empty());

    storage
        .finish_scheduled_command(&due.schedule_id, ScheduledCommandStatus::Fired, 2_500, None)
        .unwrap();
    let items = storage.list_scheduled_commands(None, 10).unwrap();
    let fired = items
        .iter()
        .find(|r| r.schedule_id == due.schedule_id)
        .unwrap();
    assert_eq!(fired.status, ScheduledCommandStatus::Fired);
    assert_eq!(fired.fired_at_ms, Some(2_500));
    assert_eq!(fired.attempts, 1);
}

#[test]
fn cancel_only_touches_pending_rows() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = open(&dir.path().join("schedules.db3")).unwrap();
    let session = SessionId::generate();
    let a = pending(&session, "echo a", 2_000);
    let b = pending(&session, "echo b", 3_000);
    storage.insert_scheduled_command(a.clone()).unwrap();
    storage.insert_scheduled_command(b.clone()).unwrap();

    assert!(storage
        .cancel_scheduled_command(&a.schedule_id, 1_500)
        .unwrap());
    // 이미 취소한 행은 다시 취소할 수 없다.
    assert!(!storage
        .cancel_scheduled_command(&a.schedule_id, 1_600)
        .unwrap());
    // 클레임(firing)된 행도 취소할 수 없다 — 발사와 취소가 경합하지 않는다.
    let claimed = storage.claim_due_scheduled_commands(3_000).unwrap();
    assert_eq!(claimed.len(), 1);
    assert!(!storage
        .cancel_scheduled_command(&b.schedule_id, 3_100)
        .unwrap());

    let items = storage.list_scheduled_commands(None, 10).unwrap();
    let a_row = items
        .iter()
        .find(|r| r.schedule_id == a.schedule_id)
        .unwrap();
    assert_eq!(a_row.status, ScheduledCommandStatus::Cancelled);
    assert_eq!(a_row.fired_at_ms, Some(1_500));
}

#[test]
fn reschedule_returns_the_claim_to_pending_with_a_new_fire_time() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = open(&dir.path().join("schedules.db3")).unwrap();
    let session = SessionId::generate();
    let row = pending(&session, "echo retry", 2_000);
    storage.insert_scheduled_command(row.clone()).unwrap();

    storage.claim_due_scheduled_commands(2_000).unwrap();
    storage
        .reschedule_scheduled_attempt(&row.schedule_id, 4_000)
        .unwrap();
    // pending으로 돌아왔으므로 취소가 다시 가능하다(재시도 대기 중 취소).
    assert!(storage
        .cancel_scheduled_command(&row.schedule_id, 2_100)
        .unwrap());
}

#[test]
fn session_exit_and_boot_sweeps_skip_pending_rows_with_reasons() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = open(&dir.path().join("schedules.db3")).unwrap();
    let live = SessionId::generate();
    let dead = SessionId::generate();
    let old = pending(&dead, "echo old", 2_000);
    let mut old_row = old.clone();
    old_row.created_at_ms = 500; // 이전 데몬 시대의 행
    let fresh = pending(&live, "echo fresh", 2_000);
    storage.insert_scheduled_command(old_row).unwrap();
    storage.insert_scheduled_command(fresh.clone()).unwrap();

    // 부팅 스윕: 데몬 시작(=1_000) 이전에 만들어진 행만 건너뛴다.
    let swept = storage.skip_stale_scheduled_commands(1_000, 1_200).unwrap();
    assert_eq!(swept, 1);
    let items = storage.list_scheduled_commands(None, 10).unwrap();
    let old_item = items
        .iter()
        .find(|r| r.schedule_id == old.schedule_id)
        .unwrap();
    assert_eq!(old_item.status, ScheduledCommandStatus::Skipped);
    assert_eq!(old_item.reason_code.as_deref(), Some("daemon-restart"));
    assert!(
        items
            .iter()
            .any(|r| r.schedule_id == fresh.schedule_id
                && r.status == ScheduledCommandStatus::Pending)
    );

    // 세션 종료 훅: 그 세션의 pending만 건너뛰고 건너뛴 행을 돌려준다.
    let skipped = storage
        .skip_pending_schedules_for_session(&live, 1_500, "session-exited")
        .unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].session_id, live);
    assert_eq!(skipped[0].reason_code.as_deref(), Some("session-exited"));
}

#[test]
fn list_filters_by_session_and_orders_newest_first() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = open(&dir.path().join("schedules.db3")).unwrap();
    let a = SessionId::generate();
    let b = SessionId::generate();
    storage
        .insert_scheduled_command(pending(&a, "echo 1", 5_000))
        .unwrap();
    storage
        .insert_scheduled_command(pending(&b, "echo 2", 9_000))
        .unwrap();
    storage
        .insert_scheduled_command(pending(&a, "echo 3", 7_000))
        .unwrap();

    let all = storage.list_scheduled_commands(None, 10).unwrap();
    assert_eq!(
        all.iter().map(|r| r.fire_at_ms).collect::<Vec<_>>(),
        vec![9_000, 7_000, 5_000]
    );
    let only_a = storage.list_scheduled_commands(Some(&a), 10).unwrap();
    assert_eq!(only_a.len(), 2);
    assert!(only_a.iter().all(|r| r.session_id == a));
}
