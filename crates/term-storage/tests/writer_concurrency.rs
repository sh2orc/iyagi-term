//! Single-writer serialization: 16 threads x 50 launch intents through one
//! shared Storage all land; the database stays in WAL mode.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use common::{launch_intent, open, raw_conn};
use term_contracts::launch::Priority;
use term_contracts::state::WorkloadState;
use term_storage::LaunchIntentOutcome;

const THREADS: usize = 16;
const PER_THREAD: usize = 50;

#[test]
fn concurrent_intents_all_land_through_the_single_writer() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = Arc::new(open(&path).unwrap());

    let counter = AtomicU64::new(0);
    let failures: Vec<String> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for thread_index in 0..THREADS {
            let storage = Arc::clone(&storage);
            let counter = &counter;
            handles.push(scope.spawn(move || {
                let mut failures = Vec::new();
                for _ in 0..PER_THREAD {
                    let n = counter.fetch_add(1, Ordering::SeqCst);
                    let tag = format!("t{thread_index}-n{n}");
                    let mut intent = launch_intent(&tag);
                    // Spread priorities to exercise the queue index.
                    intent.priority = Priority((n % 3) as u8);
                    match storage.record_launch_intent(intent) {
                        Ok(LaunchIntentOutcome::Created { state, .. }) => {
                            assert_eq!(state, WorkloadState::Queued);
                        }
                        Ok(other) => {
                            failures.push(format!("{tag}: unexpected outcome {other:?}"));
                        }
                        Err(e) => failures.push(format!("{tag}: {e}")),
                    }
                }
                failures
            }));
        }
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("worker thread"))
            .collect()
    });
    assert!(failures.is_empty(), "failures: {failures:?}");

    // WAL mode is asserted via PRAGMA on a read connection.
    assert_eq!(storage.journal_mode().unwrap(), "wal");

    // Every intent landed exactly once.
    let conn = raw_conn(&path);
    for table in ["workloads", "sessions", "tasks", "attempts", "requests"] {
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n as usize, THREADS * PER_THREAD, "{table} row count");
    }
    let events: i64 = conn
        .query_row("SELECT COUNT(*) FROM lifecycle_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(events as usize, THREADS * PER_THREAD);

    // Reads stay consistent: queue snapshot and active workloads agree.
    let queue = storage.queue_snapshot().unwrap();
    assert_eq!(queue.len(), THREADS * PER_THREAD);
    let active = storage.active_workloads().unwrap();
    assert_eq!(active.len(), THREADS * PER_THREAD);
    assert!(active.iter().all(|w| w.state == WorkloadState::Queued));

    // Queue ordering: priority ascending; ties fall to created_at then id.
    let mut ordered = queue.clone();
    ordered.sort_by(|a, b| {
        a.priority
            .0
            .cmp(&b.priority.0)
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.workload_id.as_str().cmp(b.workload_id.as_str()))
    });
    assert_eq!(
        queue, ordered,
        "queue_snapshot must be priority->created_at->id ordered"
    );
}

#[test]
fn queue_snapshot_orders_priority_then_created_at_then_id() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    // Insert in reverse priority order; snapshot must come back 0-first.
    for (i, priority) in [
        (0, Priority(2)),
        (1, Priority(0)),
        (2, Priority(1)),
        (3, Priority(0)),
    ] {
        let mut intent = launch_intent(&format!("order-{i}"));
        intent.priority = priority;
        storage.record_launch_intent(intent).unwrap();
    }

    let queue = storage.queue_snapshot().unwrap();
    let priorities: Vec<u8> = queue.iter().map(|q| q.priority.0).collect();
    assert_eq!(priorities, vec![0, 0, 1, 2]);

    // Entries carry their request ids.
    assert!(queue.iter().all(|q| q.request_id.is_some()));
}
