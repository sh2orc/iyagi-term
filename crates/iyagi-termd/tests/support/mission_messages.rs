use super::*;
use iyagi_termd_lib::agent_runtime::{CancelReceipt, DeliveryReceipt, EventStream, RunProbe};
use std::sync::Condvar;
use term_contracts::mission::MissionErrorCode;
use term_storage::mission::types::{OutboxOperation, OutboxState};

struct ReceiptGate {
    open: Mutex<bool>,
    changed: Condvar,
    calls: Mutex<Vec<(Id, String)>>,
    receipt: DeliveryReceipt,
}
impl ReceiptGate {
    fn new(receipt: DeliveryReceipt) -> Arc<Self> {
        Arc::new(Self {
            open: Mutex::new(false),
            changed: Condvar::new(),
            calls: Mutex::new(vec![]),
            receipt,
        })
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
struct ReleaseOnDrop(Arc<ReceiptGate>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct ObservedAdapter {
    inner: Arc<dyn AgentAdapter>,
    gate: Arc<ReceiptGate>,
}
impl AgentAdapter for ObservedAdapter {
    fn name(&self) -> &'static str {
        "message-fixture"
    }
    fn start(&self, run: RunStart) -> std::io::Result<()> {
        self.inner.start(run)
    }
    fn send_message(&self, id: &Id, body: &str) -> DeliveryReceipt {
        self.gate
            .calls
            .lock()
            .unwrap()
            .push((id.clone(), body.into()));
        let open = self.gate.open.lock().unwrap();
        // Bounded even if a test assertion fails before releasing the gate.
        let (open, _) = self
            .gate
            .changed
            .wait_timeout_while(open, Duration::from_secs(10), |v| !*v)
            .unwrap();
        if *open {
            self.gate.receipt.clone()
        } else {
            DeliveryReceipt::Unknown {
                reason: "fixture receipt timeout",
            }
        }
    }
    fn answer(&self, id: &Id, request: &str, answer: &str) -> DeliveryReceipt {
        self.inner.answer(id, request, answer)
    }
    fn interrupt(&self, id: &Id) -> CancelReceipt {
        self.inner.interrupt(id)
    }
    fn close(&self, id: &Id) -> CancelReceipt {
        self.inner.close(id)
    }
    fn inspect(&self, id: &Id) -> RunProbe {
        self.inner.inspect(id)
    }
    fn subscribe(&self) -> EventStream {
        self.inner.subscribe()
    }
}
fn holding_factory(gate: Arc<ReceiptGate>, contexts: Arc<Mutex<Vec<Value>>>) -> AdapterFactory {
    Arc::new(move |run| {
        contexts.lock().unwrap().push(context(run));
        Ok(Arc::new(ObservedAdapter {
            gate: gate.clone(),
            inner: scripted(FakeScript {
                steps: vec![
                    FakeStep::Started {
                        session_id: Some("message-session".into()),
                        turn_id: Some("message-turn".into()),
                    },
                    FakeStep::Approval {
                        request_id: "hold".into(),
                        question: "Fixture holds its turn".into(),
                    },
                ],
                ..Default::default()
            }),
        }))
    })
}
fn body(rig: &Rig, text: &[u8]) -> ArtifactRef {
    let store = ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions"));
    workflow::store_artifact(&store, &rig.id, "text/plain", text).unwrap()
}
fn message(rig: &Rig, text: &str, target: Option<&Id>) -> Value {
    json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"target_task_id":target,"body_ref":body(rig,text.as_bytes())})
}

#[test]
fn queued_instructions_do_not_create_extra_plans_behind_a_cost_or_provider_hold() {
    for reason in ["provider_rate_limited", "cost_unknown", "cost_limit"] {
        let rig = Rig::new(true, &["status", "--porcelain"]);
        let snapshot = rig.snapshot();
        let mut task = snapshot.tasks[0].clone();
        task.state = TaskState::Blocked;
        task.blocked_code = Some(reason.into());
        workflow::commit_upserts(
            &rig.service,
            snapshot.mission,
            "fixture.admission_hold",
            "hold",
            MissionEventType::Changed,
            vec![Entity::Task(Box::new(task.clone()))],
        )
        .unwrap();
        for text in [
            "Keep the first constraint.",
            "Also keep the second constraint.",
        ] {
            rpc(
                &rig.service,
                &rig.conn,
                "mission.message",
                message(&rig, text, None),
            );
        }
        let after = rig.snapshot();
        assert_eq!(after.tasks.len(), 1, "{reason} duplicated the Lead");
        assert_eq!(after.tasks[0], task);
        assert!(after.runs.is_empty());
        assert_eq!(after.mission.automatic_start_count, 0);
        assert_eq!(messages(&rig).len(), 2);
        assert!(messages(&rig)
            .iter()
            .all(|m| m.delivery == MessageDelivery::Queued && m.run_id.is_none()));
    }
}
fn messages(rig: &Rig) -> Vec<Message> {
    rig.storage
        .mission_snapshot(&rig.id)
        .unwrap()
        .unwrap()
        .entities
        .into_iter()
        .filter_map(|e| match e {
            Entity::Message(m) if m.role == MessageRole::User => Some(*m),
            _ => None,
        })
        .collect()
}
fn wait(actor: &mut MissionActor, predicate: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < until, "message condition timed out");
        actor.tick().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn start(rig: &Rig, gate: Arc<ReceiptGate>, contexts: Arc<Mutex<Vec<Value>>>) -> MissionActor {
    let mut actor = rig.actor(holding_factory(gate, contexts));
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::AwaitingInput)
    });
    actor
}

#[test]
fn active_message_waits_for_receipt_and_persists_once_after_storage_failure() {
    let rig = message_rig();
    let gate = ReceiptGate::new(DeliveryReceipt::Delivered {
        provider_ref: Some("message-turn".into()),
    });
    let _release = ReleaseOnDrop(gate.clone());
    let mut actor = start(&rig, gate.clone(), Arc::new(Mutex::new(vec![])));
    let params = message(&rig, "추가 테스트를 작성해 주세요.", None);
    let saved = rpc(&rig.service, &rig.conn, "mission.message", params.clone());
    wait(&mut actor, || gate.calls.lock().unwrap().len() == 1);
    assert_eq!(messages(&rig)[0].delivery, MessageDelivery::Queued);
    let before = Instant::now();
    actor.tick().unwrap();
    assert!(
        before.elapsed() < Duration::from_secs(1),
        "waiting for the provider blocked the actor"
    );
    assert_eq!(
        rpc(&rig.service, &rig.conn, "mission.message", params),
        saved
    );
    let connection = rusqlite::Connection::open(rig.dir.path().join("state.db")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_message_receipt BEFORE UPDATE ON orch_outbox WHEN NEW.operation = 'message' AND NEW.state = 'acknowledged' BEGIN SELECT RAISE(FAIL, 'fixture storage outage'); END").unwrap();
    gate.release();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Err(error) = actor.tick() {
            assert_eq!(error.code, MissionErrorCode::StorageUnavailable);
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(messages(&rig)[0].delivery, MessageDelivery::Queued);
    connection
        .execute_batch("DROP TRIGGER fail_message_receipt")
        .unwrap();
    wait(&mut actor, || {
        messages(&rig)[0].delivery == MessageDelivery::Delivered
    });
    assert_eq!(
        gate.calls.lock().unwrap().as_slice(),
        &[(
            rig.snapshot().runs[0].id.clone(),
            "추가 테스트를 작성해 주세요.".into()
        )]
    );
    actor.shutdown();
}

#[test]
fn unsupported_steer_is_delivered_in_the_next_lead_context() {
    let rig = message_rig();
    let mut binding: Binding =
        serde_json::from_value(rig.storage.mission_bindings().unwrap().remove(0)).unwrap();
    binding.capabilities.steer.supported = false;
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":binding.revision,"binding":binding}),
    );
    let gate = ReceiptGate::new(DeliveryReceipt::Delivered { provider_ref: None });
    let _release = ReleaseOnDrop(gate.clone());
    let contexts = Arc::new(Mutex::new(vec![]));
    let mut actor = start(&rig, gate.clone(), contexts.clone());
    rpc(
        &rig.service,
        &rig.conn,
        "mission.message",
        message(&rig, "Use the queued design.", None),
    );
    for _ in 0..3 {
        actor.tick().unwrap();
    }
    assert!(gate.calls.lock().unwrap().is_empty());
    assert_eq!(messages(&rig)[0].delivery, MessageDelivery::Queued);
    assert!(messages(&rig)[0].run_id.is_none());
    actor.shutdown();
    retry_failed_lead(&rig);
    let mut actor = start(&rig, gate, contexts.clone());
    assert_eq!(
        contexts.lock().unwrap()[1]["messages"][0]["body"],
        "Use the queued design."
    );
    assert_eq!(messages(&rig)[0].delivery, MessageDelivery::Delivered);
    assert_eq!(rig.snapshot().tasks.len(), 1);
    actor.shutdown();
}

#[test]
fn uncertain_message_is_not_replayed_in_a_new_lead_context() {
    let rig = message_rig();
    let gate = ReceiptGate::new(DeliveryReceipt::Unknown {
        reason: "fixture lost acknowledgement",
    });
    gate.release();
    let contexts = Arc::new(Mutex::new(vec![]));
    let mut actor = start(&rig, gate.clone(), contexts.clone());
    rpc(
        &rig.service,
        &rig.conn,
        "mission.message",
        message(&rig, "Uncertain instruction.", None),
    );
    wait(&mut actor, || {
        messages(&rig)[0].delivery == MessageDelivery::Unknown
    });
    for _ in 0..3 {
        actor.tick().unwrap();
    }
    assert_eq!(gate.calls.lock().unwrap().len(), 1);
    actor.shutdown();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.message",
        message(&rig, "New independent instruction.", None),
    );
    retry_failed_lead(&rig);
    let mut actor = start(&rig, gate, contexts.clone());
    let contexts = contexts.lock().unwrap();
    let included = contexts[1]["messages"].as_array().unwrap();
    assert_eq!(included.len(), 1);
    assert_eq!(included[0]["body"], "New independent instruction.");
    actor.shutdown();
}

#[test]
fn paused_messages_join_one_ready_lead_and_wait_for_start_acknowledgement() {
    let rig = message_rig();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":"pause"}),
    );
    for text in ["First instruction.", "Second instruction."] {
        rpc(
            &rig.service,
            &rig.conn,
            "mission.message",
            message(&rig, text, None),
        );
    }
    let gate = ReceiptGate::new(DeliveryReceipt::Delivered { provider_ref: None });
    let contexts = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(holding_factory(gate, contexts.clone()));
    actor.tick().unwrap();
    assert_eq!(rig.snapshot().tasks.len(), 1);
    assert!(rig.snapshot().runs.is_empty());
    assert!(messages(&rig)
        .iter()
        .all(|m| m.delivery == MessageDelivery::Queued));
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":"resume"}),
    );
    actor.tick().unwrap();
    assert!(messages(&rig)
        .iter()
        .all(|m| m.delivery == MessageDelivery::Queued && m.run_id.is_some()));
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::AwaitingInput)
    });
    assert_eq!(
        contexts.lock().unwrap()[0]["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(messages(&rig)
        .iter()
        .all(|m| m.delivery == MessageDelivery::Delivered));
    actor.shutdown();
}

#[test]
fn terminal_targets_and_unowned_or_malformed_message_bodies_are_rejected() {
    let rig = message_rig();
    let gate = ReceiptGate::new(DeliveryReceipt::Delivered { provider_ref: None });
    let mut actor = start(&rig, gate, Arc::new(Mutex::new(vec![])));
    actor.shutdown();
    let target = rig.snapshot().tasks[0].id.clone();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"task_id":target,"action":"cancel"}),
    );
    let params = message(&rig, "Cannot target a terminal task.", Some(&target));
    assert_eq!(
        rig.service
            .handle(&rig.conn, "mission.message", &params)
            .err()
            .unwrap()
            .code,
        MissionErrorCode::InvalidState
    );
    let mut params = message(&rig, "body", None);
    params["body_ref"]["sha256"] = json!("0".repeat(64));
    assert!(rig
        .service
        .handle(&rig.conn, "mission.message", &params)
        .is_err());
    params["body_ref"] = serde_json::to_value(body(&rig, &[0xff])).unwrap();
    assert_eq!(
        rig.service
            .handle(&rig.conn, "mission.message", &params)
            .err()
            .unwrap()
            .code,
        MissionErrorCode::InvalidArgument
    );
    let content = b"Owned by another mission, not the conversation.";
    let upload = rpc(
        &rig.service,
        &rig.conn,
        "artifact.begin",
        json!({"request_id":Id::generate(),"mission_id":null,"media_type":"text/plain","bytes":content.len().to_string(),"sha256":format!("{:x}",Sha256::digest(content))}),
    );
    rpc(
        &rig.service,
        &rig.conn,
        "artifact.write",
        json!({"upload_id":upload["upload_id"],"offset":"0","data_b64":base64::engine::general_purpose::STANDARD.encode(content)}),
    );
    let staged = rpc(
        &rig.service,
        &rig.conn,
        "artifact.commit",
        json!({"upload_id":upload["upload_id"]}),
    );
    params["body_ref"] = staged.clone();
    assert_eq!(
        rig.service
            .handle(&rig.conn, "mission.message", &params)
            .err()
            .unwrap()
            .code,
        MissionErrorCode::PolicyDenied,
        "staging is only for mission creation"
    );
    let original = rig.snapshot().mission;
    rpc(
        &rig.service,
        &rig.conn,
        "mission.create",
        json!({"request_id":Id::generate(),"title":"Other mission","repository_path":rig.repo.path(),"expected_base_oid":rig.base,"goal_ref":staged,"requirements":original.requirements,"policy":original.policy,"role_bindings":original.role_bindings}),
    );
    assert_eq!(
        rig.service
            .handle(&rig.conn, "mission.message", &params)
            .err()
            .unwrap()
            .code,
        MissionErrorCode::PolicyDenied,
        "another mission's artifact cannot enter this conversation"
    );
    assert!(messages(&rig).is_empty());
    assert!(!rig
        .storage
        .mission_outbox()
        .unwrap()
        .iter()
        .any(|i| i.operation == OutboxOperation::Message));
}

#[test]
fn recovery_preserves_uncertain_delivery_after_its_run_is_terminal() {
    for archived in [false, true] {
        recover_terminal_message(archived);
    }
}

fn recover_terminal_message(archived: bool) {
    let rig = message_rig();
    let gate = ReceiptGate::new(DeliveryReceipt::Delivered { provider_ref: None });
    let _release = ReleaseOnDrop(gate.clone());
    let mut actor = start(&rig, gate.clone(), Arc::new(Mutex::new(vec![])));
    rpc(
        &rig.service,
        &rig.conn,
        "mission.message",
        message(&rig, "Pending reply.", None),
    );
    wait(&mut actor, || gate.calls.lock().unwrap().len() == 1);
    let mut snapshot = rig.snapshot();
    let mut run = snapshot.runs[0].clone();
    run.state = RunState::Succeeded;
    if archived {
        snapshot.mission.state = MissionState::Cancelled;
        snapshot.mission.archived_at = Some(term_storage::time::now_iso8601());
    }
    workflow::commit_upserts(
        &rig.service,
        snapshot.mission,
        "fixture.terminal",
        "terminal",
        MissionEventType::Changed,
        vec![Entity::Run(Box::new(run))],
    )
    .unwrap();
    rig.service.recover_on_startup().unwrap();
    assert_eq!(messages(&rig)[0].delivery, MessageDelivery::Unknown);
    assert!(rig
        .storage
        .mission_outbox()
        .unwrap()
        .iter()
        .any(|i| i.operation == OutboxOperation::Message && i.state == OutboxState::Unknown));
    gate.release();
    actor.shutdown();
    assert_eq!(
        messages(&rig)[0].delivery,
        MessageDelivery::Unknown,
        "old receipt overwrote recovery"
    );
    assert_eq!(gate.calls.lock().unwrap().len(), 1);
}

fn message_rig() -> Rig {
    let rig = Rig::new(true, &["status", "--porcelain"]);
    let mut binding: Binding =
        serde_json::from_value(rig.storage.mission_bindings().unwrap().remove(0)).unwrap();
    binding.capabilities.steer = Support {
        supported: true,
        reason_code: None,
    };
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":binding.revision,"binding":binding}),
    );
    rig
}

fn retry_failed_lead(rig: &Rig) {
    let snapshot = rig.snapshot();
    let task = snapshot
        .tasks
        .iter()
        .filter(|t| t.role == Some(Role::Lead))
        .max_by_key(|t| t.ordinal)
        .unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.task.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":snapshot.mission.revision,"task_id":task.id,"action":"retry","binding_id":null}),
    );
}

#[test]
fn queued_messages_after_a_finished_lead_share_one_new_plan_and_keep_requirements() {
    let rig = message_rig();
    let mut actor = rig.actor(factory(Arc::new(Mutex::new(vec![]))));
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    actor.shutdown();
    let before = rig.snapshot();
    for text in [
        "Reconsider the implementation strategy.",
        "Keep the original completion conditions.",
    ] {
        rpc(
            &rig.service,
            &rig.conn,
            "mission.message",
            message(&rig, text, None),
        );
    }
    let after = rig.snapshot();
    assert_eq!(after.tasks.len(), before.tasks.len() + 1);
    assert_eq!(after.mission.phase, Phase::Planning);
    assert_eq!(after.mission.requirements, before.mission.requirements);
    assert_eq!(after.mission.goal_ref, before.mission.goal_ref);
    assert_eq!(after.mission.plan_revision, before.mission.plan_revision);
    assert_eq!(
        after.mission.revision.get(),
        before.mission.revision.get() + 2
    );
}

#[test]
fn cancellation_rejects_the_next_message_without_sending_it() {
    let rig = message_rig();
    let gate = ReceiptGate::new(DeliveryReceipt::Delivered { provider_ref: None });
    let _release = ReleaseOnDrop(gate.clone());
    let mut actor = start(&rig, gate.clone(), Arc::new(Mutex::new(vec![])));
    rpc(
        &rig.service,
        &rig.conn,
        "mission.message",
        message(&rig, "First delivery.", None),
    );
    wait(&mut actor, || gate.calls.lock().unwrap().len() == 1);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.message",
        message(&rig, "Must not be sent after cancellation.", None),
    );
    actor.tick().unwrap();
    assert_eq!(
        gate.calls.lock().unwrap().len(),
        1,
        "two deliveries overlapped on one Run"
    );
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":"cancel"}),
    );
    actor.tick().unwrap();
    assert_eq!(
        messages(&rig)
            .iter()
            .filter(|m| m.delivery == MessageDelivery::Rejected)
            .count(),
        1
    );
    gate.release();
    actor.shutdown();
    assert_eq!(gate.calls.lock().unwrap().len(), 1);
}

#[test]
fn queued_replanning_at_the_plan_limit_records_one_decision_without_stalling_the_actor() {
    let rig = message_rig();
    let mut actor = rig.actor(factory(Arc::new(Mutex::new(vec![]))));
    rig.tick_until(&mut actor, |s| s.mission.phase == Phase::AwaitingAcceptance);
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":"pause"}),
    );
    rpc(
        &rig.service,
        &rig.conn,
        "mission.message",
        message(&rig, "Queued strategy change.", None),
    );
    actor.shutdown();
    let mut mission = rig.snapshot().mission;
    mission.plan_revision =
        term_contracts::mission::validation::MissionLimits::load().max_plan_revisions;
    workflow::commit_upserts(
        &rig.service,
        mission,
        "fixture.plan_limit",
        "limit",
        MissionEventType::Changed,
        vec![],
    )
    .unwrap();
    rpc(
        &rig.service,
        &rig.conn,
        "mission.control",
        json!({"request_id":Id::generate(),"mission_id":rig.id,"expected_revision":rig.snapshot().mission.revision,"action":"resume"}),
    );
    let mut actor = rig.actor(factory(Arc::new(Mutex::new(vec![]))));
    actor.tick().unwrap();
    let before = rig.snapshot();
    assert_eq!(
        before
            .decisions
            .iter()
            .filter(|d| d.kind == DecisionKind::Budget && d.state == DecisionState::Open)
            .count(),
        1
    );
    actor.tick().unwrap();
    assert_eq!(rig.snapshot().mission.revision, before.mission.revision);
    assert_eq!(messages(&rig)[0].delivery, MessageDelivery::Queued);
    actor.shutdown();
}

#[path = "mission_message_replacement.rs"]
mod replacement;
