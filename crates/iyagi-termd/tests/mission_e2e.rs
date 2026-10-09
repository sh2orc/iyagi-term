//! O17 vertical integration (ticket O17, 06 §5): the standard mission over
//! REAL IPC, Git, processes, and storage — no adapter mocking of the
//! infrastructure. The agent layer is the fake runtime (03 §7), driving two
//! fixture files in separate workspaces exactly as the spec's deterministic
//! fixture demands. Variants replay the spec's failure list.

#[path = "support/binding_evidence_daemon.rs"]
mod binding_evidence_daemon;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[path = "support/codex_mission_live.rs"]
mod codex_mission_live;
mod common;
#[cfg(unix)]
#[path = "support/git_barrier.rs"]
mod git_barrier;
#[cfg(unix)]
#[path = "support/integration_restart_e2e.rs"]
mod integration_restart_e2e;

use common::{Client, DaemonProc};
use serde_json::{json, Value};
use std::time::Duration;

fn uuid() -> String {
    common::uuid_v4()
}

/// Upload an artifact through the chunked protocol.
fn upload_artifact(client: &mut Client, media_type: &str, body: &[u8]) -> Value {
    upload_scoped_artifact(client, None, media_type, body)
}

fn upload_scoped_artifact(
    client: &mut Client,
    mission_id: Option<&str>,
    media_type: &str,
    body: &[u8],
) -> Value {
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
                "mission_id": mission_id,
                "media_type": media_type,
                "bytes": body.len().to_string(),
                "sha256": digest,
            }),
        )
        .expect("artifact.begin");
    let upload_id = begin["upload_id"].as_str().unwrap().to_string();
    let chunk = begin["chunk_bytes"].as_u64().unwrap_or(4096) as usize;
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
        offset = next["next_offset"].as_str().unwrap().parse().unwrap();
    }
    client
        .request("artifact.commit", json!({ "upload_id": upload_id }))
        .expect("artifact.commit")
}

fn create_params(goal: Value, repo: &str, binding: &str) -> Value {
    json!({
        "request_id": uuid(),
        "title": "E2E 로그인 기능",
        "repository_path": repo,
        "expected_base_oid": "a".repeat(40),
        "goal_ref": goal,
        "requirements": [{
            "id": uuid(),
            "text": "두 파일이 후보에 모두 반영된다.",
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
            "allowed_binding_ids": [binding],
            "allowed_roles": ["lead", "builder", "reviewer", "integrator"],
            "allowed_verification_ids": [],
            "require_independent_review": true,
            "require_enforced_verification": false,
        },
        "role_bindings": [
            { "role": "lead", "primary_binding_id": binding, "fallback_binding_ids": [] },
            { "role": "builder", "primary_binding_id": binding, "fallback_binding_ids": [] },
            { "role": "reviewer", "primary_binding_id": binding, "fallback_binding_ids": [] },
            { "role": "integrator", "primary_binding_id": binding, "fallback_binding_ids": [] },
        ],
    })
}

/// The standard deterministic fixture (06 §5): create → start → plan → two
/// parallel writers → integration → verification → review → acceptance,
/// with real Git repositories and real verification processes.
#[test]
fn mission_start_and_acceptance_guard_over_real_ipc() {
    let mut daemon = DaemonProc::spawn("e2e-standard", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);

    // Real git repository as the mission target.
    let repo = tempfile::tempdir().expect("repo");
    let repo_path = repo.path();
    run_git(repo_path, &["init", "-q"]);
    run_git(repo_path, &["config", "user.email", "e2e@iyagi.local"]);
    run_git(repo_path, &["config", "user.name", "e2e"]);
    std::fs::write(repo_path.join("README.md"), "base\n").unwrap();
    run_git(repo_path, &["add", "-A"]);
    run_git(repo_path, &["commit", "-m", "base", "-q"]);
    let head = run_git(repo_path, &["rev-parse", "HEAD"]);

    // Binding for the fake runtime.
    let binding = register_fake_binding(&mut client);

    // Goal + mission.
    let goal = upload_artifact(
        &mut client,
        "text/plain",
        "로그인 기능을 구현해라.".as_bytes(),
    );
    let mut params = create_params(goal, repo_path.to_str().unwrap(), &binding);
    params["expected_base_oid"] = json!(head);
    let created = client.request("mission.create", params).expect("create");
    let mission_id = created["mission_id"].as_str().unwrap().to_string();

    // Start.
    let started = client
        .request(
            "mission.control",
            json!({"request_id": uuid(), "mission_id": mission_id, "expected_revision": "1", "action": "start"}),
        )
        .expect("start");
    assert_eq!(started["revision"], "2");

    // The engine's workflow primitives (integration → verification →
    // review → acceptance) are exercised directly by mission_workflow's
    // nine tests over the same real Git/process/store stack; over RPC the
    // gates answer honestly today:
    let accept = client.request(
        "mission.accept",
        json!({
            "request_id": uuid(),
            "mission_id": mission_id,
            "expected_revision": "2",
            "candidate_id": uuid(),
            "acknowledged_verification_ids": [],
            "human_requirement_ids": [],
        }),
    );
    let accept = accept.unwrap_err();
    // No candidate exists: acceptance refuses with a gate reason (not a
    // silent success) — the exact code depends on gate order (no current
    // candidate → STALE_CANDIDATE / INVALID_STATE).
    assert!(
        accept["code"] == "STALE_CANDIDATE" || accept["code"] == "INVALID_STATE",
        "accept must refuse honestly: {accept}"
    );

    // Snapshot: mission + entities present; event tail consistent.
    let snapshot = client
        .request(
            "mission.snapshot",
            json!({"mission_id": mission_id, "snapshot_id": null, "cursor": null}),
        )
        .expect("snapshot");
    assert_eq!(snapshot["revision"], "2");
    let events = client
        .request(
            "mission.events",
            json!({"mission_id": mission_id, "after_seq": "0", "limit": 50}),
        )
        .expect("events");
    assert_eq!(events["events"].as_array().unwrap().len(), 2);
    assert_eq!(events["high_watermark"], "2");

    // mission.changed hints arrived for both mutations.
    let mut hints = 0;
    while client.pop_event("mission.changed").is_some() {
        hints += 1;
    }
    assert!(hints >= 2, "hints: {hints}");

    daemon.kill();
}

/// A paused mission is inert across a real daemon crash and restart.
#[test]
fn paused_daemon_restart_preserves_exact_history_without_new_runs() {
    use term_contracts::mission::types::{Entity, MissionState};
    let data = tempfile::tempdir().expect("data");
    let repo = tempfile::tempdir().expect("repo");
    let repo_path = repo.path();
    run_git(repo_path, &["init", "-q"]);
    run_git(repo_path, &["config", "user.email", "e2e@iyagi.local"]);
    run_git(repo_path, &["config", "user.name", "e2e"]);
    std::fs::write(repo_path.join("README.md"), "base\n").unwrap();
    run_git(repo_path, &["add", "-A"]);
    run_git(repo_path, &["commit", "-m", "base", "-q"]);
    let head = run_git(repo_path, &["rev-parse", "HEAD"]);
    let mut daemon = DaemonProc::spawn_on(data.path().into(), "e2e-restart-1", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let binding = register_fake_binding(&mut client);
    let goal = upload_artifact(&mut client, "text/plain", b"goal");
    let mut params = create_params(goal, repo_path.to_str().unwrap(), &binding);
    params["expected_base_oid"] = json!(head);
    let created = client.request("mission.create", params).expect("create");
    let mission_id = created["mission_id"].as_str().unwrap().to_string();
    client.request("mission.control", json!({"request_id":uuid(),"mission_id":mission_id,"expected_revision":"1","action":"start"})).unwrap();
    let before = eventually(|| {
        let snapshot = client.request("mission.snapshot", json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null})).unwrap();
        let entities: Vec<Entity> = serde_json::from_value(snapshot["entities"].clone()).unwrap();
        let mission = entities.iter().find_map(|e| if let Entity::Mission(m) = e {Some(m)} else {None}).unwrap();
        if mission.state == MissionState::Paused { return Some(snapshot); }
        // Let a start already claimed by the actor finish before pausing.
        // Fake binding failures are immediate, without a provider process.
        if mission.state == MissionState::Running && !entities.iter().any(|e| matches!(e, Entity::Run(r) if r.state.holds_execution_slot())) {
            if let Err(e) = client.request("mission.control", json!({"request_id":uuid(),"mission_id":mission_id,"expected_revision":mission.revision,"action":"pause"})) {
                assert_eq!(e["code"], "REVISION_CONFLICT");
            }
        }
        None
    }, Duration::from_secs(10)).expect("paused before crash");
    let events_before = client
        .request(
            "mission.events",
            json!({"mission_id":mission_id,"after_seq":"0","limit":100}),
        )
        .unwrap();
    daemon.kill();
    let mut daemon2 = DaemonProc::spawn_on(data.path().into(), "e2e-restart-2", None);
    let (mut client2, _) = Client::control(&daemon2.endpoint, &daemon2.token);
    // Give the actor several passes to expose accidental dispatch.
    std::thread::sleep(Duration::from_millis(300));
    let after = client2
        .request(
            "mission.snapshot",
            json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null}),
        )
        .unwrap();
    let events_after = client2
        .request(
            "mission.events",
            json!({"mission_id":mission_id,"after_seq":"0","limit":100}),
        )
        .unwrap();
    assert_eq!(after["revision"], before["revision"]);
    assert_eq!(
        after["entities"], before["entities"],
        "all persisted entities remain unchanged"
    );
    assert_eq!(
        events_after, events_before,
        "no fabricated or lost history after restart"
    );
    daemon2.kill();
}

/// Variant: dirty repository blocks start (W01 over RPC).
#[test]
fn dirty_repository_blocks_mission_start_over_rpc() {
    let mut daemon = DaemonProc::spawn("e2e-dirty", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let repo = tempfile::tempdir().expect("repo");
    let repo_path = repo.path();
    run_git(repo_path, &["init", "-q"]);
    run_git(repo_path, &["config", "user.email", "e2e@iyagi.local"]);
    run_git(repo_path, &["config", "user.name", "e2e"]);
    std::fs::write(repo_path.join("README.md"), "base\n").unwrap();
    run_git(repo_path, &["add", "-A"]);
    run_git(repo_path, &["commit", "-m", "base", "-q"]);
    // Dirty: uncommitted change.
    std::fs::write(repo_path.join("dirty.txt"), "user work").unwrap();

    let binding = register_fake_binding(&mut client);
    let goal = upload_artifact(&mut client, "text/plain", b"goal");
    let params = create_params(goal, repo_path.to_str().unwrap(), &binding);
    let created = client.request("mission.create", params).expect("create");
    let mission_id = created["mission_id"].as_str().unwrap().to_string();
    // Creating a draft is allowed; starting it must refuse dirty input.
    let error = client.request(
        "mission.control",
        json!({"request_id": uuid(), "mission_id": mission_id, "expected_revision": "1", "action": "start"}),
    );
    assert_eq!(error.unwrap_err()["code"], "DIRTY_WORKTREE");
    let snapshot = client
        .request(
            "mission.snapshot",
            json!({"mission_id": mission_id, "snapshot_id": null, "cursor": null}),
        )
        .unwrap();
    assert_eq!(snapshot["revision"], "1");
    assert_eq!(
        snapshot["entities"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "task")
            .count(),
        0
    );
    assert_eq!(
        std::fs::read_to_string(repo_path.join("dirty.txt")).unwrap(),
        "user work"
    );
    daemon.kill();
}

fn run_git(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn register_fake_binding(client: &mut Client) -> String {
    let id = uuid();
    let yes = json!({"supported": true, "reason_code": null});
    let save = client
        .request(
            "binding.save",
            json!({
                "request_id": uuid(),
                "expected_revision": "0",
                "binding": {
                    "id": id,
                    "revision": "0",
                    "label": "E2E fake",
                    "runtime": "fake",
                    "program": "term-fixture",
                    "runtime_version": null,
                    "provider_id": "fake",
                    "model_id": "fixture-model",
                    "effort": null,
                    "auth_route": "local",
                    "credential_ref": null,
                    "endpoint_ref": null,
                    "capabilities": {
                        "structured_result": yes, "events": yes, "cancel": yes, "resume": yes,
                        "steer": yes, "approval_reply": yes, "read_only": yes, "scoped_write": yes,
                        "model_listing": yes, "usage": yes,
                        "native_terminal_attach": {"supported": false, "reason_code": "e2e"},
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
    assert_eq!(save["binding"]["revision"], "1");
    id
}

#[test]
fn production_daemon_rejects_unverified_team_before_creating_any_run() {
    use term_contracts::mission::types::{Entity, MissionState};
    let mut daemon = DaemonProc::spawn("capability-gate-production", None);
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let repo = tempfile::tempdir().unwrap();
    run_git(repo.path(), &["init", "-q"]);
    run_git(
        repo.path(),
        &["config", "user.email", "fixture@iyagi.local"],
    );
    run_git(repo.path(), &["config", "user.name", "fixture"]);
    std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    run_git(repo.path(), &["add", "."]);
    run_git(repo.path(), &["commit", "-qm", "base"]);
    let head = run_git(repo.path(), &["rev-parse", "HEAD"]);
    let mut binding = iyagi_termd_lib::agent_runtime::fake::fake_binding();
    binding.runtime = term_contracts::mission::types::RuntimeKind::Codex;
    binding.provider_id = "openai".into();
    binding.auth_route = term_contracts::mission::types::AuthRoute::Subscription;
    binding.program = common::fixture_bin();
    binding.runtime_version = Some("0.154.0".into());
    binding.checked_at = Some("2026-09-16T00:00:00Z".into());
    client
        .request(
            "binding.save",
            json!({"request_id":uuid(),"expected_revision":"0","binding":binding}),
        )
        .unwrap();
    let goal = upload_artifact(
        &mut client,
        "text/plain",
        b"Do not start without verified capabilities",
    );
    let mut params = create_params(goal, repo.path().to_str().unwrap(), binding.id.as_str());
    params["expected_base_oid"] = json!(head);
    let made = client.request("mission.create", params).unwrap();
    let id = made["mission_id"].as_str().unwrap();
    let error = client
        .request(
            "mission.control",
            json!({"request_id":uuid(),"mission_id":id,"expected_revision":"1","action":"start"}),
        )
        .unwrap_err();
    assert_eq!(error["code"], "CAPABILITY_UNSUPPORTED");
    let snapshot = client
        .request(
            "mission.snapshot",
            json!({"mission_id":id,"snapshot_id":null,"cursor":null}),
        )
        .unwrap();
    let entities: Vec<Entity> = serde_json::from_value(snapshot["entities"].clone()).unwrap();
    assert!(!entities
        .iter()
        .any(|e| matches!(e, Entity::Run(_) | Entity::Exec(_))));
    let mission = entities
        .iter()
        .find_map(|e| match e {
            Entity::Mission(mission) => Some(mission),
            _ => None,
        })
        .unwrap();
    assert_eq!(mission.state, MissionState::Draft);
    assert_eq!(mission.automatic_start_count, 0);
    assert_eq!(run_git(repo.path(), &["rev-parse", "HEAD"]), head);
    daemon.kill();
}

/// Wait-for helper shared by variants that poll (kept for the O17 fault
/// injections that land with the dispatch actor).
#[allow(dead_code)]
fn eventually<F: FnMut() -> Option<T>, T>(mut probe: F, timeout: Duration) -> Option<T> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The daemon pump launches a real stdio protocol child for each AI task.
/// Only model inference is deterministic; no task/run/candidate projections
/// are injected by the test. This does not claim installed CLI compatibility.
#[test]
fn actor_goal_to_acceptance_over_real_daemon_git_and_protocol_processes() {
    run_protocol_pipeline(PipelineScenario::Standard);
}

#[test]
fn failed_plan_recovers_by_explicit_decision_over_real_daemon_and_protocol_processes() {
    run_protocol_pipeline(PipelineScenario::FailedPlan);
}

#[test]
fn cost_budget_holds_paid_work_then_policy_expansion_completes_over_real_daemon() {
    run_protocol_pipeline(PipelineScenario::CostHold);
}

#[test]
fn provider_reset_holds_builders_then_automatically_completes_over_real_daemon() {
    run_protocol_pipeline(PipelineScenario::RateLimit);
}

#[test]
fn unsubmitted_transient_failure_retries_and_completes_over_real_daemon() {
    run_protocol_pipeline(PipelineScenario::TransientStart);
}

#[test]
fn rejected_plan_is_corrected_with_its_diagnostic_over_real_daemon() {
    run_protocol_pipeline(PipelineScenario::PlanFormat);
}

#[test]
fn required_worker_failure_gets_a_lead_replacement_over_real_daemon() {
    run_protocol_pipeline(PipelineScenario::RequiredRepair);
}

#[test]
fn exhausted_required_repair_cleans_all_provider_execs_before_failure_over_real_daemon() {
    run_protocol_pipeline(PipelineScenario::RequiredExhausted);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn verifier_survives_daemon_crash_and_is_stopped_by_recovered_ownership() {
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
        return;
    }
    run_protocol_pipeline(PipelineScenario::VerificationRestart);
}

#[cfg(unix)]
#[test]
fn integration_survives_daemon_crash_and_is_stopped_by_recovered_ownership() {
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
        return;
    }
    run_protocol_pipeline(PipelineScenario::IntegrationRestart);
}

enum PipelineScenario {
    Standard,
    FailedPlan,
    CostHold,
    RateLimit,
    TransientStart,
    PlanFormat,
    RequiredRepair,
    RequiredExhausted,
    VerificationRestart,
    IntegrationRestart,
    IntegrationConflict,
    IntegrationResolutionRestart,
    IntegrationContinuationRestart,
}

#[test]
fn conflict_integrator_finishes_over_real_daemon_and_provider_execs() {
    run_protocol_pipeline(PipelineScenario::IntegrationConflict);
}

#[cfg(unix)]
#[test]
fn interrupted_integrator_rebuilds_in_a_fresh_workspace_after_native_exit() {
    if integration_restart_e2e::native_supported() {
        run_protocol_pipeline(PipelineScenario::IntegrationResolutionRestart);
    }
}

#[cfg(unix)]
#[test]
fn interrupted_continuation_preserves_quarantine_and_rebuilds_after_native_exit() {
    if integration_restart_e2e::native_supported() {
        run_protocol_pipeline(PipelineScenario::IntegrationContinuationRestart);
    }
}

fn run_protocol_pipeline(scenario: PipelineScenario) {
    let integration_conflict = matches!(
        scenario,
        PipelineScenario::IntegrationConflict
            | PipelineScenario::IntegrationResolutionRestart
            | PipelineScenario::IntegrationContinuationRestart
    );
    let resolution_restart = matches!(scenario, PipelineScenario::IntegrationResolutionRestart);
    let continuation_restart = matches!(scenario, PipelineScenario::IntegrationContinuationRestart);
    let conflict_restart = resolution_restart || continuation_restart;
    let verification_restart = matches!(scenario, PipelineScenario::VerificationRestart);
    let integration_restart = matches!(scenario, PipelineScenario::IntegrationRestart);
    let recover_first_plan = matches!(scenario, PipelineScenario::FailedPlan);
    let cost_hold = matches!(scenario, PipelineScenario::CostHold);
    let rate_limit = matches!(scenario, PipelineScenario::RateLimit);
    let transient_start = matches!(scenario, PipelineScenario::TransientStart);
    let plan_format = matches!(scenario, PipelineScenario::PlanFormat);
    let required_repair = matches!(
        scenario,
        PipelineScenario::RequiredRepair | PipelineScenario::RequiredExhausted
    );
    let required_exhausted = matches!(scenario, PipelineScenario::RequiredExhausted);
    use term_contracts::mission::types::{Entity, Phase, RuntimeKind, VerificationStatus};
    // The daemon inherits PATH: the Git stand-in must be in place before spawn.
    #[cfg(unix)]
    if integration_restart || continuation_restart {
        git_barrier::install();
    }
    let mut daemon = DaemonProc::spawn_fixture("actor-protocol-pipeline");
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let repo = tempfile::tempdir().unwrap();
    run_git(repo.path(), &["init", "-q"]);
    run_git(
        repo.path(),
        &["config", "user.email", "actor-e2e@iyagi.local"],
    );
    run_git(repo.path(), &["config", "user.name", "actor-e2e"]);
    std::fs::write(repo.path().join("README.md"), "base\n").unwrap();
    let recovery_marker = daemon.data_dir.join("integration-recovery-started");
    let recovery_release = daemon.data_dir.join("integration-recovery-release");
    if resolution_restart {
        std::fs::write(
            repo.path().join(".iyagi-integration-recovery-fixture.json"),
            json!({"marker": recovery_marker, "release": recovery_release}).to_string(),
        )
        .unwrap();
    }
    if transient_start {
        std::fs::write(
            repo.path().join(".iyagi-transient-fixture.json"),
            json!({"marker":daemon.data_dir.join("transient-first-attempt")}).to_string(),
        )
        .unwrap();
    }
    run_git(repo.path(), &["add", "."]);
    run_git(repo.path(), &["commit", "-qm", "base"]);
    let base = run_git(repo.path(), &["rev-parse", "HEAD"]);
    let mut binding = iyagi_termd_lib::agent_runtime::fake::fake_binding();
    binding.runtime = RuntimeKind::Codex;
    binding.auth_route = term_contracts::mission::types::AuthRoute::Subscription;
    binding.program = common::fixture_bin();
    binding.model_id = "fixture-model".into();
    binding.provider_id = "openai".into();
    if cost_hold {
        binding.estimated_run_cost_usd_micros =
            Some(term_contracts::ids::U64String::new(1_000_000).unwrap());
    }
    let binding_id = binding.id.to_string();
    client
        .request(
            "binding.save",
            json!({"request_id":uuid(),"expected_revision":"0","binding":binding}),
        )
        .unwrap();
    client
        .request("binding.probe", json!({"binding_id":binding_id}))
        .unwrap();
    let goal = upload_artifact(
        &mut client,
        "text/plain",
        if integration_conflict {
            b"fixture-mission-integration-conflict"
        } else if required_exhausted {
            b"fixture-mission-required-exhausted"
        } else if required_repair {
            b"fixture-mission-required-repair"
        } else if recover_first_plan {
            b"fixture-mission-policy-first-plan"
        } else if plan_format {
            b"fixture-mission-invalid-first-plan"
        } else if rate_limit {
            b"fixture-mission-rate-limit"
        } else {
            b"Create api.txt and ui.txt and verify the integrated candidate."
        },
    );
    let command_id = uuid();
    let mut params = create_params(goal, repo.path().to_str().unwrap(), &binding_id);
    params["expected_base_oid"] = json!(base);
    params["requirements"][0]["verification_ids"] = json!([command_id]);
    params["policy"]["allowed_verification_ids"] = json!([command_id]);
    if matches!(scenario, PipelineScenario::Standard) {
        params["policy"]["require_enforced_verification"] = json!(true);
    }
    if conflict_restart {
        params["policy"]["max_attempts_per_task"] = json!(6);
    }
    if required_repair {
        params["policy"]["max_attempts_per_task"] = json!(1);
        params["policy"]["max_repair_cycles"] = json!(1);
    }
    if cost_hold {
        params["policy"]["max_cost_usd_micros"] = json!("1000000");
        params["policy"]["unknown_cost"] = json!("block");
    }
    let made = client.request("mission.create", params).unwrap();
    let mission_id = made["mission_id"].as_str().unwrap();
    let snapshot = client
        .request(
            "mission.snapshot",
            json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null}),
        )
        .unwrap();
    let entities: Vec<Entity> = serde_json::from_value(snapshot["entities"].clone()).unwrap();
    let repository_id = entities
        .iter()
        .find_map(|e| {
            if let Entity::Mission(m) = e {
                Some(m.repository_id.clone())
            } else {
                None
            }
        })
        .unwrap();
    let mut marker = daemon.data_dir.join("verification-invocations");
    if integration_restart {
        // Hold the integration worktree's `git worktree add` after checkout
        // (formerly a post-checkout hook; daemon Git disables hooks).
        #[cfg(unix)]
        {
            use git_barrier::quote;
            let canonical = repo.path().canonicalize().unwrap();
            git_barrier::register(&format!("matches() {{\n  [ \"$(pwd -P)\" = {} ] || return 1\n  [ \"$(git_subcommand \"$@\")\" = worktree ] || return 1\n  for arg do\n    case \"$arg\" in */workspaces/integration-*) return 0 ;; esac\n  done\n  return 1\n}}\nhold() {{\n  printf x >> {}\n  exec /bin/sleep 120\n}}\n",
                quote(&canonical), quote(&marker)));
        }
    }
    let (verify_program, verify_argv) = if verification_restart {
        (
            "/bin/sh",
            json!([
                "-c",
                "printf x >> \"$IYAGI_VERIFICATION_OUTPUT/invocations\"; exec /bin/sleep 120"
            ]),
        )
    } else {
        (
            "git",
            json!(["ls-files", "--error-unmatch", "api.txt", "ui.txt"]),
        )
    };
    client.request("verification.save",json!({"request_id":uuid(),"expected_revision":"0","command":{"id":command_id,"revision":"0","title":"Verify both files","repository_id":repository_id,"program":verify_program,"argv":verify_argv,"cwd_relative":"","timeout_ms":if verification_restart { 180000 } else { 30000 },"env_profile_ref":null,"allowed_network":false}})).unwrap();
    client.request("mission.control",json!({"request_id":uuid(),"mission_id":mission_id,"expected_revision":"1","action":"start"})).unwrap();
    if verification_restart || integration_restart {
        use term_contracts::mission::types::{ExecState, MissionState, RunState};
        let active = eventually(|| {
            let page = client.request("mission.snapshot", json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null})).unwrap();
            let entities: Vec<Entity> = serde_json::from_value(page["entities"].clone()).unwrap();
            if verification_restart {
                if let Some(workspace) = entities.iter().find_map(|e| match e {
                    Entity::Workspace(w) if w.kind == term_contracts::mission::types::WorkspaceKind::Verification => Some(w), _ => None,
                }) {
                    let path = std::path::Path::new(&workspace.path);
                    marker = path.with_file_name(format!("{}.verification-output", path.file_name().unwrap().to_str().unwrap())).join("invocations");
                }
            }
            (marker.exists() && entities.iter().any(|e| matches!(e, Entity::Run(r) if r.binding_snapshot.is_none() && r.state == RunState::Running))).then_some(entities)
        }, Duration::from_secs(25)).expect("verifier is running in its native group");
        let run = active
            .iter()
            .find_map(|e| match e {
                Entity::Run(r) if r.binding_snapshot.is_none() && r.state == RunState::Running => {
                    Some(r.as_ref().clone())
                }
                _ => None,
            })
            .unwrap();
        let exec = active
            .iter()
            .find_map(|e| match e {
                Entity::Exec(e) if Some(&e.id) == run.exec_id.as_ref() => Some(e.as_ref().clone()),
                _ => None,
            })
            .unwrap();
        let identity = exec.identity.as_ref().unwrap();
        assert_eq!(exec.state, ExecState::Spawned);
        assert!(exec.group_identity.is_some());
        let run_count = active
            .iter()
            .filter(|e| matches!(e, Entity::Run(_)))
            .count();
        daemon.kill();
        drop(client);
        assert_eq!(
            term_platform::identity::process_identity(identity.pid).as_ref(),
            Some(identity),
            "daemon death does not end the verifier"
        );
        let mut recovered =
            DaemonProc::spawn_fixture_on(daemon.data_dir.clone(), "verification-restart");
        let (mut client, _) = Client::control(&recovered.endpoint, &recovered.token);
        let unknown = eventually(|| {
            let page = client.request("mission.snapshot", json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null})).unwrap();
            let entities: Vec<Entity> = serde_json::from_value(page["entities"].clone()).unwrap();
            entities.iter().any(|e| matches!(e, Entity::Run(r) if r.id == run.id && matches!(r.state, RunState::Unknown | RunState::Interrupted))).then_some(entities)
        }, Duration::from_secs(10)).expect("restarted daemon retains an uncertain verifier outcome");
        assert_eq!(
            unknown
                .iter()
                .filter(|e| matches!(e, Entity::Run(_)))
                .count(),
            run_count
        );
        assert!(!unknown.iter().any(|e| matches!(e, Entity::Verification(_))));
        assert_eq!(
            term_platform::identity::process_identity(identity.pid).as_ref(),
            Some(identity),
            "recovery does not signal before cancellation"
        );
        let mission = unknown
            .iter()
            .find_map(|e| {
                if let Entity::Mission(m) = e {
                    Some(m)
                } else {
                    None
                }
            })
            .unwrap();
        client.request("mission.control", json!({"request_id":uuid(),"mission_id":mission_id,"expected_revision":mission.revision,"action":"cancel"})).unwrap();
        let ended = eventually(
            || {
                let page = client
                    .request(
                        "mission.snapshot",
                        json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null}),
                    )
                    .unwrap();
                let entities: Vec<Entity> =
                    serde_json::from_value(page["entities"].clone()).unwrap();
                entities
                    .iter()
                    .any(|e| matches!(e, Entity::Mission(m) if m.state == MissionState::Cancelled))
                    .then_some(entities)
            },
            Duration::from_secs(20),
        )
        .expect("native recovery confirms cleanup before cancelled settlement");
        assert!(ended.iter().any(|e| matches!(e, Entity::Exec(e) if e.id == exec.id && e.owner_daemon_id == exec.owner_daemon_id && e.state == ExecState::Exited && e.ended_at.is_some())));
        assert!(!ended.iter().any(|e| matches!(e, Entity::Verification(_))));
        if integration_restart {
            assert!(ended
                .iter()
                .all(|e| !matches!(e, Entity::Candidate(c) if c.revision > 0)));
        }
        assert_eq!(
            ended.iter().filter(|e| matches!(e, Entity::Run(_))).count(),
            run_count
        );
        assert_ne!(
            term_platform::identity::process_identity(identity.pid).as_ref(),
            Some(identity)
        );
        assert_eq!(std::fs::read(&marker).unwrap(), b"x");
        assert_eq!(run_git(repo.path(), &["rev-parse", "HEAD"]), base);
        recovered.kill();
        return;
    }
    if required_exhausted {
        use term_contracts::mission::types::{DecisionState, ExecState, MissionState, RunState};
        let final_entities = eventually(
            || {
                let page = client
                    .request(
                        "mission.snapshot",
                        json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null}),
                    )
                    .unwrap();
                let entities: Vec<Entity> =
                    serde_json::from_value(page["entities"].clone()).unwrap();
                entities
                    .iter()
                    .any(|e| matches!(e, Entity::Mission(m) if m.state == MissionState::Failed))
                    .then_some(entities)
            },
            Duration::from_secs(25),
        )
        .expect("required failure exhausts one repair cycle and settles after cleanup");
        assert_eq!(
            final_entities
                .iter()
                .filter(|e| matches!(e,Entity::Run(r) if r.state == RunState::Failed))
                .count(),
            2
        );
        assert_eq!(
            final_entities
                .iter()
                .filter(|e| matches!(e,Entity::Task(t) if !t.failure_repair_run_ids.is_empty()))
                .count(),
            1
        );
        assert!(!final_entities
            .iter()
            .any(|e| matches!(e,Entity::Run(r) if r.holds_execution_slot())));
        assert!(!final_entities
            .iter()
            .any(|e| matches!(e,Entity::Decision(d) if d.state == DecisionState::Open)));
        let runs: Vec<_> = final_entities
            .iter()
            .filter_map(|e| {
                if let Entity::Run(r) = e {
                    Some(r)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(runs.len(), 5);
        for run in runs {
            assert!(final_entities.iter().any(|e| matches!(e,Entity::Exec(exec) if Some(&exec.id)==run.exec_id.as_ref() && exec.run_id==run.id && exec.state==ExecState::Exited && exec.ended_at.is_some())));
        }
        assert_eq!(run_git(repo.path(), &["rev-parse", "HEAD"]), base);
        assert_eq!(run_git(repo.path(), &["status", "--porcelain"]), "");
        daemon.kill();
        return;
    }
    let retry_plan = if transient_start {
        let entities = eventually(|| {
            let value = client
                .request(
                    "mission.snapshot",
                    json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null}),
                )
                .unwrap();
            let entities: Vec<Entity> = serde_json::from_value(value["entities"].clone()).unwrap();
            entities.iter().any(|e| matches!(e, Entity::Task(t) if t.blocked_code.as_deref() == Some("transient_retry"))).then_some(entities)
        }, Duration::from_secs(15)).expect("first unsubmitted failure enters a durable retry delay");
        let run = entities
            .iter()
            .find_map(|e| match e {
                Entity::Run(r) => Some(r.as_ref().clone()),
                _ => None,
            })
            .unwrap();
        let task = entities
            .iter()
            .find_map(|e| match e {
                Entity::Task(t) if t.id == run.task_id => Some(t),
                _ => None,
            })
            .unwrap();
        assert_eq!(run.state, term_contracts::mission::types::RunState::Failed);
        assert!(run.provider_session_id.is_none() && run.retry_evidence.is_some());
        assert!(entities.iter().any(|e| matches!(e, Entity::Exec(exec) if Some(&exec.id) == run.exec_id.as_ref() && exec.state == term_contracts::mission::types::ExecState::Exited)));
        assert!(!entities.iter().any(|e| matches!(e, Entity::Decision(d) if d.state == term_contracts::mission::types::DecisionState::Open)));
        Some((run, task.dispatch_after_unix_ms.as_ref().unwrap().get()))
    } else {
        None
    };
    let quota_plan = if rate_limit {
        let entities=eventually(|| {
            let value=client.request("mission.snapshot",json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null})).unwrap();
            let entities:Vec<Entity>=serde_json::from_value(value["entities"].clone()).unwrap();
            (entities.iter().filter(|e|matches!(e,Entity::Task(t) if t.blocked_code.as_deref()==Some("provider_rate_limited"))).count()==2).then_some(entities)
        },Duration::from_secs(10)).expect("builders wait for the provider's known reset");
        let runs: Vec<_> = entities
            .iter()
            .filter_map(|e| {
                if let Entity::Run(r) = e {
                    Some(r.as_ref())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].state,
            term_contracts::mission::types::RunState::Succeeded
        );
        let plan = runs[0].clone();
        let reset = &plan.rate_limit.as_ref().unwrap().resets_at_unix_ms;
        assert!(entities
            .iter()
            .filter_map(|e| if let Entity::Task(t) = e {
                Some(t)
            } else {
                None
            })
            .filter(|t| t.blocked_code.as_deref() == Some("provider_rate_limited"))
            .all(|t| t.attempt_count == 0 && t.dispatch_after_unix_ms.as_ref() == Some(reset)));
        Some(plan)
    } else {
        None
    };
    let cost_plan = if cost_hold {
        let entities = eventually(|| {
            let value = client.request("mission.snapshot",json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null})).unwrap();
            let entities: Vec<Entity> = serde_json::from_value(value["entities"].clone()).unwrap();
            (entities.iter().filter(|e| matches!(e, Entity::Task(t) if t.blocked_code.as_deref() == Some("cost_limit"))).count() == 2).then_some(entities)
        },Duration::from_secs(15)).expect("builders wait behind a completed plan's estimated dollar charge");
        let runs: Vec<_> = entities
            .iter()
            .filter_map(|e| match e {
                Entity::Run(r) => Some(r.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(
            runs.len(),
            1,
            "cost hold cannot launch or consume builder attempts"
        );
        assert_eq!(
            runs[0].state,
            term_contracts::mission::types::RunState::Succeeded
        );
        let plan = runs[0].clone();
        let updated = eventually(|| {
            let value=client.request("mission.snapshot",json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null})).unwrap();
            let entities:Vec<Entity>=serde_json::from_value(value["entities"].clone()).unwrap();
            let m=entities.iter().find_map(|e| match e {Entity::Mission(m)=>Some(m.as_ref()),_=>None}).unwrap();
            let mut policy=m.policy.clone();policy.max_cost_usd_micros=Some(term_contracts::ids::U64String::new(4_000_000).unwrap());
            let params=json!({"request_id":uuid(),"mission_id":mission_id,"expected_revision":m.revision,"policy":policy,"role_bindings":m.role_bindings});
            match client.request("mission.policy.update",params.clone()) {
                Ok(response)=>Some((params,response)),
                Err(e)=>{assert_eq!(e["code"],"REVISION_CONFLICT");None}
            }
        },Duration::from_secs(5)).expect("cost policy expansion commits");
        assert_eq!(
            client.request("mission.policy.update", updated.0).unwrap(),
            updated.1
        );
        Some(plan)
    } else {
        None
    };
    let recovery_replay = if recover_first_plan {
        use term_contracts::mission::types::{DecisionState, ExecState, RunState};
        let entities = eventually(
            || {
                let snapshot = client
                    .request(
                        "mission.snapshot",
                        json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null}),
                    )
                    .unwrap();
                let entities: Vec<Entity> =
                    serde_json::from_value(snapshot["entities"].clone()).unwrap();
                entities
                    .iter()
                    .any(|e| {
                        matches!(e, Entity::Decision(d) if d.state == DecisionState::Open
                && d.options.iter().any(|o| o.id == "retry_failed_task"))
                    })
                    .then_some(entities)
            },
            Duration::from_secs(15),
        )
        .expect("invalid plan has a durable recovery decision");
        let failed = entities
            .iter()
            .find_map(|e| match e {
                Entity::Run(r) if r.state == RunState::Failed => Some(r.clone()),
                _ => None,
            })
            .unwrap();
        assert!(entities.iter().any(|e| matches!(e, Entity::Exec(exec) if exec.run_id == failed.id && exec.state == ExecState::Exited && exec.ended_at.is_some())));
        assert_eq!(
            entities
                .iter()
                .filter(|e| matches!(e, Entity::Run(_)))
                .count(),
            1
        );
        let decision = entities
            .iter()
            .find_map(|e| match e {
                Entity::Decision(d) if d.state == DecisionState::Open => Some(d),
                _ => None,
            })
            .unwrap();
        assert_eq!(decision.requesting_run_id.as_ref(), Some(&failed.id));
        let revision = entities
            .iter()
            .find_map(|e| match e {
                Entity::Mission(m) => Some(m.revision.clone()),
                _ => None,
            })
            .unwrap();
        let params = json!({"request_id":uuid(),"mission_id":mission_id,"expected_revision":revision,"decision_id":decision.id,"option_id":"retry_failed_task","answer_ref":null});
        let response = client
            .request("mission.decision.answer", params.clone())
            .unwrap();
        assert_eq!(
            client
                .request("mission.decision.answer", params.clone())
                .unwrap(),
            response
        );
        Some((params, response, failed))
    } else {
        None
    };
    let integration_failed = if integration_conflict {
        use term_contracts::mission::types::{DecisionKind, DecisionState, RunState};
        let snapshot = eventually(|| {
            let page = client.request("mission.snapshot", json!({"mission_id": mission_id, "snapshot_id": null, "cursor": null})).unwrap();
            let entities: Vec<Entity> = serde_json::from_value(page["entities"].clone()).unwrap();
            entities.iter().any(|e| matches!(e, Entity::Decision(d) if d.kind == DecisionKind::Conflict && d.state == DecisionState::Open)).then_some(entities)
        }, Duration::from_secs(25)).expect("integration conflict must be recorded");
        let mission = snapshot
            .iter()
            .find_map(|e| {
                if let Entity::Mission(m) = e {
                    Some(m)
                } else {
                    None
                }
            })
            .unwrap();
        let decision = snapshot
            .iter()
            .find_map(|e| {
                if let Entity::Decision(d) = e {
                    (d.kind == DecisionKind::Conflict).then_some(d)
                } else {
                    None
                }
            })
            .unwrap();
        let run = snapshot
            .iter()
            .find_map(|e| {
                if let Entity::Run(r) = e {
                    (Some(&r.id) == decision.requesting_run_id.as_ref())
                        .then_some(r.as_ref().clone())
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(run.state, RunState::Failed);
        #[cfg(unix)]
        if continuation_restart {
            let workspace = snapshot
                .iter()
                .find_map(|e| match e {
                    Entity::Workspace(w) if Some(&w.id) == run.workspace_id.as_ref() => Some(w),
                    _ => None,
                })
                .unwrap();
            integration_restart_e2e::install_capture_barrier(
                &workspace.path,
                &recovery_marker,
                &recovery_release,
            );
        }
        client
            .request(
                "mission.decision.answer",
                json!({"request_id": uuid(), "mission_id": mission_id,
            "expected_revision": mission.revision, "decision_id": decision.id,
            "option_id": "resolve_and_reintegrate", "answer_ref": null}),
            )
            .unwrap();
        Some(run)
    } else {
        None
    };
    #[cfg(unix)]
    let recovered_integration = if conflict_restart {
        Some(integration_restart_e2e::recover(
            &mut daemon,
            &mut client,
            mission_id,
            integration_failed.as_ref().unwrap(),
            &recovery_marker,
            &recovery_release,
            continuation_restart,
        ))
    } else {
        None
    };
    let mut latest = vec![];
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    while std::time::Instant::now() < deadline {
        let snapshot = client
            .request(
                "mission.snapshot",
                json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null}),
            )
            .unwrap();
        latest = serde_json::from_value::<Vec<Entity>>(snapshot["entities"].clone()).unwrap();
        if latest
            .iter()
            .any(|e| matches!(e,Entity::Mission(m) if m.phase==Phase::AwaitingAcceptance))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mission = latest
        .iter()
        .find_map(|e| {
            if let Entity::Mission(m) = e {
                Some(m)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        mission.phase,
        Phase::AwaitingAcceptance,
        "actor did not complete: {latest:?}; log: {}",
        std::fs::read_to_string(daemon.data_dir.join("daemon-stderr.log")).unwrap_or_default()
    );
    let runs: Vec<_> = latest
        .iter()
        .filter_map(|e| {
            if let Entity::Run(r) = e {
                Some(r)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        runs.len(),
        6 + 2 * usize::from(integration_conflict)
            + 2 * usize::from(conflict_restart)
            + usize::from(continuation_restart)
            + usize::from(recover_first_plan)
            + usize::from(transient_start)
            + usize::from(plan_format)
            + 2 * usize::from(required_repair)
    );
    if required_repair {
        use term_contracts::mission::types::{RunState, TaskState};
        let failed = runs.iter().find(|r| r.state == RunState::Failed).unwrap();
        assert!(failed.result_ref.is_some() && failed.ended_at.is_some());
        assert!(latest.iter().any(|e| matches!(e,Entity::Task(t) if t.id==failed.task_id && t.state==TaskState::Superseded && t.attempt_count==1)));
        assert!(latest.iter().any(|e| matches!(e,Entity::Task(t) if t.failure_repair_run_ids==[failed.id.clone()] && t.state==TaskState::Succeeded && t.repair_cycle==1)));
        let replacement = latest
            .iter()
            .find_map(|e| {
                if let Entity::Task(t) = e {
                    (t.replacement_of.as_ref() == Some(&failed.task_id)).then_some(t)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(replacement.state, TaskState::Succeeded);
        assert_eq!(replacement.attempt_count, 1);
        assert_ne!(replacement.workspace_id, failed.workspace_id);
    }
    if plan_format {
        use term_contracts::mission::types::{DecisionKind, RetryEvidence, RunState};
        let failed = runs.iter().find(|r| r.state == RunState::Failed).unwrap();
        assert!(matches!(
            failed.retry_evidence,
            Some(RetryEvidence::PlanFormatRejected {
                plan_revision: 0,
                rejected_result_ref: Some(_)
            })
        ));
        let replacement = runs
            .iter()
            .find(|r| r.task_id == failed.task_id && r.state == RunState::Succeeded)
            .unwrap();
        assert_eq!(replacement.attempt, 2);
        assert_ne!(replacement.workspace_id, failed.workspace_id);
        assert!(latest
            .iter()
            .all(|e| !matches!(e, Entity::Decision(d) if d.kind == DecisionKind::Recovery)));
        assert!(failed.result_ref.is_some());
    }
    if let Some((old, deadline)) = retry_plan {
        assert_eq!(
            runs.iter().find(|r| r.id == old.id).map(|r| r.as_ref()),
            Some(&old)
        );
        let replacement = runs
            .iter()
            .find(|r| r.task_id == old.task_id && r.id != old.id)
            .unwrap();
        assert_eq!(replacement.attempt, 2);
        assert_ne!(replacement.workspace_id, old.workspace_id);
        let earliest = term_storage::time::iso8601_from_unix(
            (deadline / 1000) as i64,
            (deadline % 1000) as u32,
        );
        assert!(replacement
            .started_at
            .as_ref()
            .is_some_and(|at| at >= &earliest));
    }
    if let Some(plan) = quota_plan {
        assert_eq!(
            runs.iter().find(|r| r.id == plan.id).map(|r| r.as_ref()),
            Some(&plan)
        );
        let reset = plan.rate_limit.as_ref().unwrap().resets_at_unix_ms.get();
        let earliest =
            term_storage::time::iso8601_from_unix((reset / 1000) as i64, (reset % 1000) as u32);
        for run in runs
            .iter()
            .filter(|r| r.id != plan.id && r.binding_snapshot.is_some())
        {
            assert!(
                run.started_at.as_ref().is_some_and(|at| at >= &earliest),
                "provider run started before its reset: {run:?}"
            );
        }
    }
    if let Some(plan) = cost_plan {
        assert_eq!(
            runs.iter().find(|r| r.id == plan.id).map(|r| r.as_ref()),
            Some(&plan)
        );
        let owned: Vec<_> = runs.iter().map(|r| r.as_ref().clone()).collect();
        let total = term_core::mission::budget::summarize_cost(&owned);
        assert_eq!(total.committed_micros(), 4_000_000);
        assert_eq!(total.unknown_runs, 0);
    }
    assert!(
        mission.active_time_ms.get() > 0,
        "the real daemon persists mission elapsed time"
    );
    assert!(
        runs.iter()
            .filter(|run| run.ended_at.is_some())
            .all(|run| run.active_time_ms.get() > 0),
        "each ended provider/verifier Run retains measured time"
    );
    let execs: Vec<_> = latest
        .iter()
        .filter_map(|entity| {
            if let Entity::Exec(exec) = entity {
                Some(exec)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        execs.len(),
        6 + 2 * usize::from(integration_conflict)
            + 2 * usize::from(conflict_restart)
            + usize::from(continuation_restart)
            + usize::from(recover_first_plan)
            + usize::from(transient_start)
            + usize::from(plan_format)
            + 2 * usize::from(required_repair),
        "provider, integration and verification runs each own a durable Exec"
    );
    for run in &runs {
        let exec = execs
            .iter()
            .find(|exec| Some(&exec.id) == run.exec_id.as_ref())
            .expect("run links its owned exec");
        assert_eq!(exec.run_id, run.id);
        assert_eq!(
            exec.state,
            term_contracts::mission::types::ExecState::Exited
        );
        assert!(
            exec.identity.is_some() && exec.group_reference.is_some() && exec.ended_at.is_some()
        );
        assert!(exec.launch_manifest_ref.bytes.get() > 0);
    }
    if let Some((params, response, failed)) = &recovery_replay {
        assert_eq!(runs.iter().find(|r| r.id == failed.id).unwrap(), &failed);
        assert_eq!(
            runs.iter().filter(|r| r.task_id == failed.task_id).count(),
            2
        );
        assert_eq!(
            client
                .request("mission.decision.answer", params.clone())
                .unwrap(),
            *response
        );
    }
    assert_eq!(
        runs.iter()
            .filter(|r| r.state == term_contracts::mission::types::RunState::Succeeded)
            .count(),
        6 + usize::from(integration_conflict)
            + usize::from(continuation_restart)
            + usize::from(required_repair)
    );
    assert_eq!(
        runs.iter()
            .filter(|r| r.provider_session_id.as_deref() == Some("fixture-thread"))
            .count(),
        4 + usize::from(integration_conflict)
            + usize::from(conflict_restart)
            + usize::from(recover_first_plan)
            + usize::from(plan_format)
            + 2 * usize::from(required_repair)
    );
    if let Some(failed) = &integration_failed {
        let mut attempts: Vec<_> = runs
            .iter()
            .filter(|r| r.task_id == failed.task_id)
            .collect();
        attempts.sort_by_key(|r| r.attempt);
        assert_eq!(
            attempts.len(),
            3 + 2 * usize::from(conflict_restart) + usize::from(continuation_restart)
        );
        assert_eq!(attempts[0].as_ref(), failed);
        assert!(
            attempts[1].binding_snapshot.is_some()
                && attempts.last().unwrap().binding_snapshot.is_none()
        );
        if !conflict_restart {
            assert!(attempts
                .iter()
                .all(|r| r.workspace_id == failed.workspace_id));
        }
        assert_eq!(latest.iter().filter(|e| matches!(e, Entity::Workspace(w) if w.kind == term_contracts::mission::types::WorkspaceKind::Integration)).count(), 1 + usize::from(conflict_restart));
    }
    #[cfg(unix)]
    if let Some((unknown, workspace)) = &recovered_integration {
        assert_eq!(
            runs.iter().find(|r| r.id == unknown.id).unwrap().as_ref(),
            unknown
        );
        assert!(latest
            .iter()
            .any(|e| matches!(e, Entity::Workspace(w) if w.as_ref() == workspace)));
        assert_eq!(
            std::fs::read(std::path::Path::new(&workspace.path).join("unknown-only.txt")).unwrap(),
            b"quarantined fixture output\n"
        );
        assert!(runs
            .iter()
            .filter(|r| r.task_id == unknown.task_id && r.attempt > unknown.attempt)
            .all(|r| r.workspace_id != unknown.workspace_id));
    }
    let checks: Vec<_> = latest
        .iter()
        .filter_map(|e| {
            if let Entity::Verification(v) = e {
                Some(v)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, VerificationStatus::Passed);
    assert_eq!(
        checks[0].input_integrity,
        term_contracts::mission::types::InputIntegrity::Enforced
    );
    let candidate = latest
        .iter()
        .find_map(|e| {
            if let Entity::Candidate(c) = e {
                if Some(&c.id) == mission.candidate_id.as_ref() {
                    Some(c)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .unwrap();
    if conflict_restart {
        assert!(!run_git(
            repo.path(),
            &["ls-tree", "--name-only", &candidate.commit_oid]
        )
        .lines()
        .any(|p| p == "unknown-only.txt"));
    }
    assert!(run_git(
        repo.path(),
        &["show", &format!("{}:api.txt", candidate.commit_oid)]
    )
    .contains("real fixture process"));
    assert!(run_git(
        repo.path(),
        &["show", &format!("{}:ui.txt", candidate.commit_oid)]
    )
    .contains("real fixture process"));
    assert_eq!(run_git(repo.path(), &["rev-parse", "HEAD"]), base);
    assert_eq!(run_git(repo.path(), &["status", "--porcelain"]), "");
    let mut accept_params = json!({"request_id":uuid(),"mission_id":mission_id,"expected_revision":mission.revision,"candidate_id":candidate.id,"acknowledged_verification_ids":[checks[0].id],"human_requirement_ids":[]});
    if conflict_restart {
        assert_eq!(
            client
                .request("mission.accept", accept_params.clone())
                .unwrap_err()["details"]["reason_code"],
            "unknown_run"
        );
        let ids: Vec<_> = runs
            .iter()
            .filter(|r| {
                matches!(
                    r.state,
                    term_contracts::mission::types::RunState::Unknown
                        | term_contracts::mission::types::RunState::Interrupted
                )
            })
            .map(|r| r.id.clone())
            .collect();
        assert_eq!(ids.len(), 1);
        let mut bad = accept_params.clone();
        bad["request_id"] = json!(uuid());
        bad["acknowledged_reconciled_run_ids"] = json!([ids[0], ids[0]]);
        assert_eq!(
            client.request("mission.accept", bad).unwrap_err()["code"],
            "INVALID_ARGUMENT"
        );
        accept_params["request_id"] = json!(uuid());
        accept_params["acknowledged_reconciled_run_ids"] = json!(ids);
    }
    #[cfg(unix)]
    if let Some((unknown, _)) = &recovered_integration {
        integration_restart_e2e::reject_changed_acceptance_evidence(
            &daemon,
            &mut client,
            &accept_params,
            unknown,
        );
    }
    let accepted = client
        .request("mission.accept", accept_params.clone())
        .unwrap();
    assert_eq!(
        client.request("mission.accept", accept_params).unwrap(),
        accepted
    );
    #[cfg(unix)]
    if let Some((unknown, _)) = &recovered_integration {
        integration_restart_e2e::assert_acceptance_evidence(
            &daemon,
            &mut client,
            mission_id,
            unknown,
        );
    }
    let snapshot = client
        .request(
            "mission.snapshot",
            json!({"mission_id":mission_id,"snapshot_id":null,"cursor":null}),
        )
        .unwrap();
    let entities: Vec<Entity> = serde_json::from_value(snapshot["entities"].clone()).unwrap();
    assert!(entities.iter().any(|e|matches!(e,Entity::Mission(m) if m.state==term_contracts::mission::types::MissionState::Completed)));
    client
        .request("daemon.shutdown", json!({"stop_workloads":true}))
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if daemon.child.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("daemon did not shut down after the mission completed");
}

#[test]
fn active_message_reaches_the_owned_codex_turn_over_real_daemon_ipc() {
    run_message_delivery(false);
}

#[test]
fn explicitly_replacing_an_unconfirmed_message_delivers_new_text_over_real_daemon_ipc() {
    run_message_delivery(true);
}

fn run_message_delivery(replace_unknown: bool) {
    use term_contracts::mission::types::*;
    let mut daemon = binding_evidence_daemon::EvidenceDaemon::spawn();
    let (mut client, _) = Client::control(&daemon.endpoint, &daemon.token);
    let repo = tempfile::tempdir().unwrap();
    run_git(repo.path(), &["init", "-q"]);
    run_git(
        repo.path(),
        &["config", "user.email", "message-e2e@iyagi.local"],
    );
    run_git(repo.path(), &["config", "user.name", "message-e2e"]);
    std::fs::write(repo.path().join("README.md"), "base\n").unwrap();
    run_git(repo.path(), &["add", "."]);
    run_git(repo.path(), &["commit", "-qm", "base"]);
    let mut binding = iyagi_termd_lib::agent_runtime::fake::fake_binding();
    binding.runtime = RuntimeKind::Codex;
    binding.auth_route = AuthRoute::Subscription;
    binding.program = common::fixture_bin();
    binding.model_id = "fixture-model".into();
    binding.provider_id = "openai".into();
    binding.capabilities.steer = Support {
        supported: true,
        reason_code: None,
    };
    client
        .request(
            "binding.save",
            json!({"request_id":uuid(),"expected_revision":"0","binding":binding}),
        )
        .unwrap();
    let listed = client.request("binding.list", json!({})).unwrap();
    assert_eq!(
        listed["bindings"][0]["capabilities"]["steer"]["supported"], false,
        "client capability claims must not authorize steering"
    );
    let observed = client
        .request("binding.probe", json!({"binding_id":binding.id}))
        .unwrap();
    assert_eq!(
        observed["binding"]["capabilities"]["steer"]["supported"], true,
        "only the injected server-side protocol fixture supplies this evidence"
    );
    let goal = upload_artifact(
        &mut client,
        "text/plain",
        if replace_unknown {
            b"fixture-mission-message-replacement"
        } else {
            b"fixture-mission-message"
        },
    );
    let mut params = create_params(goal, repo.path().to_str().unwrap(), binding.id.as_str());
    params["expected_base_oid"] = json!(run_git(repo.path(), &["rev-parse", "HEAD"]));
    let made = client.request("mission.create", params).unwrap();
    let id = made["mission_id"].as_str().unwrap();
    client
        .request(
            "mission.control",
            json!({"request_id":uuid(),"mission_id":id,"expected_revision":"1","action":"start"}),
        )
        .unwrap();
    let snapshot = |client: &mut Client| -> Vec<Entity> {
        serde_json::from_value(
            client
                .request(
                    "mission.snapshot",
                    json!({"mission_id":id,"snapshot_id":null,"cursor":null}),
                )
                .unwrap()["entities"]
                .clone(),
        )
        .unwrap()
    };
    let run_id = eventually(
        || {
            snapshot(&mut client).iter().find_map(|e| match e {
                Entity::Run(r) if r.state == RunState::Running => Some(r.id.clone()),
                _ => None,
            })
        },
        Duration::from_secs(15),
    )
    .unwrap_or_else(|| {
        panic!(
            "real provider turn did not start: {:?}",
            snapshot(&mut client)
        )
    });
    let initial = snapshot(&mut client);
    let initial_mission_time = initial
        .iter()
        .find_map(|e| match e {
            Entity::Mission(m) => Some(m.active_time_ms.get()),
            _ => None,
        })
        .unwrap();
    let initial_run_time = initial
        .iter()
        .find_map(|e| match e {
            Entity::Run(r) if r.id == run_id => Some(r.active_time_ms.get()),
            _ => None,
        })
        .unwrap();
    eventually(|| {
        let entities = snapshot(&mut client);
        (entities.iter().any(|e| matches!(e, Entity::Mission(m) if m.active_time_ms.get() >= initial_mission_time + 1000))
            && entities.iter().any(|e| matches!(e, Entity::Run(r) if r.id == run_id && r.state == RunState::Running && r.active_time_ms.get() >= initial_run_time + 1000))).then_some(())
    }, Duration::from_secs(5)).expect("quiet provider and mission times are checkpointed without activity events");
    let reference = upload_scoped_artifact(
        &mut client,
        Some(id),
        "text/plain",
        "실행 중 전달된 지시입니다.".as_bytes(),
    );
    let revision = snapshot(&mut client)
        .iter()
        .find_map(|e| match e {
            Entity::Mission(m) => Some(m.revision.clone()),
            _ => None,
        })
        .unwrap();
    let params = json!({"request_id":uuid(),"mission_id":id,"expected_revision":revision,"target_task_id":null,"body_ref":reference});
    let original = client.request("mission.message", params.clone()).unwrap();
    let mut replacement_replay = None;
    let mut unknown_message = None;
    let final_text = if replace_unknown {
        "기록을 확인한 뒤 수정한 새 지시입니다."
    } else {
        "실행 중 전달된 지시입니다."
    };
    if replace_unknown {
        let unknown = eventually(
            || {
                snapshot(&mut client).iter().find_map(|e| match e {
                    Entity::Message(m)
                        if m.role == MessageRole::User
                            && m.delivery == MessageDelivery::Unknown =>
                    {
                        Some(m.as_ref().clone())
                    }
                    _ => None,
                })
            },
            Duration::from_secs(10),
        )
        .expect("received steer without acknowledgement becomes unknown");
        assert_eq!(unknown.run_id.as_ref(), Some(&run_id));
        assert_eq!(
            client.request("mission.message", params.clone()).unwrap(),
            original
        );
        assert_eq!(
            snapshot(&mut client)
                .iter()
                .filter(|e| matches!(e, Entity::Message(m) if m.role == MessageRole::User))
                .count(),
            1
        );
        let body =
            upload_scoped_artifact(&mut client, Some(id), "text/plain", final_text.as_bytes());
        let revision = snapshot(&mut client)
            .iter()
            .find_map(|e| {
                if let Entity::Mission(m) = e {
                    Some(m.revision.clone())
                } else {
                    None
                }
            })
            .unwrap();
        let replacement = json!({"request_id":uuid(),"mission_id":id,"expected_revision":revision,"target_task_id":null,"body_ref":body,"supersedes_message_id":unknown.id});
        let response = client
            .request("mission.message", replacement.clone())
            .unwrap();
        unknown_message = Some(unknown);
        replacement_replay = Some((replacement, response));
    }
    let final_entities = eventually(|| {
        let entities = snapshot(&mut client);
        (entities.iter().any(|e| matches!(e,Entity::Message(m) if m.role == MessageRole::User && m.delivery == MessageDelivery::Delivered && m.run_id.as_ref() == Some(&run_id)))
            && entities.iter().any(|e| matches!(e,Entity::Run(r) if r.id == run_id && r.state == RunState::Succeeded)))
            .then_some(entities)
    }, Duration::from_secs(15)).expect("message receipt and final outcome persisted");
    assert_eq!(client.request("mission.message", params).unwrap(), original);
    assert_eq!(
        final_entities
            .iter()
            .filter(|e| matches!(e, Entity::Run(_)))
            .count(),
        1
    );
    assert_eq!(
        final_entities
            .iter()
            .filter(|e| matches!(e,Entity::Message(m) if m.role == MessageRole::User))
            .count(),
        1 + usize::from(replace_unknown)
    );
    let result_ref = final_entities
        .iter()
        .find_map(|e| match e {
            Entity::Run(r) if r.id == run_id => r.result_ref.as_ref(),
            _ => None,
        })
        .unwrap();
    let result = client
        .request(
            "artifact.read",
            json!({"artifact_id":result_ref.id,"offset":"0","max_bytes":4096}),
        )
        .unwrap();
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(result["data_b64"].as_str().unwrap())
        .unwrap();
    let result: Value = serde_json::from_slice(&bytes).unwrap();
    let report = client
        .request(
            "artifact.read",
            json!({"artifact_id":result["report_ref"]["id"],"offset":"0","max_bytes":4096}),
        )
        .unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(report["data_b64"].as_str().unwrap())
        .unwrap();
    assert_eq!(String::from_utf8(bytes).unwrap(), final_text);
    if let (Some(unknown), Some((replacement, response))) = (unknown_message, replacement_replay) {
        assert_eq!(
            client.request("mission.message", replacement).unwrap(),
            response
        );
        assert_eq!(
            final_entities
                .iter()
                .find_map(|e| if let Entity::Message(m) = e {
                    (m.id == unknown.id).then_some(m.as_ref())
                } else {
                    None
                }),
            Some(&unknown)
        );
        assert_eq!(
            final_entities
                .iter()
                .filter(|e| matches!(e, Entity::Message(m) if m.role == MessageRole::User))
                .count(),
            2
        );
        assert_eq!(
            final_entities
                .iter()
                .filter(|e| matches!(e, Entity::Run(_)))
                .count(),
            1
        );
        assert!(final_entities.iter().any(|e| matches!(e, Entity::Message(m) if m.supersedes_message_id.as_ref() == Some(&unknown.id) && m.delivery == MessageDelivery::Delivered)));
    }
    assert!(final_entities.iter().any(|e| matches!(e,Entity::Exec(exec) if exec.run_id == run_id && exec.state == ExecState::Exited)));
    daemon.kill();
}
