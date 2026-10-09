//! A provider `blocked` result must open a recovery decision instead of
//! leaving the task silently blocked, and each answer must reach the next Run.
use super::*;
use term_contracts::mission::MissionErrorCode;

const REPORT: &str = "The private registry rejected the download.";

fn blocked_factory(contexts: Arc<Mutex<Vec<Value>>>) -> AdapterFactory {
    Arc::new(move |run| {
        let ctx = context(run);
        if ctx["task_kind"] == "plan" {
            let spec = serde_json::from_value(json!({"local_key":"api","title":"Write api","kind":"implement","role":"builder","required":true,"parent_key":null,"depends_on_keys":[],"objective_text":"Create api.txt","requirement_ids":[ctx["requirements"][0]["id"]],"input_artifact_ids":[],"allowed_paths":["api.txt"],"expected_outputs":["patch"],"verification_ids":[],"specialty":null,"binding_id":null,"replacement_of":null})).unwrap();
            return Ok(scripted(script(ProviderResult::Plan {
                based_on_plan_revision: ctx["plan_revision"].as_u64().unwrap() as u32,
                tasks: vec![spec],
                retire_task_ids: vec![],
                rationale_text: "One writer.".into(),
            })));
        }
        contexts.lock().unwrap().push(ctx);
        Ok(scripted(script(ProviderResult::Blocked {
            code: "registry_unreachable".into(),
            report_text: REPORT.into(),
        })))
    })
}

fn is_blocked_decision(d: &Decision) -> bool {
    d.state == DecisionState::Open
        && d.kind == DecisionKind::Recovery
        && d.options.iter().any(|o| o.id == "retry_with_instruction")
}

fn blocked_rig() -> (Rig, MissionActor, Arc<Mutex<Vec<Value>>>, Decision, Task) {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let contexts = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(blocked_factory(contexts.clone()));
    rig.tick_until(&mut actor, |s| s.decisions.iter().any(is_blocked_decision));
    let snapshot = rig.snapshot();
    let decision = snapshot
        .decisions
        .iter()
        .find(|d| is_blocked_decision(d))
        .unwrap()
        .clone();
    let task = snapshot
        .tasks
        .iter()
        .find(|t| t.kind == TaskKind::Implement)
        .unwrap()
        .clone();
    (rig, actor, contexts, decision, task)
}

fn text_ref(rig: &Rig, text: &str) -> ArtifactRef {
    let store = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
    workflow::store_artifact(&store, &rig.id, "text/plain", text.as_bytes()).unwrap()
}

fn read(rig: &Rig, reference: &ArtifactRef) -> String {
    String::from_utf8(
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"))
            .read_mission_body(&rig.id, reference, 262_144)
            .unwrap(),
    )
    .unwrap()
}

fn answer(rig: &Rig, decision: &Decision, option: Option<&str>, text: Option<&str>) -> Value {
    json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,
        "decision_id":decision.id,"option_id":option,"answer_ref":text.map(|t| text_ref(rig, t))})
}

#[test]
fn blocked_result_opens_one_linked_decision_and_retry_delivers_the_instruction() {
    let (rig, mut actor, contexts, decision, task) = blocked_rig();
    let blocked = rig.snapshot();
    let current = blocked.tasks.iter().find(|t| t.id == task.id).unwrap();
    assert_eq!(current.state, TaskState::Blocked);
    assert_eq!(
        current.blocked_code.as_deref(),
        Some("provider_blocked:registry_unreachable")
    );
    let run = blocked.runs.iter().find(|r| r.task_id == task.id).unwrap();
    assert_eq!(run.state, RunState::Succeeded);
    assert_eq!(decision.requesting_run_id.as_ref(), Some(&run.id));
    assert_eq!(decision.affected_task_ids, vec![task.id.clone()]);
    assert!(!decision.blocking, "independent tasks keep running");
    assert_eq!(
        decision
            .options
            .iter()
            .map(|o| o.id.as_str())
            .collect::<Vec<_>>(),
        [
            "retry_with_instruction",
            "change_model",
            "replan",
            "stop_mission"
        ]
    );
    let question: Value = serde_json::from_str(&read(&rig, &decision.question_ref)).unwrap();
    assert_eq!(question["kind"], "provider_blocked");
    assert_eq!(question["code"], "registry_unreachable");
    assert_eq!(question["run_id"], json!(run.id));
    let report: ArtifactRef = serde_json::from_value(question["report_ref"].clone()).unwrap();
    assert_eq!(read(&rig, &report), REPORT);

    for _ in 0..4 {
        actor.tick().unwrap();
    }
    assert_eq!(
        rig.snapshot()
            .decisions
            .iter()
            .filter(|d| is_blocked_decision(d))
            .count(),
        1,
        "repeated reconciliation does not duplicate the decision"
    );

    let instruction = "Use the public mirror instead of the private registry.";
    rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        answer(
            &rig,
            &decision,
            Some("retry_with_instruction"),
            Some(instruction),
        ),
    );
    let answered = rig.snapshot();
    let retried = answered.tasks.iter().find(|t| t.id == task.id).unwrap();
    assert_eq!(retried.state, TaskState::Ready);
    assert_eq!(retried.blocked_code, None);
    let recorded = answered
        .decisions
        .iter()
        .find(|d| d.id == decision.id)
        .unwrap();
    assert_eq!(recorded.state, DecisionState::Answered);
    let message = answered
        .messages
        .iter()
        .find(|m| Some(&m.id) == recorded.answer_message_id.as_ref())
        .unwrap();
    assert_eq!(message.target_task_id.as_ref(), Some(&task.id));
    assert_eq!(message.delivery, MessageDelivery::Queued);
    let body: Value = serde_json::from_str(&read(&rig, &message.body_ref)).unwrap();
    assert_eq!(body["kind"], "decision_answer");
    assert_eq!(body["option_id"], "retry_with_instruction");
    assert_eq!(body["answer_text"], instruction);

    rig.tick_until(&mut actor, |_| contexts.lock().unwrap().len() >= 2);
    let second = contexts.lock().unwrap()[1].clone();
    assert!(
        second["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["body"].as_str().is_some_and(|b| b.contains(instruction))),
        "the next Run receives the answer: {second}"
    );
    actor.shutdown();
}

#[test]
fn blocked_task_answers_validate_choices_and_replan_hands_the_report_to_the_lead() {
    let (rig, mut actor, _contexts, decision, task) = blocked_rig();
    let missing = rig
        .service
        .handle(
            &rig.conn,
            "mission.decision.answer",
            &answer(&rig, &decision, None, Some("Just continue.")),
        )
        .err()
        .unwrap();
    assert_eq!(missing.code, MissionErrorCode::InvalidArgument);
    assert_eq!(
        missing.details.reason_code.as_deref(),
        Some("option_required")
    );
    let unchanged = rig
        .service
        .handle(
            &rig.conn,
            "mission.decision.answer",
            &answer(&rig, &decision, Some("change_model"), None),
        )
        .err()
        .unwrap();
    assert_eq!(unchanged.code, MissionErrorCode::InvalidState);
    assert_eq!(
        unchanged.details.reason_code.as_deref(),
        Some("model_not_changed"),
        "a model change needs an explicit reassignment first"
    );
    let before = rig.snapshot();
    assert_eq!(
        before
            .decisions
            .iter()
            .find(|d| d.id == decision.id)
            .unwrap()
            .state,
        DecisionState::Open
    );

    let instruction = "Avoid the private registry entirely.";
    rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        answer(&rig, &decision, Some("replan"), Some(instruction)),
    );
    let after = rig.snapshot();
    assert_eq!(
        after.tasks.iter().find(|t| t.id == task.id).unwrap().state,
        TaskState::Blocked,
        "the Lead's plan retires or replaces the blocked task"
    );
    let lead = after
        .tasks
        .iter()
        .filter(|t| t.kind == TaskKind::Plan && t.state == TaskState::Ready)
        .max_by_key(|t| t.ordinal)
        .unwrap();
    assert_eq!(lead.role, Some(Role::Lead));
    let objective: Value = serde_json::from_str(&read(&rig, &lead.contract.objective_ref)).unwrap();
    assert_eq!(objective["kind"], "provider_blocked_replan");
    assert_eq!(objective["blocked_task_id"], json!(task.id));
    assert_eq!(objective["code"], "registry_unreachable");
    assert_eq!(objective["report"], REPORT);
    assert_eq!(objective["user_instruction"], instruction);
    assert_eq!(after.mission.phase, before.mission.phase);
    assert_eq!(after.tasks.len(), before.tasks.len() + 1);
    actor.shutdown();
}

#[test]
fn plan_revision_text_reaches_the_new_plan_objective_with_the_rejected_proposal() {
    let rig = Rig::new(false, &["status", "--porcelain"]);
    let mut actor = rig.actor(blocked_factory(Arc::new(Mutex::new(vec![]))));
    rig.tick_until(&mut actor, |s| {
        s.decisions
            .iter()
            .any(|d| d.state == DecisionState::Open && d.kind == DecisionKind::Plan)
    });
    let decision = rig
        .snapshot()
        .decisions
        .into_iter()
        .find(|d| d.kind == DecisionKind::Plan)
        .unwrap();
    let request = "Split the writer into separate API and UI tasks.";
    rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        answer(&rig, &decision, Some("revise"), Some(request)),
    );
    let after = rig.snapshot();
    let revised = after
        .tasks
        .iter()
        .filter(|t| t.kind == TaskKind::Plan && t.state == TaskState::Ready)
        .max_by_key(|t| t.ordinal)
        .unwrap();
    let objective: Value =
        serde_json::from_str(&read(&rig, &revised.contract.objective_ref)).unwrap();
    assert_eq!(objective["kind"], "plan_revision_request");
    assert_eq!(objective["option_id"], "revise");
    assert_eq!(objective["user_request"], request);
    assert_eq!(objective["rejected_proposal_omitted"], false);
    assert!(objective["rejected_proposal"]["tasks"].is_array());
    let recorded = after
        .decisions
        .iter()
        .find(|d| d.id == decision.id)
        .unwrap();
    let message = after
        .messages
        .iter()
        .find(|m| Some(&m.id) == recorded.answer_message_id.as_ref())
        .unwrap();
    let body: Value = serde_json::from_str(&read(&rig, &message.body_ref)).unwrap();
    assert_eq!(body["kind"], "decision_answer");
    assert_eq!(body["decision_kind"], "plan");
    assert_eq!(body["option_label"], "Revise plan");
    assert_eq!(body["answer_text"], request);
    actor.shutdown();
}

#[test]
fn replan_waits_only_for_a_progressing_lead_plan_and_abandons_an_unfinished_integration() {
    let (rig, mut actor, _contexts, decision, task) = blocked_rig();
    let snapshot = rig.snapshot();
    let mut lead_plan = snapshot
        .tasks
        .iter()
        .find(|t| t.kind == TaskKind::Plan && t.role == Some(Role::Lead))
        .unwrap()
        .clone();
    lead_plan.id = Id::generate();
    lead_plan.state = TaskState::Ready;
    lead_plan.active_run_id = None;
    lead_plan.workspace_id = None;
    lead_plan.attempt_count = 0;
    lead_plan.blocked_code = None;
    lead_plan.ordinal = snapshot.tasks.iter().map(|t| t.ordinal).max().unwrap_or(0) + 1;
    // The answer arrives while an integration is unfinished; the mission
    // still records the prior candidate.
    let mut integrating = snapshot.mission.clone();
    integrating.phase = Phase::Integrating;
    integrating.candidate_id = Some(Id::generate());
    workflow::commit_upserts(
        &rig.service,
        integrating,
        "test.fixture",
        "ready-lead-plan",
        MissionEventType::Changed,
        vec![Entity::Task(Box::new(lead_plan.clone()))],
    )
    .unwrap();
    let waiting = rig
        .service
        .handle(
            &rig.conn,
            "mission.decision.answer",
            &answer(&rig, &decision, Some("replan"), None),
        )
        .err()
        .unwrap();
    assert_eq!(waiting.code, MissionErrorCode::InvalidState);
    assert_eq!(
        waiting.details.reason_code.as_deref(),
        Some("lead_plan_in_progress")
    );

    // A Lead plan blocked for another reason is not planning.
    lead_plan.state = TaskState::Blocked;
    lead_plan.blocked_code = Some("attempt_limit".into());
    workflow::commit_upserts(
        &rig.service,
        rig.snapshot().mission,
        "test.fixture",
        "blocked-lead-plan",
        MissionEventType::Changed,
        vec![Entity::Task(Box::new(lead_plan.clone()))],
    )
    .unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        answer(&rig, &decision, Some("replan"), None),
    );
    let after = rig.snapshot();
    assert_eq!(after.mission.phase, Phase::Planning);
    assert_eq!(
        after.mission.candidate_id, None,
        "abandoned like a candidate exclusion"
    );
    assert_eq!(
        after.tasks.iter().find(|t| t.id == task.id).unwrap().state,
        TaskState::Blocked
    );
    let replanning = after
        .tasks
        .iter()
        .filter(|t| t.kind == TaskKind::Plan && t.state == TaskState::Ready)
        .count();
    assert_eq!(replanning, 1, "one new Lead plan");
    actor.shutdown();
}
