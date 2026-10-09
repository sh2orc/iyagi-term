//! `agent_sessions`(migration 0002, spec `02-runner.md` §8) 저장 규칙:
//! 마이그레이션 적용(새 DB / 이미 버전 1인 DB), upsert 충돌 의미,
//! 목록의 중복 제거·정렬, 종료·삭제·보존 정리, 데몬 재시작 정리.

mod common;

use common::{open, raw_conn, shell_intent};
use term_contracts::agent_session::AgentSessionSource;
use term_contracts::ids::{SessionId, WorkloadId};
use term_storage::{AgentSessionUpsert, Storage};

fn upsert_for(workload_id: &WorkloadId, agent: &str, session_id: &str) -> AgentSessionUpsert {
    AgentSessionUpsert {
        workload_id: workload_id.clone(),
        pty_session_id: None,
        agent: agent.to_string(),
        agent_session_id: session_id.to_string(),
        cwd: "/repo".to_string(),
        title: None,
        program: None,
        source: AgentSessionSource::Registry,
    }
}

fn versions(path: &std::path::Path) -> Vec<i64> {
    let conn = raw_conn(path);
    let mut stmt = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get(0)).unwrap();
    rows.collect::<Result<_, _>>().unwrap()
}

/// 워크로드 행이 있어야 upsert가 의미 있는 것은 아니지만(FK 없음), 실제
/// 흐름과 같게 하나 만들어 둔다.
fn workload(storage: &Storage, tag: &str) -> WorkloadId {
    let intent = shell_intent(tag);
    let id = intent.workload_id.clone();
    storage.record_launch_intent(intent).unwrap();
    id
}

#[test]
fn migration_0002_applies_on_a_fresh_db_and_on_an_existing_version_1_db() {
    let dir = tempfile::TempDir::new().unwrap();
    let fresh = dir.path().join("fresh.db3");
    {
        let _storage = open(&fresh).unwrap();
    }
    assert_eq!(versions(&fresh), vec![1, 2, 3, 4, 5, 6]);

    // 이미 버전 1만 기록된 DB(0001 파일만 적용된 상태)를 만들어 두 번째
    // 열기에서 0002가 얹히는지 확인한다.
    let upgraded = dir.path().join("upgraded.db3");
    {
        let conn = raw_conn(&upgraded);
        conn.execute_batch(include_str!("../../../docs/implementation/schema.sql"))
            .unwrap();
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'agent_sessions'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "0001만 적용된 상태여야 한다");
    }
    {
        let _storage = open(&upgraded).unwrap();
    }
    assert_eq!(versions(&upgraded), vec![1, 2, 3, 4, 5, 6]);
    let conn = raw_conn(&upgraded);
    let indexes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index'
             AND name IN ('agent_sessions_recent', 'agent_sessions_workload')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 2);

    // 세 번째 열기는 무연산이다.
    {
        let _storage = open(&upgraded).unwrap();
    }
    assert_eq!(versions(&upgraded), vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn upsert_conflict_refreshes_and_never_loses_a_known_title() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = open(&dir.path().join("m.db3")).unwrap();
    let workload_id = workload(&storage, "upsert");
    let pty = SessionId::generate();

    let mut first = upsert_for(
        &workload_id,
        "claude",
        "7db2598e-c360-48fe-a2d5-0240993c9f7a",
    );
    first.title = Some("iyagi-7d".into());
    first.program = Some("/usr/local/bin/claude".into());
    first.pty_session_id = Some(pty.clone());
    let created = storage.upsert_agent_session(first).unwrap();
    assert_eq!(created.first_seen_at, created.last_seen_at);
    assert!(!created.active, "active는 데몬이 채운다");
    assert_eq!(created.source, AgentSessionSource::Registry);

    // 종료 표시 후 재관찰: 같은 행이 되살아난다.
    assert!(storage
        .end_agent_session(
            &workload_id,
            "claude",
            "7db2598e-c360-48fe-a2d5-0240993c9f7a",
            "replaced"
        )
        .unwrap());

    // 제목/program/pty를 모르는 hook 보고가 뒤따라와도 기존 값은 남는다.
    let mut second = upsert_for(
        &workload_id,
        "claude",
        "7db2598e-c360-48fe-a2d5-0240993c9f7a",
    );
    second.source = AgentSessionSource::Hook;
    second.cwd = "/somewhere/else".into();
    let updated = storage.upsert_agent_session(second).unwrap();

    assert_eq!(updated.id, created.id, "충돌은 새 행이 아니라 갱신이다");
    assert_eq!(updated.title.as_deref(), Some("iyagi-7d"));
    assert_eq!(updated.program.as_deref(), Some("/usr/local/bin/claude"));
    assert_eq!(updated.pty_session_id, Some(pty));
    assert_eq!(updated.source, AgentSessionSource::Hook, "출처는 최신 것");
    assert_eq!(updated.cwd, "/repo", "cwd는 최초 관찰값을 유지한다");
    assert_eq!(updated.first_seen_at, created.first_seen_at);
    assert!(updated.ended_at.is_none(), "재관찰은 종료 표시를 지운다");
    assert!(updated.end_reason.is_none());

    let rows: i64 = raw_conn(&dir.path().join("m.db3"))
        .query_row("SELECT COUNT(*) FROM agent_sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn upsert_rejects_hostile_ids_without_writing() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = open(&dir.path().join("m.db3")).unwrap();
    let workload_id = workload(&storage, "hostile");

    let mut bad = upsert_for(&workload_id, "claude", "../../etc/passwd");
    bad.title = Some("x".into());
    assert!(storage.upsert_agent_session(bad).is_err());

    let mut empty_agent = upsert_for(&workload_id, "  ", "7db2598e-c360-48fe-a2d5-0240993c9f7a");
    empty_agent.title = None;
    assert!(storage.upsert_agent_session(empty_agent).is_err());

    assert!(storage.list_agent_sessions(50, None).unwrap().is_empty());
}

#[test]
fn list_dedupes_by_agent_session_and_orders_by_last_seen() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = open(&dir.path().join("m.db3")).unwrap();
    let old_workload = workload(&storage, "list-old");
    let new_workload = workload(&storage, "list-new");
    let other = workload(&storage, "list-other");

    // 같은 대화를 두 워크로드가 관찰 → 최신 하나만 목록에 나온다.
    storage
        .upsert_agent_session(upsert_for(&old_workload, "claude", "session-a"))
        .unwrap();
    // 고정폭 ISO-8601은 밀리초 해상도라 같은 밀리초에 몰리지 않게 한다.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let newer = storage
        .upsert_agent_session(upsert_for(&new_workload, "claude", "session-a"))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let mut codex = upsert_for(&other, "codex", "session-b");
    codex.cwd = "/other".into();
    codex.source = AgentSessionSource::LockFile;
    let codex_row = storage.upsert_agent_session(codex).unwrap();

    let all = storage.list_agent_sessions(50, None).unwrap();
    assert_eq!(all.len(), 2, "(agent, session id)별 한 건: {all:?}");
    assert_eq!(all[0].id, codex_row.id, "최신이 먼저");
    assert_eq!(all[1].id, newer.id);
    assert_eq!(all[1].workload_id, new_workload);

    // cwd 필터.
    let filtered = storage.list_agent_sessions(50, Some("/other")).unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].agent, "codex");
    assert_eq!(filtered[0].source, AgentSessionSource::LockFile);

    // limit.
    assert_eq!(storage.list_agent_sessions(1, None).unwrap().len(), 1);
    assert!(storage
        .list_agent_sessions(50, Some("/nowhere"))
        .unwrap()
        .is_empty());
}

#[test]
fn end_forget_and_prune_have_the_documented_effects() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("m.db3");
    let storage = open(&path).unwrap();
    let workload_id = workload(&storage, "end");

    let a = storage
        .upsert_agent_session(upsert_for(&workload_id, "claude", "session-a"))
        .unwrap();
    let b = storage
        .upsert_agent_session(upsert_for(&workload_id, "codex", "session-b"))
        .unwrap();

    // 한 건만 닫기 — 두 번째 호출은 false(이미 닫혔다).
    assert!(storage
        .end_agent_session(&workload_id, "claude", "session-a", "replaced")
        .unwrap());
    assert!(!storage
        .end_agent_session(&workload_id, "claude", "session-a", "replaced")
        .unwrap());
    assert!(!storage
        .end_agent_session(&workload_id, "claude", "nope", "replaced")
        .unwrap());

    // 워크로드 단위로 남은 것 닫기: 이미 닫힌 것은 세지 않는다.
    assert_eq!(
        storage
            .end_agent_sessions_for_workload(&workload_id, "workload_exited")
            .unwrap(),
        1
    );
    let listed = storage.list_agent_sessions(50, None).unwrap();
    let closed: Vec<_> = listed.iter().map(|r| r.end_reason.as_deref()).collect();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|r| r.ended_at.is_some()), "{closed:?}");
    assert!(closed.contains(&Some("replaced")));
    assert!(closed.contains(&Some("workload_exited")));

    // forget: 한 건만 사라진다.
    assert!(storage.forget_agent_session(&a.id).unwrap());
    assert!(!storage.forget_agent_session(&a.id).unwrap());
    assert_eq!(storage.list_agent_sessions(50, None).unwrap().len(), 1);

    // prune: 나이 기준으로는 아무것도 안 지운다(방금 관찰).
    assert_eq!(storage.prune_agent_sessions(90, 1_000).unwrap(), 0);
    // 개수 상한을 0으로 내리면 **종료된** 행은 전부 밀려난다(남은 한 건은
    // 위에서 `workload_exited`로 닫혔다).
    assert_eq!(storage.prune_agent_sessions(90, 0).unwrap(), 1);
    assert!(storage.list_agent_sessions(50, None).unwrap().is_empty());
    let _ = b;
}

/// 개수 상한은 이력을 자르는 장치다 — 아직 열려 있는 행은 밀려나도 지우지
/// 않는다(지우면 지금 붙어 있는 대화를 "이어서 열기"할 수 없다).
#[test]
fn prune_overflow_never_deletes_an_open_row() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("m.db3");
    let storage = open(&path).unwrap();
    let workload_id = workload(&storage, "prune-overflow");

    let open_row = storage
        .upsert_agent_session(upsert_for(&workload_id, "claude", "session-open"))
        .unwrap();
    storage
        .upsert_agent_session(upsert_for(&workload_id, "codex", "session-closed"))
        .unwrap();
    assert!(storage
        .end_agent_session(&workload_id, "codex", "session-closed", "workload_exited")
        .unwrap());

    // 상한 0: 두 행 모두 "밀려난" 상태지만 종료된 한 건만 사라진다.
    assert_eq!(storage.prune_agent_sessions(90, 0).unwrap(), 1);
    let left = storage.list_agent_sessions(50, None).unwrap();
    assert_eq!(left.len(), 1, "열린 행은 남는다: {left:?}");
    assert_eq!(left[0].id, open_row.id);
    assert!(left[0].ended_at.is_none());

    // 그 행이 닫히면 다음 스윕에서 비로소 지워진다.
    assert_eq!(
        storage
            .end_agent_sessions_for_workload(&workload_id, "workload_exited")
            .unwrap(),
        1
    );
    assert_eq!(storage.prune_agent_sessions(90, 0).unwrap(), 1);
    assert!(storage.list_agent_sessions(50, None).unwrap().is_empty());
}

#[test]
fn prune_keeps_open_rows_regardless_of_age() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("m.db3");
    let storage = open(&path).unwrap();
    let workload_id = workload(&storage, "prune-age");
    let open_row = storage
        .upsert_agent_session(upsert_for(&workload_id, "claude", "session-open"))
        .unwrap();
    let closed_row = storage
        .upsert_agent_session(upsert_for(&workload_id, "claude", "session-closed"))
        .unwrap();
    assert!(storage
        .end_agent_session(&workload_id, "claude", "session-closed", "workload_exited")
        .unwrap());

    // 두 행 모두 "오래됐다"고 보이게 last_seen_at을 과거로 밀어 둔다.
    raw_conn(&path)
        .execute(
            "UPDATE agent_sessions SET last_seen_at = '2020-01-01T00:00:00.000Z'",
            [],
        )
        .unwrap();

    // 종료된 것만 나이로 지운다 — 오래 붙어 있는 세션은 남는다.
    assert_eq!(storage.prune_agent_sessions(90, 1_000).unwrap(), 1);
    let left = storage.list_agent_sessions(50, None).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, open_row.id);
    let _ = closed_row;
}

#[test]
fn reopen_closes_open_agent_sessions_as_daemon_restart() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("m.db3");

    let (workload_id, row_id) = {
        let storage = open(&path).unwrap();
        let workload_id = workload(&storage, "restart");
        let row = storage
            .upsert_agent_session(upsert_for(&workload_id, "claude", "session-live"))
            .unwrap();
        assert!(row.ended_at.is_none());
        (workload_id, row.id)
    };

    {
        let storage = open(&path).unwrap();
        let listed = storage.list_agent_sessions(50, None).unwrap();
        assert_eq!(listed.len(), 1, "행은 남는다 — 복구 목록이 그것이다");
        assert_eq!(listed[0].id, row_id);
        assert!(listed[0].ended_at.is_some());
        assert_eq!(listed[0].end_reason.as_deref(), Some("daemon_restart"));

        // 같은 세션을 다시 관찰하면 되살아난다.
        let revived = storage
            .upsert_agent_session(upsert_for(&workload_id, "claude", "session-live"))
            .unwrap();
        assert_eq!(revived.id, row_id);
        assert!(revived.ended_at.is_none());
    }
}

#[test]
fn recovery_filters_precede_deduplication_and_limit() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("recovery.db3");
    let storage = open(&path).unwrap();
    let target = workload(&storage, "recovery-target");
    let other = workload(&storage, "recovery-other");
    let pty = SessionId::generate();
    let mut old = upsert_for(&target, "claude", "old-session");
    old.pty_session_id = Some(pty.clone());
    storage.upsert_agent_session(old).unwrap();
    raw_conn(&path)
        .execute(
            "UPDATE agent_sessions SET last_seen_at = '2020-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
    // Another workload observed the same conversation more recently.
    storage
        .upsert_agent_session(upsert_for(&other, "claude", "old-session"))
        .unwrap();
    for i in 0..200 {
        storage
            .upsert_agent_session(upsert_for(&other, "codex", &format!("recent-{i}")))
            .unwrap();
    }
    assert!(storage
        .list_agent_sessions(200, None)
        .unwrap()
        .iter()
        .all(|r| r.workload_id != target));
    let found = storage
        .list_agent_sessions_filtered(1, None, Some(&target), None)
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].agent_session_id, "old-session");
    assert_eq!(found[0].workload_id, target);
    assert_eq!(found[0].cwd, "/repo");
    let missing = WorkloadId::generate();
    // Workload and PTY are alternatives, including when one no longer matches.
    let by_pty = storage
        .list_agent_sessions_filtered(1, None, Some(&missing), Some(&pty))
        .unwrap();
    assert_eq!(by_pty, found);
    assert!(storage
        .list_agent_sessions_filtered(1, None, Some(&missing), None)
        .unwrap()
        .is_empty());
    assert!(storage
        .list_agent_sessions_filtered(1, Some(""), Some(&target), None)
        .unwrap()
        .is_empty());
}
