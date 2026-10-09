//! Crash reconciliation (spec §6): reopening the database on the same file
//! simulates the daemon dying between intent and terminal state. Every
//! non-terminal workload becomes INTERRUPTED exactly once; a second open
//! finds only terminal states and reports nothing.

mod common;

use common::{launch_intent, open, raw_conn};
use term_contracts::ids::{ProcessIdentity, SessionId, WorkloadId};
use term_contracts::metrics::UsageCoverage;
use term_contracts::state::WorkloadState;
use term_contracts::workload::{GroupKind, ProcessOwnership};
use term_storage::LaunchIntentOutcome;

fn ownership_for(workload_id: &WorkloadId) -> ProcessOwnership {
    ProcessOwnership {
        workload_id: workload_id.clone(),
        identity: ProcessIdentity {
            pid: 4242,
            start_token: "171000000000000000".into(),
            boot_id: "boot-1".into(),
        },
        group_kind: GroupKind::Job,
        group_reference: Some("job-object-7".into()),
        coverage: UsageCoverage::Group,
    }
}

#[test]
fn reopen_marks_nonterminal_workloads_interrupted_exactly_once() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");

    // First process: one workload left RUNNING with ownership recorded, one
    // left QUEUED, one already terminal (SUCCEEDED must stay untouched).
    let (running, queued, finished) = {
        let storage = open(&path).unwrap();
        let running = launch_intent("crash-running");
        let queued = launch_intent("crash-queued");
        let finished = launch_intent("crash-finished");
        for intent in [&running, &queued, &finished] {
            storage
                .record_launch_intent(intent.clone())
                .unwrap_or_else(|e| panic!("{e}"));
        }
        // `queued` stays QUEUED; `running` gets to RUNNING with ownership;
        // `finished` completes the full happy path before the "crash".
        storage.mark_starting(&running.workload_id).unwrap();
        storage.mark_running(&running.workload_id).unwrap();
        storage
            .save_group_identity(ownership_for(&running.workload_id))
            .unwrap();
        storage.mark_starting(&finished.workload_id).unwrap();
        storage.mark_running(&finished.workload_id).unwrap();
        storage
            .transition_to(&finished.workload_id, WorkloadState::Draining)
            .unwrap();
        storage
            .mark_terminal(
                &finished.workload_id,
                WorkloadState::Succeeded,
                Some(0),
                None,
            )
            .unwrap();
        (running, queued, finished)
        // Storage dropped here: writer drains and the connection closes,
        // leaving durable rows behind exactly like a killed process.
    };

    // Second open reconciles.
    let storage = open(&path).unwrap();
    let report = storage.take_reconciliation_report().expect("report");
    assert_eq!(report.len(), 2);

    let by_id = |id: &WorkloadId| report.iter().find(|r| r.workload_id == *id);
    let running_report = by_id(&running.workload_id).expect("running reconciled");
    assert_eq!(running_report.previous_state, WorkloadState::Running);
    let ownership = running_report
        .ownership
        .as_ref()
        .expect("ownership survives");
    assert_eq!(ownership.identity.pid, 4242);
    assert_eq!(ownership.group_kind, GroupKind::Job);
    assert_eq!(ownership.group_reference.as_deref(), Some("job-object-7"));

    let queued_report = by_id(&queued.workload_id).expect("queued reconciled");
    assert_eq!(queued_report.previous_state, WorkloadState::Queued);
    assert!(queued_report.ownership.is_none());

    assert!(by_id(&finished.workload_id).is_none(), "terminal stays put");

    // Every reconciled workload is now terminal with a DAEMON_RESTART event
    // and its request outcome flipped to unknown.
    for report in &report {
        let record = storage
            .workload_record(&report.workload_id)
            .unwrap()
            .unwrap();
        assert_eq!(record.state, WorkloadState::Interrupted);
        assert_eq!(record.last_error_code.as_deref(), Some("DAEMON_RESTART"));
    }

    let conn = raw_conn(&path);
    let interrupted_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_events WHERE to_state = 'INTERRUPTED' AND reason_code = 'DAEMON_RESTART'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(interrupted_events, 2);
    let unknown: String = conn
        .query_row(
            "SELECT outcome FROM requests WHERE workload_id = ?1",
            [&running.workload_id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unknown, "unknown");
    let succeeded_outcome: String = conn
        .query_row(
            "SELECT outcome FROM requests WHERE workload_id = ?1",
            [&finished.workload_id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(succeeded_outcome, "completed");

    // process_ownership row kept for reconciliation_required surfacing.
    let kept: i64 = conn
        .query_row("SELECT COUNT(*) FROM process_ownership", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, 1);

    // Third open: nothing left to reconcile, report is empty, and no new
    // INTERRUPTED events appear (exactly-once).
    drop(storage);
    let storage = open(&path).unwrap();
    assert!(storage
        .take_reconciliation_report()
        .unwrap_or_default()
        .is_empty());
    let conn = raw_conn(&path);
    let interrupted_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_events WHERE to_state = 'INTERRUPTED' AND reason_code = 'DAEMON_RESTART'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(interrupted_events, 2, "no duplicate reconciliation");

    // Interrupted workloads never leave terminal state.
    let record = storage
        .workload_record(&running.workload_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.state, WorkloadState::Interrupted);
}

#[test]
fn launch_after_reconcile_uses_a_new_request_id() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");

    let intent = {
        let storage = open(&path).unwrap();
        let intent = launch_intent("after-crash");
        assert!(matches!(
            storage.record_launch_intent(intent.clone()).unwrap(),
            LaunchIntentOutcome::Created { .. }
        ));
        intent
    };

    // Reopen reconciles; replaying the same request id returns the
    // INTERRUPTED workload instead of launching anything (spec §6: 재시작 뒤
    // request 기록은 남아 있으므로 동일 ID로 다시 시작하지 않는다).
    let storage = open(&path).unwrap();
    let report = storage.take_reconciliation_report().unwrap();
    assert_eq!(report.len(), 1);
    let replay = storage.record_launch_intent(intent).unwrap();
    assert!(matches!(
        replay,
        LaunchIntentOutcome::Existing {
            state: WorkloadState::Interrupted,
            ..
        }
    ));

    // A brand-new request id creates a fresh workload.
    let fresh = launch_intent("after-crash-new");
    assert!(matches!(
        storage.record_launch_intent(fresh.clone()).unwrap(),
        LaunchIntentOutcome::Created { .. }
    ));
    let record = storage
        .workload_record(&fresh.workload_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.state, WorkloadState::Queued);
}

#[test]
fn sessions_survive_reconciliation_untouched() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");

    let intent = {
        let storage = open(&path).unwrap();
        let intent = launch_intent("journal-keep");
        storage.record_launch_intent(intent.clone()).unwrap();
        storage
            .update_session_progress(&intent.session_id, 42, 4096)
            .unwrap();
        intent
    };

    drop(open(&path).unwrap());
    let storage = open(&path).unwrap();
    storage.take_reconciliation_report().unwrap();
    let sessions = storage.sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].id,
        SessionId::parse(intent.session_id.as_str()).unwrap()
    );
    assert_eq!(sessions[0].last_seq, 42);
    assert_eq!(sessions[0].journal_bytes, 4096);
}
