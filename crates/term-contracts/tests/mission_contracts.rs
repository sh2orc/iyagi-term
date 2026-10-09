//! O1 contract fixture parity: every table here replays the machine-readable
//! reference assets (`states.json`, `cases.json`, `defaults.json`) so the
//! Rust tables and the docs cannot drift apart (ticket O02, 06 §2).

use std::collections::HashMap;

use serde::Deserialize;
use term_contracts::mission::plan::{validate_plan_graph, PlanNode};
use term_contracts::mission::types::{
    AuthRoute, MissionState, Phase, Role, RoleBinding, RunState, TaskState,
};
use term_contracts::mission::validation::{MissionLimits, PolicyCeiling};
use term_contracts::mission::PlanGraphError;

const STATES_JSON: &str = include_str!("../../../docs/orchestration/states.json");
const CASES_JSON: &str = include_str!("../../../docs/orchestration/cases.json");
const DEFAULTS_JSON: &str = include_str!("../../../docs/orchestration/defaults.json");

#[derive(Debug, Deserialize)]
struct StatesFile {
    mission: HashMap<String, Vec<String>>,
    task: HashMap<String, Vec<String>>,
    run: HashMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct CasesFile {
    transitions: Vec<TransitionCase>,
    plans: Vec<PlanCase>,
}

#[derive(Debug, Deserialize)]
struct TransitionCase {
    id: String,
    entity: String,
    from: String,
    to: String,
    allowed: bool,
}

#[derive(Debug, Deserialize)]
struct PlanCase {
    id: String,
    tasks: Vec<PlanTask>,
    expected: String,
}

#[derive(Debug, Deserialize)]
struct PlanTask {
    id: String,
    mission: String,
    deps: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DefaultsFile {
    max_tasks_per_mission: usize,
    max_plan_revisions: u32,
    max_delegation_depth: u32,
    max_specialist_requests_per_task: u32,
    max_title_bytes: usize,
    max_message_bytes: usize,
    artifact_chunk_bytes: usize,
    max_artifact_bytes: u64,
    max_artifact_bytes_per_mission: u64,
    max_context_bytes: usize,
    max_role_instruction_bytes: usize,
    policy_ceiling: PolicyCeilingJson,
}

#[derive(Debug, Deserialize)]
struct PolicyCeilingJson {
    max_parallel_runs: u32,
    max_attempts_per_task: u32,
    max_repair_cycles: u32,
    max_automatic_starts: u32,
    active_time_limit_ms: u64,
    run_time_limit_ms: u64,
}

fn edge_allowed(entity: &str, from: &str, to: &str) -> bool {
    let f = |name: &str| format!("\"{name}\"");
    match entity {
        "mission" => {
            let from: MissionState = serde_json::from_str(&f(from)).unwrap();
            let to: MissionState = serde_json::from_str(&f(to)).unwrap();
            from.can_transition(to)
        }
        "task" => {
            let from: TaskState = serde_json::from_str(&f(from)).unwrap();
            let to: TaskState = serde_json::from_str(&f(to)).unwrap();
            from.can_transition(to)
        }
        "run" => {
            let from: RunState = serde_json::from_str(&f(from)).unwrap();
            let to: RunState = serde_json::from_str(&f(to)).unwrap();
            from.can_transition(to)
        }
        _ => panic!("unknown entity {entity}"),
    }
}

/// Every (from, to) pair over the full state vocabulary must match the
/// reference graph — not just the sampled cases.
#[test]
fn transition_tables_match_states_json_exactly() {
    let states: StatesFile = serde_json::from_str(STATES_JSON).unwrap();
    for (entity, graph) in [
        ("mission", &states.mission),
        ("task", &states.task),
        ("run", &states.run),
    ] {
        let all_states: Vec<String> = graph.keys().cloned().collect();
        assert!(!all_states.is_empty());
        for from in &all_states {
            for to in &all_states {
                let reference = graph
                    .get(from)
                    .map(|targets| targets.contains(to))
                    .unwrap_or(false);
                let actual = edge_allowed(entity, from, to);
                assert_eq!(
                    actual, reference,
                    "{entity} {from} -> {to}: rust table says {actual}, states.json says {reference}"
                );
            }
        }
    }
}

#[test]
fn transition_cases_s01_to_s20_replay() {
    let cases: CasesFile = serde_json::from_str(CASES_JSON).unwrap();
    assert_eq!(cases.transitions.len(), 20);
    for case in &cases.transitions {
        assert_eq!(
            edge_allowed(&case.entity, &case.from, &case.to),
            case.allowed,
            "case {} ({} {} -> {})",
            case.id,
            case.entity,
            case.from,
            case.to
        );
    }
}

#[test]
fn plan_cases_p01_to_p07_replay() {
    let cases: CasesFile = serde_json::from_str(CASES_JSON).unwrap();
    assert_eq!(cases.plans.len(), 7);
    for case in &cases.plans {
        let nodes: Vec<PlanNode> = case
            .tasks
            .iter()
            .map(|t| PlanNode {
                id: t.id.clone(),
                mission: t.mission.clone(),
                deps: t.deps.clone(),
            })
            .collect();
        let outcome = validate_plan_graph(&nodes);
        let actual = match outcome {
            Ok(()) => "ok".to_string(),
            Err(PlanGraphError::Cycle) => "cycle".to_string(),
            Err(PlanGraphError::SelfEdge(_)) => "self".to_string(),
            Err(PlanGraphError::MissingDependency(_, _)) => "missing".to_string(),
            Err(PlanGraphError::DuplicateId(_)) => "duplicate".to_string(),
            Err(PlanGraphError::CrossMission(_, _)) => "cross_mission".to_string(),
            Err(PlanGraphError::DuplicateEdge(_, _)) => "duplicate_edge".to_string(),
        };
        assert_eq!(actual, case.expected, "plan case {}", case.id);
    }
}

#[test]
fn limits_and_ceiling_match_defaults_json() {
    let defaults: DefaultsFile = serde_json::from_str(DEFAULTS_JSON).unwrap();
    let limits = MissionLimits::load();
    assert_eq!(limits.max_tasks_per_mission, defaults.max_tasks_per_mission);
    assert_eq!(limits.max_plan_revisions, defaults.max_plan_revisions);
    assert_eq!(limits.max_delegation_depth, defaults.max_delegation_depth);
    assert_eq!(
        limits.max_specialist_requests_per_task,
        defaults.max_specialist_requests_per_task
    );
    assert_eq!(limits.max_title_bytes, defaults.max_title_bytes);
    assert_eq!(limits.max_message_bytes, defaults.max_message_bytes);
    assert_eq!(limits.artifact_chunk_bytes, defaults.artifact_chunk_bytes);
    assert_eq!(limits.max_artifact_bytes, defaults.max_artifact_bytes);
    assert_eq!(
        limits.max_artifact_bytes_per_mission,
        defaults.max_artifact_bytes_per_mission
    );
    assert_eq!(limits.max_context_bytes, defaults.max_context_bytes);
    assert_eq!(
        limits.max_role_instruction_bytes,
        defaults.max_role_instruction_bytes
    );

    let ceiling = PolicyCeiling::load();
    assert_eq!(
        ceiling.max_parallel_runs,
        defaults.policy_ceiling.max_parallel_runs
    );
    assert_eq!(
        ceiling.max_attempts_per_task,
        defaults.policy_ceiling.max_attempts_per_task
    );
    assert_eq!(
        ceiling.max_repair_cycles,
        defaults.policy_ceiling.max_repair_cycles
    );
    assert_eq!(
        ceiling.max_automatic_starts,
        defaults.policy_ceiling.max_automatic_starts
    );
    assert_eq!(
        ceiling.active_time_limit_ms,
        defaults.policy_ceiling.active_time_limit_ms
    );
    assert_eq!(
        ceiling.run_time_limit_ms,
        defaults.policy_ceiling.run_time_limit_ms
    );
}

/// The 00 role table is closed: exactly these roles exist on the wire, and
/// phase/task-kind vocabularies round-trip.
#[test]
fn vocabulary_round_trips() {
    for role in [
        Role::Lead,
        Role::Researcher,
        Role::Architect,
        Role::Builder,
        Role::TestAuthor,
        Role::Reviewer,
        Role::Specialist,
        Role::Diagnostician,
        Role::Integrator,
        Role::Documenter,
    ] {
        let json = serde_json::to_string(&role).unwrap();
        let back: Role = serde_json::from_str(&json).unwrap();
        assert_eq!(back, role);
    }
    assert_eq!(
        serde_json::to_string(&AuthRoute::Subscription).unwrap(),
        "\"subscription\""
    );
    assert_eq!(
        serde_json::to_string(&Phase::AwaitingAcceptance).unwrap(),
        "\"awaiting_acceptance\""
    );
    let binding: RoleBinding = serde_json::from_value(serde_json::json!({
        "role": "integrator",
        "primary_binding_id": "10000000-0000-4000-8000-000000000003",
        "fallback_binding_ids": []
    }))
    .unwrap();
    assert_eq!(binding.role, Role::Integrator);
}

/// The reference `contracts.examples.ts` fixture must deserialize into the
/// Rust mirror with identical values (offline fake only).
#[test]
fn reference_example_fixture_round_trips() {
    use term_contracts::ids::U64String;
    use term_contracts::mission::rpc::MissionControlParams;
    use term_contracts::mission::types::{ProviderResult, ProviderTaskSpec};

    let start: MissionControlParams = serde_json::from_value(serde_json::json!({
        "request_id": "10000000-0000-4000-8000-000000000007",
        "mission_id": "10000000-0000-4000-8000-000000000001",
        "expected_revision": "1",
        "action": "start"
    }))
    .unwrap();
    assert_eq!(start.expected_revision, U64String::new(1).unwrap());

    let plan: ProviderResult = serde_json::from_value(serde_json::json!({
        "kind": "plan",
        "based_on_plan_revision": 0,
        "retire_task_ids": [],
        "rationale_text": "API 응답 계약을 공유하고 API와 화면을 독립 작업 공간에서 구현합니다.",
        "tasks": [
            {"local_key": "api", "title": "API 구현", "kind": "implement", "role": "builder",
             "required": true, "parent_key": null, "depends_on_keys": [],
             "objective_text": "로그인 API와 관련 테스트를 구현한다.",
             "requirement_ids": ["10000000-0000-4000-8000-000000000005"],
             "input_artifact_ids": [], "allowed_paths": ["src/api/"],
             "expected_outputs": ["patch"],
             "verification_ids": ["10000000-0000-4000-8000-000000000006"],
             "specialty": null, "binding_id": "10000000-0000-4000-8000-000000000003",
             "replacement_of": null},
            {"local_key": "ui", "title": "화면 구현", "kind": "implement", "role": "builder",
             "required": true, "parent_key": null, "depends_on_keys": [],
             "objective_text": "공유 API 계약에 맞춰 로그인 화면을 구현한다.",
             "requirement_ids": ["10000000-0000-4000-8000-000000000005"],
             "input_artifact_ids": [], "allowed_paths": ["src/ui/"],
             "expected_outputs": ["patch"],
             "verification_ids": ["10000000-0000-4000-8000-000000000006"],
             "specialty": null, "binding_id": "10000000-0000-4000-8000-000000000003",
             "replacement_of": null}
        ]
    }))
    .unwrap();
    match &plan {
        ProviderResult::Plan { tasks, .. } => {
            assert_eq!(tasks.len(), 2);
            let first: &ProviderTaskSpec = &tasks[0];
            assert_eq!(first.local_key, "api");
            assert_eq!(first.allowed_paths, vec!["src/api/"]);
        }
        other => panic!("unexpected variant {other:?}"),
    }
}
