use super::*;
use term_core::mission::budget::{cost_admission, summarize_cost, CostBlock};

fn snapshot(rig: &Rig, id: &Id) -> workflow::MissionEntities {
    workflow::load_entities(&rig.storage, id).unwrap()
}
fn save_binding(rig: &Rig, mut binding: Binding, estimate: Option<u64>) -> Binding {
    binding.estimated_run_cost_usd_micros = estimate.map(|v| U64String::new(v).unwrap());
    let saved = rig
        .storage
        .save_mission_binding(
            Id::generate(),
            "binding.save",
            &"e".repeat(64),
            binding.revision.get(),
            serde_json::to_value(binding).unwrap(),
            term_storage::time::now_iso8601(),
        )
        .unwrap();
    serde_json::from_value(saved.document).unwrap()
}
fn current_binding(rig: &Rig) -> Binding {
    serde_json::from_value(
        rig.storage
            .mission_bindings()
            .unwrap()
            .into_iter()
            .find(|b| b["id"] == rig.binding.id.as_str())
            .unwrap(),
    )
    .unwrap()
}
fn policy(rig: &Rig, id: &Id, cap: Option<u64>, unknown: UnknownCostPolicy) {
    let mut mission = snapshot(rig, id).mission;
    mission.policy.max_cost_usd_micros = cap.map(|v| U64String::new(v).unwrap());
    mission.policy.unknown_cost = unknown;
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.cost_policy",
        "policy",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
}
fn finish(rig: &Rig, id: &Id, run_id: &Id, cost: Option<u64>) -> Run {
    let snapshot = snapshot(rig, id);
    let mut run = snapshot
        .runs
        .iter()
        .find(|r| &r.id == run_id)
        .unwrap()
        .clone();
    let mut task = snapshot
        .tasks
        .iter()
        .find(|t| t.id == run.task_id)
        .unwrap()
        .clone();
    run.state = RunState::Succeeded;
    run.dispatch_state = RunDispatchState::Acknowledged;
    run.started_at = Some(term_storage::time::now_iso8601());
    run.ended_at = run.started_at.clone();
    run.usage.cost_usd_micros = cost.map(|v| U64String::new(v).unwrap());
    run.usage.cost_source = if cost.is_some() {
        UsageCostSource::Provider
    } else {
        UsageCostSource::Unknown
    };
    task.state = TaskState::Succeeded;
    task.active_run_id = None;
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "fixture.cost_result",
        "result",
        MissionEventType::Changed,
        vec![
            Entity::Run(Box::new(run.clone())),
            Entity::Task(Box::new(task)),
        ],
    )
    .unwrap();
    run
}

#[test]
fn reservations_prevent_parallel_oversubscription_and_release_unused_estimates() {
    let rig = Rig::new();
    save_binding(&rig, current_binding(&rig), Some(600));
    let id = rig.seed(MissionState::Running, 2, 64);
    policy(&rig, &id, Some(1000), UnknownCostPolicy::Block);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    let before = snapshot(&rig, &id);
    assert_eq!(before.runs.len(), 1);
    assert_eq!(before.mission.automatic_start_count, 1);
    assert_eq!(before.mission.open_decision_count, 1);
    assert!(!before.decisions[0].blocking);
    assert_eq!(summarize_cost(&before.runs).committed_micros(), 600);
    rig.service.dispatch_tick().unwrap();
    assert_eq!(
        snapshot(&rig, &id).mission.revision,
        before.mission.revision
    );
    let ended = finish(&rig, &id, &before.runs[0].id, Some(100));
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    let after = snapshot(&rig, &id);
    assert_eq!(after.decisions[0].state, DecisionState::Obsolete);
    assert_eq!(after.mission.open_decision_count, 0);
    assert_eq!(after.runs.iter().find(|r| r.id == ended.id), Some(&ended));
    let total = summarize_cost(&after.runs);
    assert_eq!(
        (
            total.observed_micros,
            total.estimated_micros,
            total.unknown_runs
        ),
        (100, 600, 0)
    );
}

#[test]
fn unknown_policy_requires_an_estimate_even_when_the_runtime_reports_usage() {
    let rig = Rig::new();
    let mut supported = current_binding(&rig);
    supported.capabilities.usage = Support {
        supported: true,
        reason_code: None,
    };
    let supported = save_binding(&rig, supported, None);
    let id = rig.seed(MissionState::Running, 1, 64);
    policy(&rig, &id, None, UnknownCostPolicy::Block);
    assert!(supported.capabilities.usage.supported);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    let held = snapshot(&rig, &id);
    assert!(held.runs.is_empty());
    assert_eq!(held.tasks[0].blocked_code.as_deref(), Some("cost_unknown"));
    save_binding(&rig, current_binding(&rig), Some(500));
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    let ready = snapshot(&rig, &id);
    assert_eq!(ready.decisions[0].state, DecisionState::Obsolete);
    assert_eq!(
        ready.runs[0]
            .binding_snapshot
            .as_ref()
            .unwrap()
            .estimated_run_cost_usd_micros
            .as_ref()
            .unwrap()
            .get(),
        500
    );
}

#[test]
fn an_expensive_binding_does_not_block_an_affordable_independent_task() {
    let rig = Rig::new();
    let expensive = save_binding(&rig, current_binding(&rig), Some(1100));
    let mut cheap = expensive.clone();
    cheap.id = Id::generate();
    cheap.revision = U64String::new(0).unwrap();
    let cheap = save_binding(&rig, cheap, Some(100));
    let id = rig.seed(MissionState::Running, 2, 64);
    policy(&rig, &id, Some(1000), UnknownCostPolicy::Block);
    let mut s = snapshot(&rig, &id);
    s.mission.policy.allowed_binding_ids.push(cheap.id.clone());
    let mut task = s.tasks.iter().find(|t| t.ordinal == 1).unwrap().clone();
    task.binding_id = Some(cheap.id.clone());
    workflow::commit_upserts(
        &rig.service,
        s.mission,
        "fixture.cheap",
        "cheap",
        MissionEventType::Changed,
        vec![Entity::Task(Box::new(task))],
    )
    .unwrap();
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    let s = snapshot(&rig, &id);
    assert_eq!(s.runs[0].binding_snapshot.as_ref().unwrap().id, cheap.id);
    assert_eq!(
        s.tasks
            .iter()
            .find(|t| t.ordinal == 0)
            .unwrap()
            .blocked_code
            .as_deref(),
        Some("cost_limit")
    );
    assert!(!s.decisions[0].blocking);
}

#[test]
fn historical_unknown_cost_is_not_repriced_by_a_new_binding_estimate() {
    let rig = Rig::new();
    let id = rig.seed(MissionState::Running, 1, 64);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    let old = finish(&rig, &id, &snapshot(&rig, &id).runs[0].id, None);
    save_binding(&rig, current_binding(&rig), Some(100));
    let mut s = snapshot(&rig, &id);
    let mut next = s.tasks[0].clone();
    next.id = Id::generate();
    next.ordinal += 1;
    next.state = TaskState::Ready;
    next.attempt_count = 0;
    s.mission.policy.unknown_cost = UnknownCostPolicy::Block;
    workflow::commit_upserts(
        &rig.service,
        s.mission,
        "fixture.followup",
        "followup",
        MissionEventType::Changed,
        vec![Entity::Task(Box::new(next))],
    )
    .unwrap();
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    assert_eq!(summarize_cost(&snapshot(&rig, &id).runs).unknown_runs, 1);
    policy(&rig, &id, Some(1000), UnknownCostPolicy::AllowWithNotice);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    let after = snapshot(&rig, &id);
    assert_eq!(after.runs.iter().find(|r| r.id == old.id), Some(&old));
    assert_eq!(summarize_cost(&after.runs).unknown_runs, 1);
    assert_eq!(summarize_cost(&after.runs).estimated_micros, 100);
}

#[test]
fn summaries_keep_unknown_ownership_and_distinguish_zero_from_missing_cost() {
    let rig = Rig::new();
    let binding = save_binding(&rig, current_binding(&rig), Some(600));
    let id = rig.seed(MissionState::Running, 1, 64);
    rig.service.dispatch_tick().unwrap();
    let mut run = snapshot(&rig, &id).runs[0].clone();
    run.state = RunState::Unknown;
    run.dispatch_state = RunDispatchState::MayHaveSent;
    assert_eq!(summarize_cost(&[run.clone()]).estimated_micros, 600);
    run.state = RunState::Stopping;
    run.usage.cost_usd_micros = Some(U64String::new(200).unwrap());
    run.usage.cost_source = UsageCostSource::Provider;
    let total = summarize_cost(&[run.clone()]);
    assert_eq!((total.observed_micros, total.estimated_micros), (200, 400));
    run.state = RunState::Failed;
    assert_eq!(summarize_cost(&[run.clone()]).committed_micros(), 200);
    run.usage.cost_usd_micros = Some(U64String::new(0).unwrap());
    assert_eq!(summarize_cost(&[run.clone()]).committed_micros(), 0);
    run.usage.cost_usd_micros = None;
    run.usage.cost_source = UsageCostSource::Unknown;
    assert_eq!(summarize_cost(&[run.clone()]).estimated_micros, 600);
    run.state = RunState::Cancelled;
    run.dispatch_state = RunDispatchState::Unsent;
    assert_eq!(summarize_cost(&[run.clone()]).committed_micros(), 0);
    assert_eq!(summarize_cost(&[run.clone()]).unknown_runs, 0);
    run.state = RunState::Succeeded;
    run.dispatch_state = RunDispatchState::Acknowledged;
    run.usage.cost_source = UsageCostSource::Provider;
    run.usage.cost_usd_micros = Some(U64String::new(U64String::MAX).unwrap());
    let mut other = run.clone();
    other.id = Id::generate();
    let runs = [run, other];
    assert_eq!(
        summarize_cost(&runs).observed_micros,
        U64String::MAX as u128 * 2
    );
    let mut policy = snapshot(&rig, &id).mission.policy;
    policy.max_cost_usd_micros = Some(U64String::new(U64String::MAX).unwrap());
    assert_eq!(
        cost_admission(&policy, &runs, Some(&binding)),
        Err(CostBlock::Limit)
    );
    assert_eq!(
        cost_admission(&policy, &runs, None),
        Ok(()),
        "deterministic verification has no provider-dollar charge"
    );
}

#[test]
fn a_failed_cost_decision_transaction_does_not_consume_an_attempt_or_reservation() {
    let rig = Rig::new();
    save_binding(&rig, current_binding(&rig), Some(600));
    let id = rig.seed(MissionState::Running, 1, 64);
    policy(&rig, &id, Some(500), UnknownCostPolicy::Block);
    let before = snapshot(&rig, &id);
    let db = rusqlite::Connection::open(rig._dir.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_cost BEFORE INSERT ON orch_entities WHEN NEW.kind='decision' BEGIN SELECT RAISE(FAIL,'fixture cost storage outage'); END").unwrap();
    assert_eq!(
        rig.service.dispatch_tick().unwrap_err().code,
        term_contracts::mission::MissionErrorCode::StorageUnavailable
    );
    let failed = snapshot(&rig, &id);
    assert_eq!(failed.mission.revision, before.mission.revision);
    assert_eq!(failed.tasks, before.tasks);
    assert!(failed.runs.is_empty());
    db.execute_batch("DROP TRIGGER fail_cost").unwrap();
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    let held = snapshot(&rig, &id);
    assert_eq!(held.mission.open_decision_count, 1);
    assert_eq!(held.mission.automatic_start_count, 0);
    assert_eq!(held.tasks[0].attempt_count, 0);
}

#[test]
fn exact_estimate_fits_but_observed_overrun_blocks_further_paid_work() {
    let rig = Rig::new();
    save_binding(&rig, current_binding(&rig), Some(500));
    let id = rig.seed(MissionState::Running, 2, 64);
    policy(&rig, &id, Some(500), UnknownCostPolicy::Block);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    let first = snapshot(&rig, &id).runs[0].clone();
    finish(&rig, &id, &first.id, Some(700));
    assert_eq!(rig.service.dispatch_tick().unwrap(), 0);
    policy(&rig, &id, Some(1200), UnknownCostPolicy::Block);
    assert_eq!(rig.service.dispatch_tick().unwrap(), 1);
    assert_eq!(
        summarize_cost(&snapshot(&rig, &id).runs).committed_micros(),
        1200
    );
}
