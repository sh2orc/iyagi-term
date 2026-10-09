//! O11 core engine tests (06 §2): plan validation (E04/E05/E21), scheduler
//! fairness and caps (E06/E07/E08), control reducers (E09/E10 partial).
//! Pure functions only — no mock adapters, observable outcomes asserted.

use std::collections::HashMap;

use term_contracts::ids::U64String;
use term_contracts::mission::plan::PlanGraphError;
use term_contracts::mission::types::{
    ArtifactRef, AuthRoute, Binding, Id, Mission, MissionState, Phase, Policy, ProviderTaskSpec,
    Role, RoleBinding, Run, RunState, Task, TaskKind, TaskState, UnknownCostPolicy,
};
use term_contracts::mission::validation::MissionLimits;
use term_core::mission::plan::{validate_proposal, PlanCandidate, PlanError};
use term_core::mission::reducer::{
    apply_control, apply_run_terminal, stopping_confirmed, ControlError, MissionAction,
    MissionRules,
};
use term_core::mission::scheduler::{select_dispatches, CapLimits, MissionSlice, SkipReason};

fn artifact() -> ArtifactRef {
    ArtifactRef {
        id: Id::generate(),
        sha256: "a".repeat(64),
        bytes: U64String::parse("1").unwrap(),
        media_type: "text/plain".into(),
    }
}

fn policy(binding: &Id) -> Policy {
    Policy {
        max_parallel_runs: 4,
        max_attempts_per_task: 3,
        max_repair_cycles: 3,
        max_automatic_starts: 64,
        active_time_limit_ms: U64String::parse("14400000").unwrap(),
        run_time_limit_ms: U64String::parse("2700000").unwrap(),
        max_cost_usd_micros: None,
        unknown_cost: UnknownCostPolicy::AllowWithNotice,
        allow_network: false,
        allow_automatic_plan_apply: true,
        allow_recovery_of_unsent: true,
        allowed_binding_ids: vec![binding.clone()],
        allowed_roles: vec![
            Role::Lead,
            Role::Builder,
            Role::Reviewer,
            Role::Integrator,
            Role::TestAuthor,
        ],
        allowed_verification_ids: Vec::new(),
        require_independent_review: true,
        require_enforced_verification: false,
    }
}

fn mission(binding: &Id) -> Mission {
    Mission {
        id: Id::generate(),
        revision: U64String::parse("1").unwrap(),
        semantic_revision: None,
        follow_up_of: None,
        base_snapshot: None,
        state: MissionState::Running,
        phase: Phase::Planning,
        title: "로그인 기능".into(),
        repository_path: "/repo".into(),
        repository_id: Id::generate(),
        base_oid: "a".repeat(40),
        goal_ref: artifact(),
        requirements: vec![term_contracts::mission::types::Requirement {
            id: Id::generate(),
            text: "로그인 성공과 실패 경로를 검증한다.".into(),
            verification_ids: Vec::new(),
            human_check: false,
        }],
        policy: policy(binding),
        role_bindings: vec![RoleBinding {
            role: Role::Lead,
            primary_binding_id: binding.clone(),
            fallback_binding_ids: Vec::new(),
        }],
        plan_revision: 0,
        candidate_id: None,
        open_decision_count: 0,
        active_time_ms: U64String::parse("0").unwrap(),
        automatic_start_count: 0,
        created_at: String::new(),
        updated_at: String::new(),
        archived_at: None,
        accepted_at: None,
        failure_code: None,
    }
}

fn candidate(
    key: &str,
    kind: TaskKind,
    role: Option<Role>,
    binding: &Id,
    deps: &[&str],
) -> PlanCandidate {
    PlanCandidate {
        id: Id::generate(),
        spec: ProviderTaskSpec {
            local_key: key.into(),
            title: format!("{key} 작업"),
            kind,
            role,
            required: true,
            parent_key: None,
            depends_on_keys: deps.iter().map(|s| s.to_string()).collect(),
            objective_text: format!("{key} 목표"),
            requirement_ids: Vec::new(),
            input_artifact_ids: Vec::new(),
            allowed_paths: vec!["src/".into()],
            expected_outputs: vec![term_contracts::mission::types::ExpectedOutput::Patch],
            verification_ids: Vec::new(),
            specialty: None,
            binding_id: Some(binding.clone()),
            replacement_of: None,
        },
    }
}

fn refs(candidates: &[PlanCandidate]) -> HashMap<Id, ArtifactRef> {
    candidates
        .iter()
        .map(|c| (c.id.clone(), artifact()))
        .collect()
}

#[test]
fn e04_cycle_self_missing_and_cross_edges_reject_the_whole_plan() {
    let binding = Id::generate();
    let m = mission(&binding);
    let limits = MissionLimits::load();

    // self edge (refs must reference the SAME resolved candidate id)
    let a = candidate(
        "a",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &["a"],
    );
    let a_refs = refs(std::slice::from_ref(&a));
    let err =
        validate_proposal(&m, &[], std::slice::from_ref(&a), &[], &a_refs, &limits).unwrap_err();
    assert!(matches!(err, PlanError::Graph(PlanGraphError::SelfEdge(_))));

    // cycle a→b→a
    let ab = candidate(
        "a",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &["b"],
    );
    let ba = candidate(
        "b",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &["a"],
    );
    let mut all = vec![ab.clone(), ba.clone()];
    let err = validate_proposal(&m, &[], &all, &[], &refs(&all), &limits).unwrap_err();
    assert!(matches!(err, PlanError::Graph(PlanGraphError::Cycle)));
    all.clear();

    // missing dependency
    let missing = candidate(
        "a",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &["ghost"],
    );
    let missing_refs = refs(std::slice::from_ref(&missing.clone()));
    let err = validate_proposal(
        &m,
        &[],
        std::slice::from_ref(&missing),
        &[],
        &missing_refs,
        &limits,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        PlanError::Graph(PlanGraphError::MissingDependency(_, _))
    ));
}

#[test]
fn e05_retiring_a_succeeded_task_is_refused() {
    let binding = Id::generate();
    let m = mission(&binding);
    let limits = MissionLimits::load();
    let mut done = candidate(
        "done",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &[],
    )
    .spec
    .clone();
    let done_id = Id::generate();
    let task = Task {
        id: done_id.clone(),
        mission_id: m.id.clone(),
        title: done.title.clone(),
        kind: TaskKind::Implement,
        role: Some(Role::Builder),
        state: TaskState::Succeeded,
        required: true,
        parent_task_id: None,
        depends_on: Vec::new(),
        contract: term_contracts::mission::types::TaskContract {
            objective_ref: artifact(),
            requirement_ids: Vec::new(),
            input_artifact_ids: Vec::new(),
            allowed_paths: vec!["src/".into()],
            expected_outputs: vec![term_contracts::mission::types::ExpectedOutput::Patch],
            verification_ids: Vec::new(),
            specialty: None,
        },
        binding_id: Some(binding.clone()),
        active_run_id: None,
        ordinal: 0,
        attempt_count: 1,
        repair_cycle: 0,
        failure_repair_run_ids: vec![],
        integration: None,
        replacement_of: None,
        blocked_code: None,
        dispatch_after_unix_ms: None,
        workspace_id: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    done.title = String::new();
    let err =
        validate_proposal(&m, &[task], &[], &[done_id], &HashMap::new(), &limits).unwrap_err();
    assert!(matches!(err, PlanError::RetireIneligible(_)));
}

#[test]
fn cancelled_work_can_be_replaced_without_waiving_coverage_or_dependencies() {
    let binding = Id::generate();
    let m = mission(&binding);
    let limits = MissionLimits::load();
    let mut first = candidate(
        "first",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &[],
    );
    first.spec.requirement_ids = vec![m.requirements[0].id.clone()];
    let initial = vec![first];
    let mut old = validate_proposal(&m, &[], &initial, &[], &refs(&initial), &limits)
        .unwrap()
        .tasks
        .remove(0);
    old.state = TaskState::Cancelled;
    let mut new = candidate(
        "replacement",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &[],
    );
    new.spec.requirement_ids = old.contract.requirement_ids.clone();
    new.spec.replacement_of = Some(old.id.clone());
    let proposed = vec![new];
    assert!(validate_proposal(
        &m,
        &[old.clone()],
        &[],
        &[old.id.clone()],
        &HashMap::new(),
        &limits
    )
    .is_err());
    let accepted = validate_proposal(
        &m,
        &[old.clone()],
        &proposed,
        &[old.id.clone()],
        &refs(&proposed),
        &limits,
    )
    .unwrap();
    assert_eq!(accepted.tasks[0].replacement_of, Some(old.id.clone()));
    let mut dependent = old.clone();
    dependent.id = Id::generate();
    dependent.state = TaskState::Planned;
    dependent.depends_on = vec![old.id.clone()];
    assert!(matches!(
        validate_proposal(
            &m,
            &[old.clone(), dependent],
            &proposed,
            &[old.id.clone()],
            &refs(&proposed),
            &limits
        ),
        Err(PlanError::RetireHasDependents(_))
    ));
    old.active_run_id = Some(Id::generate());
    assert!(matches!(
        validate_proposal(
            &m,
            &[old.clone()],
            &proposed,
            &[old.id.clone()],
            &refs(&proposed),
            &limits
        ),
        Err(PlanError::RetireIneligible(_))
    ));
}

#[test]
fn excluded_history_cannot_cover_work_but_still_consumes_task_limits() {
    use term_core::mission::plan::validate_proposal_with_exclusions;
    let binding = Id::generate();
    let mut m = mission(&binding);
    let mut limits = MissionLimits::load();
    let mut first = candidate(
        "first",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &[],
    );
    first.spec.requirement_ids = vec![m.requirements[0].id.clone()];
    let initial = vec![first];
    let mut old = validate_proposal(&m, &[], &initial, &[], &refs(&initial), &limits)
        .unwrap()
        .tasks
        .remove(0);
    old.state = TaskState::Succeeded;
    old.ordinal = 19;
    let excluded = std::collections::HashSet::from([old.id.clone()]);
    let history = vec![old];
    assert!(matches!(
        validate_proposal_with_exclusions(
            &m,
            &history,
            &[],
            &[],
            &HashMap::new(),
            &limits,
            &excluded
        ),
        Err(PlanError::UncoveredRequirement(_))
    ));
    m.requirements[0].human_check = true;
    assert!(validate_proposal_with_exclusions(
        &m,
        &history,
        &[],
        &[],
        &HashMap::new(),
        &limits,
        &excluded
    )
    .unwrap()
    .tasks
    .is_empty());
    assert!(validate_proposal(&m, &history, &[], &[], &HashMap::new(), &limits).is_err());
    let new = vec![candidate(
        "next",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &[],
    )];
    let applied =
        validate_proposal_with_exclusions(&m, &history, &new, &[], &refs(&new), &limits, &excluded)
            .unwrap();
    assert_eq!(applied.tasks[0].ordinal, 20);
    limits.max_tasks_per_mission = 1;
    assert!(matches!(
        validate_proposal_with_exclusions(&m, &history, &new, &[], &refs(&new), &limits, &excluded),
        Err(PlanError::TooManyTasks { cap: 1 })
    ));
}

#[test]
fn requirement_coverage_and_role_pairing_are_enforced() {
    let binding = Id::generate();
    let m = mission(&binding);
    let limits = MissionLimits::load();
    // Uncovered requirement (no task references it, human_check=false).
    let c = candidate(
        "api",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &[],
    );
    let c_refs = refs(std::slice::from_ref(&c));
    let err =
        validate_proposal(&m, &[], std::slice::from_ref(&c), &[], &c_refs, &limits).unwrap_err();
    assert!(matches!(err, PlanError::UncoveredRequirement(_)));

    // Coverage via requirement_ids.
    let mut covered = candidate(
        "api",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &[],
    );
    covered.spec.requirement_ids = m.requirements.iter().map(|r| r.id.clone()).collect();
    let app =
        validate_proposal(&m, &[], &[covered.clone()], &[], &refs(&[covered]), &limits).unwrap();
    assert_eq!(app.tasks.len(), 1);
    assert_eq!(app.next_plan_revision, 1);

    // Role/kind mismatch.
    let mut bad = candidate(
        "api",
        TaskKind::Implement,
        Some(Role::Reviewer),
        &binding,
        &[],
    );
    bad.spec.requirement_ids = m.requirements.iter().map(|r| r.id.clone()).collect();
    let err = validate_proposal(&m, &[], &[bad.clone()], &[], &refs(&[bad]), &limits).unwrap_err();
    assert!(matches!(err, PlanError::RoleMismatch { .. }));
}

#[test]
fn e07_round_robin_prevents_one_mission_from_starving_another() {
    let binding = Id::generate();
    let policy = policy(&binding);
    let mut tasks_a = Vec::new();
    for index in 0..10 {
        tasks_a.push(Task {
            id: Id::generate(),
            mission_id: Id::generate(),
            title: format!("a{index}"),
            kind: TaskKind::Implement,
            role: Some(Role::Builder),
            state: TaskState::Ready,
            required: true,
            parent_task_id: None,
            depends_on: Vec::new(),
            contract: term_contracts::mission::types::TaskContract {
                objective_ref: artifact(),
                requirement_ids: Vec::new(),
                input_artifact_ids: Vec::new(),
                allowed_paths: vec!["src/".into()],
                expected_outputs: vec![term_contracts::mission::types::ExpectedOutput::Patch],
                verification_ids: Vec::new(),
                specialty: None,
            },
            binding_id: Some(binding.clone()),
            active_run_id: None,
            ordinal: index as u32,
            attempt_count: 0,
            repair_cycle: 0,
            failure_repair_run_ids: vec![],
            integration: None,
            replacement_of: None,
            blocked_code: None,
            dispatch_after_unix_ms: None,
            workspace_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        });
    }
    let one = Task {
        id: Id::generate(),
        title: "b0".into(),
        ..tasks_a[0].clone()
    };
    let refs_a: Vec<&Task> = tasks_a.iter().collect();
    let mut slices = [
        MissionSlice {
            mission_id: Id::generate(),
            state: MissionState::Running,
            policy: &policy,
            ready_tasks: refs_a,
            live_runs: Vec::new(),
            open_blocking_decisions: Vec::new(),
            fairness_cursor: 0,
        },
        MissionSlice {
            mission_id: Id::generate(),
            state: MissionState::Running,
            policy: &policy,
            ready_tasks: vec![&one],
            live_runs: Vec::new(),
            open_blocking_decisions: Vec::new(),
            fairness_cursor: 0,
        },
    ];
    let results = select_dispatches(&mut slices, CapLimits::default());
    let dispatched: Vec<_> = results
        .iter()
        .filter(|(_, v)| {
            matches!(
                v,
                term_core::mission::scheduler::DispatchVerdict::Dispatch(_)
            )
        })
        .collect();
    // Both missions dispatched before caps bind: the ten-task mission
    // cannot consume every slot first.
    let first_two: Vec<Id> = dispatched
        .iter()
        .take(2)
        .map(|(c, _)| c.mission_id.clone())
        .collect();
    assert_ne!(
        first_two[0], first_two[1],
        "round-robin alternates missions"
    );
}

#[test]
fn e08_binding_cap_gives_queue_reasons_and_others_proceed() {
    let binding = Id::generate();
    let other = Id::generate();
    let policy = policy(&binding);
    let mk = |name: &str, bind: Id| Task {
        id: Id::generate(),
        mission_id: Id::generate(),
        title: name.into(),
        kind: TaskKind::Implement,
        role: Some(Role::Builder),
        state: TaskState::Ready,
        required: true,
        parent_task_id: None,
        depends_on: Vec::new(),
        contract: term_contracts::mission::types::TaskContract {
            objective_ref: artifact(),
            requirement_ids: Vec::new(),
            input_artifact_ids: Vec::new(),
            allowed_paths: vec!["src/".into()],
            expected_outputs: vec![term_contracts::mission::types::ExpectedOutput::Patch],
            verification_ids: Vec::new(),
            specialty: None,
        },
        binding_id: Some(bind),
        active_run_id: None,
        ordinal: 0,
        attempt_count: 0,
        repair_cycle: 0,
        failure_repair_run_ids: vec![],
        integration: None,
        replacement_of: None,
        blocked_code: None,
        dispatch_after_unix_ms: None,
        workspace_id: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let t1 = mk("t1", binding.clone());
    let t2 = mk("t2", binding.clone());
    let t3 = mk("t3", binding.clone());
    let t_other = mk("other", other);
    let refs: Vec<&Task> = vec![&t1, &t2, &t3, &t_other];
    let slice = MissionSlice {
        mission_id: Id::generate(),
        state: MissionState::Running,
        policy: &policy,
        ready_tasks: refs,
        live_runs: Vec::new(),
        open_blocking_decisions: Vec::new(),
        fairness_cursor: 0,
    };
    let mut slices = [slice];
    let results = select_dispatches(&mut slices, CapLimits::default());
    let skips: Vec<_> = results
        .iter()
        .filter_map(|(_, v)| match v {
            term_core::mission::scheduler::DispatchVerdict::Skip(reason) => Some(reason.clone()),
            _ => None,
        })
        .collect();
    // The third task on the same binding skips with BindingCapFull (cap 2),
    // while the other-binding task proceeds.
    assert!(
        skips
            .iter()
            .any(|reason| matches!(reason, SkipReason::BindingCapFull { .. })),
        "{skips:?}"
    );
}

#[test]
fn automatic_integration_reserves_global_capacity_without_using_its_assigned_model_slot() {
    use term_contracts::mission::types::{IntegrationStep, IntegrationTask};
    use term_core::mission::scheduler::DispatchVerdict;
    let binding = Id::generate();
    let mission = mission(&binding);
    let mut spec = candidate(
        "integration",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &[],
    );
    spec.spec.requirement_ids = mission.requirements.iter().map(|r| r.id.clone()).collect();
    let mut task = validate_proposal(
        &mission,
        &[],
        &[spec.clone()],
        &[],
        &refs(&[spec]),
        &MissionLimits::load(),
    )
    .unwrap()
    .tasks
    .remove(0);
    task.kind = TaskKind::Integrate;
    task.role = Some(Role::Integrator);
    task.state = TaskState::Ready;
    task.integration = Some(IntegrationTask {
        plan_ref: task.contract.objective_ref.clone(),
        step: IntegrationStep::Automatic,
    });
    let select = |task: &Task| {
        let mut slices = [MissionSlice {
            mission_id: mission.id.clone(),
            state: MissionState::Running,
            policy: &mission.policy,
            ready_tasks: vec![task],
            live_runs: vec![
                (Id::generate(), Some(binding.clone())),
                (Id::generate(), Some(binding.clone())),
            ],
            open_blocking_decisions: vec![],
            fairness_cursor: 0,
        }];
        select_dispatches(&mut slices, CapLimits::default())
    };
    let automatic = select(&task);
    assert!(
        matches!(&automatic[0].1, DispatchVerdict::Dispatch(choice) if choice.binding_id.is_none())
    );
    assert_eq!(
        task.binding_id.as_ref(),
        Some(&binding),
        "future integrator assignment remains stored"
    );
    task.integration.as_mut().unwrap().step = IntegrationStep::Resolving {
        conflict_run_id: Id::generate(),
    };
    let resolver = select(&task);
    assert!(
        matches!(&resolver[0].1, DispatchVerdict::Skip(SkipReason::BindingCapFull { binding_id }) if binding_id == &binding)
    );
}

#[test]
fn e09_pause_reducer_completes_only_when_runs_drain() {
    let binding = Id::generate();
    let mut m = mission(&binding);
    let rules = MissionRules::default();
    let (state, _) = apply_control(&m, 1, MissionAction::Pause, &rules).unwrap();
    assert_eq!(state, MissionState::Pausing);
    m.state = MissionState::Pausing;
    assert_eq!(term_core::mission::reducer::pause_drained(&m, 1), None);
    assert_eq!(
        term_core::mission::reducer::pause_drained(&m, 0),
        Some(MissionState::Paused)
    );
}

#[test]
fn e10_late_result_cannot_resurrect_a_cancelled_task() {
    let binding = Id::generate();
    let m = mission(&binding);
    let mut task = Task {
        id: Id::generate(),
        mission_id: m.id.clone(),
        title: "t".into(),
        kind: TaskKind::Implement,
        role: Some(Role::Builder),
        state: TaskState::Cancelled,
        required: true,
        parent_task_id: None,
        depends_on: Vec::new(),
        contract: term_contracts::mission::types::TaskContract {
            objective_ref: artifact(),
            requirement_ids: Vec::new(),
            input_artifact_ids: Vec::new(),
            allowed_paths: vec!["src/".into()],
            expected_outputs: vec![term_contracts::mission::types::ExpectedOutput::Patch],
            verification_ids: Vec::new(),
            specialty: None,
        },
        binding_id: Some(binding.clone()),
        active_run_id: None,
        ordinal: 0,
        attempt_count: 1,
        repair_cycle: 0,
        failure_repair_run_ids: vec![],
        integration: None,
        replacement_of: None,
        blocked_code: None,
        dispatch_after_unix_ms: None,
        workspace_id: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let run = Run {
        id: Id::generate(),
        mission_id: m.id.clone(),
        task_id: task.id.clone(),
        attempt: 1,
        state: RunState::Succeeded,
        binding_snapshot: None,
        requested_model: None,
        observed_model: None,
        provider_session_id: None,
        provider_turn_id: None,
        exec_id: None,
        pty_session_id: None,
        workspace_id: None,
        fencing_token: U64String::parse("1").unwrap(),
        dispatch_state: term_contracts::mission::types::RunDispatchState::Acknowledged,
        context_ref: artifact(),
        result_ref: None,
        usage: term_contracts::mission::validation::unknown_usage(),
        last_activity_at: None,
        active_time_ms: U64String::parse("0").unwrap(),
        started_at: None,
        ended_at: None,
        failure_code: None,
        reconciliation_ref: None,
        reconciliation_kind: None,
        rate_limit: None,
        retry_evidence: None,
    };
    let outcome = apply_run_terminal(&m, &task, &run, true, None, &MissionRules::default());
    assert_eq!(outcome.task_state, TaskState::Cancelled);

    // Failure below the attempt cap requeues as ready.
    task.state = TaskState::Running;
    let outcome = apply_run_terminal(
        &m,
        &task,
        &run,
        false,
        Some("provider_down"),
        &MissionRules::default(),
    );
    assert_eq!(outcome.task_state, TaskState::Ready);
}

#[test]
fn e21_attempt_cap_marks_failure_not_infinite_retry() {
    let binding = Id::generate();
    let m = mission(&binding);
    let mut task = Task {
        id: Id::generate(),
        mission_id: m.id.clone(),
        title: "t".into(),
        kind: TaskKind::Implement,
        role: Some(Role::Builder),
        state: TaskState::Running,
        required: true,
        parent_task_id: None,
        depends_on: Vec::new(),
        contract: term_contracts::mission::types::TaskContract {
            objective_ref: artifact(),
            requirement_ids: Vec::new(),
            input_artifact_ids: Vec::new(),
            allowed_paths: vec!["src/".into()],
            expected_outputs: vec![term_contracts::mission::types::ExpectedOutput::Patch],
            verification_ids: Vec::new(),
            specialty: None,
        },
        binding_id: Some(binding.clone()),
        active_run_id: None,
        ordinal: 0,
        attempt_count: 2,
        repair_cycle: 0,
        failure_repair_run_ids: vec![],
        integration: None,
        replacement_of: None,
        blocked_code: None,
        dispatch_after_unix_ms: None,
        workspace_id: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let run = Run {
        id: Id::generate(),
        mission_id: m.id.clone(),
        task_id: task.id.clone(),
        attempt: 3,
        state: RunState::Failed,
        binding_snapshot: None,
        requested_model: None,
        observed_model: None,
        provider_session_id: None,
        provider_turn_id: None,
        exec_id: None,
        pty_session_id: None,
        workspace_id: None,
        fencing_token: U64String::parse("1").unwrap(),
        dispatch_state: term_contracts::mission::types::RunDispatchState::Acknowledged,
        context_ref: artifact(),
        result_ref: None,
        usage: term_contracts::mission::validation::unknown_usage(),
        last_activity_at: None,
        active_time_ms: U64String::parse("0").unwrap(),
        started_at: None,
        ended_at: None,
        failure_code: None,
        reconciliation_ref: None,
        reconciliation_kind: None,
        rate_limit: None,
        retry_evidence: None,
    };
    // attempt_count+1 = 3 >= cap 3 → failed (E21).
    let outcome = apply_run_terminal(
        &m,
        &task,
        &run,
        false,
        Some("quota"),
        &MissionRules::default(),
    );
    assert_eq!(outcome.task_state, TaskState::Failed);
    task.state = TaskState::Running;
    task.attempt_count = 1;
    let outcome = apply_run_terminal(
        &m,
        &task,
        &run,
        false,
        Some("quota"),
        &MissionRules::default(),
    );
    assert_eq!(outcome.task_state, TaskState::Ready);
}

#[test]
fn stopping_confirms_only_without_unknowns() {
    let binding = Id::generate();
    let mut m = mission(&binding);
    m.state = MissionState::Stopping;
    assert_eq!(stopping_confirmed(&m, 0, 0), Some(MissionState::Cancelled));
    assert_eq!(stopping_confirmed(&m, 0, 1), None, "unknown keeps stopping");
    assert_eq!(stopping_confirmed(&m, 1, 0), None);
}

#[test]
fn start_requires_bindings_and_budget() {
    let binding = Id::generate();
    let mut m = mission(&binding);
    let rules = MissionRules::default();
    m.state = MissionState::Draft;
    let (state, intent) = apply_control(&m, 0, MissionAction::Start, &rules).unwrap();
    assert_eq!(state, MissionState::Running);
    assert!(matches!(
        intent,
        term_core::mission::reducer::ControlIntent::StartBootstrapPlan
    ));
    m.policy.allowed_binding_ids.clear();
    assert!(matches!(
        apply_control(&m, 0, MissionAction::Start, &rules),
        Err(ControlError::NoBindings)
    ));
    m.policy.allowed_binding_ids = vec![binding.clone()];
    m.automatic_start_count = 64;
    assert!(matches!(
        apply_control(&m, 0, MissionAction::Start, &rules),
        Err(ControlError::BudgetExhausted(_))
    ));
}

#[test]
fn verify_tasks_must_be_roleless_and_bindingless() {
    let binding = Id::generate();
    let m = mission(&binding);
    let limits = MissionLimits::load();
    let mut verify = candidate("verify", TaskKind::Verify, None, &binding, &[]);
    verify.spec.requirement_ids = m.requirements.iter().map(|r| r.id.clone()).collect();
    let app = validate_proposal(&m, &[], &[verify.clone()], &[], &refs(&[verify]), &limits);
    // Verify with binding_id set is tainted.
    let mut tainted = candidate("verify", TaskKind::Verify, None, &binding, &[]);
    tainted.spec.requirement_ids = m.requirements.iter().map(|r| r.id.clone()).collect();
    let err = validate_proposal(&m, &[], &[tainted.clone()], &[], &refs(&[tainted]), &limits)
        .unwrap_err();
    assert!(matches!(err, PlanError::VerifyTainted { .. }));
    drop(app);
}

#[test]
fn fake_binding_shape_matches_the_reference_example() {
    // Sanity: the offline fixture Binding shape from contracts.examples.ts
    // round-trips through our constructors.
    let binding = Binding {
        id: Id::generate(),
        revision: U64String::parse("1").unwrap(),
        label: "Offline fixture".into(),
        runtime: term_contracts::mission::types::RuntimeKind::Fake,
        program: "/fixture/iyagi-agent".into(),
        runtime_version: Some("fixture-v1".into()),
        provider_id: "fake".into(),
        model_id: "fixture-model".into(),
        effort: None,
        auth_route: AuthRoute::Local,
        credential_ref: None,
        endpoint_ref: None,
        capabilities: term_contracts::mission::types::RuntimeCapabilities {
            structured_result: term_contracts::mission::types::Support {
                supported: true,
                reason_code: None,
            },
            events: term_contracts::mission::types::Support {
                supported: true,
                reason_code: None,
            },
            cancel: term_contracts::mission::types::Support {
                supported: true,
                reason_code: None,
            },
            resume: term_contracts::mission::types::Support {
                supported: true,
                reason_code: None,
            },
            steer: term_contracts::mission::types::Support {
                supported: true,
                reason_code: None,
            },
            approval_reply: term_contracts::mission::types::Support {
                supported: true,
                reason_code: None,
            },
            read_only: term_contracts::mission::types::Support {
                supported: true,
                reason_code: None,
            },
            scoped_write: term_contracts::mission::types::Support {
                supported: true,
                reason_code: None,
            },
            model_listing: term_contracts::mission::types::Support {
                supported: true,
                reason_code: None,
            },
            usage: term_contracts::mission::types::Support {
                supported: true,
                reason_code: None,
            },
            native_terminal_attach: term_contracts::mission::types::Support {
                supported: false,
                reason_code: Some("fake_no_native_terminal".into()),
            },
        },
        checked_at: None,
        enabled: true,
        experimental_version: None,
        local_evidence: None,
        estimated_run_cost_usd_micros: None,
        resource_policy: term_contracts::mission::types::ResourcePolicy {
            reservation_bytes: U64String::parse("2147483648").unwrap(),
            cpu_slots: 1,
            enforcement: term_contracts::launch::Enforcement::Observe,
            memory_max_bytes: None,
            cpu_max_cores: None,
            pids_max: None,
        },
    };
    assert_eq!(binding.provider_id, "fake");
}

#[test]
fn an_acyclic_plan_cannot_make_implementation_wait_for_future_review() {
    use term_contracts::mission::types::ExpectedOutput;
    let binding = Id::generate();
    let m = mission(&binding);
    let mut writer = candidate(
        "writer",
        TaskKind::Implement,
        Some(Role::Builder),
        &binding,
        &["review"],
    );
    writer.spec.requirement_ids = vec![m.requirements[0].id.clone()];
    let mut review = candidate(
        "review",
        TaskKind::Review,
        Some(Role::Reviewer),
        &binding,
        &[],
    );
    review.spec.expected_outputs = vec![ExpectedOutput::Review];
    review.spec.allowed_paths.clear();
    let candidates = vec![writer, review];
    let error = validate_proposal(
        &m,
        &[],
        &candidates,
        &[],
        &refs(&candidates),
        &MissionLimits::load(),
    )
    .unwrap_err();
    assert!(
        matches!(error,PlanError::Invalid(ref message) if message.contains("later execution phase"))
    );
}
