//! Opt-in paid inference through the production daemon, IPC, SQLite and Git.
use super::*;
use std::{collections::HashSet, path::Path, time::Instant};
use term_contracts::mission::types::*;

fn snapshot(client: &mut Client, id: &str) -> Vec<Entity> {
    let mut entities = vec![];
    let mut snapshot_id = Value::Null;
    let mut cursor = Value::Null;
    loop {
        let page = client
            .request(
                "mission.snapshot",
                json!({"mission_id":id,"snapshot_id":snapshot_id,"cursor":cursor}),
            )
            .unwrap();
        entities.extend(serde_json::from_value::<Vec<Entity>>(page["entities"].clone()).unwrap());
        if page["next_cursor"].is_null() {
            break;
        }
        snapshot_id = page["snapshot_id"].clone();
        cursor = page["next_cursor"].clone();
    }
    entities
}
fn mission(entities: &[Entity]) -> &Mission {
    entities
        .iter()
        .find_map(|entity| {
            if let Entity::Mission(value) = entity {
                Some(value.as_ref())
            } else {
                None
            }
        })
        .unwrap()
}
fn read(client: &mut Client, reference: &ArtifactRef) -> Value {
    use base64::Engine;
    assert!(reference.bytes.get() <= 128 * 1024);
    let mut body = vec![];
    while body.len() < reference.bytes.get() as usize {
        let response = client.request("artifact.read", json!({"artifact_id":reference.id,"offset":body.len().to_string(),"max_bytes":4096})).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(response["data_b64"].as_str().unwrap())
            .unwrap();
        assert!(!bytes.is_empty());
        body.extend(bytes);
    }
    serde_json::from_slice(&body).unwrap_or_else(|_| json!(String::from_utf8_lossy(&body)))
}
fn allowed_file_change(question: &Value, run: &Run, entities: &[Entity]) -> bool {
    let Some(text) = question["question"].as_str() else {
        return false;
    };
    let Ok(details) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let Some(workspace) = entities.iter().find_map(|entity| match entity {
        Entity::Workspace(w)
            if Some(&w.id) == run.workspace_id.as_ref()
                && w.writer_run_id.as_ref() == Some(&run.id) =>
        {
            Some(w)
        }
        _ => None,
    }) else {
        return false;
    };
    let Some(task) = entities.iter().find_map(|entity| match entity {
        Entity::Task(t) if t.id == run.task_id => Some(t),
        _ => None,
    }) else {
        return false;
    };
    details["type"] == "file_change"
        && details["details_available"] == true
        && details["grant_root"].is_null()
        && details["changes"].as_array().is_some_and(|changes| {
            !changes.is_empty()
                && changes.iter().all(|change| {
                    let Some(path) = change["path"].as_str() else {
                        return false;
                    };
                    let path = Path::new(&workspace.path).join(path);
                    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                        return false;
                    };
                    ["sum.txt", "label.txt"].contains(&name)
                        && task
                            .contract
                            .allowed_paths
                            .iter()
                            .any(|allowed| allowed == name)
                        && change["kind"]["type"] == "add"
                        && !path.exists()
                        && path.parent().and_then(|parent| parent.canonicalize().ok())
                            == Path::new(&workspace.path).canonicalize().ok()
                })
        })
}
fn allowed_read_command(question: &Value, run: &Run, entities: &[Entity]) -> bool {
    let Some(workspace) = entities.iter().find_map(|entity| match entity {
        Entity::Workspace(w) if Some(&w.id) == run.workspace_id.as_ref() => Some(w),
        _ => None,
    }) else {
        return false;
    };
    let Some(text) = question["question"].as_str() else {
        return false;
    };
    let mut commands = vec![];
    for file in ["sum.txt", "label.txt"] {
        let regular = std::fs::symlink_metadata(Path::new(&workspace.path).join(file))
            .is_ok_and(|metadata| metadata.file_type().is_file());
        if !regular {
            continue;
        }
        commands.extend([format!("od -An -t x1 {file}"), format!("cat {file}")]);
    }
    if let Some(candidate) = entities.iter().find_map(|entity| match entity {
        Entity::Candidate(candidate)
            if Some(&candidate.id) == mission(entities).candidate_id.as_ref()
                && candidate.commit_oid == workspace.base_oid =>
        {
            Some(candidate)
        }
        _ => None,
    }) {
        commands.push(format!(
            "git diff --no-ext-diff --name-only {} {}",
            candidate.base_oid, candidate.commit_oid
        ));
    }
    for command in commands {
        for shell in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
            if text
                == format!(
                    "command approval (command): {shell} -c '{command}' @ {}",
                    workspace.path
                )
            {
                return true;
            }
        }
    }
    false
}

fn summary(entities: &[Entity]) -> Value {
    json!({"state":mission(entities).state,"phase":mission(entities).phase,
        "runs":entities.iter().filter_map(|e| if let Entity::Run(r)=e {Some(json!({"id":r.id,"task_id":r.task_id,"state":r.state,"failure_code":r.failure_code,"requested_model":r.requested_model,"observed_model":r.observed_model,"exec_id":r.exec_id}))}else{None}).collect::<Vec<_>>(),
        "tasks":entities.iter().filter_map(|e| if let Entity::Task(t)=e {Some(json!({"id":t.id,"kind":t.kind,"state":t.state,"blocked_code":t.blocked_code}))}else{None}).collect::<Vec<_>>()})
}

#[test]
#[ignore = "paid installed Codex full mission; explicit IYAGI_CODEX_BIN, IYAGI_CODEX_MISSION_EVIDENCE_OUT required"]
fn installed_codex_mission_reaches_verified_reviewed_acceptance() {
    let program = std::env::var("IYAGI_CODEX_BIN").expect("explicit installed Codex binary");
    let evidence =
        std::env::var("IYAGI_CODEX_MISSION_EVIDENCE_OUT").expect("explicit evidence output");
    let mut daemon = DaemonProc::spawn("codex-real-mission", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let repo = tempfile::tempdir().unwrap();
    run_git(repo.path(), &["init", "-q"]);
    run_git(
        repo.path(),
        &["config", "user.name", "iyagi compatibility test"],
    );
    run_git(repo.path(), &["config", "user.email", "test@iyagi.invalid"]);
    std::fs::write(
        repo.path().join("README.md"),
        "Owned live compatibility repository.\n",
    )
    .unwrap();
    run_git(repo.path(), &["add", "README.md"]);
    run_git(repo.path(), &["commit", "-qm", "base"]);
    let base = run_git(repo.path(), &["rev-parse", "HEAD"]);
    let mut binding = iyagi_termd_lib::agent_runtime::fake::fake_binding();
    binding.runtime = RuntimeKind::Codex;
    binding.program = program;
    binding.provider_id = "openai".into();
    binding.model_id = "gpt-5.6-luna".into();
    binding.auth_route = AuthRoute::Subscription;
    binding.credential_ref = None;
    binding.endpoint_ref = None;
    binding.runtime_version = None;
    binding.checked_at = None;
    let binding_id = binding.id.to_string();
    client
        .request(
            "binding.save",
            json!({"request_id":uuid(),"expected_revision":"0","binding":binding}),
        )
        .unwrap();
    let probed = client
        .request("binding.probe", json!({"binding_id":binding_id}))
        .unwrap();
    assert_eq!(probed["binding"]["runtime_version"], "0.154.0");
    assert_eq!(
        probed["binding"]["capabilities"]["scoped_write"]["supported"],
        true
    );
    let goal = upload_artifact(&mut client, "text/plain", b"Create sum.txt containing exactly 42 followed by one newline, and label.txt containing exactly verified followed by one newline. Change no other files. Plan two independent required implement tasks with role builder, one for each file, allowed_paths containing only that exact file name, expected_outputs patch, and both linked to the supplied requirement and verification command IDs. The daemon performs integration, verification and independent review automatically; do not plan those phases yourself. Builders must create their file with apply_patch, then return a patch result. Reviewers must inspect the actual candidate files and return a review for the supplied candidate ID. For this compatibility test, the only permitted review shell commands are exactly `od -An -t x1 sum.txt`, `od -An -t x1 label.txt`, and `git diff --no-ext-diff --name-only BASE CANDIDATE` (substitute the supplied candidate base_oid and commit_oid). Execute each command separately, without chaining, extra flags, or output formatting commands. Do not use network, install packages, run other agents, push, or publish.");
    let command_id = uuid();
    let mut params = create_params(goal, repo.path().to_str().unwrap(), &binding_id);
    params["title"] = json!("Installed Codex full mission");
    params["expected_base_oid"] = json!(base);
    params["requirements"][0]["text"] = json!("sum.txt is exactly 42 newline; label.txt is exactly verified newline; no other files change.");
    params["requirements"][0]["verification_ids"] = json!([command_id]);
    params["policy"]["allowed_verification_ids"] = json!([command_id]);
    params["policy"]["require_enforced_verification"] = json!(true);
    params["policy"]["max_automatic_starts"] = json!(8);
    params["policy"]["max_attempts_per_task"] = json!(2);
    params["policy"]["max_repair_cycles"] = json!(1);
    params["policy"]["max_parallel_runs"] = json!(2);
    params["policy"]["run_time_limit_ms"] = json!("120000");
    params["policy"]["active_time_limit_ms"] = json!("600000");
    let made = client.request("mission.create", params).unwrap();
    let id = made["mission_id"].as_str().unwrap().to_owned();
    let initial = snapshot(&mut client, &id);
    let python = std::process::Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let python = String::from_utf8(python.stdout).unwrap();
    client.request("verification.save", json!({"request_id":uuid(),"expected_revision":"0","command":{
        "id":command_id,"revision":"0","title":"Exact two-file contents","repository_id":mission(&initial).repository_id,
        "program":python.trim(),"argv":["-c","from pathlib import Path; assert Path('sum.txt').read_bytes() == b'42\\n'; assert Path('label.txt').read_bytes() == b'verified\\n'"],
        "cwd_relative":"","timeout_ms":10000,"env_profile_ref":null,"allowed_network":false}})).unwrap();
    let mut approvals = vec![];
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.request("mission.control", json!({"request_id":uuid(),"mission_id":id,"expected_revision":"1","action":"start"})).unwrap();
        let deadline = Instant::now() + Duration::from_secs(360);
        let mut seen = HashSet::new();
        let mut last = String::new();
        loop {
            let entities = snapshot(&mut client, &id);
            let m = mission(&entities);
            let progress = summary(&entities).to_string();
            if progress != last {
                eprintln!("MISSION_PROGRESS {progress}");
                last = progress;
            }
            assert!(Instant::now() < deadline, "full mission timed out");
            assert!(
                !matches!(
                    m.state,
                    MissionState::Completed | MissionState::Failed | MissionState::Cancelled
                ),
                "mission stopped before acceptance"
            );
            if m.phase == Phase::AwaitingAcceptance {
                let candidate = entities
                    .iter()
                    .find_map(|e| match e {
                        Entity::Candidate(c) if Some(&c.id) == m.candidate_id.as_ref() => Some(c),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(
                    run_git(
                        repo.path(),
                        &["show", &format!("{}:sum.txt", candidate.commit_oid)]
                    ),
                    "42"
                );
                assert_eq!(
                    run_git(
                        repo.path(),
                        &["show", &format!("{}:label.txt", candidate.commit_oid)]
                    ),
                    "verified"
                );
                assert_eq!(
                    run_git(
                        repo.path(),
                        &["diff", "--name-only", &base, &candidate.commit_oid]
                    ),
                    "label.txt\nsum.txt"
                );
                let checks: Vec<_> = entities
                    .iter()
                    .filter_map(|e| match e {
                        Entity::Verification(v) if v.candidate_id == candidate.id => Some(v),
                        _ => None,
                    })
                    .collect();
                assert!(
                    !checks.is_empty()
                        && checks
                            .iter()
                            .all(|v| v.status == VerificationStatus::Passed)
                );
                assert!(checks
                    .iter()
                    .all(|v| v.input_integrity == InputIntegrity::Enforced));
                assert!(entities.iter().any(|e| matches!(e, Entity::Task(t) if t.kind==TaskKind::Review && t.state==TaskState::Succeeded)));
                let accept = json!({"request_id":uuid(),"mission_id":id,"expected_revision":m.revision,"candidate_id":candidate.id,"acknowledged_verification_ids":checks.iter().map(|v|&v.id).collect::<Vec<_>>(),"human_requirement_ids":[]});
                let receipt = client.request("mission.accept", accept.clone()).unwrap();
                assert_eq!(client.request("mission.accept", accept).unwrap(), receipt);
                break;
            }
            if let Some(decision) = entities.iter().find_map(|e| match e {
                Entity::Decision(d) if d.state == DecisionState::Open && !seen.contains(&d.id) => {
                    Some(d)
                }
                _ => None,
            }) {
                let question = read(&mut client, &decision.question_ref);
                assert_eq!(
                    decision.kind,
                    DecisionKind::Approval,
                    "unexpected decision: {question}"
                );
                let run = entities
                    .iter()
                    .find_map(|e| match e {
                        Entity::Run(r) if Some(&r.id) == decision.requesting_run_id.as_ref() => {
                            Some(r)
                        }
                        _ => None,
                    })
                    .unwrap();
                assert!(
                    allowed_file_change(&question, run, &entities)
                        || allowed_read_command(&question, run, &entities),
                    "approval has no verified owned-file changes: {question}"
                );
                let answer = json!({"request_id":uuid(),"mission_id":id,"expected_revision":m.revision,"decision_id":decision.id,"option_id":"accept","answer_ref":null});
                match client.request("mission.decision.answer", answer) {
                    Ok(_) => {
                        seen.insert(decision.id.clone());
                        approvals.push(question);
                    }
                    Err(error) => assert_eq!(error["code"], "REVISION_CONFLICT"),
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }));
    // Every test outcome drains the owned mission before the daemon is stopped.
    let deadline = Instant::now() + Duration::from_secs(40);
    let final_entities = loop {
        let entities = snapshot(&mut client, &id);
        let m = mission(&entities);
        if !matches!(
            m.state,
            MissionState::Completed | MissionState::Failed | MissionState::Cancelled
        ) && m.state != MissionState::Stopping
        {
            let _ = client.request("mission.control",json!({"request_id":uuid(),"mission_id":id,"expected_revision":m.revision,"action":"cancel"}));
        }
        if matches!(
            m.state,
            MissionState::Completed | MissionState::Failed | MissionState::Cancelled
        ) && entities
            .iter()
            .all(|e| !matches!(e, Entity::Exec(exec) if exec.state != ExecState::Exited))
            && entities
                .iter()
                .all(|e| !matches!(e, Entity::Run(run) if run.holds_execution_slot()))
        {
            break entities;
        }
        assert!(
            Instant::now() < deadline,
            "owned mission cleanup timed out; daemon data retained at {:?}",
            daemon.data_dir
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let accepted = mission(&final_entities).state == MissionState::Completed;
    let providers: Vec<_> = final_entities
        .iter()
        .filter_map(|e| match e {
            Entity::Run(r) if r.binding_snapshot.is_some() => Some(r),
            _ => None,
        })
        .collect();
    let exact_models = providers.len() >= 4
        && providers.iter().all(|r| {
            r.requested_model.as_deref() == Some("gpt-5.6-luna")
                && r.observed_model.as_deref() == Some("gpt-5.6-luna")
                && r.state == RunState::Succeeded
        });
    let original_unchanged = run_git(repo.path(), &["rev-parse", "HEAD"]) == base
        && run_git(repo.path(), &["status", "--porcelain"]).is_empty();
    let report = json!({"format":1,"runtime":"codex","version":"0.154.0","os":"macos","arch":"aarch64","provider":"openai","auth_route":"subscription","model":"gpt-5.6-luna",
        "tested_at":term_storage::time::now_iso8601(),"scope":"production daemon + IPC + SQLite + Git + native Exec + real model inference; two independent writers, verification, review and acceptance",
        "passed":outcome.is_ok() && accepted && exact_models && original_unchanged,"accepted":accepted,"exact_models":exact_models,"original_unchanged":original_unchanged,"cleanup":true,
        "summary":summary(&final_entities),"approvals":approvals,
        "require_enforced_verification":mission(&final_entities).policy.require_enforced_verification,
        "verifications":final_entities.iter().filter_map(|e| match e {
            Entity::Verification(v) => Some(json!({"id":v.id,"candidate_id":v.candidate_id,"run_id":v.run_id,
                "status":v.status,"input_integrity":v.input_integrity,"exit_code":v.exit_code})), _=>None,
        }).collect::<Vec<_>>()});
    std::fs::write(evidence, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    eprintln!("MISSION_DATA {}", daemon.data_dir.display());
    daemon.kill();
    assert_eq!(
        report["passed"], true,
        "full mission failed; inspect evidence and retained daemon artifacts"
    );
}
