//! Transition legality enforced through the storage API (spec §5): illegal
//! edges are rejected with INVALID_STATE semantics, nothing is written, and
//! every legal move appends a lifecycle event.

mod common;

use common::{launch_intent, open, raw_conn};
use term_contracts::state::WorkloadState;
use term_storage::StorageError;

#[test]
fn queued_cannot_jump_directly_to_running() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("illegal");
    storage.record_launch_intent(intent.clone()).unwrap();

    let err = storage.mark_running(&intent.workload_id).unwrap_err();
    match &err {
        StorageError::InvalidState { from, to, .. } => {
            assert_eq!(*from, WorkloadState::Queued);
            assert_eq!(*to, WorkloadState::Running);
        }
        other => panic!("expected InvalidState, got {other:?}"),
    }

    // Nothing changed and no event was appended.
    let record = storage
        .workload_record(&intent.workload_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.state, WorkloadState::Queued);
    let conn = raw_conn(&path);
    let events: i64 = conn
        .query_row("SELECT COUNT(*) FROM lifecycle_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(events, 1, "only the initial QUEUED event");
}

#[test]
fn happy_path_chain_reaches_succeeded_with_lifecycle_events() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("happy");
    storage.record_launch_intent(intent.clone()).unwrap();
    storage.mark_starting(&intent.workload_id).unwrap();
    storage.mark_running(&intent.workload_id).unwrap();
    storage
        .transition_to(&intent.workload_id, WorkloadState::Draining)
        .unwrap();
    storage
        .mark_terminal(&intent.workload_id, WorkloadState::Succeeded, Some(0), None)
        .unwrap();

    let record = storage
        .workload_record(&intent.workload_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.state, WorkloadState::Succeeded);
    assert_eq!(record.exit_code, Some(0));

    let conn = raw_conn(&path);
    let chain: Vec<(Option<String>, String)> = conn
        .prepare("SELECT from_state, to_state FROM lifecycle_events ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        chain,
        vec![
            (None, "QUEUED".to_string()),
            (Some("QUEUED".to_string()), "STARTING".to_string()),
            (Some("STARTING".to_string()), "RUNNING".to_string()),
            (Some("RUNNING".to_string()), "DRAINING".to_string()),
            (Some("DRAINING".to_string()), "SUCCEEDED".to_string()),
        ]
    );
    let started_at: String = conn
        .query_row(
            "SELECT started_at FROM workloads WHERE id = ?1",
            [&intent.workload_id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        started_at.starts_with("20"),
        "started_at stamped: {started_at}"
    );
    let finished_at: String = conn
        .query_row(
            "SELECT finished_at FROM workloads WHERE id = ?1",
            [&intent.workload_id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        finished_at.starts_with("20"),
        "finished_at stamped: {finished_at}"
    );
}

#[test]
fn terminal_states_never_leave() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("terminal");
    storage.record_launch_intent(intent.clone()).unwrap();
    // QUEUED -> CANCELLED is the legal cancel edge from the queue.
    storage
        .mark_terminal(&intent.workload_id, WorkloadState::Cancelled, None, None)
        .unwrap();

    for illegal in [
        WorkloadState::Queued,
        WorkloadState::Starting,
        WorkloadState::Running,
        WorkloadState::Stopping,
        WorkloadState::Draining,
    ] {
        let err = storage
            .transition_to(&intent.workload_id, illegal)
            .unwrap_err();
        assert!(
            matches!(err, StorageError::InvalidState { .. }),
            "CANCELLED -> {illegal:?} must be InvalidState"
        );
    }
    for illegal in [
        WorkloadState::Succeeded,
        WorkloadState::Failed,
        WorkloadState::Interrupted,
    ] {
        let err = storage
            .mark_terminal(&intent.workload_id, illegal, None, None)
            .unwrap_err();
        assert!(
            matches!(err, StorageError::InvalidState { .. }),
            "CANCELLED -> {illegal:?} must be InvalidState"
        );
    }
    // Non-terminal helper refuses terminal targets outright.
    assert!(matches!(
        storage
            .transition_to(&intent.workload_id, WorkloadState::Succeeded)
            .unwrap_err(),
        StorageError::InvalidArgument(_)
    ));
    // mark_terminal refuses non-terminal targets.
    assert!(matches!(
        storage
            .mark_terminal(&intent.workload_id, WorkloadState::Running, None, None)
            .unwrap_err(),
        StorageError::InvalidArgument(_)
    ));
}

#[test]
fn stopping_path_records_reason_and_exit_code() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("stop");
    storage.record_launch_intent(intent.clone()).unwrap();
    storage.mark_starting(&intent.workload_id).unwrap();
    storage.mark_running(&intent.workload_id).unwrap();
    storage
        .set_cancel_requested(&intent.workload_id, true)
        .unwrap();
    storage
        .transition_to(&intent.workload_id, WorkloadState::Stopping)
        .unwrap();
    storage
        .transition_to(&intent.workload_id, WorkloadState::Draining)
        .unwrap();
    storage
        .mark_terminal(
            &intent.workload_id,
            WorkloadState::Cancelled,
            Some(9),
            Some("USER_CANCEL".into()),
        )
        .unwrap();

    let record = storage
        .workload_record(&intent.workload_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.state, WorkloadState::Cancelled);
    assert!(record.cancel_requested);
    assert_eq!(record.exit_code, Some(9));
    assert_eq!(record.last_error_code.as_deref(), Some("USER_CANCEL"));

    // starting->failed keeps exit code too (helper spawn failure path).
    let failed = launch_intent("spawn-fail");
    storage.record_launch_intent(failed.clone()).unwrap();
    storage.mark_starting(&failed.workload_id).unwrap();
    storage
        .mark_terminal(
            &failed.workload_id,
            WorkloadState::Failed,
            Some(127),
            Some("SPAWN_FAILED".into()),
        )
        .unwrap();
    let record = storage
        .workload_record(&failed.workload_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.state, WorkloadState::Failed);
    assert_eq!(record.exit_code, Some(127));
}

#[test]
fn unknown_workload_is_a_not_found_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let missing = launch_intent("ghost").workload_id;
    for result in [
        storage.mark_starting(&missing),
        storage.set_root_exited(&missing, true),
        storage.set_queue_reason(&missing, None),
    ] {
        assert!(matches!(
            result.unwrap_err(),
            StorageError::WorkloadNotFound { .. }
        ));
    }
    assert_eq!(storage.workload_record(&missing).unwrap(), None);
}

#[test]
fn set_queue_reason_persists_contract_strings() {
    use term_contracts::snapshot::QueueReason;

    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("queue-reason");
    storage.record_launch_intent(intent.clone()).unwrap();

    storage
        .set_queue_reason(
            &intent.workload_id,
            Some(QueueReason::WaitReservationBudget),
        )
        .unwrap();
    let conn = raw_conn(&path);
    let stored: String = conn
        .query_row(
            "SELECT queue_reason FROM workloads WHERE id = ?1",
            [&intent.workload_id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, "WAIT_RESERVATION_BUDGET");

    let snapshot = storage.queue_snapshot().unwrap();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(
        snapshot[0].queue_reason,
        Some(QueueReason::WaitReservationBudget)
    );

    storage.set_queue_reason(&intent.workload_id, None).unwrap();
    let snapshot = storage.queue_snapshot().unwrap();
    assert_eq!(snapshot[0].queue_reason, None);
}
