//! Duplicate request handling (spec §6): same id + same fingerprint returns
//! the existing workload's current state and writes nothing; same id +
//! different fingerprint is `REQUEST_CONFLICT`.

mod common;

use common::{fingerprint_for, launch_intent, open, raw_conn};
use term_contracts::state::WorkloadState;
use term_storage::{LaunchIntentOutcome, RequestOutcome, RequestResolution, StorageError};

#[test]
fn same_fingerprint_replay_returns_existing_state_and_writes_nothing() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("dup");
    let created = storage.record_launch_intent(intent.clone()).unwrap();
    assert!(matches!(
        created,
        LaunchIntentOutcome::Created {
            state: WorkloadState::Queued,
            ..
        }
    ));

    // Replay after a state change: duplicate response carries the *current*
    // workload state (spec §4).
    storage.mark_starting(&intent.workload_id).unwrap();
    let replay = storage.record_launch_intent(intent.clone()).unwrap();
    match replay {
        LaunchIntentOutcome::Existing {
            outcome,
            workload_id,
            state,
        } => {
            assert_eq!(outcome, RequestOutcome::Accepted);
            assert_eq!(workload_id, intent.workload_id);
            assert_eq!(state, WorkloadState::Starting);
        }
        other => panic!("expected Existing, got {other:?}"),
    }

    // No second row anywhere.
    let conn = raw_conn(&path);
    let workloads: i64 = conn
        .query_row("SELECT COUNT(*) FROM workloads", [], |r| r.get(0))
        .unwrap();
    let requests: i64 = conn
        .query_row("SELECT COUNT(*) FROM requests", [], |r| r.get(0))
        .unwrap();
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    let lifecycle: i64 = conn
        .query_row("SELECT COUNT(*) FROM lifecycle_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(workloads, 1);
    assert_eq!(requests, 1);
    assert_eq!(sessions, 1);
    assert_eq!(lifecycle, 2, "one QUEUED event + one STARTING event");
}

#[test]
fn different_fingerprint_is_request_conflict_and_keeps_the_original() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("conflict");
    storage.record_launch_intent(intent.clone()).unwrap();

    let mut other = launch_intent("conflict");
    other.request_id = intent.request_id.clone();
    other.fingerprint = fingerprint_for("different");

    let err = storage.record_launch_intent(other).unwrap_err();
    assert!(
        matches!(err, StorageError::RequestConflict { .. }),
        "{err:?}"
    );

    let conn = raw_conn(&path);
    let workloads: i64 = conn
        .query_row("SELECT COUNT(*) FROM workloads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(workloads, 1, "no second workload row after conflict");
    let stored_fp: String = conn
        .query_row("SELECT fingerprint FROM requests", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored_fp, intent.fingerprint, "original fingerprint kept");
}

#[test]
fn resolve_request_reports_new_existing_and_conflict() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("resolve");
    assert_eq!(
        storage
            .resolve_request(&intent.request_id, &intent.fingerprint)
            .unwrap(),
        RequestResolution::New
    );

    storage.record_launch_intent(intent.clone()).unwrap();
    assert_eq!(
        storage
            .resolve_request(&intent.request_id, &intent.fingerprint)
            .unwrap(),
        RequestResolution::Existing {
            outcome: RequestOutcome::Accepted,
            workload_id: intent.workload_id.clone(),
            state: WorkloadState::Queued,
        }
    );

    assert_eq!(
        storage
            .resolve_request(&intent.request_id, &fingerprint_for("not-the-same"))
            .unwrap(),
        RequestResolution::Conflict
    );

    // Outcome upgrades after the workload finishes.
    storage.mark_starting(&intent.workload_id).unwrap();
    storage.mark_running(&intent.workload_id).unwrap();
    storage
        .transition_to(&intent.workload_id, WorkloadState::Draining)
        .unwrap();
    storage
        .mark_terminal(
            &intent.workload_id,
            WorkloadState::Failed,
            Some(3),
            Some("SPAWN_FAILED".into()),
        )
        .unwrap();
    assert_eq!(
        storage
            .resolve_request(&intent.request_id, &intent.fingerprint)
            .unwrap(),
        RequestResolution::Existing {
            outcome: RequestOutcome::Failed,
            workload_id: intent.workload_id.clone(),
            state: WorkloadState::Failed,
        }
    );
}
