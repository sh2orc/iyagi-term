//! Migration 0001 -> 0002 behavior through the public [`Storage`] API:
//! fresh apply, idempotent reopen, and R1 row preservation across the
//! upgrade (O1 spec `01-contracts.md` §5). Fault-injection and
//! untracked-table scenarios live as unit tests next to the runner in
//! `src/migration.rs`, where its private entry points are visible.

mod common;

use std::path::PathBuf;

use common::{open, raw_conn};
use tempfile::TempDir;

const SCHEMA_0001: &str = include_str!("../../../docs/implementation/schema.sql");

const ORCH_TABLES: &[&str] = &[
    "orch_missions",
    "orch_tasks",
    "orch_dependencies",
    "orch_runs",
    "orch_execs",
    "orch_entities",
    "orch_events",
    "orch_requests",
    "orch_outbox",
    "orch_workspace_leases",
    "orch_artifacts",
    "orch_uploads",
    "orch_bindings",
    "orch_config",
];

const ORCH_INDEXES: &[&str] = &[
    "orch_one_live_run_per_task",
    "orch_missions_recent",
    "orch_tasks_schedule",
    "orch_runs_by_mission",
    "orch_outbox_pending",
    "orch_artifacts_mission",
];

fn fresh_db() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    (dir, path)
}

fn object_count(conn: &rusqlite::Connection, name: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
        [name],
        |row| row.get(0),
    )
    .unwrap()
}

fn applied_versions(conn: &rusqlite::Connection) -> Vec<i64> {
    conn.prepare("SELECT version FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn fresh_apply_registers_both_versions_and_creates_orchestration_objects() {
    let (_dir, path) = fresh_db();
    {
        let storage = open(&path).unwrap();
        drop(storage);
    }
    let conn = raw_conn(&path);
    for table in ORCH_TABLES {
        assert_eq!(object_count(&conn, table), 1, "table {table} missing");
    }
    for index in ORCH_INDEXES {
        assert_eq!(object_count(&conn, index), 1, "index {index} missing");
    }
    assert_eq!(applied_versions(&conn), vec![1, 2, 3, 4, 5]);
}

#[test]
fn second_open_is_a_no_op_and_reopen_keeps_versions_stable() {
    let (_dir, path) = fresh_db();
    {
        let storage = open(&path).unwrap();
        drop(storage);
    }
    // Reopen the migrated file: no error, no duplicate objects, versions
    // stay [1, 2, 3].
    {
        let storage = open(&path).unwrap();
        drop(storage);
    }
    let conn = raw_conn(&path);
    for table in ORCH_TABLES {
        assert_eq!(object_count(&conn, table), 1, "table {table} duplicated");
    }
    assert_eq!(applied_versions(&conn), vec![1, 2, 3, 4, 5]);
}

#[test]
fn upgrade_from_0001_preserves_existing_r1_rows() {
    let (_dir, path) = fresh_db();
    {
        // A database at version 0001 holding a pre-upgrade R1 row.
        let conn = raw_conn(&path);
        conn.execute_batch(SCHEMA_0001).unwrap();
        conn.execute(
            "INSERT INTO tasks(id, title, created_at)
             VALUES ('legacy', 'keep me', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
    }
    // Storage::open sees version 1 recorded, applies 0002+0003.
    {
        let storage = open(&path).unwrap();
        drop(storage);
    }
    let conn = raw_conn(&path);
    let title: String = conn
        .query_row("SELECT title FROM tasks WHERE id = 'legacy'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(title, "keep me");
    assert_eq!(applied_versions(&conn), vec![1, 2, 3, 4, 5]);
}
