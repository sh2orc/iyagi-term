//! Exercise the actor itself, with injected provider events and real SQLite,
//! Git worktrees, captured patches, integration, and command execution.
//! These are not claims of installed-provider or real-daemon compatibility.
use base64::Engine;
use iyagi_termd_lib::{
    agent_runtime::{
        fake::{fake_binding, FakeAdapter, FakeScript, FakeStep, WallClock},
        AgentAdapter, RunStart,
    },
    mission::{
        actor::{AdapterFactory, MissionActor},
        artifacts::ArtifactStore,
        workflow, MissionService,
    },
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use term_contracts::{ids::ConnectionId, mission::types::*};
use term_storage::Storage;

#[path = "support/mission_messages.rs"]
mod mission_messages;

#[path = "support/mission_failures.rs"]
mod mission_failures;

#[path = "support/mission_cancellation.rs"]
mod mission_cancellation;

#[path = "support/mission_transient_retry.rs"]
mod mission_transient_retry;

#[path = "support/mission_plan_repair.rs"]
mod mission_plan_repair;

#[path = "support/mission_required_repair.rs"]
mod mission_required_repair;

#[cfg(unix)]
#[path = "support/mission_verification_exec.rs"]
mod mission_verification_exec;

#[cfg(unix)]
#[path = "support/mission_integration_exec.rs"]
mod mission_integration_exec;

#[path = "support/mission_timing.rs"]
mod mission_timing;

#[path = "support/mission_cost_usage.rs"]
mod mission_cost_usage;

#[path = "support/mission_reconciliation.rs"]
mod mission_reconciliation;

#[path = "support/mission_exec_recovery.rs"]
mod mission_exec_recovery;

#[path = "support/mission_capability_gate.rs"]
mod mission_capability_gate;

#[path = "support/mission_provider_blocks.rs"]
mod mission_provider_blocks;

#[path = "support/mission_usability.rs"]
mod mission_usability;

fn git(path: &Path, args: &[&str]) -> String {
    iyagi_termd_lib::workspace::git::run_git_for_test(path, args)
}
struct Rig {
    dir: tempfile::TempDir,
    repo: tempfile::TempDir,
    storage: Arc<Storage>,
    service: Arc<MissionService>,
    conn: ConnectionId,
    id: Id,
    base: String,
}
impl Rig {
    fn new(automatic: bool, command_argv: &[&str]) -> Self {
        Self::with_recovery_policy(automatic, command_argv, true)
    }
    fn with_recovery_policy(automatic: bool, command_argv: &[&str], recover_unsent: bool) -> Self {
        Self::with_clock(automatic, command_argv, recover_unsent, None)
    }
    fn with_clock(
        automatic: bool,
        command_argv: &[&str],
        recover_unsent: bool,
        clock: Option<Arc<dyn Fn() -> Instant + Send + Sync>>,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("base.txt"), "original\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "base"]);
        let base = git(repo.path(), &["rev-parse", "HEAD"]);
        let storage = Arc::new(Storage::open(dir.path().join("state.db")).unwrap());
        let mut service = MissionService::new(
            storage.clone(),
            ArtifactStore::new(storage.clone(), dir.path().join("missions")),
        );
        if let Some(clock) = clock {
            service = service.with_monotonic_clock(move || clock());
        }
        let service = Arc::new(service);
        let conn = ConnectionId::generate();
        let binding = fake_binding();
        let binding_id = binding.id.clone();
        rpc(
            &service,
            &conn,
            "binding.save",
            json!({"request_id":Id::generate(),"expected_revision":"0","binding":binding}),
        );
        let body = b"Create api.txt and ui.txt with working content.";
        let sha = format!("{:x}", Sha256::digest(body));
        let upload = rpc(
            &service,
            &conn,
            "artifact.begin",
            json!({"request_id":Id::generate(),"mission_id":null,"media_type":"text/plain","bytes":body.len().to_string(),"sha256":sha}),
        );
        rpc(
            &service,
            &conn,
            "artifact.write",
            json!({"upload_id":upload["upload_id"],"offset":"0","data_b64":base64::engine::general_purpose::STANDARD.encode(body)}),
        );
        let goal = rpc(
            &service,
            &conn,
            "artifact.commit",
            json!({"upload_id":upload["upload_id"]}),
        );
        let command_id = Id::generate();
        let made = rpc(
            &service,
            &conn,
            "mission.create",
            json!({"request_id":Id::generate(),"title":"Actor pipeline","repository_path":repo.path(),"expected_base_oid":base,"goal_ref":goal,
            "requirements":[{"id":Id::generate(),"text":"Both files exist in the final candidate","verification_ids":[command_id],"human_check":false}],
            "policy":{"max_parallel_runs":4,"max_attempts_per_task":3,"max_repair_cycles":3,"max_automatic_starts":64,"active_time_limit_ms":"14400000","run_time_limit_ms":"2700000","max_cost_usd_micros":null,"unknown_cost":"allow_with_notice","allow_network":false,"allow_automatic_plan_apply":automatic,"allow_recovery_of_unsent":recover_unsent,"allowed_binding_ids":[binding_id],"allowed_roles":["lead","builder","reviewer","integrator"],"allowed_verification_ids":[command_id],"require_independent_review":true,"require_enforced_verification":false},
            "role_bindings":[{"role":"lead","primary_binding_id":binding_id,"fallback_binding_ids":[]},{"role":"builder","primary_binding_id":binding_id,"fallback_binding_ids":[]},{"role":"reviewer","primary_binding_id":binding_id,"fallback_binding_ids":[]},{"role":"integrator","primary_binding_id":binding_id,"fallback_binding_ids":[]}]}),
        );
        let id: Id = serde_json::from_value(made["mission_id"].clone()).unwrap();
        let mission = workflow::load_entities(&storage, &id).unwrap().mission;
        let command = VerificationCommand {
            id: command_id,
            title: "Verify files".into(),
            program: "git".into(),
            argv: command_argv.iter().map(|s| s.to_string()).collect(),
            revision: term_contracts::ids::U64String::new(0).unwrap(),
            repository_id: mission.repository_id,
            cwd_relative: String::new(),
            timeout_ms: 30000,
            env_profile_ref: None,
            allowed_network: false,
        };
        rpc(
            &service,
            &conn,
            "verification.save",
            json!({"request_id":Id::generate(),"expected_revision":"0","command":command}),
        );
        rpc(
            &service,
            &conn,
            "mission.control",
            json!({"request_id":Id::generate(),"mission_id":id,"expected_revision":"1","action":"start"}),
        );
        Self {
            dir,
            repo,
            storage,
            service,
            conn,
            id,
            base,
        }
    }
    fn snapshot(&self) -> workflow::MissionEntities {
        workflow::load_entities(&self.storage, &self.id).unwrap()
    }
    fn actor(&self, factory: AdapterFactory) -> MissionActor {
        MissionActor::new(
            self.service.clone(),
            self.dir.path().join("missions"),
            factory,
        )
    }
    fn tick_until(
        &self,
        actor: &mut MissionActor,
        predicate: impl Fn(&workflow::MissionEntities) -> bool,
    ) {
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            actor.tick().unwrap();
            let snapshot = self.snapshot();
            if predicate(&snapshot) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let snapshot = self.snapshot();
        panic!(
            "actor stalled at {:?}; tasks: {:?}; decisions: {:?}",
            snapshot.mission.phase,
            snapshot
                .tasks
                .iter()
                .map(|t| (&t.title, t.state, &t.blocked_code))
                .collect::<Vec<_>>(),
            snapshot.decisions
        );
    }
}
fn rpc(service: &MissionService, conn: &ConnectionId, method: &str, value: Value) -> Value {
    service
        .handle(conn, method, &value)
        .unwrap_or_else(|e| panic!("{method}: {e:?}"))
        .result
}
fn context(run: &RunStart) -> Value {
    serde_json::from_str(
        run.prompt_stdin
            .split("TASK_CONTEXT_JSON\n")
            .nth(1)
            .unwrap()
            .split("\nEND_TASK_CONTEXT_JSON")
            .next()
            .unwrap(),
    )
    .unwrap()
}
fn script(result: ProviderResult) -> FakeScript {
    FakeScript {
        steps: vec![
            FakeStep::Started {
                session_id: Some("fixture".into()),
                turn_id: None,
            },
            FakeStep::Result { value: result },
        ],
        ..Default::default()
    }
}
fn scripted(script: FakeScript) -> Arc<dyn AgentAdapter> {
    let adapter = FakeAdapter::in_process(Arc::new(WallClock::new()));
    adapter.set_default_script(script);
    adapter
}
fn factory(seen: Arc<Mutex<Vec<(String, String)>>>) -> AdapterFactory {
    Arc::new(move |run| {
        let ctx = context(run);
        let kind = ctx["task_kind"].as_str().unwrap();
        seen.lock().unwrap().push((
            kind.into(),
            run.workspace.as_ref().unwrap().display().to_string(),
        ));
        let result = match kind {
            "plan" => {
                let tasks=["api","ui"].iter().map(|key|serde_json::from_value(json!({"local_key":key,"title":format!("Write {key}"),"kind":"implement","role":"builder","required":true,"parent_key":null,"depends_on_keys":[],"objective_text":format!("Create {key}.txt"),"requirement_ids":[ctx["requirements"][0]["id"]],"input_artifact_ids":[],"allowed_paths":[format!("{key}.txt")],"expected_outputs":["patch"],"verification_ids":[],"specialty":null,"binding_id":null,"replacement_of":null})).unwrap()).collect();
                ProviderResult::Plan {
                    based_on_plan_revision: ctx["plan_revision"].as_u64().unwrap() as u32,
                    tasks,
                    retire_task_ids: vec![],
                    rationale_text: "Disjoint files can be written independently.".into(),
                }
            }
            "implement" => {
                let file = ctx["task_contract"]["allowed_paths"][0].as_str().unwrap();
                std::fs::write(
                    run.workspace.as_ref().unwrap().join(file),
                    format!("{file} works\n"),
                )
                .unwrap();
                ProviderResult::Patch {
                    report_text: "Implemented.".into(),
                    verification_claims: vec![],
                }
            }
            "review" => {
                assert!(run.workspace.as_ref().unwrap().join("api.txt").is_file());
                assert!(run.workspace.as_ref().unwrap().join("ui.txt").is_file());
                assert_eq!(
                    run.workspace_access,
                    iyagi_termd_lib::agent_runtime::WorkspaceAccess::ReadOnly
                );
                ProviderResult::Review {
                    candidate_id: serde_json::from_value(ctx["candidate"]["id"].clone()).unwrap(),
                    report_text: "The candidate contains both requested files.".into(),
                    findings: vec![],
                }
            }
            other => panic!("unexpected adapter task {other}"),
        };
        Ok(scripted(script(result)))
    })
}

#[test]
fn actor_drives_plan_writers_real_verification_review_and_user_acceptance() {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let seen = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(factory(seen.clone()));
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.runs.len(), 5);
    assert!(snapshot.runs.iter().all(|r| r.state == RunState::Succeeded));
    assert_eq!(snapshot.verifications.len(), 1);
    assert_eq!(snapshot.verifications[0].status, VerificationStatus::Passed);
    assert_eq!(snapshot.candidates.len(), 3);
    assert_eq!(snapshot.mission.automatic_start_count, 5);
    assert!(rig.storage.mission_outbox().unwrap().is_empty());
    assert_eq!(actor.live_count(), 0);
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
    assert!(!rig.repo.path().join("api.txt").exists());
    assert_eq!(git(rig.repo.path(), &["status", "--porcelain"]), "");
    let observed = seen.lock().unwrap();
    assert_eq!(observed.len(), 4);
    assert_eq!(
        observed
            .iter()
            .map(|(_, p)| p)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
    rpc(
        &rig.service,
        &rig.conn,
        "mission.accept",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"candidate_id":snapshot.mission.candidate_id,"acknowledged_verification_ids":[snapshot.verifications[0].id],"human_requirement_ids":[]}),
    );
    assert_eq!(rig.snapshot().mission.state, MissionState::Completed);
    assert!(
        rig.storage.mission_bindings().unwrap()[0]
            .get("local_evidence")
            .is_none(),
        "the fixture runtime is a test adapter, not an installed CLI: five \
         successful Runs on it are not compatibility evidence (11 §7)"
    );
}

/// 11 §7: a Run that finished is the strongest thing this machine knows about
/// the CLI it used. The counters are written after the transition commits,
/// they distinguish the two workspace modes, and they never reach back into
/// the Run they came from.
#[test]
fn finished_runs_record_what_they_proved_about_the_installed_cli() {
    let mut rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    // The same connection, now naming an installed CLI. Only the installation
    // observation and the capability registry are injected; the gate, the
    // snapshot, the commit path and the actor are the production ones.
    rig.service = Arc::new(
        MissionService::new(
            rig.storage.clone(),
            ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
        )
        .with_binding_evidence(
            |_, _| Ok("0.154.0".into()),
            |_, _, _| fake_binding().capabilities,
        ),
    );
    let mut binding: Binding =
        serde_json::from_value(rig.storage.mission_bindings().unwrap().remove(0)).unwrap();
    binding.runtime = RuntimeKind::Codex;
    binding.provider_id = "openai".into();
    binding.auth_route = AuthRoute::Subscription;
    binding.model_id = "gpt-fixture".into();
    let binding_id = binding.id.clone();
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":binding.revision,"binding":binding}),
    );
    rpc(
        &rig.service,
        &rig.conn,
        "binding.probe",
        json!({"binding_id":binding_id}),
    );
    let seen = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(factory(seen.clone()));
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    assert!(snapshot.runs.iter().all(|r| r.state == RunState::Succeeded));
    let started = seen.lock().unwrap().clone();
    let writes = started
        .iter()
        .filter(|(kind, _)| kind == "implement")
        .count();
    let reads = started.len() - writes;
    assert!(
        writes > 0 && reads > 0,
        "both workspace modes were exercised"
    );

    let evidence = rig.storage.mission_bindings().unwrap().remove(0)["local_evidence"].clone();
    assert_eq!(evidence["os"], std::env::consts::OS);
    assert_eq!(evidence["version"], "0.154.0");
    assert_eq!(evidence["model_id"], "gpt-fixture");
    assert_eq!(evidence["runs"]["succeeded_read_only"], reads);
    assert_eq!(evidence["runs"]["succeeded_write"], writes);
    assert_eq!(evidence["runs"]["cancelled"], 0);
    assert_eq!(evidence["runs"]["invalid_result"], 0);
    assert!(evidence["runs"]["last_at"].is_string());
    assert!(
        evidence["probe"].is_null(),
        "an injected registry states capabilities; it does not measure them"
    );
    // The deterministic integration Run has no connection to learn about, so
    // the counters never add up to more Runs than a CLI actually served.
    assert_eq!(
        started.len(),
        snapshot
            .runs
            .iter()
            .filter(|r| r.binding_snapshot.is_some())
            .count()
    );
    // Observing a Run cannot change it.
    assert_eq!(rig.snapshot().runs, snapshot.runs);
}

#[test]
fn start_failure_without_a_process_becomes_a_recorded_failure() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let factory: AdapterFactory =
        Arc::new(|_| Err(std::io::Error::other("provider is not installed")));
    let mut actor = rig.actor(factory);
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::Failed)
    });
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.runs[0].state, RunState::Failed);
    assert_eq!(snapshot.tasks[0].state, TaskState::Failed);
    assert!(rig.storage.mission_outbox().unwrap().is_empty());
    assert_eq!(actor.live_count(), 0);
}

#[test]
fn manual_plan_stays_blocked_until_a_user_adopts_the_proposal() {
    let rig = Rig::new(false, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let seen = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(factory(seen.clone()));
    rig.tick_until(&mut actor, |s| !s.decisions.is_empty());
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.mission.plan_revision, 0);
    for _ in 0..3 {
        actor.tick().unwrap();
    }
    assert_eq!(seen.lock().unwrap().len(), 1);
    let proposal = snapshot.decisions[0].question_ref.clone();
    let params = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"proposal_ref":proposal});
    let result = rpc(
        &rig.service,
        &rig.conn,
        "mission.plan.apply",
        params.clone(),
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.plan.apply", params),
        result,
        "request replay survives the changed plan revision"
    );
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
}

#[test]
fn cancellation_waits_for_cleanup_and_acknowledges_the_cancel_intent() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let factory: AdapterFactory = Arc::new(|_| {
        Ok(scripted(FakeScript {
            steps: vec![
                FakeStep::Started {
                    session_id: None,
                    turn_id: None,
                },
                FakeStep::Delay { ms: 100 },
                FakeStep::Result {
                    value: ProviderResult::Blocked {
                        code: "stopped".into(),
                        report_text: "done".into(),
                    },
                },
            ],
            ..Default::default()
        }))
    });
    let mut actor = rig.actor(factory);
    actor.tick().unwrap();
    let snapshot = rig.snapshot();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"action":"cancel"}),
    );
    assert_eq!(rig.snapshot().mission.state, MissionState::Stopping);
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Cancelled);
    assert_eq!(actor.live_count(), 0);
    assert!(rig.storage.mission_outbox().unwrap().is_empty());
    assert!(rig
        .snapshot()
        .runs
        .iter()
        .all(|r| r.state == RunState::Cancelled));
}

#[test]
fn review_repair_creates_a_new_candidate_and_repeats_verification_and_review() {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let seen = Arc::new(Mutex::new(vec![]));
    let normal = factory(seen);
    let scripted_factory: AdapterFactory = Arc::new(move |run| {
        let ctx = context(run);
        let kind = ctx["task_kind"].as_str().unwrap();
        if kind == "review" && ctx["candidate"]["revision"] == 1 {
            return Ok(scripted(script(ProviderResult::Review {
                candidate_id: serde_json::from_value(ctx["candidate"]["id"].clone()).unwrap(),
                report_text: "API content needs correction.".into(),
                findings: vec![ProviderFindingDraft {
                    severity: FindingSeverity::Major,
                    path: Some("api.txt".into()),
                    line: Some(1),
                    evidence_text: "Expected corrected API content.".into(),
                    requirement_id: None,
                }],
            })));
        }
        if kind == "plan" && ctx["plan_revision"] == 1 {
            let spec=serde_json::from_value(json!({"local_key":"repair_api","title":"Repair API","kind":"implement","role":"builder","required":true,"parent_key":null,"depends_on_keys":[],"objective_text":"Correct api.txt on the reviewed candidate","requirement_ids":[ctx["requirements"][0]["id"]],"input_artifact_ids":[],"allowed_paths":["api.txt"],"expected_outputs":["patch"],"verification_ids":[],"specialty":null,"binding_id":null,"replacement_of":null})).unwrap();
            return Ok(scripted(script(ProviderResult::Plan {
                based_on_plan_revision: 1,
                tasks: vec![spec],
                retire_task_ids: vec![],
                rationale_text: "Repair the review finding on a new candidate.".into(),
            })));
        }
        if kind == "implement" && ctx["plan_revision"] == 2 {
            assert_eq!(
                std::fs::read_to_string(run.workspace.as_ref().unwrap().join("api.txt")).unwrap(),
                "api.txt works\n"
            );
            std::fs::write(
                run.workspace.as_ref().unwrap().join("api.txt"),
                "corrected API\n",
            )
            .unwrap();
            return Ok(scripted(script(ProviderResult::Patch {
                report_text: "Corrected API.".into(),
                verification_claims: vec![],
            })));
        }
        if kind == "review" && ctx["candidate"]["revision"] == 2 {
            assert_eq!(
                std::fs::read_to_string(run.workspace.as_ref().unwrap().join("api.txt")).unwrap(),
                "corrected API\n"
            );
        }
        normal(run)
    });
    let mut actor = rig.actor(scripted_factory);
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.mission.plan_revision, 2);
    assert_eq!(snapshot.verifications.len(), 2);
    let candidate = snapshot
        .candidates
        .iter()
        .find(|c| Some(&c.id) == snapshot.mission.candidate_id.as_ref())
        .unwrap();
    assert_eq!(candidate.revision, 2);
    assert!(candidate.supersedes_id.is_some());
    assert_ne!(
        snapshot.verifications[0].candidate_id,
        snapshot.verifications[1].candidate_id
    );
    assert_eq!(
        snapshot
            .tasks
            .iter()
            .filter(|t| t.kind == TaskKind::Review && t.state == TaskState::Succeeded)
            .count(),
        2
    );
    assert_eq!(
        snapshot.findings[0].resolution,
        FindingResolution::Open,
        "prior findings remain immutable evidence"
    );
    let stale = rig.service.handle(&rig.conn,"mission.finding.resolve",&json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":snapshot.mission.revision,"finding_id":snapshot.findings[0].id,"resolution":"dismissed","reason_ref":snapshot.mission.goal_ref})).err().expect("prior candidate finding must remain immutable");
    assert_eq!(
        stale.code,
        term_contracts::mission::MissionErrorCode::StaleCandidate
    );
    let current_checks: Vec<_> = snapshot
        .verifications
        .iter()
        .filter(|v| v.candidate_id == candidate.id)
        .map(|v| v.id.clone())
        .collect();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.accept",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"candidate_id":candidate.id,"acknowledged_verification_ids":current_checks,"human_requirement_ids":[]}),
    );
    assert_eq!(rig.snapshot().mission.state, MissionState::Completed);
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
}

#[test]
#[cfg(unix)]
fn pause_during_verification_preserves_the_new_revision_and_waits_for_completion() {
    let rig = Rig::new(
        true,
        &[
            "-c",
            "alias.delayed=!sleep 0.3; git ls-files --error-unmatch api.txt ui.txt",
            "delayed",
        ],
    );
    let mut actor = rig.actor(factory(Arc::new(Mutex::new(vec![]))));
    rig.tick_until(&mut actor, |s| {
        s.runs
            .iter()
            .any(|r| r.binding_snapshot.is_none() && r.state == RunState::Running)
    });
    let snapshot = rig.snapshot();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"action":"pause"}),
    );
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Paused);
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.verifications.len(), 1);
    assert_eq!(snapshot.verifications[0].status, VerificationStatus::Passed);
    assert_eq!(actor.live_count(), 0);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"action":"resume"}),
    );
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
}

#[test]
fn decision_answer_applies_a_manual_plan_atomically_and_replays_the_response() {
    let rig = Rig::new(false, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let mut actor = rig.actor(factory(Arc::new(Mutex::new(vec![]))));
    rig.tick_until(&mut actor, |s| !s.decisions.is_empty());
    let snapshot = rig.snapshot();
    let params = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"decision_id":snapshot.decisions[0].id,"option_id":"apply","answer_ref":null});
    let first = rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        params.clone(),
    );
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.mission.plan_revision, 1);
    assert_eq!(snapshot.mission.open_decision_count, 0);
    assert_eq!(snapshot.decisions[0].state, DecisionState::Answered);
    assert_eq!(snapshot.tasks.len(), 3);
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.decision.answer", params),
        first
    );
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
}

#[test]
fn approval_answer_is_delivered_to_the_exact_live_provider_request() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let factory: AdapterFactory = Arc::new(|_| {
        Ok(scripted(FakeScript {
            steps: vec![
                FakeStep::Started {
                    session_id: Some("approval-test".into()),
                    turn_id: None,
                },
                FakeStep::Approval {
                    request_id: "request-7".into(),
                    question: "Read the permitted file?".into(),
                },
                FakeStep::Result {
                    value: ProviderResult::Blocked {
                        code: "fixture_finished".into(),
                        report_text: "Approval received.".into(),
                    },
                },
            ],
            ..Default::default()
        }))
    });
    let mut actor = rig.actor(factory);
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::AwaitingInput)
    });
    let snapshot = rig.snapshot();
    let params = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"decision_id":snapshot.decisions[0].id,"option_id":"accept","answer_ref":null});
    let first = rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        params.clone(),
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.decision.answer", params),
        first
    );
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().all(|r| r.state == RunState::Succeeded)
    });
    actor.tick().unwrap();
    let settled = rig.snapshot();
    assert_eq!(
        settled
            .decisions
            .iter()
            .find(|d| d.kind == DecisionKind::Approval)
            .unwrap()
            .state,
        DecisionState::Answered
    );
    // The final `blocked` result asks how to continue instead of stalling.
    let open: Vec<_> = settled
        .decisions
        .iter()
        .filter(|d| d.state == DecisionState::Open)
        .collect();
    assert_eq!(settled.mission.open_decision_count as usize, open.len());
    assert!(open.iter().all(|d| d.kind == DecisionKind::Recovery
        && d.requesting_run_id.as_ref() == Some(&settled.runs[0].id)));
    assert!(rig.storage.mission_outbox().unwrap().is_empty());
    let snapshot = rig.storage.mission_snapshot(&rig.id).unwrap().unwrap();
    assert!(snapshot.entities.iter().any(|e|matches!(e,Entity::Message(m) if m.delivery==MessageDelivery::Delivered && m.run_id.is_some())));
}

#[test]
fn recovery_fences_sent_and_acknowledged_runs_without_resending() {
    use iyagi_termd_lib::agent_runtime::AdapterEvent;
    use term_storage::mission::types::OutboxState;
    for acknowledged in [false, true] {
        let rig = Rig::new(true, &["status", "--short"]);
        rig.service.dispatch_tick().unwrap();
        let intent = rig
            .storage
            .mission_outbox()
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let prepared = rig
            .service
            .prepare_run(&intent, &rig.dir.path().join("missions"))
            .unwrap()
            .unwrap();
        let started = AdapterEvent::Started {
            run_id: prepared.start.run_id.clone(),
            fencing_token: prepared.start.fencing_token,
            provider_session_id: Some("prior-session".into()),
            provider_turn_id: Some("prior-turn".into()),
        };
        if acknowledged {
            rig.service
                .apply_adapter_event(&rig.id, &started, Some(&prepared.workspace))
                .unwrap();
        }
        assert_eq!(rig.service.recover_on_startup().unwrap().inspect, 1);
        let snapshot = rig.snapshot();
        let recovered = &snapshot.runs[0];
        assert_eq!(recovered.state, RunState::Unknown);
        assert!(recovered.fencing_token.get() > prepared.start.fencing_token);
        assert_eq!(
            snapshot.tasks[0].active_run_id.as_ref(),
            Some(&recovered.id)
        );
        assert_eq!(
            snapshot
                .decisions
                .iter()
                .filter(|d| d.kind == DecisionKind::Recovery && d.state == DecisionState::Open)
                .count(),
            1
        );
        let revision = snapshot.mission.revision;
        rig.service.recover_on_startup().unwrap();
        assert_eq!(
            rig.snapshot().mission.revision,
            revision,
            "recovery scan is idempotent"
        );
        rig.service
            .apply_adapter_event(&rig.id, &started, Some(&prepared.workspace))
            .unwrap();
        assert_eq!(
            rig.snapshot().mission.revision,
            revision,
            "prior actor callback is fenced"
        );
        let mut actor = rig.actor(Arc::new(|_| panic!("unknown run must never be resent")));
        actor.tick().unwrap();
        assert_eq!(rig.snapshot().runs.len(), 1);
        if !acknowledged {
            assert_eq!(
                rig.storage.mission_outbox().unwrap()[0].state,
                OutboxState::Unknown
            );
        }
    }
}

#[test]
fn explicit_unsent_recovery_authorization_survives_another_restart() {
    let rig = Rig::with_recovery_policy(true, &["status", "--short"], false);
    rig.service.dispatch_tick().unwrap();
    let original_run = rig.snapshot().runs[0].id.clone();
    assert_eq!(rig.service.recover_on_startup().unwrap().held, 1);
    let snapshot = rig.snapshot();
    assert_eq!(
        snapshot.tasks[0].blocked_code.as_deref(),
        Some("recovery_held")
    );
    let decision = snapshot
        .decisions
        .iter()
        .find(|d| d.kind == DecisionKind::Recovery)
        .unwrap();
    let mut held_actor = rig.actor(Arc::new(|_| panic!("unsent policy requires a decision")));
    held_actor.tick().unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":snapshot.mission.revision,"decision_id":decision.id,"option_id":"resume_unsent","answer_ref":null}),
    );
    let answered_revision = rig.snapshot().mission.revision;
    assert_eq!(rig.service.recover_on_startup().unwrap().dispatch, 1);
    assert_eq!(
        rig.snapshot().mission.revision,
        answered_revision,
        "do not ask twice for the same authorization"
    );
    let seen = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(factory(seen.clone()));
    actor.tick().unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(
        rig.snapshot().runs[0].id,
        original_run,
        "resume the same intent without incrementing attempt"
    );
    actor.shutdown();
}

#[test]
fn pause_and_cancel_before_send_never_launch_a_provider() {
    let rig = Rig::new(true, &["status", "--short"]);
    rig.service.dispatch_tick().unwrap();
    let run_id = rig.snapshot().runs[0].id.clone();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"action":"pause"}),
    );
    assert_eq!(rig.snapshot().mission.state, MissionState::Paused);
    let mut actor = rig.actor(Arc::new(|_| panic!("pause/cancel forbids sending")));
    actor.tick().unwrap();
    assert_eq!(rig.snapshot().runs[0].state, RunState::Prepared);
    assert_eq!(rig.snapshot().runs[0].id, run_id);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,
        "expected_revision":rig.snapshot().mission.revision,"action":"cancel"}),
    );
    actor.tick().unwrap();
    assert_eq!(rig.snapshot().mission.state, MissionState::Cancelled);
    assert_eq!(rig.snapshot().runs[0].state, RunState::Cancelled);
    assert!(rig.snapshot().tasks[0].active_run_id.is_none());
    assert!(
        rig.storage.mission_outbox().unwrap().is_empty(),
        "no abandoned prepared intents"
    );
}

#[test]
fn dependent_writers_receive_transitive_patches_from_multiple_branches() {
    let rig = Rig::new(
        true,
        &[
            "ls-files",
            "--error-unmatch",
            "api.txt",
            "ui.txt",
            "summary.txt",
        ],
    );
    let seen = Arc::new(Mutex::new(vec![]));
    let normal = factory(seen);
    let provider: AdapterFactory = Arc::new(move |run| {
        let ctx = context(run);
        if ctx["task_kind"] == "plan" {
            let specs = [
                ("api", "api.txt", vec![]),
                ("ui", "ui.txt", vec![]),
                ("refine", "api.txt", vec!["api"]),
                ("combine", "summary.txt", vec!["ui", "refine"]),
            ];
            let tasks = specs.into_iter().map(|(key,path,deps)| serde_json::from_value(json!({"local_key":key,"title":key,
                "kind":"implement","role":"builder","required":true,"parent_key":null,"depends_on_keys":deps,
                "objective_text":key,"requirement_ids":[ctx["requirements"][0]["id"]],"input_artifact_ids":[],
                "allowed_paths":[path],"expected_outputs":["patch"],"verification_ids":[],"specialty":null,"binding_id":null,"replacement_of":null})).unwrap()).collect();
            return Ok(scripted(script(ProviderResult::Plan {
                based_on_plan_revision: 0,
                tasks,
                retire_task_ids: vec![],
                rationale_text: "A dependent writer needs both branches of the DAG.".into(),
            })));
        }
        if ctx["objective"] == "refine" || ctx["objective"] == "combine" {
            let path = run.workspace.as_ref().unwrap();
            let api = std::fs::read_to_string(path.join("api.txt"))
                .expect("dependency file in actual workspace");
            if ctx["objective"] == "refine" {
                assert_eq!(api, "api.txt works\n");
                std::fs::write(path.join("api.txt"), "refined API\n").unwrap();
            } else {
                assert_eq!(api, "refined API\n", "transitive patch order");
                assert!(path.join("ui.txt").is_file(), "second branch also applied");
                std::fs::write(
                    path.join("summary.txt"),
                    "Both dependency branches are present.\n",
                )
                .unwrap();
            }
            return Ok(scripted(script(ProviderResult::Patch {
                report_text: "Applied dependent work.".into(),
                verification_claims: vec![],
            })));
        }
        normal(run)
    });
    let mut actor = rig.actor(provider);
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    assert_eq!(
        snapshot
            .tasks
            .iter()
            .filter(|t| t.kind == TaskKind::Implement && t.state == TaskState::Succeeded)
            .count(),
        4
    );
    assert!(snapshot
        .verifications
        .iter()
        .all(|v| v.status == VerificationStatus::Passed));
    let candidate = snapshot
        .candidates
        .iter()
        .find(|c| Some(&c.id) == snapshot.mission.candidate_id.as_ref())
        .unwrap();
    assert_eq!(
        git(
            rig.repo.path(),
            &["show", &format!("{}:api.txt", candidate.commit_oid)]
        ),
        "refined API"
    );
    assert_eq!(git(rig.repo.path(), &["rev-parse", "HEAD"]), rig.base);
    assert!(git(rig.repo.path(), &["status", "--porcelain"]).is_empty());
}

#[test]
fn observed_model_survives_reopen_and_ignores_stale_or_terminal_events() {
    use iyagi_termd_lib::agent_runtime::AdapterEvent;
    let rig = Rig::new(true, &["status", "--short"]);
    rig.service.dispatch_tick().unwrap();
    let intent = rig
        .storage
        .mission_outbox()
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let prepared = rig
        .service
        .prepare_run(&intent, &rig.dir.path().join("missions"))
        .unwrap()
        .unwrap();
    let run_id = prepared.start.run_id;
    let token = prepared.start.fencing_token;
    let event = |token, model: &str| AdapterEvent::ModelObserved {
        run_id: run_id.clone(),
        fencing_token: token,
        model: model.into(),
    };
    let revision = rig.snapshot().mission.revision;
    assert!(!rig
        .service
        .apply_adapter_event(&rig.id, &event(token + 1, "stale"), None)
        .unwrap());
    assert_eq!(rig.snapshot().mission.revision, revision);
    // The return value denotes a terminal event, not whether a write occurred.
    assert!(!rig
        .service
        .apply_adapter_event(&rig.id, &event(token, "confirmed-model"), None)
        .unwrap());
    let reopened = Storage::open(rig.dir.path().join("state.db")).unwrap();
    let snapshot = workflow::load_entities(&reopened, &rig.id).unwrap();
    let run = snapshot.runs.iter().find(|run| run.id == run_id).unwrap();
    assert_eq!(run.observed_model.as_deref(), Some("confirmed-model"));
    assert_eq!(run.requested_model.as_deref(), Some("fake-model"));
    assert!(rig
        .service
        .apply_adapter_event(
            &rig.id,
            &AdapterEvent::Failed {
                run_id: run_id.clone(),
                fencing_token: token,
                code: term_contracts::mission::MissionErrorCode::ModelUnavailable,
                message: "test ended".into(),
            },
            Some(&prepared.workspace)
        )
        .unwrap());
    let revision = rig.snapshot().mission.revision;
    assert!(!rig
        .service
        .apply_adapter_event(&rig.id, &event(token, "late"), None)
        .unwrap());
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.mission.revision, revision);
    assert_eq!(
        snapshot
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .unwrap()
            .observed_model
            .as_deref(),
        Some("confirmed-model")
    );
}

#[test]
fn durable_activity_is_bounded_paginated_and_fenced_without_per_delta_revisions() {
    use iyagi_termd_lib::agent_runtime::AdapterEvent;
    let rig = Rig::new(true, &["status", "--short"]);
    rig.service.dispatch_tick().unwrap();
    let intent = rig
        .storage
        .mission_outbox()
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let prepared = rig
        .service
        .prepare_run(&intent, &rig.dir.path().join("missions"))
        .unwrap()
        .unwrap();
    let revision = rig.snapshot().mission.revision.get();
    let id = prepared.start.run_id.clone();
    let token = prepared.start.fencing_token;
    for _ in 0..50 {
        rig.service
            .apply_adapter_event(
                &rig.id,
                &AdapterEvent::Activity {
                    run_id: id.clone(),
                    fencing_token: token,
                    chunk: "\u{1b}[31mactivity\u{1b}[0m\n".into(),
                },
                Some(&prepared.workspace),
            )
            .unwrap();
    }
    assert!(
        rig.snapshot().mission.revision.get() - revision < 5,
        "text does not change mission revision for each delta"
    );
    let first = rpc(
        &rig.service,
        &rig.conn,
        "mission.activity",
        json!({"mission_id":rig.id,"run_id":id,"after_offset":"0","max_bytes":16}),
    );
    assert_eq!(first["next_offset"], "16");
    assert_eq!(first["complete"], false);
    let body = rpc(
        &rig.service,
        &rig.conn,
        "artifact.read",
        json!({"artifact_id":first["body_ref"]["id"],"offset":"0","max_bytes":4096}),
    );
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(body["data_b64"].as_str().unwrap())
            .unwrap(),
        b"activity\nactivit"
    );
    let long = "\u{ac00}".repeat(400000);
    rig.service
        .apply_adapter_event(
            &rig.id,
            &AdapterEvent::Activity {
                run_id: id.clone(),
                fencing_token: token,
                chunk: long,
            },
            Some(&prepared.workspace),
        )
        .unwrap();
    let reopened = MissionService::new(
        rig.storage.clone(),
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
    );
    let mut offset = "0".to_string();
    let mut total = 0;
    loop {
        let page = rpc(
            &reopened,
            &rig.conn,
            "mission.activity",
            json!({"mission_id":rig.id,"run_id":id,"after_offset":offset,"max_bytes":65536}),
        );
        let next = page["next_offset"].as_str().unwrap().to_string();
        if page["body_ref"].is_null() {
            assert_eq!(next, offset);
            break;
        }
        total += page["body_ref"]["bytes"]
            .as_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        offset = next;
    }
    assert!((1024 * 1024 - 4..=1024 * 1024).contains(&total));
    let end = offset.clone();
    rig.service
        .apply_adapter_event(
            &rig.id,
            &AdapterEvent::Failed {
                run_id: id.clone(),
                fencing_token: token,
                code: term_contracts::mission::MissionErrorCode::ProviderUnavailable,
                message: "closed for test".into(),
            },
            Some(&prepared.workspace),
        )
        .unwrap();
    assert!(!rig
        .service
        .apply_adapter_event(
            &rig.id,
            &AdapterEvent::Activity {
                run_id: id.clone(),
                fencing_token: token,
                chunk: "late".into()
            },
            None
        )
        .unwrap());
    let final_page = rpc(
        &reopened,
        &rig.conn,
        "mission.activity",
        json!({"mission_id":rig.id,"run_id":id,"after_offset":end,"max_bytes":65536}),
    );
    assert_eq!(final_page["complete"], true);
    assert!(final_page["body_ref"].is_null());
}

#[test]
fn policy_updates_preserve_reserved_run_snapshots_and_replay_after_cancellation() {
    let rig = Rig::new(true, &["status", "--short"]);
    rig.service.dispatch_tick().unwrap();
    let before = rig.snapshot();
    let binding_snapshot = before.runs[0].binding_snapshot.clone();
    let mut policy = before.mission.policy.clone();
    policy.run_time_limit_ms = term_contracts::ids::U64String::new(1000).unwrap();
    let reduced = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":before.mission.revision,
        "policy":policy,"role_bindings":before.mission.role_bindings});
    let error = rig
        .service
        .handle(&rig.conn, "mission.policy.update", &reduced)
        .err()
        .expect("unsafe reduction rejected");
    assert_eq!(
        error.code,
        term_contracts::mission::MissionErrorCode::InvalidState
    );
    assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
    policy = before.mission.policy.clone();
    policy.max_attempts_per_task += 1;
    let expanded = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":before.mission.revision,
        "policy":policy,"role_bindings":before.mission.role_bindings});
    let applied = rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        expanded.clone(),
    );
    assert_eq!(rig.snapshot().mission.policy.max_attempts_per_task, 4);
    assert_eq!(rig.snapshot().runs[0].binding_snapshot, binding_snapshot);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":"cancel"}),
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.policy.update", expanded),
        applied,
        "replay precedes terminal-state validation"
    );
}

#[test]
fn current_finding_dismissal_requires_owned_reason_and_is_idempotent() {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let normal = factory(Arc::new(Mutex::new(vec![])));
    let provider: AdapterFactory = Arc::new(move |run| {
        let ctx = context(run);
        if ctx["task_kind"] == "review" {
            return Ok(scripted(script(ProviderResult::Review {
                candidate_id: serde_json::from_value(ctx["candidate"]["id"].clone()).unwrap(),
                report_text: "One optional naming suggestion.".into(),
                findings: vec![ProviderFindingDraft {
                    severity: FindingSeverity::Minor,
                    path: Some("api.txt".into()),
                    line: Some(1),
                    evidence_text: "Consider a more specific name.".into(),
                    requirement_id: None,
                }],
            })));
        }
        normal(run)
    });
    let mut actor = rig.actor(provider);
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    let store = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
    let reason = workflow::store_artifact(
        &store,
        &rig.id,
        "text/plain",
        b"The current name is required by our public API.",
    )
    .unwrap();
    let answer = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,
        "finding_id":snapshot.findings[0].id,"resolution":"dismissed","reason_ref":reason});
    let mut forged = answer.clone();
    forged["reason_ref"]["sha256"] = json!("0".repeat(64));
    assert!(rig
        .service
        .handle(&rig.conn, "mission.finding.resolve", &forged)
        .is_err());
    assert_eq!(
        rig.snapshot().findings[0].resolution,
        FindingResolution::Open
    );
    let applied = rpc(
        &rig.service,
        &rig.conn,
        "mission.finding.resolve",
        answer.clone(),
    );
    assert_eq!(
        rig.snapshot().findings[0].resolution,
        FindingResolution::Dismissed
    );
    assert_eq!(
        rig.snapshot().findings[0].resolution_ref.as_ref(),
        Some(&reason)
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.finding.resolve", answer),
        applied
    );
    assert_eq!(
        rig.snapshot().mission.candidate_id,
        snapshot.mission.candidate_id
    );
}

#[test]
fn uncertain_approval_receipt_is_persisted_unknown_without_retransmission() {
    use iyagi_termd_lib::agent_runtime::{CancelReceipt, DeliveryReceipt, EventStream, RunProbe};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct UncertainAnswer {
        inner: Arc<dyn AgentAdapter>,
        calls: Arc<AtomicUsize>,
    }
    impl AgentAdapter for UncertainAnswer {
        fn name(&self) -> &'static str {
            "uncertain-answer-fixture"
        }
        fn start(&self, run: RunStart) -> std::io::Result<()> {
            self.inner.start(run)
        }
        fn send_message(&self, id: &Id, body: &str) -> DeliveryReceipt {
            self.inner.send_message(id, body)
        }
        fn answer(&self, id: &Id, request: &str, answer: &str) -> DeliveryReceipt {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(matches!(
                self.inner.answer(id, request, answer),
                DeliveryReceipt::Delivered { .. }
            ));
            DeliveryReceipt::Unknown {
                reason: "response lost after provider accepted",
            }
        }
        fn interrupt(&self, id: &Id) -> CancelReceipt {
            self.inner.interrupt(id)
        }
        fn inspect(&self, id: &Id) -> RunProbe {
            self.inner.inspect(id)
        }
        fn close(&self, id: &Id) -> CancelReceipt {
            self.inner.close(id)
        }
        fn subscribe(&self) -> EventStream {
            self.inner.subscribe()
        }
    }
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let factory: AdapterFactory = Arc::new(move |_| {
        Ok(Arc::new(UncertainAnswer {
            calls: count.clone(),
            inner: scripted(FakeScript {
                steps: vec![
                    FakeStep::Started {
                        session_id: Some("receipt-test".into()),
                        turn_id: None,
                    },
                    FakeStep::Approval {
                        request_id: "request-uncertain".into(),
                        question: "Read permitted file?".into(),
                    },
                    FakeStep::Result {
                        value: ProviderResult::Blocked {
                            code: "fixture_finished".into(),
                            report_text: "Finished after receiving approval".into(),
                        },
                    },
                ],
                ..Default::default()
            }),
        }))
    });
    let mut actor = rig.actor(factory);
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::AwaitingInput)
    });
    let snapshot = rig.snapshot();
    let answer = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"decision_id":snapshot.decisions[0].id,"option_id":"accept","answer_ref":null});
    let initial = rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        answer.clone(),
    );
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().all(|r| r.state == RunState::Succeeded)
    });
    for _ in 0..3 {
        actor.tick().unwrap();
    }
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.decision.answer", answer),
        initial
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let stored = rig.storage.mission_snapshot(&rig.id).unwrap().unwrap();
    assert!(stored
        .entities
        .iter()
        .any(|e| matches!(e,Entity::Message(m)if m.delivery==MessageDelivery::Unknown)));
    assert!(rig
        .storage
        .mission_outbox()
        .unwrap()
        .iter()
        .any(
            |o| o.operation == term_storage::mission::types::OutboxOperation::Answer
                && o.state == term_storage::mission::types::OutboxState::Unknown
        ));
}

#[test]
fn one_review_cannot_advance_past_another_required_dependent_review() {
    let rig = Rig::new(true, &["ls-files", "--error-unmatch", "api.txt", "ui.txt"]);
    let seen = Arc::new(Mutex::new(vec![]));
    let base = factory(seen.clone());
    let factory: AdapterFactory = Arc::new(move |run| {
        let ctx = context(run);
        if ctx["task_kind"] != "plan" {
            return base(run);
        }
        let mut tasks = vec![];
        for key in ["api", "ui", "review_one", "review_two"] {
            let review = key.starts_with("review");
            let deps = match key {
                "review_one" => vec!["api", "ui"],
                "review_two" => vec!["review_one"],
                _ => vec![],
            };
            tasks.push(serde_json::from_value(json!({"local_key":key,"title":key,"kind":if review{"review"}else{"implement"},"role":if review{"reviewer"}else{"builder"},"required":true,"parent_key":null,"depends_on_keys":deps,"objective_text":key,"requirement_ids":[ctx["requirements"][0]["id"]],"input_artifact_ids":[],"allowed_paths":if review{vec![]}else{vec![format!("{key}.txt")]},"expected_outputs":[if review{"review"}else{"patch"}],"verification_ids":[],"specialty":null,"binding_id":null,"replacement_of":null})).unwrap());
        }
        Ok(scripted(script(ProviderResult::Plan {
            based_on_plan_revision: ctx["plan_revision"].as_u64().unwrap() as u32,
            tasks,
            retire_task_ids: vec![],
            rationale_text: "Both required reviews must complete in dependency order".into(),
        })))
    });
    let mut actor = rig.actor(factory);
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    let snapshot = rig.snapshot();
    let reviews: Vec<_> = snapshot
        .tasks
        .iter()
        .filter(|t| t.kind == TaskKind::Review)
        .collect();
    assert_eq!(reviews.len(), 2);
    assert!(reviews.iter().all(|t| t.state == TaskState::Succeeded));
    assert_eq!(
        seen.lock()
            .unwrap()
            .iter()
            .filter(|(kind, _)| kind == "review")
            .count(),
        2
    );
    assert_eq!(actor.live_count(), 0);
}

fn prepared_exec(rig: &Rig) -> (ExecRecord, Vec<u8>) {
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    let intent = rig
        .storage
        .mission_outbox()
        .unwrap()
        .into_iter()
        .find(|o| o.mission_id == rig.id)
        .unwrap();
    let prepared = rig
        .service
        .prepare_run(&intent, &rig.dir.path().join("missions"))
        .unwrap()
        .unwrap();
    let body = serde_json::to_vec(&json!({"program":prepared.start.binding.program,"argv":["--fixture"],"cwd":prepared.start.workspace,"env_keys":["PRIVATE_TOKEN"]})).unwrap();
    let record = ExecRecord {
        id: Id::generate(),
        mission_id: rig.id.clone(),
        run_id: prepared.start.run_id,
        state: ExecState::Prepared,
        identity: None,
        group_kind: None,
        group_reference: None,
        group_identity: None,
        resource_policy: prepared.start.binding.resource_policy,
        launch_manifest_ref: ArtifactRef {
            id: Id::generate(),
            sha256: format!("{:x}", Sha256::digest(&body)),
            bytes: term_contracts::ids::U64String::new(body.len() as u64).unwrap(),
            media_type: "application/json".into(),
        },
        owner_daemon_id: prepared.start.owner_daemon_id,
        started_at: None,
        ended_at: None,
        exit_code: None,
    };
    (record, body)
}
fn observed_spawn(mut record: ExecRecord) -> ExecRecord {
    record.state = ExecState::Spawned;
    record.identity =
        Some(term_platform::current_process_identity().expect("test process identity"));
    record.group_kind = Some(ExecGroupKind::ObservedTree);
    record.group_reference = Some(format!("owned-test-group:{}", record.id));
    record.started_at = Some(term_storage::time::now_iso8601());
    record
}
#[test]
fn exec_store_preparation_links_run_and_real_manifest_atomically_and_replays() {
    let rig = Rig::new(false, &["status", "--porcelain"]);
    let (mut record, body) = prepared_exec(&rig);
    let store = rig.service.exec_persistence();
    let reference = store.prepare(record.clone(), &body).unwrap();
    assert_ne!(reference.id, record.launch_manifest_ref.id);
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.execs.len(), 1);
    assert_eq!(snapshot.runs[0].exec_id.as_ref(), Some(&record.id));
    assert_eq!(snapshot.execs[0].launch_manifest_ref, reference);
    let artifacts = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
    assert_eq!(
        artifacts
            .read_mission_body(&rig.id, &reference, 256 * 1024)
            .unwrap(),
        body
    );
    assert!(artifacts
        .read_mission_body(&Id::generate(), &reference, 256 * 1024)
        .is_err());
    let revision = snapshot.mission.revision;
    assert_eq!(store.prepare(record.clone(), &body).unwrap(), reference);
    assert_eq!(
        rig.snapshot().mission.revision,
        revision,
        "retry created another event"
    );
    record.launch_manifest_ref = reference;
    let spawned = observed_spawn(record.clone());
    store.update(spawned.clone()).unwrap();
    let revision = rig.snapshot().mission.revision;
    store.update(spawned.clone()).unwrap();
    assert_eq!(rig.snapshot().mission.revision, revision);
    let mut changed = spawned.clone();
    changed
        .identity
        .as_mut()
        .unwrap()
        .start_token
        .push_str("-different");
    assert!(store.update(changed).is_err());
    let mut changed = spawned.clone();
    changed.started_at = Some("2099-01-01T00:00:00Z".into());
    assert!(store.update(changed).is_err());
    let mut exited = spawned.clone();
    exited.state = ExecState::Exited;
    exited.ended_at = Some(term_storage::time::now_iso8601());
    exited.exit_code = Some(0);
    store.update(exited.clone()).unwrap();
    let revision = rig.snapshot().mission.revision;
    exited.ended_at = Some("2099-01-01T00:00:00Z".into());
    store.update(exited).unwrap();
    assert_eq!(rig.snapshot().mission.revision, revision);
    assert!(
        store.update(spawned).is_err(),
        "Exited regressed to Spawned"
    );
    record.id = Id::generate();
    assert!(
        store.prepare(record, &body).is_err(),
        "run acquired a second exec"
    );
}
#[test]
fn exec_store_cancel_between_preparation_and_release_denies_launch_but_allows_cleanup() {
    let rig = Rig::new(false, &["status", "--porcelain"]);
    let (mut record, body) = prepared_exec(&rig);
    let store = rig.service.exec_persistence();
    record.launch_manifest_ref = store.prepare(record.clone(), &body).unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":"cancel"}),
    );
    let mut observed = observed_spawn(record);
    assert!(
        store.update(observed.clone()).is_err(),
        "cancelled run released its launch gate"
    );
    observed.state = ExecState::Exited;
    observed.ended_at = Some(term_storage::time::now_iso8601());
    store.update(observed).unwrap();
    assert_eq!(rig.snapshot().execs[0].state, ExecState::Exited);
}
#[test]
fn exec_store_rejects_unowned_mismatched_and_secret_bearing_manifests() {
    let rig = Rig::new(false, &["status", "--porcelain"]);
    let (record, body) = prepared_exec(&rig);
    let store = rig.service.exec_persistence();
    for field in ["owner", "run", "hash", "program", "env", "cwd"] {
        let mut changed = record.clone();
        let mut document: Value = serde_json::from_slice(&body).unwrap();
        match field {
            "owner" => changed.owner_daemon_id = Id::generate(),
            "run" => changed.run_id = Id::generate(),
            "hash" => changed.launch_manifest_ref.sha256 = "0".repeat(64),
            "program" => document["program"] = json!("/other/program"),
            "cwd" => document["cwd"] = json!(rig.repo.path()),
            "env" => document["env_values"] = json!({"PRIVATE_TOKEN":"should-not-store"}),
            _ => unreachable!(),
        }
        let modified = serde_json::to_vec(&document).unwrap();
        if field != "hash" {
            changed.launch_manifest_ref.sha256 = format!("{:x}", Sha256::digest(&modified));
            changed.launch_manifest_ref.bytes =
                term_contracts::ids::U64String::new(modified.len() as u64).unwrap();
        }
        assert!(
            store.prepare(changed, &modified).is_err(),
            "accepted invalid {field}"
        );
        assert!(rig.snapshot().execs.is_empty());
        assert!(rig.snapshot().runs[0].exec_id.is_none());
    }
}

struct SlowStart {
    inner: Arc<dyn AgentAdapter>,
    entered: Arc<std::sync::atomic::AtomicBool>,
    release: Arc<std::sync::atomic::AtomicBool>,
}
impl AgentAdapter for SlowStart {
    fn name(&self) -> &'static str {
        "slow-start-fixture"
    }
    fn start(&self, run: RunStart) -> std::io::Result<()> {
        use std::sync::atomic::Ordering;
        self.entered.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.release.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.inner.start(run)
    }
    fn send_message(&self, id: &Id, body: &str) -> iyagi_termd_lib::agent_runtime::DeliveryReceipt {
        self.inner.send_message(id, body)
    }
    fn answer(
        &self,
        id: &Id,
        request: &str,
        answer: &str,
    ) -> iyagi_termd_lib::agent_runtime::DeliveryReceipt {
        self.inner.answer(id, request, answer)
    }
    fn interrupt(&self, id: &Id) -> iyagi_termd_lib::agent_runtime::CancelReceipt {
        self.inner.interrupt(id)
    }
    fn close(&self, id: &Id) -> iyagi_termd_lib::agent_runtime::CancelReceipt {
        self.inner.close(id)
    }
    fn inspect(&self, id: &Id) -> iyagi_termd_lib::agent_runtime::RunProbe {
        self.inner.inspect(id)
    }
    fn subscribe(&self) -> iyagi_termd_lib::agent_runtime::EventStream {
        self.inner.subscribe()
    }
}
#[test]
fn slow_start_does_not_block_actor_and_cancel_is_reissued_after_registration() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let rig = Rig::new(false, &["status", "--porcelain"]);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let worker_entered = entered.clone();
    let worker_release = release.clone();
    let factory: AdapterFactory = Arc::new(move |_| {
        Ok(Arc::new(SlowStart {
            inner: scripted(FakeScript {
                steps: vec![
                    FakeStep::Started {
                        session_id: None,
                        turn_id: None,
                    },
                    FakeStep::Approval {
                        request_id: "wait-for-cancel".into(),
                        question: "cancel fixture".into(),
                    },
                ],
                ..Default::default()
            }),
            entered: worker_entered.clone(),
            release: worker_release.clone(),
        }))
    });
    let mut actor = rig.actor(factory);
    let before = Instant::now();
    actor.tick().unwrap();
    assert!(
        before.elapsed() < Duration::from_secs(1),
        "actor blocked on provider start"
    );
    let deadline = Instant::now() + Duration::from_secs(1);
    while !entered.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":"cancel"}),
    );
    actor.tick().unwrap();
    assert_eq!(rig.snapshot().mission.state, MissionState::Stopping);
    assert_eq!(
        actor.live_count(),
        1,
        "starting worker lost its reservation"
    );
    release.store(true, Ordering::Release);
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Cancelled);
    assert_eq!(actor.live_count(), 0);
    assert!(rig
        .snapshot()
        .runs
        .iter()
        .all(|r| r.state == RunState::Cancelled));
}

#[tokio::test]
async fn durable_exec_store_and_native_gate_run_together_with_real_owned_artifact() {
    use iyagi_termd_lib::exec::{gated::GateConfig, ExecSupervisor, SpawnRequest};
    let rig = Rig::new(false, &["status", "--porcelain"]);
    let daemon = std::path::PathBuf::from(env!("CARGO_BIN_EXE_iyagi-termd"));
    let fixture = daemon.parent().unwrap().join(if cfg!(windows) {
        "term-fixture.exe"
    } else {
        "term-fixture"
    });
    let mut binding: Binding =
        serde_json::from_value(rig.storage.mission_bindings().unwrap().remove(0)).unwrap();
    binding.program = fixture.to_string_lossy().into_owned();
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":binding.revision,"binding":binding}),
    );
    let (record, body) = prepared_exec(&rig);
    let manifest: Value = serde_json::from_slice(&body).unwrap();
    let gate_dir = tempfile::tempdir().unwrap();
    let supervisor = ExecSupervisor::persistent(
        term_core::AdmissionConfig {
            logical_cpus: 8,
            managed_concurrency: 2,
            telemetry_stale_ms: 3000,
            host_reserve_min_bytes: 2 << 30,
            host_reserve_percent: 15,
            managed_budget_percent: 50,
        },
        rig.service.exec_persistence(),
        term_core::AdmissionHost {
            total_bytes: 16 << 30,
            available_bytes: Some(10 << 30),
            sample_age_ms: 0,
            reconciliation_required: false,
            pressure: term_contracts::metrics::PressureLevel::Normal,
        },
        GateConfig {
            helper_program: daemon,
            directory: gate_dir.path().into(),
            platform: Arc::from(term_platform::group::select_backend()),
            timeout: Duration::from_secs(5),
        },
    );
    let marker = rig.dir.path().join("target-ran");
    supervisor.refresh_recovery().unwrap();
    let exec = supervisor
        .spawn(SpawnRequest {
            exec_id: record.id.clone(),
            mission_id: rig.id.clone(),
            run_id: record.run_id.clone(),
            owner_daemon_id: record.owner_daemon_id,
            program: fixture,
            argv: vec![
                "gate-observer".into(),
                "--marker".into(),
                marker.to_string_lossy().into_owned(),
            ],
            cwd: manifest["cwd"].as_str().unwrap().into(),
            env_overrides: Default::default(),
            env_clear: false,
            stdin: None,
            resource_policy: record.resource_policy,
            spool_bytes: 1024,
            redactor: None,
            sink: Arc::new(|_, _| {}),
            validate_path: None,
        })
        .await
        .unwrap();
    assert_eq!(exec.wait().await.unwrap().code, Some(0));
    assert!(marker.exists());
    let snapshot = rig.snapshot();
    assert_eq!(snapshot.execs.len(), 1);
    assert_eq!(snapshot.execs[0].state, ExecState::Exited);
    assert!(snapshot.execs[0].identity.is_some());
    assert_eq!(snapshot.runs[0].exec_id.as_ref(), Some(&record.id));
    let artifacts = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
    let actual = artifacts
        .read_mission_body(&rig.id, &snapshot.execs[0].launch_manifest_ref, 256 * 1024)
        .unwrap();
    let actual: Value = serde_json::from_slice(&actual).unwrap();
    assert_eq!(actual["argv"][0], "gate-observer");
    assert_eq!(actual["cwd"], manifest["cwd"]);
    assert_eq!(supervisor.ledger().active_count(), 0);
    // Lose the provider result after real native cleanup. A replacement
    // service must recover from the persisted Exec, not an in-memory handle.
    let old = snapshot.runs[0].clone();
    rig.service
        .apply_adapter_event(
            &rig.id,
            &iyagi_termd_lib::agent_runtime::AdapterEvent::Disconnected {
                run_id: old.id.clone(),
                fencing_token: old.fencing_token.get(),
            },
            None,
        )
        .unwrap();
    let restarted = MissionService::new(
        rig.storage.clone(),
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
    );
    restarted.recover_on_startup().unwrap();
    assert_eq!(restarted.dispatch_tick().unwrap(), 0);
    let after = rig.snapshot();
    assert_eq!(after.runs[0].state, RunState::Unknown);
    assert!(after.runs[0].reconciliation_ref.is_some());
    assert!(!after.runs[0].holds_execution_slot());
    assert!(after.tasks[0].active_run_id.is_none());
    assert_eq!(after.workspaces[0].state, WorkspaceState::Quarantined);
    assert!(after.workspaces[0].writer_run_id.is_none());
    assert!(after
        .decisions
        .iter()
        .any(|d| d.state == DecisionState::Open
            && d.options.iter().any(|o| o.id == "retry_reconciled_task")));
}
