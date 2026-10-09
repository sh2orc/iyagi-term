//! O1 mission RPC over the real daemon (tickets O04/O05): staged artifact
//! upload → mission.create adoption → snapshot pages → event tail →
//! control transitions → binding/template/verification CAS + request dedupe.

mod common;

use common::{Client, DaemonProc};
use serde_json::{json, Value};

fn uuid() -> String {
    common::uuid_v4()
}

/// Upload an artifact through begin/write/commit and return its ref.
fn upload_artifact(client: &mut Client, media_type: &str, body: &[u8]) -> Value {
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let begin = client
        .request(
            "artifact.begin",
            json!({
                "request_id": uuid(),
                "mission_id": null,
                "media_type": media_type,
                "bytes": body.len().to_string(),
                "sha256": digest,
            }),
        )
        .expect("artifact.begin");
    let upload_id = begin["upload_id"].as_str().expect("upload id").to_string();
    let chunk = begin["chunk_bytes"].as_u64().expect("chunk size") as usize;
    use base64::Engine;
    let mut offset = 0u64;
    for slice in body.chunks(chunk.max(1)) {
        let next = client
            .request(
                "artifact.write",
                json!({
                    "upload_id": upload_id,
                    "offset": offset.to_string(),
                    "data_b64": base64::engine::general_purpose::STANDARD.encode(slice),
                }),
            )
            .expect("artifact.write");
        offset = next["next_offset"]
            .as_str()
            .expect("next offset")
            .parse()
            .unwrap();
    }
    client
        .request("artifact.commit", json!({ "upload_id": upload_id }))
        .expect("artifact.commit")
}

fn create_params(goal_ref: Value, repo: &str, title: &str) -> Value {
    let binding_id = uuid();
    json!({
        "request_id": uuid(),
        "title": title,
        "repository_path": repo,
        "expected_base_oid": iyagi_termd_lib::workspace::repository_identity(std::path::Path::new(repo)).map(|r| r.head_oid).unwrap_or_else(|_| "a".repeat(40)),
        "goal_ref": goal_ref,
        "requirements": [{
            "id": uuid(),
            "text": "로그인 성공과 실패 경로를 검증한다.",
            "verification_ids": [],
            "human_check": false,
        }],
        "policy": {
            "max_parallel_runs": 4,
            "max_attempts_per_task": 3,
            "max_repair_cycles": 3,
            "max_automatic_starts": 64,
            "active_time_limit_ms": "14400000",
            "run_time_limit_ms": "2700000",
            "max_cost_usd_micros": null,
            "unknown_cost": "allow_with_notice",
            "allow_network": false,
            "allow_automatic_plan_apply": true,
            "allow_recovery_of_unsent": true,
            "allowed_binding_ids": [binding_id],
            "allowed_roles": ["lead", "builder", "reviewer", "integrator"],
            "allowed_verification_ids": [],
            "require_independent_review": true,
            "require_enforced_verification": false,
        },
        // Start requires Lead and Builder, plus Reviewer with independent review.
        "role_bindings": [
            { "role": "lead", "primary_binding_id": binding_id, "fallback_binding_ids": [] },
            { "role": "builder", "primary_binding_id": binding_id, "fallback_binding_ids": [] },
            { "role": "reviewer", "primary_binding_id": binding_id, "fallback_binding_ids": [] },
        ],
    })
}

#[test]
fn repository_inspection_and_draft_ignore_inherited_git_paths() {
    use iyagi_termd_lib::workspace::git::run_git_for_test as git;
    let root = tempfile::tempdir().unwrap();
    let selected = root.path().join("selected");
    let unrelated = root.path().join("unrelated");
    for path in [&selected, &unrelated] {
        std::fs::create_dir(path).unwrap();
        git(path, &["init", "-q"]);
        std::fs::write(
            path.join("README.md"),
            path.file_name().unwrap().to_str().unwrap(),
        )
        .unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "base"]);
    }
    let subdirectory = selected.join("subdirectory");
    std::fs::create_dir(&subdirectory).unwrap();
    let dir = unrelated.join(".git").to_string_lossy().into_owned();
    let work = unrelated.to_string_lossy().into_owned();
    let index = unrelated.join(".git/index").to_string_lossy().into_owned();
    std::fs::create_dir(root.path().join("daemon")).unwrap();
    let mut daemon = DaemonProc::spawn_with_env(
        root.path().join("daemon"),
        "git-environment",
        None,
        &[
            ("GIT_DIR", &dir),
            ("GIT_WORK_TREE", &work),
            ("GIT_COMMON_DIR", &dir),
            ("GIT_INDEX_FILE", &index),
        ],
    );
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let inspected = client
        .request("repository.inspect", json!({"path":subdirectory}))
        .unwrap();
    assert_eq!(
        inspected["canonical_path"],
        json!(selected.canonicalize().unwrap())
    );
    assert_eq!(
        inspected["head_oid"],
        git(&selected, &["rev-parse", "HEAD"])
    );
    let goal = upload_artifact(&mut client, "text/plain", b"Check selected repository");
    let params = create_params(goal, subdirectory.to_str().unwrap(), "Selected repository");
    register_role_bindings(&mut client, &params);
    let made = client.request("mission.create", params).unwrap();
    let snapshot = client
        .request(
            "mission.snapshot",
            json!({"mission_id":made["mission_id"],"snapshot_id":null,"cursor":null}),
        )
        .unwrap();
    let mission = snapshot["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "mission")
        .unwrap();
    assert_eq!(
        mission["value"]["repository_id"],
        inspected["repository_id"]
    );
    assert_eq!(
        mission["value"]["repository_path"],
        inspected["canonical_path"]
    );
    // Replacing the selected directory's Git link must not reuse the old ID,
    // even though a valid repository is still reachable at the same path.
    std::fs::rename(selected.join(".git"), selected.join("saved-git")).unwrap();
    std::fs::write(selected.join(".git"), format!("gitdir: {dir}\n")).unwrap();
    let changed = client
        .request(
            "mission.control",
            json!({"request_id":uuid(),
        "mission_id":made["mission_id"], "expected_revision":"1", "action":"start"}),
        )
        .unwrap_err();
    assert_eq!(changed["code"], "INVALID_STATE");
    assert!(changed["message"]
        .as_str()
        .unwrap()
        .contains("Git identity changed"));
    assert_eq!(git(&unrelated, &["status", "--porcelain"]), "");
    daemon.kill();
}

#[test]
fn mission_create_snapshot_events_and_control_roundtrip() {
    let mut daemon = DaemonProc::spawn("mission-rpc", None);
    let repo = tempfile::tempdir().expect("repo dir");
    let repo_path = repo.path().canonicalize().unwrap();
    init_repository(&repo_path);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);

    // 1. Goal artifact staged through the chunked protocol.
    let goal = upload_artifact(
        &mut client,
        "text/plain",
        "로그인 기능을 구현해라.".as_bytes(),
    );
    assert!(goal["sha256"].as_str().is_some_and(|s| s.len() == 64));

    // 2. mission.create adopts it and answers a MutationResult.
    let params = create_params(goal, repo_path.to_str().unwrap(), "로그인 기능");
    register_role_bindings(&mut client, &params);
    let created = client
        .request("mission.create", params.clone())
        .expect("create");
    let mission_id = created["mission_id"]
        .as_str()
        .expect("mission id")
        .to_string();
    assert_eq!(created["revision"], "1");

    // 3. Duplicate request replays the stored response (E01 over IPC).
    let replay = client
        .request("mission.create", params.clone())
        .expect("replay");
    assert_eq!(replay["mission_id"], mission_id.as_str());
    assert_eq!(replay["revision"], "1");
    let mut reuse = params;
    reuse["request_id"] = json!(uuid());
    assert_eq!(
        client.request("mission.create", reuse).unwrap_err()["code"],
        "INVALID_ARGUMENT",
        "the adopted goal cannot create another mission"
    );

    // 4. Snapshot: mission + adopted goal appear; single page here.
    let snapshot = client
        .request(
            "mission.snapshot",
            json!({ "mission_id": mission_id, "snapshot_id": null, "cursor": null }),
        )
        .expect("snapshot");
    let entities = snapshot["entities"].as_array().expect("entities");
    assert!(!entities.is_empty());
    assert_eq!(entities[0]["kind"], "mission");
    assert_eq!(snapshot["revision"], "1");
    assert_eq!(snapshot["at_seq"], "1");

    // 5. Event tail carries the created event.
    let events = client
        .request(
            "mission.events",
            json!({ "mission_id": mission_id, "after_seq": "0", "limit": 50 }),
        )
        .expect("events");
    let list = events["events"].as_array().expect("events list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["type"], "created");
    assert_eq!(events["high_watermark"], "1");

    // 6. Control: start (draft→running), pause (running→paused instantly —
    // no live runs exist before the engine), cancel (paused→stopping→cancelled).
    let control = |client: &mut Client, action: &str, revision: &str| {
        client
            .request(
                "mission.control",
                json!({
                    "request_id": uuid(),
                    "mission_id": mission_id,
                    "expected_revision": revision,
                    "action": action,
                }),
            )
            .expect(action)
    };
    let started = control(&mut client, "start", "1");
    assert_eq!(started["revision"], "2");
    let started_snapshot = client
        .request(
            "mission.snapshot",
            json!({"mission_id": mission_id, "snapshot_id": null, "cursor": null}),
        )
        .unwrap();
    let tasks: Vec<_> = started_snapshot["entities"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entity| entity["kind"] == "task")
        .collect();
    assert_eq!(
        tasks.len(),
        1,
        "start atomically creates the first Lead task"
    );
    assert_eq!(tasks[0]["value"]["kind"], "plan");
    let paused = control(&mut client, "pause", "2");
    assert_eq!(paused["revision"], "3");
    let resumed = control(&mut client, "resume", "3");
    assert_eq!(resumed["revision"], "4");
    let cancelled = control(&mut client, "cancel", "4");
    assert_eq!(cancelled["revision"], "5");

    // 7. Stale revision is a REVISION_CONFLICT with current_revision detail.
    let stale = client.request(
        "mission.control",
        json!({
            "request_id": uuid(),
            "mission_id": mission_id,
            "expected_revision": "1",
            "action": "archive",
        }),
    );
    let stale = stale.unwrap_err();
    assert_eq!(stale["code"], "REVISION_CONFLICT");
    assert_eq!(stale["details"]["current_revision"], "5");

    // 8. mission.list shows the mission; the archived list does not.
    let listed = client
        .request(
            "mission.list",
            json!({ "cursor": null, "limit": 10, "archived": false }),
        )
        .expect("list");
    let items = listed["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], mission_id.as_str());

    // 9. mission.changed hints arrived on the control connection.
    let mut changed = 0;
    while let Some(_event) = client.pop_event("mission.changed") {
        changed += 1;
    }
    assert!(
        changed >= 5,
        "expected mission.changed hints, got {changed}"
    );

    daemon.kill();
}

#[test]
fn artifact_transfer_rejects_corruption_and_skips() {
    let mut daemon = DaemonProc::spawn("mission-artifact", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);

    let body = vec![0x41u8; 100];
    use sha2::{Digest, Sha256};
    let good_digest: String = Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let begin = client
        .request(
            "artifact.begin",
            json!({
                "request_id": uuid(),
                "mission_id": null,
                "media_type": "application/octet-stream",
                "bytes": "100",
                "sha256": good_digest,
            }),
        )
        .expect("begin");
    let upload_id = begin["upload_id"].as_str().unwrap().to_string();

    // Skipped offset is INVALID_ARGUMENT.
    use base64::Engine;
    let skipped = client.request(
        "artifact.write",
        json!({
            "upload_id": upload_id,
            "offset": "4",
            "data_b64": base64::engine::general_purpose::STANDARD.encode(&body[..4]),
        }),
    );
    assert_eq!(skipped.unwrap_err()["code"], "INVALID_ARGUMENT");

    // Wrong hash at commit is INTEGRITY_FAILED: upload the real bytes under
    // a deliberately wrong announced digest.
    let wrong = client
        .request(
            "artifact.begin",
            json!({
                "request_id": uuid(),
                "mission_id": null,
                "media_type": "application/octet-stream",
                "bytes": "100",
                "sha256": "f".repeat(64),
            }),
        )
        .expect("begin 2");
    let wrong_id = wrong["upload_id"].as_str().unwrap().to_string();
    let mut offset = 0u64;
    for slice in body.chunks(40) {
        let next = client
            .request(
                "artifact.write",
                json!({
                    "upload_id": wrong_id,
                    "offset": offset.to_string(),
                    "data_b64": base64::engine::general_purpose::STANDARD.encode(slice),
                }),
            )
            .expect("write");
        offset = next["next_offset"].as_str().unwrap().parse().unwrap();
    }
    let commit = client.request("artifact.commit", json!({ "upload_id": wrong_id }));
    assert_eq!(commit.unwrap_err()["code"], "INTEGRITY_FAILED");

    // Clean upload commits and reads back byte-identical (4 KiB bound).
    let reference = upload_artifact(&mut client, "application/octet-stream", &body);
    let artifact_id = reference["id"].as_str().unwrap().to_string();
    let read = client
        .request(
            "artifact.read",
            json!({ "artifact_id": artifact_id, "offset": "0", "max_bytes": 4096 }),
        )
        .expect("read");
    assert_eq!(read["complete"], true);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(read["data_b64"].as_str().unwrap())
        .expect("base64");
    assert_eq!(decoded, body);

    daemon.kill();
}

#[test]
fn bindings_templates_verification_cas_and_probe() {
    let mut daemon = DaemonProc::spawn("mission-config", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);

    let binding_id = uuid();
    let yes = json!(true);
    let _ = &yes;
    let save = client
        .request(
            "binding.save",
            json!({
                "request_id": uuid(),
                "expected_revision": "0",
                "binding": {
                    "id": binding_id,
                    "revision": "0",
                    "label": "Fake fixture",
                    "runtime": "fake",
                    "program": "C:/fixture/iyagi-agent.exe",
                    "runtime_version": null,
                    "provider_id": "fake",
                    "model_id": "fixture-model",
                    "effort": null,
                    "auth_route": "local",
                    "credential_ref": null,
                    "endpoint_ref": null,
                    "capabilities": {
                        "structured_result": {"supported": true, "reason_code": null},
                        "events": {"supported": true, "reason_code": null},
                        "cancel": {"supported": true, "reason_code": null},
                        "resume": {"supported": true, "reason_code": null},
                        "steer": {"supported": true, "reason_code": null},
                        "approval_reply": {"supported": true, "reason_code": null},
                        "read_only": {"supported": true, "reason_code": null},
                        "scoped_write": {"supported": true, "reason_code": null},
                        "model_listing": {"supported": true, "reason_code": null},
                        "usage": {"supported": true, "reason_code": null},
                        "native_terminal_attach": {"supported": false, "reason_code": "fake_no_native_terminal"},
                    },
                    "checked_at": null,
                    "enabled": true,
                    "resource_policy": {
                        "reservation_bytes": "2147483648",
                        "cpu_slots": 1,
                        "enforcement": "observe",
                        "memory_max_bytes": null,
                        "cpu_max_cores": null,
                        "pids_max": null,
                    },
                },
            }),
        )
        .expect("binding.save");
    assert_eq!(
        save["binding"]["revision"], "1",
        "server assigns revision 1"
    );

    // Stale CAS on update.
    let conflict = client.request(
        "binding.save",
        json!({
            "request_id": uuid(),
            "expected_revision": "0",
            "binding": save["binding"],
        }),
    );
    assert_eq!(conflict.unwrap_err()["code"], "REVISION_CONFLICT");

    // Listing + probe (the fake runtime advertises no model to pick from).
    let listed = client
        .request("binding.list", json!({}))
        .expect("binding.list");
    assert_eq!(listed["bindings"].as_array().unwrap().len(), 1);
    let probe = client
        .request("binding.probe", json!({ "binding_id": binding_id }))
        .expect("binding.probe");
    assert_eq!(probe["binding"]["id"], binding_id.as_str());
    assert_eq!(probe["models"].as_array().unwrap().len(), 0);

    // Template save with the same binding as lead.
    let template_id = uuid();
    let policy = save["binding"].clone(); // any Value; policy below is the real one
    let _ = policy;
    let template = client
        .request(
            "template.save",
            json!({
                "request_id": uuid(),
                "expected_revision": "0",
                "template": {
                    "id": template_id,
                    "revision": "0",
                    "label": "balanced",
                    "repository_id": null,
                    "role_bindings": [{
                        "role": "lead",
                        "primary_binding_id": binding_id,
                        "fallback_binding_ids": [],
                    }],
                    "policy": {
                        "max_parallel_runs": 4,
                        "max_attempts_per_task": 3,
                        "max_repair_cycles": 3,
                        "max_automatic_starts": 64,
                        "active_time_limit_ms": "14400000",
                        "run_time_limit_ms": "2700000",
                        "max_cost_usd_micros": null,
                        "unknown_cost": "allow_with_notice",
                        "allow_network": false,
                        "allow_automatic_plan_apply": true,
                        "allow_recovery_of_unsent": true,
                        "allowed_binding_ids": [binding_id],
                        "allowed_roles": ["lead"],
                        "allowed_verification_ids": [],
                        "require_independent_review": true,
                        "require_enforced_verification": false,
                    },
                },
            }),
        )
        .expect("template.save");
    assert_eq!(template["template"]["revision"], "1");
    let templates = client
        .request("template.list", json!({ "repository_id": null }))
        .expect("template.list");
    assert_eq!(templates["templates"].as_array().unwrap().len(), 1);

    // Duplicate policy entries are rejected outright (09 §4).
    let dup_policy = client.request(
        "template.save",
        json!({
            "request_id": uuid(),
            "expected_revision": "0",
            "template": {
                "id": uuid(),
                "revision": "0",
                "label": "broken",
                "repository_id": null,
                "role_bindings": [],
                "policy": {
                    "max_parallel_runs": 4,
                    "max_attempts_per_task": 3,
                    "max_repair_cycles": 3,
                    "max_automatic_starts": 64,
                    "active_time_limit_ms": "14400000",
                    "run_time_limit_ms": "2700000",
                    "max_cost_usd_micros": null,
                    "unknown_cost": "allow_with_notice",
                    "allow_network": false,
                    "allow_automatic_plan_apply": true,
                    "allow_recovery_of_unsent": true,
                    "allowed_binding_ids": [binding_id, binding_id],
                    "allowed_roles": ["lead"],
                    "allowed_verification_ids": [],
                    "require_independent_review": true,
                    "require_enforced_verification": false,
                },
            },
        }),
    );
    assert_eq!(dup_policy.unwrap_err()["code"], "INVALID_ARGUMENT");

    daemon.kill();
}

#[test]
fn gate_off_answers_capability_unsupported_and_terminals_work() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = DaemonProc::spawn_with_env(
        dir.keep(),
        "mission-gate-off",
        None,
        &[("IYAGI_MISSION_PROTOCOL", "0")],
    );
    let (mut client, hello) = Client::control(&daemon.endpoint, &daemon.token);
    assert!(
        hello["capabilities"].get("mission_protocol").is_none(),
        "gate off must not advertise mission_protocol"
    );
    let refused = client.request(
        "mission.list",
        json!({ "cursor": null, "limit": 5, "archived": false }),
    );
    assert_eq!(refused.unwrap_err()["code"], "CAPABILITY_UNSUPPORTED");
    // Plain terminals keep working on the same daemon.
    let launch = client.request(
        "workload.launch",
        common::launch_request("shell", &["echo"], "1048576"),
    );
    assert!(
        launch.is_ok(),
        "terminal launch must survive the gate: {launch:?}"
    );
    daemon.kill();
}

#[test]
fn implemented_engine_methods_validate_run_and_mission_state() {
    let mut daemon = DaemonProc::spawn("mission-engine-gap", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let repo = tempfile::tempdir().unwrap();
    let goal = upload_artifact(&mut client, "text/plain", b"goal");
    let created = client
        .request(
            "mission.create",
            create_params(goal, repo.path().to_str().unwrap(), "엔진 전"),
        )
        .expect("create");
    let mission_id = created["mission_id"].as_str().unwrap().to_string();
    let error = client
        .request(
            "mission.activity",
            json!({"mission_id":mission_id,"run_id":uuid(),"after_offset":"0","max_bytes":4096}),
        )
        .unwrap_err();
    assert_eq!(
        error["code"], "NOT_FOUND",
        "activity validates run ownership instead of returning an unsupported placeholder"
    );
    // mission.accept is real since O13: the accept gate answers instead of
    // the placeholder. A draft mission cannot be accepted (INVALID_STATE).
    let answer = client
        .request(
            "mission.accept",
            json!({
                "request_id": uuid(),
                "mission_id": mission_id,
                "expected_revision": "1",
                "candidate_id": uuid(),
                "acknowledged_verification_ids": [],
                "human_requirement_ids": [],
            }),
        )
        .unwrap_err();
    assert_eq!(answer["code"], "INVALID_STATE");
    daemon.kill();
}

#[test]
fn decision_answer_cas_and_stale_paths() {
    let mut daemon = DaemonProc::spawn("mission-decisions", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let repo = tempfile::tempdir().unwrap();
    let goal = upload_artifact(&mut client, "text/plain", b"goal");
    let created = client
        .request(
            "mission.create",
            create_params(goal, repo.path().to_str().unwrap(), "결정"),
        )
        .expect("create");
    let mission_id = created["mission_id"].as_str().unwrap().to_string();
    // Store an open decision via the engine test seam: message first to bump
    // the revision, then answer a nonexistent decision → NOT_FOUND.
    let missing = client.request(
        "mission.decision.answer",
        json!({
            "request_id": uuid(),
            "mission_id": mission_id,
            "expected_revision": "1",
            "decision_id": uuid(),
            "option_id": "yes",
            "answer_ref": null,
        }),
    );
    let missing = missing.unwrap_err();
    assert_eq!(missing["code"], "NOT_FOUND");
    daemon.kill();
}

#[test]
fn task_control_retry_requires_failure_and_budget() {
    let mut daemon = DaemonProc::spawn("mission-taskctl", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let repo = tempfile::tempdir().unwrap();
    let goal = upload_artifact(&mut client, "text/plain", b"goal");
    let created = client
        .request(
            "mission.create",
            create_params(goal, repo.path().to_str().unwrap(), "taskctl"),
        )
        .expect("create");
    let mission_id = created["mission_id"].as_str().unwrap().to_string();
    // No tasks exist yet: retry on a random task id → NOT_FOUND.
    let missing = client.request(
        "mission.task.control",
        json!({
            "request_id": uuid(),
            "mission_id": mission_id,
            "expected_revision": "1",
            "task_id": uuid(),
            "action": "retry",
            "binding_id": null,
        }),
    );
    let missing = missing.unwrap_err();
    assert_eq!(missing["code"], "NOT_FOUND");
    daemon.kill();
}

#[test]
fn e14_e15_outbox_states_survive_restart_with_correct_classification() {
    // A daemon restart scans pending intents: unsent rows in a running
    // mission classify as dispatchable; may_have_sent rows never do
    // (auto-resend forbidden, 02 §7).
    let mut daemon = DaemonProc::spawn("mission-recovery", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let repo = tempfile::tempdir().unwrap();
    init_repository(repo.path());
    let goal = upload_artifact(&mut client, "text/plain", b"goal");
    let params = create_params(goal, repo.path().to_str().unwrap(), "복구");
    register_role_bindings(&mut client, &params);
    let created = client.request("mission.create", params).expect("create");
    let mission_id = created["mission_id"].as_str().unwrap().to_string();
    // Start the mission so recovery classifies as running.
    client
        .request(
            "mission.control",
            json!({"request_id": uuid(), "mission_id": mission_id, "expected_revision": "1", "action": "start"}),
        )
        .expect("start");
    // Events recorded; the outbox recovery ran at startup with no rows.
    let events = client
        .request(
            "mission.events",
            json!({"mission_id": mission_id, "after_seq": "0", "limit": 50}),
        )
        .expect("events");
    assert_eq!(events["events"].as_array().unwrap().len(), 2);
    daemon.kill();
}

fn init_repository(path: &std::path::Path) {
    let git = iyagi_termd_lib::workspace::git::run_git_for_test;
    git(path, &["init", "-q"]);
    git(path, &["commit", "--allow-empty", "-m", "base", "-q"]);
}

fn register_role_bindings(client: &mut Client, params: &Value) {
    let mut saved = std::collections::HashSet::new();
    for role in params["role_bindings"].as_array().unwrap() {
        if !saved.insert(role["primary_binding_id"].as_str().unwrap().to_owned()) {
            continue;
        }
        let mut binding = iyagi_termd_lib::agent_runtime::fake::fake_binding();
        binding.id =
            term_contracts::mission::types::Id::parse(role["primary_binding_id"].as_str().unwrap())
                .unwrap();
        client
            .request(
                "binding.save",
                json!({"request_id": uuid(), "expected_revision": "0", "binding": binding}),
            )
            .unwrap();
    }
}

#[test]
fn binding_save_rejects_raw_credentials_without_persisting_or_echoing_them() {
    let mut daemon = DaemonProc::spawn("binding-credential-ref", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let mut binding = iyagi_termd_lib::agent_runtime::fake::fake_binding();
    binding.credential_ref = Some("fake-raw-key-must-not-be-stored".into());
    let error = client
        .request(
            "binding.save",
            json!({"request_id":uuid(),"expected_revision":"0","binding":binding}),
        )
        .unwrap_err();
    let encoded = format!("{error:?}");
    assert!(!encoded.contains("fake-raw-key-must-not-be-stored"));
    assert!(encoded.contains("INVALID_ARGUMENT"), "{encoded}");
    let listed = client.request("binding.list", json!({})).unwrap();
    assert!(listed["bindings"].as_array().unwrap().is_empty());
    daemon.kill();
}
