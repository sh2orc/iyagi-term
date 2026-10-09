//! Schema 0001 constraints reject malformed rows. Raw SQL on a migrated
//! database asserts what the CHECK/PK/UNIQUE constraints themselves enforce
//! (below the Rust validation layer). STRICT tables require typed literals so
//! each case hits the intended CHECK, not a type error.

mod common;

use common::{open, raw_conn};
use tempfile::TempDir;

fn migrated_raw() -> (TempDir, rusqlite::Connection) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    {
        let storage = open(&path).unwrap();
        drop(storage);
    }
    let conn = raw_conn(&path);
    (dir, conn)
}

const VALID_WORKLOAD: &str = "INSERT INTO workloads(
    id, attempt_id, mode, state, priority, reservation_bytes, cpu_slots, enforcement,
    memory_max_bytes, cpu_max_cores, pids_max, effective_policy_json, queue_reason,
    cancel_requested, root_exited, exit_code, last_error_code, created_at, started_at, finished_at)
VALUES (
    'wid', 'aid', 'managed', 'QUEUED', 1, 1073741824, 1, 'observe',
    NULL, NULL, NULL, '{}', NULL,
    0, 0, NULL, NULL, '2026-01-01T00:00:00.000Z', NULL, NULL)";

fn seed_task_and_attempt(conn: &rusqlite::Connection) {
    conn.execute(
        "INSERT INTO tasks(id, title, created_at) VALUES ('tid', 't', '2026-01-01T00:00:00.000Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO attempts(id, task_id, ordinal, created_at)
         VALUES ('aid', 'tid', 1, '2026-01-01T00:00:00.000Z')",
        [],
    )
    .unwrap();
}

/// Insert the canonical valid workload row.
fn valid_workload(conn: &rusqlite::Connection) -> usize {
    conn.execute(VALID_WORKLOAD, []).unwrap()
}

/// Insert a workload row, replacing one fragment of the canonical statement.
fn workload_with(conn: &rusqlite::Connection, from: &str, to: &str) -> rusqlite::Result<usize> {
    assert!(
        VALID_WORKLOAD.contains(from),
        "fixture fragment {from:?} missing"
    );
    conn.execute(&VALID_WORKLOAD.replacen(from, to, 1), [])
}

#[test]
fn bad_workload_enum_and_range_values_are_rejected() {
    let (_dir, conn) = migrated_raw();
    seed_task_and_attempt(&conn);

    let bad = [
        ("'managed'", "'daemon'", "mode"),
        ("'QUEUED'", "'PAUSED'", "state"),
        ("'observe'", "'force'", "enforcement"),
        ("1, 1073741824, 1", "1, 0, 1", "reservation_bytes > 0"),
        ("1, 1073741824, 1", "1, 1073741824, 0", "cpu_slots > 0"),
    ];
    for (from, to, what) in bad {
        let result = workload_with(&conn, from, to);
        assert!(result.is_err(), "{what} must be rejected ({from} -> {to})");
    }
    // priority must stay within 0..=2.
    assert!(workload_with(&conn, "'QUEUED', 1,", "'QUEUED', 3,").is_err());
    // bool-ish flags only accept 0/1.
    assert!(workload_with(&conn, "0, 0, NULL", "2, 0, NULL").is_err());
}

#[test]
fn managed_requires_attempt_and_shell_forbids_it() {
    let (_dir, conn) = migrated_raw();
    seed_task_and_attempt(&conn);
    // shell + attempt_id violates the mode/attempt CHECK.
    assert!(workload_with(&conn, "'managed'", "'shell'").is_err());

    // managed without attempt violates it too.
    conn.execute("DELETE FROM workloads", []).unwrap();
    assert!(workload_with(&conn, "'aid', 'managed'", "NULL, 'managed'").is_err());

    // shell without attempt is the valid combination.
    conn.execute("DELETE FROM workloads", []).unwrap();
    workload_with(&conn, "'aid', 'managed'", "NULL, 'shell'").unwrap();
}

#[test]
fn optional_limit_columns_must_be_positive() {
    let (_dir, conn) = migrated_raw();
    seed_task_and_attempt(&conn);
    assert!(workload_with(&conn, "NULL, NULL, NULL, '{}'", "0, NULL, NULL, '{}'").is_err());
    conn.execute("DELETE FROM workloads", []).unwrap();
    assert!(workload_with(&conn, "NULL, NULL, NULL, '{}'", "NULL, 0.0, NULL, '{}'").is_err());
    conn.execute("DELETE FROM workloads", []).unwrap();
    assert!(workload_with(&conn, "NULL, NULL, NULL, '{}'", "NULL, NULL, 0, '{}'").is_err());
    conn.execute("DELETE FROM workloads", []).unwrap();
    workload_with(&conn, "NULL, NULL, NULL, '{}'", "8589934592, 2.5, 64, '{}'").unwrap();
}

#[test]
fn session_constraints_reject_bad_dimensions_and_limits() {
    let (_dir, conn) = migrated_raw();
    seed_task_and_attempt(&conn);
    valid_workload(&conn);
    let base = "INSERT INTO sessions(id, workload_id, initial_cols, initial_rows, journal_relative_path, journal_limit_bytes, created_at) VALUES ('sid', 'wid', ";
    let bad = [
        format!("{base}1, 24, 'journals/a.jnl', 134217728, '2026-01-01T00:00:00.000Z')"),
        format!("{base}1001, 24, 'journals/a.jnl', 134217728, '2026-01-01T00:00:00.000Z')"),
        format!("{base}80, 1, 'journals/a.jnl', 134217728, '2026-01-01T00:00:00.000Z')"),
        format!("{base}80, 1001, 'journals/a.jnl', 134217728, '2026-01-01T00:00:00.000Z')"),
        format!("{base}80, 24, 'journals/a.jnl', 0, '2026-01-01T00:00:00.000Z')"),
    ];
    for sql in bad {
        assert!(conn.execute(&sql, []).is_err(), "must reject: {sql}");
    }
    conn.execute(
        "INSERT INTO sessions(id, workload_id, initial_cols, initial_rows, journal_relative_path, journal_limit_bytes, created_at)
         VALUES ('sid', 'wid', 80, 24, 'journals/a.jnl', 134217728, '2026-01-01T00:00:00.000Z')",
        [],
    )
    .unwrap();
}

#[test]
fn session_flag_and_replay_constraints() {
    let (_dir, conn) = migrated_raw();
    seed_task_and_attempt(&conn);
    valid_workload(&conn);
    conn.execute(
        "INSERT INTO sessions(id, workload_id, initial_cols, initial_rows, journal_relative_path, journal_limit_bytes, created_at)
         VALUES ('sid', 'wid', 80, 24, 'journals/a.jnl', 134217728, '2026-01-01T00:00:00.000Z')",
        [],
    )
    .unwrap();
    assert!(conn
        .execute(
            "INSERT INTO sessions(id, workload_id, initial_cols, initial_rows, journal_relative_path, journal_limit_bytes, pinned, created_at)
             VALUES ('sid2', 'wid', 80, 24, 'journals/b.jnl', 134217728, 2, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .is_err());
    // one session per workload (UNIQUE)
    assert!(conn
        .execute(
            "INSERT INTO sessions(id, workload_id, initial_cols, initial_rows, journal_relative_path, journal_limit_bytes, created_at)
             VALUES ('sid3', 'wid', 80, 24, 'journals/c.jnl', 134217728, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .is_err());
    // replay_status vocabulary
    assert!(conn
        .execute(
            "UPDATE sessions SET replay_status = 'partial' WHERE id = 'sid'",
            [],
        )
        .is_err());
    conn.execute(
        "UPDATE sessions SET replay_status = 'tail_truncated' WHERE id = 'sid'",
        [],
    )
    .unwrap();
}

#[test]
fn process_ownership_constraints_reject_bad_rows() {
    let (_dir, conn) = migrated_raw();
    seed_task_and_attempt(&conn);
    valid_workload(&conn);
    let insert = |sql: &str| conn.execute(sql, []);
    assert!(insert(
        "INSERT INTO process_ownership(workload_id, pid, start_token, boot_id, group_kind, group_reference, coverage, recorded_at)
         VALUES ('wid', 0, 'tok', 'boot', 'job', NULL, 'group', '2026-01-01T00:00:00.000Z')"
    )
    .is_err());
    assert!(insert(
        "INSERT INTO process_ownership(workload_id, pid, start_token, boot_id, group_kind, group_reference, coverage, recorded_at)
         VALUES ('wid', 12, 'tok', 'boot', 'gang', NULL, 'group', '2026-01-01T00:00:00.000Z')"
    )
    .is_err());
    assert!(insert(
        "INSERT INTO process_ownership(workload_id, pid, start_token, boot_id, group_kind, group_reference, coverage, recorded_at)
         VALUES ('wid', 12, 'tok', 'boot', 'job', NULL, 'full', '2026-01-01T00:00:00.000Z')"
    )
    .is_err());
    insert(
        "INSERT INTO process_ownership(workload_id, pid, start_token, boot_id, group_kind, group_reference, coverage, recorded_at)
         VALUES ('wid', 12, 'tok', 'boot', 'job', 'job-obj-7', 'group', '2026-01-01T00:00:00.000Z')",
    )
    .unwrap();
}

#[test]
fn request_constraints_reject_bad_method_fingerprint_outcome_and_duplicates() {
    let (_dir, conn) = migrated_raw();
    seed_task_and_attempt(&conn);
    valid_workload(&conn);
    let fp = "ab".repeat(32);
    let ok = format!(
        "INSERT INTO requests(id, method, fingerprint, workload_id, outcome, created_at)
         VALUES ('rid', 'workload.launch', '{fp}', 'wid', 'accepted', '2026-01-01T00:00:00.000Z')"
    );
    conn.execute(&ok, []).unwrap();

    // PK duplicate.
    assert!(conn.execute(&ok, []).is_err());
    // Unknown method.
    assert!(conn
        .execute(
            "INSERT INTO requests(id, method, fingerprint, workload_id, outcome, created_at)
             VALUES ('rid2', 'workload.kill', ?, 'wid', 'accepted', '2026-01-01T00:00:00.000Z')",
            [&fp],
        )
        .is_err());
    // Fingerprint length must be exactly 64.
    assert!(conn
        .execute(
            "INSERT INTO requests(id, method, fingerprint, workload_id, outcome, created_at)
             VALUES ('rid3', 'workload.launch', ?, 'wid', 'accepted', '2026-01-01T00:00:00.000Z')",
            ["ab".repeat(31)],
        )
        .is_err());
    assert!(conn
        .execute(
            "INSERT INTO requests(id, method, fingerprint, workload_id, outcome, created_at)
             VALUES ('rid4', 'workload.launch', ?, 'wid', 'accepted', '2026-01-01T00:00:00.000Z')",
            ["ab".repeat(33)],
        )
        .is_err());
    // Unknown outcome.
    assert!(conn
        .execute(
            "INSERT INTO requests(id, method, fingerprint, workload_id, outcome, created_at)
             VALUES ('rid5', 'workload.launch', ?, 'wid', 'pending', '2026-01-01T00:00:00.000Z')",
            [&fp],
        )
        .is_err());
}

#[test]
fn unique_and_value_constraints_on_attempts_layouts() {
    let (_dir, conn) = migrated_raw();
    seed_task_and_attempt(&conn);
    // (task_id, ordinal) unique.
    assert!(conn
        .execute(
            "INSERT INTO attempts(id, task_id, ordinal, created_at)
             VALUES ('aid2', 'tid', 1, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .is_err());
    // ordinal >= 1.
    assert!(conn
        .execute(
            "INSERT INTO attempts(id, task_id, ordinal, created_at)
             VALUES ('aid3', 'tid', 0, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .is_err());

    // layouts schema_version pinned to 1.
    assert!(conn
        .execute(
            "INSERT INTO layouts(workspace_id, schema_version, tree_json, updated_at)
             VALUES ('ws', 2, '{}', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .is_err());
}
