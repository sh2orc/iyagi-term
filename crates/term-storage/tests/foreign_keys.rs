//! foreign_keys=ON is part of the connection policy (spec §7): orphan rows are
//! rejected on every connection type.

mod common;

use common::{launch_intent, open, raw_conn};
use term_contracts::ids::ProcessIdentity;
use term_contracts::metrics::UsageCoverage;
use term_contracts::workload::{GroupKind, ProcessOwnership};
use term_storage::StorageError;

#[test]
fn attempt_insert_with_missing_task_fails() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    {
        let storage = open(&path).unwrap();
        drop(storage);
    }
    let conn = raw_conn(&path);
    let err = conn
        .execute(
            "INSERT INTO attempts(id, task_id, ordinal, created_at)
             VALUES ('aid', 'no-such-task', 1, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("FOREIGN KEY constraint failed"),
        "{err}"
    );
}

#[test]
fn ownership_for_missing_workload_fails_via_fk_and_api() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    {
        let storage = open(&path).unwrap();
        drop(storage);
    }
    let conn = raw_conn(&path);
    let err = conn
        .execute(
            "INSERT INTO process_ownership(workload_id, pid, start_token, boot_id, group_kind, group_reference, coverage, recorded_at)
             VALUES ('no-such-workload', 5, 'tok', 'boot', 'job', NULL, 'group', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("FOREIGN KEY constraint failed"),
        "{err}"
    );

    // The storage API maps it to a friendly NotFound before SQLite would.
    let storage = open(&path).unwrap();
    let missing = launch_intent("fk-missing").workload_id;
    let ownership = ProcessOwnership {
        workload_id: missing.clone(),
        identity: ProcessIdentity {
            pid: 7,
            start_token: "tok".into(),
            boot_id: "boot".into(),
        },
        group_kind: GroupKind::ObservedTree,
        group_reference: None,
        coverage: UsageCoverage::Partial,
    };
    assert!(matches!(
        storage.save_group_identity(ownership).unwrap_err(),
        StorageError::WorkloadNotFound { .. }
    ));
}

#[test]
fn deleting_a_task_with_attempts_is_blocked() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("fk-cascade");
    storage.record_launch_intent(intent.clone()).unwrap();

    let conn = raw_conn(&path);
    let task_id = intent.task_id.as_ref().unwrap().as_str();
    let err = conn
        .execute("DELETE FROM tasks WHERE id = ?1", [task_id])
        .unwrap_err();
    assert!(
        err.to_string().contains("FOREIGN KEY constraint failed"),
        "attempts keep the task row alive: {err}"
    );
}
