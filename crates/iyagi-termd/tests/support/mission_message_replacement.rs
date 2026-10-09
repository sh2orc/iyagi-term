use super::*;

fn seed(rig: &Rig, role: MessageRole, delivery: MessageDelivery, target: Option<Id>) -> Message {
    let message = Message {
        id: Id::generate(),
        mission_id: rig.id.clone(),
        target_task_id: target,
        role,
        run_id: None,
        body_ref: body(rig, b"Original instruction."),
        delivery,
        supersedes_message_id: None,
        created_at: term_storage::time::now_iso8601(),
    };
    workflow::commit_upserts(
        &rig.service,
        rig.snapshot().mission,
        "fixture.uncertain_message",
        "seed",
        MissionEventType::Changed,
        vec![Entity::Message(Box::new(message.clone()))],
    )
    .unwrap();
    message
}
fn replace(rig: &Rig, original: &Message, text: &str) -> Value {
    let mut params = message(rig, text, original.target_task_id.as_ref());
    params["supersedes_message_id"] = json!(original.id);
    params
}
fn reject(rig: &Rig, params: &Value, code: MissionErrorCode) {
    let before = messages(rig);
    assert_eq!(
        rig.service
            .handle(&rig.conn, "mission.message", params)
            .err()
            .unwrap()
            .code,
        code
    );
    assert_eq!(messages(rig), before);
}

#[test]
fn explicit_replacement_preserves_unknown_history_and_sends_one_new_intent() {
    let rig = message_rig();
    let gate = ReceiptGate::new(DeliveryReceipt::Unknown {
        reason: "fixture receipt lost",
    });
    gate.release();
    let mut actor = start(&rig, gate.clone(), Arc::new(Mutex::new(vec![])));
    rpc(
        &rig.service,
        &rig.conn,
        "mission.message",
        message(&rig, "Original instruction.", None),
    );
    wait(&mut actor, || {
        messages(&rig)[0].delivery == MessageDelivery::Unknown
    });
    let original = messages(&rig)[0].clone();
    let params = replace(&rig, &original, "Reviewed replacement instruction.");
    let saved = rpc(&rig.service, &rig.conn, "mission.message", params.clone());
    wait(&mut actor, || {
        gate.calls.lock().unwrap().len() == 2
            && messages(&rig)
                .iter()
                .all(|m| m.delivery == MessageDelivery::Unknown)
    });
    for _ in 0..5 {
        actor.tick().unwrap();
    }
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.message", params),
        saved
    );
    assert_eq!(gate.calls.lock().unwrap().len(), 2);
    assert_eq!(
        gate.calls.lock().unwrap()[1].1,
        "Reviewed replacement instruction."
    );
    assert_eq!(
        messages(&rig).iter().find(|m| m.id == original.id),
        Some(&original)
    );
    let successor = messages(&rig)
        .into_iter()
        .find(|m| m.supersedes_message_id.as_ref() == Some(&original.id))
        .unwrap();
    assert_ne!(successor.id, original.id);
    reject(
        &rig,
        &replace(&rig, &original, "An accidental duplicate."),
        MissionErrorCode::InvalidState,
    );
    actor.shutdown();
}

#[test]
fn explicit_replacement_survives_restart_and_only_the_new_instruction_enters_context() {
    let mut rig = message_rig();
    let gate = ReceiptGate::new(DeliveryReceipt::Unknown {
        reason: "fixture receipt lost",
    });
    gate.release();
    let mut actor = start(&rig, gate.clone(), Arc::new(Mutex::new(vec![])));
    rpc(
        &rig.service,
        &rig.conn,
        "mission.message",
        message(&rig, "Original uncertain instruction.", None),
    );
    wait(&mut actor, || {
        messages(&rig)[0].delivery == MessageDelivery::Unknown
    });
    let original = messages(&rig)[0].clone();
    actor.shutdown();
    let params = replace(&rig, &original, "Reviewed instruction after restart.");
    let saved = rpc(&rig.service, &rig.conn, "mission.message", params.clone());
    rig.service = Arc::new(MissionService::new(
        rig.storage.clone(),
        ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
    ));
    rig.service.recover_on_startup().unwrap();
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.message", params),
        saved
    );
    retry_failed_lead(&rig);
    let contexts = Arc::new(Mutex::new(vec![]));
    let mut actor = start(&rig, gate, contexts.clone());
    assert_eq!(
        contexts.lock().unwrap()[0]["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        contexts.lock().unwrap()[0]["messages"][0]["body"],
        "Reviewed instruction after restart."
    );
    assert_eq!(
        contexts.lock().unwrap()[0]["messages"][0]["supersedes_message_id"],
        json!(original.id)
    );
    assert_eq!(
        messages(&rig).iter().find(|m| m.id == original.id),
        Some(&original)
    );
    actor.shutdown();
}

#[test]
fn replacement_and_outbox_commit_atomically_and_competing_requests_cannot_fork_history() {
    let rig = message_rig();
    let original = seed(&rig, MessageRole::User, MessageDelivery::Rejected, None);
    let params = replace(&rig, &original, "One explicit replacement.");
    let db = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER hold_replacement BEFORE INSERT ON orch_outbox WHEN NEW.operation = 'message' BEGIN SELECT RAISE(ABORT, 'outbox outage'); END;").unwrap();
    reject(&rig, &params, MissionErrorCode::StorageUnavailable);
    assert_eq!(messages(&rig), vec![original.clone()]);
    assert!(rig.storage.mission_outbox().unwrap().is_empty());
    assert!(rig
        .storage
        .mission_request(&serde_json::from_value(params["request_id"].clone()).unwrap())
        .unwrap()
        .is_none());
    db.execute_batch("DROP TRIGGER hold_replacement;").unwrap();
    let mut other = params.clone();
    other["request_id"] = json!(Id::generate());
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let results = std::thread::scope(|scope| {
        [params.clone(), other]
            .into_iter()
            .map(|request| {
                let barrier = barrier.clone();
                let service = rig.service.clone();
                let conn = rig.conn.clone();
                scope.spawn(move || {
                    barrier.wait();
                    service
                        .handle(&conn, "mission.message", &request)
                        .map(|_| ())
                        .map_err(|e| e.code)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == Err(MissionErrorCode::RevisionConflict))
            .count(),
        1
    );
    assert_eq!(messages(&rig).len(), 2);
    assert_eq!(rig.storage.mission_outbox().unwrap().len(), 1);
    reject(
        &rig,
        &replace(&rig, &original, "Second replacement."),
        MissionErrorCode::InvalidState,
    );
}

#[test]
fn replacement_rejects_delivered_pending_agent_and_cross_recipient_messages() {
    for (role, delivery) in [
        (MessageRole::User, MessageDelivery::Delivered),
        (MessageRole::User, MessageDelivery::Queued),
        (MessageRole::Agent, MessageDelivery::Unknown),
        (MessageRole::System, MessageDelivery::Rejected),
    ] {
        let rig = message_rig();
        let original = seed(&rig, role, delivery, None);
        reject(
            &rig,
            &replace(&rig, &original, "Replacement."),
            MissionErrorCode::InvalidState,
        );
    }
    let rig = message_rig();
    let original = seed(&rig, MessageRole::User, MessageDelivery::Unknown, None);
    let mut request = replace(&rig, &original, "Replacement.");
    request["target_task_id"] = json!(rig.snapshot().tasks[0].id);
    reject(&rig, &request, MissionErrorCode::InvalidState);
    request["target_task_id"] = Value::Null;
    request["supersedes_message_id"] = json!(Id::generate());
    reject(&rig, &request, MissionErrorCode::NotFound);
    reject(
        &rig,
        &replace(&rig, &original, "  "),
        MissionErrorCode::InvalidArgument,
    );
    let mut future = replace(&rig, &original, "Replacement.");
    future["expected_revision"] = json!((rig.snapshot().mission.revision.get() + 1).to_string());
    reject(&rig, &future, MissionErrorCode::RevisionConflict);
}

#[test]
fn decision_answers_and_unfinished_intents_cannot_be_replayed_as_instructions() {
    let rig = message_rig();
    let gate = ReceiptGate::new(DeliveryReceipt::Delivered { provider_ref: None });
    let mut actor = start(&rig, gate, Arc::new(Mutex::new(vec![])));
    let original = seed(&rig, MessageRole::User, MessageDelivery::Unknown, None);
    let mut decision = rig.snapshot().decisions[0].clone();
    decision.state = DecisionState::Answered;
    decision.answer_message_id = Some(original.id.clone());
    workflow::commit_upserts(
        &rig.service,
        rig.snapshot().mission,
        "fixture.answer",
        "link",
        MissionEventType::Changed,
        vec![Entity::Decision(Box::new(decision))],
    )
    .unwrap();
    reject(
        &rig,
        &replace(&rig, &original, "Do not repeat an approval."),
        MissionErrorCode::InvalidState,
    );
    actor.shutdown();

    let rig = message_rig();
    let params = message(&rig, "Still pending.", None);
    rpc(&rig.service, &rig.conn, "mission.message", params);
    let mut original = messages(&rig)[0].clone();
    original.delivery = MessageDelivery::Unknown;
    workflow::commit_upserts(
        &rig.service,
        rig.snapshot().mission,
        "fixture.pending",
        "inconsistent",
        MissionEventType::Changed,
        vec![Entity::Message(Box::new(original.clone()))],
    )
    .unwrap();
    reject(
        &rig,
        &replace(&rig, &original, "Pending intent still exists."),
        MissionErrorCode::InvalidState,
    );
}

#[test]
fn legacy_message_payload_and_null_replacement_keep_the_same_request_fingerprint() {
    use term_contracts::mission::rpc::MissionMessageParams;
    let rig = message_rig();
    let params = message(&rig, "Legacy instruction.", None);
    let typed: MissionMessageParams = serde_json::from_value(params.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), params);
    let saved = rpc(&rig.service, &rig.conn, "mission.message", params.clone());
    let mut nullable = params;
    nullable["supersedes_message_id"] = Value::Null;
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.message", nullable),
        saved
    );
    assert_eq!(messages(&rig).len(), 1);
}
