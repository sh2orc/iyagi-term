//! Migration runner.
//!
//! `docs/implementation/schema.sql` is embedded with `include_str!` and stays
//! the single source of the 0001 schema (spec `01-contracts.md` §7: the file
//! is migration 0001's input). Migration 0002 is the R1 agent-sessions schema
//! (`docs/implementation/schema-0002-agent-sessions.sql`), and migration 0003
//! is the orchestration schema (O1 spec `01-contracts.md` §5); 0003 lives
//! under `migrations/` so the reference DDL and the applied DDL cannot drift
//! apart.
//!
//! Applied versions are tracked in `schema_migrations`, which migration 0001
//! creates and self-records — that file ends with its own
//! `INSERT INTO schema_migrations`, and the runner's extra `INSERT OR IGNORE`
//! inside the same transaction preserves that row's `applied_at`.
//!
//! Application layout per migration: statements before the first
//! `CREATE` (the PRAGMA header; later migrations have none) run in autocommit,
//! where SQLite permits PRAGMA changes. Everything after runs inside one
//! `BEGIN IMMEDIATE` transaction **together with the version-row insert**, so
//! the schema and its version record commit or roll back as a unit — a crash
//! or fault mid-migration leaves version N unrecorded and no half schema
//! behind (spec `01-contracts.md` §5, case E28). All statements still execute
//! verbatim and in file order.
//!
//! Before applying an unrecorded migration the runner checks whether any
//! table it would create already exists. A pre-existing table without the
//! version row means the database was written outside this runner (torn DDL
//! from an older runner, manual copy, legacy pre-migration database): instead
//! of dropping or overwriting it, the runner fails with `MIGRATION_FAILED`
//! and recovery guidance.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::error::{StorageError, StorageResult};

/// (version, sql) in ascending order; 0001 is the docs file verbatim.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, SCHEMA_0001),
    (2, SCHEMA_0002),
    (3, SCHEMA_0003),
    (4, SCHEMA_0004),
    (5, SCHEMA_0005),
    (6, SCHEMA_0006),
];

const SCHEMA_0001: &str = include_str!("../../../docs/implementation/schema.sql");
const SCHEMA_0002: &str =
    include_str!("../../../docs/implementation/schema-0002-agent-sessions.sql");
const SCHEMA_0003: &str = include_str!("../migrations/0003_orchestration.sql");
const SCHEMA_0004: &str = include_str!("../migrations/0004_rate_limit_reset.sql");
const SCHEMA_0005: &str = include_str!("../migrations/0005_exec_recovery.sql");
const SCHEMA_0006: &str = include_str!("../migrations/0006_orch_retention.sql");

pub(crate) fn migrate(conn: &mut Connection) -> StorageResult<()> {
    for &(version, sql) in MIGRATIONS {
        // The tracker itself only exists once 0001 has run; on a fresh
        // database nothing is applied and every migration must run.
        if has_tracker(conn)? && is_applied(conn, version)? {
            continue;
        }
        precheck_untracked_tables(conn, version, sql)?;
        apply(conn, version, sql)?;
    }
    Ok(())
}

fn has_tracker(conn: &Connection) -> StorageResult<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
        [],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn is_applied(conn: &Connection, version: i64) -> StorageResult<bool> {
    let applied = conn
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = ?1",
            params![version],
            |_| Ok(()),
        )
        .optional()?;
    Ok(applied.is_some())
}

/// Refuse to run DDL that would collide with an untracked table (spec
/// `01-contracts.md` §5): if a table this migration creates already exists
/// while its version is unrecorded, the database was written outside this
/// runner — fail loudly with recovery guidance instead of dropping or
/// overwriting existing data.
fn precheck_untracked_tables(conn: &Connection, version: i64, sql: &str) -> StorageResult<()> {
    for table in created_tables(sql) {
        let exists = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            return Err(StorageError::MigrationFailed {
                version,
                message: format!(
                    "table '{table}' already exists but migration {version} is not recorded; \
                     restore from a backup or verify the existing schema and record the version \
                     manually — refusing to overwrite it"
                ),
            });
        }
    }
    Ok(())
}

/// Table names introduced by the `CREATE TABLE <name>` statements of `sql`.
/// Names in our migration files are bare identifiers; `IF NOT EXISTS` is
/// tolerated so the parse could not misread it as a table named "if".
fn created_tables(sql: &str) -> Vec<&str> {
    let mut tables = Vec::new();
    let mut rest = sql;
    while let Some(index) = rest.find("CREATE TABLE") {
        rest = rest[index + "CREATE TABLE".len()..].trim_start();
        if let Some(suffix) = rest.strip_prefix("IF NOT EXISTS") {
            rest = suffix.trim_start();
        }
        let name_len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let name = &rest[..name_len];
        if !name.is_empty() {
            tables.push(name);
        }
        rest = &rest[name_len..];
    }
    tables
}

fn apply(conn: &mut Connection, version: i64, sql: &str) -> StorageResult<()> {
    // Index-only migrations must keep their DDL inside the same transaction
    // as the version row, just like table migrations.
    let split = sql.find("CREATE ").unwrap_or(0);
    let (head, body) = sql.split_at(split);

    let fail = |error: rusqlite::Error| StorageError::MigrationFailed {
        version,
        message: error.to_string(),
    };

    // PRAGMA header (0002 has none): autocommit, where SQLite permits
    // PRAGMA changes.
    if !head.trim().is_empty() {
        conn.execute_batch(head).map_err(fail)?;
    }
    // Schema and version row commit as one unit: a fault anywhere in the
    // body rolls both back, so version N is recorded if and only if its DDL
    // landed. 0001 self-records its version inside this same transaction;
    // OR IGNORE keeps that row (and its applied_at) untouched.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(fail)?;
    tx.execute_batch(body).map_err(fail)?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
        params![version, crate::time::now_iso8601()],
    )
    .map_err(fail)?;
    tx.commit().map_err(fail)?;

    if !is_applied(conn, version)? {
        return Err(StorageError::MigrationFailed {
            version,
            message: "migration completed without recording its version".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;",
        )
        .unwrap();
        conn
    }

    fn versions(conn: &Connection) -> Vec<i64> {
        conn.prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn object_exists(conn: &Connection, name: &str) -> bool {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .unwrap();
        count == 1
    }

    #[test]
    fn schema_0001_head_contains_only_pragmas_and_comments() {
        // The split point must exist and the head must not contain DDL.
        let split = SCHEMA_0001.find("CREATE TABLE").expect("split marker");
        let head = &SCHEMA_0001[..split];
        assert!(head.contains("PRAGMA"));
        assert!(!head.contains("CREATE"));
        assert!(SCHEMA_0001[split..].starts_with("CREATE TABLE schema_migrations"));
    }

    #[test]
    fn schema_0002_has_no_head_before_its_first_create_table() {
        // 0002 starts directly with CREATE TABLE; the runner must tolerate
        // the empty head batch.
        assert!(SCHEMA_0002.starts_with("CREATE TABLE"));
    }

    #[test]
    fn fresh_apply_creates_every_table_and_records_version() {
        let mut conn = memory();
        migrate(&mut conn).unwrap();
        for table in [
            "tasks",
            "attempts",
            "workloads",
            "sessions",
            "process_ownership",
            "requests",
            "lifecycle_events",
            "layouts",
            "orch_missions",
        ] {
            assert!(object_exists(&conn, table), "table {table} missing");
        }
        assert_eq!(versions(&conn), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn second_run_is_a_no_op() {
        let mut conn = memory();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(versions(&conn), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn index_only_migration_failure_rolls_back_ddl_and_version() {
        let mut conn = memory();
        migrate(&mut conn).unwrap();
        let sql = "CREATE INDEX fixture_rate_index ON orch_runs(state); INSERT INTO missing_table VALUES (1);";
        assert!(apply(&mut conn, 6, sql).is_err());
        assert!(!object_exists(&conn, "fixture_rate_index"));
        assert_eq!(versions(&conn), vec![1, 2, 3, 4, 5, 6]);
        assert!(object_exists(&conn, "orch_run_rate_limit_reset"));
        assert!(object_exists(&conn, "orch_execs_pending"));
    }

    #[test]
    fn migrations_are_ascending() {
        let mut last = 0;
        for &(v, _) in MIGRATIONS {
            assert!(v > last, "migration versions must be ascending");
            last = v;
        }
    }

    #[test]
    fn headless_migration_body_applies_without_a_head_batch() {
        let mut conn = memory();
        migrate(&mut conn).unwrap();
        apply(
            &mut conn,
            98,
            "CREATE TABLE headless_probe(x INTEGER) STRICT;",
        )
        .unwrap();
        assert!(is_applied(&conn, 98).unwrap());
        assert!(object_exists(&conn, "headless_probe"));
    }

    #[test]
    fn fault_mid_migration_rolls_back_ddl_and_version_row() {
        // E28: a statement failing partway must leave neither the tables
        // created before the fault nor the version row.
        let mut conn = memory();
        migrate(&mut conn).unwrap();
        let sql = "CREATE TABLE orch_fault_probe(id TEXT PRIMARY KEY) STRICT;\nTHIS IS NOT SQL;";
        let error = apply(&mut conn, 99, sql).unwrap_err();
        assert!(
            matches!(error, StorageError::MigrationFailed { version: 99, .. }),
            "{error}"
        );
        assert!(!is_applied(&conn, 99).unwrap());
        assert!(
            !object_exists(&conn, "orch_fault_probe"),
            "table created before the fault must roll back"
        );
        // The connection stays usable and previously applied versions survive.
        assert_eq!(versions(&conn), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn preexisting_orchestration_table_without_version_fails_loudly() {
        // Simulate a database where 0001/0002 ran but an orchestration table
        // was created outside the runner (e.g. a torn manual apply): migration
        // 3 must refuse with recovery guidance instead of overwriting it.
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_0001).unwrap();
        conn.execute_batch("CREATE TABLE orch_missions (id TEXT PRIMARY KEY) STRICT;")
            .unwrap();
        match migrate(&mut conn).unwrap_err() {
            StorageError::MigrationFailed {
                version: 3,
                message,
            } => {
                assert!(message.contains("orch_missions"), "{message}");
                assert!(message.contains("backup"), "{message}");
                assert!(message.contains("manually"), "{message}");
            }
            other => panic!("expected MigrationFailed for version 3, got {other}"),
        }
        // The untracked table is untouched.
        assert!(object_exists(&conn, "orch_missions"));
    }

    #[test]
    fn legacy_database_without_tracker_fails_loudly() {
        // A pre-migration database (tables but no schema_migrations at all)
        // must fail with guidance rather than re-run 0001 DDL over it.
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at TEXT NOT NULL);",
        )
        .unwrap();
        match migrate(&mut conn).unwrap_err() {
            StorageError::MigrationFailed {
                version: 1,
                message,
            } => {
                assert!(message.contains("tasks"), "{message}");
                assert!(message.contains("backup"), "{message}");
            }
            other => panic!("expected MigrationFailed for version 1, got {other}"),
        }
    }
}
