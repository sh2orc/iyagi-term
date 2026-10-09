#![allow(dead_code)]
//! Shared fixtures for integration tests.

use std::path::Path;

use term_contracts::ids::{RequestId, SessionId, U64String, WorkloadId};
use term_contracts::launch::{Enforcement, LaunchMode, LaunchPolicy, Priority};
use term_storage::{AttemptId, LaunchIntent, RequestMethod, Storage, StorageResult, TaskId};

/// Distinct launch intent tagged with a unique suffix (journal paths and ids
/// must not collide across inserts).
pub fn launch_intent(tag: &str) -> LaunchIntent {
    LaunchIntent {
        request_id: RequestId::generate(),
        method: RequestMethod::Launch,
        fingerprint: fingerprint_for(tag),
        title: format!("task {tag}"),
        task_id: Some(TaskId::generate()),
        attempt_id: Some(AttemptId::generate()),
        attempt_ordinal: 1,
        workload_id: WorkloadId::generate(),
        session_id: SessionId::generate(),
        mode: LaunchMode::Managed,
        priority: Priority(1),
        policy: LaunchPolicy {
            reservation_bytes: U64String::new(2_147_483_648).unwrap(),
            cpu_slots: 1,
            enforcement: Enforcement::Observe,
            memory_max_bytes: None,
            cpu_max_cores: None,
            pids_max: None,
        },
        journal_relative_path: format!("journals/{tag}.jnl"),
        journal_limit_bytes: 128 << 20,
        cols: 80,
        rows: 24,
    }
}

/// Stable 64-hex fingerprint derived from the tag.
pub fn fingerprint_for(tag: &str) -> String {
    let seed = tag.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |acc, b| {
        (acc ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let mut hex = format!("{seed:016x}");
    hex = hex.repeat(4);
    hex.truncate(64);
    hex
}

pub fn shell_intent(tag: &str) -> LaunchIntent {
    let mut intent = launch_intent(tag);
    intent.mode = LaunchMode::Shell;
    intent.task_id = None;
    intent.attempt_id = None;
    intent
}

pub fn open(path: &Path) -> StorageResult<Storage> {
    Storage::open(path)
}

/// Direct rusqlite connection with the same connection policy (for asserting
/// on raw rows independently of the storage API).
pub fn raw_conn(path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
        .unwrap();
    conn
}

pub fn count(conn: &rusqlite::Connection, sql: &str, param: &str) -> i64 {
    conn.query_row(sql, [param], |row| row.get(0)).unwrap()
}
