//! Real daemon death during an Integrator or continuation Exec, followed by
//! observed native exit and explicit reconstruction from immutable sources.
use super::*;
use std::path::{Path, PathBuf};
use term_contracts::mission::types::*;

pub(super) fn native_supported() -> bool {
    if cfg!(target_os = "linux")
        && term_platform::group::select_backend()
            .capabilities()
            .memory_limit_kind
            .support
            != term_contracts::snapshot::LimitSupport::Supported
    {
        assert_ne!(
            std::env::var("IYAGI_CGROUP_REQUIRE_DELEGATION").as_deref(),
            Ok("1")
        );
        return false;
    }
    true
}

pub(super) fn install_capture_barrier(repo: &Path, workspace: &str, marker: &Path, release: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fn quoted(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
    let hook = repo.join(".git/hooks/post-index-change");
    let canonical = Path::new(workspace).canonicalize().unwrap();
    std::fs::write(&hook, format!("#!/bin/sh\n[ \"$(pwd -P)\" = {} ] || exit 0\n[ -e {} ] && exit 0\nprintf x > {}\nprintf 'quarantined fixture output\\n' > unknown-only.txt\nwhile [ ! -e {} ]; do sleep 0.05; done\n",
        quoted(canonical.to_str().unwrap()), quoted(marker.to_str().unwrap()), quoted(marker.to_str().unwrap()), quoted(release.to_str().unwrap()))).unwrap();
    std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn snapshot(client: &mut Client, mission: &str) -> Vec<Entity> {
    let page = client
        .request(
            "mission.snapshot",
            json!({"mission_id": mission, "snapshot_id": null, "cursor": null}),
        )
        .unwrap();
    serde_json::from_value(page["entities"].clone()).unwrap()
}
fn mission(entities: &[Entity]) -> &Mission {
    entities
        .iter()
        .find_map(|e| match e {
            Entity::Mission(m) => Some(m.as_ref()),
            _ => None,
        })
        .unwrap()
}
fn run(entities: &[Entity], id: &Id) -> Run {
    entities
        .iter()
        .find_map(|e| match e {
            Entity::Run(r) if &r.id == id => Some(r.as_ref().clone()),
            _ => None,
        })
        .unwrap()
}
fn workspace(entities: &[Entity], id: &Id) -> Workspace {
    entities
        .iter()
        .find_map(|e| match e {
            Entity::Workspace(w) if &w.id == id => Some(w.as_ref().clone()),
            _ => None,
        })
        .unwrap()
}
struct ReleaseOnDrop(PathBuf);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"release");
    }
}

pub(super) fn recover(
    daemon: &mut DaemonProc,
    client: &mut Client,
    mission_id: &str,
    failed: &Run,
    marker: &Path,
    release: &Path,
    continuation: bool,
) -> (Run, Workspace) {
    let _release = ReleaseOnDrop(release.to_path_buf());
    let attempt = if continuation { 3 } else { 2 };
    let active = eventually(|| {
        let s = snapshot(client, mission_id);
        (marker.exists() && s.iter().any(|e| matches!(e, Entity::Run(r) if r.task_id == failed.task_id && r.attempt == attempt && r.state == RunState::Running))).then_some(s)
    }, Duration::from_secs(25)).expect("owned integration execution reaches the crash barrier");
    let running = active
        .iter()
        .find_map(|e| match e {
            Entity::Run(r) if r.task_id == failed.task_id && r.attempt == attempt => {
                Some(r.as_ref().clone())
            }
            _ => None,
        })
        .unwrap();
    let exec = active
        .iter()
        .find_map(|e| match e {
            Entity::Exec(e) if Some(&e.id) == running.exec_id.as_ref() => Some(e.as_ref().clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(exec.state, ExecState::Spawned);
    assert!(exec.group_identity.is_some());
    let identity = exec.identity.clone().unwrap();
    let count = active
        .iter()
        .filter(|e| matches!(e, Entity::Run(_)))
        .count();
    daemon.kill();
    assert_eq!(
        term_platform::identity::process_identity(identity.pid).as_ref(),
        Some(&identity)
    );
    let recovered =
        DaemonProc::spawn_fixture_on(daemon.data_dir.clone(), "integration-conflict-restart");
    let (next_client, _) = Client::control(&recovered.endpoint, &recovered.token);
    *daemon = recovered;
    *client = next_client;
    let unknown = eventually(
        || {
            let s = snapshot(client, mission_id);
            matches!(
                run(&s, &running.id).state,
                RunState::Unknown | RunState::Interrupted
            )
            .then_some(s)
        },
        Duration::from_secs(10),
    )
    .expect("daemon records an uncertain result without ending its process");
    assert_eq!(
        unknown
            .iter()
            .filter(|e| matches!(e, Entity::Run(_)))
            .count(),
        count
    );
    assert!(run(&unknown, &running.id).holds_execution_slot());
    assert_eq!(
        term_platform::identity::process_identity(identity.pid).as_ref(),
        Some(&identity)
    );
    assert!(unknown.iter().all(|e| !matches!(e, Entity::Decision(d) if d.requesting_run_id.as_ref() == Some(&running.id) && d.options.iter().any(|o| o.id == "retry_reconciled_task"))));
    std::fs::write(release, b"release").unwrap();
    let ended = eventually(|| {
        let s = snapshot(client, mission_id);
        s.iter().any(|e| matches!(e, Entity::Decision(d) if d.state == DecisionState::Open && d.requesting_run_id.as_ref() == Some(&running.id) && d.options.iter().any(|o| o.id == "retry_reconciled_task"))).then_some(s)
    }, Duration::from_secs(20)).expect("native exit creates an explicit recovery choice");
    let historical = run(&ended, &running.id);
    assert!(historical.reconciliation_ref.is_some());
    assert!(!historical.holds_execution_slot());
    assert!(matches!(
        historical.state,
        RunState::Unknown | RunState::Interrupted
    ));
    assert_eq!(
        ended.iter().filter(|e| matches!(e, Entity::Run(_))).count(),
        count
    );
    let quarantined = workspace(&ended, historical.workspace_id.as_ref().unwrap());
    assert_eq!(quarantined.state, WorkspaceState::Quarantined);
    assert!(quarantined.writer_run_id.is_none());
    assert!(ended.iter().any(|e| matches!(e, Entity::Exec(e) if e.id == exec.id && e.owner_daemon_id == exec.owner_daemon_id && e.state == ExecState::Exited && e.ended_at.is_some())));
    let decision = ended
        .iter()
        .find_map(|e| match e {
            Entity::Decision(d)
                if d.state == DecisionState::Open
                    && d.requesting_run_id.as_ref() == Some(&running.id) =>
            {
                Some(d)
            }
            _ => None,
        })
        .unwrap();
    let params = json!({"request_id": uuid(), "mission_id": mission_id, "expected_revision": mission(&ended).revision,
        "decision_id": decision.id, "option_id": "retry_reconciled_task", "answer_ref": null});
    let db = rusqlite::Connection::open(daemon.data_dir.join("data/iyagi.db")).unwrap();
    db.execute_batch("CREATE TRIGGER hold_integration_rebuild BEFORE UPDATE ON orch_tasks WHEN json_extract(NEW.document_json, '$.integration.step') = 'automatic' BEGIN SELECT RAISE(ABORT, 'rebuild storage outage'); END;").unwrap();
    assert_eq!(
        client
            .request("mission.decision.answer", params.clone())
            .unwrap_err()["code"],
        "STORAGE_UNAVAILABLE"
    );
    let unchanged = snapshot(client, mission_id);
    assert_eq!(mission(&unchanged).revision, mission(&ended).revision);
    assert_eq!(run(&unchanged, &historical.id), historical);
    assert_eq!(workspace(&unchanged, &quarantined.id), quarantined);
    assert!(unchanged.iter().any(|e| matches!(e, Entity::Decision(d) if d.id == decision.id && d.state == DecisionState::Open)));
    db.execute_batch("DROP TRIGGER hold_integration_rebuild")
        .unwrap();
    let receipt = client
        .request("mission.decision.answer", params.clone())
        .unwrap();
    assert_eq!(
        client.request("mission.decision.answer", params).unwrap(),
        receipt
    );
    let rebuilt = eventually(|| {
        let s = snapshot(client, mission_id);
        s.iter().any(|e| matches!(e, Entity::Decision(d) if d.kind == DecisionKind::Conflict && d.state == DecisionState::Open && d.requesting_run_id.as_ref() != Some(&failed.id))).then_some(s)
    }, Duration::from_secs(20)).expect("frozen inputs reconstruct the original conflict in a new workspace");
    assert_eq!(run(&rebuilt, &running.id), historical);
    assert_eq!(workspace(&rebuilt, &quarantined.id), quarantined);
    let decision = rebuilt
        .iter()
        .find_map(|e| match e {
            Entity::Decision(d)
                if d.kind == DecisionKind::Conflict && d.state == DecisionState::Open =>
            {
                Some(d)
            }
            _ => None,
        })
        .unwrap();
    let fresh = run(&rebuilt, decision.requesting_run_id.as_ref().unwrap());
    assert_eq!(fresh.task_id, failed.task_id);
    assert_eq!(fresh.attempt, attempt + 1);
    assert_ne!(fresh.workspace_id, historical.workspace_id);
    assert!(fresh.binding_snapshot.is_none());
    let fresh_workspace = workspace(&rebuilt, fresh.workspace_id.as_ref().unwrap());
    assert!(!Path::new(&fresh_workspace.path)
        .join("unknown-only.txt")
        .exists());
    assert!(
        std::fs::read_to_string(Path::new(&fresh_workspace.path).join("shared.txt"))
            .unwrap()
            .contains("<<<<<<<")
    );
    client
        .request(
            "mission.decision.answer",
            json!({"request_id": uuid(), "mission_id": mission_id,
        "expected_revision": mission(&rebuilt).revision, "decision_id": decision.id,
        "option_id": "resolve_and_reintegrate", "answer_ref": null}),
        )
        .unwrap();
    (historical, quarantined)
}

pub(super) fn reject_changed_acceptance_evidence(
    daemon: &DaemonProc,
    client: &mut Client,
    params: &Value,
    unknown: &Run,
) {
    let db = rusqlite::Connection::open(daemon.data_dir.join("data/iyagi.db")).unwrap();
    for (table, id, field, changed) in [
        (
            "orch_execs",
            unknown.exec_id.as_ref().unwrap(),
            "ended_at",
            json!("changed termination"),
        ),
        (
            "orch_tasks",
            &unknown.task_id,
            "active_run_id",
            json!(unknown.id),
        ),
    ] {
        let document: String = db
            .query_row(
                &format!("SELECT document_json FROM {table} WHERE id=?1"),
                [id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        let mut modified: Value = serde_json::from_str(&document).unwrap();
        modified[field] = changed;
        db.execute(
            &format!("UPDATE {table} SET document_json=?1 WHERE id=?2"),
            rusqlite::params![modified.to_string(), id.as_str()],
        )
        .unwrap();
        let mut attempt = params.clone();
        attempt["request_id"] = json!(uuid());
        let rejected = client.request("mission.accept", attempt);
        db.execute(
            &format!("UPDATE {table} SET document_json=?1 WHERE id=?2"),
            rusqlite::params![document, id.as_str()],
        )
        .unwrap();
        assert_eq!(
            rejected.unwrap_err()["code"],
            "INVALID_STATE",
            "{table} altered evidence must fail"
        );
    }
    db.execute_batch("CREATE TRIGGER hold_reconciled_acceptance BEFORE INSERT ON orch_events WHEN NEW.event_type = 'accepted' BEGIN SELECT RAISE(ABORT, 'acceptance storage outage'); END;").unwrap();
    let failed = client.request("mission.accept", params.clone());
    db.execute_batch("DROP TRIGGER hold_reconciled_acceptance")
        .unwrap();
    assert_eq!(failed.unwrap_err()["code"], "STORAGE_UNAVAILABLE");
    let current = snapshot(client, params["mission_id"].as_str().unwrap());
    assert_eq!(mission(&current).state, MissionState::Running);
    assert_eq!(
        mission(&current).revision.get().to_string(),
        params["expected_revision"].as_str().unwrap()
    );
    assert_eq!(run(&current, &unknown.id), *unknown);
}

pub(super) fn assert_acceptance_evidence(
    daemon: &DaemonProc,
    client: &mut Client,
    mission_id: &str,
    unknown: &Run,
) {
    let db = rusqlite::Connection::open(daemon.data_dir.join("data/iyagi.db")).unwrap();
    let (count, document): (usize, String) = db.query_row("SELECT count(*), payload_json FROM orch_events WHERE mission_id=?1 AND event_type='accepted'", [mission_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(count, 1);
    let event: Value = serde_json::from_str(&document).unwrap();
    let artifact = &event["changes_ref"];
    assert!(
        artifact["id"].is_string(),
        "accepted event must retain review evidence: {event}"
    );
    let body = client
        .request(
            "artifact.read",
            json!({"artifact_id": artifact["id"], "offset": "0", "max_bytes": 4096}),
        )
        .unwrap();
    use base64::Engine;
    let evidence: Value = serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(body["data_b64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(evidence["kind"], "acceptance_reconciliation_review");
    assert_eq!(evidence["reconciliations"][0]["run_id"], json!(unknown.id));
    assert_eq!(
        evidence["reconciliations"][0]["termination_ref"],
        json!(unknown.reconciliation_ref)
    );
    assert!(evidence["reconciliations"][0]["replacement_run_id"].is_string());
    let current = snapshot(client, mission_id);
    assert_eq!(run(&current, &unknown.id), *unknown);
}
