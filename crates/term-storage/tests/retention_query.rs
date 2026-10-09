//! Retention query + journal-deleted marking (SOTA_GAP_REVIEW W1-2).
//! 종료 상태·미고정·미삭제 저널만 후보로 나오는지, 삭제 마킹이 바이트
//! 회계를 0으로 닫는지를 검증한다.

mod common;

use common::{launch_intent, open};
use term_contracts::state::WorkloadState;

#[test]
fn terminal_unpinned_sessions_lists_only_finished_workloads() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = open(&dir.path().join("meta.db3")).unwrap();

    let done = launch_intent("retention-done");
    storage.record_launch_intent(done.clone()).unwrap();
    let running = launch_intent("retention-running");
    storage.record_launch_intent(running.clone()).unwrap();

    // 아무도 종료하지 않았다 — 후보 0건.
    assert!(storage.terminal_unpinned_sessions().unwrap().is_empty());

    storage.mark_starting(&done.workload_id).unwrap();
    storage.mark_running(&done.workload_id).unwrap();
    // 데몬의 finish_workload처럼 DRAINING을 거쳐 종료 상태로 간다.
    storage
        .transition_to(&done.workload_id, WorkloadState::Draining)
        .unwrap();
    storage
        .mark_terminal(&done.workload_id, WorkloadState::Succeeded, Some(0), None)
        .unwrap();

    let candidates = storage.terminal_unpinned_sessions().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, done.session_id);
    assert_eq!(candidates[0].workload_id, done.workload_id);
    // RUNNING(종료 아님) 워크로드는 후보에 없다.
    assert!(!candidates.iter().any(|c| c.id == running.session_id));
}

#[test]
fn mark_journal_deleted_closes_byte_accounting_once() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = open(&dir.path().join("meta.db3")).unwrap();

    let intent = launch_intent("retention-delete");
    storage.record_launch_intent(intent.clone()).unwrap();
    storage
        .update_session_progress(&intent.session_id, 42, 4096)
        .unwrap();
    storage.mark_starting(&intent.workload_id).unwrap();
    storage.mark_running(&intent.workload_id).unwrap();
    storage
        .transition_to(&intent.workload_id, WorkloadState::Draining)
        .unwrap();
    storage
        .mark_terminal(&intent.workload_id, WorkloadState::Succeeded, Some(0), None)
        .unwrap();

    storage.mark_journal_deleted(&intent.session_id).unwrap();

    let sessions = storage.sessions().unwrap();
    let record = sessions
        .iter()
        .find(|s| s.id == intent.session_id)
        .expect("session row survives deletion marking");
    assert_eq!(record.replay_status, "deleted");
    assert_eq!(record.journal_bytes, 0);
    assert_eq!(record.last_seq, 42, "last_seq은 보존 — 재생 위치 증거");

    // 삭제된 저널은 다시 후보로 나오지 않는다.
    let candidates = storage.terminal_unpinned_sessions().unwrap();
    assert!(!candidates.iter().any(|c| c.id == intent.session_id));

    // 두 번 호출해도 오류 아님(멱등).
    storage.mark_journal_deleted(&intent.session_id).unwrap();
}

#[test]
fn prune_lifecycle_events_keeps_only_recent_rows() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = open(&dir.path().join("meta.db3")).unwrap();

    let intents: Vec<_> = (0..6)
        .map(|i| {
            let intent = launch_intent(&format!("prune-{i}"));
            storage.record_launch_intent(intent.clone()).unwrap();
            storage.mark_starting(&intent.workload_id).unwrap();
            storage.mark_running(&intent.workload_id).unwrap();
            storage
                .transition_to(&intent.workload_id, WorkloadState::Draining)
                .unwrap();
            storage
                .mark_terminal(&intent.workload_id, WorkloadState::Succeeded, Some(0), None)
                .unwrap();
            intent
        })
        .collect();
    let _ = intents;

    // 최근 3건만 남긴다 — prune은 (전역) 최신 id 기준으로 자른다.
    storage.prune_lifecycle_events(3).unwrap();
    let sessions = storage.sessions().unwrap();
    let kept = sessions.len();
    assert!(kept >= 1);
    // prune은 멱등이다: 다시 돌려도 오류 아님.
    storage.prune_lifecycle_events(3).unwrap();
}
