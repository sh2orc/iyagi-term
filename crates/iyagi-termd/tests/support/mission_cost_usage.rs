use super::*;
use term_contracts::mission::MissionErrorCode;

#[test]
fn sparse_and_lower_usage_updates_preserve_reported_cumulative_cost_and_tokens() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let mut actor = rig.actor(Arc::new(|_| {
        Ok(scripted(FakeScript {
            steps: vec![
                FakeStep::Started {
                    session_id: None,
                    turn_id: None,
                },
                FakeStep::Usage {
                    input_tokens: Some(200),
                    output_tokens: Some(10),
                    cost_usd_micros: Some(300),
                },
                FakeStep::Usage {
                    input_tokens: None,
                    output_tokens: Some(20),
                    cost_usd_micros: None,
                },
                FakeStep::Usage {
                    input_tokens: Some(50),
                    output_tokens: Some(5),
                    cost_usd_micros: Some(100),
                },
                FakeStep::Approval {
                    request_id: "cost-hold".into(),
                    question: "Hold after usage".into(),
                },
            ],
            ..Default::default()
        }))
    }));
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::AwaitingInput)
    });
    let before = rig.snapshot();
    let usage = &before.runs[0].usage;
    assert_eq!(usage.input_tokens.as_ref().unwrap().get(), 200);
    assert_eq!(usage.output_tokens.as_ref().unwrap().get(), 20);
    assert_eq!(usage.cost_usd_micros.as_ref().unwrap().get(), 300);
    assert_eq!(usage.cost_source, UsageCostSource::Provider);
    actor.shutdown();
    assert_eq!(rig.snapshot().runs[0].usage, *usage);
}

#[test]
fn cost_decision_stop_is_an_explicit_idempotent_host_action_without_a_provider_start() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let snapshot = rig.snapshot();
    let mut policy = snapshot.mission.policy.clone();
    policy.unknown_cost = UnknownCostPolicy::Block;
    rpc(
        &rig.service,
        &rig.conn,
        "mission.policy.update",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"policy":policy,"role_bindings":snapshot.mission.role_bindings}),
    );
    let mut actor = rig.actor(Arc::new(|_| {
        panic!("unknown-cost admission must prevent provider startup")
    }));
    rig.tick_until(&mut actor, |s| {
        s.decisions
            .iter()
            .any(|d| d.options.iter().any(|o| o.id == "stop_cost_mission"))
    });
    let snapshot = rig.snapshot();
    let decision = &snapshot.decisions[0];
    let mut answer = json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"decision_id":decision.id,"option_id":null,"answer_ref":null});
    assert_eq!(
        rig.service
            .handle(&rig.conn, "mission.decision.answer", &answer)
            .err()
            .unwrap()
            .code,
        MissionErrorCode::InvalidArgument
    );
    answer["option_id"] = json!("stop_cost_mission");
    let response = rpc(
        &rig.service,
        &rig.conn,
        "mission.decision.answer",
        answer.clone(),
    );
    rig.tick_until(&mut actor, |s| s.mission.state == MissionState::Cancelled);
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.decision.answer", answer),
        response
    );
    let after = rig.snapshot();
    assert!(after.runs.is_empty());
    assert_eq!(after.mission.automatic_start_count, 0);
    assert_eq!(after.decisions[0].state, DecisionState::Answered);
    assert!(rig.storage.mission_snapshot(&rig.id).unwrap().unwrap().entities.iter().any(|e|matches!(e,Entity::Message(m) if m.role==MessageRole::System && m.delivery==MessageDelivery::Delivered)));
    actor.shutdown();
}

#[test]
fn zero_binding_estimate_is_rejected_without_replacing_the_saved_connection() {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let before = rig.storage.mission_bindings().unwrap();
    let mut binding: Binding = serde_json::from_value(before[0].clone()).unwrap();
    binding.estimated_run_cost_usd_micros = Some(term_contracts::ids::U64String::new(0).unwrap());
    let params =
        json!({"request_id":Id::generate(),"expected_revision":binding.revision,"binding":binding});
    assert_eq!(
        rig.service
            .handle(&rig.conn, "binding.save", &params)
            .err()
            .unwrap()
            .code,
        MissionErrorCode::InvalidArgument
    );
    assert_eq!(rig.storage.mission_bindings().unwrap(), before);
}
